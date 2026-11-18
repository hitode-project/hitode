//! Lower owner-only computation admission at a canonical unit entry.

use std::mem::offset_of;

use super::{
    Access, Check, Effect, Expr, ExprId, Form, GATHERED_WORDS, Limit, PARAMETER_WORDS, Proof, Var,
    VarId,
};
use crate::asm::{Asm, Label};
use crate::encode::Cond::*;
use crate::encode::Index::{Offset, Post, Pre};
use crate::encode::Shift::{Lsl, Lsr};
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::Runtime;

const fn r(n: u8) -> Reg {
    Reg::new(n)
}

const SP: Reg = r(31);
const ZR: Reg = r(31);
const RUNTIME: Reg = r(29);
const GUEST: [Reg; 8] = [r(19), r(20), r(21), r(22), r(23), r(24), r(25), r(26)];

// x6 appends ranges, x7 saves NZCV, and x8 addresses the fixed host frame.
// Internal calls use x17, leaving the incoming LR and shadow stack alone.
const RANGE_END: Reg = r(6);
const FLAGS: Reg = r(7);
const FRAME: Reg = r(8);
const LINK: Reg = r(17);
const ENTRIES: u32 = 0;
const WORD_COUNT: u32 = 8;
const CODE_HIT: u32 = 16;
const WORDS: u32 = 32;
const CACHE: u32 = WORDS + PARAMETER_WORDS as u32 * 8;

#[derive(Clone, Copy)]
enum RangeKind {
    FrameRead = 1,
    FrameWrite = 2,
    BasedRead = 4,
    BasedWrite = 8,
    Parameter = 16,
    Output = 32,
    Table = 64,
    Invariant = 128,
    Read = 256,
}

impl RangeKind {
    /// The symmetric form of the overlap checks in Proof::evaluate_core.
    fn conflicts(self) -> u32 {
        use RangeKind::*;
        let kinds: &[Self] = match self {
            FrameRead => &[Output, BasedWrite],
            FrameWrite => &[
                Output, Table, Parameter, Invariant, Read, BasedRead, BasedWrite,
            ],
            BasedRead => &[Output, FrameWrite],
            BasedWrite => &[
                Output, Table, Parameter, Invariant, Read, FrameRead, FrameWrite,
            ],
            Parameter | Table | Invariant => &[Output, FrameWrite, BasedWrite],
            Output => &[
                FrameRead, FrameWrite, BasedRead, BasedWrite, Parameter, Table, Invariant,
            ],
            Read => &[FrameWrite, BasedWrite],
        };
        kinds.iter().fold(0, |mask, kind| mask | *kind as u32)
    }
}

#[derive(Clone, Copy)]
enum Routine {
    Expr(ExprId),
    Gather(VarId),
}

#[derive(Clone, Copy)]
struct Cached {
    entry: Label,
    slot: u32,
}

struct Guard<'a> {
    asm: &'a mut Asm,
    proof: &'a Proof,
    reject: Label,
    read_pages: Label,
    write_pages: Label,
    record: Label,
    parameter: Label,
    exprs: Vec<Option<Cached>>,
    gathered: Vec<Option<Cached>>,
    pending: Vec<(Routine, Cached)>,
    cache_end: u32,
}

/// Fall through on admission, branch to `fail` on refusal, or leave a
/// write sharing an observed code line for Rust to decide at `cold`.
pub(crate) fn emit_guard(asm: &mut Asm, proof: &Proof, fail: Label, cold: Label) {
    let reject = asm.label();
    let read_pages = asm.label();
    let write_pages = asm.label();
    let record = asm.label();
    let parameter = asm.label();
    let mut guard = Guard {
        asm,
        proof,
        reject,
        read_pages,
        write_pages,
        record,
        parameter,
        exprs: vec![None; proof.exprs.len()],
        gathered: vec![None; proof.vars.len()],
        pending: Vec::new(),
        cache_end: CACHE,
    };
    guard.emit(fail, cold);
}

