//! Software x87 transcendental algorithms using 113-bit SoftFloat intermediates.
//! These are mathematical approximations rounded to extended80, not a promise of
//! bit identity with every Intel/AMD microcode implementation (SDM Vol. 1, 8.3.10).
use super::softfloat::{self as sf, Quad as Q};
use crate::X87Register as Ext;

const INDEFINITE: Ext = Ext([0, 0, 0, 0, 0, 0, 0, 0xc0, 0xff, 0xff]);
fn infinity(negative: bool) -> Ext {
    Ext::from_parts(1 << 63, 0x7fff | if negative { 0x8000 } else { 0 })
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
fn finish(value: Q, control: u16, inherent: u16) -> (Ext, u16) {
    sf::configure(control | 0x300);
    let out = value.to_ext();
    (out, sf::flags() | inherent)
}
fn propagate(a: Ext, b: Ext, control: u16) -> (Ext, u16) {
    sf::configure(control | 0x300);
    let result = sf::binary(a, b, 0);
    (result, sf::flags())
}

/// ln(m) = 2 atanh((m-1)/(m+1)); callers reduce m to [1,2].
/// At |t|<=1/3, 48 terms leave <2^-150 truncation error.
fn logarithm_series(t: Q) -> Q {
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
fn log2_positive(mut x: Q) -> Q {
    // Normalize the binary128 encoding, including subnormal inputs. Multiplying
    // by 2^112 is exact and puts every nonzero subnormal into the normal range.
    let mut correction = 0;
    if x.0[1] & 0x7fff_0000_0000_0000 == 0 {
        x = x.mul(Q([0, 0x406f_0000_0000_0000]));
        correction = -112;
    }
    let exponent = ((x.0[1] >> 48) & 0x7fff) as i32 - 0x3fff + correction;
    x.0[1] = (x.0[1] & 0x0000_ffff_ffff_ffff) | 0x3fff_0000_0000_0000;
    let t = x.sub(Q::ONE).div(x.add(Q::ONE));
    logarithm_series(t).add(Q::integer(exponent))
}
pub(super) fn log_product(x: Ext, y: Ext, plus_one: bool, control: u16) -> (Ext, u16) {
    if nan(x) || nan(y) {
        return propagate(x, y, control);
    }
    start();
    let a = Q::from_ext(x);
    let target = if plus_one { a.add(Q::ONE) } else { a };
    if target.lt(Q::ZERO) {
        return (INDEFINITE, 1);
    }
    let log = if target.abs() == Q::ZERO {
        Q::from_ext(infinity(true))
    } else if infinite(x) {
        Q::from_ext(infinity(false))
    } else if plus_one && a.abs().lt(Q([0, 0x3ffe_0000_0000_0000])) {
        // x/(2+x) avoids losing tiny x to the addition 1+x.
        logarithm_series(a.div(Q::integer(2).add(a)))
    } else {
        log2_positive(target)
    };
    let result = Q::from_ext(y).mul(log);
    let mut flags = sf::flags() & 5;
    if target.abs() == Q::ZERO && !zero(y) {
        flags |= 4;
    }
    if !infinite(x) && !infinite(y) && !zero(y) && target.abs() != Q::ZERO && target != Q::ONE {
        flags |= 0x20;
    }
    finish(result, control, flags)
}
pub(super) fn exp2_minus_one(x: Ext, control: u16) -> Option<(Ext, u16)> {
    if nan(x) {
        return Some(propagate(x, x, control));
    }
    start();
    let a = Q::from_ext(x);
    if Q::ONE.lt(a.abs()) {
        return None;
    } // Architectural result is undefined outside [-1,1].
    if zero(x) {
        return Some((x, 0));
    }
    if a == Q::ONE {
        return Some((Ext::from_parts(1 << 63, 0x3fff), 0x20));
    }
    if a == Q::ONE.neg() {
        return Some((Ext::from_parts(1 << 63, 0xbffe), 0x20));
    }
    // expm1 has no subtraction cancellation, including subnormal x.
    let t = a.mul(Q::LN2);
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
    Some(finish(sum, control, 0x20))
}
pub(super) fn scale(a: Ext, b: Ext, control: u16) -> (Ext, u16) {
    if nan(a) || nan(b) {
        return propagate(a, b, control);
    }
    if infinite(b) {
        if zero(a) && !negative(b) || infinite(a) && negative(b) {
            return (INDEFINITE, 1);
        }
        return if negative(b) {
            (Ext::from_parts(0, a.sign_exponent() & 0x8000), 0)
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

/// Range reduction uses binary128 pi. The architecture permits |x|<2^63;
/// callers set C2 without modifying the stack outside that range.
pub(super) fn sin_cos(x: Ext, control: u16) -> Option<((Ext, Ext), u16)> {
    if nan(x) {
        let (v, f) = propagate(x, x, control);
        return Some(((v, v), f));
    }
    if infinite(x) {
        return Some(((INDEFINITE, INDEFINITE), 1));
    }
    if x.sign_exponent() & 0x7fff >= 0x403e {
        return None;
    }
    if zero(x) {
        return Some(((x, Ext::from_parts(1 << 63, 0x3fff)), 0));
    }
    start();
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
    let (sine, cosine) = match quadrant {
        0 => (sine, cosine),
        1 => (cosine, sine.neg()),
        2 => (sine.neg(), cosine.neg()),
        _ => (cosine.neg(), sine),
    };
    let (s, sf) = finish(sine, control, 0x20);
    let (c, cf) = finish(cosine, control, 0x20);
    Some(((s, c), sf | cf))
}
fn atan_unit(a: Q) -> Q {
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
pub(super) fn atan2(y: Ext, x: Ext, control: u16) -> (Ext, u16) {
    if nan(x) || nan(y) {
        return propagate(y, x, control);
    }
    start();
    let ax = Q::from_ext(x).abs();
    let ay = Q::from_ext(y).abs();
    let pi2 = Q::PI.div(Q::integer(2));
    let mut result = if zero(y) {
        Q::ZERO
    } else if zero(x) {
        pi2
    } else if infinite(x) && infinite(y) {
        Q::PI.div(Q::integer(4))
    } else if infinite(y) {
        pi2
    } else if infinite(x) {
        Q::ZERO
    } else if ax.lt(ay) {
        pi2.sub(atan_unit(ax.div(ay)))
    } else {
        atan_unit(ay.div(ax))
    };
    if negative(x) {
        result = Q::PI.sub(result);
    }
    if negative(y) {
        result = result.neg();
    }
    finish(
        result,
        control,
        if zero(y) && !negative(x) { 0 } else { 0x20 },
    )
}
