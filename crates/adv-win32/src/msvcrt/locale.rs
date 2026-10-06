//! CRT locale categories and returned strings belong to their CRT module.
//! They do not change Win32's ACP or the independent multibyte code page.
use super::*;
use adv_abi::{AnsiStr, guest::memory_failure, win32};

const CATEGORY_NAMES: [&str; 5] = [
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
];
const BUFFER_BYTES: u32 = 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Language {
    C,
    Japanese,
    Chinese,
    English,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Locale {
    language: Language,
    codepage: u32,
}
impl Default for Locale {
    fn default() -> Self {
        Self {
            language: Language::C,
            codepage: 0,
        }
    }
}
impl Locale {
    fn name(self) -> String {
        let language = match self.language {
            Language::C => return "C".to_owned(),
            Language::Japanese => "Japanese_Japan",
            Language::Chinese => "Chinese (Simplified)_China",
            Language::English => "English_United States",
        };
        format!("{language}.{}", self.codepage)
    }
}
#[derive(Clone, Default)]
pub(super) struct LocaleState {
    categories: [Locale; 5],
    buffer: u32,
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(setlocale_ABI);
}

fn default_language(acp: u32) -> Language {
    // Match kernel32's existing virtual GetUserDefaultLCID profile, not the
    // machine running the host: CP936 is zh-CN, otherwise the profile is ja-JP.
    if acp == 936 {
        Language::Chinese
    } else {
        Language::Japanese
    }
}
fn language(text: &str) -> Option<Language> {
    if ["Japanese", "Japanese_Japan"]
        .iter()
        .any(|name| text.eq_ignore_ascii_case(name))
    {
        Some(Language::Japanese)
    } else if ["Chinese (Simplified)", "Chinese (Simplified)_China"]
        .iter()
        .any(|name| text.eq_ignore_ascii_case(name))
    {
        Some(Language::Chinese)
    } else if ["English", "English_United States"]
        .iter()
        .any(|name| text.eq_ignore_ascii_case(name))
    {
        Some(Language::English)
    } else {
        None
    }
}
fn parse(text: &str, acp: u32) -> Option<Locale> {
    if text == "C" {
        return Some(Locale::default());
    }
    let (name, suffix) = text.split_once('.').unwrap_or((text, ""));
    let language = if name.is_empty() {
        default_language(acp)
    } else {
        language(name)?
    };
    let default_cp = if name.is_empty() {
        acp
    } else {
        match language {
            Language::Japanese => 932,
            Language::Chinese => 936,
            Language::English => 1252,
            Language::C => unreachable!(),
        }
    };
    let codepage = if suffix.is_empty()
        || suffix.eq_ignore_ascii_case("ACP")
        || suffix.eq_ignore_ascii_case("OCP")
    {
        default_cp
    } else {
        if !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        suffix.parse().ok()?
    };
    // These encodings and the corresponding ASCII character/numeric behavior
    // are already provided by the guest Codec. Legacy VC90 has no UTF-8 mode.
    matches!(codepage, 932 | 936 | 1252).then_some(Locale { language, codepage })
}
fn updated(
    mut categories: [Locale; 5],
    category: usize,
    text: &str,
    acp: u32,
) -> Option<[Locale; 5]> {
    if category == 0 && text.contains('=') {
        for item in text.split(';') {
            let (name, value) = item.split_once('=')?;
            let index = CATEGORY_NAMES
                .iter()
                .position(|category| *category == name)?;
            categories[index] = parse(value, acp)?;
        }
    } else {
        let value = parse(text, acp)?;
        if category == 0 {
            categories.fill(value);
        } else {
            categories[category - 1] = value;
        }
    }
    Some(categories)
}
fn name(categories: &[Locale; 5], category: usize) -> String {
    if category != 0 {
        return categories[category - 1].name();
    }
    if categories.iter().all(|locale| *locale == categories[0]) {
        return categories[0].name();
    }
    CATEGORY_NAMES
        .iter()
        .zip(categories)
        .map(|(category, locale)| format!("{category}={}", locale.name()))
        .collect::<Vec<_>>()
        .join(";")
}

#[win32(msvcr90, name = "setlocale", level = state)]
fn setlocale(env: &mut Env<'_>, category: i32, requested: AnsiStr) -> Outcome {
    if !(0..=5).contains(&category) {
        return invalid_parameter_result(env, "setlocale", 22, 0);
    }
    let module = crt_module(&env.sys.thunks.get(env.thunk).module).to_owned();
    let state = env
        .ext
        .get_or_default::<CrtState>()
        .locales
        .get(&module)
        .cloned()
        .unwrap_or_default();
    let categories = if requested.0.is_null() {
        state.categories
    } else {
        let bytes = match requested.read(env.cpu, 4096) {
            Ok(bytes) => bytes,
            Err(error) => return memory_failure(error),
        };
        let parsed = std::str::from_utf8(&bytes).ok().and_then(|text| {
            updated(
                state.categories,
                category as usize,
                text,
                env.ext.get::<crate::Win32>().codec.acp,
            )
        });
        let Some(categories) = parsed else {
            adv_abi::warn_unavailable(
                env,
                &format!("{module}!setlocale"),
                "CRT locale language, country, code page or category expression is unavailable (supported: C, virtual default, Japanese/Japan, Simplified Chinese/China, English/United States; CP932/936/1252)",
            );
            // Native invalid locale specifications do not change errno, the
            // previous locale, or the string returned by an earlier query.
            return Outcome::Return(0);
        };
        categories
    };
    let mut bytes = name(&categories, category as usize).into_bytes();
    bytes.push(0);
    debug_assert!(bytes.len() <= BUFFER_BYTES as usize);
    let fresh = state.buffer == 0;
    let pointer = if fresh {
        allocate(env, BUFFER_BYTES, true)
    } else {
        state.buffer
    };
    if pointer == 0 {
        return match floating::errno_address(env).write(env.cpu, 12) {
            Ok(()) => Outcome::Return(0),
            Err(error) => memory_failure(error),
        };
    }
    if let Err(error) = adv_abi::guest::check_range(
        env.cpu,
        pointer,
        bytes.len() as u32,
        adv_cpu::backend::Prot::WRITE,
    )
    .and_then(|_| env.cpu.write(pointer, &bytes))
    {
        if fresh {
            let heaps = env.ext.get_mut::<Heaps>();
            heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer);
        }
        return memory_failure(error);
    }
    env.ext.get_mut::<CrtState>().locales.insert(
        module,
        LocaleState {
            categories,
            buffer: pointer,
        },
    );
    Outcome::Return(pointer)
}

pub(super) fn release_process_tables(env: &mut Env<'_>) -> Result<(), String> {
    let Some(state) = env.ext.try_get::<CrtState>() else {
        return Ok(());
    };
    let cells: Vec<_> = state
        .locales
        .iter()
        .map(|(module, state)| (module.clone(), state.buffer))
        .collect();
    for (module, pointer) in cells {
        if pointer != 0 {
            let heaps = env.ext.get_mut::<Heaps>();
            if !heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer) {
                return Err(format!(
                    "CRT locale detach: invalid heap block {pointer:#x}"
                ));
            }
        }
        env.ext.get_mut::<CrtState>().locales.remove(&module);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
