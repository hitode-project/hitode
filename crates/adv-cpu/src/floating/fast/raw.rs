//! Guarded operations on the full extended significand. No intermediate host
//! float rounding: copies/comparisons retain all bits, stores round once to
//! their destination. Exceptional encodings and exponent boundaries fall back.
use super::*;
use std::cmp::Ordering;

/// A normal finite extended value or signed zero. The transaction owns pseudo
/// encodings, denormals, infinities and NaNs, including their exception rules.
pub(super) fn ordinary(value: Ext) -> Option<Ext> {
    let exp = value.sign_exponent() & 0x7fff;
    let sig = value.significand();
    ((exp == 0 && sig == 0) || (exp != 0 && exp != 0x7fff && sig >> 63 != 0)).then_some(value)
}

pub(super) fn operand(state: &CpuState, index: u8) -> Option<Ext> {
    let slot = state.x87.physical_index(usize::from(index));
    if state.x87.physical_tag(slot) == 3 {
        return None;
    }
    ordinary(state.x87.regs[slot])
}

pub(super) fn source(
    source: Source,
    i: &Instruction,
    state: &CpuState,
    memory: &Memory,
) -> Option<(Ext, Option<u32>)> {
    if let Source::St(index) = source {
        Some((operand(state, index)?, None))
    } else {
        let (value, at) = load(source, i, state, memory, i32::MAX)?;
        Some((extended(value), at))
    }
}

pub(super) fn compare(a: Ext, b: Ext) -> Ordering {
    if a.significand() == 0 && b.significand() == 0 {
        return Ordering::Equal;
    }
    let a_sign = a.sign_exponent() >> 15;
    let b_sign = b.sign_exponent() >> 15;
    if a_sign != b_sign {
        return b_sign.cmp(&a_sign);
    }
    let order = (a.sign_exponent() & 0x7fff, a.significand())
        .cmp(&(b.sign_exponent() & 0x7fff, b.significand()));
    if a_sign != 0 { order.reverse() } else { order }
}

/// Round an unsigned magnitude right by `shift`, retaining enough information
/// to distinguish both sides of a tie even for a sub-unit integer conversion.
fn round(sig: u64, shift: u32, negative: bool, rc: u16) -> (u64, bool, bool) {
    if shift == 0 {
        return (sig, true, false);
    }
    let (whole, tail, half) = if shift < 64 {
        (sig >> shift, sig & ((1 << shift) - 1), 1 << (shift - 1))
    } else if shift == 64 {
        (0, sig, 1 << 63)
    } else {
        // The nonzero value is strictly below 1/2; only directed rounding can
        // increment its zero integer part. Never perform an oversized shift.
        return (
            u64::from(sig != 0 && (rc == 1 && negative || rc == 2 && !negative)),
            sig == 0,
            sig != 0 && (rc == 1 && negative || rc == 2 && !negative),
        );
    };
    let up = tail != 0
        && match rc {
            0 => tail > half || tail == half && whole & 1 != 0,
            1 => negative,
            2 => !negative,
            _ => false,
        };
    (whole + u64::from(up), tail == 0, up)
}

/// Result bits, exactness and magnitude increment. Precision control never
/// affects stores. PE/C1 are committed by the caller after checking its mask.
pub(super) fn store_bits(
    value: Ext,
    width: u8,
    integer: bool,
    control: u16,
) -> Option<(u64, bool, bool)> {
    ordinary(value)?;
    let sig = value.significand();
    let negative = value.sign_exponent() & 0x8000 != 0;
    let exponent = i32::from(value.sign_exponent() & 0x7fff) - 0x3fff;
    let rc = control >> 10 & 3;
    if integer {
        if sig == 0 {
            return Some((0, true, false));
        }
        if exponent > 63 {
            return None;
        }
        let (magnitude, exact, up) = round(sig, (63 - exponent) as u32, negative, rc);
        let limit = 1u64 << (width * 8 - 1);
        if magnitude > limit || magnitude == limit && !negative {
            return None;
        }
        Some((
            if negative {
                magnitude.wrapping_neg()
            } else {
                magnitude
            },
            exact,
            up,
        ))
    } else {
        let (precision, bias) = if width == 8 { (53, 1023) } else { (24, 127) };
        let sign = u64::from(negative) << (width * 8 - 1);
        if sig == 0 {
            return Some((sign, true, false));
        }
        // Keeping both input and output normal excludes underflow, overflow
        // and tininess-before/after-rounding differences from this fast lane.
        if !(1 - bias..=bias).contains(&exponent) {
            return None;
        }
        let (rounded, exact, up) = round(sig, 64 - precision, negative, rc);
        let carry = rounded >> precision;
        let exponent = exponent + bias + carry as i32;
        if exponent > 2 * bias {
            return None;
        }
        let fraction = (rounded >> carry) & ((1 << (precision - 1)) - 1);
        Some((
            sign | (exponent as u64) << (precision - 1) | fraction,
            exact,
            up,
        ))
    }
}
