//! VC90 byte classifications over the existing LC_CTYPE state and heap owner.
use super::*;

const TABLE_BYTES: u32 = 257 * 2;

// CP1252 bytes80..BF from the VC90 oracle. C0..FF reduce to case ranges below.
const CP1252_LOW: [u16; 64] = [
    0x200, 0x220, 0x210, 0x302, 0x210, 0x210, 0x210, 0x210, 0x200, 0x210, 0x301, 0x210, 0x301,
    0x220, 0x301, 0x220, 0x220, 0x210, 0x210, 0x210, 0x210, 0x210, 0x210, 0x210, 0x200, 0x200,
    0x302, 0x210, 0x302, 0x220, 0x302, 0x301, 0x248, 0x210, 0x210, 0x210, 0x210, 0x210, 0x210,
    0x210, 0x210, 0x210, 0x312, 0x210, 0x210, 0x230, 0x210, 0x210, 0x210, 0x210, 0x214, 0x214,
    0x210, 0x312, 0x210, 0x210, 0x210, 0x214, 0x312, 0x210, 0x210, 0x210, 0x210, 0x210,
];

fn ascii(byte: u8, localized: bool) -> u16 {
    let mut flags = 0;
    if byte.is_ascii_uppercase() {
        flags |= 1;
    }
    if byte.is_ascii_lowercase() {
        flags |= 2;
    }
    if byte.is_ascii_digit() {
        flags |= 4;
    }
    if matches!(byte, b' ' | 9..=13) {
        flags |= 8;
    }
    if byte.is_ascii_punctuation() {
        flags |= 0x10;
    }
    if byte.is_ascii_control() {
        flags |= 0x20;
    }
    if byte == b' ' {
        flags |= 0x40;
    }
    if byte.is_ascii_hexdigit() {
        flags |= 0x80;
    }
    if localized {
        flags |= 0x200; // C1_DEFINED in localized tables, absent from C.
        if byte.is_ascii_alphabetic() {
            flags |= 0x100;
        }
        if byte == b'\t' {
            flags |= 0x40;
        }
    }
    flags
}
fn byte_flags(codepage: u32, byte: u8) -> Option<u16> {
    if !matches!(codepage, 0 | 932 | 936 | 1252) {
        return None;
    }
    Some(if byte < 128 {
        ascii(byte, codepage != 0)
    } else {
        match codepage {
            0 => 0,
            932 => match byte {
                0x80 => 0x220,
                0x81..=0x9f | 0xe0..=0xfc => 0x8000,
                0xa1..=0xa5 => 0x210,
                0xa6..=0xdf => 0x300,
                _ => 0x200,
            },
            936 => {
                if (0x81..=0xfe).contains(&byte) {
                    0x8000
                } else {
                    0x200
                }
            }
            1252 => match byte {
                0x80..=0xbf => CP1252_LOW[usize::from(byte - 0x80)],
                0xc0..=0xd6 | 0xd8..=0xde => 0x301,
                0xdf..=0xf6 | 0xf8..=0xff => 0x302,
                _ => 0x210,
            },
            _ => unreachable!(),
        }
    })
}

fn table(codepage: u32) -> Option<[u16; 257]> {
    byte_flags(codepage, 0)?;
    // EOF at[-1] stays zero; the returned pointer starts at[1]. Each byte
    // frontend consumes this same classifier without constructing an array.
    Some(std::array::from_fn(|index| {
        if index == 0 {
            0
        } else {
            byte_flags(codepage, (index - 1) as u8).unwrap()
        }
    }))
}

pub(super) fn byte_spaces(codepage: u32) -> Option<[bool; 256]> {
    table(codepage).map(|words| std::array::from_fn(|i| words[i + 1] & 8 != 0))
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_ABI);
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        for api in [
            lower_ABI,
            upper_ABI,
            alphabetic_ABI,
            alphanumeric_ABI,
            digit_ABI,
            space_ABI,
            control_ABI,
            uppercase_ABI,
            lowercase_ABI,
            hex_digit_ABI,
            punctuation_ABI,
            printable_ABI,
            graphical_ABI,
        ] {
            registry.register(adv_abi::Api { module, ..api });
        }
    }
}

