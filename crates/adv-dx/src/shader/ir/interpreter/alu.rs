use super::super::*;

pub(super) fn flush(value: u32) -> u32 {
    if value & 0x7f80_0000 == 0 {
        value & 0x8000_0000
    } else {
        value
    }
}

pub(super) fn saturate(value: u32) -> u32 {
    let value = f32::from_bits(flush(value));
    if value > 0.0 {
        value.min(1.0).to_bits()
    } else {
        0
    }
}

pub(super) fn modify(value: u32, modifier: Modifier, ty: ValueType, denormals: bool) -> u32 {
    let flush = |value| if denormals { flush(value) } else { value };
    match (modifier, ty) {
        (Modifier::None, _) => value,
        (Modifier::Negate, ValueType::Int | ValueType::Uint) => value.wrapping_neg(),
        (Modifier::Absolute, ValueType::Int | ValueType::Uint) => {
            (value as i32).wrapping_abs() as u32
        }
        (Modifier::AbsoluteNegate, ValueType::Int | ValueType::Uint) => {
            (value as i32).wrapping_abs().wrapping_neg() as u32
        }
        (Modifier::Negate, _) => flush(value) ^ 0x8000_0000,
        (Modifier::Absolute, _) => flush(value) & 0x7fff_ffff,
        (Modifier::AbsoluteNegate, _) => flush(value) | 0x8000_0000,
    }
}

fn to_half(bits: u32) -> u32 {
    let sign = bits >> 16 & 0x8000;
    let mantissa = bits & 0x007f_ffff;
    let exponent = (bits >> 23) & 255;
    sign | match exponent {
        255 if mantissa != 0 => 0x7e00 | (mantissa >> 13),
        255 => 0x7c00,
        143..=254 => 0x7bff,
        113..=142 => ((exponent - 112) << 10) | (mantissa >> 13),
        103..=112 => (mantissa | 0x0080_0000) >> (126 - exponent),
        _ => 0,
    }
}

fn from_half(bits: u32) -> u32 {
    let sign = (bits & 0x8000) << 16;
    let mantissa = bits & 1023;
    sign | match bits >> 10 & 31 {
        0 if mantissa == 0 => 0,
        0 => {
            let shift = mantissa.leading_zeros() - 21;
            ((113 - shift) << 23) | (((mantissa << shift) & 1023) << 13)
        }
        31 => 0x7f80_0000 | mantissa << 13,
        exponent => ((exponent + 112) << 23) | mantissa << 13,
    }
}

