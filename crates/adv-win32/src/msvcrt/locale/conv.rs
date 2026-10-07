//! VC90's published x86 lconv prefix: ten ANSI pointers and eight CHAR fields.
//! The SDK declaration supplies the cdecl ABI, not its modern wide-field tail.
use super::*;

const PREFIX_BYTES: usize = 48;

/// Numeric and monetary data come from their own categories. VC90 builds the
/// monetary strings using LC_CTYPE's code page when LC_MONETARY changes; C uses
/// the monetary category's code page. Later CTYPE changes do not rebuild them.
pub(super) fn updated_monetary_codepage(state: &LocaleState, next: &[Locale; 5]) -> u32 {
    if next[2] == state.categories[2] {
        state.monetary_codepage
    } else if next[2].language == Language::C {
        0
    } else if next[1].codepage != 0 {
        next[1].codepage
    } else {
        next[2].codepage
    }
}

/// At most 4 numeric languages * 4 monetary languages * 4 encodings. Other
/// category changes cannot grow this cache. Each block owns all ten strings.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Profile {
    numeric: Language,
    monetary: Language,
    codepage: u32,
}
impl Profile {
    fn from_state(state: &LocaleState) -> Option<Self> {
        if ![
            state.categories[2].codepage,
            state.categories[3].codepage,
            state.monetary_codepage,
        ]
        .iter()
        .all(|cp| matches!(cp, 0 | 932 | 936 | 1252))
        {
            return None;
        }
        Some(Self {
            numeric: state.categories[3].language,
            monetary: state.categories[2].language,
            codepage: if state.categories[2].language == Language::C {
                0
            } else {
                state.monetary_codepage
            },
        })
    }
    fn data(self) -> Option<([&'static [u8]; 10], [u8; 8])> {
        let numeric = self.numeric != Language::C;
        let monetary = self.monetary != Language::C;
        // VC90's locale information uses the yen sign, including zh-CN. Its
        // Windows best-fit conversions are 5C (932), A3A4 (936), A5 (1252).
        // These bytes were checked against VC90, rather than substituting the
        // Unicode full-width glyph or Win32 ACP/encoding_rs replacement bytes.
        let currency: &[u8] = match self.monetary {
            Language::C => b"",
            Language::English => b"$",
            Language::Japanese | Language::Chinese => match self.codepage {
                932 => b"\\",
                936 => b"\xa3\xa4",
                1252 => b"\xa5",
                _ => return None,
            },
        };
        let international: &[u8] = match self.monetary {
            Language::C => b"",
            Language::Japanese => b"JPY",
            Language::Chinese => b"CNY",
            Language::English => b"USD",
        };
        let flags = match self.monetary {
            Language::C => [127; 8],
            Language::Japanese => [0, 0, 1, 0, 1, 0, 3, 3],
            Language::Chinese => [2, 2, 1, 0, 1, 0, 4, 4],
            Language::English => [2, 2, 1, 0, 1, 0, 3, 0],
        };
        Some((
            [
                b".",
                if numeric { b"," } else { b"" },
                if numeric { b"\x03" } else { b"" },
                international,
                currency,
                if monetary { b"." } else { b"" },
                if monetary { b"," } else { b"" },
                if monetary { b"\x03" } else { b"" },
                b"",
                if monetary { b"-" } else { b"" },
            ],
            flags,
        ))
    }
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_ABI);
}

#[win32(msvcr90, name = "localeconv", level = state)]
fn get(env: &mut Env<'_>) -> Outcome {
    let module = crt_module(&env.sys.thunks.get(env.thunk).module).to_owned();
    let state = env
        .ext
        .try_get::<CrtState>()
        .and_then(|s| s.locales.get(&module));
    let profile = Profile::from_state(state.unwrap_or(&LocaleState::default()));
    if let Some(pointer) = profile.and_then(|p| state.and_then(|s| s.conversions.get(&p))) {
        // A getter must not rewrite already-published (possibly read-only)
        // memory. Earlier snapshots remain owned until CRT process detach.
        return Outcome::Return(*pointer);
    }
    let Some((profile, (strings, flags))) = profile.and_then(|p| p.data().map(|data| (p, data)))
    else {
        adv_abi::warn_unavailable(
            env,
            &format!("{module}!localeconv"),
            "VC90 numeric/monetary locale or captured currency encoding is unavailable (supported: C, Japanese/Japan, Simplified Chinese/China, English/United States; CP932/936/1252)",
        );
        return match floating::errno_address(env).write(env.cpu, 40) {
            Ok(()) => Outcome::Return(0),
            Err(error) => memory_failure(error),
        };
    };
    let size = PREFIX_BYTES + strings.iter().map(|s| s.len() + 1).sum::<usize>();
    let pointer = allocate(env, size as u32, false);
    if pointer == 0 {
        return match floating::errno_address(env).write(env.cpu, 12) {
            Ok(()) => Outcome::Return(0),
            Err(error) => memory_failure(error),
        };
    }
    let mut bytes = vec![0; size];
    bytes[40..48].copy_from_slice(&flags);
    let mut offset = PREFIX_BYTES;
    for (index, string) in strings.iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&(pointer + offset as u32).to_le_bytes());
        bytes[offset..offset + string.len()].copy_from_slice(string);
        offset += string.len() + 1;
    }
    if let Err(error) =
        adv_abi::guest::check_range(env.cpu, pointer, size as u32, adv_cpu::backend::Prot::WRITE)
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
        .conversions
        .insert(profile, pointer);
    Outcome::Return(pointer)
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
                .conversions
                .iter()
                .map(move |(&profile, &pointer)| (module.clone(), profile, pointer))
        })
        .collect();
    for (module, profile, pointer) in cells {
        let heaps = env.ext.get_mut::<Heaps>();
        if !heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer) {
            return Err(format!(
                "CRT localeconv detach: invalid heap block {pointer:#x}"
            ));
        }
        env.ext
            .get_mut::<CrtState>()
            .locales
            .get_mut(&module)
            .unwrap()
            .conversions
            .remove(&profile);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
