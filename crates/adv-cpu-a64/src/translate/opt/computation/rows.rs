//! A prepared affine row domain. The generated entry proves its entire
//! ordinary-window and alias footprint before it executes any guest effect.

use super::{Access, Check, Effect, Expr, ExprId, Graph, Proof, Var};
use crate::asm::{Asm, Label};
use crate::encode::{
    Cond, Extend, Reg,
    Shift::Lsl,
    Size::{W, X},
    add, add_ext, add_imm, and_imm, cmp, csel, ldr, lsr_imm, mov, movk, movz, mrs_nzcv, msr_nzcv,
    mul, sbfx, str, sub, sub_imm,
};
use std::cell::Cell;

#[derive(Debug)]
pub(crate) struct Domain {
    exprs: Vec<Expr>,
    checks: Vec<Check>,
    spans: Vec<Span>,
    aliases: Vec<(usize, usize)>,
    rows: ExprId,
    inner: Vec<ExprId>,
    slots_read: usize,
    frame: u32,
}

#[derive(Debug, PartialEq, Eq)]
struct Span {
    base: ExprId,
    terms: Vec<(ExprId, ExprId)>,
    bytes: u32,
    access: Access,
    tracked: Option<u8>,
    parameter: bool,
}

impl Domain {
    pub(crate) fn build(trace: &crate::translate::front::trace::Trace) -> Option<(Self, Proof)> {
        let head = trace.insts.first()?.pc;
        let graph = Graph::build(trace)
            .inspect_err(|why| {
                crate::jit_map::optimized(head, format_args!("row graph {why:?}"));
            })
            .ok()?;
        let outer = graph.heads[0]?;
        if !graph
            .loops
            .iter()
            .enumerate()
            .any(|(i, lp)| i != outer && graph.loops[outer].inside[lp.head])
        {
            return None;
        }
        let refused = Cell::new(None);
        let proof = Proof::prove(trace, &refused, true).or_else(|| {
            let reason = refused.get();
            crate::jit_map::optimized(head, format_args!("row proof {reason:?}"));
            None
        })?;
        let outer = proof.outer?;
        let rows = outer.count;
        let inner: Vec<_> = proof
            .vars
            .iter()
            .enumerate()
            .filter_map(|(id, var)| match *var {
                Var::Counter(Some(count)) if Some(id) != outer.counter => Some(count),
                _ => None,
            })
            .collect();
        if inner.is_empty() {
            return None;
        }
        let mut spans = Vec::new();
        for effect in &proof.effects {
            let Effect {
                addr,
                limits,
                access,
                bytes,
                ..
            } = effect;
            if !limits.is_empty() || addr.terms.len() > 2 || *bytes == 0 {
                return None;
            }
            let tracked = if addr.terms.is_empty() {
                tracked_base(&proof.exprs, addr.base)
                    .filter(|&base| base == super::ESP || base == super::EBP || base == proof.based)
            } else {
                None
            };
            let terms = addr
                .terms
                .iter()
                .map(|&(var, step)| match proof.vars[var] {
                    Var::Counter(Some(count)) => Some((count, step)),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let span = Span {
                base: addr.base,
                terms,
                bytes: *bytes,
                access: *access,
                tracked,
                parameter: false,
            };
            if !spans.contains(&span) {
                spans.push(span);
            }
        }
        // Entry parameter reads, including the indirect slot, cannot be
        // overwritten anywhere in the proved region.
        let mut used = vec![false; proof.exprs.len()];
        let mut mark = |id| mark_expression(&proof.exprs, &mut used, id);
        mark(rows);
        for &count in &inner {
            mark(count);
        }
        for check in &proof.checks {
            match *check {
                Check::Equal(a, b) => {
                    mark(a);
                    mark(b);
                }
                Check::Positive(a, _) | Check::Target(a, _) | Check::Mask(a, ..) => mark(a),
                Check::StackAlignment(_) => {}
            }
        }
        for span in &spans {
            mark(span.base);
            for &(count, step) in &span.terms {
                mark(count);
                mark(step);
            }
        }
        let mut slots_read = 0;
        for (&expr, read) in proof.exprs.iter().zip(used) {
            if read && let Expr::Word(base) = expr {
                slots_read += 1;
                let span = Span {
                    base,
                    terms: Vec::new(),
                    bytes: 4,
                    access: Access::Read,
                    tracked: None,
                    parameter: true,
                };
                if !spans.contains(&span) {
                    spans.push(span);
                }
            }
        }
        let mut aliases = Vec::new();
        for (a, left) in spans.iter().enumerate() {
            for (b, right) in spans.iter().enumerate().skip(a + 1) {
                if !left.writes() && !right.writes() {
                    continue;
                }
                let same_base = left.tracked.is_some() && left.tracked == right.tracked;
                if !same_base && (left.stable() || right.stable()) {
                    aliases.push((a, b));
                }
            }
        }
        let frame = (proof.exprs.len() * 4 + spans.len() * 8).next_multiple_of(16);
        if frame >= 4096 {
            return None;
        }
        let domain = Self {
            exprs: proof.exprs.clone(),
            checks: proof.checks.clone(),
            spans,
            aliases,
            rows,
            inner,
            slots_read,
            frame: frame as u32,
        };
        Some((domain, proof))
    }

    /// Entry code owns its scratch. A failed guard leaves with the original
    /// Tier-1 assignment, including the incoming flags.
    pub(crate) fn emit(
        &self,
        asm: &mut Asm,
        minimum: (Label, Label),
        miss: Label,
        target_miss: Label,
        offer: Option<Label>,
    ) -> u32 {
        let start = asm.offset();
        let fail = asm.label();
        let success = asm.label();
        let target = if miss == target_miss {
            fail
        } else {
            asm.label()
        };
        let search = asm.label();
        let mut emit = RangeEmitter {
            asm,
            exprs: &self.exprs,
            slots: vec![false; self.exprs.len()],
            fail,
            search,
        };
        let frame = self.frame;
        emit.asm.emit(mrs_nzcv(FLAGS));
        emit.asm.emit(sub_imm(X, SP, SP, frame, false));
        let rows = if offer.is_some() { ROWS } else { A };
        emit.expression(self.rows, rows);
        emit.asm.ldr_literal(B, minimum.0);
        emit.asm.emit(cmp(W, rows, B, Lsl, 0));
        emit.asm.b_cond(Cond::Lo, fail);
        for &inner in &self.inner {
            emit.expression(inner, A);
            emit.asm.ldr_literal(B, minimum.1);
            emit.asm.emit(cmp(W, A, B, Lsl, 0));
            emit.asm.b_cond(Cond::Lo, fail);
        }
        for &check in &self.checks {
            match check {
                Check::Equal(a, b) => {
                    emit.expression(a, A);
                    emit.expression(b, B);
                    emit.load(a, A);
                    emit.asm.emit(cmp(W, A, B, Lsl, 0));
                    emit.asm.b_cond(Cond::Ne, fail);
                }
                Check::Positive(a, max) => {
                    emit.expression(a, A);
                    emit.asm.cbz(W, false, A, fail);
                    if max != u32::MAX {
                        emit.constant(B, max);
                        emit.asm.emit(cmp(W, A, B, Lsl, 0));
                        emit.asm.b_cond(Cond::Hi, fail);
                    }
                }
                Check::StackAlignment(align) if align > 1 => {
                    emit.asm
                        .emit(and_imm(W, A, guest(super::ESP), u64::from(align - 1)));
                    emit.asm.cbz(W, true, A, fail);
                }
                Check::StackAlignment(_) => {}
                Check::Mask(value, mask, nonzero) => {
                    emit.expression(value, A);
                    emit.asm.emit(and_imm(W, A, A, u64::from(mask)));
                    emit.asm.cbz(W, !nonzero, A, fail);
                }
                Check::Target(word, expected) => {
                    emit.expression(word, A);
                    crate::facts::guard_indirect_target(emit.asm, A, B, expected, target);
                }
            }
        }
        let ranges = self.exprs.len() as u32 * 4;
        for (i, span) in self.spans.iter().enumerate() {
            // Materialize the shared parameters before using scratch for
            // endpoints. Every count fits a positive signed dword and
            // there are at most two products of two signed dwords.
            emit.expression(span.base, A);
            for &(count, step) in &span.terms {
                emit.expression(count, A);
                emit.expression(step, B);
                emit.load(count, A);
                emit.asm.emit(sub_imm(W, A, A, 1, false));
                emit.asm.tbz(true, A, 31, fail);
            }
            emit.load(span.base, A);
            emit.asm.emit(mov(W, B, A));
            for &(count, step) in &span.terms {
                emit.load(count, C);
                emit.load(step, D);
                emit.asm.emit_all(&[
                    sub_imm(W, C, C, 1, false),
                    sbfx(X, D, D, 0, 32),
                    mul(X, C, C, D),
                    cmp(X, C, Reg::ZR, Lsl, 0),
                    csel(X, D, C, Reg::ZR, Cond::Lt),
                    add(X, A, A, D, Lsl, 0),
                    csel(X, C, C, Reg::ZR, Cond::Gt),
                    add(X, B, B, C, Lsl, 0),
                ]);
            }
            let offset = ranges + i as u32 * 8;
            emit.constant(C, span.bytes - 1);
            emit.asm.emit_all(&[
                add(X, B, B, C, Lsl, 0),
                lsr_imm(X, C, A, 32),
                lsr_imm(X, D, B, 32),
            ]);
            emit.asm.cbz(X, true, C, fail);
            emit.asm.cbz(X, true, D, fail);
            emit.asm
                .emit_all(&[str(W, A, SP, offset), str(W, B, SP, offset + 4)]);
            // Parameter ranges were already checked before their entry load.
            if !span.parameter {
                emit.range(span.writes());
            }
        }
        for &(a, b) in &self.aliases {
            let (a, b) = (ranges + a as u32 * 8, ranges + b as u32 * 8);
            emit.asm
                .emit_all(&[ldr(W, A, SP, a + 4), ldr(W, B, SP, b), cmp(W, A, B, Lsl, 0)]);
            let apart = emit.asm.label();
            emit.asm.b_cond(Cond::Lo, apart);
            emit.asm
                .emit_all(&[ldr(W, A, SP, b + 4), ldr(W, B, SP, a), cmp(W, A, B, Lsl, 0)]);
            emit.asm.b_cond(Cond::Hs, fail);
            emit.asm.bind(apart);
        }
        emit.asm.b(success);
        emit.asm.bind(fail);
        emit.asm
            .emit_all(&[add_imm(X, SP, SP, frame, false), msr_nzcv(FLAGS)]);
        emit.asm.b(miss);
        let proof_words = ((emit.asm.offset() - start) / 4) as u32;
        if miss != target_miss {
            emit.asm.bind(target);
            emit.asm
                .emit_all(&[add_imm(X, SP, SP, frame, false), msr_nzcv(FLAGS)]);
            emit.asm.b(target_miss);
        }
        crate::facts::batch_span_search(emit.asm, search, false);
        // Keep the offer target in reach of the decision's bit test even
        // when the emitted loop body is large. Search exits always return.
        if let Some(leave) = offer {
            emit.asm.bind(leave);
            emit.asm.emit(0);
        }
        emit.asm.bind(success);
        emit.asm
            .emit_all(&[add_imm(X, SP, SP, frame, false), msr_nzcv(FLAGS)]);
        // Account for the bounded shared search at each range query.
        proof_words
            + 2
            + 84 * (self.spans.iter().filter(|span| !span.parameter).count() + self.slots_read)
                as u32
    }
}

fn mark_expression(exprs: &[Expr], used: &mut [bool], id: ExprId) {
    if std::mem::replace(&mut used[id], true) {
        return;
    }
    match exprs[id] {
        Expr::Add(a, b) | Expr::Mul(a, b) => {
            mark_expression(exprs, used, a);
            mark_expression(exprs, used, b);
        }
        Expr::Neg(a) | Expr::Word(a) | Expr::SignTrip(a, _) => mark_expression(exprs, used, a),
        Expr::Constant(_) | Expr::Register(_) => {}
    }
}

fn tracked_base(exprs: &[Expr], id: ExprId) -> Option<u8> {
    match exprs[id] {
        Expr::Register(base) => Some(base),
        Expr::Add(a, b) => match (exprs[a], exprs[b]) {
            (Expr::Register(base), Expr::Constant(_))
            | (Expr::Constant(_), Expr::Register(base)) => Some(base),
            _ => None,
        },
        _ => None,
    }
}

impl Span {
    fn writes(&self) -> bool {
        matches!(self.access, Access::FrameWrite | Access::Write)
    }

    fn stable(&self) -> bool {
        self.parameter || self.tracked.is_some()
    }
}

const SP: Reg = Reg::new(31);
const WINDOW: Reg = Reg::new(27);
const FLAGS: Reg = Reg::new(6);
const ROWS: Reg = Reg::new(7);
const A: Reg = Reg::new(0);
const B: Reg = Reg::new(1);
const C: Reg = Reg::new(2);
const D: Reg = Reg::new(3);
const TEMP: Reg = Reg::new(8);

const fn guest(reg: u8) -> Reg {
    Reg::new(19 + reg)
}

struct RangeEmitter<'a> {
    asm: &'a mut Asm,
    exprs: &'a [Expr],
    slots: Vec<bool>,
    fail: Label,
    search: Label,
}

impl RangeEmitter<'_> {
    fn constant(&mut self, register: Reg, value: u32) {
        self.asm.emit(movz(W, register, value as u16, 0));
        if value >> 16 != 0 {
            self.asm.emit(movk(W, register, (value >> 16) as u16, 16));
        }
    }

