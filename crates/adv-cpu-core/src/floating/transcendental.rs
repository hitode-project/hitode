//! Native binary64 x87 transcendental algorithms (A64F-FP-10) and bit-exact
//! FSCALE (A64F-FP-8).
//!
//! Finite in-range results for FSIN, FCOS, FSINCOS, FPTAN, FPATAN, F2XM1,
//! FYL2X and FYL2XP1 use native binary64 arithmetic without FMA contraction
//! and stay within 2 ULP of binary64 while preserving exact x87 special values,
//! range-reduction C2 behavior, rounding/overflow/underflow flags, and C1.
use super::softfloat::{self as sf, Quad as Q};
use crate::X87Register as Ext;
use crate::backend::x87::{decode_f64, encode_f64};

const INDEFINITE: Ext = Ext([0, 0, 0, 0, 0, 0, 0, 0xc0, 0xff, 0xff]);
const ONE: Ext = Ext([0, 0, 0, 0, 0, 0, 0, 0x80, 0xff, 0x3f]);
const NEG_HALF: Ext = Ext([0, 0, 0, 0, 0, 0, 0, 0x80, 0xfe, 0xbf]);
const IE: u16 = 0x01;
const ZE: u16 = 0x04;
const OE: u16 = 0x08;
const UE: u16 = 0x10;
const PE: u16 = 0x20;
const SCALE_2_M63: f64 = f64::from_bits((1023 - 63) << 52); // 2^-63 exact
const SCALE_2_M64: f64 = f64::from_bits((1023 - 64) << 52); // 2^-64 exact
// High and low parts of ln(2) whose sum is accurate to ~106 bits.
const LN2_HI: f64 = std::f64::consts::LN_2;
const LN2_LO: f64 = f64::from_bits(0x3c7a_bc9e_3b39_803f);

fn infinity(negative: bool) -> Ext {
    Ext::from_parts(1 << 63, 0x7fff | if negative { 0x8000 } else { 0 })
}
fn signed_zero(negative: bool) -> Ext {
    Ext::from_parts(0, if negative { 0x8000 } else { 0 })
}
fn nan(v: Ext) -> bool {
    v.sign_exponent() & 0x7fff == 0x7fff && v.significand() & !(1 << 63) != 0
}
fn infinite(v: Ext) -> bool {
    v.sign_exponent() & 0x7fff == 0x7fff && v.significand() == 1 << 63
}
fn zero(v: Ext) -> bool {
    v.significand() == 0
}
fn negative(v: Ext) -> bool {
    v.sign_exponent() & 0x8000 != 0
}
fn start() {
    sf::configure(0x37f);
}
/// Discard internal approximation flags, then perform the architectural final
/// conversion in RC at full extended precision. PC does not limit transcendentals.
fn finish(value: Q, control: u16, inherent: u16) -> (Ext, u16, bool) {
    sf::configure(control | 0x300);
    let out = value.to_ext();
    (out, sf::flags() | inherent, sf::rounded_up())
}
fn propagate(a: Ext, b: Ext, control: u16) -> (Ext, u16, bool) {
    sf::configure(control | 0x300);
    let result = sf::binary(a, b, 0);
    (result, sf::flags(), false)
}

/// Decompose a finite non-zero extended80 value into `(negative, unbiased_exp, norm_sig)`
/// where `norm_sig >= 2^63` and `value = (-1)^negative * (norm_sig / 2^63) * 2^unbiased_exp`.
fn decompose_finite(v: Ext) -> (bool, i32, u64) {
    let sign = negative(v);
    let raw_exp = (v.sign_exponent() & 0x7fff) as i32;
    let sig = v.significand();
    debug_assert!(sig != 0 && raw_exp != 0x7fff);
    if raw_exp == 0 {
        let shift = sig.leading_zeros();
        (sign, 1 - 0x3fff - shift as i32, sig << shift)
    } else {
        (sign, raw_exp - 0x3fff, sig)
    }
}

/// Split a normal finite extended80 value with `-959 <= unbiased_exp <= 1023`
/// into an exact `f64` sum `hi + lo` with `|lo| <= 2^-53 * |hi|`.
fn ext_to_f64_pair(v: Ext) -> (f64, f64) {
    let hi = decode_f64(&v.0);
    let sig = v.significand();
    if sig & 0x7ff == 0 {
        return (hi, 0.0);
    }
    let hi_ext = Ext(encode_f64(hi));
    let exp = (v.sign_exponent() & 0x7fff) as i32 - 0x3fff;
    let hi_exp = (hi_ext.sign_exponent() & 0x7fff) as i32 - 0x3fff;
    let diff = if hi_exp == exp {
        (sig as i128 - hi_ext.significand() as i128) as i64
    } else {
        // Rounding to binary64 carried into the next binade (`hi_sig == 1 << 63`).
        debug_assert_eq!(hi_exp, exp + 1);
        (sig as i128 - (1i128 << 64)) as i64
    };
    let scale = f64::from_bits(((exp - 63 + 1023) as u64) << 52);
    let lo = (diff as f64) * scale;
    (hi, if negative(v) { -lo } else { lo })
}

