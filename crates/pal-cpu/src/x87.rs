//! Conversion between IEEE binary64 and the x87 80-bit register representation.

/// Encode a double exactly, including subnormals and signed zero.
pub fn encode_f64(value: f64) -> [u8; 10] {
    let bits = value.to_bits();
    let sign = ((bits >> 48) & 0x8000) as u16;
    let exponent = ((bits >> 52) & 0x7ff) as u16;
    let fraction = bits & ((1u64 << 52) - 1);
    let (mantissa, exponent) = match exponent {
        0 if fraction == 0 => (0, 0),
        0 => {
            let leading = 63 - fraction.leading_zeros();
            (fraction << (63 - leading), (16383 - 1074 + leading) as u16)
        }
        0x7ff => ((1u64 << 63) | (fraction << 11), 0x7fff),
        _ => ((1u64 << 63) | (fraction << 11), exponent + 16383 - 1023),
    };
    let mut out = [0; 10];
    out[..8].copy_from_slice(&mantissa.to_le_bytes());
    out[8..].copy_from_slice(&(exponent | sign).to_le_bytes());
    out
}

fn round_shift(value: u64, shift: u32) -> u64 {
    match shift {
        0 => value,
        1..=63 => {
            let result = value >> shift;
            let remainder = value & ((1u64 << shift) - 1);
            let halfway = 1u64 << (shift - 1);
            result + u64::from(remainder > halfway || (remainder == halfway && result & 1 != 0))
        }
        64 => u64::from(value > 1u64 << 63),
        _ => 0,
    }
}

/// Decode an x87 register with round-to-nearest, ties-to-even binary64 rounding.
/// This is a host representation conversion; it does not change guest FP status flags.
pub fn decode_f64(bytes: &[u8]) -> f64 {
    assert!(bytes.len() >= 10, "x87 register requires ten bytes");
    let mantissa = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    let sign_exp = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
    let sign = u64::from(sign_exp & 0x8000) << 48;
    let exponent = sign_exp & 0x7fff;
    if exponent == 0x7fff {
        let fraction = mantissa & !(1u64 << 63);
        let payload = if fraction == 0 { 0 } else { (fraction >> 11) | (1u64 << 51) };
        return f64::from_bits(sign | (0x7ffu64 << 52) | payload);
    }
    // An unnormal x87 encoding is not a number. Pseudo-denormals (E=0,J=1) have
    // the same numeric value as E=1 and are accepted, as on x87 hardware.
    if exponent != 0 && mantissa & (1u64 << 63) == 0 {
        return f64::from_bits(sign | 0x7ff8_0000_0000_0000);
    }
    if mantissa == 0 {
        return f64::from_bits(sign);
    }
    let power = i32::from(exponent.max(1)) - 16383;
    let leading = 63 - mantissa.leading_zeros();
    let mut binary_exp = power - 63 + leading as i32;
    if binary_exp < -1022 {
        let shift = power + 1011;
        let fraction = if shift >= 0 { mantissa << shift } else { round_shift(mantissa, (-shift) as u32) };
        // Rounding may promote the largest subnormal to the smallest normal.
        return f64::from_bits(sign | fraction);
    }
    let mut significand = if leading > 52 { round_shift(mantissa, leading - 52) } else { mantissa << (52 - leading) };
    if significand == 1u64 << 53 {
        significand >>= 1;
        binary_exp += 1;
    }
    if binary_exp > 1023 {
        return f64::from_bits(sign | (0x7ffu64 << 52));
    }
    f64::from_bits(sign | (((binary_exp + 1023) as u64) << 52) | (significand & ((1u64 << 52) - 1)))
}

/// The two-bit x87 tag for a value produced by [`encode_f64`].
pub fn tag_f64(value: f64) -> u64 {
    if value == 0.0 {
        1
    } else if !value.is_finite() {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_round_trip_without_reducing_to_single_precision() {
        for bits in [
            0,
            1u64 << 63,
            1,
            (1u64 << 52) - 1,
            1u64 << 52,
            0x3ff0_0000_0000_0001,
            0x4009_21fb_5444_2d18,
            0x7fef_ffff_ffff_ffff,
            0x7ff0_0000_0000_0000,
            0xfff0_0000_0000_0000,
            0x7ff8_1234_5678_9abc,
            0xfff8_1234_5678_9abc,
        ] {
            let value = f64::from_bits(bits);
            assert_eq!(decode_f64(&encode_f64(value)).to_bits(), bits);
        }
    }

    #[test]
    fn extended_precision_rounds_ties_even_and_handles_underflow_overflow() {
        let extended = |mantissa: u64, exponent: u16| {
            let mut bytes = [0; 10];
            bytes[..8].copy_from_slice(&mantissa.to_le_bytes());
            bytes[8..].copy_from_slice(&exponent.to_le_bytes());
            bytes
        };
        assert_eq!(decode_f64(&extended((1u64 << 63) + (1u64 << 10), 16383)).to_bits(), 1f64.to_bits());
        assert_eq!(decode_f64(&extended((1u64 << 63) + 3 * (1u64 << 10), 16383)).to_bits(), 1f64.to_bits() + 2);
        assert_eq!(decode_f64(&extended(1u64 << 63, 16383 - 1075)).to_bits(), 0);
        assert_eq!(decode_f64(&extended((1u64 << 63) + 1, 16383 - 1075)).to_bits(), 1);
        assert_eq!(decode_f64(&extended(u64::MAX, 16383 + 1023)), f64::INFINITY);
    }
}
