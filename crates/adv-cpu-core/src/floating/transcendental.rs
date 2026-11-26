//! Native binary64 double-double x87 transcendental algorithms (A64F-FP-10)
//! and bit-exact FSCALE (A64F-FP-8).
//!
//! Finite in-range results for FSIN, FCOS, FSINCOS, FPTAN, FPATAN, F2XM1,
//! FYL2X and FYL2XP1 use native non-contracted binary64 double-double
//! arithmetic (~100-bit intermediates) with 320-bit Payne-Hanek trigonometric
//! range reduction across the entire `|x| < 2^63` domain. This stays well
//! within 2 ULP of binary64 while preserving exact x87 special values,
//! range-reduction C2 behavior, directed rounding, overflow/underflow flags,
//! and exact C1 round-up reporting in every rounding mode.
use super::softfloat::{self as sf, Quad as Q};
use crate::X87Register as Ext;

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
const SCALE_2_M105: f64 = f64::from_bits((1023 - 105) << 52); // 2^-105 exact
const SQRT2_SIG: u64 = 0xb504_f333_f9de_6484; // floor(sqrt(2) * 2^63)

/// First 320 fractional bits of `2/pi` in little-endian `u64` limbs:
/// `2/pi = sum(TWO_OVER_PI[i] * 2^(64*i)) / 2^320 + eps`, `0 < eps < 2^-320`.
const TWO_OVER_PI: [u64; 5] = [
    0xb724_6e3a_424d_d2e0,
    0xfe51_63ab_debb_c561,
    0xdb62_9599_3c43_9041,
    0xfc27_57d1_f534_ddc0,
    0xa2f9_836e_4e44_1529,
];

/// Non-contracted IEEE 754 binary64 double-double (~106-bit significand).
#[derive(Clone, Copy, Debug)]
struct Dd {
    hi: f64,
    lo: f64,
}

const PI_DD: Dd = Dd::from_bits(0x4009_21fb_5444_2d18, 0x3ca1_a626_3314_5c07);
const HALF_PI_DD: Dd = Dd::from_bits(0x3ff9_21fb_5444_2d18, 0x3c91_a626_3314_5c07);
const LN2_DD: Dd = Dd::from_bits(0x3fe6_2e42_fefa_39ef, 0x3c7a_bc9e_3b39_803f);
const INV_LN2_DD: Dd = Dd::from_bits(0x3ff7_1547_652b_82fe, 0x3c77_77d0_ffda_0d24);
const INV_3_DD: Dd = Dd::from_bits(0x3fd5_5555_5555_5555, 0x3c75_5555_5555_5555);
const INV_5_DD: Dd = Dd::from_bits(0x3fc9_9999_9999_999a, 0xbc69_9999_9999_999a);
const INV_6_DD: Dd = Dd::from_bits(0x3fc5_5555_5555_5555, 0x3c65_5555_5555_5555);
const INV_7_DD: Dd = Dd::from_bits(0x3fc2_4924_9249_2492, 0x3c62_4924_9249_2492);
const INV_9_DD: Dd = Dd::from_bits(0x3fbc_71c7_1c71_c71c, 0x3c5c_71c7_1c71_c71c);
const INV_11_DD: Dd = Dd::from_bits(0x3fb7_45d1_745d_1746, 0xbc47_45d1_745d_1746);
const INV_24_DD: Dd = Dd::from_bits(0x3fa5_5555_5555_5555, 0x3c45_5555_5555_5555);
const INV_120_DD: Dd = Dd::from_bits(0x3f81_1111_1111_1111, 0x3c01_1111_1111_1111);
const INV_720_DD: Dd = Dd::from_bits(0x3f56_c16c_16c1_6c17, 0xbbef_49f4_9f49_f49f);
const INV_5040_DD: Dd = Dd::from_bits(0x3f2a_01a0_1a01_a01a, 0x3b6a_01a0_1a01_a01a);
const INV_40320_DD: Dd = Dd::from_bits(0x3efa_01a0_1a01_a01a, 0x3b3a_01a0_1a01_a01a);

#[inline]
fn two_sum(a: f64, b: f64) -> Dd {
    let s = a + b;
    let v = s - a;
    let e = (a - (s - v)) + (b - v);
    Dd { hi: s, lo: e }
}

/// Veltkamp split + Dekker exact product without FMA contraction (A64F-FP-10).
#[inline]
fn two_prod(a: f64, b: f64) -> Dd {
    const SPLIT: f64 = 134_217_729.0; // 2^27 + 1
    let p = a * b;
    let ca = SPLIT * a;
    let a_hi = ca - (ca - a);
    let a_lo = a - a_hi;
    let cb = SPLIT * b;
    let b_hi = cb - (cb - b);
    let b_lo = b - b_hi;
    let err = ((a_hi * b_hi - p) + a_hi * b_lo + a_lo * b_hi) + a_lo * b_lo;
    Dd { hi: p, lo: err }
}

impl Dd {
    const ONE: Self = Self { hi: 1.0, lo: 0.0 };

    const fn from_bits(hi: u64, lo: u64) -> Self {
        Self {
            hi: f64::from_bits(hi),
            lo: f64::from_bits(lo),
        }
    }

    #[inline]
    const fn from_f64(hi: f64) -> Self {
        Self { hi, lo: 0.0 }
    }

    #[inline]
    fn neg(self) -> Self {
        Self {
            hi: -self.hi,
            lo: -self.lo,
        }
    }

    #[inline]
    fn scale_pow2(self, exp: i32) -> Self {
        let s = f64::from_bits(((exp + 1023) as u64) << 52);
        Self {
            hi: self.hi * s,
            lo: self.lo * s,
        }
    }

