//! Forward interval, finiteness, and dyadic-scale analysis for x87 operations,
//! plus two-step binary32 frame store-reload elimination.
//!
//! Soundness invariants:
//! - `finite` and `binary32_exact` are tracked separately (`Op::X87Load { single: true }`
//!   admits `±Inf`, and subnormals when `denormal == false`).
//! - Proving `Op::Fp` (binary64) does not alter lazy `Recipe::C1` recipes.
//! - Folding `X87Load(X87Store(v)) -> v` and `X87StoreC1 -> 0` requires `v` to be
//!   proved finite, exact in binary32, and normal or signed zero (preserving `-0.0`).

use std::collections::{BTreeMap, BTreeSet};

use super::{
    Block, End, FlagKind, Func, IntOp, IntUnary, Location, Op, Recipe, Type, Value, apply_aliases,
    dominators,
};
use crate::translate::front::tags::ConditionCode;
use crate::translate::opt::ir::{
    DomainGuardKind, FmaOp, FpOp, FpUnary, MAX_DOMAIN_GUARDS, SINGLE_A, SINGLE_B, SINGLE_C,
    SINGLE_EXACT, SINGLE_RESULT,
};

const ZERO_Q: i16 = i16::MAX;
const F32_MAX_F64: f64 = f32::MAX as f64;
const F32_MIN_NORMAL_F64: f64 = f32::MIN_POSITIVE as f64;
/// `2^-125`, the lowest value strictly above the binary32 minimum normal binade.
const F32_SAFE_MIN_F64: f64 = 2.350_988_701_644_575e-38; // f32::from_bits(0x0100_0000) as f64
const WIDEN_LIMIT: u8 = 3;
const MAX_DOMAIN_TRIP_COUNT: u32 = 1 << 20;
const MAX_GUARDED_INPUTS: usize = 24;
const MAX_BOUND_DEGREE: u8 = 3;
const F64_WIN_MIN_EXP: i16 = -240;
const F64_WIN_MAX_EXP: i16 = 240;
const F32_STORE_MIN_EXP: i16 = -125;
const F32_STORE_MAX_EXP: i16 = 126;
const F32_EXACT_MIN_EXP: i16 = -48;
const F32_EXACT_MAX_EXP: i16 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IntRange {
    lo: i64,
    hi: i64,
}

impl IntRange {
    fn top(ty: Type) -> Self {
        if ty == Type::I32 {
            Self {
                lo: i64::from(i32::MIN),
                hi: i64::from(i32::MAX),
            }
        } else {
            Self {
                lo: i64::MIN,
                hi: i64::MAX,
            }
        }
    }

    fn singleton(v: i64) -> Self {
        Self { lo: v, hi: v }
    }