pub(super) fn evaluate(op: &Instruction, arguments: &[Vector]) -> Result<[Vector; 2], Error> {
    let mut result = [[0; 4]; 2];
    for lane in 0..4 {
        let word = |i| arguments.get(i).map_or(0, |v: &Vector| v[lane]);
        let (a, b, c, d) = (word(0), word(1), word(2), word(3));
        let (x, y, z) = (f32::from_bits(a), f32::from_bits(b), f32::from_bits(c));
        let (i, j) = (a as i32, b as i32);
        let truth = |yes: bool| if yes { u32::MAX } else { 0 };
        let pair = |value: u64| [(value >> 32) as u32, value as u32];
        let one = |value| [value, 0];
        let fp = |value: f32| one(value.to_bits());
        use Opcode::*;
        let [first, second] = match op.opcode {
            Mov => one(a),
            Movc => one(if a == 0 { c } else { b }),
            Swapc => {
                if a == 0 {
                    [b, c]
                } else {
                    [c, b]
                }
            }
            Add => fp(x + y),
            Div => fp(x / y),
            Dp2 | Dp3 | Dp4 => {
                let count = if op.opcode == Dp2 {
                    2
                } else if op.opcode == Dp3 {
                    3
                } else {
                    4
                };
                let mut products = (0..count)
                    .map(|k| f32::from_bits(arguments[0][k]) * f32::from_bits(arguments[1][k]));
                fp(products.next().unwrap() + products.sum::<f32>())
            }
            Eq => one(truth(x == y)),
            Ne => one(truth(x != y)),
            Lt => one(truth(x < y)),
            Ge => one(truth(x >= y)),
            Exp => fp(x.exp2()),
            Frc => fp(x - x.floor()),
            Ftoi => one(x as i32 as u32),
            Ftou => one(x as u32),
            Log => fp(x.log2()),
            Mad => fp(x * y + z),
            Min => fp(x.min(y)),
            Max => fp(x.max(y)),
            Mul => fp(x * y),
            RoundNe => fp(x.round_ties_even()),
            RoundNi => fp(x.floor()),
            RoundPi => fp(x.ceil()),
            RoundZ => fp(x.trunc()),
            Rcp => fp(1.0 / x),
            Rsq => fp(1.0 / x.sqrt()),
            Sqrt => fp(x.sqrt()),
            Sincos => [x.sin().to_bits(), x.cos().to_bits()],
            F32ToF16Rtz => one(to_half(a)),
            F16ToF32 => one(from_half(a)),
            Iadd => one(a.wrapping_add(b)),
            Ieq => one(truth(a == b)),
            Ine => one(truth(a != b)),
            Ige => one(truth(i >= j)),
            Ilt => one(truth(i < j)),
            Imad | Umad => one(a.wrapping_mul(b).wrapping_add(c)),
            Imax => one(i.max(j) as u32),
            Imin => one(i.min(j) as u32),
            Imul => pair((i64::from(i) * i64::from(j)) as u64),
            Umul => pair(u64::from(a) * u64::from(b)),
            Ineg => one(a.wrapping_neg()),
            Ishl => one(a.wrapping_shl(b)),
            Ishr => one(i.wrapping_shr(b) as u32),
            Ushr => one(a.wrapping_shr(b)),
            Itof => fp(i as f32),
            Utof => fp(a as f32),
            And => one(a & b),
            Not => one(!a),
            Or => one(a | b),
            Xor => one(a ^ b),
            Udiv => a
                .checked_div(b)
                .map_or([u32::MAX; 2], |quotient| [quotient, a % b]),
            Ult => one(truth(a < b)),
            Uge => one(truth(a >= b)),
            Umax => one(a.max(b)),
            Umin => one(a.min(b)),
            Uaddc => {
                let (sum, carry) = a.overflowing_add(b);
                [sum, u32::from(carry)]
            }
            Usubb => {
                let (diff, borrow) = a.overflowing_sub(b);
                [diff, u32::from(borrow)]
            }
            Countbits => one(a.count_ones()),
            FirstbitHi => one(if a == 0 { u32::MAX } else { a.leading_zeros() }),
            FirstbitLo => one(if a == 0 { u32::MAX } else { a.trailing_zeros() }),
            FirstbitShi => {
                let bits = if i < 0 { !a } else { a };
                one(if bits == 0 {
                    u32::MAX
                } else {
                    bits.leading_zeros()
                })
            }
            Bfrev => one(a.reverse_bits()),
            Ubfe | Ibfe | Bfi => {
                let width = a & 31;
                let offset = b & 31;
                let mask = ((1u32 << width) - 1) << offset;
                let extracted = (c & mask) >> offset;
                let extended = if offset + width >= 32 {
                    (c as i32 >> offset) as u32
                } else if width != 0 {
                    ((extracted << (32 - width)) as i32 >> (32 - width)) as u32
                } else {
                    0
                };
                one(if op.opcode == Bfi {
                    (d & !mask) | ((c << offset) & mask)
                } else if op.opcode == Ibfe {
                    extended
                } else {
                    extracted
                })
            }
            Msad => {
                let sum = (0..4)
                    .map(|byte| {
                        let expected = a >> (8 * byte) & 255;
                        let actual = b >> (8 * byte) & 255;
                        if expected == 0 {
                            0
                        } else {
                            actual.abs_diff(expected)
                        }
                    })
                    .sum::<u32>();
                one(c.saturating_add(sum))
            }
            other => return Err(Error::UnsupportedOpcode(other)),
        };
        result[0][lane] = first;
        result[1][lane] = second;
    }
    Ok(result)
}
