//! Integer conversions share checked prefix scanning and x86-width saturation.
//! Reads stop at the first non-digit instead of materializing a guest string.
//! The module/thread locale and errno owners remain the existing CRT state.
use super::*;
use adv_abi::{GuestPtr, guest::memory_failure};
use adv_cpu::backend::MemFault;

const NAMES: &[&str] = &[
    "atoi",
    "atol",
    "atoll",
    "_atoi64",
    "_wtoi",
    "_wtol",
    "_wtoll",
    "_wtoi64",
    "strtol",
    "strtoul",
    "strtoll",
    "strtoull",
    "_strtoi64",
    "_strtoui64",
    "wcstol",
    "wcstoul",
    "wcstoll",
    "wcstoull",
    "_wcstoi64",
    "_wcstoui64",
];
pub(super) fn register(registry: &mut Registry) {
    let exports: Vec<_> = NAMES
        .iter()
        .map(|&name| (name, call as adv_machine::Handler))
        .collect();
    versions::register(registry, &exports);
}

#[derive(Clone, Copy)]
struct Format {
    wide: bool,
    signed: bool,
    bits: u32,
    decimal: bool,
}
impl Format {
    fn name(name: &str) -> Self {
        Self {
            wide: name.starts_with("_w") || name.starts_with("wcs"),
            signed: !name.contains('u'),
            bits: if name.contains("64") || name.ends_with("ll") {
                64
            } else {
                32
            },
            decimal: matches!(
                name,
                "atoi" | "atol" | "atoll" | "_atoi64" | "_wtoi" | "_wtol" | "_wtoll" | "_wtoi64"
            ),
        }
    }
    fn result(self, value: u64) -> Outcome {
        if self.bits == 64 {
            Outcome::Return64(value)
        } else {
            Outcome::Return(value as u32)
        }
    }
}
struct Parsed {
    value: u64,
    consumed: u32,
    range: bool,
}

fn digit(unit: u16, wide: bool) -> Option<u32> {
    match unit {
        48..=57 => return Some(u32::from(unit - 48)),
        65..=90 => return Some(u32::from(unit - 65 + 10)),
        97..=122 => return Some(u32::from(unit - 97 + 10)),
        _ => {}
    }
    if wide {
        // Decimal digit blocks accepted by the native VC90 wide parser. These
        // are character ranges, not a parallel locale or code-page database.
        for zero in [
            0x0660, 0x06f0, 0x0966, 0x09e6, 0x0a66, 0x0ae6, 0x0b66, 0x0c66, 0x0ce6, 0x0d66, 0x0e50,
            0x0ed0, 0x0f20, 0x1040, 0x17e0, 0x1810, 0xff10,
        ] {
            if (zero..zero + 10).contains(&unit) {
                return Some(u32::from(unit - zero));
            }
        }
    }
    None
}

fn parse(
    read: impl Fn(u32) -> Result<u16, MemFault>,
    space: impl Fn(u16) -> bool,
    format: Format,
    base: u32,
    track_end: bool,
) -> Result<Parsed, MemFault> {
    let mut at = 0u32;
    let mut unit = read(at)?;
    while space(unit) {
        at += 1;
        unit = read(at)?;
    }
    let negative = unit == u16::from(b'-');
    if negative || unit == u16::from(b'+') {
        at += 1;
        unit = read(at)?;
    }
    let mut radix = base;
    if radix == 0 {
        radix = if digit(unit, format.wide) == Some(0) {
            8
        } else {
            10
        };
    }
    if matches!(base, 0 | 16)
        && digit(unit, format.wide) == Some(0)
        && matches!(read(at + 1)?, 88 | 120)
    {
        radix = 16;
        at += 2;
        unit = read(at)?;
    }
    let begin = at;
    let maximum = if format.signed {
        (1u64 << (format.bits - 1)) - u64::from(!negative)
    } else if format.bits == 64 {
        u64::MAX
    } else {
        u64::from(u32::MAX)
    };
    let mut value = 0u64;
    let mut range = false;
    while let Some(d) = digit(unit, format.wide).filter(|&d| d < radix) {
        if value > maximum / u64::from(radix)
            || (value == maximum / u64::from(radix) && u64::from(d) > maximum % u64::from(radix))
        {
            range = true;
        } else if !range {
            value = value * u64::from(radix) + u64::from(d);
        }
        at += 1;
        // Native conversion need not scan a suffix after known overflow when
        // the caller requested no end position. Retain that access boundary.
        if range && !track_end {
            break;
        }
        unit = read(at)?;
    }
    if at == begin {
        return Ok(Parsed {
            value: 0,
            consumed: 0,
            range: false,
        });
    }
    if range {
        value = maximum;
    }
    if negative && (format.signed || !range) {
        value = value.wrapping_neg();
    }
    Ok(Parsed {
        value,
        consumed: at,
        range,
    })
}