    fn load(&mut self, id: ExprId, dst: Reg) {
        self.asm.emit(ldr(W, dst, SP, id as u32 * 4));
    }

    fn range(&mut self, write: bool) {
        let hit = self.asm.label();
        crate::facts::batch_span(self.asm, write, false, self.search, hit);
        self.asm.b(self.fail);
        self.asm.bind(hit);
    }

    fn expression(&mut self, id: ExprId, dst: Reg) {
        if self.slots[id] {
            self.load(id, dst);
            return;
        }
        match self.exprs[id] {
            Expr::Constant(value) => self.constant(dst, value),
            Expr::Register(reg) => self.asm.emit(mov(W, dst, guest(reg))),
            Expr::Neg(value) => {
                self.expression(value, dst);
                self.asm.emit(sub(W, dst, Reg::ZR, dst, Lsl, 0));
            }
            Expr::Add(a, b) | Expr::Mul(a, b) => {
                self.expression(a, A);
                self.expression(b, B);
                self.load(a, A);
                self.asm.emit(if matches!(self.exprs[id], Expr::Add(..)) {
                    add(W, dst, A, B, Lsl, 0)
                } else {
                    mul(W, dst, A, B)
                });
            }
            Expr::SignTrip(first, shift) => {
                self.expression(first, TEMP);
                self.asm.emit_all(&[
                    lsr_imm(W, A, TEMP, shift),
                    add_imm(W, A, A, 2, false),
                    movz(W, B, 1, 0),
                    cmp(W, TEMP, Reg::ZR, Lsl, 0),
                    csel(W, dst, A, B, Cond::Ge),
                ]);
            }
            Expr::Word(address) => {
                self.expression(address, TEMP);
                self.asm
                    .emit_all(&[mov(W, A, TEMP), add_imm(X, B, A, 3, false)]);
                self.range(false);
                self.asm
                    .emit(add_ext(X, TEMP, WINDOW, TEMP, Extend::Uxtw, 0));
                self.asm.emit(ldr(W, dst, TEMP, 0));
            }
        }
        self.slots[id] = true;
        self.asm.emit(str(W, dst, SP, id as u32 * 4));
    }
}
