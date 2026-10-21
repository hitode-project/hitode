//! What each instruction of an x87 run does to values held as exact pairs
//! of doubles (A64F-FP-7).
//!
//! A host register holds a zero or a normal extended value `x` as its
//! high lane `h`, `x` rounded to 53 bits, and its low lane `l = x - h`,
//! exactly; zeros keep `l` zero. The exponent of `x` is within -959 and
//! 1022, so both lanes are normal doubles. A value whose low lane is known
//! to be zero at translation time is narrow.
//!
//! Arithmetic runs natively under the control word the guest had when the
//! unit was translated, if it masks every exception, rounds to nearest and
//! has precision 24, 53 or 64; each instruction checks the control word is
//! still that one. Precision 53 takes narrow operands and rounds once in
//! binary64. Precision 24 also needs single-exact operands, then rounds
//! that double's significand to 24 bits in integers, keeping the extended
//! exponent range; binary64 is wide enough that the second rounding is
//! innocuous for these operations. Precision 64 approximates the exact result as `H + t` from
//! fused and error-free operations, rounds `t` to the 64-bit grid of `H`
//! with a magic constant, and leaves for the integer path unless the error
//! bound keeps that rounding and the inexact flag certain. The integer path
//! also takes every other case, in cold code, through the extended
//! encoding.

use adv_cpu_core::floating::fast::{Fast, Op as Arith, PE, SINGLE_TAIL, Source};

use crate::asm::Label;
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime;

use super::super::{Translator, WINDOW, X0, X1, X2, X3, X5, X6, X13, X16, X17};
use super::{C0, C1, C2, C3, V0, V1, V2, V3, V4, V5, X7, X8, X87};

const V6: VReg = VReg::new(6);
/// The lowest and the span of biased binary64 exponents a run holds.
const LOW: u32 = 64;
const SPAN: u32 = 1981;
/// The biased exponents of `H` the precision-64 path takes, so that every
/// error term it needs exact stays normal.
const WIDE_LOW: u32 = 123;
const WIDE_SPAN: u32 = 1920;
const FRACTION: u64 = (1 << 52) - 1;

/// Host register `n` of the cache.
const fn v(n: u8) -> VReg {
    VReg::new(24 + n)
}

/// The precision a control word lets arithmetic run natively in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Precision {
    Single,
    Double,
    Extended,
}

fn precision(control: u16) -> Option<Precision> {
    if control & 0x3f != 0x3f || control >> 10 & 3 != 0 {
        return None;
    }
    match control >> 8 & 3 {
        0 => Some(Precision::Single),
        2 => Some(Precision::Double),
        3 => Some(Precision::Extended),
        _ => None,
    }
}

/// An arithmetic instruction on the integer path, in cold code.
pub(in super::super) struct Generic {
    label: Label,
    join: Label,
    op: Arith,
    reverse: bool,
    /// The host registers of the destination and the source, `None` for
    /// a memory source in d1.
    dest: u8,
    source: Option<u8>,
    to: u8,
    /// Whether the run takes the result as narrow.
    narrow: bool,
}

/// An operand: its high lane, and whether its low lane is zero.
#[derive(Clone, Copy)]
struct Operand {
    high: VReg,
    reg: Option<u8>,
    narrow: bool,
}