fn call(env: &mut Env<'_>) -> Outcome {
    let format = Format::name(&env.sys.thunks.get(env.thunk).name);
    let source = env.arg(0);
    let end = if format.decimal { 0 } else { env.arg(1) };
    let base = if format.decimal { 10 } else { env.arg(2) };
    // Native VC90 initializes the output before validating source and radix.
    if end != 0
        && let Err(fault) = env.cpu.write_u32(end, source)
    {
        return memory_failure(fault);
    }
    if source == 0 || (base != 0 && !(2..=36).contains(&base)) {
        return errors::invalid_outcome(env, format.result(0));
    }
    let Some((spaces, _)) = locale::scalar_float_profile(env) else {
        let thunk = env.sys.thunks.get(env.thunk);
        adv_abi::warn_unavailable(
            env,
            &format!("{}!{}", thunk.module, thunk.name),
            "integer conversion requires C or CP932/936/1252 locale categories",
        );
        return format.result(0);
    };
    let codepage = locale::formatting_codepage(env);
    let width = if format.wide { 2 } else { 1 };
    let parsed = parse(
        |index| {
            let address = index
                .checked_mul(width)
                .and_then(|n| source.checked_add(n))
                .ok_or(MemFault {
                    addr: source,
                    len: u32::MAX,
                })?;
            if format.wide {
                GuestPtr::<u16>::new(address).read(env.cpu)
            } else {
                GuestPtr::<u8>::new(address).read(env.cpu).map(u16::from)
            }
        },
        |unit| {
            if format.wide {
                if codepage == 0 && unit < 256 {
                    matches!(unit, 9..=13 | 32)
                } else {
                    adv_os::nls::ctype1(unit) & 8 != 0
                }
            } else {
                spaces[usize::from(unit)]
            }
        },
        format,
        base,
        end != 0,
    );
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(fault) => return memory_failure(fault),
    };
    if end != 0 {
        let pointer = parsed
            .consumed
            .checked_mul(width)
            .and_then(|n| source.checked_add(n));
        let Some(pointer) = pointer else {
            return memory_failure(MemFault {
                addr: source,
                len: u32::MAX,
            });
        };
        if let Err(fault) = env.cpu.write_u32(end, pointer) {
            return memory_failure(fault);
        }
    }
    if parsed.range
        && let Err(fault) = floating::errno_address(env).write(env.cpu, 34)
    {
        return memory_failure(fault);
    }
    format.result(parsed.value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_sign_prefix_and_end_positions() {
        for (name, text, base, value, consumed, range) in [
            ("wcstoull", "18446744073709551615!", 10, u64::MAX, 20, false),
            ("wcstoull", "18446744073709551616!", 10, u64::MAX, 20, true),
            ("wcstoull", "  -1x", 0, u64::MAX, 4, false),
            ("wcstoull", "  +0x1fZ", 0, 31, 7, false),
            ("wcstoull", "0xG", 0, 0, 0, false),
            ("wcstoull", "09", 0, 0, 1, false),
            ("wcstoull", " +猫", 10, 0, 0, false),
            ("wcstoull", "z", 36, 35, 1, false),
            (
                "strtol",
                "-2147483648!",
                10,
                i32::MIN as i64 as u64,
                11,
                false,
            ),
            (
                "strtol",
                "-2147483649!",
                10,
                i32::MIN as i64 as u64,
                11,
                true,
            ),
            ("strtol", "2147483648!", 10, i32::MAX as u64, 10, true),
            ("strtoul", "-4294967295!", 10, 0xffffffff00000001, 11, false),
            ("strtoul", "-4294967296!", 10, u32::MAX as u64, 11, true),
            ("_wcstoi64", "１２٣!", 10, 123, 3, false),
        ] {
            let text: Vec<_> = text.encode_utf16().chain([0]).collect();
            let parsed = parse(
                |i| Ok(text[i as usize]),
                |u| matches!(u, 9..=13 | 32),
                Format::name(name),
                base,
                true,
            )
            .unwrap();
            assert_eq!(
                (parsed.value, parsed.consumed, parsed.range),
                (value, consumed, range),
                "{name}: {text:?}"
            );
        }
    }
}
