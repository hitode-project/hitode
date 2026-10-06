//! D3DX Effect's bounded CPU constant-expression evaluator.
//! Opcode/register encodings are format facts checked against authored fxc FXO.
//! Calculations use f64 operands and round the temporary/output tables to f32,
//! as the native Effect runtime does; this is separate from the GPU shader IR.
use super::effect::{Constant, constants};
use std::collections::BTreeMap;
const LIMIT: usize = 16_384;
// Constant register set, register index, and packed component values.
type RegisterWrite = (u32, u32, Vec<u32>);
#[derive(Clone, Copy)]
struct Operand {
    table: usize,
    offset: usize,
}
pub(super) fn comments(words: &[u32]) -> Result<Vec<(u32, &[u32])>, &'static str> {
    let mut at = 1;
    let mut result = Vec::new();
    while at < words.len() {
        let token = words[at];
        if token & 0xffff == 0xffff {
            break;
        }
        if token & 0xffff == 0xfffe {
            let n = ((token >> 16) & 0x7fff) as usize;
            let data = words
                .get(at + 1..at + 1 + n)
                .ok_or("Effect expression comment bounds")?;
            if let Some(tag) = data.first() {
                result.push((*tag, &data[1..]));
            }
            at += 1 + n;
        } else {
            let n = ((token >> 24) & 15) as usize;
            at += 1 + n;
        }
    }
    Ok(result)
}
fn operand(words: &[u32], at: &mut usize) -> Result<Operand, &'static str> {
    let p = words
        .get(*at..*at + 3)
        .ok_or("Effect expression operand bounds")?;
    if p[0] != 0 {
        return Err("Effect expression relative operands are unavailable");
    }
    if !(1..=7).contains(&p[1]) || p[2] as usize >= LIMIT {
        return Err("Effect expression register limit");
    }
    *at += 3;
    Ok(Operand {
        table: p[1] as usize,
        offset: if p[1] == 5 {
            p[2] as usize / 4
        } else {
            p[2] as usize
        },
    })
}
pub(super) fn evaluate(
    bytes: &[u8],
    mut parameter: impl FnMut(&Constant) -> Option<Vec<u32>>,
) -> Result<Vec<RegisterWrite>, &'static str> {
    if !bytes.len().is_multiple_of(4) {
        return Err("Effect expression bytecode alignment");
    }
    let words: Vec<_> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| u32::from_le_bytes(*b))
        .collect();
    if words.first().is_none_or(|v| v & 0xffff0000 != 0x46580000) {
        return Err("Effect expression signature");
    }
    let sections = comments(&words)?;
    let mut registers: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    if let Some((_, literals)) = sections.iter().find(|(tag, _)| *tag == 0x54494c43) {
        let n = *literals.first().ok_or("Effect expression literal header")? as usize;
        if n > LIMIT || literals.len() < 1 + n * 2 {
            return Err("Effect expression literal limit");
        }
        for i in 0..n {
            registers.insert(
                (1, i),
                f64::from_bits(
                    u64::from(literals[1 + i * 2]) | (u64::from(literals[2 + i * 2]) << 32),
                ),
            );
        }
    }
    for constant in constants(bytes)? {
        if let Some(data) = parameter(&constant) {
            let table = match constant.set {
                0 => 5,
                1 => 6,
                2 => 2,
                _ => return Err("Effect expression sampler input"),
            };
            let base = constant.index as usize * if table == 5 { 1 } else { 4 };
            if base.checked_add(data.len()).is_none_or(|n| n > LIMIT) {
                return Err("Effect expression input limit");
            }
            for (i, bits) in data.into_iter().enumerate() {
                let value = match table {
                    5 => f64::from(bits != 0),
                    6 => (bits as i32) as f64,
                    _ => f32::from_bits(bits) as f64,
                };
                registers.insert((table, base + i), value);
            }
        }
    }
    let Some((_, code)) = sections.iter().find(|(tag, _)| *tag == 0x434c5846) else {
        return Ok(Vec::new());
    };
    let count = *code.first().ok_or("Effect expression instruction header")? as usize;
    if count > 65_536 {
        return Err("Effect expression instruction limit");
    }
    let mut at = 1;
    let mut output: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
    for _ in 0..count {
        let op = *code.get(at).ok_or("Effect expression instruction bounds")?;
        let inputs = *code
            .get(at + 1)
            .ok_or("Effect expression instruction bounds")? as usize;
        at += 2;
        let components = (op & 0xffff) as usize;
        let opcode = (op >> 20) & 0x7ff;
        let scalar = op & 0x80000000 != 0;
        let expected = match opcode {
            0 => 0,
            0x100 | 0x101 | 0x103..=0x10c => 1,
            0x200..=0x206 | 0x208 | 0x500 => 2,
            0x300 => 3,
            0x70e if inputs == 6 || inputs == 8 => inputs,
            _ => return Err("Effect expression opcode is unavailable"),
        };
        if inputs != expected || !(1..=4).contains(&components) {
            return Err("Effect expression instruction shape");
        }
        let args = (0..inputs)
            .map(|_| operand(code, &mut at))
            .collect::<Result<Vec<_>, _>>()?;
        let dest = operand(code, &mut at)?;
        if !matches!(dest.table, 4..=7) {
            return Err("Effect expression destination table");
        }
        let get = |i: usize, component: usize| {
            *registers
                .get(&(
                    args[i].table,
                    args[i].offset + if scalar && i == 0 { 0 } else { component },
                ))
                .unwrap_or(&0.0)
        };
        let mut values = Vec::new();
        for component in 0..if opcode == 0x500 { 1 } else { components } {
            let a = if inputs > 0 { get(0, component) } else { 0.0 };
            let b = if inputs > 1 { get(1, component) } else { 0.0 };
            let v = match opcode {
                0 => 0.0,
                0x100 => a,
                0x101 => -a,
                0x103 => 1.0 / a,
                0x104 => a - a.floor(),
                0x105 => a.exp2(),
                0x106 => {
                    if a == 0.0 {
                        0.0
                    } else {
                        a.abs().log2()
                    }
                }
                0x107 => 1.0 / a.abs().sqrt(),
                0x108 => a.sin(),
                0x109 => a.cos(),
                0x10a => a.asin(),
                0x10b => a.acos(),
                0x10c => a.atan(),
                0x200 => a.min(b),
                0x201 => a.max(b),
                0x202 => f64::from(a < b),
                0x203 => f64::from(a >= b),
                0x204 => a + b,
                0x205 => a * b,
                0x206 => a.atan2(b),
                0x208 => 0.0,
                0x300 => {
                    if a >= 0.0 {
                        b
                    } else {
                        get(2, component)
                    }
                }
                0x500 => (0..components).map(|j| get(0, j) * get(1, j)).sum(),
                0x70e => (0..inputs / 2)
                    .map(|j| get(j, component) * get(j + inputs / 2, component))
                    .sum(),
                _ => unreachable!(),
            };
            values.push(v);
        }
        for (component, value) in values.into_iter().enumerate() {
            let offset = dest.offset + component;
            if offset >= LIMIT {
                return Err("Effect expression output limit");
            }
            let (bits, rounded) = match dest.table {
                5 => (u32::from(value != 0.0), f64::from(value != 0.0)),
                6 => {
                    let v = value.round_ties_even() as i32;
                    (v as u32, v as f64)
                }
                _ => ((value as f32).to_bits(), (value as f32) as f64),
            };
            registers.insert((dest.table, offset), rounded);
            if dest.table != 7 {
                let (set, stride) = match dest.table {
                    4 => (2, 4),
                    5 => (0, 1),
                    6 => (1, 4),
                    _ => unreachable!(),
                };
                let reg = offset / stride;
                let row = output
                    .entry((set, reg as u32))
                    .or_insert_with(|| vec![0; stride]);
                row[offset % stride] = bits;
            }
        }
    }
    Ok(output
        .into_iter()
        .map(|((set, index), values)| (set, index, values))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn expression(code: &[u32], literals: &[f64]) -> Vec<u8> {
        let mut words = vec![
            0x46580200,
            ((literals.len() * 2 + 2) as u32) << 16 | 0xfffe,
            0x54494c43,
            literals.len() as u32,
        ];
        for value in literals {
            let bits = value.to_bits();
            words.push(bits as u32);
            words.push((bits >> 32) as u32);
        }
        words.push(((code.len() + 1) as u32) << 16 | 0xfffe);
        words.push(0x434c5846);
        words.extend_from_slice(code);
        words.push(0xffff);
        bytemuck::cast_slice(&words).to_vec()
    }
    #[test]
    fn reciprocal_multiply_and_move_use_native_table_precision() {
        let data = expression(
            &[
                3, 0x10300001, 1, 0, 1, 0, 0, 7, 0, 0x20500001, 2, 0, 7, 0, 0, 1, 1, 0, 7, 1,
                0x10000001, 1, 0, 7, 1, 0, 4, 4,
            ],
            &[3.0, 2.0],
        );
        let values = evaluate(&data, |_| None).unwrap();
        assert_eq!(
            values,
            vec![(2, 1, vec![((1.0f32 / 3.0) * 2.0).to_bits(), 0, 0, 0])]
        );
    }
    #[test]
    fn all_truncated_expression_prefixes_and_unknown_opcodes_fail_bounded() {
        let data = expression(&[1, 0x10000001, 1, 0, 1, 0, 0, 4, 0], &[1.0]);
        for len in 0..data.len() {
            let _ = evaluate(&data[..len], |_| None);
        }
        let data = expression(&[1, 0x7ff00001, 1, 0, 1, 0, 0, 4, 0], &[1.0]);
        assert!(evaluate(&data, |_| None).is_err());
    }
}