/// Convert `val * 2^exp_shift` (for finite `val`) to extended80, handling
/// overflow, underflow, rounding direction `RC`, and `C1`.
fn f64_scaled_to_ext(val: f64, exp_shift: i32, control: u16, inherent: u16) -> (Ext, u16, bool) {
    let bits = val.to_bits();
    let sign = ((bits >> 48) & 0x8000) as u16;
    let neg = sign != 0;
    let raw_e = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    if raw_e == 0 && frac == 0 {
        return (Ext::from_parts(0, sign), inherent, false);
    }
    let (sig, unbiased) = if raw_e == 0 {
        let shift = frac.leading_zeros();
        (frac << shift, -1011 - shift as i32)
    } else {
        ((1u64 << 63) | (frac << 11), raw_e - 1023)
    };
    let total_exp = unbiased + exp_shift;
    let rc = (control >> 10) & 3;
    if total_exp > 16383 {
        let flags = inherent | OE | PE;
        let to_inf = match rc {
            0 => true,
            1 => neg,
            2 => !neg,
            _ => false,
        };
        if to_inf {
            return (infinity(neg), flags, true);
        }
        return (Ext::from_parts(u64::MAX, sign | 0x7ffe), flags, false);
    }
    if total_exp < -16382 {
        let flags = inherent | UE | PE;
        let rshift = (-16382 - total_exp) as u32;
        let (quot, rem_gt_half, rem_eq_half, rem_nonzero) = if rshift >= 65 {
            (0u64, false, false, true)
        } else if rshift == 64 {
            (0u64, sig > (1u64 << 63), sig == (1u64 << 63), true)
        } else {
            let quot = sig >> rshift;
            let rem = sig & ((1u64 << rshift) - 1);
            let half = 1u64 << (rshift - 1);
            (quot, rem > half, rem == half, rem != 0)
        };
        let inc = rem_nonzero
            && match rc {
                0 => rem_gt_half || (rem_eq_half && quot & 1 != 0),
                1 => neg,
                2 => !neg,
                _ => false,
            };
        let out_sig = quot + u64::from(inc);
        if out_sig == (1u64 << 63) {
            return (Ext::from_parts(1u64 << 63, sign | 1), flags, true);
        }
        return (Ext::from_parts(out_sig, sign), flags, inc);
    }
    let biased = (total_exp + 0x3fff) as u16;
    let rounded_up = match rc {
        1 => neg,
        2 => !neg,
        _ => false,
    };
    (Ext::from_parts(sig, sign | biased), inherent, rounded_up)
}

/// Exact 80-bit rounding of `x +/- tiny_eps` where `0 < tiny_eps < 2^-65 * |x|`.
/// Used for `sin(x)` (`toward_zero = true`) and `tan(x)` (`toward_zero = false`)
/// when `0 < |x| < 2^-33`.
fn round_tiny_trig(x: Ext, toward_zero: bool, control: u16) -> (Ext, u16, bool) {
    let sign = x.sign_exponent() & 0x8000;
    let neg = sign != 0;
    let raw_exp = x.sign_exponent() & 0x7fff;
    let sig = x.significand();
    let rc = (control >> 10) & 3;
    let base_flags = PE | if raw_exp == 0 { UE } else { 0 };
    if toward_zero {
        // True value has magnitude strictly in (|x| - eps, |x|).
        let step_down = match rc {
            0 => false,
            1 => !neg,
            2 => neg,
            _ => true,
        };
        if !step_down {
            return (x, base_flags, true);
        }
        if raw_exp == 0 {
            return (Ext::from_parts(sig - 1, sign), UE | PE, false);
        }
        if sig == (1u64 << 63) {
            if raw_exp == 1 {
                return (Ext::from_parts((1u64 << 63) - 1, sign), UE | PE, false);
            }
            return (Ext::from_parts(u64::MAX, sign | (raw_exp - 1)), PE, false);
        }
        return (Ext::from_parts(sig - 1, sign | raw_exp), PE, false);
    }
    // True value has magnitude strictly in (|x|, |x| + eps).
    let step_up = match rc {
        0 => false,
        1 => neg,
        2 => !neg,
        _ => false,
    };
    if !step_up {
        return (x, base_flags, false);
    }
    if raw_exp == 0 {
        if sig == (1u64 << 63) - 1 {
            return (Ext::from_parts(1u64 << 63, sign | 1), UE | PE, true);
        }
        return (Ext::from_parts(sig + 1, sign), UE | PE, true);
    }
    if sig == u64::MAX {
        return (Ext::from_parts(1u64 << 63, sign | (raw_exp + 1)), PE, true);
    }
    (Ext::from_parts(sig + 1, sign | raw_exp), PE, true)
}

/// Exact 80-bit rounding of `1.0 - tiny_eps` (`0 < tiny_eps < 2^-67`) for `cos(x)`
/// when `0 < |x| < 2^-33`.
fn round_near_one(control: u16) -> (Ext, u16, bool) {
    let rc = (control >> 10) & 3;
    if rc == 1 || rc == 3 {
        (Ext::from_parts(u64::MAX, 0x3ffe), PE, false)
    } else {
        (ONE, PE, true)
    }
}

/// Exact 80-bit rounded multiple of pi (`pi`, `pi/2`, `pi/4`, or `3*pi/4`).
fn rounded_pi_multiple(sig_rn: u64, exp: u16, neg: bool, control: u16) -> (Ext, u16, bool) {
    // All four constants (pi, pi/2, pi/4, 3*pi/4) round UP in magnitude under RN.
    let rc = (control >> 10) & 3;
    let round_down_mag = match rc {
        0 => false,
        1 => !neg,
        2 => neg,
        _ => true,
    };
    let sign = if neg { 0x8000 } else { 0 };
    if round_down_mag {
        (Ext::from_parts(sig_rn - 1, sign | exp), PE, false)
    } else {
        (Ext::from_parts(sig_rn, sign | exp), PE, true)
    }
}