    #[inline]
    fn add(self, rhs: Self) -> Self {
        let s = two_sum(self.hi, rhs.hi);
        let t = two_sum(self.lo, rhs.lo);
        let c = two_sum(s.hi, s.lo + t.hi);
        two_sum(c.hi, c.lo + t.lo)
    }

    #[inline]
    fn add_f64(self, rhs: f64) -> Self {
        let s = two_sum(self.hi, rhs);
        two_sum(s.hi, s.lo + self.lo)
    }

    #[inline]
    fn sub(self, rhs: Self) -> Self {
        self.add(rhs.neg())
    }

    #[inline]
    fn mul(self, rhs: Self) -> Self {
        let p = two_prod(self.hi, rhs.hi);
        let lo = p.lo + (self.hi * rhs.lo + self.lo * rhs.hi);
        two_sum(p.hi, lo)
    }

    #[inline]
    fn mul_f64(self, rhs: f64) -> Self {
        let p = two_prod(self.hi, rhs);
        two_sum(p.hi, p.lo + self.lo * rhs)
    }

    #[inline]
    fn div(self, rhs: Self) -> Self {
        let q1 = self.hi / rhs.hi;
        let r = self.sub(rhs.mul_f64(q1));
        let q2 = r.hi / rhs.hi;
        let r = r.sub(rhs.mul_f64(q2));
        let q3 = r.hi / rhs.hi;
        two_sum(q1, q2).add_f64(q3)
    }

    #[inline]
    fn sqrt(self) -> Self {
        let x = self.hi.sqrt();
        let rem = self.sub(two_prod(x, x));
        two_sum(x, rem.hi / (2.0 * x))
    }
}

/// Exact `Dd` representation of `m * 2^-63` for any 64-bit `m`.
#[inline]
fn u64_to_dd_m63(m: u64) -> Dd {
    let hi_u = m & 0xffff_ffff_0000_0000;
    let lo_u = m & 0x0000_0000_ffff_ffff;
    two_sum((hi_u as f64) * SCALE_2_M63, (lo_u as f64) * SCALE_2_M63)
}

/// Exact `Dd` representation of `m * 2^-64` for any 64-bit `m`.
#[inline]
fn u64_to_dd_m64(m: u64) -> Dd {
    let hi_u = m & 0xffff_ffff_0000_0000;
    let lo_u = m & 0x0000_0000_ffff_ffff;
    two_sum((hi_u as f64) * SCALE_2_M64, (lo_u as f64) * SCALE_2_M64)
}

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

/// Decompose a finite non-zero `f64` into `(negative, unbiased_exp, norm_sig_64)`
/// where `norm_sig_64 >= 2^63` and `value = (-1)^negative * (norm_sig_64 / 2^63) * 2^unbiased_exp`.
fn decompose_f64(v: f64) -> Option<(bool, i32, u64)> {
    let bits = v.to_bits();
    let neg = (bits >> 63) != 0;
    let raw_e = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    if raw_e == 0 {
        if frac == 0 {
            return None;
        }
        let shift = frac.leading_zeros();
        Some((neg, -1011 - shift as i32, frac << shift))
    } else {
        Some((neg, raw_e - 1023, (1u64 << 63) | (frac << 11)))
    }
}