fn unavailable(env: &mut Env<'_>, reason: &str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    Outcome::Fail(format!("{api}: {reason}"))
}

fn classify(env: &mut Env<'_>, character: i32, mask: u16) -> Outcome {
    let codepage = formatting_codepage(env);
    if byte_flags(codepage, 0).is_none() {
        return unavailable(
            env,
            "CRT byte classification requires C or CP932/936/1252 LC_CTYPE",
        );
    }
    if character == -1 {
        return Outcome::Return(0);
    }
    let Ok(byte) = u8::try_from(character) else {
        return unavailable(
            env,
            "CRT byte classification requires EOF or a byte; packed DBCS and other integers are unavailable",
        );
    };
    Outcome::Return(u32::from(byte_flags(codepage, byte).unwrap() & mask))
}

fn case(env: &mut Env<'_>, character: i32, upper: bool) -> Outcome {
    let codepage = formatting_codepage(env);
    if byte_flags(codepage, 0).is_none() {
        return unavailable(
            env,
            "CRT byte case conversion requires C or CP932/936/1252 LC_CTYPE",
        );
    }
    if character == -1 {
        let module = crt_module(&env.sys.thunks.get(env.thunk).module);
        let changed = env
            .ext
            .try_get::<CrtState>()
            .and_then(|state| state.locales.get(module))
            .is_some_and(|state| state.ever_non_c);
        if codepage == 0 && !changed {
            return Outcome::Return(u32::MAX);
        }
        // VC80's localized exported function has exceptional EOF conversion
        // and errno behavior. Do not project this quirk onto all three CRTs.
        return unavailable(
            env,
            "localized or restored-C CRT EOF case conversion is unavailable",
        );
    }
    let Ok(byte) = u8::try_from(character) else {
        return unavailable(
            env,
            "CRT byte case conversion requires EOF or a byte; packed DBCS and other integers are unavailable",
        );
    };
    let converted = if upper {
        match byte {
            b'a'..=b'z' => byte - 32,
            0xe0..=0xf6 | 0xf8..=0xfe if codepage == 1252 => byte - 32,
            0x9a | 0x9c | 0x9e if codepage == 1252 => byte - 16,
            0xff if codepage == 1252 => 0x9f,
            _ => byte,
        }
    } else {
        match byte {
            b'A'..=b'Z' => byte + 32,
            0xc0..=0xd6 | 0xd8..=0xde if codepage == 1252 => byte + 32,
            0x8a | 0x8c | 0x8e if codepage == 1252 => byte + 16,
            0x9f if codepage == 1252 => 0xff,
            _ => byte,
        }
    };
    Outcome::Return(u32::from(converted))
}