#[cold]
fn log_product_quad(x: Ext, y: Ext, control: u16) -> (Ext, u16, bool) {
    start();
    let target = Q::from_ext(x).add(Q::ONE);
    if target.abs() == Q::ZERO {
        return (infinity(!negative(y)), ZE, false);
    }
    let mut v = target;
    let mut correction = 0;
    if v.0[1] & 0x7fff_0000_0000_0000 == 0 {
        v = v.mul(Q([0, 0x406f_0000_0000_0000]));
        correction = -112;
    }
    let exponent = ((v.0[1] >> 48) & 0x7fff) as i32 - 0x3fff + correction;
    v.0[1] = (v.0[1] & 0x0000_ffff_ffff_ffff) | 0x3fff_0000_0000_0000;
    let t = v.sub(Q::ONE).div(v.add(Q::ONE));
    let square = t.mul(t);
    let mut term = t;
    let mut sum = t;
    for denominator in (3..=97).step_by(2) {
        term = term.mul(square);
        let next = sum.add(term.div(Q::integer(denominator)));
        if next == sum {
            break;
        }
        sum = next;
    }
    let log = sum.mul(Q::integer(2)).div(Q::LN2).add(Q::integer(exponent));
    finish(Q::from_ext(y).mul(log), control, PE)
}

pub(super) fn log_product(x: Ext, y: Ext, plus_one: bool, control: u16) -> (Ext, u16, bool) {
    if nan(x) || nan(y) {
        return propagate(x, y, control);
    }
    let sx = negative(x);
    let sy = negative(y);
    if !plus_one {
        // FYL2X: y * log2(x)
        if zero(x) {
            if zero(y) {
                return (INDEFINITE, IE, false);
            }
            return (infinity(!sy), ZE, false);
        }
        if sx {
            return (INDEFINITE, IE, false);
        }
        if infinite(x) {
            if zero(y) {
                return (INDEFINITE, IE, false);
            }
            return (infinity(sy), 0, false);
        }
        if x == ONE {
            if infinite(y) {
                return (INDEFINITE, IE, false);
            }
            return (signed_zero(sy), 0, false);
        }
        let (_, ex, mx) = decompose_finite(x);
        let log_neg = ex < 0;
        if zero(y) {
            return (signed_zero(sy ^ log_neg), 0, false);
        }
        if infinite(y) {
            return (infinity(sy ^ log_neg), 0, false);
        }
        let log_val = if mx == (1u64 << 63) {
            ex as f64
        } else if ex == 0 {
            let delta = ((mx - (1u64 << 63)) as f64) * SCALE_2_M63;
            delta.ln_1p() * std::f64::consts::LOG2_E
        } else if ex == -1 && mx > 0xc000_0000_0000_0000 {
            let delta = -((mx.wrapping_neg()) as f64) * SCALE_2_M64;
            delta.ln_1p() * std::f64::consts::LOG2_E
        } else {
            let m = (mx as f64) * SCALE_2_M63;
            (ex as f64) + m.log2()
        };
        let (_, ey, my) = decompose_finite(y);
        let ym = (my as f64) * SCALE_2_M63;
        let prod = if sy { -ym } else { ym } * log_val;
        return f64_scaled_to_ext(prod, ey, control, PE);
    }

    // FYL2XP1: y * log2(x + 1)
    if zero(x) {
        if infinite(y) {
            return (INDEFINITE, IE, false);
        }
        return (signed_zero(sy ^ sx), 0, false);
    }
    if infinite(x) {
        if sx {
            return (INDEFINITE, IE, false);
        }
        if zero(y) {
            return (INDEFINITE, IE, false);
        }
        return (infinity(sy), 0, false);
    }
    let (_, ex, mx) = decompose_finite(x);
    if sx {
        if ex > 0 || (ex == 0 && mx > (1u64 << 63)) {
            return (INDEFINITE, IE, false);
        }
        if ex == 0 && mx == (1u64 << 63) {
            // x == -1.0 -> log2(0) = -Inf
            if zero(y) {
                return (INDEFINITE, IE, false);
            }
            return (infinity(!sy), ZE, false);
        }
    }
    if zero(y) {
        return (signed_zero(sy ^ sx), 0, false);
    }
    if infinite(y) {
        return (infinity(sy ^ sx), 0, false);
    }
    let (_, ey, my) = decompose_finite(y);
    let ym = {
        let mag = (my as f64) * SCALE_2_M63;
        if sy { -mag } else { mag }
    };
    if ex < -54 {
        let xm = {
            let mag = (mx as f64) * SCALE_2_M63;
            if sx { -mag } else { mag }
        };
        let prod = ym * (xm * std::f64::consts::LOG2_E);
        return f64_scaled_to_ext(prod, ey + ex, control, PE);
    }
    let log_val = if ex > 54 {
        let m = (mx as f64) * SCALE_2_M63;
        (ex as f64) + m.log2()
    } else if sx && ex == -1 && mx > 0xc000_0000_0000_0000 {
        if mx > 0xffff_ffff_f800_0000 {
            return log_product_quad(x, y, control);
        }
        let one_plus_x = (mx.wrapping_neg() as f64) * SCALE_2_M64;
        one_plus_x.log2()
    } else {
        let (hi, lo) = ext_to_f64_pair(x);
        (hi.ln_1p() + lo / (1.0 + hi)) * std::f64::consts::LOG2_E
    };
    f64_scaled_to_ext(ym * log_val, ey, control, PE)
}

pub(super) fn exp2_minus_one(x: Ext, control: u16) -> Option<(Ext, u16, bool)> {
    if nan(x) {
        return Some(propagate(x, x, control));
    }
    let raw_exp = x.sign_exponent() & 0x7fff;
    let sig = x.significand();
    // Architectural result is undefined outside [-1, 1] (including ±Inf).
    if raw_exp > 0x3fff || (raw_exp == 0x3fff && sig > (1u64 << 63)) {
        return None;
    }
    if zero(x) {
        return Some((x, 0, false));
    }
    if raw_exp == 0x3fff && sig == (1u64 << 63) {
        return Some((if negative(x) { NEG_HALF } else { ONE }, PE, false));
    }
    let (sx, ex, mx) = decompose_finite(x);
    if ex < -54 {
        let xm = {
            let mag = (mx as f64) * SCALE_2_M63;
            if sx { -mag } else { mag }
        };
        return Some(f64_scaled_to_ext(xm * LN2_HI, ex, control, PE));
    }
    let (hi, lo) = ext_to_f64_pair(x);
    let t = hi * LN2_HI;
    let dt = lo * LN2_HI + hi * LN2_LO;
    let e = t.exp_m1();
    let result = e + dt * (e + 1.0);
    Some(f64_scaled_to_ext(result, 0, control, PE))
}

