//! A prepared affine row domain. The generated entry proves its ordinary
//! window, alignment and aliases before any guest effect, clipping at the
//! first denied page when a profitable prefix remains. An observed optional
//! tail is guarded separately from the strip that pays for the proof.

use super::{Access, Check, Effect, Expr, ExprId, Graph, Proof, Var, observe_expr};
use crate::asm::{Asm, Label};
use crate::encode::{
    Cond, Extend, Index, Reg,
    Shift::Lsl,
    Size::{W, X},
    add, add_ext, add_imm, and_imm, asr_imm, cmp, csel, ldp, ldr, lsr_imm, mov, movk, movz,
    mrs_nzcv, msr_nzcv, mul, mvn, sbfx, str, sub, sub_imm, udiv,
};
use crate::translate::front::{
    decode::Op,
    trace::{Edge, Next, Trace},
};
use crate::translate::lower::resident::RowFailure;
use std::cell::Cell;

#[derive(Debug)]
pub(crate) struct Domain {
    exprs: Vec<Expr>,
    checks: Vec<Check>,
    spans: Vec<Span>,
    aliases: Vec<(usize, usize)>,
    rows: ExprId,
    row_counter: Option<u8>,
    row_exits: Vec<u32>,
    inner: Vec<(u32, ExprId)>,
    aligned: Vec<usize>,
    alignment_strip: Option<(u32, ExprId, u32, u32)>,
    slots_read: usize,
    direct_calls: u32,
    target: Option<crate::translate::lower::resident::RowTarget>,
    frame: u32,
}

#[derive(Debug, PartialEq, Eq)]
struct Span {
    base: ExprId,
    terms: Vec<(ExprId, ExprId)>,
    row_step: Option<ExprId>,
    bytes: u32,
    access: Access,
    tracked: Option<u8>,
    parameter: bool,
}

impl Domain {
    pub(crate) fn build(trace: &Trace, observed: &[u32; 8]) -> Option<(Self, Proof)> {
        let head = trace.insts.first()?.pc;
        let graph = Graph::build_selected(trace, true)
            .inspect_err(|why| {
                crate::jit_map::optimized(head, format_args!("row graph {why:?}"));
            })
            .ok()?;
        let outer_index = graph.heads[0]?;
        if !graph
            .loops
            .iter()
            .enumerate()
            .any(|(i, lp)| i != outer_index && graph.loops[outer_index].inside[lp.head])
        {
            return None;
        }
        let refused = Cell::new(None);
        let proof = Proof::prove(trace, &refused, Some(observed)).or_else(|| {
            let reason = refused.get();
            let op = reason
                .as_ref()
                .and_then(|why| trace.insts.get(why.at))
                .map(|inst| inst.op);
            crate::jit_map::optimized(head, format_args!("row proof {reason:?} {op:?}"));
            None
        })?;
        let outer = proof.outer?;
        let rows = outer.count;
        let row_counter = outer.regs.iter().enumerate().find_map(|(reg, pair)| {
            let (start, step) = (*pair)?;
            (start == rows
                && proof.exprs[start] == Expr::Register(reg as u8)
                && proof.exprs[step] == Expr::Constant(u32::MAX))
            .then_some(reg as u8)
        });
        let mut row_exits = Vec::new();
        {
            let outer = &graph.loops[outer_index];
            let mut interior = Vec::new();
            for (at, inst) in trace.insts.iter().enumerate() {
                let inside = outer.inside[at];
                if inside {
                    interior.push(inst.pc);
                } else {
                    row_exits.push(inst.pc);
                }
                let next = match inst.next {
                    Next::Edge(edge) => Some(edge),
                    _ => None,
                };
                for edge in [next, inst.taken].into_iter().flatten() {
                    if let Edge::Exit(pc) | Edge::Event(pc) = edge {
                        if inside && at != outer.test {
                            interior.push(pc);
                        } else {
                            row_exits.push(pc);
                        }
                    }
                }
            }
            row_exits.retain(|pc| !interior.contains(pc));
        }
        let inner: Vec<_> = proof
            .vars
            .iter()
            .enumerate()
            .filter_map(|(id, var)| match *var {
                Var::Counter(head, Some(count)) if Some(id) != outer.counter => Some((head, count)),
                _ => None,
            })
            .collect();
        if inner.is_empty() {
            return None;
        }
        let mut spans = Vec::new();
        let mut aligned = Vec::new();
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
                    Var::Counter(_, Some(count)) => Some((count, step)),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let span = Span {
                base: addr.base,
                terms,
                row_step: addr
                    .terms
                    .iter()
                    .find_map(|&(var, step)| (Some(var) == outer.counter).then_some(step)),
                bytes: *bytes,
                access: *access,
                tracked,
                parameter: false,
            };
            if !spans.contains(&span) {
                spans.push(span);
            }
            aligned.extend(
                effect.indices.iter().copied().filter(
                    |&i| matches!(trace.insts[i].op, Op::Simd(s) if s.aligned && s.memory()),
                ),
            );
        }
        aligned.sort_unstable();
        aligned.dedup();
        // Tier-1 tests address alignment at each such access. A proved
        // aligned strip may pay for its row guard even without a wider batch.
        let alignment_strip = graph
            .loops
            .iter()
            .enumerate()
            .filter(|&(i, lp)| i != outer_index && graph.loops[outer_index].inside[lp.head])
            .filter_map(|(i, lp)| {
                let head = trace.insts[lp.head].pc;
                let count = inner
                    .iter()
                    .find_map(|&(pc, count)| (pc == head).then_some(count))?;
                let accesses = aligned
                    .iter()
                    .filter(|&&site| lp.inside[site] && dominates_latch(&graph, i, site))
                    .count() as u32;
                let minimum = observe_expr(&proof.exprs, observed, count)
                    .and_then(|count| count.checked_ilog2())
                    .map_or(1, |shift| 1 << shift);
                let saved = (accesses * 2).saturating_mul(minimum).saturating_sub(2);
                (saved != 0).then_some((head, count, minimum, saved))
            })
            .max_by_key(|&(_, _, _, saved)| saved);

