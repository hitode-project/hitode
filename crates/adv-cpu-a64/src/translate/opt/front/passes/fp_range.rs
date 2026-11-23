//! Forward interval, finiteness, and dyadic-scale analysis for x87 operations,
//! plus two-step binary32 frame store-reload elimination.
//!
//! Soundness invariants:
//! - `finite` and `binary32_exact` are tracked separately (`Op::X87Load { single: true }`
//!   admits `±Inf`, and subnormals when `denormal == false`).
//! - Proving `Op::Fp` (binary64) does not alter lazy `Recipe::C1` recipes.
//! - Folding `X87Load(X87Store(v)) -> v` and `X87StoreC1 -> 0` requires `v` to be
//!   proved finite, exact in binary32, and normal or signed zero (preserving `-0.0`).

use super::{Func, IntOp, IntUnary, Op, Type, Value, apply_aliases};
use crate::translate::opt::ir::{FpOp, FpUnary, SINGLE_A, SINGLE_B, SINGLE_EXACT, SINGLE_RESULT};

const ZERO_Q: i16 = i16::MAX;
const F32_MAX_F64: f64 = f32::MAX as f64;
const F32_MIN_NORMAL_F64: f64 = f32::MIN_POSITIVE as f64;
/// `2^-125`, the lowest value strictly above the binary32 minimum normal binade.
const F32_SAFE_MIN_F64: f64 = 2.350_988_701_644_575e-38; // f32::from_bits(0x0100_0000) as f64
const WIDEN_LIMIT: u8 = 3;

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
            dyadic_q: Some(ZERO_Q),
        }
    }

    fn is_zero(self) -> bool {
        self.finite && self.lo == 0.0 && self.hi == 0.0
    }

    fn max_abs(self) -> f64 {
        self.lo.abs().max(self.hi.abs())
    }

    /// Lower bound on `|x|` for all nonzero values in `self`, or `0.0` if unknown.
    fn min_nonzero_abs(self) -> f64 {
        if !self.finite {
            return 0.0;
        }
        let mut bound = 0.0f64;
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
        let (lo, hi, dyadic_q) = if finite {
            (
                self.lo.min(other.lo),
                self.hi.max(other.hi),
                dyadic_min(self.dyadic_q, other.dyadic_q),
            )
        } else {
            (f64::NEG_INFINITY, f64::INFINITY, None)
        };
        Self {
            finite,
            binary32_exact: self.binary32_exact && other.binary32_exact,
            f32_normal_or_zero: self.f32_normal_or_zero && other.f32_normal_or_zero,
            f32_safe_nonzero: self.f32_safe_nonzero && other.f32_safe_nonzero,
            lo,
            hi,
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

    let exact_f64 = q_cand
        .is_some_and(|q| q >= -1022 && fits_mantissa_bits(lo.abs().max(hi.abs()), q, 53))
        || (op == FpOp::Mul && both_f32);

    if !exact_f64 {
        lo = lo.next_down();
        hi = hi.next_up();
        if !lo.is_finite() || !hi.is_finite() {
            return (false, FpFact::TOP);
        }
    }

    let max_abs = lo.abs().max(hi.abs());
    let proved = match op {
        FpOp::Add | FpOp::Sub => {
            both_f32
                || q_cand.is_some_and(|q| q >= -1022)
                || lo >= f64::MIN_POSITIVE
                || hi <= -f64::MIN_POSITIVE
                || (lo == 0.0 && hi == 0.0)
        }
        FpOp::Mul => {
            both_f32
                || a.is_zero()
                || b.is_zero()
                || q_cand.is_some_and(|q| q >= -1021)
                || a.min_nonzero_abs() * b.min_nonzero_abs() > f64::MIN_POSITIVE
        }
        FpOp::Div => {
            a.is_zero()
                || (exact_f64 && q_cand.is_some_and(|q| q >= -1021))
                || a.min_nonzero_abs() / b.max_abs() > f64::MIN_POSITIVE
        }
    };

    let dyadic_q = if exact_f64 { q_cand } else { None };
    let binary32_exact = exact_f64
        && max_abs <= F32_MAX_F64
        && dyadic_q.is_some_and(|q| q >= -126 && fits_mantissa_bits(max_abs, q, 24));
    let f32_normal_or_zero = dyadic_q.is_some_and(|q| q >= -126)
        || lo >= F32_MIN_NORMAL_F64
        || hi <= -F32_MIN_NORMAL_F64
        || (lo == 0.0 && hi == 0.0);
    let f32_safe_nonzero = dyadic_q.is_some_and(|q| q >= -125)
        || lo >= F32_SAFE_MIN_F64
        || hi <= -F32_SAFE_MIN_F64
        || (lo == 0.0 && hi == 0.0);

    let fact = FpFact {
        finite: true,
        binary32_exact,
        f32_normal_or_zero,
        f32_safe_nonzero,
        lo,
        hi,
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

    // Even when unproved, a succeeding Op::X87Single always produces a finite,
    // exact, normal-or-zero binary32 value.
    let base_fact = FpFact {
        finite: true,
        binary32_exact: true,
        f32_normal_or_zero: true,
        f32_safe_nonzero: false,
        lo: -F32_MAX_F64,
        hi: F32_MAX_F64,
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
    let exact_f32 = raw_max <= F32_MAX_F64
        && q_cand.is_some_and(|q| q >= -125 && fits_mantissa_bits(raw_max, q, 24));

    if exact_f32 {
        flags |= SINGLE_RESULT | SINGLE_EXACT;
        let fact = FpFact {
            finite: true,
            binary32_exact: true,
            f32_normal_or_zero: true,
            f32_safe_nonzero: true,
            lo: raw_lo,
            hi: raw_hi,
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

    let result_safe = match op {
        FpOp::Add | FpOp::Sub => {
            q_cand.is_some_and(|q| q >= -125)
                || lo >= F32_SAFE_MIN_F64
                || hi <= -F32_SAFE_MIN_F64
                || (lo == 0.0 && hi == 0.0)
        }
        FpOp::Mul => {
            a.is_zero()
                || b.is_zero()
                || a.min_nonzero_abs() * b.min_nonzero_abs() >= F32_SAFE_MIN_F64
        }
        FpOp::Div => a.is_zero() || a.min_nonzero_abs() / b.max_abs() >= F32_SAFE_MIN_F64,
    };

    if result_safe {
        flags |= SINGLE_RESULT;
    }

    let f32_safe_nonzero = result_safe
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

    let n = func.types.len();
    let defs = super::definitions(func);
    let mut int_facts: Vec<Option<IntRange>> = vec![None; n];
    let mut fp_facts: Vec<Option<FpFact>> = vec![None; n];
    let mut param_updates = vec![0u8; n];

    for &p in &func.blocks[0].params {
        match func.ty(p) {
            Type::I32 | Type::I64 => int_facts[p.0 as usize] = Some(IntRange::top(func.ty(p))),
            Type::F64 => fp_facts[p.0 as usize] = Some(FpFact::TOP),
            _ => {}
        }
    }

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
                        let next = eval_fp_op(op, func, &defs, &int_facts, &fp_facts);
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
                            let next = match fp_facts[param.0 as usize] {
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

    // Apply proved flags and Step 1.2B store-reload / C1 folds.
    let mut changed = false;
    let mut alias: Vec<Value> = (0..n).map(|i| Value(i as u32)).collect();
    let mut aliased = false;

    for block in &mut func.blocks {
        for (result, op) in &mut block.insts {
            match *op {
                Op::Fp(fp_op, a, b, false) => {
                    let fa = fp_facts[a.0 as usize].unwrap_or(FpFact::TOP);
                    let fb = fp_facts[b.0 as usize].unwrap_or(FpFact::TOP);
                    let (proved, _) = eval_fp(fp_op, fa, fb, a == b);
                    if proved {
                        *op = Op::Fp(fp_op, a, b, true);
                        changed = true;
                    }
                }
                Op::X87Single(fp_op, a, b, cur) => {
                    let fa = fp_facts[a.0 as usize].unwrap_or(FpFact::TOP);
                    let fb = fp_facts[b.0 as usize].unwrap_or(FpFact::TOP);
                    let (proved, _) = eval_x87_single(fp_op, fa, fb, a == b);
                    let merged = cur | proved;
                    if merged != cur {
                        *op = Op::X87Single(fp_op, a, b, merged);
                        changed = true;
                    }
                }
                Op::X87Load {
                    bits,
                    denormal,
                    single,
                    proved,
                } => {
                    if let Some(const_bits) = const_x87_load(&defs, bits, denormal, single) {
                        *op = Op::Constant(Type::F64, u128::from(const_bits));
                        changed = true;
                        continue;
                    }
                    if single {
                        let src_bits = peel_i32_bits(&defs, bits);
                        if let Some(Op::X87Store {
                            value,
                            width: 4,
                            integer: false,
                            ..
                        }) = defs[src_bits.0 as usize]
                        {
                            let fv = fp_facts[value.0 as usize].unwrap_or(FpFact::TOP);
                            if fv.finite && fv.binary32_exact && fv.f32_normal_or_zero {
                                // Step 1.2B: exact normal-or-zero binary32 round-trip
                                // preserves all bits including -0.0.
                                if let Some(r) = *result {
                                    alias[r.0 as usize] = value;
                                    aliased = true;
                                    changed = true;
                                }
                            } else if !proved {
                                // Step 1.2A: X87Store already validated finite normal-or-zero f32.
                                *op = Op::X87Load {
                                    bits,
                                    denormal,
                                    single: true,
                                    proved: true,
                                };
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
                    let fv = fp_facts[value.0 as usize].unwrap_or(FpFact::TOP);
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
                        *op = Op::X87Store {
                            value,
                            width,
                            rounding,
                            integer,
                            proved: true,
                        };
                        changed = true;
                    }
                }
                Op::X87StoreC1 { value, integer, .. } => {
                    let fv = fp_facts[value.0 as usize].unwrap_or(FpFact::TOP);
                    let exact = if integer {
                        fv.finite && fv.dyadic_q.is_some_and(|q| q >= 0)
                    } else {
                        fv.finite && fv.binary32_exact && fv.f32_normal_or_zero
                    };
                    if exact {
                        *op = Op::Constant(Type::I32, 0);
                        changed = true;
                    }
                }
                _ => {}
            }
        }
    }

    if aliased {
        for block in &mut func.blocks {
            block
                .insts
                .retain(|(r, _)| r.is_none_or(|r| alias[r.0 as usize] == r));
        }
        apply_aliases(func, &alias);
    }

    changed
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
                dyadic_q: Some(q),
            }
        }
        Op::FpUnary(unary, a) => {
            let a = get_fp(a);
            match unary {
                FpUnary::Neg => FpFact {
                    lo: if a.finite { -a.hi } else { f64::NEG_INFINITY },
                    hi: if a.finite { -a.lo } else { f64::INFINITY },
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
                    FpFact { lo, hi, ..a }
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
                    return FpFact {
                        finite: true,
                        binary32_exact: true,
                        f32_normal_or_zero: true,
                        f32_safe_nonzero: !store_proved || fv.f32_safe_nonzero,
                        lo,
                        hi,
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
                dyadic_q: None,
            }
        }
        Op::Select { a, b, .. } => get_fp(a).join(get_fp(b)),
        _ => FpFact::TOP,
    }
}