impl Guard<'_> {
    fn emit(&mut self, fail: Label, cold: Label) {
        let entry = self.asm.label();
        let body = self.asm.label();
        let accept = self.asm.label();
        self.asm.b(entry);
        self.asm.bind(body);
        for &check in &self.proof.checks {
            self.check(check);
        }
        for effect in &self.proof.effects {
            self.effect(effect);
        }
        self.asm.b(accept);

        // Emit only the expressions and gathers reached by the admission walk.
        // A routine can add its dependencies to this list.
        let mut ranges = self.proof.effects.len();
        while let Some((routine, cache)) = self.pending.pop() {
            self.asm.bind(cache.entry);
            match routine {
                Routine::Expr(id) => {
                    ranges += usize::from(matches!(self.proof.exprs[id], Expr::Word(_)));
                    self.expression(id, cache.slot);
                }
                Routine::Gather(id) => {
                    ranges += 1;
                    self.gathered(id, cache.slot);
                }
            }
        }
        self.page_check(false);
        self.page_check(true);
        self.record_range();
        self.parameter_word();

        let frame_size = self.cache_end + 16 * ranges as u32;
        self.asm.bind(entry);
        imm(self.asm, r(0), frame_size);
        self.asm.emit_all(&[
            mrs_nzcv(FLAGS),
            sub_ext(X, SP, SP, r(0), Extend::Uxtw, 0),
            mov_sp(FRAME, SP),
        ]);
        // Only the counters and cache markers need initialization.
        self.asm.emit(stp(X, Offset, ZR, ZR, FRAME, 0));
        self.asm.emit(str(X, ZR, FRAME, CODE_HIT));
        self.add_offset(RANGE_END, FRAME, self.cache_end);
        if self.cache_end != CACHE {
            self.asm.emit(add_imm(X, r(0), FRAME, CACHE, false));
            let zero = self.asm.label();
            self.asm.bind(zero);
            self.asm.emit_all(&[
                str_indexed(X, Post, ZR, r(0), 16),
                cmp(X, r(0), RANGE_END, Lsl, 0),
            ]);
            self.asm.b_cond(Ne, zero);
        }
        self.asm.b(body);

        self.asm.bind(self.reject);
        self.restore(frame_size);
        self.asm.b(fail);
        self.asm.bind(accept);
        self.asm.emit(ldr(X, r(1), FRAME, CODE_HIT));
        self.restore(frame_size);
        let plain = self.asm.label();
        self.asm.cbz(X, false, r(1), plain);
        self.asm.b(cold);
        self.asm.bind(plain);
    }

    fn restore(&mut self, frame_size: u32) {
        imm(self.asm, r(0), frame_size);
        self.asm.emit_all(&[
            add_ext(X, SP, FRAME, r(0), Extend::Uxtw, 0),
            msr_nzcv(FLAGS),
        ]);
    }

    fn reject_if(&mut self, cond: Cond) {
        // The guard can contain many specialized routines. Keep conditional
        // branches local and use the full branch displacement for rejection.
        let next = self.asm.label();
        self.asm.b_cond(cond.invert(), next);
        self.asm.b(self.reject);
        self.asm.bind(next);
    }

    fn call(&mut self, to: Label) {
        let resume = self.asm.label();
        self.asm.adr(LINK, resume);
        self.asm.b(to);
        self.asm.bind(resume);
    }

    fn push(&mut self, a: Reg, b: Reg) {
        self.asm.emit(stp(X, Pre, a, b, SP, -16));
    }

    fn pop(&mut self, a: Reg, b: Reg) {
        self.asm.emit(ldp(X, Post, a, b, SP, 16));
    }

    fn add_offset(&mut self, dst: Reg, base: Reg, offset: u32) {
        if offset < 4096 {
            self.asm.emit(add_imm(X, dst, base, offset, false));
        } else {
            imm(self.asm, dst, offset);
            self.asm.emit(add(X, dst, base, dst, Lsl, 0));
        }
    }

    fn check(&mut self, check: Check) {
        match check {
            Check::Equal(a, b) => {
                self.value(a);
                self.push(r(0), ZR);
                self.value(b);
                self.pop(r(1), ZR);
                self.asm.emit(cmp(W, r(0), r(1), Lsl, 0));
                self.reject_if(Ne);
            }
            Check::Positive(expr, max) => {
                self.value(expr);
                imm(self.asm, r(1), max - 1);
                self.asm
                    .emit_all(&[sub_imm(W, r(0), r(0), 1, false), cmp(W, r(0), r(1), Lsl, 0)]);
                self.reject_if(Hi);
            }
            Check::StackAlignment(n) => {
                imm(self.asm, r(0), n - 1);
                self.asm.emit(tst(W, GUEST[4], r(0), Lsl, 0));
                self.reject_if(Ne);
            }
        }
    }

    fn effect(&mut self, effect: &Effect) {
        use RangeKind::*;
        let empty = self.asm.label();
        self.interval(&effect.addr, &effect.limits, empty);
        self.end(effect.bytes);
        let write = matches!(effect.access, Access::FrameWrite | Access::Write);
        self.allow_pages(write);
        let kind = match effect.access {
            Access::FrameRead => FrameRead,
            Access::FrameWrite => FrameWrite,
            access
                if effect
                    .addr
                    .invariant()
                    .is_some_and(|a| self.proof.based_expression(a)) =>
            {
                if access == Access::Write {
                    BasedWrite
                } else {
                    BasedRead
                }
            }
            Access::Write => Output,
            Access::Read if self.proof.invariant(effect) => Invariant,
            Access::Read => Read,
        };
        self.range(kind);
        self.asm.bind(empty);
    }

    /// Produce the inclusive bounds in x0 and x1, or branch on an empty term.
    fn interval(&mut self, form: &Form, limits: &[Limit], empty: Label) {
        self.value(form.base);
        self.asm.emit(mov(X, r(1), r(0)));
        for &(var, coefficient) in &form.terms {
            self.push(r(0), r(1));
            self.value(coefficient);
            self.asm.emit(sbfx(X, r(0), r(0), 0, 32));
            self.push(r(0), ZR);
            self.bounds(var, limits);
            self.pop(r(2), ZR);
            self.pop(r(3), r(4));
            self.asm.emit(cmp(X, r(1), r(0), Lsl, 0));
            let nonempty = self.asm.label();
            self.asm.b_cond(Ge, nonempty);
            self.asm.b(empty);
            self.asm.bind(nonempty);
            // Each bound has at most 32 bits of magnitude and the
            // coefficient is signed 32-bit, so the products fit i64.
            self.asm.emit_all(&[
                mul(X, r(0), r(0), r(2)),
                mul(X, r(1), r(1), r(2)),
                cmp(X, r(0), r(1), Lsl, 0),
                csel(X, r(2), r(0), r(1), Le),
                csel(X, r(1), r(1), r(0), Le),
                adds(X, r(0), r(3), r(2), Lsl, 0),
            ]);
            self.reject_if(Vs);
            self.asm.emit(adds(X, r(1), r(4), r(1), Lsl, 0));
            self.reject_if(Vs);
        }
        self.asm.emit_all(&[
            and_imm(X, r(2), r(0), 0xffff_ffff_0000_0000),
            sub(X, r(1), r(1), r(2), Lsl, 0),
            mov(W, r(0), r(0)),
        ]);
    }

    fn bounds(&mut self, var: VarId, limits: &[Limit]) {
        match self.proof.vars[var] {
            Var::Counter(Some(count)) => {
                self.value(count);
                self.asm.emit(sub_imm(X, r(1), r(0), 1, false));
                self.asm.emit(mov(W, r(0), ZR));
            }
            Var::Remainder {
                divisor,
                normalized,
            } => {
                let trim = limits
                    .iter()
                    .filter(|l| l.var == var)
                    .map(|l| l.trim)
                    .max()
                    .unwrap_or(0);
                self.value(divisor);
                imm(self.asm, r(2), trim);
                self.asm.emit_all(&[
                    sub_imm(X, r(1), r(0), 1, false),
                    sub(X, r(1), r(1), r(2), Lsl, 0),
                ]);
                if normalized {
                    self.asm.emit(mov(W, r(0), ZR));
                } else {
                    self.asm
                        .emit_all(&[movz(W, r(2), 1, 0), sub(X, r(0), r(2), r(0), Lsl, 0)]);
                }
            }
            Var::Bounded(max) => {
                imm(self.asm, r(1), max);
                self.asm.emit(mov(W, r(0), ZR));
            }
            Var::Gathered(_) => self.cached_call(Routine::Gather(var)),
            Var::Counter(None) | Var::Either(_) => self.asm.b(self.reject),
        }
    }

    fn value(&mut self, id: ExprId) {
        match self.proof.exprs[id] {
            Expr::Constant(value) => imm(self.asm, r(0), value),
            Expr::Register(reg) => self.asm.emit(mov(W, r(0), GUEST[usize::from(reg)])),
            _ => self.cached_call(Routine::Expr(id)),
        }
    }

    fn cached_call(&mut self, routine: Routine) {
        let slot = match routine {
            Routine::Expr(id) => &mut self.exprs[id],
            Routine::Gather(id) => &mut self.gathered[id],
        };
        let cache = *slot.get_or_insert_with(|| {
            let cache = Cached {
                entry: self.asm.label(),
                slot: self.cache_end,
            };
            self.cache_end += 16;
            self.pending.push((routine, cache));
            cache
        });
        self.call(cache.entry);
    }

    fn expression(&mut self, id: ExprId, slot: u32) {
        self.add_offset(r(2), FRAME, slot);
        self.asm.emit(ldr(X, r(0), r(2), 0));
        let done = self.asm.label();
        self.asm.cbz(X, true, r(0), done);
        self.push(LINK, ZR);
        match self.proof.exprs[id] {
            Expr::Word(addr) => {
                self.value(addr);
                self.asm.emit(mov(X, r(1), r(0)));
                self.end(4);
                let kind = if self.proof.stack_expression(addr) {
                    RangeKind::FrameRead
                } else if self.proof.based_expression(addr) {
                    RangeKind::BasedRead
                } else {
                    RangeKind::Parameter
                };
                self.range(kind);
                self.call(self.parameter);
            }
            Expr::Add(a, b) | Expr::Mul(a, b) => {
                self.value(a);
                self.push(r(0), ZR);
                self.value(b);
                self.pop(r(1), ZR);
                self.asm.emit(match self.proof.exprs[id] {
                    Expr::Add(..) => add(W, r(0), r(1), r(0), Lsl, 0),
                    _ => mul(W, r(0), r(1), r(0)),
                });
            }
            Expr::Neg(a) => {
                self.value(a);
                self.asm.emit(sub(W, r(0), ZR, r(0), Lsl, 0));
            }
            Expr::Constant(_) | Expr::Register(_) => unreachable!(),
        }
        self.add_offset(r(2), FRAME, slot);
        self.asm
            .emit_all(&[orr_imm(X, r(0), r(0), 1 << 32), str(X, r(0), r(2), 0)]);
        self.pop(LINK, ZR);
        self.asm.bind(done);
        self.asm.emit_all(&[mov(W, r(0), r(0)), br(LINK)]);
    }

    fn gathered(&mut self, id: VarId, slot: u32) {
        let Var::Gathered(addr) = &self.proof.vars[id] else {
            unreachable!();
        };
        self.add_offset(r(2), FRAME, slot);
        self.asm.emit(ldr(X, r(0), r(2), 0));
        let ready = self.asm.label();
        let scan = self.asm.label();
        self.asm.cbz(X, false, r(0), scan);
        self.asm.emit(ldr(X, r(1), r(2), 8));
        self.asm.b(ready);
        self.asm.bind(scan);
        self.push(LINK, ZR);
        self.interval(addr, &[], self.reject);
        self.end(4);
        self.range(RangeKind::Table);
        self.push(r(0), r(1));

        // Euclid's algorithm on the absolute address coefficients.
        self.asm.emit(mov(W, r(0), ZR));
        for &(_, coefficient) in &addr.terms {
            self.push(r(0), ZR);
            self.value(coefficient);
            self.pop(r(1), ZR);
            self.asm.emit_all(&[
                cmp_imm(W, r(0), 0, false),
                sub(W, r(2), ZR, r(0), Lsl, 0),
                csel(W, r(2), r(2), r(0), Mi),
                mov(W, r(0), r(1)),
            ]);
            let gcd = self.asm.label();
            let done = self.asm.label();
            self.asm.bind(gcd);
            self.asm.cbz(W, false, r(2), done);
            self.asm.emit_all(&[
                udiv(W, r(1), r(0), r(2)),
                msub(W, r(1), r(1), r(2), r(0)),
                mov(W, r(0), r(2)),
                mov(W, r(2), r(1)),
            ]);
            self.asm.b(gcd);
            self.asm.bind(done);
        }
        self.pop(r(3), r(4));
        self.asm.emit_all(&[
            cmp_imm(W, r(0), 0, false),
            csinc(W, r(5), r(0), ZR, Ne), // stride, at least one
            sub_imm(X, r(4), r(4), 4, false),
            sub(X, r(4), r(4), r(3), Lsl, 0),
            udiv(X, r(4), r(4), r(5)),
            add_imm(X, r(4), r(4), 1, false),
            ldr(X, r(0), FRAME, ENTRIES),
            add(X, r(0), r(0), r(4), Lsl, 0),
            cmp_imm(X, r(0), GATHERED_WORDS as u32 >> 12, true),
        ]);
        self.reject_if(Hi);
        self.asm.emit_all(&[
            str(X, r(0), FRAME, ENTRIES),
            mvn(W, r(13), ZR),
            mov(W, r(14), ZR),
        ]);

        let next = self.asm.label();
        self.asm.bind(next);
        self.push(r(3), r(4));
        self.push(r(5), ZR);
        self.push(r(13), r(14));
        self.asm
            .emit_all(&[mov(W, r(0), r(3)), add_imm(X, r(1), r(0), 4, false)]);
        self.allow_pages(false);
        self.load_word();
        self.pop(r(13), r(14));
        self.pop(r(5), ZR);
        self.pop(r(3), r(4));
        self.asm.emit_all(&[
            cmp(W, r(0), r(13), Lsl, 0),
            csel(W, r(13), r(0), r(13), Lo),
            cmp(W, r(0), r(14), Lsl, 0),
            csel(W, r(14), r(0), r(14), Hi),
            add(X, r(3), r(3), r(5), Lsl, 0),
            subs_imm(X, r(4), r(4), 1, false),
        ]);
        self.asm.b_cond(Ne, next);
        self.add_offset(r(2), FRAME, slot);
        self.asm.emit_all(&[
            orr_imm(X, r(0), r(13), 1 << 32),
            mov(X, r(1), r(14)),
            str(X, r(0), r(2), 0),
            str(X, r(1), r(2), 8),
        ]);
        self.pop(LINK, ZR);
        self.asm.bind(ready);
        self.asm.emit_all(&[mov(W, r(0), r(0)), br(LINK)]);
    }

    // Turn the inclusive upper bound into an exclusive end without wrapping.
    fn end(&mut self, bytes: u32) {
        imm(self.asm, r(2), bytes.wrapping_neg());
        self.asm.emit(cmp(X, r(1), r(2), Lsl, 0));
        self.reject_if(Hi);
        imm(self.asm, r(2), bytes);
        self.asm.emit(add(X, r(1), r(1), r(2), Lsl, 0));
    }

    fn allow_pages(&mut self, write: bool) {
        self.call(if write {
            self.write_pages
        } else {
            self.read_pages
        });
    }

    // Check the guest span x0..x1, preserving it for the caller.
    fn page_check(&mut self, write: bool) {
        self.asm.bind(if write {
            self.write_pages
        } else {
            self.read_pages
        });
        self.asm.emit(cmp(X, r(1), r(0), Lsl, 0));
        self.reject_if(Ls);
        self.asm.emit_all(&[
            ldr(W, r(3), RUNTIME, offset_of!(Runtime, memory_base) as u32),
            cmp(W, r(0), r(3), Lsl, 0),
        ]);
        self.reject_if(Lo);
        self.asm.emit_all(&[
            sub_imm(X, r(4), r(1), 1, false),
            sub(X, r(4), r(4), r(3), Lsl, 0),
            sub(X, r(3), r(0), r(3), Lsl, 0),
            lsr_imm(X, r(4), r(4), 12),
            lsr_imm(X, r(3), r(3), 12),
            ldr(W, r(5), RUNTIME, offset_of!(Runtime, memory_pages) as u32),
            cmp(X, r(4), r(5), Lsl, 0),
        ]);
        self.reject_if(Hs);

        self.asm.emit_all(&[
            ldr(X, r(13), RUNTIME, offset_of!(Runtime, plain_pages) as u32),
            lsr_imm(X, r(14), r(4), 6), // last word
            lsr_imm(X, r(15), r(3), 6), // current word
            mvn(X, r(5), ZR),
            mvn(W, r(16), r(4)), // low six bits are 63 - (last % 64)
            shiftv(X, Lsr, r(16), r(5), r(16)),
            shiftv(X, Lsl, r(5), r(5), r(3)),
        ]);
        let next = self.asm.label();
        self.asm.bind(next);
        self.asm.emit_all(&[
            cmp(X, r(15), r(14), Lsl, 0),
            csel(X, r(4), r(16), r(5), Eq),
            and(X, r(4), r(4), r(5), Lsl, 0),
            add(X, r(3), r(15), r(15), Lsl, 1),
            add(X, r(3), r(13), r(3), Lsl, 3),
            ldr(X, r(2), r(3), if write { 8 } else { 0 }),
            and(X, r(2), r(2), r(4), Lsl, 0),
            cmp(X, r(2), r(4), Lsl, 0),
        ]);
        self.reject_if(Ne);
        if write {
            self.code_check();
        }
        self.asm.emit_all(&[
            add_imm(X, r(15), r(15), 1, false),
            mvn(X, r(5), ZR),
            cmp(X, r(15), r(14), Lsl, 0),
        ]);
        self.asm.b_cond(Ls, next);
        self.asm.emit(br(LINK));
    }

    // x3 addresses the current bitmap group and x4 selects its covered pages.
    // Only marked pages need a code-lines load. A line hit may be a byte miss,
    // so remember it while checking the rest of the proof.
    fn code_check(&mut self) {
        self.asm
            .emit_all(&[ldr(X, r(2), r(3), 16), and(X, r(2), r(2), r(4), Lsl, 0)]);
        let plain = self.asm.label();
        let next = self.asm.label();
        let done = self.asm.label();
        let hit = self.asm.label();
        self.asm.cbz(X, false, r(2), plain);
        self.push(r(13), r(14));
        self.push(r(15), r(16));
        self.asm.emit_all(&[
            ldr(W, r(3), RUNTIME, offset_of!(Runtime, memory_base) as u32),
            // Convert the relative group start to an absolute guest page.
            lsl_imm(X, r(15), r(15), 6),
            add(X, r(15), r(15), r(3), Lsr, 12),
            ldr(X, r(13), RUNTIME, offset_of!(Runtime, code_lines) as u32),
            sub_imm(X, r(14), r(1), 1, false),
        ]);
        self.asm.bind(next);
        self.asm.emit_all(&[
            rbit(X, r(3), r(2)),
            clz(X, r(3), r(3)),
            add(X, r(3), r(15), r(3), Lsl, 0),
            ldr_reg(X, r(4), r(13), r(3), true),
            lsr_imm(X, r(5), r(0), 6),
            cmp(X, r(3), r(5), Lsr, 6),
            csel(X, r(5), r(5), ZR, Eq),
            mvn(X, r(16), ZR),
            shiftv(X, Lsl, r(5), r(16), r(5)),
            and(X, r(4), r(4), r(5), Lsl, 0),
            lsr_imm(X, r(5), r(14), 6),
            cmp(X, r(3), r(5), Lsr, 6),
            mvn(W, r(5), r(5)),
            csel(X, r(5), r(5), ZR, Eq),
            shiftv(X, Lsr, r(5), r(16), r(5)),
            tst(X, r(4), r(5), Lsl, 0),
        ]);
        self.asm.b_cond(Ne, hit);
        self.asm.emit_all(&[
            sub_imm(X, r(3), r(2), 1, false),
            ands(X, r(2), r(2), r(3), Lsl, 0),
        ]);
        self.asm.b_cond(Ne, next);
        self.asm.b(done);
        self.asm.bind(hit);
        self.asm.emit(str(X, r(2), FRAME, CODE_HIT));
        self.asm.bind(done);
        self.pop(r(15), r(16));
        self.pop(r(13), r(14));
        self.asm.bind(plain);
    }

    fn range(&mut self, kind: RangeKind) {
        imm(self.asm, r(2), kind.conflicts());
        imm(self.asm, r(3), kind as u32);
        self.call(self.record);
    }

    // Test x0..x1 against earlier ranges of the selected kinds and append it.
    // Keep x0 and x1 for the caller.
    fn record_range(&mut self) {
        self.asm.bind(self.record);
        self.add_offset(r(4), FRAME, self.cache_end);
        let next = self.asm.label();
        let keep = self.asm.label();
        let advance = self.asm.label();
        self.asm.bind(next);
        self.asm.emit(cmp(X, r(4), RANGE_END, Lsl, 0));
        self.asm.b_cond(Eq, keep);
        self.asm.emit_all(&[
            ldp(X, Post, r(5), r(13), r(4), 16),
            tst(X, r(5), r(2), Lsl, 32),
        ]);
        self.asm.b_cond(Eq, advance);
        self.asm.emit(cmp(X, r(0), r(13), Lsl, 0));
        self.asm.b_cond(Hs, advance);
        self.asm
            .emit_all(&[mov(W, r(5), r(5)), cmp(X, r(5), r(1), Lsl, 0)]);
        self.reject_if(Lo);
        self.asm.bind(advance);
        self.asm.b(next);
        self.asm.bind(keep);
        self.asm.emit_all(&[
            orr(X, r(3), r(0), r(3), Lsl, 32),
            stp(X, Post, r(3), r(1), RANGE_END, 16),
            br(LINK),
        ]);
    }

    fn load_word(&mut self) {
        self.asm.emit_all(&[
            ldr(X, r(3), RUNTIME, offset_of!(Runtime, alias) as u32),
            ldr_reg(W, r(0), r(3), r(0), false),
        ]);
    }

    // The evaluator shares a budget and a cached value by address across
    // Word expressions, independent of the expression cache or gather budget.
    fn parameter_word(&mut self) {
        self.asm.bind(self.parameter);
        self.asm.emit_all(&[
            ldr(X, r(4), FRAME, WORD_COUNT),
            add_imm(X, r(3), FRAME, WORDS, false),
            add(X, r(5), r(3), r(4), Lsl, 3),
        ]);
        let next = self.asm.label();
        let miss = self.asm.label();
        let hit = self.asm.label();
        self.asm.bind(next);
        self.asm.emit(cmp(X, r(3), r(5), Lsl, 0));
        self.asm.b_cond(Eq, miss);
        self.asm.emit_all(&[
            ldr_indexed(X, Post, r(2), r(3), 8),
            cmp(X, r(0), r(2), Lsr, 32),
        ]);
        self.asm.b_cond(Eq, hit);
        self.asm.b(next);
        self.asm.bind(hit);
        self.asm.emit_all(&[mov(W, r(0), r(2)), br(LINK)]);
        self.asm.bind(miss);
        self.asm
            .emit(cmp_imm(X, r(4), PARAMETER_WORDS as u32, false));
        self.reject_if(Eq);
        self.push(LINK, r(0));
        self.push(r(4), r(5));
        self.allow_pages(false);
        self.load_word();
        self.pop(r(4), r(5));
        self.pop(LINK, r(3));
        self.asm.emit_all(&[
            orr(X, r(3), r(0), r(3), Lsl, 32),
            str(X, r(3), r(5), 0),
            add_imm(X, r(4), r(4), 1, false),
            str(X, r(4), FRAME, WORD_COUNT),
            br(LINK),
        ]);
    }
}

fn imm(asm: &mut Asm, dst: Reg, value: u32) {
    asm.emit(movz(W, dst, value as u16, 0));
    if value >> 16 != 0 {
        asm.emit(movk(W, dst, (value >> 16) as u16, 16));
    }
}