        // Entry parameter reads, including the indirect slot, cannot be
        // overwritten anywhere in the proved region.
        let mut used = vec![false; proof.exprs.len()];
        let mut mark = |id| mark_expression(&proof.exprs, &mut used, id);
        mark(rows);
        for &(_, count) in &inner {
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
                    row_step: None,
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
        let mut targets = proof.checks.iter().filter_map(|&check| {
            if let Check::Target(word, value) = check {
                Some((word, value))
            } else {
                None
            }
        });
        let target = if let Some((word, value)) = targets.next() {
            if targets.next().is_some() {
                return None;
            }
            let Expr::Word(addr) = proof.exprs[word] else {
                return None;
            };
            let (base, offset) = register_offset(&proof.exprs, addr)?;
            Some(crate::translate::lower::resident::RowTarget {
                base,
                offset,
                value,
            })
        } else {
            None
        };
        let direct_calls = u32::from(
            target.is_some()
                && trace.insts.iter().enumerate().any(|(index, inst)| {
                    inst.guarded_slot.is_some()
                        && graph.loops[outer_index].inside[index]
                        && dominates_latch(&graph, outer_index, index)
                }),
        );
        let domain = Self {
            direct_calls,
            target,
            exprs: proof.exprs.clone(),
            checks: proof.checks.clone(),
            spans,
            aliases,
            rows,
            row_counter,
            row_exits,
            inner,
            aligned,
            alignment_strip,
            slots_read,
            frame: frame as u32,
        };
        Some((domain, proof))
    }

    /// These accesses are aligned on every row under the entry's masks.
    pub(crate) fn use_proved_alignment(&self, trace: &mut Trace) {
        for &at in &self.aligned {
            if let Op::Simd(s) = &mut trace.insts[at].op {
                s.aligned = false;
            }
        }
    }

    /// Keep only edges covered by the same guards the row entry emits.
    fn use_proved_branches(&self, proof: &Proof, trace: &mut Trace) -> Option<()> {
        let mut choices = vec![[false; 2]; trace.len()];
        for &(at, branch) in &proof.branches {
            if let Some((taken, guard)) = branch {
                if !self.checks.iter().any(|&check| match (guard, check) {
                    (Check::Positive(a, limit), Check::Positive(b, max)) => a == b && max <= limit,
                    (Check::Equal(..) | Check::Mask(..), _) => guard == check,
                    _ => false,
                }) {
                    return None;
                }
                choices[at][usize::from(taken)] = true;
            } else {
                choices[at] = [true; 2];
            }
        }
        trace.select_row_branches(&choices);
        Some(())
    }