#[win32(msvcrt, name = "tolower", level = implement)]
fn lower(env: &mut Env<'_>, character: i32) -> Outcome {
    case(env, character, false)
}
#[win32(msvcrt, name = "toupper", level = implement)]
fn upper(env: &mut Env<'_>, character: i32) -> Outcome {
    case(env, character, true)
}
#[win32(msvcrt, name = "isalpha", level = implement)]
fn alphabetic(env: &mut Env<'_>, character: i32) -> Outcome {
    classify(env, character, 0x103)
}
#[win32(msvcrt, name = "isalnum", level = implement)]
fn alphanumeric(env: &mut Env<'_>, character: i32) -> Outcome {
    classify(env, character, 0x107)
}
#[win32(msvcrt, name = "isdigit", level = implement)]
fn digit(env: &mut Env<'_>, character: i32) -> Outcome {
    classify(env, character, 4)
}
#[win32(msvcrt, name = "isspace", level = implement)]
fn space(env: &mut Env<'_>, character: i32) -> Outcome {
    classify(env, character, 8)
}
#[win32(msvcrt, name = "iscntrl", level = implement)]
fn control(env: &mut Env<'_>, character: i32) -> Outcome {
    classify(env, character, 0x20)
}
#[win32(msvcrt, name = "isupper", level = implement)]
fn uppercase(env: &mut Env<'_>, character: i32) -> Outcome {
    classify(env, character, 1)
}
#[win32(msvcrt, name = "islower", level = implement)]
fn lowercase(env: &mut Env<'_>, character: i32) -> Outcome {
    classify(env, character, 2)
}
#[win32(msvcrt, name = "isxdigit", level = implement)]
fn hex_digit(env: &mut Env<'_>, character: i32) -> Outcome {
    classify(env, character, 0x80)
}
#[win32(msvcrt, name = "ispunct", level = implement)]
fn punctuation(env: &mut Env<'_>, character: i32) -> Outcome {
    classify(env, character, 0x10)
}
#[win32(msvcrt, name = "isprint", level = implement)]
fn printable(env: &mut Env<'_>, character: i32) -> Outcome {
    classify(env, character, 0x157)
}
#[win32(msvcrt, name = "isgraph", level = implement)]
fn graphical(env: &mut Env<'_>, character: i32) -> Outcome {
    classify(env, character, 0x117)
}
#[win32(msvcr90, name = "__pctype_func", level = state)]
fn get(env: &mut Env<'_>) -> Outcome {
    let module = crt_module(&env.sys.thunks.get(env.thunk).module);
    let state = env
        .ext
        .try_get::<CrtState>()
        .and_then(|state| state.locales.get(module));
    let codepage = state.map_or(0, |state| state.categories[1].codepage);
    if let Some(&pointer) = state.and_then(|state| state.ctype_tables.get(&codepage)) {
        // Tables are immutable cached profiles. A getter never republishes
        // storage, including a table the guest has made read-only.
        return Outcome::Return(pointer + 2);
    }
    let module = module.to_owned();
    let Some(words) = table(codepage) else {
        adv_abi::warn_unavailable(
            env,
            &format!("{module}!__pctype_func"),
            &format!("CRT byte-classification profile for code page {codepage} is unavailable"),
        );
        return Outcome::Return(0);
    };
    let pointer = allocate(env, TABLE_BYTES, false);
    if pointer == 0 {
        return match floating::errno_address(env).write(env.cpu, 12) {
            Ok(()) => Outcome::Return(0),
            Err(error) => memory_failure(error),
        };
    }
    let bytes: Vec<_> = words.into_iter().flat_map(u16::to_le_bytes).collect();
    if let Err(error) =
        adv_abi::guest::check_range(env.cpu, pointer, TABLE_BYTES, adv_cpu::backend::Prot::WRITE)
            .and_then(|_| env.cpu.write(pointer, &bytes))
    {
        let heaps = env.ext.get_mut::<Heaps>();
        heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer);
        return memory_failure(error);
    }
    env.ext
        .get_or_default::<CrtState>()
        .locales
        .entry(module)
        .or_default()
        .ctype_tables
        .insert(codepage, pointer);
    Outcome::Return(pointer + 2)
}

pub(super) fn release_tables(env: &mut Env<'_>) -> Result<(), String> {
    let Some(state) = env.ext.try_get::<CrtState>() else {
        return Ok(());
    };
    let cells: Vec<_> = state
        .locales
        .iter()
        .flat_map(|(module, state)| {
            state
                .ctype_tables
                .iter()
                .map(move |(&cp, &pointer)| (module.clone(), cp, pointer))
        })
        .collect();
    for (module, cp, pointer) in cells {
        let heaps = env.ext.get_mut::<Heaps>();
        if !heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer) {
            return Err(format!("CRT ctype detach: invalid heap block {pointer:#x}"));
        }
        env.ext
            .get_mut::<CrtState>()
            .locales
            .get_mut(&module)
            .unwrap()
            .ctype_tables
            .remove(&cp);
    }
    Ok(())
}

#[cfg(test)]
mod front_tests;
#[cfg(test)]
mod tests;
