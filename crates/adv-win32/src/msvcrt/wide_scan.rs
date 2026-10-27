//! Bounded wide scanf integer conversions through checked guest outputs.
use super::*;
use adv_abi::{
    Api, GuestPtr, ImplementationLevel, WideStr,
    guest::{check_range, memory_failure},
};
use adv_cpu::backend::Prot;

pub(super) fn register(registry: &mut Registry) {
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        registry.register(Api {
            module,
            name: "swscanf_s",
            conv: Conv::CdeclVariadic { fixed: 2 },
            handler: scan,
            level: ImplementationLevel::Implement,
        });
    }
}
fn space(c: u16) -> bool {
    matches!(c, 9..=13 | 32)
}
fn digit(c: u16) -> Option<u32> {
    match c {
        48..=57 => Some(u32::from(c - 48)),
        65..=70 => Some(u32::from(c - 65) + 10),
        97..=102 => Some(u32::from(c - 97) + 10),
        _ => None,
    }
}
fn unsupported(env: &mut Env<'_>) -> Outcome {
    adv_abi::warn_unavailable(
        env,
        "CRT swscanf_s",
        "wide scanner supports integer conversions, widths, suppression, literals and whitespace",
    );
    Outcome::Fail("unsupported wide scanf conversion".into())
}
fn scan(env: &mut Env<'_>) -> Outcome {
    if env.arg(0) == 0 || env.arg(1) == 0 {
        return invalid_parameter_result(env, "swscanf_s", 22, u32::MAX);
    }
    let (input, format) = match WideStr(GuestPtr::new(env.arg(0)))
        .read(env.cpu, 32768)
        .and_then(|input| {
            WideStr(GuestPtr::new(env.arg(1)))
                .read(env.cpu, 4096)
                .map(|format| (input, format))
        }) {
        Ok(value) => value,
        Err(error) => return memory_failure(error),
    };
    let (mut at, mut f, mut argument, mut assigned) = (0usize, 0usize, 2u32, 0u32);
    let mut converted = false;
    let legacy = env.sys.thunks.get(env.thunk).module == "msvcrt.dll";
    while f < format.len() {
        if space(format[f]) {
            while f < format.len() && space(format[f]) {
                f += 1;
            }
            while at < input.len() && space(input[at]) {
                at += 1;
            }
            continue;
        }
        if format[f] != 37 {
            if at == input.len() {
                return Outcome::Return(if !converted && !legacy {
                    u32::MAX
                } else {
                    assigned
                });
            }
            if input[at] != format[f] {
                break;
            }
            at += 1;
            f += 1;
            continue;
        }
        f += 1;
        if format.get(f) == Some(&37) {
            if input.get(at) != Some(&37) {
                break;
            }
            at += 1;
            f += 1;
            continue;
        }
        let suppressed = format.get(f) == Some(&42);
        if suppressed {
            f += 1;
        }
        let mut width = 0usize;
        while let Some(d) = format.get(f).and_then(|c| digit(*c)).filter(|d| *d < 10) {
            width = width.saturating_mul(10).saturating_add(d as usize);
            f += 1;
        }
        let mut bytes = 4u32;
        if format.get(f) == Some(&104) {
            bytes = 2;
            f += 1;
            if format.get(f) == Some(&104) {
                bytes = 1;
                f += 1;
            }
        } else if format.get(f) == Some(&108) {
            f += 1;
            if format.get(f) == Some(&108) {
                bytes = 8;
                f += 1;
            }
        } else if format.get(f..f + 3) == Some(&[73, 54, 52]) {
            bytes = 8;
            f += 3;
        }
        let Some(conversion) = format.get(f).copied() else {
            return unsupported(env);
        };
        f += 1;
        if !matches!(conversion, 100 | 105 | 117 | 120 | 88 | 111) {
            return unsupported(env);
        }
        while at < input.len() && space(input[at]) {
            at += 1;
        }
        if at == input.len() {
            return Outcome::Return(if !converted && !legacy {
                u32::MAX
            } else {
                assigned
            });
        }
        let end = at
            .saturating_add(if width == 0 { input.len() } else { width })
            .min(input.len());
        let mut cursor = at;
        let negative = input.get(cursor) == Some(&45);
        if cursor < end && matches!(input[cursor], 43 | 45) {
            cursor += 1;
        }
        let mut base = match conversion {
            120 | 88 => 16,
            111 => 8,
            _ => 10,
        };
        if conversion == 105 {
            base = if input.get(cursor) == Some(&48) {
                8
            } else {
                10
            };
        }
        if matches!(conversion, 105 | 120 | 88)
            && cursor + 1 < end
            && input[cursor] == 48
            && matches!(input[cursor + 1], 120 | 88)
        {
            base = 16;
            cursor += 2;
        }
        let start = cursor;
        let mut value = 0u64;
        while cursor < end {
            let Some(d) = digit(input[cursor]).filter(|d| *d < base) else {
                break;
            };
            let Some(next) = value
                .checked_mul(u64::from(base))
                .and_then(|v| v.checked_add(u64::from(d)))
            else {
                return unsupported(env);
            };
            value = next;
            cursor += 1;
        }
        if cursor == start {
            return Outcome::Return(if cursor == input.len() && !converted && !legacy {
                u32::MAX
            } else {
                assigned
            });
        }
        converted = true;
        at = cursor;
        if !suppressed {
            let pointer = env.arg(argument);
            argument += 1;
            if pointer == 0 {
                return invalid_parameter_result(env, "swscanf_s", 22, assigned);
            }
            if let Err(error) = check_range(env.cpu, pointer, bytes, Prot::WRITE) {
                return memory_failure(error);
            }
            if negative {
                value = value.wrapping_neg();
            }
            if let Err(error) = env
                .cpu
                .write(pointer, &value.to_le_bytes()[..bytes as usize])
            {
                return memory_failure(error);
            }
            assigned += 1;
        }
    }
    Outcome::Return(assigned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{JitCpu, backend::Guest};
    use adv_machine::{
        Clock, Machine,
        layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
    };
    #[test]
    fn native_wide_integer_scan_matches_counts_values_and_untouched_outputs() {
        let mut registry = Registry::default();
        register(&mut registry);
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        m.cpu.map(0x410000, 4096, Prot::RW).unwrap();
        for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
            let thunk =
                (m.resolve_import(module, "swscanf_s").unwrap() - THUNK_BASE) / THUNK_STRIDE;
            for (text, format, expected) in [
                ("Save001.bin", "Save%03d.bin", [1, 1, 0x12345, 0x12345]),
                ("2026/10/08", "%d/%d/%d", [3, 2026, 10, 8]),
                ("0xff -12 77", "%x %d %o", [3, 255, (-12i32) as u32, 63]),
                ("", "%d", [u32::MAX, 0x12345, 0x12345, 0x12345]),
                ("x", "%d", [0, 0x12345, 0x12345, 0x12345]),
                ("1", "%*d%d", [0, 0x12345, 0x12345, 0x12345]),
                ("1 x", "%*d%d", [0, 0x12345, 0x12345, 0x12345]),
                ("0x", "%x", [u32::MAX, 0x12345, 0x12345, 0x12345]),
                ("123", "%2d%d", [2, 12, 3, 0x12345]),
                ("槽位001", "槽位%3u", [1, 1, 0x12345, 0x12345]),
            ] {
                for (address, value) in [(0x410100, text), (0x410200, format)] {
                    let bytes: Vec<_> = value
                        .encode_utf16()
                        .chain([0])
                        .flat_map(u16::to_le_bytes)
                        .collect();
                    m.cpu.write(address, &bytes).unwrap();
                }
                m.cpu.set_reg(Reg::Esp, 0x410000);
                for (i, value) in [0, 0x410100, 0x410200, 0x410300, 0x410304, 0x410308]
                    .into_iter()
                    .enumerate()
                {
                    m.cpu.write_u32(0x410000 + i as u32 * 4, value).unwrap();
                }
                for i in 0..3 {
                    m.cpu.write_u32(0x410300 + i * 4, 0x12345).unwrap();
                }
                let mut env = Env {
                    context: 0,
                    cpu: &mut m.cpu,
                    sys: &mut m.sys,
                    ext: &mut m.ext,
                    thunk,
                };
                let Outcome::Return(result) = scan(&mut env) else {
                    panic!("expected supported native integer scan")
                };
                assert_eq!(
                    result,
                    if module == "msvcrt.dll" && expected[0] == u32::MAX {
                        0
                    } else {
                        expected[0]
                    },
                    "{module} {format} {text}"
                );
                for i in 0..3 {
                    assert_eq!(
                        m.cpu.read_u32(0x410300 + i * 4).unwrap(),
                        expected[i as usize + 1]
                    );
                }
            }
        }
    }
}