/// Register use as in `x87`, with v0 to v6 scratch.
impl Translator<'_> {
    fn x87_narrow(&self, n: u8) -> bool {
        self.x87.model.narrow & (1 << n) != 0
    }

    /// Records host register `n` as holding a new value.
    fn x87_written(&mut self, n: u8, narrow: bool) {
        let model = &mut self.x87.model;
        model.narrow = model.narrow & !(1 << n) | u8::from(narrow) << n;
    }

    fn operand(&self, i: u8) -> Operand {
        let n = self.x87.model.reg(i);
        Operand {
            high: v(n),
            reg: Some(n),
            narrow: self.x87_narrow(n),
        }
    }

    /// What cached instruction `x` does once its values are in host
    /// registers; whether it records the data pointer and the instruction
    /// pointer and opcode, and its integer path if any. C1 need not be
    /// computed when `c1_dead`.
    pub(super) fn pair_body(
        &mut self,
        x: X87,
        bail: Label,
        c1_dead: bool,
    ) -> (bool, bool, Option<Generic>) {
        let dp = x.mem.is_some();
        let mut generic = None;
        match x.fast {
            Fast::Wait | Fast::LoadControl | Fast::StoreControl | Fast::StoreStatus => {
                if x.fast == Fast::StoreStatus {
                    self.x87_status();
                }
                let done = self.x87_body(x, bail);
                if x.fast == Fast::LoadControl {
                    self.emit(mov(W, X8, X0));
                }
                return (done.0, done.1, None);
            }
            Fast::Load(Source::St(index)) => {
                self.x87_push_copy(index);
                self.clear_c1();
            }
            Fast::Load(source) => {
                let to = self.x87_alloc();
                self.memory_bounded(source, v(to), (LOW, SPAN), bail);
                self.x87.model.push();
                self.x87_set(0, to, true);
                self.clear_c1();
            }
            Fast::Constant(one) => {
                let to = self.x87_alloc();
                if one {
                    self.emit_all(&[movz(X, X16, 0x3ff0, 48), fmov_to_v(X, v(to), X16)]);
                } else {
                    self.emit(vzero(false, v(to)));
                }
                self.x87.model.push();
                self.x87_set(0, to, true);
                self.clear_c1();
            }
            Fast::Store {
                width,
                integer,
                pop,
            } => self.pair_store(width, integer, pop, bail),
            Fast::StoreSt { index, pop } => {
                self.x87_copy(index, 0);
                self.clear_c1();
                if pop {
                    self.x87_pop();
                }
            }
            Fast::Arith {
                op,
                dest,
                source,
                reverse,
                pop,
            } => generic = Some(self.pair_arith(op, dest, source, reverse, pop, bail, c1_dead)),
            Fast::Compare { source, pops } => self.pair_compare(source, pops, bail),
            Fast::Exchange(index) => {
                self.x87_exchange(index);
                self.clear_c1();
            }
            Fast::Negate | Fast::Absolute => {
                let from = self.x87.model.reg(0);
                let to = self.x87_alloc();
                if x.fast == Fast::Negate {
                    self.emit(vfneg2d(v(to), v(from)));
                } else {
                    // Both lanes take the sign of the high one off.
                    self.emit_all(&[
                        fmov_from_v(X, X16, v(from)),
                        and_imm(X, X16, X16, 1 << 63),
                        vdup(Arr::D2, V0, X16),
                        vlogic(Logic::Eor, true, v(to), v(from), V0),
                    ]);
                }
                let narrow = self.x87_narrow(from);
                self.x87_set(0, to, narrow);
                self.clear_c1();
            }
            Fast::Round | Fast::Scale | Fast::Examine => unreachable!("not cached"),
        }
        (dp, true, generic)
    }

    /// ST(i) = the new value in host register `n`.
    fn x87_set(&mut self, i: u8, n: u8, narrow: bool) {
        self.x87.model.set(i, super::cache::Slot::Dirty(n));
        self.x87_written(n, narrow);
    }

    fn pair_store(&mut self, width: u8, integer: bool, pop: bool, bail: Label) {
        let n = self.x87.model.reg(0);
        if width == 8 && !integer {
            // The high lane is the value rounded to nearest; a low lane
            // that is not zero makes it inexact, rounded up when its sign
            // differs.
            self.emit(fmov_from_v(X, X2, v(n)));
            if !self.x87_narrow(n) {
                let (exact, flagged) = (self.asm.label(), self.asm.label());
                self.emit_all(&[vumov(3, X16, v(n), 1), lsl_imm(X, X17, X16, 1)]);
                self.asm.cbz(X, false, X17, exact);
                // Inexact needs rounding to nearest and PE masked.
                self.emit(and_imm(W, X17, X8, 0xc00));
                self.asm.cbz(W, true, X17, bail);
                self.emit(and_imm(W, X17, X8, u64::from(PE)));
                self.asm.cbz(W, false, X17, bail);
                self.emit_all(&[
                    orr_imm(W, X13, X13, u64::from(PE)),
                    eor(X, X17, X16, X2, Lsl, 0),
                    lsr_imm(X, X17, X17, 63),
                    bfi(W, X13, X17, 9, 1),
                ]);
                self.asm.b(flagged);
                self.asm.bind(exact);
                self.clear_c1();
                self.asm.bind(flagged);
            } else {
                self.clear_c1();
            }
        } else {
            self.store_rounding(integer, bail);
            // The sticky double: the top 53 bits of the value, the lowest
            // ORed with the rest. A low lane of the other sign takes one
            // unit off the high lane's magnitude.
            if self.x87_narrow(n) {
                self.emit_all(&[movz(W, X3, 0, 0), vmov(false, V0, v(n))]);
            } else {
                self.emit_all(&[
                    fmov_from_v(X, X16, v(n)),
                    vumov(3, X17, v(n), 1),
                    lsl_imm(X, X0, X17, 1),
                    cmp_imm(X, X0, 0, false),
                    cset(W, X3, Cond::Ne),
                    eor(X, X0, X17, X16, Lsl, 0),
                    lsr_imm(X, X0, X0, 63),
                    and(W, X0, X0, X3, Lsl, 0),
                    sub(X, X16, X16, X0, Lsl, 0),
                    orr(X, X16, X16, X3, Lsl, 0),
                    fmov_to_v(X, V0, X16),
                ]);
            }
            self.store_sticky(width, integer, bail);
            self.store_flags(bail);
        }
        self.site();
        let size = if width == 8 { X } else { W };
        self.emit(str_reg(size, X2, WINDOW, X6, false));
        if pop {
            self.x87_pop();
        }
    }

    fn pair_compare(&mut self, source: Source, pops: u8, bail: Label) {
        let a = self.operand(0);
        let b = match source {
            Source::St(i) => self.operand(i),
            source => {
                self.memory_operand(source, V1, None, bail);
                Operand {
                    high: V1,
                    reg: None,
                    narrow: true,
                }
            }
        };
        // The high lanes order the values unless equal; the low ones then.
        self.emit(fcmp(a.high, b.high));
        if !(a.narrow && b.narrow) {
            let decided = self.asm.label();
            self.asm.b_cond(Cond::Ne, decided);
            self.low(a, V2);
            self.low(b, V3);
            self.emit(fcmp(V2, V3));
            self.asm.bind(decided);
        }
        self.emit_all(&[
            cset(W, X16, Cond::Eq),
            cset(W, X17, Cond::Mi),
            movz(W, X2, (C0 | C1 | C2 | C3) as u16, 0),
            bic(W, X13, X13, X2, Lsl, 0),
            orr(W, X13, X13, X16, Lsl, 14),
            orr(W, X13, X13, X17, Lsl, 8),
        ]);
        if pops == 2 {
            self.x87_free(1);
        }
        for _ in 0..pops {
            self.x87_pop();
        }
    }

    /// The low lane of `operand` as a scalar in `d`.
    fn low(&mut self, operand: Operand, d: VReg) {
        match operand.reg {
            Some(n) if !operand.narrow => self.emit(dup_high(d, v(n))),
            _ => self.emit(vzero(false, d)),
        }
    }

    /// Branches to `to` unless the control word is the one translated for.
    fn pair_guard(&mut self, to: Label) {
        let control = u32::from(self.control);
        if control >> 12 != 0 {
            self.emit(sub_imm(W, X16, X8, control >> 12, true));
            self.emit(sub_imm(W, X16, X16, control & 0xfff, false));
        } else {
            self.emit(sub_imm(W, X16, X8, control, false));
        }
        self.asm.cbz(W, true, X16, to);
    }

    #[allow(clippy::too_many_arguments)]
    fn pair_arith(
        &mut self,
        op: Arith,
        dest: u8,
        source: Source,
        reverse: bool,
        pop: bool,
        bail: Label,
        c1_dead: bool,
    ) -> Generic {
        let d = self.operand(dest);
        let s = match source {
            Source::St(i) => self.operand(i),
            source => {
                self.memory_operand(source, V1, None, bail);
                Operand {
                    high: V1,
                    reg: None,
                    narrow: true,
                }
            }
        };
        let to = self.x87_alloc();
        let (label, join) = (self.asm.label(), self.asm.label());
        let (a, b) = if reverse { (s, d) } else { (d, s) };
        let precision = precision(self.control);
        match precision {
            Some(Precision::Single) => self.pair_double(op, a, b, to, label, c1_dead, true),
            Some(Precision::Double) => self.pair_double(op, a, b, to, label, c1_dead, false),
            Some(Precision::Extended) => self.pair_extended(op, a, b, to, label, c1_dead),
            None => self.asm.b(label),
        }
        self.asm.bind(join);
        let narrow = matches!(precision, Some(Precision::Single | Precision::Double));
        self.x87_set(dest, to, narrow);
        if pop {
            self.x87_pop();
        }
        Generic {
            label,
            join,
            op,
            reverse,
            dest: d.reg.expect("the destination is a register"),
            source: s.reg,
            to,
            narrow,
        }
    }

    /// Precision 53, or 24 when `single`: narrow operands, one rounding,
    /// the error from the residual, then for 24 the significand rounded.
    /// Falls through to the join.
    #[allow(clippy::too_many_arguments)]
    fn pair_double(
        &mut self,
        op: Arith,
        a: Operand,
        b: Operand,
        to: u8,
        generic: Label,
        c1_dead: bool,
        single: bool,
    ) {
        self.pair_guard(generic);
        match (a.narrow, b.narrow) {
            (true, true) => {}
            (false, true) => self.emit(vumov(3, X16, a.high, 1)),
            (true, false) => self.emit(vumov(3, X16, b.high, 1)),
            (false, false) => self.emit_all(&[
                vumov(3, X16, a.high, 1),
                vumov(3, X17, b.high, 1),
                orr(X, X16, X16, X17, Lsl, 0),
            ]),
        }
        if !(a.narrow && b.narrow) {
            self.emit(lsl_imm(X, X16, X16, 1));
            self.asm.cbz(X, true, X16, generic);
        }
        if single {
            self.emit_all(&[
                fmov_from_v(X, X16, a.high),
                fmov_from_v(X, X17, b.high),
                orr(X, X16, X16, X17, Lsl, 0),
                and_imm(X, X16, X16, SINGLE_TAIL),
            ]);
            self.asm.cbz(X, true, X16, generic);
        }
        let float = match op {
            Arith::Add => Float::Fadd,
            Arith::Sub => Float::Fsub,
            Arith::Mul => Float::Fmul,
            Arith::Div => Float::Fdiv,
        };
        let r = v(to);
        let (zero, exact, join) = (self.asm.label(), self.asm.label(), self.asm.label());
        let round = if single { self.asm.label() } else { join };
        self.emit(dfloat(float, r, a.high, b.high));
        // Rounding to 24 bits may carry into the next binade.
        let span = if single { SPAN - 1 } else { SPAN };
        self.pair_range(r, zero, (LOW, span), generic);
        if c1_dead {
            self.asm.tbz(true, X13, 5, round);
        }
        // x16 the bits of the error, whose sign is that of exact - rounded.
        match op {
            Arith::Add | Arith::Sub => {
                let b = if op == Arith::Sub {
                    self.emit(unary(Unary::Fneg, V5, b.high));
                    V5
                } else {
                    b.high
                };
                self.two_sum(a.high, b, r, V3);
                self.emit(fmov_from_v(X, X16, V3));
            }
            Arith::Mul => self.emit_all(&[fnmsub(V3, a.high, b.high, r), fmov_from_v(X, X16, V3)]),
            Arith::Div => self.emit_all(&[
                fmsub(V3, r, b.high, a.high),
                fmov_from_v(X, X16, V3),
                fmov_from_v(X, X17, b.high),
                and_imm(X, X17, X17, 1 << 63),
                eor(X, X16, X16, X17, Lsl, 0),
            ]),
        }
        self.emit(lsl_imm(X, X17, X16, 1));
        self.asm.cbz(X, false, X17, exact);
        self.inexact(r, c1_dead);
        self.asm.b(round);
        self.asm.bind(exact);
        if !c1_dead {
            self.clear_c1();
        }
        if single {
            self.asm.bind(round);
            self.pair_single(r, join, c1_dead);
        }
        self.asm.b(join);
        self.asm.bind(zero);
        self.exact_zero(op, a, b, generic);
        if !c1_dead {
            self.clear_c1();
        }
        self.asm.bind(join);
    }

    /// Rounds the significand of the double in `r` to 24 bits, to nearest
    /// even; a change sets PE and, unless `c1_dead`, C1 if it grew. Uses
    /// x0, x1, x3 and x17.
    fn pair_single(&mut self, r: VReg, join: Label, c1_dead: bool) {
        self.emit_all(&[fmov_from_v(X, X0, r), and_imm(X, X3, X0, SINGLE_TAIL)]);
        self.asm.cbz(X, false, X3, join);
        self.mov_imm(X1, (1 << 28) - 1);
        self.emit_all(&[
            ubfx(X, X17, X0, 29, 1),
            add(X, X1, X0, X1, Lsl, 0),
            add(X, X1, X1, X17, Lsl, 0),
            and_imm(X, X1, X1, !SINGLE_TAIL),
            orr_imm(W, X13, X13, u64::from(PE)),
        ]);
        if !c1_dead {
            self.emit_all(&[
                cmp(X, X1, X0, Lsl, 0),
                cset(W, X17, Cond::Hi),
                bfi(W, X13, X17, 9, 1),
            ]);
        }
        self.emit(fmov_to_v(X, r, X1));
    }

    /// Sets PE and, unless `c1_dead`, C1 from the error bits in x16 and
    /// the result's sign in `r`; uses x17.
    fn inexact(&mut self, r: VReg, c1_dead: bool) {
        self.emit(orr_imm(W, X13, X13, u64::from(PE)));
        if !c1_dead {
            self.emit_all(&[
                fmov_from_v(X, X17, r),
                eor(X, X17, X17, X16, Lsl, 0),
                lsr_imm(X, X17, X17, 63),
                bfi(W, X13, X17, 9, 1),
            ]);
        }
    }

    /// TwoSum: `s` = a + b rounded, `e` the exact rest; uses d0.
    fn two_sum(&mut self, a: VReg, b: VReg, s: VReg, e: VReg) {
        self.emit_all(&[
            dfloat(Float::Fsub, V0, s, a),
            dfloat(Float::Fsub, e, s, V0),
            dfloat(Float::Fsub, e, a, e),
            dfloat(Float::Fsub, V0, b, V0),
            dfloat(Float::Fadd, e, e, V0),
        ]);
    }

    /// Branches to `zero` for a zero in `r`, else to `out` unless its
    /// biased exponent is within `bounds`; leaves its bits in x16 and the
    /// exponent in w7.
    fn pair_range(&mut self, r: VReg, zero: Label, bounds: (u32, u32), out: Label) {
        self.emit_all(&[fmov_from_v(X, X16, r), lsl_imm(X, X17, X16, 1)]);
        self.asm.cbz(X, false, X17, zero);
        self.emit_all(&[
            ubfx(X, X7, X16, 52, 11),
            sub_imm(W, X17, X7, bounds.0, false),
            cmp_imm(W, X17, bounds.1, false),
        ]);
        self.asm.b_cond(Cond::Hi, out);
    }

    /// A zero result is exact unless a product or quotient underflowed.
    fn exact_zero(&mut self, op: Arith, a: Operand, b: Operand, generic: Label) {
        match op {
            Arith::Add | Arith::Sub => {}
            Arith::Mul => {
                let ok = self.asm.label();
                self.emit(fcmp_zero(a.high));
                self.asm.b_cond(Cond::Eq, ok);
                self.emit(fcmp_zero(b.high));
                self.asm.b_cond(Cond::Ne, generic);
                self.asm.bind(ok);
            }
            Arith::Div => {
                self.emit(fcmp_zero(a.high));
                self.asm.b_cond(Cond::Ne, generic);
            }
        }
    }

    /// Precision 64. `H + t` approximates the exact result; x3 is zero
    /// when it is exact. Falls through to the join.
    fn pair_extended(
        &mut self,
        op: Arith,
        a: Operand,
        b: Operand,
        to: u8,
        generic: Label,
        c1_dead: bool,
    ) {
        self.pair_guard(generic);
        let (al, bl) = (
            (!a.narrow).then(|| {
                self.low(a, V2);
                V2
            }),
            (!b.narrow).then(|| {
                self.low(b, V3);
                V3
            }),
        );
        // The low lanes are known zero, or x3 tells.
        let known = al.is_none() && bl.is_none() && op != Arith::Div;
        match (al, bl) {
            (None, None) => {}
            (Some(l), None) | (None, Some(l)) => {
                self.emit_all(&[fmov_from_v(X, X3, l), lsl_imm(X, X3, X3, 1)]);
            }
            (Some(_), Some(_)) => self.emit_all(&[
                fmov_from_v(X, X3, V2),
                fmov_from_v(X, X17, V3),
                orr(X, X3, X3, X17, Lsl, 0),
                lsl_imm(X, X3, X3, 1),
            ]),
        }
        // H in `high`, t in d5; d4 holds the first rounded result, the
        // sum's for a zero.
        let high = match op {
            Arith::Mul => {
                self.emit_all(&[
                    dfloat(Float::Fmul, V4, a.high, b.high),
                    fnmsub(V5, a.high, b.high, V4),
                ]);
                match (al, bl) {
                    (None, None) => {}
                    (Some(al), None) => self.emit(fmadd(V5, al, b.high, V5)),
                    (None, Some(bl)) => self.emit(fmadd(V5, a.high, bl, V5)),
                    (Some(al), Some(bl)) => self.emit_all(&[
                        dfloat(Float::Fmul, V6, a.high, bl),
                        fmadd(V6, al, b.high, V6),
                        dfloat(Float::Fadd, V5, V5, V6),
                    ]),
                }
                V4
            }
            Arith::Add | Arith::Sub => {
                let (bh, bl) = if op == Arith::Sub {
                    self.emit(unary(Unary::Fneg, V6, b.high));
                    if let Some(bl) = bl {
                        self.emit(unary(Unary::Fneg, bl, bl));
                    }
                    (V6, bl)
                } else {
                    (b.high, bl)
                };
                self.emit(dfloat(Float::Fadd, V4, a.high, bh));
                self.two_sum(a.high, bh, V4, V5);
                let m = match (al, bl) {
                    (None, None) => None,
                    (Some(l), None) | (None, Some(l)) => Some(l),
                    (Some(al), Some(bl)) => {
                        self.emit(dfloat(Float::Fadd, al, al, bl));
                        Some(al)
                    }
                };
                match m {
                    None => V4,
                    Some(m) => {
                        // TwoSum(s, e + m) into d6 and d5.
                        self.emit_all(&[
                            dfloat(Float::Fadd, V5, V5, m),
                            dfloat(Float::Fadd, V6, V4, V5),
                        ]);
                        self.two_sum(V4, V5, V6, V3);
                        self.emit(vmov(false, V5, V3));
                        V6
                    }
                }
            }
            Arith::Div => {
                self.emit_all(&[
                    dfloat(Float::Fdiv, V4, a.high, b.high),
                    fmsub(V5, V4, b.high, a.high),
                    fmov_from_v(X, X17, V5),
                ]);
                if al.is_none() && bl.is_none() {
                    self.emit(lsl_imm(X, X3, X17, 1));
                } else {
                    self.emit(orr(X, X3, X3, X17, Lsl, 1));
                }
                if let Some(al) = al {
                    self.emit(dfloat(Float::Fadd, V5, V5, al));
                }
                if let Some(bl) = bl {
                    self.emit(fmsub(V5, V4, bl, V5));
                }
                self.emit(dfloat(Float::Fdiv, V5, V5, b.high));
                V4
            }
        };
        let r = v(to);
        let (zero, aligned, join) = (self.asm.label(), self.asm.label(), self.asm.label());
        self.pair_range(high, zero, (WIDE_LOW, WIDE_SPAN), generic);
        // Below a power of two the grid is finer: the integer path.
        self.emit(tst_imm(X, X16, FRACTION));
        self.asm.b_cond(Cond::Ne, aligned);
        self.emit_all(&[fmov_from_v(X, X17, V5), lsl_imm(X, X0, X17, 1)]);
        self.asm.cbz(X, false, X0, aligned);
        self.emit_all(&[eor(X, X17, X17, X16, Lsl, 0), cmp_imm(X, X17, 0, false)]);
        self.asm.b_cond(Cond::Mi, generic);
        self.asm.bind(aligned);
        // M = 1.5 * 2^(e - 11) rounds t to the multiples of 2^(e - 63);
        // d2 the rounded t, d0 the rest.
        self.emit_all(&[
            sub_imm(X, X17, X7, 11, false),
            lsl_imm(X, X17, X17, 52),
            orr_imm(X, X17, X17, 1 << 51),
            fmov_to_v(X, V0, X17),
            dfloat(Float::Fadd, V2, V5, V0),
            dfloat(Float::Fsub, V2, V2, V0),
            dfloat(Float::Fsub, V0, V5, V2),
            dfloat(Float::Fadd, r, high, V2),
            dfloat(Float::Fsub, V3, r, high),
            dfloat(Float::Fsub, V3, V2, V3),
            vins(3, r, 1, V3, 0),
            fmov_from_v(X, X16, V0),
        ]);
        let (certain, exact) = (self.asm.label(), self.asm.label());
        if !known {
            self.asm.cbz(X, false, X3, certain);
            // An approximate rest near half a unit could round either way.
            self.emit_all(&[
                and_imm(X, X17, X16, !(1 << 63)),
                sub_imm(X, X0, X7, 64, false),
                lsl_imm(X, X0, X0, 52),
                movz(X, X1, 1, 32),
                sub(X, X0, X0, X1, Lsl, 0),
                cmp(X, X17, X0, Lsl, 0),
            ]);
            self.asm.b_cond(Cond::Hs, generic);
            if c1_dead {
                self.asm.tbz(true, X13, 5, join);
            }
            // Nor can a rest near zero tell whether the result is exact.
            self.emit_all(&[
                sub_imm(X, X0, X7, 84, false),
                lsl_imm(X, X0, X0, 52),
                cmp(X, X17, X0, Lsl, 0),
            ]);
            self.asm.b_cond(Cond::Lo, generic);
            self.inexact(high, c1_dead);
            self.asm.b(join);
        }
        self.asm.bind(certain);
        if c1_dead {
            self.asm.tbz(true, X13, 5, join);
        }
        self.emit(lsl_imm(X, X17, X16, 1));
        self.asm.cbz(X, false, X17, exact);
        self.inexact(high, c1_dead);
        self.asm.b(join);
        self.asm.bind(exact);
        if !c1_dead {
            self.clear_c1();
        }
        self.asm.b(join);
        self.asm.bind(zero);
        self.exact_zero(op, a, b, generic);
        self.emit(vmov(false, r, V4));
        if !c1_dead {
            self.clear_c1();
        }
        self.asm.bind(join);
    }

    /// The integer path of an arithmetic instruction, which goes on at
    /// its join or bails.
    pub(super) fn pair_generic(&mut self, g: Generic, bail: Label) {
        self.asm.bind(g.label);
        match g.source {
            Some(n) => self.pair_to_extended(n, X2, X3),
            None => self.extended(V1, X2, X3),
        }
        self.pair_to_extended(g.dest, X0, X1);
        self.wide_ops(g.op, g.reverse, bail);
        self.pair_from_extended(X0, X1, g.to, g.narrow, bail);
        self.emit(ldrh(X8, X5, runtime::X87_CONTROL));
        self.asm.b(g.join);
    }

    /// The extended encoding of host register `n` in `sig` and `sexp`;
    /// uses x16, x17, d0 and d1.
    pub(super) fn pair_to_extended(&mut self, n: u8, sig: Reg, sexp: Reg) {
        let (zero, case_a, sign, done) = (
            self.asm.label(),
            self.asm.label(),
            self.asm.label(),
            self.asm.label(),
        );
        self.emit_all(&[fmov_from_v(X, X16, v(n)), lsl_imm(X, X17, X16, 1)]);
        self.asm.cbz(X, false, X17, zero);
        // l * 2^(64 - e) in `sig` is an integer, negated for a negative h.
        self.emit_all(&[
            ubfx(X, X17, X16, 52, 11),
            movz(W, sexp, 2110, 0),
            sub(W, sexp, sexp, X17, Lsl, 0),
            lsl_imm(X, sexp, sexp, 52),
            fmov_to_v(X, V0, sexp),
            dup_high(V1, v(n)),
            dfloat(Float::Fmul, V1, V1, V0),
            fcvt_int(X, true, sig, V1),
        ]);
        let positive = self.asm.label();
        self.asm.tbz(false, X16, 63, positive);
        self.emit(sub(X, sig, Reg::ZR, sig, Lsl, 0));
        self.asm.bind(positive);
        // The extended exponent of h; h's significand at the top.
        self.emit_all(&[
            add_imm(W, sexp, X17, 15360 >> 12, true),
            add_imm(W, sexp, sexp, 15360 & 0xfff, false),
            lsl_imm(X, X17, X16, 11),
            orr_imm(X, X17, X17, 1 << 63),
        ]);
        // Below a power of two the value has the next lower exponent, and
        // the rest alone is its significand.
        self.asm.tbz(false, sig, 63, case_a);
        self.emit(tst_imm(X, X16, FRACTION));
        self.asm.b_cond(Cond::Ne, case_a);
        self.emit(sub_imm(W, sexp, sexp, 1, false));
        self.asm.b(sign);
        self.asm.bind(case_a);
        self.emit(add(X, sig, X17, sig, Shift::Asr, 1));
        self.asm.bind(sign);
        self.emit_all(&[lsr_imm(X, X17, X16, 63), orr(W, sexp, sexp, X17, Lsl, 15)]);
        self.asm.b(done);
        self.asm.bind(zero);
        self.emit_all(&[
            movz(X, sig, 0, 0),
            lsr_imm(X, sexp, X16, 63),
            lsl_imm(W, sexp, sexp, 15),
        ]);
        self.asm.bind(done);
    }

    /// Host register `n` = the extended value in `sig` and `sexp`,
    /// branching to `fail` unless a run may hold it, or when `narrow` and
    /// its low lane would not be zero; uses x16, x17, d0 and d1.
    pub(super) fn pair_from_extended(
        &mut self,
        sig: Reg,
        sexp: Reg,
        n: u8,
        narrow: bool,
        fail: Label,
    ) {
        let (zero, done) = (self.asm.label(), self.asm.label());
        self.emit(and_imm(W, X17, sexp, 0x7fff));
        self.asm.cbz(X, false, sig, zero);
        // Callers fail to cold code, beyond tbz reach.
        self.emit(tst_imm(X, sig, 1 << 63));
        self.asm.b_cond(Cond::Eq, fail);
        // The scale 2^(e - 63) with the value's sign, as a double.
        let below = 0x3fff - 1023 + LOW;
        self.emit_all(&[
            sub_imm(W, X17, X17, below >> 12, true),
            sub_imm(W, X17, X17, below & 0xfff, false),
            cmp_imm(W, X17, SPAN, false),
        ]);
        self.asm.b_cond(Cond::Hi, fail);
        self.emit_all(&[
            add_imm(W, X17, X17, LOW - 63, false),
            lsl_imm(X, X17, X17, 52),
            lsr_imm(W, X16, sexp, 15),
            orr(X, X17, X17, X16, Lsl, 63),
            fmov_to_v(X, V0, X17),
            // The significand rounded to 53 bits, nearest even, and the
            // signed rest.
            ubfx(X, X16, sig, 11, 1),
            add(X, X16, sig, X16, Lsl, 0),
            add_imm(X, X16, X16, 0x3ff, false),
            and_imm(X, X16, X16, !0x7ff),
            sub(X, X16, sig, X16, Lsl, 0),
        ]);
        if narrow {
            self.asm.cbz(X, true, X16, fail);
        }
        self.emit_all(&[
            ucvtf(v(n), sig),
            dfloat(Float::Fmul, v(n), v(n), V0),
            scvtf(X, V1, X16),
            dfloat(Float::Fmul, V1, V1, V0),
            vins(3, v(n), 1, V1, 0),
        ]);
        self.asm.b(done);
        self.asm.bind(zero);
        self.asm.cbz(W, true, X17, fail);
        self.emit_all(&[
            lsr_imm(W, X16, sexp, 15),
            lsl_imm(X, X16, X16, 63),
            fmov_to_v(X, v(n), X16),
        ]);
        self.asm.bind(done);
    }
}