/// Bit-exact FSCALE (A64F-FP-8).
pub(super) fn scale(a: Ext, b: Ext, control: u16) -> (Ext, u16) {
    if nan(a) || nan(b) {
        let (r, f, _) = propagate(a, b, control);
        return (r, f);
    }
    if infinite(b) {
        if zero(a) && !negative(b) || infinite(a) && negative(b) {
            return (INDEFINITE, IE);
        }
        return if negative(b) {
            (signed_zero(negative(a)), 0)
        } else {
            (infinity(negative(a)), 0)
        };
    }
    start();
    let count = Q::from_ext(b);
    let limit = Q::integer(32768);
    let count = if limit.lt(count) {
        32768
    } else if count.lt(limit.neg()) {
        -32768
    } else {
        count.to_i32(1)
    };
    sf::configure(control | 0x300);
    let result = sf::scale(a, count);
    (result, sf::flags())
}

// High-precision fallback for the rare case where an extended80 trig argument
// has non-zero low 11 significand bits AND either |x| > 2^20 or lands near a
// zero of sin/cos where first-order correction in `lo` would cancel.
fn sin_cos_series(x: Ext) -> (Q, Q) {
    let a = Q::from_ext(x);
    let half_pi = Q::PI.div(Q::integer(2));
    let quotient = a.div(half_pi).round(0);
    let r = a.sub(quotient.mul(half_pi));
    let quadrant = quotient.to_i64(1) & 3;
    let minus_square = r.mul(r).neg();
    let mut sine = r;
    let mut sin_term = r;
    let mut cosine = Q::ONE;
    let mut cos_term = Q::ONE;
    for n in 1..=32 {
        sin_term = sin_term
            .mul(minus_square)
            .div(Q::integer((2 * n) * (2 * n + 1)));
        cos_term = cos_term
            .mul(minus_square)
            .div(Q::integer((2 * n - 1) * (2 * n)));
        sine = sine.add(sin_term);
        cosine = cosine.add(cos_term);
    }
    match quadrant {
        0 => (sine, cosine),
        1 => (cosine, sine.neg()),
        2 => (sine.neg(), cosine.neg()),
        _ => (cosine.neg(), sine),
    }
}

fn sin_cos_fallback(x: Ext, control: u16) -> ((Ext, Ext), u16, (bool, bool)) {
    start();
    let (sine, cosine) = sin_cos_series(x);
    let (s, sf, su) = finish(sine, control, PE);
    let (c, cf, cu) = finish(cosine, control, PE);
    ((s, c), sf | cf, (su, cu))
}

fn check_trig_special(x: Ext, control: u16) -> Result<Option<(Ext, u16, bool)>, ()> {
    if nan(x) {
        return Ok(Some(propagate(x, x, control)));
    }
    if infinite(x) {
        return Ok(Some((INDEFINITE, IE, false)));
    }
    if x.sign_exponent() & 0x7fff >= 0x403e {
        return Err(());
    }
    Ok(None)
}

pub(super) fn sin(x: Ext, control: u16) -> Option<(Ext, u16, bool)> {
    match check_trig_special(x, control) {
        Ok(Some(special)) => return Some(special),
        Err(()) => return None,
        Ok(None) => {}
    }
    if zero(x) {
        return Some((x, 0, false));
    }
    let (_, ex, _) = decompose_finite(x);
    if ex < -33 {
        return Some(round_tiny_trig(x, true, control));
    }
    let (hi, lo) = ext_to_f64_pair(x);
    if lo == 0.0 {
        return Some(f64_scaled_to_ext(hi.sin(), 0, control, PE));
    }
    if ex <= 20 {
        let (s0, c0) = hi.sin_cos();
        if s0.abs() >= 64.0 * lo.abs() {
            return Some(f64_scaled_to_ext(s0 + lo * c0, 0, control, PE));
        }
    }
    let ((s, _), flags, (su, _)) = sin_cos_fallback(x, control);
    Some((s, flags, su))
}

pub(super) fn cos(x: Ext, control: u16) -> Option<(Ext, u16, bool)> {
    match check_trig_special(x, control) {
        Ok(Some(special)) => return Some(special),
        Err(()) => return None,
        Ok(None) => {}
    }
    if zero(x) {
        return Some((ONE, 0, false));
    }
    let (_, ex, _) = decompose_finite(x);
    if ex < -33 {
        return Some(round_near_one(control));
    }
    let (hi, lo) = ext_to_f64_pair(x);
    if lo == 0.0 {
        return Some(f64_scaled_to_ext(hi.cos(), 0, control, PE));
    }
    if ex <= 20 {
        let (s0, c0) = hi.sin_cos();
        if c0.abs() >= 64.0 * lo.abs() {
            return Some(f64_scaled_to_ext(c0 - lo * s0, 0, control, PE));
        }
    }
    let ((_, c), flags, (_, cu)) = sin_cos_fallback(x, control);
    Some((c, flags, cu))
}