    pub(crate) fn prove_scoped_split(&self, proof: &mut Proof, trace: &mut Trace) {
        if self.use_proved_branches(proof, trace).is_some() {
            proof.prove_scoped_split(trace);
        }
    }

    pub(crate) fn target(&self) -> Option<crate::translate::lower::resident::RowTarget> {
        self.target
    }

    /// A guarded inline call still pushes its return address. The checked
    /// indirect table probe and shadow call/return transfers are eliminated.
    pub(crate) fn call_savings(&self) -> u32 {
        self.direct_calls * crate::policy::proved::INDIRECT_CALL_WORDS
    }

    pub(crate) fn count_register(&self) -> Option<u8> {
        match self.exprs[self.rows] {
            Expr::Register(reg) => Some(reg),
            _ => None,
        }
    }

    pub(crate) fn row_counter(&self) -> Option<u8> {
        self.row_counter
    }

    pub(crate) fn completes_rows_at(&self, eip: u32) -> bool {
        self.row_exits.contains(&eip)
    }

    pub(crate) fn strip_count(&self, head: u32) -> Option<usize> {
        self.inner
            .iter()
            .find_map(|&(pc, count)| (pc == head).then_some(count))
    }

    pub(crate) fn alignment_strip(&self) -> Option<(u32, ExprId, u32, u32)> {
        self.alignment_strip
    }