    fn join(self, other: Self) -> Self {
        Self {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }

    fn from_i128(lo: i128, hi: i128, ty: Type) -> Self {
        let top = Self::top(ty);
        if lo >= i128::from(top.lo) && hi <= i128::from(top.hi) {
            Self {
                lo: lo as i64,
                hi: hi as i64,
            }
        } else {
            top
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FpFact {
    /// Proved finite (neither NaN nor ±Inf).
    finite: bool,
    /// Proved non-NaN and exact in binary32 (`f64::from(x as f32) == x`).
    /// Does NOT imply `finite` or `f32_normal_or_zero`.
    binary32_exact: bool,
    /// If finite and nonzero, `|x| >= 2^-126` (`f32::MIN_POSITIVE`).
    f32_normal_or_zero: bool,
    /// If finite and nonzero, `|x| >= 2^-125` (`f32::from_bits(0x0100_0000)`).
    f32_safe_nonzero: bool,
    /// Sound finite interval `[lo, hi]` when `finite == true`.
    lo: f64,
    hi: f64,
    /// Sound lower bound on `|x|` for all nonzero values when `finite == true`
    /// (`0.0` if unknown, `f64::INFINITY` if identically zero).
    min_nonzero: f64,
    /// Whether `-0.0` is possible (soundly `true` when unknown).
    may_be_neg_zero: bool,
    /// Upper bound on significant mantissa bit-width `w` when finite (`0` for `±0.0`).
    sig_bits: Option<u8>,
    /// Exact dyadic scale `q`: every value is `k * 2^q` for integer `k`.
    /// `Some(ZERO_Q)` means identically `±0.0`. Valid only when `finite`.
    dyadic_q: Option<i16>,
}

impl FpFact {
    const TOP: Self = Self {
        finite: false,
        binary32_exact: false,
        f32_normal_or_zero: false,
        f32_safe_nonzero: false,
        lo: f64::NEG_INFINITY,
        hi: f64::INFINITY,
        min_nonzero: 0.0,
        may_be_neg_zero: true,
        sig_bits: None,
        dyadic_q: None,
    };

    fn zero() -> Self {
        Self {
            finite: true,
            binary32_exact: true,
            f32_normal_or_zero: true,
            f32_safe_nonzero: true,
            lo: 0.0,
            hi: 0.0,
            min_nonzero: f64::INFINITY,
            may_be_neg_zero: false,
            sig_bits: Some(0),
            dyadic_q: Some(ZERO_Q),
        }
    }

    fn is_zero(self) -> bool {
        self.finite && self.lo == 0.0 && self.hi == 0.0
    }

    fn is_pos_zero(self) -> bool {
        self.is_zero() && !self.may_be_neg_zero
    }

    fn effective_sig_bits(self) -> Option<u8> {
        if !self.finite {
            return None;
        }
        match (self.sig_bits, self.binary32_exact) {
            (Some(w), true) => Some(w.min(24)),
            (Some(w), false) => Some(w),
            (None, true) => Some(24),
            (None, false) => None,
        }
    }

    fn max_abs(self) -> f64 {
        self.lo.abs().max(self.hi.abs())
    }

    /// Lower bound on `|x|` for all nonzero values in `self`, or `0.0` if unknown.
    fn min_nonzero_abs(self) -> f64 {
        if !self.finite {
            return 0.0;
        }
        let mut bound = self.min_nonzero;
        if self.lo > 0.0 {
            bound = bound.max(self.lo);
        } else if self.hi < 0.0 {
            bound = bound.max(-self.hi);
        }
        if let Some(q) = self.dyadic_q {
            if q == ZERO_Q {
                return f64::INFINITY;
            }
            if q >= -1022 {
                bound = bound.max(pow2_f64(i32::from(q)));
            }
        }
        if self.f32_safe_nonzero {
            bound = bound.max(F32_SAFE_MIN_F64);
        } else if self.f32_normal_or_zero {
            bound = bound.max(F32_MIN_NORMAL_F64);
        } else if self.binary32_exact {
            // Every nonzero finite f32 (even subnormal) has |x| >= 2^-149.
            bound = bound.max(f64::from(f32::from_bits(1)));
        }
        bound
    }

    fn singleton_pow2(self) -> Option<i16> {
        if !self.finite || self.lo != self.hi || self.lo == 0.0 {
            return None;
        }
        let bits = self.lo.abs().to_bits();
        let frac = bits & ((1u64 << 52) - 1);
        let exp = ((bits >> 52) & 0x7ff) as i32;
        if frac == 0 && exp > 0 && exp < 0x7ff {
            Some((exp - 1023) as i16)
        } else {
            None
        }
    }

    fn join(self, other: Self) -> Self {
        let finite = self.finite && other.finite;
        let (lo, hi, min_nonzero, sig_bits, dyadic_q) = if finite {
            let sig = match (self.effective_sig_bits(), other.effective_sig_bits()) {
                (Some(a), Some(b)) => Some(a.max(b)),
                _ => None,
            };
            (
                self.lo.min(other.lo),
                self.hi.max(other.hi),
                self.min_nonzero_abs().min(other.min_nonzero_abs()),
                sig,
                dyadic_min(self.dyadic_q, other.dyadic_q),
            )
        } else {
            (f64::NEG_INFINITY, f64::INFINITY, 0.0, None, None)
        };
        let may_be_neg_zero =
            (self.may_be_neg_zero || other.may_be_neg_zero) && !(finite && (lo > 0.0 || hi < 0.0));
        Self {
            finite,
            binary32_exact: self.binary32_exact && other.binary32_exact,
            f32_normal_or_zero: self.f32_normal_or_zero && other.f32_normal_or_zero,
            f32_safe_nonzero: self.f32_safe_nonzero && other.f32_safe_nonzero,
            lo,
            hi,
            min_nonzero,
            may_be_neg_zero,
            sig_bits,
            dyadic_q,
        }
    }

    /// Intersects `self` with an additional proved invariant `other`.
    fn meet(self, other: Self) -> Self {
        let finite = self.finite || other.finite;
        let (lo, hi, min_nonzero, sig_bits, dyadic_q) = if self.finite && other.finite {
            let lo = self.lo.max(other.lo);
            let hi = self.hi.min(other.hi);
            let (lo, hi) = if lo <= hi {
                (lo, hi)
            } else {
                (other.lo, other.hi)
            };
            let sig = match (self.effective_sig_bits(), other.effective_sig_bits()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            (
                lo,
                hi,
                self.min_nonzero_abs().max(other.min_nonzero_abs()),
                sig,
                self.dyadic_q.or(other.dyadic_q),
            )
        } else if self.finite {
            (
                self.lo,
                self.hi,
                self.min_nonzero_abs(),
                self.effective_sig_bits(),
                self.dyadic_q,
            )
        } else if other.finite {
            (
                other.lo,
                other.hi,
                other.min_nonzero_abs(),
                other.effective_sig_bits(),
                other.dyadic_q,
            )
        } else {
            (f64::NEG_INFINITY, f64::INFINITY, 0.0, None, None)
        };
        let may_be_neg_zero =
            self.may_be_neg_zero && other.may_be_neg_zero && !(finite && (lo > 0.0 || hi < 0.0));
        Self {
            finite,
            binary32_exact: self.binary32_exact || other.binary32_exact,
            f32_normal_or_zero: self.f32_normal_or_zero
                || other.f32_normal_or_zero
                || min_nonzero >= F32_MIN_NORMAL_F64,
            f32_safe_nonzero: self.f32_safe_nonzero
                || other.f32_safe_nonzero
                || min_nonzero >= F32_SAFE_MIN_F64,
            lo,
            hi,
            min_nonzero,
            may_be_neg_zero,
            sig_bits,
            dyadic_q,
        }
    }

    /// Widen a loop-head fact when its numeric bounds keep growing while
    /// retaining structural invariants (`binary32_exact`, etc.) that hold on
    /// every arrival.
    fn widen(self, next: Self) -> Self {
        let joined = self.join(next);
        if joined == self {
            return self;
        }
        if joined.finite && (joined.lo < self.lo || joined.hi > self.hi) {
            if joined.binary32_exact {
                // Any finite binary32 value is bounded by [-f32::MAX, f32::MAX].
                Self {
                    finite: true,
                    binary32_exact: true,
                    f32_normal_or_zero: joined.f32_normal_or_zero,
                    f32_safe_nonzero: joined.f32_safe_nonzero,
                    lo: -F32_MAX_F64,
                    hi: F32_MAX_F64,
                    min_nonzero: joined.min_nonzero,
                    may_be_neg_zero: joined.may_be_neg_zero,
                    sig_bits: Some(24),
                    dyadic_q: None,
                }
            } else {
                Self {
                    finite: false,
                    binary32_exact: false,
                    f32_normal_or_zero: joined.f32_normal_or_zero,
                    f32_safe_nonzero: joined.f32_safe_nonzero,
                    lo: f64::NEG_INFINITY,
                    hi: f64::INFINITY,
                    min_nonzero: 0.0,
                    may_be_neg_zero: joined.may_be_neg_zero,
                    sig_bits: None,
                    dyadic_q: None,
                }
            }
        } else {
            joined
        }
    }
}

fn pow2_f64(exp: i32) -> f64 {
    debug_assert!((-1022..=1023).contains(&exp));
    f64::from_bits(((exp + 1023) as u64) << 52)
}

/// Exact significant mantissa bit-width of a finite `f64` bit pattern (`0` for `±0.0`).
fn exact_sig_bits(bits: u64) -> u8 {
    let mag = bits & !(1u64 << 63);
    if mag == 0 {
        return 0;
    }
    let exp = (mag >> 52) & 0x7ff;
    let frac = mag & ((1u64 << 52) - 1);
    if exp == 0 {
        (64 - frac.leading_zeros() - frac.trailing_zeros()) as u8
    } else if frac == 0 {
        1
    } else {
        (53 - frac.trailing_zeros()) as u8
    }
}

fn int_sig_bits(lo: i64, hi: i64) -> Option<u8> {
    let max_mag = lo.unsigned_abs().max(hi.unsigned_abs());
    if max_mag == 0 {
        Some(0)
    } else {
        let bits = 64 - max_mag.leading_zeros();
        (bits <= 53).then_some(bits as u8)
    }
}

fn is_literal_neg_zero(defs: &[Option<Op>], v: Value) -> bool {
    matches!(
        defs.get(v.0 as usize),
        Some(Some(Op::Constant(Type::F64, bits))) if *bits as u64 == (-0.0f64).to_bits()
    )
}

fn result_may_be_neg_zero(op: FpOp, a: FpFact, b: FpFact, lo: f64, hi: f64) -> bool {
    if lo > 0.0 || hi < 0.0 {
        return false;
    }
    match op {
        FpOp::Add => a.may_be_neg_zero && b.may_be_neg_zero,
        FpOp::Sub => a.may_be_neg_zero && b.lo <= 0.0 && b.hi >= 0.0,
        FpOp::Mul | FpOp::Div => {
            !((a.lo >= 0.0 && !a.may_be_neg_zero && b.lo >= 0.0 && !b.may_be_neg_zero)
                || (a.hi < 0.0 && b.hi < 0.0))
        }
    }
}

/// Exact dyadic scale `q` of a finite `f64` bit pattern (`x = k * 2^q` with odd `k`).
fn exact_dyadic_q(bits: u64) -> i16 {
    let mag = bits & !(1u64 << 63);
    if mag == 0 {
        return ZERO_Q;
    }
    let exp = ((mag >> 52) & 0x7ff) as i16;
    let frac = mag & ((1u64 << 52) - 1);
    if exp == 0 {
        // Subnormal binary64: value is frac * 2^-1074.
        -1074 + frac.trailing_zeros() as i16
    } else {
        let tz = if frac == 0 {
            52
        } else {
            frac.trailing_zeros() as i16
        };
        (exp - 1023) - (52 - tz)
    }
}

fn dyadic_min(a: Option<i16>, b: Option<i16>) -> Option<i16> {
    match (a, b) {
        (Some(qa), Some(qb)) => Some(qa.min(qb)),
        _ => None,
    }
}

fn dyadic_mul(a: Option<i16>, b: Option<i16>) -> Option<i16> {
    match (a, b) {
        (Some(ZERO_Q), _) | (_, Some(ZERO_Q)) => Some(ZERO_Q),
        (Some(qa), Some(qb)) => {
            let sum = i32::from(qa) + i32::from(qb);
            if sum > i32::from(i16::MIN) && sum < i32::from(ZERO_Q) {
                Some(sum as i16)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether every `x = k * 2^q` with `|x| <= max_abs` has `|k| <= 2^bits`.
fn fits_mantissa_bits(max_abs: f64, q: i16, bits: u32) -> bool {
    if !max_abs.is_finite() {
        return false;
    }
    if q == ZERO_Q || max_abs == 0.0 {
        return true;
    }
    let limit_exp = i32::from(q) + bits as i32;
    if limit_exp >= 1024 {
        true
    } else if limit_exp < -1022 {
        false
    } else {
        max_abs <= pow2_f64(limit_exp)
    }
}

fn round_mode(x: f64, rounding: u8) -> f64 {
    match rounding {
        0 => x.round_ties_even(),
        1 => x.floor(),
        2 => x.ceil(),
        _ => x.trunc(),
    }
}

fn i64_to_f64_down(n: i64) -> f64 {
    let f = n as f64;
    if (f as i128) > i128::from(n) {
        f.next_down()
    } else {
        f
    }
}

fn i64_to_f64_up(n: i64) -> f64 {
    let f = n as f64;
    if (f as i128) < i128::from(n) {
        f.next_up()
    } else {
        f
    }
}

fn f64_to_f32_down(x: f64) -> f64 {
    let s = x as f32;
    let w = f64::from(s);
    if w > x { f64::from(s.next_down()) } else { w }
}

fn f64_to_f32_up(x: f64) -> f64 {
    let s = x as f32;
    let w = f64::from(s);
    if w < x { f64::from(s.next_up()) } else { w }
}

fn peel_i32_bits(defs: &[Option<Op>], mut v: Value) -> Value {
    while let Some(Op::Bits {
        to: Type::I32,
        value,
    }) = defs[v.0 as usize]
    {
        v = value;
    }
    v
}

/// Evaluates `Op::Fp` abstractly and returns `(proved, fact)`.
fn eval_fp(op: FpOp, a: FpFact, b: FpFact, same_operands: bool) -> (bool, FpFact) {
    if !a.finite || !b.finite {
        return (false, FpFact::TOP);
    }
    if same_operands && op == FpOp::Sub {
        // Any finite x satisfies x - x == +0.0 with no overflow or subnormal.
        return (true, FpFact::zero());
    }
    if op == FpOp::Add && b.is_zero() {
        let proved = a.binary32_exact
            || a.f32_normal_or_zero
            || a.dyadic_q.is_some_and(|q| q >= -1022)
            || a.min_nonzero_abs() >= f64::MIN_POSITIVE;
        return (proved, a);
    }
    if op == FpOp::Add && a.is_zero() {
        let proved = b.binary32_exact
            || b.f32_normal_or_zero
            || b.dyadic_q.is_some_and(|q| q >= -1022)
            || b.min_nonzero_abs() >= f64::MIN_POSITIVE;
        return (proved, b);
    }
    let both_f32 = a.binary32_exact && b.binary32_exact;
    let (mut lo, mut hi, q_cand) = match op {
        FpOp::Add => (a.lo + b.lo, a.hi + b.hi, dyadic_min(a.dyadic_q, b.dyadic_q)),
        FpOp::Sub => (a.lo - b.hi, a.hi - b.lo, dyadic_min(a.dyadic_q, b.dyadic_q)),
        FpOp::Mul => {
            let corners = [a.lo * b.lo, a.lo * b.hi, a.hi * b.lo, a.hi * b.hi];
            let lo = corners.into_iter().fold(f64::INFINITY, f64::min);
            let hi = corners.into_iter().fold(f64::NEG_INFINITY, f64::max);
            (lo, hi, dyadic_mul(a.dyadic_q, b.dyadic_q))
        }
        FpOp::Div => {
            if !(b.lo > 0.0 || b.hi < 0.0) {
                return (false, FpFact::TOP);
            }
            let corners = [a.lo / b.lo, a.lo / b.hi, a.hi / b.lo, a.hi / b.hi];
            let lo = corners.into_iter().fold(f64::INFINITY, f64::min);
            let hi = corners.into_iter().fold(f64::NEG_INFINITY, f64::max);
            let q = match (a.dyadic_q, b.singleton_pow2()) {
                (Some(ZERO_Q), Some(_)) => Some(ZERO_Q),
                (Some(qa), Some(kb)) => {
                    let diff = i32::from(qa) - i32::from(kb);
                    if diff > i32::from(i16::MIN) && diff < i32::from(ZERO_Q) {
                        Some(diff as i16)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            (lo, hi, q)
        }
    };

    if !lo.is_finite() || !hi.is_finite() {
        return (false, FpFact::TOP);
    }

    let mul_sig = if op == FpOp::Mul {
        a.effective_sig_bits()
            .zip(b.effective_sig_bits())
            .and_then(|(wa, wb)| wa.checked_add(wb))
    } else {
        None
    };
    // `lo` and `hi` were computed with f64 round-to-nearest, so an inexact
    // mathematical result such as `3 * 3002399751580331 = 2^53 + 1` can round
    // down onto the exact `2^(q + 53)` boundary. Check the outward-rounded
    // magnitude (`next_up()`) so any value that could have rounded down from
    // `>= 2^(q + 53) + 2^q` is never claimed exact.
    let exact_f64 = q_cand
        .is_some_and(|q| q >= -1022 && fits_mantissa_bits(lo.abs().max(hi.abs()).next_up(), q, 53))
        || (op == FpOp::Mul
            && (both_f32
                || (mul_sig.is_some_and(|w| w <= 53)
                    && (a.is_zero()
                        || b.is_zero()
                        || a.min_nonzero_abs() * b.min_nonzero_abs() > 2.0 * f64::MIN_POSITIVE))));

    if !exact_f64 {
        lo = lo.next_down();
        hi = hi.next_up();
        if !lo.is_finite() || !hi.is_finite() {
            return (false, FpFact::TOP);
        }
    }

    let max_abs = lo.abs().max(hi.abs());
    let ma = a.min_nonzero_abs();
    let mb = b.min_nonzero_abs();
    let m_in = ma.min(mb);
    let f64_add_safe_min = pow2_f64(-968);
    let proved = match op {
        FpOp::Add | FpOp::Sub => {
            both_f32
                || q_cand.is_some_and(|q| q >= -1022)
                || m_in >= f64_add_safe_min
                || lo >= f64::MIN_POSITIVE
                || hi <= -f64::MIN_POSITIVE
                || (lo == 0.0 && hi == 0.0)
        }
        FpOp::Mul => {
            both_f32
                || a.is_zero()
                || b.is_zero()
                || q_cand.is_some_and(|q| q >= -1021)
                || ma * mb > 2.0 * f64::MIN_POSITIVE
        }
        FpOp::Div => {
            a.is_zero()
                || (exact_f64 && q_cand.is_some_and(|q| q >= -1021))
                || ma / b.max_abs() > 2.0 * f64::MIN_POSITIVE
        }
    };

    let min_nonzero = match op {
        FpOp::Add | FpOp::Sub => {
            let no_cancel = (op == FpOp::Add
                && ((a.lo >= 0.0 && b.lo >= 0.0) || (a.hi <= 0.0 && b.hi <= 0.0)))
                || (op == FpOp::Sub
                    && ((a.lo >= 0.0 && b.hi <= 0.0) || (a.hi <= 0.0 && b.lo >= 0.0)));
            if no_cancel {
                m_in
            } else if m_in >= f64_add_safe_min {
                m_in * pow2_f64(-53)
            } else {
                0.0
            }
        }
        FpOp::Mul => {
            if a.is_zero() || b.is_zero() {
                f64::INFINITY
            } else if ma * mb >= pow2_f64(-1020) {
                0.5 * (ma * mb)
            } else {
                0.0
            }
        }
        FpOp::Div => {
            if a.is_zero() {
                f64::INFINITY
            } else if b.max_abs() > 0.0 && ma / b.max_abs() >= pow2_f64(-1020) {
                0.5 * (ma / b.max_abs())
            } else {
                0.0
            }
        }
    };

    let dyadic_q = if exact_f64 { q_cand } else { None };
    let binary32_exact = exact_f64
        && max_abs <= F32_MAX_F64
        && dyadic_q.is_some_and(|q| q >= -126 && fits_mantissa_bits(max_abs, q, 24));
    let f32_normal_or_zero = dyadic_q.is_some_and(|q| q >= -126)
        || min_nonzero >= F32_MIN_NORMAL_F64
        || lo >= F32_MIN_NORMAL_F64
        || hi <= -F32_MIN_NORMAL_F64
        || (lo == 0.0 && hi == 0.0);
    let f32_safe_nonzero = dyadic_q.is_some_and(|q| q >= -125)
        || min_nonzero >= F32_SAFE_MIN_F64
        || lo >= F32_SAFE_MIN_F64
        || hi <= -F32_SAFE_MIN_F64
        || (lo == 0.0 && hi == 0.0);

    let sig_bits = if exact_f64 {
        if lo == 0.0 && hi == 0.0 {
            Some(0)
        } else if let Some(w) = mul_sig.filter(|&w| w <= 53) {
            Some(w)
        } else if let Some(q) = dyadic_q {
            if max_abs == 0.0 {
                Some(0)
            } else if fits_mantissa_bits(max_abs, q, 24) {
                Some(24)
            } else {
                Some(53)
            }
        } else {
            None
        }
    } else {
        None
    };
    let fact = FpFact {
        finite: true,
        binary32_exact,
        f32_normal_or_zero,
        f32_safe_nonzero,
        lo,
        hi,
        min_nonzero,
        may_be_neg_zero: result_may_be_neg_zero(op, a, b, lo, hi),
        sig_bits,
        dyadic_q,
    };
    (proved, fact)
}

/// Evaluates `Op::X87Single` abstractly and returns `(proved_flags, fact)`.
fn eval_x87_single(op: FpOp, a: FpFact, b: FpFact, same_operands: bool) -> (u8, FpFact) {
    let mut flags = 0u8;
    if a.binary32_exact {
        flags |= SINGLE_A;
    }
    if b.binary32_exact {
        flags |= SINGLE_B;
    }
    if same_operands && op == FpOp::Sub && a.finite && a.binary32_exact {
        return (
            SINGLE_A | SINGLE_B | SINGLE_RESULT | SINGLE_EXACT,
            FpFact::zero(),
        );
    }
    if op == FpOp::Add && b.is_zero() && a.finite && a.binary32_exact && a.f32_safe_nonzero {
        return (
            SINGLE_A | SINGLE_B | SINGLE_RESULT | SINGLE_EXACT,
            FpFact {
                may_be_neg_zero: a.may_be_neg_zero && b.may_be_neg_zero,
                ..a
            },
        );
    }
    if op == FpOp::Add && a.is_zero() && b.finite && b.binary32_exact && b.f32_safe_nonzero {
        return (
            SINGLE_A | SINGLE_B | SINGLE_RESULT | SINGLE_EXACT,
            FpFact {
                may_be_neg_zero: a.may_be_neg_zero && b.may_be_neg_zero,
                ..b
            },
        );
    }

    // Even when unproved, a succeeding Op::X87Single always produces a finite,
    // exact, normal-or-zero binary32 value.
    let base_fact = FpFact {
        finite: true,
        binary32_exact: true,
        f32_normal_or_zero: true,
        f32_safe_nonzero: false,
        lo: -F32_MAX_F64,
        hi: F32_MAX_F64,
        min_nonzero: F32_MIN_NORMAL_F64,
        may_be_neg_zero: true,
        sig_bits: Some(24),
        dyadic_q: None,
    };

    if !(a.finite && b.finite && a.binary32_exact && b.binary32_exact) {
        return (flags, base_fact);
    }

    let (raw_lo, raw_hi, q_cand) = match op {
        FpOp::Add => (a.lo + b.lo, a.hi + b.hi, dyadic_min(a.dyadic_q, b.dyadic_q)),
        FpOp::Sub => (a.lo - b.hi, a.hi - b.lo, dyadic_min(a.dyadic_q, b.dyadic_q)),
        FpOp::Mul => {
            let corners = [a.lo * b.lo, a.lo * b.hi, a.hi * b.lo, a.hi * b.hi];
            let lo = corners.into_iter().fold(f64::INFINITY, f64::min);
            let hi = corners.into_iter().fold(f64::NEG_INFINITY, f64::max);
            (lo, hi, dyadic_mul(a.dyadic_q, b.dyadic_q))
        }
        FpOp::Div => {
            if !(b.lo > 0.0 || b.hi < 0.0) {
                return (flags, base_fact);
            }
            let corners = [a.lo / b.lo, a.lo / b.hi, a.hi / b.lo, a.hi / b.hi];
            let lo = corners.into_iter().fold(f64::INFINITY, f64::min);
            let hi = corners.into_iter().fold(f64::NEG_INFINITY, f64::max);
            let q = match (a.dyadic_q, b.singleton_pow2()) {
                (Some(ZERO_Q), Some(_)) => Some(ZERO_Q),
                (Some(qa), Some(kb)) => {
                    let diff = i32::from(qa) - i32::from(kb);
                    if diff > i32::from(i16::MIN) && diff < i32::from(ZERO_Q) {
                        Some(diff as i16)
                    } else {
                        None
                    }
                }
                _ => None,
            };
            (lo, hi, q)
        }
    };

    if !raw_lo.is_finite() || !raw_hi.is_finite() {
        return (flags, base_fact);
    }

    let raw_max = raw_lo.abs().max(raw_hi.abs());
    let mul_sig = if op == FpOp::Mul {
        a.effective_sig_bits()
            .zip(b.effective_sig_bits())
            .and_then(|(wa, wb)| wa.checked_add(wb))
    } else {
        None
    };
    let mul_safe_min =
        a.is_zero() || b.is_zero() || a.min_nonzero_abs() * b.min_nonzero_abs() >= F32_SAFE_MIN_F64;
    let exact_f32 = raw_max <= F32_MAX_F64
        && (q_cand.is_some_and(|q| q >= -125 && fits_mantissa_bits(raw_max, q, 24))
            || (op == FpOp::Mul && mul_sig.is_some_and(|w| w <= 24) && mul_safe_min));

    if exact_f32 {
        flags |= SINGLE_RESULT | SINGLE_EXACT;
        let mn = if op == FpOp::Mul && !a.is_zero() && !b.is_zero() {
            (0.5 * a.min_nonzero_abs() * b.min_nonzero_abs()).max(F32_SAFE_MIN_F64)
        } else if raw_lo == 0.0 && raw_hi == 0.0 {
            f64::INFINITY
        } else {
            F32_SAFE_MIN_F64
        };
        let fact = FpFact {
            finite: true,
            binary32_exact: true,
            f32_normal_or_zero: true,
            f32_safe_nonzero: true,
            lo: raw_lo,
            hi: raw_hi,
            min_nonzero: mn,
            may_be_neg_zero: result_may_be_neg_zero(op, a, b, raw_lo, raw_hi),
            sig_bits: mul_sig.filter(|&w| w <= 24).or(Some(24)),
            dyadic_q: q_cand,
        };
        return (flags, fact);
    }

    // Outward-round to f32 bounds (widening by 1 f64 ULP first if f64 arithmetic may have rounded).
    let lo = f64_to_f32_down(if op == FpOp::Mul {
        raw_lo
    } else {
        raw_lo.next_down()
    });
    let hi = f64_to_f32_up(if op == FpOp::Mul {
        raw_hi
    } else {
        raw_hi.next_up()
    });
    if !lo.is_finite() || !hi.is_finite() || lo.abs().max(hi.abs()) > F32_MAX_F64 {
        return (flags, base_fact);
    }

    let ma = a.min_nonzero_abs();
    let mb = b.min_nonzero_abs();
    let m_in = ma.min(mb);
    let f32_add_safe_min = pow2_f64(-101);
    let result_safe = match op {
        FpOp::Add | FpOp::Sub => {
            q_cand.is_some_and(|q| q >= -125)
                || m_in >= f32_add_safe_min
                || lo >= F32_SAFE_MIN_F64
                || hi <= -F32_SAFE_MIN_F64
                || (lo == 0.0 && hi == 0.0)
        }
        FpOp::Mul => a.is_zero() || b.is_zero() || ma * mb >= F32_SAFE_MIN_F64,
        FpOp::Div => a.is_zero() || ma / b.max_abs() >= F32_SAFE_MIN_F64,
    };

    if result_safe {
        flags |= SINGLE_RESULT;
    }

    let min_nonzero = match op {
        FpOp::Add | FpOp::Sub => {
            let no_cancel = (op == FpOp::Add
                && ((a.lo >= 0.0 && b.lo >= 0.0) || (a.hi <= 0.0 && b.hi <= 0.0)))
                || (op == FpOp::Sub
                    && ((a.lo >= 0.0 && b.hi <= 0.0) || (a.hi <= 0.0 && b.lo >= 0.0)));
            if no_cancel {
                m_in
            } else if m_in >= f32_add_safe_min {
                m_in * pow2_f64(-24)
            } else {
                F32_MIN_NORMAL_F64
            }
        }
        FpOp::Mul => {
            if a.is_zero() || b.is_zero() {
                f64::INFINITY
            } else if ma * mb >= F32_SAFE_MIN_F64 {
                0.5 * (ma * mb)
            } else {
                F32_MIN_NORMAL_F64
            }
        }
        FpOp::Div => {
            if a.is_zero() {
                f64::INFINITY
            } else if b.max_abs() > 0.0 && ma / b.max_abs() >= F32_SAFE_MIN_F64 {
                0.5 * (ma / b.max_abs())
            } else {
                F32_MIN_NORMAL_F64
            }
        }
    };
    let f32_safe_nonzero = result_safe
        || min_nonzero >= F32_SAFE_MIN_F64
        || lo >= F32_SAFE_MIN_F64
        || hi <= -F32_SAFE_MIN_F64
        || (lo == 0.0 && hi == 0.0);
    let fact = FpFact {
        finite: true,
        binary32_exact: true,
        f32_normal_or_zero: true,
        f32_safe_nonzero,
        lo,
        hi,
        min_nonzero,
        may_be_neg_zero: result_may_be_neg_zero(op, a, b, lo, hi),
        sig_bits: Some(24),
        dyadic_q: None,
    };
    (flags, fact)
}

fn eval_int_op(
    op: Op,
    ty: Type,
    int_facts: &[Option<IntRange>],
    fp_facts: &[Option<FpFact>],
) -> IntRange {
    let get_i = |v: Value, t: Type| int_facts[v.0 as usize].unwrap_or_else(|| IntRange::top(t));
    match op {
        Op::Constant(Type::I32, bits) => IntRange::singleton(i64::from(bits as u32 as i32)),
        Op::Constant(Type::I64, bits) => IntRange::singleton(bits as u64 as i64),
        Op::Load { width: 1, .. } => IntRange { lo: 0, hi: 255 },
        Op::Load { width: 2, .. } => IntRange { lo: 0, hi: 65535 },
        Op::IntUnary(IntUnary::ZeroExtend8, _) => IntRange { lo: 0, hi: 255 },
        Op::IntUnary(IntUnary::ZeroExtend16, _) => IntRange { lo: 0, hi: 65535 },
        Op::IntUnary(IntUnary::SignExtend8, _) => IntRange { lo: -128, hi: 127 },
        Op::IntUnary(IntUnary::SignExtend16, _) => IntRange {
            lo: -32768,
            hi: 32767,
        },
        Op::IntUnary(IntUnary::SignWiden, a) => get_i(a, Type::I32),
        Op::IntUnary(IntUnary::Widen, a) => {
            let r = get_i(a, Type::I32);
            if r.lo >= 0 {
                r
            } else {
                IntRange {
                    lo: 0,
                    hi: i64::from(u32::MAX),
                }
            }
        }
        Op::Int(IntOp::Add, a, b) => {
            let a = get_i(a, ty);
            let b = get_i(b, ty);
            IntRange::from_i128(
                i128::from(a.lo) + i128::from(b.lo),
                i128::from(a.hi) + i128::from(b.hi),
                ty,
            )
        }
        Op::Int(IntOp::Sub, a, b) => {
            let a = get_i(a, ty);
            let b = get_i(b, ty);
            IntRange::from_i128(
                i128::from(a.lo) - i128::from(b.hi),
                i128::from(a.hi) - i128::from(b.lo),
                ty,
            )
        }
        Op::Int(IntOp::Mul, a, b) => {
            let a = get_i(a, ty);
            let b = get_i(b, ty);
            let corners = [
                i128::from(a.lo) * i128::from(b.lo),
                i128::from(a.lo) * i128::from(b.hi),
                i128::from(a.hi) * i128::from(b.lo),
                i128::from(a.hi) * i128::from(b.hi),
            ];
            let lo = corners.into_iter().min().unwrap();
            let hi = corners.into_iter().max().unwrap();
            IntRange::from_i128(lo, hi, ty)
        }
        Op::Int(IntOp::And, a, b) => {
            let a = get_i(a, ty);
            let b = get_i(b, ty);
            let mut top = IntRange::top(ty);
            for c in [a, b] {
                if c.lo == c.hi && c.lo >= 0 {
                    top.lo = 0;
                    top.hi = top.hi.min(c.lo);
                }
            }
            top
        }
        Op::Int(IntOp::Shl, a, b) => {
            let a = get_i(a, ty);
            let b = get_i(b, ty);
            let max_shift = if ty == Type::I32 { 31 } else { 63 };
            if b.lo == b.hi && (0..=max_shift).contains(&b.lo) {
                let s = b.lo as u32;
                IntRange::from_i128(i128::from(a.lo) << s, i128::from(a.hi) << s, ty)
            } else {
                IntRange::top(ty)
            }
        }
        Op::Int(IntOp::Sar, a, b) => {
            let a = get_i(a, ty);
            let b = get_i(b, ty);
            let max_shift = if ty == Type::I32 { 31 } else { 63 };
            if b.lo == b.hi && (0..=max_shift).contains(&b.lo) {
                let s = b.lo as u32;
                IntRange {
                    lo: a.lo >> s,
                    hi: a.hi >> s,
                }
            } else {
                IntRange::top(ty)
            }
        }
        Op::Select { a, b, .. } => get_i(a, ty).join(get_i(b, ty)),
        Op::X87Store {
            value,
            width,
            rounding,
            integer: true,
            ..
        } => {
            let (min, max) = match width {
                2 => (i64::from(i16::MIN), i64::from(i16::MAX)),
                4 => (i64::from(i32::MIN), i64::from(i32::MAX)),
                _ => (i64::MIN, i64::MAX),
            };
            if let Some(f) = fp_facts[value.0 as usize]
                && f.finite
            {
                let r_lo = round_mode(f.lo, rounding);
                let r_hi = round_mode(f.hi, rounding);
                let limit = (1u64 << (width * 8 - 1)) as f64;
                if r_lo.is_finite() && r_hi.is_finite() && r_lo >= -limit && r_hi < limit {
                    let lo = (r_lo as i64).clamp(min, max);
                    let hi = (r_hi as i64).clamp(min, max);
                    if lo <= hi {
                        return IntRange { lo, hi };
                    }
                }
            }
            IntRange { lo: min, hi: max }
        }
        _ => IntRange::top(ty),
    }
}

#[derive(Default)]
pub(super) struct DomainSeeds {
    inputs: BTreeMap<Value, FpFact>,
    params: BTreeMap<Value, FpFact>,
    values: BTreeMap<Value, (f64, Option<f64>)>,
}

fn analyze_fixpoint(
    func: &Func,
    defs: &[Option<Op>],
    seeds: &DomainSeeds,
) -> (Vec<Option<IntRange>>, Vec<Option<FpFact>>) {
    let n = func.types.len();
    let mut int_facts: Vec<Option<IntRange>> = vec![None; n];
    let mut fp_facts: Vec<Option<FpFact>> = vec![None; n];
    let mut param_updates = vec![0u8; n];

    for &p in &func.blocks[0].params {
        match func.ty(p) {
            Type::I32 | Type::I64 => int_facts[p.0 as usize] = Some(IntRange::top(func.ty(p))),
            Type::F64 => {
                let base = seeds.inputs.get(&p).copied().unwrap_or(FpFact::TOP);
                fp_facts[p.0 as usize] = Some(base);
            }
            _ => {}
        }
    }

    let clamp_value = |v: Value, mut f: FpFact| -> FpFact {
        if let Some(&seed) = seeds.inputs.get(&v) {
            f = f.meet(seed);
        }
        if let Some(&(bound, min_nz)) = seeds.values.get(&v)
            && f.finite
        {
            f.lo = f.lo.max(-bound);
            f.hi = f.hi.min(bound);
            if f.lo > f.hi {
                f.lo = -bound;
                f.hi = bound;
            }
            if let Some(mn) = min_nz {
                f.min_nonzero = f.min_nonzero.max(mn);
                f.f32_normal_or_zero |= mn >= F32_MIN_NORMAL_F64;
                f.f32_safe_nonzero |= mn >= F32_SAFE_MIN_F64;
            }
        }
        f
    };

    // Forward fixed-point analysis.
    loop {
        let mut changed = false;
        for block in &func.blocks {
            for &(result, op) in &block.insts {
                let Some(r) = result else { continue };
                match func.ty(r) {
                    Type::I32 | Type::I64 => {
                        let next = eval_int_op(op, func.ty(r), &int_facts, &fp_facts);
                        if int_facts[r.0 as usize] != Some(next) {
                            int_facts[r.0 as usize] = Some(next);
                            changed = true;
                        }
                    }
                    Type::F64 => {
                        let next =
                            clamp_value(r, eval_fp_op(op, func, defs, &int_facts, &fp_facts));
                        if fp_facts[r.0 as usize] != Some(next) {
                            fp_facts[r.0 as usize] = Some(next);
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
            for target in block.end.targets() {
                let params = &func.blocks[target.block.0 as usize].params;
                for (&param, &arg) in params.iter().zip(&target.args) {
                    match func.ty(param) {
                        Type::I32 | Type::I64 => {
                            let Some(a) = int_facts[arg.0 as usize] else {
                                continue;
                            };
                            let next = match int_facts[param.0 as usize] {
                                None => a,
                                Some(cur) => {
                                    let joined = cur.join(a);
                                    if joined != cur {
                                        if param_updates[param.0 as usize] >= WIDEN_LIMIT {
                                            IntRange::top(func.ty(param))
                                        } else {
                                            param_updates[param.0 as usize] += 1;
                                            joined
                                        }
                                    } else {
                                        cur
                                    }
                                }
                            };
                            if int_facts[param.0 as usize] != Some(next) {
                                int_facts[param.0 as usize] = Some(next);
                                changed = true;
                            }
                        }
                        Type::F64 => {
                            let Some(a) = fp_facts[arg.0 as usize] else {
                                continue;
                            };
                            let mut next = match fp_facts[param.0 as usize] {
                                None => a,
                                Some(cur) => {
                                    let joined = cur.join(a);
                                    if joined != cur {
                                        if param_updates[param.0 as usize] >= WIDEN_LIMIT {
                                            cur.widen(a)
                                        } else {
                                            param_updates[param.0 as usize] += 1;
                                            joined
                                        }
                                    } else {
                                        cur
                                    }
                                }
                            };
                            if let Some(&inv) = seeds.params.get(&param) {
                                next = next.meet(inv);
                            }
                            if fp_facts[param.0 as usize] != Some(next) {
                                fp_facts[param.0 as usize] = Some(next);
                                changed = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    (int_facts, fp_facts)
}

fn apply_proved(
    func: &mut Func,
    defs: &[Option<Op>],
    fp_facts: &[Option<FpFact>],
    loop_blocks: Option<&[bool]>,
    commit: bool,
) -> (bool, u32) {
    let n = func.types.len();
    let get_fp = |v: Value| {
        fp_facts
            .get(v.0 as usize)
            .copied()
            .flatten()
            .unwrap_or(FpFact::TOP)
    };
    let mut changed = false;
    let mut saved_per_iter = 0u32;
    let mut alias: Vec<Value> = (0..n).map(|i| Value(i as u32)).collect();
    let mut aliased = false;
    let mut folded_c1: Vec<(FpOp, Value, Value, bool)> = Vec::new();

    for (b, block) in func.blocks.iter_mut().enumerate() {
        let in_loop = loop_blocks.is_none_or(|lb| lb.get(b).copied().unwrap_or(false));
        for (result, op) in &mut block.insts {
            match *op {
                Op::Fp(fp_op, a, b_val, cur_proved) => {
                    let fa = get_fp(a);
                    let fb = get_fp(b_val);
                    // Phase 2.2: under RC == Nearest, finite m - m -> +0.0.
                    if fp_op == FpOp::Sub && a == b_val && fa.finite {
                        if in_loop {
                            saved_per_iter =
                                saved_per_iter.saturating_add(if cur_proved { 1 } else { 6 });
                        }
                        if commit {
                            *op = Op::Constant(Type::F64, 0);
                            folded_c1.push((fp_op, a, b_val, false));
                        }
                        changed = true;
                        continue;
                    }
                    // Phase 2.2: exact identity zero folds (binary64 normal/zero).
                    let f64_normal_or_zero = |f: FpFact| {
                        f.finite && (f.is_zero() || f.min_nonzero_abs() >= f64::MIN_POSITIVE)
                    };
                    let fold_to = match fp_op {
                        FpOp::Add => {
                            if (fb.is_pos_zero() && f64_normal_or_zero(fa) && !fa.may_be_neg_zero)
                                || (is_literal_neg_zero(defs, b_val) && f64_normal_or_zero(fa))
                            {
                                Some(a)
                            } else if (fa.is_pos_zero()
                                && f64_normal_or_zero(fb)
                                && !fb.may_be_neg_zero)
                                || (is_literal_neg_zero(defs, a) && f64_normal_or_zero(fb))
                            {
                                Some(b_val)
                            } else {
                                None
                            }
                        }
                        FpOp::Sub => {
                            if (fb.is_pos_zero() && f64_normal_or_zero(fa))
                                || (is_literal_neg_zero(defs, b_val)
                                    && f64_normal_or_zero(fa)
                                    && !fa.may_be_neg_zero)
                            {
                                Some(a)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let (Some(r), Some(target)) = (*result, fold_to) {
                        if in_loop {
                            saved_per_iter =
                                saved_per_iter.saturating_add(if cur_proved { 1 } else { 6 });
                        }
                        if commit {
                            alias[r.0 as usize] = target;
                            aliased = true;
                            folded_c1.push((fp_op, a, b_val, false));
                        }
                        changed = true;
                        continue;
                    }
                    if !cur_proved {
                        let (proved, _) = eval_fp(fp_op, fa, fb, a == b_val);
                        if proved {
                            if in_loop {
                                saved_per_iter = saved_per_iter.saturating_add(5);
                            }
                            if commit {
                                *op = Op::Fp(fp_op, a, b_val, true);
                            }
                            changed = true;
                        }
                    }
                }
                Op::X87Single(fp_op, a, b_val, cur) => {
                    let fa = get_fp(a);
                    let fb = get_fp(b_val);
                    // Phase 2.2: under RC == Nearest, finite binary32-exact m - m -> +0.0.
                    if fp_op == FpOp::Sub && a == b_val && fa.finite && fa.binary32_exact {
                        if in_loop {
                            let all = SINGLE_A | SINGLE_B | SINGLE_RESULT;
                            saved_per_iter = saved_per_iter.saturating_add(if cur & all == all {
                                1
                            } else {
                                14
                            });
                        }
                        if commit {
                            *op = Op::Constant(Type::F64, 0);
                            folded_c1.push((fp_op, a, b_val, true));
                        }
                        changed = true;
                        continue;
                    }
                    // Phase 2.2: identity zero fold ONLY when operand is already exact in
                    // binary32 (preventing (1 + 2^-30) + 0 under PC=24 from skipping rounding).
                    let f32_exact_safe =
                        |f: FpFact| f.finite && f.binary32_exact && f.f32_safe_nonzero;
                    let fold_to = match fp_op {
                        FpOp::Add => {
                            if (fb.is_pos_zero() && f32_exact_safe(fa) && !fa.may_be_neg_zero)
                                || (is_literal_neg_zero(defs, b_val) && f32_exact_safe(fa))
                            {
                                Some(a)
                            } else if (fa.is_pos_zero()
                                && f32_exact_safe(fb)
                                && !fb.may_be_neg_zero)
                                || (is_literal_neg_zero(defs, a) && f32_exact_safe(fb))
                            {
                                Some(b_val)
                            } else {
                                None
                            }
                        }
                        FpOp::Sub => {
                            if (fb.is_pos_zero() && f32_exact_safe(fa))
                                || (is_literal_neg_zero(defs, b_val)
                                    && f32_exact_safe(fa)
                                    && !fa.may_be_neg_zero)
                            {
                                Some(a)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let (Some(r), Some(target)) = (*result, fold_to) {
                        if in_loop {
                            let all = SINGLE_A | SINGLE_B | SINGLE_RESULT;
                            saved_per_iter = saved_per_iter.saturating_add(if cur & all == all {
                                1
                            } else {
                                14
                            });
                        }
                        if commit {
                            alias[r.0 as usize] = target;
                            aliased = true;
                            folded_c1.push((fp_op, a, b_val, true));
                        }
                        changed = true;
                        continue;
                    }
                    let (proved, _) = eval_x87_single(fp_op, fa, fb, a == b_val);
                    let merged = cur | proved;
                    if merged != cur {
                        if in_loop {
                            let added = merged & !cur;
                            let mut w = 0u32;
                            if added & SINGLE_A != 0 {
                                w += 3;
                            }
                            if added & SINGLE_B != 0 {
                                w += 3;
                            }
                            if added & SINGLE_RESULT != 0 {
                                w += 8;
                            }
                            saved_per_iter = saved_per_iter.saturating_add(w);
                        }
                        if commit {
                            *op = Op::X87Single(fp_op, a, b_val, merged);
                        }
                        changed = true;
                    }
                }
                Op::X87Load {
                    bits,
                    denormal,
                    single,
                    proved,
                } => {
                    if let Some(const_bits) = const_x87_load(defs, bits, denormal, single) {
                        if commit {
                            *op = Op::Constant(Type::F64, u128::from(const_bits));
                        }
                        changed = true;
                        continue;
                    }
                    if single {
                        let src_bits = peel_i32_bits(defs, bits);
                        if let Some(Op::X87Store {
                            value,
                            width: 4,
                            integer: false,
                            ..
                        }) = defs.get(src_bits.0 as usize).copied().flatten()
                        {
                            let fv = get_fp(value);
                            if fv.finite && fv.binary32_exact && fv.f32_normal_or_zero {
                                // Step 1.2B: exact normal-or-zero binary32 round-trip
                                // preserves all bits including -0.0.
                                if in_loop {
                                    saved_per_iter = saved_per_iter.saturating_add(4);
                                }
                                if commit && let Some(r) = *result {
                                    alias[r.0 as usize] = value;
                                    aliased = true;
                                }
                                changed = true;
                            } else if !proved {
                                // Step 1.2A: X87Store already validated finite normal-or-zero f32.
                                if in_loop {
                                    saved_per_iter = saved_per_iter.saturating_add(3);
                                }
                                if commit {
                                    *op = Op::X87Load {
                                        bits,
                                        denormal,
                                        single: true,
                                        proved: true,
                                    };
                                }
                                changed = true;
                            }
                        }
                    }
                }
                Op::X87Store {
                    value,
                    width,
                    rounding,
                    integer,
                    proved: false,
                } => {
                    let fv = get_fp(value);
                    let ok = if integer {
                        let limit = (1u64 << (width * 8 - 1)) as f64;
                        if fv.finite {
                            let r_lo = round_mode(fv.lo, rounding);
                            let r_hi = round_mode(fv.hi, rounding);
                            r_lo.is_finite() && r_hi.is_finite() && r_lo >= -limit && r_hi < limit
                        } else {
                            false
                        }
                    } else {
                        fv.finite
                            && fv.lo >= -F32_MAX_F64
                            && fv.hi <= F32_MAX_F64
                            && ((fv.binary32_exact && fv.f32_normal_or_zero) || fv.f32_safe_nonzero)
                    };
                    if ok {
                        if in_loop {
                            saved_per_iter = saved_per_iter.saturating_add(6);
                        }
                        if commit {
                            *op = Op::X87Store {
                                value,
                                width,
                                rounding,
                                integer,
                                proved: true,
                            };
                        }
                        changed = true;
                    }
                }
                Op::X87StoreC1 { value, integer, .. } => {
                    let fv = get_fp(value);
                    let exact = if integer {
                        fv.finite && fv.dyadic_q.is_some_and(|q| q >= 0)
                    } else {
                        fv.finite && fv.binary32_exact && fv.f32_normal_or_zero
                    };
                    if exact {
                        if commit {
                            *op = Op::Constant(Type::I32, 0);
                        }
                        changed = true;
                    }
                }
                _ => {}
            }
        }
    }

    if commit && !folded_c1.is_empty() {
        for record in &mut func.records {
            if let Some(x87) = &mut record.x87
                && let Some(Location::Recipe(Recipe::C1 { op, a, b, single })) = x87.c1
                && folded_c1.contains(&(op, a, b, single))
            {
                x87.c1 = Some(Location::Constant(0));
            }
        }
    }

    if commit && aliased {
        for block in &mut func.blocks {
            block
                .insts
                .retain(|(r, _)| r.is_none_or(|r| alias[r.0 as usize] == r));
        }
        apply_aliases(func, &alias);
    }

    (changed, saved_per_iter)
}

pub(super) fn run(func: &mut Func) -> bool {
    let has_x87 = func.blocks.iter().flat_map(|b| &b.insts).any(|(_, op)| {
        matches!(
            op,
            Op::Fp(..)
                | Op::X87Single(..)
                | Op::X87Load { .. }
                | Op::X87Store { .. }
                | Op::X87StoreC1 { .. }
        )
    });
    if !has_x87 {
        return false;
    }

    let defs = super::definitions(func);
    let (_, fp_facts) = analyze_fixpoint(func, &defs, &DomainSeeds::default());
    apply_proved(func, &defs, &fp_facts, None, true).0
}

fn const_x87_load(defs: &[Option<Op>], bits: Value, denormal: bool, single: bool) -> Option<u64> {
    let raw = match defs[bits.0 as usize] {
        Some(Op::Constant(_, raw)) => raw,
        _ => return None,
    };
    let (x, tiny) = if single {
        let f = f32::from_bits(raw as u32);
        (f64::from(f), denormal && f.is_subnormal())
    } else {
        let f = f64::from_bits(raw as u64);
        (f, denormal && f.is_subnormal())
    };
    (!x.is_nan() && !tiny).then(|| x.to_bits())
}

fn fp_fact_of_const(bits: u64) -> FpFact {
    let x = f64::from_bits(bits);
    if x.is_finite() {
        FpFact {
            finite: true,
            binary32_exact: f64::from(x as f32).to_bits() == bits,
            f32_normal_or_zero: x == 0.0 || x.abs() >= F32_MIN_NORMAL_F64,
            f32_safe_nonzero: x == 0.0 || x.abs() >= F32_SAFE_MIN_F64,
            lo: x,
            hi: x,
            min_nonzero: if x == 0.0 { f64::INFINITY } else { x.abs() },
            may_be_neg_zero: bits == (-0.0f64).to_bits(),
            sig_bits: Some(exact_sig_bits(bits)),
            dyadic_q: Some(exact_dyadic_q(bits)),
        }
    } else if x.is_infinite() {
        FpFact {
            finite: false,
            binary32_exact: true,
            f32_normal_or_zero: false,
            f32_safe_nonzero: false,
            lo: f64::NEG_INFINITY,
            hi: f64::INFINITY,
            min_nonzero: 0.0,
            may_be_neg_zero: false,
            sig_bits: None,
            dyadic_q: None,
        }
    } else {
        FpFact::TOP
    }
}

fn eval_fp_op(
    op: Op,
    func: &Func,
    defs: &[Option<Op>],
    int_facts: &[Option<IntRange>],
    fp_facts: &[Option<FpFact>],
) -> FpFact {
    let get_fp = |v: Value| fp_facts[v.0 as usize].unwrap_or(FpFact::TOP);
    match op {
        Op::Constant(Type::F64, bits) => fp_fact_of_const(bits as u64),
        Op::FromInt(a) => {
            let ty = func.ty(a);
            let r = int_facts[a.0 as usize].unwrap_or_else(|| IntRange::top(ty));
            let lo = i64_to_f64_down(r.lo);
            let hi = i64_to_f64_up(r.hi);
            let max_abs = lo.abs().max(hi.abs());
            let q = if r.lo == 0 && r.hi == 0 { ZERO_Q } else { 0 };
            FpFact {
                finite: true,
                binary32_exact: fits_mantissa_bits(max_abs, 0, 24),
                f32_normal_or_zero: true,
                f32_safe_nonzero: true,
                lo,
                hi,
                min_nonzero: if q == ZERO_Q { f64::INFINITY } else { 1.0 },
                may_be_neg_zero: false,
                sig_bits: int_sig_bits(r.lo, r.hi),
                dyadic_q: Some(q),
            }
        }
        Op::FpUnary(unary, a) => {
            let a = get_fp(a);
            match unary {
                FpUnary::Neg => FpFact {
                    lo: if a.finite { -a.hi } else { f64::NEG_INFINITY },
                    hi: if a.finite { -a.lo } else { f64::INFINITY },
                    may_be_neg_zero: !a.finite || (a.lo <= 0.0 && a.hi >= 0.0),
                    ..a
                },
                FpUnary::Abs => {
                    let (lo, hi) = if !a.finite {
                        (f64::NEG_INFINITY, f64::INFINITY)
                    } else if a.lo <= 0.0 && a.hi >= 0.0 {
                        (0.0, a.lo.abs().max(a.hi.abs()))
                    } else {
                        (a.lo.abs().min(a.hi.abs()), a.lo.abs().max(a.hi.abs()))
                    };
                    FpFact {
                        lo,
                        hi,
                        may_be_neg_zero: false,
                        ..a
                    }
                }
            }
        }
        Op::Fp(fp_op, a, b, _) => eval_fp(fp_op, get_fp(a), get_fp(b), a == b).1,
        Op::X87Single(fp_op, a, b, _) => eval_x87_single(fp_op, get_fp(a), get_fp(b), a == b).1,
        Op::X87Load {
            bits,
            denormal,
            single,
            ..
        } => {
            if let Some(const_bits) = const_x87_load(defs, bits, denormal, single) {
                return fp_fact_of_const(const_bits);
            }
            if single {
                let src_bits = peel_i32_bits(defs, bits);
                if let Some(Op::X87Store {
                    value,
                    width: 4,
                    integer: false,
                    proved: store_proved,
                    ..
                }) = defs[src_bits.0 as usize]
                {
                    let fv = get_fp(value);
                    if fv.finite && fv.binary32_exact && fv.f32_normal_or_zero {
                        return fv;
                    }
                    let (lo, hi) = if fv.finite {
                        let c_lo = fv.lo.clamp(-F32_MAX_F64, F32_MAX_F64);
                        let c_hi = fv.hi.clamp(-F32_MAX_F64, F32_MAX_F64);
                        (f64_to_f32_down(c_lo), f64_to_f32_up(c_hi))
                    } else {
                        (-F32_MAX_F64, F32_MAX_F64)
                    };
                    let f32_safe_nonzero = !store_proved || fv.f32_safe_nonzero;
                    let mn = if fv.min_nonzero_abs() >= F32_SAFE_MIN_F64 {
                        f64_to_f32_down(fv.min_nonzero_abs().min(F32_MAX_F64))
                    } else if f32_safe_nonzero {
                        F32_SAFE_MIN_F64
                    } else {
                        F32_MIN_NORMAL_F64
                    };
                    return FpFact {
                        finite: true,
                        binary32_exact: true,
                        f32_normal_or_zero: true,
                        f32_safe_nonzero,
                        lo,
                        hi,
                        min_nonzero: mn,
                        may_be_neg_zero: fv.may_be_neg_zero && !(lo > 0.0 || hi < 0.0),
                        sig_bits: Some(24),
                        dyadic_q: None,
                    };
                }
            }
            // General X87Load admits ±Inf (so finite = false) and, when
            // denormal == false, admits subnormals (so f32_normal_or_zero = single && denormal).
            FpFact {
                finite: false,
                binary32_exact: single,
                f32_normal_or_zero: single && denormal,
                f32_safe_nonzero: false,
                lo: f64::NEG_INFINITY,
                hi: f64::INFINITY,
                min_nonzero: 0.0,
                may_be_neg_zero: true,
                sig_bits: None,
                dyadic_q: None,
            }
        }
        Op::Select { a, b, .. } => get_fp(a).join(get_fp(b)),
        _ => FpFact::TOP,
    }
}

#[derive(Clone, Copy)]
struct CountedLatch {
    i_0: Value,
    limit: Option<Value>,
    dir: i32,
    cc: ConditionCode,
}

#[derive(Clone, Copy)]
struct AffineInd {
    param: Value,
    x_0: Value,
    dx: Value,
    sum_val: Value,
    f32_step: bool,
    latch_idx: usize,
}

#[derive(Clone, Copy, Default)]
struct InputReq {
    needs_exact_f32: bool,
    needs_f32_store: bool,
}

impl InputReq {
    fn window(self) -> (i16, i16) {
        if self.needs_exact_f32 {
            (F32_EXACT_MIN_EXP, F32_EXACT_MAX_EXP)
        } else if self.needs_f32_store {
            (F32_STORE_MIN_EXP, F32_STORE_MAX_EXP)
        } else {
            (F64_WIN_MIN_EXP, F64_WIN_MAX_EXP)
        }
    }
}

fn invert_cc(cc: ConditionCode) -> Option<ConditionCode> {
    Some(match cc {
        ConditionCode::e => ConditionCode::ne,
        ConditionCode::ne => ConditionCode::e,
        ConditionCode::l => ConditionCode::ge,
        ConditionCode::ge => ConditionCode::l,
        ConditionCode::le => ConditionCode::g,
        ConditionCode::g => ConditionCode::le,
        ConditionCode::b => ConditionCode::ae,
        ConditionCode::ae => ConditionCode::b,
        ConditionCode::be => ConditionCode::a,
        ConditionCode::a => ConditionCode::be,
        _ => return None,
    })
}

fn swap_cc(cc: ConditionCode) -> Option<ConditionCode> {
    Some(match cc {
        ConditionCode::e => ConditionCode::e,
        ConditionCode::ne => ConditionCode::ne,
        ConditionCode::l => ConditionCode::g,
        ConditionCode::g => ConditionCode::l,
        ConditionCode::le => ConditionCode::ge,
        ConditionCode::ge => ConditionCode::le,
        ConditionCode::b => ConditionCode::a,
        ConditionCode::a => ConditionCode::b,
        ConditionCode::be => ConditionCode::ae,
        ConditionCode::ae => ConditionCode::be,
        _ => return None,
    })
}

fn safe_int_limit(width: u8) -> (f64, f64) {
    match width {
        2 => (32_760.0, 32_761.0),
        4 => (2_147_000_000.0, 2_147_010_000.0),
        _ => {
            let lim = (1u64 << 62) as f64;
            (lim, lim * 1.01)
        }
    }
}

fn fp_window_fact(min_exp: i16, max_exp: i16, exact_f32: bool) -> FpFact {
    let hi = pow2_f64(i32::from(max_exp) + 1);
    let min_nz = pow2_f64(i32::from(min_exp));
    FpFact {
        finite: true,
        binary32_exact: exact_f32,
        f32_normal_or_zero: min_nz >= F32_MIN_NORMAL_F64,
        f32_safe_nonzero: min_nz >= F32_SAFE_MIN_F64,
        lo: -hi,
        hi,
        min_nonzero: min_nz,
        may_be_neg_zero: true,
        sig_bits: exact_f32.then_some(24),
        dyadic_q: None,
    }
}

fn fact_satisfies_window(f: FpFact, min_exp: i16, max_exp: i16, exact_f32: bool) -> bool {
    if !f.finite {
        return false;
    }
    if exact_f32 && !f.binary32_exact {
        return false;
    }
    let hi = pow2_f64(i32::from(max_exp) + 1);
    let min_nz = pow2_f64(i32::from(min_exp));
    f.max_abs() < hi && (f.is_zero() || f.min_nonzero_abs() >= min_nz)
}

fn peel_f32_roundtrip(defs: &[Option<Op>], v: Value) -> (Value, bool) {
    if let Some(Op::X87Load {
        bits, single: true, ..
    }) = defs[v.0 as usize]
    {
        let src = peel_i32_bits(defs, bits);
        if let Some(Op::X87Store {
            value,
            width: 4,
            integer: false,
            ..
        }) = defs[src.0 as usize]
        {
            return (value, true);
        }
    }
    (v, false)
}

fn peel_add_zero(defs: &[Option<Op>], fp_facts: &[Option<FpFact>], mut v: Value) -> Value {
    let is_zero = |x: Value| fp_facts[x.0 as usize].is_some_and(FpFact::is_zero);
    for _ in 0..8 {
        match defs[v.0 as usize] {
            Some(Op::Fp(FpOp::Add, a, b, _) | Op::X87Single(FpOp::Add, a, b, _)) => {
                if is_zero(b) {
                    v = a;
                } else if is_zero(a) {
                    v = b;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    v
}

struct LoopEdge<'a> {
    head: usize,
    latch: usize,
    entry_args: &'a [Value],
    back_args: &'a [Value],
}

fn detect_counted_latch(
    func: &Func,
    defs: &[Option<Op>],
    preds: &[Vec<usize>],
    edge: &LoopEdge<'_>,
    entry_avail: &[bool],
) -> Option<CountedLatch> {
    let (head, latch) = (edge.head, edge.latch);
    let (entry_args, back_args) = (edge.entry_args, edge.back_args);
    let (branch_block, branch_target) = match &func.blocks[latch].end {
        End::Branch { .. } => (latch, head),
        End::Jump(t) if t.block.0 as usize == head && preds[latch].len() == 1 => {
            (preds[latch][0], latch)
        }
        _ => return None,
    };
    let End::Branch { cond, taken, fall } = &func.blocks[branch_block].end else {
        return None;
    };
    let (branch_taken, raw_cc, flags) = {
        let Some(Op::Test { flags, cond: cc }) = defs[cond.0 as usize] else {
            return None;
        };
        if taken.block.0 as usize == branch_target && fall.block.0 as usize != branch_target {
            (true, cc, flags)
        } else if fall.block.0 as usize == branch_target && taken.block.0 as usize != branch_target
        {
            (false, cc, flags)
        } else {
            return None;
        }
    };
    let cc = if branch_taken {
        raw_cc
    } else {
        invert_cc(raw_cc)?
    };
    let head_params = &func.blocks[head].params;
    let match_step = |v: Value| -> Option<(usize, i32)> {
        let (base, dir) = match defs[v.0 as usize]? {
            Op::Int(IntOp::Add, a, b) => match (defs[a.0 as usize], defs[b.0 as usize]) {
                (_, Some(Op::Constant(Type::I32, c))) => {
                    let s = c as u32 as i32;
                    if s == 1 || s == -1 {
                        (a, s)
                    } else {
                        return None;
                    }
                }
                (Some(Op::Constant(Type::I32, c)), _) => {
                    let s = c as u32 as i32;
                    if s == 1 || s == -1 {
                        (b, s)
                    } else {
                        return None;
                    }
                }
                _ => return None,
            },
            Op::Int(IntOp::Sub, a, b) => {
                if let Some(Op::Constant(Type::I32, c)) = defs[b.0 as usize] {
                    let s = c as u32 as i32;
                    if s == 1 || s == -1 {
                        (a, -s)
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            _ => return None,
        };
        let idx = head_params.iter().position(|&p| p == base)?;
        if func.ty(base) == Type::I32
            && back_args.get(idx) == Some(&v)
            && entry_args
                .get(idx)
                .is_some_and(|&v0| entry_avail[v0.0 as usize])
        {
            Some((idx, dir))
        } else {
            None
        }
    };

    match defs[flags.0 as usize]? {
        Op::Flags {
            kind: FlagKind::Sub,
            width: 4,
            a,
            b,
            ..
        } => {
            if let Some((idx, dir)) = match_step(a)
                && entry_avail[b.0 as usize]
                && func.ty(b) == Type::I32
            {
                let supported = matches!(
                    (dir, cc),
                    (
                        1,
                        ConditionCode::l | ConditionCode::b | ConditionCode::le | ConditionCode::ne
                    ) | (-1, ConditionCode::g | ConditionCode::ne)
                );
                if supported {
                    return Some(CountedLatch {
                        i_0: entry_args[idx],
                        limit: Some(b),
                        dir,
                        cc,
                    });
                }
            }
            if let Some((idx, dir)) = match_step(b)
                && entry_avail[a.0 as usize]
                && func.ty(a) == Type::I32
            {
                let swapped = swap_cc(cc)?;
                let supported = matches!(
                    (dir, swapped),
                    (
                        1,
                        ConditionCode::l | ConditionCode::b | ConditionCode::le | ConditionCode::ne
                    ) | (-1, ConditionCode::g | ConditionCode::ne)
                );
                if supported {
                    return Some(CountedLatch {
                        i_0: entry_args[idx],
                        limit: Some(a),
                        dir,
                        cc: swapped,
                    });
                }
            }
            None
        }
        Op::Flags {
            kind: FlagKind::Decrement,
            width: 4,
            a,
            ..
        } if cc == ConditionCode::ne => {
            // `DEC` builds `Op::Flags(Decrement, old, 1)` with the pre-decrement
            // induction value, while the back-edge carries `Sub(old, 1)`.
            let idx = head_params.iter().position(|&p| p == a)?;
            let back = *back_args.get(idx)?;
            let (step_idx, dir) = match_step(back)?;
            (step_idx == idx && dir == -1).then(|| CountedLatch {
                i_0: entry_args[idx],
                limit: None,
                dir,
                cc,
            })
        }
        _ => None,
    }
}

fn detect_affine_inductions(
    func: &Func,
    defs: &[Option<Op>],
    fp_facts: &[Option<FpFact>],
    edge: &LoopEdge<'_>,
    entry_avail: &[bool],
    latch_idx: usize,
) -> Vec<AffineInd> {
    let (head, entry_args, back_args) = (edge.head, edge.entry_args, edge.back_args);
    let mut out = Vec::new();
    for (idx, &param) in func.blocks[head].params.iter().enumerate() {
        if func.ty(param) != Type::F64 {
            continue;
        }
        let x_0 = entry_args[idx];
        let x_back = back_args[idx];
        if !entry_avail[x_0.0 as usize] {
            continue;
        }
        let (v1, rounded_f32) = peel_f32_roundtrip(defs, x_back);
        let v2 = peel_add_zero(defs, fp_facts, v1);
        let (a, b, is_single) = match defs[v2.0 as usize] {
            Some(Op::Fp(FpOp::Add, a, b, _)) => (a, b, false),
            Some(Op::X87Single(FpOp::Add, a, b, _)) => (a, b, true),
            _ => continue,
        };
        let dx = if peel_add_zero(defs, fp_facts, a) == param && entry_avail[b.0 as usize] {
            b
        } else if peel_add_zero(defs, fp_facts, b) == param && entry_avail[a.0 as usize] {
            a
        } else {
            continue;
        };
        out.push(AffineInd {
            param,
            x_0,
            dx,
            sum_val: v2,
            f32_step: rounded_f32 || is_single,
            latch_idx,
        });
    }
    out
}

fn can_build_bound(
    v: Value,
    defs: &[Option<Op>],
    fp_facts: &[Option<FpFact>],
    affine_params: &BTreeSet<Value>,
    guarded_inputs: &BTreeMap<Value, InputReq>,
    block0_avail: &[bool],
    depth: u8,
) -> Option<u8> {
    if depth == 0 {
        return None;
    }
    if affine_params.contains(&v) {
        return Some(1);
    }
    if guarded_inputs.contains_key(&v) {
        return Some(1);
    }
    if let Some(Op::Constant(Type::F64, bits)) = defs[v.0 as usize] {
        let x = f64::from_bits(bits as u64);
        if x == 0.0 || (x.is_finite() && x.abs() >= pow2_f64(-240) && x.abs() <= pow2_f64(240)) {
            return Some(0);
        }
        return None;
    }
    if block0_avail[v.0 as usize] {
        let f = fp_facts[v.0 as usize]?;
        if f.finite
            && f.max_abs() <= pow2_f64(240)
            && (f.is_zero() || f.min_nonzero_abs() >= pow2_f64(-240))
        {
            return Some(1);
        }
        return None;
    }
    match defs[v.0 as usize]? {
        Op::FpUnary(FpUnary::Neg | FpUnary::Abs, a) => can_build_bound(
            a,
            defs,
            fp_facts,
            affine_params,
            guarded_inputs,
            block0_avail,
            depth - 1,
        ),
        Op::Fp(FpOp::Add | FpOp::Sub, a, b, _) | Op::X87Single(FpOp::Add | FpOp::Sub, a, b, _) => {
            if fp_facts[b.0 as usize].is_some_and(FpFact::is_zero) {
                return can_build_bound(
                    a,
                    defs,
                    fp_facts,
                    affine_params,
                    guarded_inputs,
                    block0_avail,
                    depth - 1,
                );
            }
            if fp_facts[a.0 as usize].is_some_and(FpFact::is_zero) {
                return can_build_bound(
                    b,
                    defs,
                    fp_facts,
                    affine_params,
                    guarded_inputs,
                    block0_avail,
                    depth - 1,
                );
            }
            let da = can_build_bound(
                a,
                defs,
                fp_facts,
                affine_params,
                guarded_inputs,
                block0_avail,
                depth - 1,
            )?;
            let db = can_build_bound(
                b,
                defs,
                fp_facts,
                affine_params,
                guarded_inputs,
                block0_avail,
                depth - 1,
            )?;
            Some(da.max(db))
        }
        Op::Fp(FpOp::Mul, a, b, _) | Op::X87Single(FpOp::Mul, a, b, _) => {
            let da = can_build_bound(
                a,
                defs,
                fp_facts,
                affine_params,
                guarded_inputs,
                block0_avail,
                depth - 1,
            )?;
            let db = can_build_bound(
                b,
                defs,
                fp_facts,
                affine_params,
                guarded_inputs,
                block0_avail,
                depth - 1,
            )?;
            let sum = da.saturating_add(db);
            (sum <= MAX_BOUND_DEGREE).then_some(sum)
        }
        _ => None,
    }
}

struct EntryBuilder<'a> {
    func: &'a mut Func,
    insts: Vec<(Option<Value>, Op)>,
}

impl<'a> EntryBuilder<'a> {
    fn emit(&mut self, op: Op) -> Option<Value> {
        let result = self.func.result(&op).map(|ty| {
            let v = Value(self.func.types.len() as u32);
            self.func.types.push(ty);
            v
        });
        self.insts.push((result, op));
        result
    }

    fn const_i32(&mut self, n: u32) -> Value {
        self.emit(Op::Constant(Type::I32, u128::from(n))).unwrap()
    }

    fn const_f64(&mut self, x: f64) -> Value {
        self.emit(Op::Constant(Type::F64, u128::from(x.to_bits())))
            .unwrap()
    }

    fn cmp_i32(&mut self, a: Value, b: Value, cond: ConditionCode) -> Value {
        let flags = self
            .emit(Op::Flags {
                kind: FlagKind::Sub,
                width: 4,
                a,
                b,
                prior: None,
            })
            .unwrap();
        self.emit(Op::Test { flags, cond }).unwrap()
    }

    fn materialize(&mut self, v: Value, defs: &[Option<Op>], block0_avail: &[bool]) -> Value {
        if block0_avail[v.0 as usize] {
            return v;
        }
        match defs[v.0 as usize] {
            Some(Op::Constant(ty, bits)) => self.emit(Op::Constant(ty, bits)).unwrap(),
            _ => unreachable!("entry_avail guarantees Block(0) or Constant"),
        }
    }

    fn build_trip_count(
        &mut self,
        latch: CountedLatch,
        defs: &[Option<Op>],
        block0_avail: &[bool],
    ) -> Value {
        let i_0 = self.materialize(latch.i_0, defs, block0_avail);
        let limit_val = latch.limit.map(|l| self.materialize(l, defs, block0_avail));
        let one = self.const_i32(1);
        let fail = self.const_i32(u32::MAX);
        match (latch.dir, latch.cc) {
            (1, ConditionCode::l) => {
                let limit = limit_val.unwrap();
                let diff = self.emit(Op::Int(IntOp::Sub, limit, i_0)).unwrap();
                let in_order = self.cmp_i32(i_0, limit, ConditionCode::l);
                let imax = self.const_i32(i32::MAX as u32);
                let not_max = self.cmp_i32(i_0, imax, ConditionCode::ne);
                let fallback = self
                    .emit(Op::Select {
                        cond: not_max,
                        a: one,
                        b: fail,
                    })
                    .unwrap();
                self.emit(Op::Select {
                    cond: in_order,
                    a: diff,
                    b: fallback,
                })
                .unwrap()
            }
            (1, ConditionCode::b) => {
                let limit = limit_val.unwrap();
                let diff = self.emit(Op::Int(IntOp::Sub, limit, i_0)).unwrap();
                let in_order = self.cmp_i32(i_0, limit, ConditionCode::b);
                let not_max = self.cmp_i32(i_0, fail, ConditionCode::ne);
                let fallback = self
                    .emit(Op::Select {
                        cond: not_max,
                        a: one,
                        b: fail,
                    })
                    .unwrap();
                self.emit(Op::Select {
                    cond: in_order,
                    a: diff,
                    b: fallback,
                })
                .unwrap()
            }
            (1, ConditionCode::le) => {
                let limit = limit_val.unwrap();
                let diff = self.emit(Op::Int(IntOp::Sub, limit, i_0)).unwrap();
                let count = self.emit(Op::Int(IntOp::Add, diff, one)).unwrap();
                let in_order = self.cmp_i32(i_0, limit, ConditionCode::le);
                let imax = self.const_i32(i32::MAX as u32);
                let lim_not_max = self.cmp_i32(limit, imax, ConditionCode::ne);
                let i_not_max = self.cmp_i32(i_0, imax, ConditionCode::ne);
                let safe_count = self
                    .emit(Op::Select {
                        cond: lim_not_max,
                        a: count,
                        b: fail,
                    })
                    .unwrap();
                let fallback = self
                    .emit(Op::Select {
                        cond: i_not_max,
                        a: one,
                        b: fail,
                    })
                    .unwrap();
                self.emit(Op::Select {
                    cond: in_order,
                    a: safe_count,
                    b: fallback,
                })
                .unwrap()
            }
            (-1, ConditionCode::g) => {
                let limit = limit_val.unwrap();
                let diff = self.emit(Op::Int(IntOp::Sub, i_0, limit)).unwrap();
                let in_order = self.cmp_i32(i_0, limit, ConditionCode::g);
                let imin = self.const_i32(i32::MIN as u32);
                let not_min = self.cmp_i32(i_0, imin, ConditionCode::ne);
                let fallback = self
                    .emit(Op::Select {
                        cond: not_min,
                        a: one,
                        b: fail,
                    })
                    .unwrap();
                self.emit(Op::Select {
                    cond: in_order,
                    a: diff,
                    b: fallback,
                })
                .unwrap()
            }
            (1, ConditionCode::ne) => {
                let limit = limit_val.unwrap();
                let diff = self.emit(Op::Int(IntOp::Sub, limit, i_0)).unwrap();
                let not_eq = self.cmp_i32(i_0, limit, ConditionCode::ne);
                self.emit(Op::Select {
                    cond: not_eq,
                    a: diff,
                    b: fail,
                })
                .unwrap()
            }
            (-1, ConditionCode::ne) => {
                let limit = limit_val.unwrap_or_else(|| self.const_i32(0));
                let diff = self.emit(Op::Int(IntOp::Sub, i_0, limit)).unwrap();
                let not_eq = self.cmp_i32(i_0, limit, ConditionCode::ne);
                self.emit(Op::Select {
                    cond: not_eq,
                    a: diff,
                    b: fail,
                })
                .unwrap()
            }
            _ => unreachable!("filtered by detect_counted_latch"),
        }
    }

    fn emit_bound(
        &mut self,
        v: Value,
        defs: &[Option<Op>],
        fp_facts: &[Option<FpFact>],
        sym_bounds: &BTreeMap<Value, Value>,
        block0_avail: &[bool],
        cache: &mut BTreeMap<Value, Value>,
    ) -> Value {
        if let Some(&cached) = cache.get(&v) {
            return cached;
        }
        if let Some(&b) = sym_bounds.get(&v) {
            return b;
        }
        let res = if let Some(Op::Constant(Type::F64, bits)) = defs[v.0 as usize] {
            self.const_f64(f64::from_bits(bits as u64).abs())
        } else if block0_avail[v.0 as usize] {
            self.emit(Op::FpUnary(FpUnary::Abs, v)).unwrap()
        } else {
            match defs[v.0 as usize].unwrap() {
                Op::FpUnary(FpUnary::Neg | FpUnary::Abs, a) => {
                    self.emit_bound(a, defs, fp_facts, sym_bounds, block0_avail, cache)
                }
                Op::Fp(FpOp::Add | FpOp::Sub, a, b, _)
                | Op::X87Single(FpOp::Add | FpOp::Sub, a, b, _) => {
                    if fp_facts[b.0 as usize].is_some_and(FpFact::is_zero) {
                        self.emit_bound(a, defs, fp_facts, sym_bounds, block0_avail, cache)
                    } else if fp_facts[a.0 as usize].is_some_and(FpFact::is_zero) {
                        self.emit_bound(b, defs, fp_facts, sym_bounds, block0_avail, cache)
                    } else {
                        let ba =
                            self.emit_bound(a, defs, fp_facts, sym_bounds, block0_avail, cache);
                        let bb =
                            self.emit_bound(b, defs, fp_facts, sym_bounds, block0_avail, cache);
                        self.emit(Op::Fp(FpOp::Add, ba, bb, true)).unwrap()
                    }
                }
                Op::Fp(FpOp::Mul, a, b, _) | Op::X87Single(FpOp::Mul, a, b, _) => {
                    let ba = self.emit_bound(a, defs, fp_facts, sym_bounds, block0_avail, cache);
                    let bb = self.emit_bound(b, defs, fp_facts, sym_bounds, block0_avail, cache);
                    self.emit(Op::Fp(FpOp::Mul, ba, bb, true)).unwrap()
                }
                _ => unreachable!("verified by can_build_bound"),
            }
        };
        cache.insert(v, res);
        res
    }
}

/// Infers optional `Block(0)` numerical domain guards for resident x87 loops
/// when static analysis leaves unproved arithmetic or stores inside the loop.
pub(super) fn infer_domain_guards(
    func: &mut Func,
    refused_domain: &dyn Fn(u32) -> bool,
) -> Option<DomainSeeds> {
    if !func.resident
        || func.blocks.len() <= 1
        || func.domain_guards.len() >= MAX_DOMAIN_GUARDS
        || !func.domain_guards.is_empty()
    {
        return None;
    }
    // Block(0) must be a one-shot entry prologue (never a loop head).
    if func
        .blocks
        .iter()
        .flat_map(|b| b.end.targets())
        .any(|t| t.block == Block(0))
    {
        return None;
    }

    let first_effect = func.blocks[0]
        .insts
        .iter()
        .position(|(_, op)| {
            matches!(
                op,
                Op::Store { .. } | Op::FrameStore { .. } | Op::ScopeWritten(_)
            )
        })
        .unwrap_or(func.blocks[0].insts.len());
    let last_plain = func.blocks[0]
        .insts
        .iter()
        .rposition(|(_, op)| matches!(op, Op::PlainPage { .. }))
        .map_or(0, |i| i + 1);
    if first_effect < last_plain {
        return None;
    }
    let insert_pos = first_effect;

    let n = func.types.len();
    let mut block0_avail = vec![false; n];
    for &p in &func.blocks[0].params {
        block0_avail[p.0 as usize] = true;
    }
    for &(r, _) in &func.blocks[0].insts[..insert_pos] {
        if let Some(r) = r {
            block0_avail[r.0 as usize] = true;
        }
    }

    let defs = super::definitions(func);
    let mut entry_avail = block0_avail.clone();
    for (i, def) in defs.iter().enumerate() {
        if matches!(def, Some(Op::Constant(..))) {
            entry_avail[i] = true;
        }
    }

    let dom = dominators(func);
    let has_dom = |b: usize, d: usize| dom[b][d / 64] >> (d % 64) & 1 != 0;

    // Locate all single-entry, single-backedge natural loops.
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); func.blocks.len()];
    for (b, block) in func.blocks.iter().enumerate() {
        for t in block.end.targets() {
            preds[t.block.0 as usize].push(b);
        }
    }
    let mut loops = Vec::new();
    for (head, head_preds) in preds.iter().enumerate().skip(1) {
        let mut non_back = Vec::new();
        let mut backedges = Vec::new();
        for &p in head_preds {
            if has_dom(p, head) {
                backedges.push(p);
            } else {
                non_back.push(p);
            }
        }
        if non_back.len() == 1 && backedges.len() == 1 {
            loops.push((head, non_back[0], backedges[0]));
        }
    }
    if loops.is_empty() {
        return None;
    }

    let primary_head = loops
        .iter()
        .find(|&&(_, entry, _)| entry == 0)
        .map_or(loops[0].0, |&(h, _, _)| h);
    let guard_id = func.blocks[primary_head]
        .head
        .map(|h| func.records[h.record.0 as usize].eip)
        .unwrap_or_else(|| func.records[0].eip);
    if refused_domain(guard_id) {
        return None;
    }

    // Union of natural loop blocks across all detected loops.
    let mut in_loop = vec![false; func.blocks.len()];
    for &(head, _, latch) in &loops {
        in_loop[head] = true;
        let mut work = vec![latch];
        while let Some(b) = work.pop() {
            if !in_loop[b] && has_dom(b, head) {
                in_loop[b] = true;
                for &p in &preds[b] {
                    if !in_loop[p] {
                        work.push(p);
                    }
                }
            }
        }
    }

    // Check whether any loop still has unproved x87 work.
    let has_unproved = func
        .blocks
        .iter()
        .enumerate()
        .filter(|(b, _)| in_loop[*b])
        .flat_map(|(_, blk)| &blk.insts)
        .any(|(_, op)| match *op {
            Op::Fp(.., false) | Op::X87Store { proved: false, .. } => true,
            Op::X87Single(.., cur) => {
                cur & (SINGLE_A | SINGLE_B | SINGLE_RESULT) != (SINGLE_A | SINGLE_B | SINGLE_RESULT)
            }
            _ => false,
        });
    if !has_unproved {
        return None;
    }

    let (_, static_fp) = analyze_fixpoint(func, &defs, &DomainSeeds::default());

    let mut latches = Vec::<CountedLatch>::new();
    let mut affine_inds = Vec::<AffineInd>::new();
    for &(head, entry_pred, latch) in &loops {
        let Some(entry_args) = func.blocks[entry_pred]
            .end
            .targets()
            .find(|t| t.block.0 as usize == head)
            .map(|t| t.args.clone())
        else {
            continue;
        };
        let Some(back_args) = func.blocks[latch]
            .end
            .targets()
            .find(|t| t.block.0 as usize == head)
            .map(|t| t.args.clone())
        else {
            continue;
        };
        let edge = LoopEdge {
            head,
            latch,
            entry_args: &entry_args,
            back_args: &back_args,
        };
        let Some(cl) = detect_counted_latch(func, &defs, &preds, &edge, &entry_avail) else {
            continue;
        };
        let latch_idx = latches.len();
        let inds =
            detect_affine_inductions(func, &defs, &static_fp, &edge, &entry_avail, latch_idx);
        if !inds.is_empty() {
            latches.push(cl);
            affine_inds.extend(inds);
        }
    }
    let affine_map: BTreeMap<Value, AffineInd> =
        affine_inds.iter().map(|&ind| (ind.param, ind)).collect();
    let affine_params: BTreeSet<Value> = affine_map.keys().copied().collect();

    // Propagate exact_f32 and f32_store requirements backwards from loop x87 ops
    // to Block(0) F64 inputs and affine inductions.
    let mut val_reqs = vec![InputReq::default(); n];
    let mut queue: Vec<Value> = Vec::new();
    let mark_req =
        |v: Value, exact: bool, store: bool, reqs: &mut [InputReq], q: &mut Vec<Value>| {
            if func.ty(v) != Type::F64 {
                return;
            }
            let r = &mut reqs[v.0 as usize];
            let old = (r.needs_exact_f32, r.needs_f32_store);
            r.needs_exact_f32 |= exact;
            r.needs_f32_store |= store;
            if (r.needs_exact_f32, r.needs_f32_store) != old {
                q.push(v);
            }
        };

    let mut work_seeds = Vec::new();
    for (b, block) in func.blocks.iter().enumerate() {
        if !in_loop[b] {
            continue;
        }
        for &(_, op) in &block.insts {
            match op {
                Op::Fp(_, a, b_val, false) => {
                    work_seeds.push(a);
                    work_seeds.push(b_val);
                }
                Op::X87Single(_, a, b_val, cur)
                    if cur & (SINGLE_A | SINGLE_B | SINGLE_RESULT)
                        != (SINGLE_A | SINGLE_B | SINGLE_RESULT) =>
                {
                    mark_req(a, true, true, &mut val_reqs, &mut queue);
                    mark_req(b_val, true, true, &mut val_reqs, &mut queue);
                    work_seeds.push(a);
                    work_seeds.push(b_val);
                }
                Op::X87Store {
                    value,
                    integer: false,
                    proved: false,
                    ..
                } => {
                    mark_req(value, false, true, &mut val_reqs, &mut queue);
                    work_seeds.push(value);
                }
                Op::X87Store {
                    value,
                    integer: true,
                    proved: false,
                    ..
                } => {
                    work_seeds.push(value);
                }
                _ => {}
            }
        }
    }
    // Propagate requirements backwards across loop F64 ops and affine inductions.
    while let Some(v) = queue.pop() {
        let req = val_reqs[v.0 as usize];
        if let Some(&ind) = affine_map.get(&v) {
            mark_req(
                ind.x_0,
                req.needs_exact_f32,
                req.needs_f32_store,
                &mut val_reqs,
                &mut queue,
            );
            mark_req(
                ind.dx,
                req.needs_exact_f32,
                req.needs_f32_store,
                &mut val_reqs,
                &mut queue,
            );
            continue;
        }
        if block0_avail[v.0 as usize] {
            continue;
        }
        if let Some(op) = defs[v.0 as usize] {
            match op {
                Op::Fp(_, a, b, _) | Op::X87Single(_, a, b, _) => {
                    mark_req(a, req.needs_exact_f32, false, &mut val_reqs, &mut queue);
                    mark_req(b, req.needs_exact_f32, false, &mut val_reqs, &mut queue);
                }
                Op::FpUnary(_, a) => {
                    mark_req(
                        a,
                        req.needs_exact_f32,
                        req.needs_f32_store,
                        &mut val_reqs,
                        &mut queue,
                    );
                }
                _ => {}
            }
        }
    }
    for &ind in &affine_inds {
        if ind.f32_step {
            let r = val_reqs[ind.param.0 as usize];
            val_reqs[ind.param.0 as usize].needs_f32_store = true;
            val_reqs[ind.x_0.0 as usize].needs_f32_store = true;
            val_reqs[ind.dx.0 as usize].needs_f32_store = true;
            val_reqs[ind.x_0.0 as usize].needs_exact_f32 |= r.needs_exact_f32;
            val_reqs[ind.dx.0 as usize].needs_exact_f32 |= r.needs_exact_f32;
        }
    }

    // Collect all Block(0) F64 inputs reachable from unproved loop x87 ops.
    let mut visited = vec![false; n];
    let mut guarded_inputs = BTreeMap::<Value, InputReq>::new();
    while let Some(v) = work_seeds.pop() {
        if visited[v.0 as usize] || func.ty(v) != Type::F64 {
            continue;
        }
        visited[v.0 as usize] = true;
        if let Some(&ind) = affine_map.get(&v) {
            work_seeds.push(ind.x_0);
            work_seeds.push(ind.dx);
            continue;
        }
        if block0_avail[v.0 as usize] {
            if matches!(defs[v.0 as usize], Some(Op::Constant(..))) {
                continue;
            }
            let req = val_reqs[v.0 as usize];
            let (min_e, max_e) = req.window();
            let already_ok = static_fp[v.0 as usize]
                .is_some_and(|f| fact_satisfies_window(f, min_e, max_e, req.needs_exact_f32));
            if !already_ok {
                guarded_inputs.insert(v, req);
            }
            continue;
        }
        if let Some(op) = defs[v.0 as usize] {
            for u in op.uses() {
                if func.ty(u) == Type::F64 {
                    work_seeds.push(u);
                }
            }
        }
    }
    if guarded_inputs.len() > MAX_GUARDED_INPUTS {
        return None;
    }

    // Build abstract seeds for profitability simulation.
    let mut seeds = DomainSeeds::default();
    for (&v, &req) in &guarded_inputs {
        let (min_e, max_e) = req.window();
        let exact =
            req.needs_exact_f32 || static_fp[v.0 as usize].is_some_and(|f| f.binary32_exact);
        seeds.inputs.insert(v, fp_window_fact(min_e, max_e, exact));
    }

    // Seed affine induction invariants and their step sums.
    let mut active_inds = Vec::new();
    for &ind in &affine_inds {
        let fact_of = |v: Value| {
            let base = static_fp[v.0 as usize].unwrap_or(FpFact::TOP);
            if let Some(&seed) = seeds.inputs.get(&v) {
                base.meet(seed)
            } else {
                base
            }
        };
        let fx0 = fact_of(ind.x_0);
        let fdx = fact_of(ind.dx);
        if !fx0.finite || !fdx.finite {
            continue;
        }
        let req = val_reqs[ind.param.0 as usize];
        let x_limit = if req.needs_exact_f32 {
            pow2_f64(i32::from(F32_EXACT_MAX_EXP))
        } else if ind.f32_step || req.needs_f32_store {
            F32_MAX_F64
        } else {
            pow2_f64(i32::from(F64_WIN_MAX_EXP))
        };
        let min_nz = fx0.min_nonzero_abs().min(fdx.min_nonzero_abs());
        let binary32_exact = fx0.binary32_exact && ind.f32_step;
        let inv = FpFact {
            finite: true,
            binary32_exact,
            f32_normal_or_zero: min_nz >= F32_MIN_NORMAL_F64,
            f32_safe_nonzero: min_nz >= F32_SAFE_MIN_F64,
            lo: -x_limit,
            hi: x_limit,
            min_nonzero: min_nz,
            may_be_neg_zero: fx0.may_be_neg_zero,
            sig_bits: binary32_exact.then_some(24),
            dyadic_q: None,
        };
        seeds.params.insert(ind.param, inv);
        seeds.values.insert(ind.sum_val, (x_limit, Some(min_nz)));
        active_inds.push((ind, x_limit, fx0.is_zero(), fdx.is_zero()));
    }

    // Collect integer X87Store sinks whose symbolic bounds can be checked in Block(0).
    let mut int_sinks = BTreeMap::<Value, u8>::new();
    for (b, block) in func.blocks.iter().enumerate() {
        if !in_loop[b] {
            continue;
        }
        for &(_, op) in &block.insts {
            if let Op::X87Store {
                value,
                width,
                integer: true,
                proved: false,
                ..
            } = op
                && can_build_bound(
                    value,
                    &defs,
                    &static_fp,
                    &affine_params,
                    &guarded_inputs,
                    &block0_avail,
                    6,
                )
                .is_some()
            {
                int_sinks
                    .entry(value)
                    .and_modify(|w| *w = (*w).min(width))
                    .or_insert(width);
            }
        }
    }
    for (&sink, &width) in &int_sinks {
        let (_, widened) = safe_int_limit(width);
        seeds.values.insert(sink, (widened, None));
    }

    if seeds.inputs.is_empty() && active_inds.is_empty() && int_sinks.is_empty() {
        return None;
    }

    // Simulate fixpoint with the planned domain guards to verify profitability.
    let (_, guarded_fp) = analyze_fixpoint(func, &defs, &seeds);
    // The guard's price is settled at run time on its own ledger (policy::resident),
    // so the pass exports any positive saving instead of gating on a threshold.
    let (_, saved_per_iter) = apply_proved(func, &defs, &guarded_fp, Some(&in_loop), false);
    if saved_per_iter == 0 {
        return None;
    }

    // Optional independent secondary ordinal: guard Block(0) binary64 inputs
    // feeding PC=53 Mul -> Add/Sub with a binary32-exact multiplicand so
    // Phase 2.1 can contract to single-instruction FMADD/FMSUB/FNMSUB D.
    // Separate guard_id ensures non-f32 f64 workloads withdraw ONLY FMA exactness
    // without touching ordinal 0 finiteness/range proofs.
    let fma_guard_id = guard_id ^ 0x464d_4131;
    let mut fma_exact_inputs = BTreeSet::<Value>::new();
    let mut fma_pairs = 0u32;
    if !refused_domain(fma_guard_id) && func.domain_guards.len() + 1 < MAX_DOMAIN_GUARDS {
        let fma_need = |v: Value| -> Option<Option<Value>> {
            let fv = guarded_fp[v.0 as usize].unwrap_or(FpFact::TOP);
            if !fv.finite {
                return None;
            }
            if fv.binary32_exact {
                return Some(None);
            }
            if guarded_inputs.contains_key(&v) {
                return Some(Some(v));
            }
            if let Some(&ind) = affine_map.get(&v)
                && ind.f32_step
                && guarded_inputs.contains_key(&ind.x_0)
            {
                return Some(Some(ind.x_0));
            }
            None
        };
        for (b, block) in func.blocks.iter().enumerate() {
            if !in_loop[b] {
                continue;
            }
            let mut mul_to_need = BTreeMap::<Value, (Option<Value>, Option<Value>)>::new();
            for &(res, op) in &block.insts {
                match op {
                    Op::Store { .. }
                    | Op::FrameStore { .. }
                    | Op::ScopeWritten(_)
                    | Op::PageCross { .. }
                    | Op::FrameDisjoint { .. } => {
                        mul_to_need.clear();
                    }
                    Op::Fp(FpOp::Mul, a, b_val, _) => {
                        if let Some(r) = res
                            && let (Some(na), Some(nb)) = (fma_need(a), fma_need(b_val))
                            && (na.is_some() || nb.is_some())
                        {
                            mul_to_need.insert(r, (na, nb));
                        }
                    }
                    Op::Fp(FpOp::Add | FpOp::Sub, lhs, rhs, _) => {
                        let matched = mul_to_need
                            .get(&lhs)
                            .map(|&need| (need, rhs))
                            .or_else(|| mul_to_need.get(&rhs).map(|&need| (need, lhs)));
                        if let Some(((na, nb), c)) = matched
                            && guarded_fp[c.0 as usize].is_some_and(|fc| fc.finite)
                        {
                            if let Some(v) = na {
                                fma_exact_inputs.insert(v);
                            }
                            if let Some(v) = nb {
                                fma_exact_inputs.insert(v);
                            }
                            fma_pairs += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Commit: emit Block(0) helper ops and DomainGuards at `insert_pos`.
    let ordinal = func.domain_guards.len() as u8;
    func.domain_guards.push(guard_id);
    func.domain_saved_per_iter.push(saved_per_iter);
    let fma_ordinal = if !fma_exact_inputs.is_empty() {
        let ord = func.domain_guards.len() as u8;
        func.domain_guards.push(fma_guard_id);
        func.domain_saved_per_iter.push(fma_pairs);
        Some(ord)
    } else {
        None
    };

    let mut eb = EntryBuilder {
        func,
        insts: Vec::new(),
    };
    for (&v, &req) in &guarded_inputs {
        let (min_exp, max_exp) = req.window();
        let exact_f32 =
            req.needs_exact_f32 && !static_fp[v.0 as usize].is_some_and(|f| f.binary32_exact);
        eb.emit(Op::DomainGuard {
            kind: DomainGuardKind::FpWindow {
                value: v,
                min_exp,
                max_exp,
                exact_f32,
            },
            ordinal,
        });
    }
    if let Some(ord) = fma_ordinal {
        for &v in &fma_exact_inputs {
            eb.emit(Op::DomainGuard {
                kind: DomainGuardKind::ExactF32 { value: v },
                ordinal: ord,
            });
            if let Some(seed) = seeds.inputs.get_mut(&v) {
                seed.binary32_exact = true;
                seed.sig_bits = Some(24);
            }
        }
        for &(ind, ..) in &active_inds {
            if ind.f32_step
                && fma_exact_inputs.contains(&ind.x_0)
                && let Some(param_seed) = seeds.params.get_mut(&ind.param)
            {
                param_seed.binary32_exact = true;
                param_seed.sig_bits = Some(24);
            }
        }
    }

    let mut sym_bounds = BTreeMap::<Value, Value>::new();
    let mut bound_cache = BTreeMap::<Value, Value>::new();
    if !active_inds.is_empty() {
        let mut latch_n_f64 = BTreeMap::<usize, Value>::new();
        let two = eb.const_f64(2.0);
        for &(ind, x_limit, x0_zero, dx_zero) in &active_inds {
            let n_f64 = *latch_n_f64.entry(ind.latch_idx).or_insert_with(|| {
                let n_val = eb.build_trip_count(latches[ind.latch_idx], &defs, &block0_avail);
                eb.emit(Op::DomainGuard {
                    kind: DomainGuardKind::U32Le {
                        value: n_val,
                        max: MAX_DOMAIN_TRIP_COUNT,
                    },
                    ordinal,
                });
                eb.emit(Op::FromInt(n_val)).unwrap()
            });
            let x_0 = eb.materialize(ind.x_0, &defs, &block0_avail);
            let dx = eb.materialize(ind.dx, &defs, &block0_avail);
            if !x0_zero && !dx_zero {
                eb.emit(Op::DomainGuard {
                    kind: DomainGuardKind::SameSign { a: x_0, b: dx },
                    ordinal,
                });
            }
            let abs_dx = eb.emit(Op::FpUnary(FpUnary::Abs, dx)).unwrap();
            let span = eb.emit(Op::Fp(FpOp::Mul, n_f64, abs_dx, true)).unwrap();
            let total = if x0_zero {
                span
            } else {
                let abs_x0 = eb.emit(Op::FpUnary(FpUnary::Abs, x_0)).unwrap();
                eb.emit(Op::Fp(FpOp::Add, abs_x0, span, true)).unwrap()
            };
            let x_max = eb.emit(Op::Fp(FpOp::Mul, two, total, true)).unwrap();
            eb.emit(Op::DomainGuard {
                kind: DomainGuardKind::FpLe {
                    value: x_max,
                    bound_bits: x_limit.to_bits(),
                },
                ordinal,
            });
            sym_bounds.insert(ind.param, x_max);
        }
    }

    for (&sink, &width) in &int_sinks {
        let bound_val = eb.emit_bound(
            sink,
            &defs,
            &static_fp,
            &sym_bounds,
            &block0_avail,
            &mut bound_cache,
        );
        let (lim, _) = safe_int_limit(width);
        eb.emit(Op::DomainGuard {
            kind: DomainGuardKind::FpLe {
                value: bound_val,
                bound_bits: lim.to_bits(),
            },
            ordinal,
        });
    }

    let inserted = eb.insts;
    func.blocks[0]
        .insts
        .splice(insert_pos..insert_pos, inserted);

    // Apply proved flags using the simulated `guarded_fp` facts.
    let new_defs = super::definitions(func);
    apply_proved(func, &new_defs, &guarded_fp, Some(&in_loop), true);
    Some(seeds)
}

fn fp_mul_is_exact_f64(a: FpFact, b: FpFact) -> bool {
    if !a.finite || !b.finite {
        return false;
    }
    let (proved, fact) = eval_fp(FpOp::Mul, a, b, false);
    if !proved || !fact.finite {
        return false;
    }
    if a.binary32_exact && b.binary32_exact {
        return true;
    }
    if let (Some(wa), Some(wb)) = (a.effective_sig_bits(), b.effective_sig_bits())
        && wa.saturating_add(wb) <= 53
    {
        return true;
    }
    fact.dyadic_q
        .is_some_and(|q| q >= -1022 && fits_mantissa_bits(fact.max_abs(), q, 53))
}

/// Phase 2.1: Contracts exact `Mul -> Add/Sub` pairs into `Op::Fma` after
/// `passes::dead`, preserving recovery regions, `Recipe::C1` observability,
/// sign semantics, and `PC=24` input exactness checks.
pub(super) fn contract_fma(func: &mut Func, domain_seeds: Option<&DomainSeeds>) -> bool {
    let has_candidate = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .any(|(_, op)| matches!(op, Op::Fp(FpOp::Mul, ..) | Op::X87Single(FpOp::Mul, ..)));
    if !has_candidate {
        return false;
    }

    let n = func.types.len();
    let defs = super::definitions(func);
    let default_seeds = DomainSeeds::default();
    let seeds = domain_seeds.unwrap_or(&default_seeds);
    let (_, fp_facts) = analyze_fixpoint(func, &defs, seeds);
    let get_fp = |v: Value| {
        fp_facts
            .get(v.0 as usize)
            .copied()
            .flatten()
            .unwrap_or(FpFact::TOP)
    };

    // Count all non-C1 uses across blocks, terminators, and IrRecords.
    // Lazy Recipe::C1 references to an Add/Sub consuming `prod` are tracked
    // separately so contracting Mul+Add/Sub can rewrite them to Recipe::C1Fma
    // without keeping `prod` alive in the hot loop.
    let mut non_c1_uses = vec![0usize; n];
    for block in &func.blocks {
        for &(_, op) in &block.insts {
            for u in op.uses() {
                non_c1_uses[u.0 as usize] += 1;
            }
        }
        if let End::Branch { cond, .. } = block.end {
            non_c1_uses[cond.0 as usize] += 1;
        }
        for t in block.end.targets() {
            for &arg in &t.args {
                non_c1_uses[arg.0 as usize] += 1;
            }
        }
    }
    let mut c1_records = Vec::<(usize, FpOp, Value, Value, bool)>::new();
    for (rec_idx, record) in func.records.iter().enumerate() {
        for u in record.uses() {
            non_c1_uses[u.0 as usize] += 1;
        }
        if let Some(x87) = &record.x87
            && let Some(Location::Recipe(Recipe::C1 {
                op: op @ (FpOp::Add | FpOp::Sub),
                a,
                b,
                single,
            })) = x87.c1
        {
            non_c1_uses[a.0 as usize] -= 1;
            non_c1_uses[b.0 as usize] -= 1;
            c1_records.push((rec_idx, op, a, b, single));
        }
    }

    let c1_compatible = |prod: Value, fp_op: FpOp, lhs: Value, rhs: Value, single: bool| {
        c1_records
            .iter()
            .filter(|&&(_, _, ra, rb, _)| ra == prod || rb == prod)
            .all(|&(_, r_op, ra, rb, r_single)| {
                r_op == fp_op && ra == lhs && rb == rhs && r_single == single
            })
    };

    let mut c1_rewrites = Vec::<(Value, FmaOp, Value, Value, Value, bool)>::new();
    let mut contracted = false;
    for block in &mut func.blocks {
        // prod -> (inst_idx, region_id, single, a, b)
        let mut muls = BTreeMap::<Value, (usize, u32, bool, Value, Value)>::new();
        let mut removed = BTreeSet::<usize>::new();
        let mut region_id = 0u32;

        for idx in 0..block.insts.len() {
            let (result, op) = block.insts[idx];
            match op {
                Op::Store { .. }
                | Op::FrameStore { .. }
                | Op::ScopeWritten(_)
                | Op::PageCross { .. }
                | Op::FrameDisjoint { .. } => {
                    region_id += 1;
                }
                Op::Fp(FpOp::Mul, a, b, _) => {
                    if let Some(prod) = result
                        && non_c1_uses[prod.0 as usize] == 1
                        && fp_mul_is_exact_f64(get_fp(a), get_fp(b))
                    {
                        muls.insert(prod, (idx, region_id, false, a, b));
                    }
                }
                Op::X87Single(FpOp::Mul, a, b, mul_flags) => {
                    let all = SINGLE_A | SINGLE_B | SINGLE_RESULT | SINGLE_EXACT;
                    if let Some(prod) = result
                        && non_c1_uses[prod.0 as usize] == 1
                        && mul_flags & all == all
                        && get_fp(a).finite
                        && get_fp(b).finite
                    {
                        muls.insert(prod, (idx, region_id, true, a, b));
                    }
                }
                Op::Fp(fp_op @ (FpOp::Add | FpOp::Sub), lhs, rhs, add_proved) => {
                    let matched = if let Some(&(mul_idx, mul_reg, false, a, b)) = muls.get(&lhs)
                        && mul_reg == region_id
                    {
                        let fma_op = if fp_op == FpOp::Add {
                            FmaOp::Add
                        } else {
                            FmaOp::Sub
                        };
                        Some((lhs, mul_idx, fma_op, a, b, rhs))
                    } else if let Some(&(mul_idx, mul_reg, false, a, b)) = muls.get(&rhs)
                        && mul_reg == region_id
                    {
                        let fma_op = if fp_op == FpOp::Add {
                            FmaOp::Add
                        } else {
                            FmaOp::RevSub
                        };
                        Some((rhs, mul_idx, fma_op, a, b, lhs))
                    } else {
                        None
                    };
                    if let Some((prod, mul_idx, fma_op, a, b, c)) = matched
                        && get_fp(c).finite
                        && c1_compatible(prod, fp_op, lhs, rhs, false)
                    {
                        let proved = if add_proved
                            || eval_fp(fp_op, get_fp(lhs), get_fp(rhs), lhs == rhs).0
                        {
                            SINGLE_RESULT
                        } else {
                            0
                        };
                        block.insts[idx].1 = Op::Fma {
                            op: fma_op,
                            a,
                            b,
                            c,
                            single: false,
                            proved,
                        };
                        removed.insert(mul_idx);
                        c1_rewrites.push((prod, fma_op, a, b, c, false));
                        contracted = true;
                    }
                }
                Op::X87Single(fp_op @ (FpOp::Add | FpOp::Sub), lhs, rhs, add_flags) => {
                    let matched = if let Some(&(mul_idx, mul_reg, true, a, b)) = muls.get(&lhs)
                        && mul_reg == region_id
                    {
                        let fma_op = if fp_op == FpOp::Add {
                            FmaOp::Add
                        } else {
                            FmaOp::Sub
                        };
                        let c_proved = add_flags & SINGLE_B != 0 || get_fp(rhs).binary32_exact;
                        Some((lhs, mul_idx, fma_op, a, b, rhs, c_proved))
                    } else if let Some(&(mul_idx, mul_reg, true, a, b)) = muls.get(&rhs)
                        && mul_reg == region_id
                    {
                        let fma_op = if fp_op == FpOp::Add {
                            FmaOp::Add
                        } else {
                            FmaOp::RevSub
                        };
                        let c_proved = add_flags & SINGLE_A != 0 || get_fp(lhs).binary32_exact;
                        Some((rhs, mul_idx, fma_op, a, b, lhs, c_proved))
                    } else {
                        None
                    };
                    if let Some((prod, mul_idx, fma_op, a, b, c, c_proved)) = matched
                        && get_fp(c).finite
                        && c1_compatible(prod, fp_op, lhs, rhs, true)
                    {
                        let mut proved = SINGLE_A | SINGLE_B;
                        if c_proved {
                            proved |= SINGLE_C;
                        }
                        if add_flags & SINGLE_RESULT != 0 {
                            proved |= SINGLE_RESULT;
                        }
                        if add_flags & SINGLE_EXACT != 0 && c_proved {
                            proved |= SINGLE_RESULT | SINGLE_EXACT;
                        }
                        block.insts[idx].1 = Op::Fma {
                            op: fma_op,
                            a,
                            b,
                            c,
                            single: true,
                            proved,
                        };
                        removed.insert(mul_idx);
                        c1_rewrites.push((prod, fma_op, a, b, c, true));
                        contracted = true;
                    }
                }
                _ => {}
            }
        }

        if !removed.is_empty() {
            let mut kept = Vec::with_capacity(block.insts.len() - removed.len());
            for (i, inst) in block.insts.drain(..).enumerate() {
                if !removed.contains(&i) {
                    kept.push(inst);
                }
            }
            block.insts = kept;
        }
    }
    if !c1_rewrites.is_empty() {
        for &(rec_idx, _, ra, rb, _) in &c1_records {
            if let Some(&(_, fma_op, a, b, c, single)) = c1_rewrites
                .iter()
                .find(|&&(prod, ..)| prod == ra || prod == rb)
                && let Some(x87) = &mut func.records[rec_idx].x87
            {
                x87.c1 = Some(Location::Recipe(Recipe::C1Fma {
                    op: fma_op,
                    a,
                    b,
                    c,
                    single,
                }));
            }
        }
    }
    contracted
}
