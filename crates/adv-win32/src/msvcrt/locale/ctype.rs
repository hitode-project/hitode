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
fn table(codepage: u32) -> Option<[u16; 257]> {
    if !matches!(codepage, 0 | 932 | 936 | 1252) {
        return None;
    }
    let mut words = [0; 257]; // EOF at[-1] stays zero; returned pointer starts at[1].
    for byte in 0..=255u8 {
        words[usize::from(byte) + 1] = if byte < 128 {
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
        };
    }
    Some(words)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_ABI);
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
mod tests;