    /// Entry code owns its scratch. A failed guard leaves with the original
    /// Tier-1 assignment, including the incoming flags.
    pub(crate) fn emit(
        &self,
        asm: &mut Asm,
        ((minimum, exact), strip, shift, extra): ((Label, Label), ExprId, u32, bool),
        short: Label,
        failures: [(RowFailure, Label); 5],
        offer: Option<Label>,
    ) -> (u32, Option<usize>, (usize, Label)) {
        let start = asm.offset();
        let small = asm.label();
        let drops = failures.map(|(kind, target)| (kind, target, asm.label()));
        let [range, alias, parameter, bad_count, span] = drops.map(|(_, _, label)| label);
        let success = asm.label();
        let search = asm.label();
        let reverse = asm.label();
        let mut emit = RangeEmitter {
            asm,
            exprs: &self.exprs,
            slots: vec![false; self.exprs.len()],
            fail: parameter,
            search,
            range_cold: 0,
        };
        let frame = self.frame;
        emit.asm.emit(mrs_nzcv(FLAGS));
        emit.asm.emit(sub_imm(X, SP, SP, frame, false));
        emit.expression(self.rows, ROWS);
        let ranges = self.exprs.len() as u32 * 4;
        emit.asm.emit(mov(W, LIMIT, ROWS));
        emit.expression(strip, A);
        if extra {
            emit.asm.cbz(W, false, A, small);
        }
        let bias = (shift != 0 || extra).then(|| {
            let at = emit.asm.offset() / 4;
            emit.asm.emit(0);
            at
        });
        if shift != 0 {
            emit.asm.emit(asr_imm(X, A, A, shift));
        }
        let (near_price, priced) = (emit.asm.label(), emit.asm.label());
        emit.price(minimum, ROWS, shift, near_price);
        emit.asm.bind(priced);
        emit.asm.emit(mov(W, WORK, A));
        for &check in &self.checks {
            match check {
                Check::Equal(a, b) => {
                    emit.expression(a, A);
                    emit.expression(b, B);
                    emit.load(a, A);
                    emit.asm.emit(cmp(W, A, B, Lsl, 0));
                    emit.asm.b_cond(Cond::Ne, parameter);
                }
                Check::Positive(a, max) => {
                    emit.expression(a, A);
                    emit.asm.cbz(W, false, A, bad_count);
                    if max != u32::MAX {
                        emit.constant(B, max);
                        emit.asm.emit(cmp(W, A, B, Lsl, 0));
                        emit.asm.b_cond(Cond::Hi, bad_count);
                    }
                }
                Check::StackAlignment(align) if align > 1 => {
                    emit.asm
                        .emit(and_imm(W, A, guest(super::ESP), u64::from(align - 1)));
                    emit.asm.cbz(W, true, A, parameter);
                }
                Check::StackAlignment(_) => {}
                Check::Mask(value, mask, nonzero) => {
                    emit.expression(value, A);
                    emit.asm.emit(and_imm(W, A, A, u64::from(mask)));
                    emit.asm.cbz(W, !nonzero, A, parameter);
                }
                Check::Target(..) => {}
            }
        }
        for (i, stream) in self.spans.iter().enumerate() {
            // Materialize the shared parameters before using scratch for
            // endpoints. Every count fits a positive signed dword and
            // there are at most two products of two signed dwords.
            emit.expression(stream.base, A);
            for &(count, step) in &stream.terms {
                emit.expression(count, A);
                emit.expression(step, B);
                emit.load(count, A);
                emit.asm.emit(sub_imm(W, A, A, 1, false));
                emit.asm.tbz(true, A, 31, bad_count);
            }
            emit.load(stream.base, A);
            emit.asm.emit(mov(W, B, A));
            for &(count, step) in &stream.terms {
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
            emit.constant(C, stream.bytes - 1);
            emit.asm.emit_all(&[
                add(X, B, B, C, Lsl, 0),
                lsr_imm(X, C, A, 32),
                lsr_imm(X, D, B, 32),
            ]);
            emit.asm.cbz(X, true, C, span);
            emit.asm.cbz(X, true, D, span);
            emit.asm
                .emit_all(&[str(W, A, SP, offset), str(W, B, SP, offset + 4)]);
            // Parameter ranges were already checked before their entry load.
            if !stream.parameter {
                if let Some(step) = stream.row_step {
                    emit.row_range(stream.writes(), offset, step, self.rows, reverse, range);
                } else {
                    emit.range(stream.writes(), range);
                }
            }
        }
        let complete = emit.asm.label();
        let bounded = emit.asm.label();
        emit.asm.emit(cmp(W, LIMIT, ROWS, Lsl, 0));
        emit.asm.b_cond(Cond::Eq, complete);
        let clip_start = emit.asm.offset();
        emit.asm.emit(mov(W, A, WORK));
        let (near_clip_price, clip_priced) = (emit.asm.label(), emit.asm.label());
        emit.price(minimum, LIMIT, 0, near_clip_price);
        emit.asm.bind(clip_priced);
        // Reproject the stored bounds against the minimum over all streams.
        // Their entry expressions and row pitches have already been loaded.
        emit.asm.emit(sub(W, ROWS, ROWS, LIMIT, Lsl, 0));
        for (i, stream) in self.spans.iter().enumerate() {
            let Some(step) = stream.row_step else {
                continue;
            };
            let offset = ranges + i as u32 * 8;
            emit.load(step, C);
            emit.asm.emit_all(&[
                ldr(W, A, SP, offset),
                ldr(W, B, SP, offset + 4),
                mul(W, D, C, ROWS),
                cmp(W, C, Reg::ZR, Lsl, 0),
                sub(W, C, A, D, Lsl, 0),
                csel(W, A, C, A, Cond::Lt),
                sub(W, C, B, D, Lsl, 0),
                csel(W, B, C, B, Cond::Ge),
                str(W, A, SP, offset),
                str(W, B, SP, offset + 4),
            ]);
        }
        emit.asm.emit(mov(W, A, LIMIT));
        emit.asm.emit(movz(W, ROWS, 0, 0));
        emit.asm.b(bounded);
        let clip_words = (emit.asm.offset() - clip_start) as u32 / 4;
        emit.asm.bind(complete);
        emit.asm.emit(movz(W, A, 0, 0));
        emit.asm.bind(bounded);
        // A clipped prefix cannot send the unproved remaining domain to
        // the worker artifact. Its charged head edge owns this row limit.
        emit.asm.emit(str(W, A, RT, crate::runtime::HEAD_EXIT));
        for &(a, b) in &self.aliases {
            let (a, b) = (ranges + a as u32 * 8, ranges + b as u32 * 8);
            emit.asm
                .emit_all(&[ldr(W, A, SP, a + 4), ldr(W, B, SP, b), cmp(W, A, B, Lsl, 0)]);
            let apart = emit.asm.label();
            emit.asm.b_cond(Cond::Lo, apart);
            emit.asm
                .emit_all(&[ldr(W, A, SP, b + 4), ldr(W, B, SP, a), cmp(W, A, B, Lsl, 0)]);
            emit.asm.b_cond(Cond::Hs, alias);
            emit.asm.bind(apart);
        }
        let sample = (emit.asm.offset() / 4, emit.asm.label());
        emit.asm.b(success);
        let proof_words = ((emit.asm.offset() - start) / 4) as u32;
        for (label, target) in
            std::iter::once((small, short)).chain(drops.map(|(_, target, label)| (label, target)))
        {
            emit.asm.bind(label);
            emit.asm
                .emit_all(&[add_imm(X, SP, SP, frame, false), msr_nzcv(FLAGS)]);
            emit.asm.b(target);
        }
        crate::facts::batch_span_search(emit.asm, search, false);
        crate::facts::batch_span_search(emit.asm, reverse, true);
        // Keep the offer target in reach of the decision's bit test even
        // when the emitted loop body is large. Search exits always return.
        if let Some(leave) = offer {
            emit.asm.bind(leave);
            emit.asm.emit(str(W, WORK, RT, crate::runtime::ROW_WORK));
            emit.asm.emit(0);
        }
        emit.asm.bind(sample.1);
        emit.load(self.rows, A);
        emit.asm.emit_all(&[
            str(W, WORK, RT, crate::runtime::ROW_WORK),
            str(W, A, RT, crate::runtime::ROW_COUNT),
        ]);
        emit.asm.b(success);
        emit.asm.bind(near_price);
        if shift == 0 {
            emit.asm.cbz(W, false, A, small);
        } else {
            emit.asm.tbz(true, B, 63, small);
        }
        emit.exact_price(exact, ROWS, small);
        emit.asm.b(priced);
        emit.asm.bind(near_clip_price);
        emit.exact_price(exact, LIMIT, range);
        emit.asm.b(clip_priced);
        emit.asm.bind(success);
        emit.asm
            .emit_all(&[add_imm(X, SP, SP, frame, false), msr_nzcv(FLAGS)]);
        (
            proof_words - clip_words - emit.range_cold
                + 2
                + crate::policy::proved::SPAN_SEARCH_WORDS
                    * (self.spans.iter().filter(|span| !span.parameter).count() + self.slots_read)
                        as u32,
            bias,
            sample,
        )
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
    register_offset(exprs, id).map(|(base, _)| base)
}

fn register_offset(exprs: &[Expr], id: ExprId) -> Option<(u8, u32)> {
    match exprs[id] {
        Expr::Register(base) => Some((base, 0)),
        Expr::Add(a, b) => match (exprs[a], exprs[b]) {
            (Expr::Register(base), Expr::Constant(offset))
            | (Expr::Constant(offset), Expr::Register(base)) => Some((base, offset)),
            _ => None,
        },
        _ => None,
    }
}

fn dominates_latch(graph: &Graph, lp: usize, call: usize) -> bool {
    let loop_ = &graph.loops[lp];
    let mut seen = vec![false; graph.edges.len()];
    let mut work = vec![loop_.head];
    while let Some(at) = work.pop() {
        if at == call || !loop_.inside[at] || std::mem::replace(&mut seen[at], true) {
            continue;
        }
        if at == loop_.tail {
            return false;
        }
        work.extend(graph.edges[at].iter().flatten().copied());
    }
    true
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
const RT: Reg = Reg::new(29);
const A: Reg = Reg::new(0);
const B: Reg = Reg::new(1);
const C: Reg = Reg::new(2);
const D: Reg = Reg::new(3);
const TEMP: Reg = Reg::new(8);
const LIMIT: Reg = Reg::new(13);
const WORK: Reg = Reg::new(14);

const fn guest(reg: u8) -> Reg {
    Reg::new(19 + reg)
}

struct RangeEmitter<'a> {
    asm: &'a mut Asm,
    exprs: &'a [Expr],
    slots: Vec<bool>,
    fail: Label,
    search: Label,
    range_cold: u32,
}

impl RangeEmitter<'_> {
    fn price(&mut self, minimum: Label, rows: Reg, shift: u32, short: Label) {
        self.asm.ldr_literal(C, minimum);
        self.asm.emit(mul(X, B, rows, A));
        self.asm.emit(cmp(X, B, C, Lsl, 0));
        self.asm
            .b_cond(if shift == 0 { Cond::Lo } else { Cond::Lt }, short);
    }

    /// The rounded test already rejected a nonnegative product in B.
    /// Add back the per-row savings left after rounding setup to whole batches.
    fn exact_price(&mut self, price: Label, rows: Reg, short: Label) {
        self.asm.adr(D, price);
        self.asm.emit_all(&[
            ldp(W, Index::Offset, C, TEMP, D, 0),
            ldr(W, D, D, 8),
            mul(X, B, B, C),
            mul(X, C, rows, TEMP),
            add(X, B, B, C, Lsl, 0),
            cmp(X, B, D, Lsl, 0),
        ]);
        self.asm.b_cond(Cond::Lo, short);
    }

    fn constant(&mut self, register: Reg, value: u32) {
        self.asm.emit(movz(W, register, value as u16, 0));
        if value >> 16 != 0 {
            self.asm.emit(movk(W, register, (value >> 16) as u16, 16));
        }
    }

    fn load(&mut self, id: ExprId, dst: Reg) {
        self.asm.emit(ldr(W, dst, SP, id as u32 * 4));
    }

    fn range(&mut self, write: bool, fail: Label) {
        let hit = self.asm.label();
        crate::facts::batch_span(self.asm, write, false, self.search, hit);
        self.asm.b(fail);
        self.asm.bind(hit);
    }

    /// Bound a row-varying stream in row order. A denial supplies the
    /// first inaccessible byte in the projected direction.
    fn row_range(
        &mut self,
        write: bool,
        offset: u32,
        step: ExprId,
        rows: ExprId,
        reverse: Label,
        miss: Label,
    ) {
        let backwards = self.asm.label();
        let forward_denial = self.asm.label();
        let narrow = self.asm.label();
        let done = self.asm.label();
        self.load(step, TEMP);
        self.asm.tbz(true, TEMP, 31, backwards);
        crate::facts::batch_span(self.asm, write, false, self.search, done);
        self.asm.b(forward_denial);

        let cold = self.asm.offset();
        self.asm.bind(backwards);
        self.asm.emit_all(&[
            ldr(W, A, SP, offset + 4),
            ldr(W, B, SP, offset),
            mvn(W, A, A),
            mvn(W, B, B),
        ]);
        crate::facts::batch_span(self.asm, write, true, reverse, done);
        self.asm.emit(ldr(W, B, SP, offset));
        self.asm.emit(mvn(W, B, B));
        self.asm.emit(sub(W, TEMP, Reg::ZR, TEMP, Lsl, 0));
        self.asm.b(narrow);

        self.asm.bind(forward_denial);
        self.asm.emit(ldr(W, B, SP, offset + 4));
        self.asm.bind(narrow);
        self.asm.cbz(W, false, TEMP, miss);
        self.load(rows, A);
        self.asm.emit_all(&[
            sub_imm(W, A, A, 1, false),
            mul(W, A, A, TEMP),
            sub(W, B, B, A, Lsl, 0),
            cmp(X, C, B, Lsl, 0),
        ]);
        self.asm.b_cond(Cond::Ls, miss);
        self.asm.emit_all(&[
            sub(X, A, C, B, Lsl, 0),
            sub_imm(X, A, A, 1, false),
            udiv(X, A, A, TEMP),
            add_imm(X, A, A, 1, false),
            cmp(W, A, LIMIT, Lsl, 0),
            csel(W, LIMIT, A, LIMIT, Cond::Lo),
        ]);
        self.asm.bind(done);
        // The backward setup adds four words to the forward path.
        self.range_cold += (self.asm.offset() - cold) as u32 / 4 - 4;
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
                self.range(false, self.fail);
                self.asm
                    .emit(add_ext(X, TEMP, WINDOW, TEMP, Extend::Uxtw, 0));
                self.asm.emit(ldr(W, dst, TEMP, 0));
            }
        }
        self.slots[id] = true;
        self.asm.emit(str(W, dst, SP, id as u32 * 4));
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Evaluation, tests::*};
    use super::*;
    use crate::CpuState;

    // spec: A64-TR-9
    #[test]
    fn scoped_rows_require_the_guard_and_reject_carried_reads_and_shared_rows() {
        #[derive(Clone, Copy, PartialEq, Debug)]
        enum Shape {
            Guarded,
            HighBytes,
            Carried,
            Shared,
        }
        for shape in [
            Shape::Guarded,
            Shape::HighBytes,
            Shape::Carried,
            Shape::Shared,
        ] {
            let mut trace = trace(|a| {
                let mut row = a.create_label();
                let mut body = a.create_label();
                let mut pretest = a.create_label();
                a.set_label(&mut row).unwrap();
                if shape == Shape::Shared {
                    a.mov(ebp, dword_ptr(esp + 8)).unwrap();
                }
                a.mov(ecx, ebx).unwrap();
                a.sub(ecx, 2).unwrap();
                a.jmp(pretest).unwrap();
                a.set_label(&mut body).unwrap();
                if shape == Shape::Carried {
                    a.mov(dword_ptr(edi), eax).unwrap();
                }
                if shape == Shape::HighBytes {
                    a.mov(al, byte_ptr(esi)).unwrap();
                } else {
                    a.mov(eax, dword_ptr(esi)).unwrap();
                }
                if shape == Shape::Shared {
                    a.mov(dword_ptr(ebp), eax).unwrap();
                    a.add(ebp, 4).unwrap();
                }
                a.mov(dword_ptr(edi), eax).unwrap();
                if shape == Shape::HighBytes {
                    a.mov(ah, byte_ptr(esi + 1)).unwrap();
                }
                a.add(edi, 4).unwrap();
                a.add(esi, 4).unwrap();
                a.sub(ecx, 2).unwrap();
                a.set_label(&mut pretest).unwrap();
                a.jns(body).unwrap();
                a.mov(dword_ptr(edi), eax).unwrap();
                a.add(edi, 4).unwrap();
                a.dec(edx).unwrap();
                a.jne(row).unwrap();
                a.int3().unwrap();
            });
            let latches = trace.row_latches();
            assert_eq!(latches.len(), 1);
            trace.separate_row_latches(&latches);
            let cpu = CpuState {
                regs: [7, 0, 8, 8, 0x50000, 0, 0x20000, 0x30000],
                ..CpuState::default()
            };
            let (mut domain, mut proof) = Domain::build(&trace, &cpu.regs).expect("row domain");
            let read = |addr| {
                if addr == cpu.regs[4] + 8 {
                    Some(0x40000)
                } else {
                    (cpu.regs[6]..cpu.regs[6] + 0x1000)
                        .contains(&addr)
                        .then_some(cpu.regs[0])
                }
            };
            let mut scratch = Evaluation::default();
            assert!(
                proof.evaluate(&cpu, &mut scratch, read).is_some(),
                "{shape:?} serial admission: {:?}",
                domain.checks
            );
            let mut worker = trace.clone();
            domain.prove_scoped_split(&mut proof, &mut worker);
            assert_eq!(
                proof.scoped_splits(),
                matches!(shape, Shape::Guarded | Shape::Shared),
                "{shape:?}",
            );
            assert_eq!(
                proof.plan(&cpu, &mut scratch, read).is_some(),
                shape == Shape::Guarded
            );

            if shape == Shape::Guarded {
                let mut short = cpu.clone();
                short.regs[3] = 1;
                assert!(proof.evaluate(&short, &mut scratch, read).is_none());
                // The same proof with the branch's guard absent cannot
                // supply a scoped artifact.
                let mut proof = Domain::build(&trace, &cpu.regs).unwrap().1;
                let (_, (_, guard)) = proof
                    .branches
                    .iter()
                    .find_map(|&(at, choice)| Some((at, choice?)))
                    .unwrap();
                domain.checks.retain(|&check| check != guard);
                domain.prove_scoped_split(&mut proof, &mut trace);
                assert!(!proof.scoped_splits());
            }
        }
    }
}