pub(super) fn sin_cos(x: Ext, control: u16) -> Option<((Ext, Ext), u16, bool)> {
    match check_trig_special(x, control) {
        Ok(Some((v, flags, up))) => return Some(((v, v), flags, up)),
        Err(()) => return None,
        Ok(None) => {}
    }
    if zero(x) {
        return Some(((x, ONE), 0, false));
    }
    let (_, ex, _) = decompose_finite(x);
    if ex < -33 {
        let (s, sf, _) = round_tiny_trig(x, true, control);
        let (c, cf, cu) = round_near_one(control);
        return Some(((s, c), sf | cf, cu));
    }
    let (hi, lo) = ext_to_f64_pair(x);
    if lo == 0.0 {
        let (s0, c0) = hi.sin_cos();
        let (s, sf, _) = f64_scaled_to_ext(s0, 0, control, PE);
        let (c, cf, cu) = f64_scaled_to_ext(c0, 0, control, PE);
        return Some(((s, c), sf | cf, cu));
    }
    if ex <= 20 {
        let (s0, c0) = hi.sin_cos();
        let thresh = 64.0 * lo.abs();
        if s0.abs() >= thresh && c0.abs() >= thresh {
            let (s, sf, _) = f64_scaled_to_ext(s0 + lo * c0, 0, control, PE);
            let (c, cf, cu) = f64_scaled_to_ext(c0 - lo * s0, 0, control, PE);
            return Some(((s, c), sf | cf, cu));
        }
    }
    let ((s, c), flags, (_, cu)) = sin_cos_fallback(x, control);
    Some(((s, c), flags, cu))
}

pub(super) fn tan(x: Ext, control: u16) -> Option<(Ext, u16, bool)> {
    match check_trig_special(x, control) {
        Ok(Some(special)) => return Some(special),
        Err(()) => return None,
        Ok(None) => {}
    }
    if zero(x) {
        return Some((x, 0, false));
    }
    let (_, ex, _) = decompose_finite(x);
    if ex < -33 {
        return Some(round_tiny_trig(x, false, control));
    }
    let (hi, lo) = ext_to_f64_pair(x);
    if lo == 0.0 {
        return Some(f64_scaled_to_ext(hi.tan(), 0, control, PE));
    }
    if ex <= 20 {
        let t0 = hi.tan();
        if (t0 * lo).abs() < 1.0 / 64.0 && t0.abs() >= 64.0 * lo.abs() {
            let result = (t0 + lo) / (1.0 - t0 * lo);
            return Some(f64_scaled_to_ext(result, 0, control, PE));
        }
    }
    start();
    let (sine, cosine) = sin_cos_series(x);
    Some(finish(sine.div(cosine), control, PE))
}