/// Convert `val * 2^exp_shift` (for a ~100-bit double-double `val`) to extended80,
/// performing exact 64-bit rounding in `RC` with exact `OE`/`UE`/`PE` and `C1`.
///
/// When `nudge != 0` and `val` lands on an exact 64-bit significand, `nudge < 0`
/// indicates the true mathematical magnitude is strictly smaller by an
/// infinitesimal epsilon (`|true| = |val| - eps`), and `nudge > 0` indicates
/// strictly larger (`|true| = |val| + eps`).
fn dd_scaled_to_ext(
    val: Dd,
    exp_shift: i32,
    control: u16,
    inherent: u16,
    nudge: i8,
) -> (Ext, u16, bool) {
    let val = two_sum(val.hi, val.lo);
    let Some((neg, mut e_hi, sig_hi)) = decompose_f64(val.hi) else {
        return (signed_zero(val.hi.is_sign_negative()), inherent, false);
    };
    let sign = if neg { 0x8000u16 } else { 0 };
    // Value is `mag * 2^(e_hi - 127)` with bit 127 set.
    let mut mag: u128 = (sig_hi as u128) << 64;
    let mut sticky = false;
    if let Some((lo_neg, e_lo, sig_lo)) = decompose_f64(val.lo) {
        let shift = e_lo - e_hi + 64;
        let (lo_aligned, lo_sticky) = if shift >= 0 {
            ((sig_lo as u128) << (shift as u32), false)
        } else {
            let rsh = (-shift) as u32;
            if rsh >= 64 {
                (0u128, true)
            } else {
                ((sig_lo as u128) >> rsh, (sig_lo & ((1u64 << rsh) - 1)) != 0)
            }
        };
        if lo_neg == neg {
            mag += lo_aligned;
            sticky = lo_sticky;
        } else if lo_sticky {
            mag -= lo_aligned + 1;
            sticky = true;
        } else {
            mag -= lo_aligned;
        }
    }
    if nudge != 0 && (mag as u64) == 0 && !sticky {
        if nudge < 0 {
            mag -= 1;
        } else {
            sticky = true;
        }
    }
    if mag < (1u128 << 127) {
        let lz = mag.leading_zeros();
        mag <<= lz;
        e_hi -= lz as i32;
    }
    if sticky {
        mag |= 1;
    }

    let total_exp = e_hi + exp_shift;
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
            (0u64, mag > (1u128 << 127), mag == (1u128 << 127), true)
        } else {
            let total_shift = 64 + rshift;
            let quot = (mag >> total_shift) as u64;
            let rem = mag & ((1u128 << total_shift) - 1);
            let half = 1u128 << (total_shift - 1);
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

    let quot = (mag >> 64) as u64;
    let rem = mag as u64;
    let half = 1u64 << 63;
    let rem_nonzero = rem != 0;
    let inc = rem_nonzero
        && match rc {
            0 => rem > half || (rem == half && quot & 1 != 0),
            1 => neg,
            2 => !neg,
            _ => false,
        };
    let (out_sig, out_exp) = if inc {
        if quot == u64::MAX {
            (1u64 << 63, total_exp + 1)
        } else {
            (quot + 1, total_exp)
        }
    } else {
        (quot, total_exp)
    };
    if out_exp > 16383 {
        return (infinity(neg), inherent | OE | PE, true);
    }
    let biased = (out_exp + 0x3fff) as u16;
    let flags = inherent | if rem_nonzero { PE } else { 0 };
    (Ext::from_parts(out_sig, sign | biased), flags, inc)
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

#[cold]
fn sin_cos_quad(x: Ext) -> (Q, Q) {
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

/// True if a `Dd` value's 64-bit rounding remainder sits within 2^-88 of a midpoint
/// or integer boundary, where 113-bit `Quad` refinement resolves Table Maker's Dilemma.
#[inline]
fn dd_near_boundary(val: Dd) -> bool {
    let val = two_sum(val.hi, val.lo);
    let Some((neg, e_hi, sig_hi)) = decompose_f64(val.hi) else {
        return false;
    };
    let mut mag: u128 = (sig_hi as u128) << 64;
    if let Some((lo_neg, e_lo, sig_lo)) = decompose_f64(val.lo) {
        let shift = e_lo - e_hi + 64;
        let lo_aligned = if shift >= 0 {
            (sig_lo as u128) << (shift as u32)
        } else {
            let rsh = (-shift) as u32;
            if rsh >= 64 {
                0u128
            } else {
                (sig_lo as u128) >> rsh
            }
        };
        if lo_neg == neg {
            mag += lo_aligned;
        } else {
            mag -= lo_aligned;
        }
    }
    if mag < (1u128 << 127) {
        mag <<= mag.leading_zeros();
    }
    let rem = mag as u64;
    let dist_half = rem.abs_diff(1u64 << 63);
    let dist_zero = rem.min(rem.wrapping_neg());
    dist_half < (1u64 << 40) || dist_zero < (1u64 << 40)
}

/// `atanh(t)` for `|t| <= 0.044` to ~104 bits.
fn atanh_dd(t: Dd) -> Dd {
    let t2 = t.mul(t);
    let u = t2.hi;
    // Tail `t^12/13 + t^14/15 + t^16/17 + t^18/19 + t^20/21` is < 2^-56, so f64 suffices.
    let tail =
        ((((1.0 / 21.0) * u + 1.0 / 19.0) * u + 1.0 / 17.0) * u + 1.0 / 15.0) * u + 1.0 / 13.0;
    let mut poly = INV_11_DD.add(t2.mul_f64(tail));
    poly = INV_9_DD.add(t2.mul(poly));
    poly = INV_7_DD.add(t2.mul(poly));
    poly = INV_5_DD.add(t2.mul(poly));
    poly = INV_3_DD.add(t2.mul(poly));
    poly = Dd::ONE.add(t2.mul(poly));
    t.mul(poly)
}

/// `ln(m)` for `m in [sqrt(0.5), sqrt(2)]` given exact `m_minus_1 = m - 1`.
fn ln_reduced_dd(m: Dd, m_minus_1: Dd) -> Dd {
    if m_minus_1.hi.abs() <= 0.0625 {
        let t = m_minus_1.div(m.add_f64(1.0));
        atanh_dd(t).scale_pow2(1)
    } else {
        let m2 = m.sqrt().sqrt();
        let t = m2.add_f64(-1.0).div(m2.add_f64(1.0));
        atanh_dd(t).scale_pow2(3)
    }
}

/// `log2(m * 2^k)` where `m` is in `[0.5, 2.0)` to ~102 bits.
fn log2_scaled_dd(mut k: i32, mut m: Dd) -> Dd {
    if m.hi >= std::f64::consts::SQRT_2 {
        m = m.scale_pow2(-1);
        k += 1;
    } else if m.hi < std::f64::consts::FRAC_1_SQRT_2 {
        m = m.scale_pow2(1);
        k -= 1;
    }
    let ln_m = ln_reduced_dd(m, m.add_f64(-1.0));
    Dd::from_f64(k as f64).add(ln_m.mul(INV_LN2_DD))
}

/// `log2(mx * 2^(ex - 63))` for normalized `mx >= 2^63` to ~102 bits with zero cancellation.
fn log2_norm_u64_dd(ex: i32, mx: u64) -> Dd {
    if mx == (1u64 << 63) {
        return Dd::from_f64(ex as f64);
    }
    let (k, m, m_minus_1) = if mx >= SQRT2_SIG {
        (
            ex + 1,
            u64_to_dd_m64(mx),
            u64_to_dd_m64(mx.wrapping_neg()).neg(),
        )
    } else {
        (ex, u64_to_dd_m63(mx), u64_to_dd_m63(mx - (1u64 << 63)))
    };
    let ln_m = ln_reduced_dd(m, m_minus_1);
    Dd::from_f64(k as f64).add(ln_m.mul(INV_LN2_DD))
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
        let (_, ey, my) = decompose_finite(y);
        let ym = {
            let mag = u64_to_dd_m63(my);
            if sy { mag.neg() } else { mag }
        };
        let log_val = log2_norm_u64_dd(ex, mx);
        return dd_scaled_to_ext(ym.mul(log_val), ey, control, PE, 0);
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
        let mag = u64_to_dd_m63(my);
        if sy { mag.neg() } else { mag }
    };
    let xm = {
        let mag = u64_to_dd_m63(mx);
        if sx { mag.neg() } else { mag }
    };
    if ex < -105 {
        let prod = ym.mul(xm).mul(INV_LN2_DD);
        return dd_scaled_to_ext(prod, ey + ex, control, PE, 0);
    }
    if ex <= -36 {
        // |x| < 2^-35: ln(1 + x) = x * (1 - x/2 + x^2/3 - x^3/4) to >106 bits.
        let x_dd = xm.scale_pow2(ex);
        let x2 = x_dd.mul(x_dd);
        let ratio = Dd::ONE
            .sub(x_dd.scale_pow2(-1))
            .add(x2.mul(INV_3_DD))
            .sub(x2.mul(x_dd).scale_pow2(-2));
        let prod = ym.mul(xm).mul(ratio).mul(INV_LN2_DD);
        return dd_scaled_to_ext(prod, ey + ex, control, PE, 0);
    }
    if ex <= -5 {
        // 2^-35 <= |x| < 0.0625: ln(1 + x) = 2 * atanh(x / (2 + x)) with zero cancellation.
        let x_dd = xm.scale_pow2(ex);
        let t = x_dd.div(x_dd.add_f64(2.0));
        let log_val = atanh_dd(t).scale_pow2(1).mul(INV_LN2_DD);
        return dd_scaled_to_ext(ym.mul(log_val), ey, control, PE, 0);
    }
    if sx && ex == -1 {
        if mx > 0xffff_ffff_f800_0000 {
            return log_product_quad(x, y, control);
        }
        // -1 < x <= -0.5: compute 1 + x exactly in integer arithmetic (zero cancellation).
        let nz = mx.wrapping_neg();
        let shift = nz.leading_zeros();
        let norm_m = nz << shift;
        let norm_e = -1 - shift as i32;
        let log_val = log2_norm_u64_dd(norm_e, norm_m);
        return dd_scaled_to_ext(ym.mul(log_val), ey, control, PE, 0);
    }
    if ex > 105 {
        // x >= 2^105: log2(1 + x) = log2(x) + tiny_eps (> log2(x) strictly).
        let log_val = log2_norm_u64_dd(ex, mx);
        return dd_scaled_to_ext(ym.mul(log_val), ey, control, PE, 1);
    }
    // -4 <= ex <= 105 (and x > -0.5): 1 + x is exact in Dd.
    let log_val = if !sx && ex >= 0 {
        let mut m = xm.add_f64(f64::from_bits(((1023 - ex) as u64) << 52));
        let mut k = ex;
        if m.hi >= 2.0 {
            m = m.scale_pow2(-1);
            k += 1;
        }
        log2_scaled_dd(k, m)
    } else {
        let m = Dd::ONE.add(xm.scale_pow2(ex));
        log2_scaled_dd(0, m)
    };
    dd_scaled_to_ext(ym.mul(log_val), ey, control, PE, 0)
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
    let xm = {
        let mag = u64_to_dd_m63(mx);
        if sx { mag.neg() } else { mag }
    };
    if ex < -105 {
        return Some(dd_scaled_to_ext(xm.mul(LN2_DD), ex, control, PE, 0));
    }
    if ex <= -36 {
        // |x| < 2^-35: exp(t) - 1 = t * (1 + t/2 + t^2/6 + t^3/24) to >106 bits.
        let t_unit = xm.mul(LN2_DD);
        let t = t_unit.scale_pow2(ex);
        let t2 = t.mul(t);
        let ratio = Dd::ONE
            .add(t.scale_pow2(-1))
            .add(t2.mul(INV_6_DD))
            .add(t2.mul(t).mul(INV_24_DD));
        return Some(dd_scaled_to_ext(t_unit.mul(ratio), ex, control, PE, 0));
    }
    // |x| < 1: u = x * ln(2) / 16 has |u| < 0.0434, so u^15 / 15! < 2^-105.
    let u = xm.scale_pow2(ex).mul(LN2_DD).scale_pow2(-4);
    let uh = u.hi;
    let tail = (((((1.0 / 87_178_291_200.0) * uh + 1.0 / 6_227_020_800.0) * uh
        + 1.0 / 479_001_600.0)
        * uh
        + 1.0 / 39_916_800.0)
        * uh
        + 1.0 / 3_628_800.0)
        * uh
        + 1.0 / 362_880.0;
    let mut e = INV_40320_DD.add(u.mul_f64(tail));
    e = INV_5040_DD.add(u.mul(e));
    e = INV_720_DD.add(u.mul(e));
    e = INV_120_DD.add(u.mul(e));
    e = INV_24_DD.add(u.mul(e));
    e = INV_6_DD.add(u.mul(e));
    e = Dd::from_f64(0.5).add(u.mul(e));
    e = Dd::ONE.add(u.mul(e));
    e = u.mul(e);
    // Four exact doubling steps: exp(2v) - 1 = (exp(v) - 1) * (exp(v) + 1) = e * (e + 2).
    for _ in 0..4 {
        e = e.mul(e.add_f64(2.0));
    }
    Some(dd_scaled_to_ext(e, 0, control, PE, 0))
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

/// Exact 320-bit Payne-Hanek range reduction of `|x| = mx * 2^(ex - 63)` for `-1 <= ex <= 62`
/// (`0.5 <= |x| < 2^63`). Returns `(quadrant, r)` where `|x| = k * (pi/2) + r`,
/// `quadrant = k & 3`, `|r| <= pi/4`, and `r` retains ~104 bits of precision across
/// the entire valid domain (including arguments near `2^63` and near multiples of `pi/2`).
fn payne_hanek_reduce(mx: u64, ex: i32) -> (u8, Dd) {
    debug_assert!((-1..=62).contains(&ex));
    let s = (63 - ex) as u32; // in 1..=64
    let mut prod = [0u64; 6];
    let mut carry = 0u128;
    for (dst, &limb) in prod.iter_mut().zip(TWO_OVER_PI.iter()) {
        let p = (mx as u128) * (limb as u128) + carry;
        *dst = p as u64;
        carry = p >> 64;
    }
    prod[5] = carry as u64;

    let (k_floor, frac) = if s == 64 {
        (0u64, [prod[2], prod[3], prod[4], prod[5]])
    } else {
        let r = 64 - s;
        (
            prod[5] >> s,
            [
                (prod[1] >> s) | (prod[2] << r),
                (prod[2] >> s) | (prod[3] << r),
                (prod[3] >> s) | (prod[4] << r),
                (prod[4] >> s) | (prod[5] << r),
            ],
        )
    };

    let (quadrant, f_neg, f_mag) = if frac[3] >> 63 != 0 {
        let mut mag = [0u64; 4];
        let mut borrow = 0u64;
        for i in 0..4 {
            let (d1, b1) = 0u64.overflowing_sub(frac[i]);
            let (d2, b2) = d1.overflowing_sub(borrow);
            mag[i] = d2;
            borrow = u64::from(b1 || b2);
        }
        ((k_floor.wrapping_add(1) & 3) as u8, true, mag)
    } else {
        ((k_floor & 3) as u8, false, frac)
    };

    let top_idx = if f_mag[3] != 0 {
        3
    } else if f_mag[2] != 0 {
        2
    } else if f_mag[1] != 0 {
        1
    } else {
        0
    };
    let limb_lz = f_mag[top_idx].leading_zeros();
    let total_lz = ((3 - top_idx) as u32) * 64 + limb_lz;
    let get = |idx: isize| -> u64 { if idx >= 0 { f_mag[idx as usize] } else { 0 } };
    let i = top_idx as isize;
    let (top, mid) = if limb_lz == 0 {
        (get(i), get(i - 1))
    } else {
        let r = 64 - limb_lz;
        (
            (get(i) << limb_lz) | (get(i - 1) >> r),
            (get(i - 1) << limb_lz) | (get(i - 2) >> r),
        )
    };
    let hi_bits = top & !0x7ff;
    let hi = (hi_bits as f64) * SCALE_2_M63;
    let lo_u = (((top & 0x7ff) << 42) | (mid >> 22)) + ((mid >> 21) & 1);
    let lo = (lo_u as f64) * SCALE_2_M105;
    let f_dd = two_sum(hi, lo).scale_pow2(-(total_lz as i32 + 1));
    let r = (if f_neg { f_dd.neg() } else { f_dd }).mul(HALF_PI_DD);
    (quadrant, r)
}

/// Compute `(sin(x), cos(x))` to ~102 bits for finite `x` with `2^-33 <= |x| < 2^63`.
/// Also returns `(sin_nudge, cos_nudge)` for exact `C1` when `|r| < 2^-52` collapses `cos(r)` to 1.
fn sin_cos_dd(x: Ext) -> ((Dd, i8), (Dd, i8)) {
    let (sx, ex, mx) = decompose_finite(x);
    let (quadrant, r) = if ex >= -1 {
        payne_hanek_reduce(mx, ex)
    } else {
        (0u8, u64_to_dd_m63(mx).scale_pow2(ex))
    };

    // Reduce |r| <= pi/4 by 8 -> |u| <= pi/32 < 0.0982.
    let u = r.scale_pow2(-3);
    let u2 = u.mul(u);
    let w = u2.hi;
    // sin(u) = u * (1 - u^2/3! + u^4/5! - u^6/7! + u^8/9! - u^10/11! + u^12/13! - u^14/15!)
    let sin_tail =
        (((-1.0 / 1_307_674_368_000.0) * w + 1.0 / 6_227_020_800.0) * w - 1.0 / 39_916_800.0) * w
            + 1.0 / 362_880.0;
    let mut sp = INV_5040_DD.neg().add(u2.mul_f64(sin_tail));
    sp = INV_120_DD.add(u2.mul(sp));
    sp = INV_6_DD.neg().add(u2.mul(sp));
    sp = Dd::ONE.add(u2.mul(sp));
    let mut s = u.mul(sp);

    // cos(u) = 1 - u^2/2! + u^4/4! - u^6/6! + u^8/8! - u^10/10! + u^12/12! - u^14/14! + u^16/16!
    let cos_tail =
        ((1.0 / 20_922_789_888_000.0 * w - 1.0 / 87_178_291_200.0) * w + 1.0 / 479_001_600.0) * w
            - 1.0 / 3_628_800.0;
    let mut cp = INV_40320_DD.add(u2.mul_f64(cos_tail));
    cp = INV_720_DD.neg().add(u2.mul(cp));
    cp = INV_24_DD.add(u2.mul(cp));
    cp = Dd::from_f64(-0.5).add(u2.mul(cp));
    let mut c = Dd::ONE.add(u2.mul(cp));

    // Three angle-doubling steps: 2u, 4u, 8u = r.
    for _ in 0..3 {
        let s_next = s.mul(c).scale_pow2(1);
        let c_next = Dd::ONE.sub(s.mul(s).scale_pow2(1));
        s = s_next;
        c = c_next;
    }

    // For non-zero r, |cos(r)| < 1 strictly (matters when |r| < 2^-53 makes 1 - r^2/2 == 1.0).
    let c_nudge: i8 = if c.hi == 1.0 && c.lo == 0.0 { -1 } else { 0 };
    let (mut sv, sn, cv, cn) = match quadrant {
        0 => (s, 0i8, c, c_nudge),
        1 => (c, c_nudge, s.neg(), 0i8),
        2 => (s.neg(), 0i8, c.neg(), c_nudge),
        _ => (c.neg(), c_nudge, s, 0i8),
    };
    if sx {
        sv = sv.neg();
    }
    ((sv, sn), (cv, cn))
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
    let ((sv, sn), _) = sin_cos_dd(x);
    if ex <= 3 && sv.hi.abs() >= 0.0625 && dd_near_boundary(sv) {
        start();
        let (sine, _) = sin_cos_quad(x);
        return Some(finish(sine, control, PE));
    }
    Some(dd_scaled_to_ext(sv, 0, control, PE, sn))
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
    let (_, (cv, cn)) = sin_cos_dd(x);
    if ex <= 3 && cv.hi.abs() >= 0.0625 && dd_near_boundary(cv) {
        start();
        let (_, cosine) = sin_cos_quad(x);
        return Some(finish(cosine, control, PE));
    }
    Some(dd_scaled_to_ext(cv, 0, control, PE, cn))
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
    let ((sv, sn), (cv, cn)) = sin_cos_dd(x);
    let (s, sf, _) = dd_scaled_to_ext(sv, 0, control, PE, sn);
    let (c, cf, cu) = dd_scaled_to_ext(cv, 0, control, PE, cn);
    Some(((s, c), sf | cf, cu))
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
    let ((sv, _), (cv, _)) = sin_cos_dd(x);
    Some(dd_scaled_to_ext(sv.div(cv), 0, control, PE, 0))
}

/// `atan(a)` for `0 < a <= 1` to ~103 bits via half-angle reduction and odd series.
fn atan_unit_dd(mut a: Dd) -> Dd {
    let mut halvings = 0i32;
    while a.hi > 0.05 {
        let denom = Dd::ONE.add(Dd::ONE.add(a.mul(a)).sqrt());
        a = a.div(denom);
        halvings += 1;
    }
    let a2 = a.mul(a);
    let w = a2.hi;
    // Tail `-a^12/13 + a^14/15 - a^16/17 + a^18/19 - a^20/21 + a^22/23` is < 2^-55.
    let tail = (((((1.0 / 23.0) * w - 1.0 / 21.0) * w + 1.0 / 19.0) * w - 1.0 / 17.0) * w
        + 1.0 / 15.0)
        * w
        - 1.0 / 13.0;
    let mut poly = INV_11_DD.add(a2.mul_f64(tail));
    poly = INV_9_DD.neg().add(a2.mul(poly));
    poly = INV_7_DD.add(a2.mul(poly));
    poly = INV_5_DD.neg().add(a2.mul(poly));
    poly = INV_3_DD.add(a2.mul(poly));
    poly = Dd::ONE.sub(a2.mul(poly));
    let res = a.mul(poly);
    if halvings > 0 {
        res.scale_pow2(halvings)
    } else {
        res
    }
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
    if diff == 0 && mx == my {
        return if sx {
            rounded_pi_multiple(THREE_PI_4_SIG, 0x4000, sy, control)
        } else {
            rounded_pi_multiple(PI_SIG, 0x3ffe, sy, control)
        };
    }
    if diff > 65 {
        return rounded_pi_multiple(PI_SIG, 0x3fff, sy, control);
    }
    if diff < -65 {
        if sx {
            return rounded_pi_multiple(PI_SIG, 0x4000, sy, control);
        }
        let ratio = u64_to_dd_m63(my).div(u64_to_dd_m63(mx));
        let signed = if sy { ratio.neg() } else { ratio };
        // atan(r) < r strictly; nudge=-1 keeps C1 exact even when y/x is an exact 64-bit value.
        return dd_scaled_to_ext(signed, diff, control, PE, -1);
    }
    let mx_dd = u64_to_dd_m63(mx);
    let my_dd = u64_to_dd_m63(my);
    let y_gt_x = diff > 0 || (diff == 0 && my > mx);
    let (mut result, nudge) = if y_gt_x {
        let ratio = mx_dd.div(my_dd).scale_pow2(-diff);
        (HALF_PI_DD.sub(atan_unit_dd(ratio)), 0i8)
    } else {
        let ratio = my_dd.div(mx_dd).scale_pow2(diff);
        let nudge = if !sx && diff < -50 { -1i8 } else { 0i8 };
        (atan_unit_dd(ratio), nudge)
    };
    if sx {
        result = PI_DD.sub(result);
    }
    if sy {
        result = result.neg();
    }
    dd_scaled_to_ext(result, 0, control, PE, if sx { 0 } else { nudge })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::x87::{decode_f64, encode_f64};

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
            let (rs, rc) = sin_cos_quad(x);
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

    // spec: A64F-FP-10
    #[test]
    fn large_magnitude_trig_payne_hanek_stays_within_2_ulp() {
        // 256-bit reference values (binary64 bits) for large valid extended80 inputs |x| < 2^63.
        let cases: &[(&str, Ext, u64, u64, u64)] = &[
            (
                "2^62 + 5.5",
                Ext::from_parts(0x8000_0000_0000_000b, 0x403d),
                0x3f6e_34d9_cfd7_073b,
                0xbfef_fff1_be44_fb4a,
                0xbf6e_34e7_4508_34cc,
            ),
            (
                "-(2^62 + 5.5)",
                Ext::from_parts(0x8000_0000_0000_000b, 0xc03d),
                0xbf6e_34d9_cfd7_073b,
                0xbfef_fff1_be44_fb4a,
                0x3f6e_34e7_4508_34cc,
            ),
            (
                "2^62",
                Ext::from_parts(1u64 << 63, 0x403d),
                0xbfe6_7e57_355f_0147,
                0xbfe6_c2b1_f820_c708,
                0x3fef_9fe5_be66_4fa7,
            ),
            (
                "2^63 - 0.5",
                Ext::from_parts(u64::MAX, 0x403d),
                0x3feb_e64f_c225_57de,
                0x3fdf_5806_bd89_0925,
                0x3ffc_7bd3_da73_25e6,
            ),
            (
                "2^63 - 1.0",
                Ext::from_parts(0xffff_ffff_ffff_fffe, 0x403d),
                0x3fe0_f881_aa0a_9a21,
                0x3feb_2114_50b0_0621,
                0x3fe4_0485_171a_4ada,
            ),
            (
                "near pi/2 @ 2^62",
                Ext::from_parts(0xffff_ffff_ffff_fffc, 0x403c),
                0x3fcb_ff00_360a_6f80,
                0xbfef_39a7_475a_081e,
                0xbfcc_b0d5_ed92_5254,
            ),
            (
                "near pi/2 @ 2^30",
                Ext::from_parts(0xffff_fffc_60cf_fa92, 0x401c),
                0xbff0_0000_0000_0000,
                0x3d74_a10d_cab5_2b4f,
                0xc268_d1bc_e51c_e20f,
            ),
        ];

        for &(label, x, ref_s_bits, ref_c_bits, ref_t_bits) in cases {
            let ref_s = f64::from_bits(ref_s_bits);
            let ref_c = f64::from_bits(ref_c_bits);
            let ref_t = f64::from_bits(ref_t_bits);

            let (s, sf, _) = sin(x, 0x37f).unwrap();
            let (c, cf, _) = cos(x, 0x37f).unwrap();
            let ((sc_s, sc_c), scf, _) = sin_cos(x, 0x37f).unwrap();
            let (t, tf, _) = tan(x, 0x37f).unwrap();
            assert_eq!(sf & PE, PE, "{label}");
            assert_eq!(cf & PE, PE, "{label}");
            assert_eq!(scf & PE, PE, "{label}");
            assert_eq!(tf & PE, PE, "{label}");

            assert!(
                ulp_diff_f64(decode_f64(&s.0), ref_s) <= 2,
                "fsin({label}): actual={:#018x} ref={:#018x}",
                decode_f64(&s.0).to_bits(),
                ref_s_bits
            );
            assert!(
                ulp_diff_f64(decode_f64(&c.0), ref_c) <= 2,
                "fcos({label}): actual={:#018x} ref={:#018x}",
                decode_f64(&c.0).to_bits(),
                ref_c_bits
            );
            assert_eq!(sc_s, s, "fsincos sin mismatch on {label}");
            assert_eq!(sc_c, c, "fsincos cos mismatch on {label}");
            assert!(
                ulp_diff_f64(decode_f64(&t.0), ref_t) <= 2,
                "fptan({label}): actual={:#018x} ref={:#018x}",
                decode_f64(&t.0).to_bits(),
                ref_t_bits
            );
        }
    }

    // spec: A64F-FP-10
    #[test]
    fn rounding_direction_c1_matches_extended_precision_across_all_transcendentals() {
        // FPATAN(1.0, 1.0) == pi/4 must match FPATAN(+Inf, +Inf) in result, flags, and C1.
        for rc in 0..4u16 {
            let cw = 0x37f | (rc << 10);
            let finite = atan2(ONE, ONE, cw);
            let inf = atan2(infinity(false), infinity(false), cw);
            assert_eq!(finite, inf, "fpatan(1, 1) vs fpatan(+inf, +inf) rc={rc}");
        }
        assert_eq!(
            atan2(ONE, ONE, 0x37f),
            (Ext::from_parts(0xc90f_daa2_2168_c235, 0x3ffe), PE, true)
        );

        // Verify C1 rounding direction in RN and all directed modes against 113-bit SoftFloat
        // reference across every transcendental, confirming both C1=0 and C1=1 occur under RN.
        let mut sin_rn = [false; 2];
        let mut cos_rn = [false; 2];
        let mut sincos_rn = [false; 2];
        let mut tan_rn = [false; 2];
        let mut atan_rn = [false; 2];
        let mut exp2_rn = [false; 2];
        let mut yl2x_rn = [false; 2];
        let mut yl2xp1_rn = [false; 2];

        for rc in 0..4u16 {
            let cw = 0x37f | (rc << 10);
            for &x in &[
                -1.7_f64, -0.7, -0.3, -0.1, 0.1, 0.25, 0.3, 0.5, 0.7, 1.0, 1.3, 1.5, 2.0,
            ] {
                let xe = Ext(encode_f64(x));
                start();
                let (rs, rc_q) = sin_cos_quad(xe);
                let (ref_s, _, ref_su) = finish(rs, cw, PE);
                let (ref_c, _, ref_cu) = finish(rc_q, cw, PE);
                let (ref_t, _, ref_tu) = finish(rs.div(rc_q), cw, PE);

                let (s, _, su) = sin(xe, cw).unwrap();
                let (c, _, cu) = cos(xe, cw).unwrap();
                let ((sc_s, sc_c), _, scu) = sin_cos(xe, cw).unwrap();
                let (t, _, tu) = tan(xe, cw).unwrap();
                assert_eq!((s, su), (ref_s, ref_su), "fsin({x}) rc={rc}");
                assert_eq!((c, cu), (ref_c, ref_cu), "fcos({x}) rc={rc}");
                assert_eq!(
                    (sc_s, sc_c, scu),
                    (ref_s, ref_c, ref_cu),
                    "fsincos({x}) rc={rc}"
                );
                assert_eq!((t, tu), (ref_t, ref_tu), "fptan({x}) rc={rc}");
                if rc == 0 {
                    sin_rn[usize::from(su)] = true;
                    cos_rn[usize::from(cu)] = true;
                    sincos_rn[usize::from(scu)] = true;
                    tan_rn[usize::from(tu)] = true;
                }
            }

            for &(y, x) in &[
                (1.0_f64, 1.0),
                (1.0, 2.0),
                (2.0, 1.0),
                (-1.0, 3.0),
                (3.0, -2.0),
                (-0.3, -0.7),
            ] {
                let ye = Ext(encode_f64(y));
                let xe = Ext(encode_f64(x));
                start();
                let ax = Q::from_ext(xe).abs();
                let ay = Q::from_ext(ye).abs();
                let pi2 = Q::PI.div(Q::integer(2));
                let mut rq = if ax.lt(ay) {
                    pi2.sub(ref_atan_unit(ax.div(ay)))
                } else {
                    ref_atan_unit(ay.div(ax))
                };
                if negative(xe) {
                    rq = Q::PI.sub(rq);
                }
                if negative(ye) {
                    rq = rq.neg();
                }
                let (ref_a, _, ref_au) = finish(rq, cw, PE);
                let (a, _, au) = atan2(ye, xe, cw);
                assert_eq!((a, au), (ref_a, ref_au), "fpatan({y}, {x}) rc={rc}");
                if rc == 0 {
                    atan_rn[usize::from(au)] = true;
                }
            }

            for &x in &[-0.9_f64, -0.5, -0.25, -0.1, 0.1, 0.25, 0.5, 0.75, 0.9] {
                let xe = Ext(encode_f64(x));
                start();
                let t = Q::from_ext(xe).mul(Q::LN2);
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
                let (ref_e, _, ref_eu) = finish(sum, cw, PE);
                let (e, _, eu) = exp2_minus_one(xe, cw).unwrap();
                assert_eq!((e, eu), (ref_e, ref_eu), "f2xm1({x}) rc={rc}");
                if rc == 0 {
                    exp2_rn[usize::from(eu)] = true;
                }
            }

            for &x in &[0.3_f64, 0.75, 1.25, 1.5, 3.0, 7.0, 10.0] {
                let xe = Ext(encode_f64(x));
                let ye = Ext(encode_f64(1.3));
                start();
                let log = ref_log2_positive(Q::from_ext(xe));
                let (ref_l, _, ref_lu) = finish(Q::from_ext(ye).mul(log), cw, PE);
                let (l, _, lu) = log_product(xe, ye, false, cw);
                assert_eq!((l, lu), (ref_l, ref_lu), "fyl2x({x}) rc={rc}");
                if rc == 0 {
                    yl2x_rn[usize::from(lu)] = true;
                }
            }

            for &x in &[-0.7_f64, -0.25, -0.1, 0.1, 0.25, 0.5, 2.0, 5.0] {
                let xe = Ext(encode_f64(x));
                let ye = Ext(encode_f64(1.3));
                start();
                let a = Q::from_ext(xe);
                let log = if a.abs().lt(Q([0, 0x3ffe_0000_0000_0000])) {
                    ref_logarithm_series(a.div(Q::integer(2).add(a)))
                } else {
                    ref_log2_positive(a.add(Q::ONE))
                };
                let (ref_l, _, ref_lu) = finish(Q::from_ext(ye).mul(log), cw, PE);
                let (l, _, lu) = log_product(xe, ye, true, cw);
                assert_eq!((l, lu), (ref_l, ref_lu), "fyl2xp1({x}) rc={rc}");
                if rc == 0 {
                    yl2xp1_rn[usize::from(lu)] = true;
                }
            }
        }

        assert_eq!(
            sin_rn,
            [true, true],
            "fsin must produce both C1=0 and C1=1 in RN"
        );
        assert_eq!(
            cos_rn,
            [true, true],
            "fcos must produce both C1=0 and C1=1 in RN"
        );
        assert_eq!(
            sincos_rn,
            [true, true],
            "fsincos must produce both C1=0 and C1=1 in RN"
        );
        assert_eq!(
            tan_rn,
            [true, true],
            "fptan must produce both C1=0 and C1=1 in RN"
        );
        assert_eq!(
            atan_rn,
            [true, true],
            "fpatan must produce both C1=0 and C1=1 in RN"
        );
        assert_eq!(
            exp2_rn,
            [true, true],
            "f2xm1 must produce both C1=0 and C1=1 in RN"
        );
        assert_eq!(
            yl2x_rn,
            [true, true],
            "fyl2x must produce both C1=0 and C1=1 in RN"
        );
        assert_eq!(
            yl2xp1_rn,
            [true, true],
            "fyl2xp1 must produce both C1=0 and C1=1 in RN"
        );
    }
}