pub(super) fn atan2(y: Ext, x: Ext, control: u16) -> (Ext, u16, bool) {
    if nan(x) || nan(y) {
        return propagate(y, x, control);
    }
    let sx = negative(x);
    let sy = negative(y);
    const PI_SIG: u64 = 0xc90f_daa2_2168_c235;
    const THREE_PI_4_SIG: u64 = 0x96cb_e3f9_990e_91a8;
    if zero(y) {
        if !sx {
            return (signed_zero(sy), 0, false);
        }
        return rounded_pi_multiple(PI_SIG, 0x4000, sy, control);
    }
    if zero(x) {
        return rounded_pi_multiple(PI_SIG, 0x3fff, sy, control);
    }
    if infinite(x) {
        if infinite(y) {
            return if sx {
                rounded_pi_multiple(THREE_PI_4_SIG, 0x4000, sy, control)
            } else {
                rounded_pi_multiple(PI_SIG, 0x3ffe, sy, control)
            };
        }
        return if sx {
            rounded_pi_multiple(PI_SIG, 0x4000, sy, control)
        } else {
            (signed_zero(sy), PE, false)
        };
    }
    if infinite(y) {
        return rounded_pi_multiple(PI_SIG, 0x3fff, sy, control);
    }
    let (_, ex, mx) = decompose_finite(x);
    let (_, ey, my) = decompose_finite(y);
    let diff = ey - ex;
    if diff > 65 {
        return rounded_pi_multiple(PI_SIG, 0x3fff, sy, control);
    }
    if diff < -65 {
        if sx {
            return rounded_pi_multiple(PI_SIG, 0x4000, sy, control);
        }
        let ratio = ((my as f64) * SCALE_2_M63) / ((mx as f64) * SCALE_2_M63);
        return f64_scaled_to_ext(if sy { -ratio } else { ratio }, diff, control, PE);
    }
    let x_scaled = {
        let mag = (mx as f64) * SCALE_2_M63;
        if sx { -mag } else { mag }
    };
    let y_scaled = {
        let mag = ((my as f64) * SCALE_2_M63) * f64::from_bits(((diff + 1023) as u64) << 52);
        if sy { -mag } else { mag }
    };
    f64_scaled_to_ext(y_scaled.atan2(x_scaled), 0, control, PE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ref_logarithm_series(t: Q) -> Q {
        let square = t.mul(t);
        let mut term = t;
        let mut sum = t;
        for denominator in (3..=97).step_by(2) {
            term = term.mul(square);
            let next = sum.add(term.div(Q::integer(denominator)));
            if next == sum {
                break;
            }
            sum = next;
        }
        sum.mul(Q::integer(2)).div(Q::LN2)
    }

    fn ref_log2_positive(mut x: Q) -> Q {
        let mut correction = 0;
        if x.0[1] & 0x7fff_0000_0000_0000 == 0 {
            x = x.mul(Q([0, 0x406f_0000_0000_0000]));
            correction = -112;
        }
        // Keep values in [0.75, 1.0) unshifted so x - 1 does not cancel against
        // adding -1 after normalizing to [1.5, 2.0).
        let raw_exp = ((x.0[1] >> 48) & 0x7fff) as i32;
        if correction == 0 && raw_exp == 0x3ffe && x.0[1] >= 0x3ffe_8000_0000_0000 {
            let t = x.sub(Q::ONE).div(x.add(Q::ONE));
            return ref_logarithm_series(t);
        }
        let exponent = raw_exp - 0x3fff + correction;
        x.0[1] = (x.0[1] & 0x0000_ffff_ffff_ffff) | 0x3fff_0000_0000_0000;
        let t = x.sub(Q::ONE).div(x.add(Q::ONE));
        ref_logarithm_series(t).add(Q::integer(exponent))
    }

    fn ref_log_product(x: Ext, y: Ext, plus_one: bool) -> f64 {
        start();
        let a = Q::from_ext(x);
        let target = if plus_one { a.add(Q::ONE) } else { a };
        let log = if plus_one && a.abs().lt(Q([0, 0x3ffe_0000_0000_0000])) {
            ref_logarithm_series(a.div(Q::integer(2).add(a)))
        } else {
            ref_log2_positive(target)
        };
        let (out, _, _) = finish(Q::from_ext(y).mul(log), 0x37f, PE);
        decode_f64(&out.0)
    }

    fn ref_exp2_minus_one(x: Ext) -> f64 {
        start();
        let t = Q::from_ext(x).mul(Q::LN2);
        let mut term = t;
        let mut sum = t;
        for n in 2..=64 {
            term = term.mul(t).div(Q::integer(n));
            let next = sum.add(term);
            if next == sum {
                break;
            }
            sum = next;
        }
        let (out, _, _) = finish(sum, 0x37f, PE);
        decode_f64(&out.0)
    }

    fn ref_atan_unit(a: Q) -> Q {
        let reduce = Q([0, 0x3ffe_0000_0000_0000]).lt(a);
        let t = if reduce {
            a.sub(Q::ONE).div(a.add(Q::ONE))
        } else {
            a
        };
        let minus_square = t.mul(t).neg();
        let mut term = t;
        let mut sum = t;
        for n in (3..=241).step_by(2) {
            term = term.mul(minus_square);
            let next = sum.add(term.div(Q::integer(n)));
            if sum == next {
                break;
            }
            sum = next;
        }
        if reduce {
            sum.add(Q::PI.div(Q::integer(4)))
        } else {
            sum
        }
    }

    fn ref_atan2(y: Ext, x: Ext) -> f64 {
        start();
        let ax = Q::from_ext(x).abs();
        let ay = Q::from_ext(y).abs();
        let pi2 = Q::PI.div(Q::integer(2));
        let mut result = if ax.lt(ay) {
            pi2.sub(ref_atan_unit(ax.div(ay)))
        } else {
            ref_atan_unit(ay.div(ax))
        };
        if negative(x) {
            result = Q::PI.sub(result);
        }
        if negative(y) {
            result = result.neg();
        }
        let (out, _, _) = finish(result, 0x37f, PE);
        decode_f64(&out.0)
    }

    fn ulp_diff_f64(actual: f64, expected: f64) -> u64 {
        assert!(
            actual.is_finite() && expected.is_finite(),
            "non-finite comparison: actual={actual} expected={expected}"
        );
        if actual == expected {
            return 0;
        }
        let ordered = |v: f64| -> i64 {
            let bits = v.to_bits() as i64;
            if bits < 0 { i64::MIN - bits } else { bits }
        };
        ordered(actual).abs_diff(ordered(expected))
    }

    // spec: A64F-FP-10
    #[test]
    fn native_x87_transcendentals_stay_within_2_ulp_of_binary64() {
        let mut trig_inputs: Vec<Ext> = Vec::new();
        for i in -200..=200 {
            let x = (i as f64) * 0.037_123_456_789_f64;
            if x != 0.0 {
                trig_inputs.push(Ext(encode_f64(x)));
            }
        }
        for &x in &[
            1e-12_f64,
            1e-7,
            1e-4,
            0.5,
            1.0,
            std::f64::consts::FRAC_PI_4,
            std::f64::consts::FRAC_PI_2 - 1e-6,
            std::f64::consts::PI - 1e-6,
            100.0,
            10_000.0,
            1e9,
        ] {
            trig_inputs.push(Ext(encode_f64(x)));
            trig_inputs.push(Ext(encode_f64(-x)));
        }
        // Include full 64-bit significand extended80 values (non-zero low 11 bits).
        for n in 0..32u64 {
            trig_inputs.push(Ext::from_parts(
                0x8000_0000_0000_0001 + n * 0x0301_0101_0101_0101,
                (0x3ffb + (n % 8) as u16) | if n % 2 != 0 { 0x8000 } else { 0 },
            ));
        }
        for &x in &trig_inputs {
            start();
            let (rs, rc) = sin_cos_series(x);
            let (ref_s_ext, _, _) = finish(rs, 0x37f, PE);
            let (ref_c_ext, _, _) = finish(rc, 0x37f, PE);
            let (ref_t_ext, _, _) = finish(rs.div(rc), 0x37f, PE);
            let ref_s = decode_f64(&ref_s_ext.0);
            let ref_c = decode_f64(&ref_c_ext.0);
            let ref_t = decode_f64(&ref_t_ext.0);

            let (s, sf, _) = sin(x, 0x37f).unwrap();
            let (c, cf, _) = cos(x, 0x37f).unwrap();
            let ((sc_s, sc_c), scf, _) = sin_cos(x, 0x37f).unwrap();
            let (t, tf, _) = tan(x, 0x37f).unwrap();
            assert_eq!(sf & PE, PE);
            assert_eq!(cf & PE, PE);
            assert_eq!(scf & PE, PE);
            assert_eq!(tf & PE, PE);

            assert!(
                ulp_diff_f64(decode_f64(&s.0), ref_s) <= 2,
                "fsin({:?}): actual={} ref={}",
                decode_f64(&x.0),
                decode_f64(&s.0),
                ref_s
            );
            assert!(
                ulp_diff_f64(decode_f64(&c.0), ref_c) <= 2,
                "fcos({:?}): actual={} ref={}",
                decode_f64(&x.0),
                decode_f64(&c.0),
                ref_c
            );
            assert!(
                ulp_diff_f64(decode_f64(&sc_s.0), ref_s) <= 2
                    && ulp_diff_f64(decode_f64(&sc_c.0), ref_c) <= 2,
                "fsincos({:?})",
                decode_f64(&x.0)
            );
            assert!(
                ulp_diff_f64(decode_f64(&t.0), ref_t) <= 2,
                "fptan({:?}): actual={} ref={}",
                decode_f64(&x.0),
                decode_f64(&t.0),
                ref_t
            );
        }

        // F2XM1 across [-1, 1] including 64-bit significands and tiny values.
        for i in -199..=199 {
            if i == 0 {
                continue;
            }
            let x = Ext(encode_f64(i as f64 / 200.0));
            let (actual, flags, _) = exp2_minus_one(x, 0x37f).unwrap();
            assert_eq!(flags & PE, PE);
            let expected = ref_exp2_minus_one(x);
            assert!(
                ulp_diff_f64(decode_f64(&actual.0), expected) <= 2,
                "f2xm1({i}/200): actual={} ref={expected}",
                decode_f64(&actual.0)
            );
        }
        for x in [
            Ext::from_parts(0x8000_0000_0000_0001, 0x3ffe),
            Ext::from_parts(0xffff_ffff_ffff_ffff, 0x3ffe),
            Ext::from_parts(0x8000_0000_0000_0001, 0xbffe),
            Ext::from_parts(0x8000_0000_0000_0000, 0x3fc0),
        ] {
            let (actual, _, _) = exp2_minus_one(x, 0x37f).unwrap();
            let expected = ref_exp2_minus_one(x);
            assert!(ulp_diff_f64(decode_f64(&actual.0), expected) <= 2);
        }

        // FPATAN across all four quadrants and wide exponent ratios.
        let coords = [
            -100.0_f64, -3.5, -1.0, -0.25, -1e-9, 1e-9, 0.25, 1.0, 3.5, 100.0,
        ];
        for &y in &coords {
            for &x in &coords {
                let ye = Ext(encode_f64(y));
                let xe = Ext(encode_f64(x));
                let (actual, flags, _) = atan2(ye, xe, 0x37f);
                assert_eq!(flags & PE, PE);
                let expected = ref_atan2(ye, xe);
                assert!(
                    ulp_diff_f64(decode_f64(&actual.0), expected) <= 2,
                    "fpatan({y}, {x}): actual={} ref={expected}",
                    decode_f64(&actual.0)
                );
            }
        }

        // FYL2X including values near 1.0 (both f64 and 80-bit) and powers of 2.
        let y_vals = [-3.25_f64, -1.0, 0.5, 1.0, std::f64::consts::LN_2, 7.5];
        let x_vals = [
            Ext(encode_f64(1e-100)),
            Ext(encode_f64(0.125)),
            Ext(encode_f64(0.75)),
            Ext(encode_f64(0.999_999)),
            Ext::from_parts(0xffff_ffff_ffff_ffff, 0x3ffe),
            Ext::from_parts(0x8000_0000_0000_0001, 0x3fff),
            Ext(encode_f64(1.000_001)),
            Ext(encode_f64(2.0)),
            Ext(encode_f64(3.0)),
            Ext(encode_f64(256.0)),
            Ext(encode_f64(1e100)),
        ];
        for &y in &y_vals {
            let ye = Ext(encode_f64(y));
            for &xe in &x_vals {
                let (actual, flags, _) = log_product(xe, ye, false, 0x37f);
                assert_eq!(flags & PE, PE);
                let expected = ref_log_product(xe, ye, false);
                assert!(
                    ulp_diff_f64(decode_f64(&actual.0), expected) <= 2,
                    "fyl2x({xe:?}, {y}): actual={} ref={expected}",
                    decode_f64(&actual.0)
                );
            }
        }

        // FYL2XP1 across (-1, +inf) including tiny and near -1 values.
        let xp1_vals = [
            Ext(encode_f64(-0.999_99)),
            Ext(encode_f64(-0.29)),
            Ext(encode_f64(-1e-8)),
            Ext(encode_f64(1e-15)),
            Ext(encode_f64(1e-8)),
            Ext(encode_f64(0.29)),
            Ext::from_parts(0x8000_0000_0000_0001, 0x3ffd),
            Ext(encode_f64(10.0)),
        ];
        for &y in &y_vals {
            let ye = Ext(encode_f64(y));
            for &xe in &xp1_vals {
                let (actual, flags, _) = log_product(xe, ye, true, 0x37f);
                assert_eq!(flags & PE, PE);
                let expected = ref_log_product(xe, ye, true);
                assert!(
                    ulp_diff_f64(decode_f64(&actual.0), expected) <= 2,
                    "fyl2xp1({xe:?}, {y}): actual={} ref={expected}",
                    decode_f64(&actual.0)
                );
            }
        }
    }

    // spec: A64F-FP-10
    #[test]
    fn special_values_c2_and_extended_exponent_edges_are_exact() {
        let pz = signed_zero(false);
        let nz = signed_zero(true);
        let pinf = infinity(false);
        let ninf = infinity(true);
        let qnan = Ext::from_parts(0xc000_0000_0000_0123, 0x7fff);
        let snan = Ext::from_parts(0x8000_0000_0000_0123, 0x7fff);
        let bound = Ext::from_parts(1u64 << 63, 0x403e); // 2^63

        // Trig special values and out-of-range C2=1 (None).
        assert_eq!(sin(pz, 0x37f), Some((pz, 0, false)));
        assert_eq!(sin(nz, 0x37f), Some((nz, 0, false)));
        assert_eq!(cos(pz, 0x37f), Some((ONE, 0, false)));
        assert_eq!(cos(nz, 0x37f), Some((ONE, 0, false)));
        assert_eq!(tan(pz, 0x37f), Some((pz, 0, false)));
        assert_eq!(tan(nz, 0x37f), Some((nz, 0, false)));
        assert_eq!(sin_cos(nz, 0x37f), Some(((nz, ONE), 0, false)));
        for inf in [pinf, ninf] {
            assert_eq!(sin(inf, 0x37f), Some((INDEFINITE, IE, false)));
            assert_eq!(cos(inf, 0x37f), Some((INDEFINITE, IE, false)));
            assert_eq!(tan(inf, 0x37f), Some((INDEFINITE, IE, false)));
            assert_eq!(
                sin_cos(inf, 0x37f),
                Some(((INDEFINITE, INDEFINITE), IE, false))
            );
        }
        assert_eq!(sin(bound, 0x37f), None);
        assert_eq!(cos(bound, 0x37f), None);
        assert_eq!(tan(bound, 0x37f), None);
        assert_eq!(sin_cos(bound, 0x37f), None);
        assert_eq!(sin(qnan, 0x37f).unwrap().1, 0);
        assert_eq!(sin(snan, 0x37f).unwrap().1, IE);

        // Tiny subnormal sin/cos/tan preserve exact underflow and rounding direction.
        let tiny = Ext::from_parts(1, 0);
        assert_eq!(sin(tiny, 0x37f), Some((tiny, UE | PE, true)));
        assert_eq!(sin(tiny, 0x77f), Some((pz, UE | PE, false))); // RD
        assert_eq!(tan(tiny, 0x37f), Some((tiny, UE | PE, false)));
        assert_eq!(
            tan(tiny, 0xb7f),
            Some((Ext::from_parts(2, 0), UE | PE, true))
        ); // RU
        assert_eq!(cos(tiny, 0x37f), Some((ONE, PE, true)));
        assert_eq!(
            cos(tiny, 0x77f),
            Some((Ext::from_parts(u64::MAX, 0x3ffe), PE, false))
        );

        // F2XM1 special values and out-of-range.
        assert_eq!(exp2_minus_one(pz, 0x37f), Some((pz, 0, false)));
        assert_eq!(exp2_minus_one(nz, 0x37f), Some((nz, 0, false)));
        assert_eq!(exp2_minus_one(ONE, 0x37f), Some((ONE, PE, false)));
        assert_eq!(
            exp2_minus_one(Ext::from_parts(1u64 << 63, 0xbfff), 0x37f),
            Some((NEG_HALF, PE, false))
        );
        assert_eq!(exp2_minus_one(Ext(encode_f64(1.5)), 0x37f), None);
        assert_eq!(exp2_minus_one(pinf, 0x37f), None);

        // FPATAN quadrant special values and extreme 80-bit scaling.
        assert_eq!(atan2(pz, pz, 0x37f), (pz, 0, false));
        assert_eq!(atan2(nz, pz, 0x37f), (nz, 0, false));
        assert_eq!(
            atan2(pz, nz, 0x37f).0,
            Ext::from_parts(0xc90f_daa2_2168_c235, 0x4000)
        );
        assert_eq!(
            atan2(nz, nz, 0x37f).0,
            Ext::from_parts(0xc90f_daa2_2168_c235, 0xc000)
        );
        assert_eq!(
            atan2(pinf, ninf, 0x37f).0,
            Ext::from_parts(0x96cb_e3f9_990e_91a8, 0x4000)
        );
        // Equal huge exponents 2^16000 / 2^16000 -> pi/4, no overflow!
        let huge = Ext::from_parts(1u64 << 63, 0x3fff + 16000);
        let (a_huge, _, _) = atan2(huge, huge, 0x37f);
        assert!(ulp_diff_f64(decode_f64(&a_huge.0), std::f64::consts::FRAC_PI_4) <= 1);
        // Equal tiny exponents 2^-16000 / 2^-16000 -> pi/4, no underflow!
        let small = Ext::from_parts(1u64 << 63, 0x3fff - 16000);
        let (a_small, _, _) = atan2(small, small, 0x37f);
        assert!(ulp_diff_f64(decode_f64(&a_small.0), std::f64::consts::FRAC_PI_4) <= 1);

        // FYL2X / FYL2XP1 special values and overflow/underflow.
        assert_eq!(log_product(nz, ONE, false, 0x37f), (ninf, ZE, false));
        assert_eq!(log_product(pz, pz, false, 0x37f), (INDEFINITE, IE, false));
        assert_eq!(
            log_product(Ext(encode_f64(-2.0)), ONE, false, 0x37f),
            (INDEFINITE, IE, false)
        );
        assert_eq!(
            log_product(ONE, pinf, false, 0x37f),
            (INDEFINITE, IE, false)
        );
        assert_eq!(
            log_product(ONE, Ext(encode_f64(-2.0)), false, 0x37f),
            (nz, 0, false)
        );
        assert_eq!(log_product(pinf, nz, false, 0x37f), (INDEFINITE, IE, false));
        assert_eq!(log_product(nz, ONE, true, 0x37f), (nz, 0, false));
        assert_eq!(
            log_product(Ext(encode_f64(-1.0)), ONE, true, 0x37f),
            (ninf, ZE, false)
        );
        assert_eq!(
            log_product(Ext(encode_f64(-2.0)), ONE, true, 0x37f),
            (INDEFINITE, IE, false)
        );
        // Overflow to +Inf under RN, +EXT_MAX under RZ.
        let max_ext = Ext::from_parts(u64::MAX, 0x7ffe);
        let (ov_rn, f_rn, up_rn) = log_product(Ext(encode_f64(4.0)), max_ext, false, 0x37f);
        assert_eq!((ov_rn, f_rn, up_rn), (pinf, OE | PE, true));
        let (ov_rz, f_rz, up_rz) = log_product(Ext(encode_f64(4.0)), max_ext, false, 0xf7f);
        assert_eq!((ov_rz, f_rz, up_rz), (max_ext, OE | PE, false));
    }
}
