//! CRT locale categories and returned strings belong to their CRT module.
//! They do not change Win32's ACP or the independent multibyte code page.
use super::*;
use adv_abi::{AnsiStr, GuestPtr, guest::memory_failure, win32};

mod conv;
mod ctype;

const CATEGORY_NAMES: [&str; 5] = [
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
];
const BUFFER_BYTES: u32 = 1024;
const LCID_OFFSET: u32 = BUFFER_BYTES - 6 * 4;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
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
    fn lcid(self) -> u32 {
        match self.language {
            Language::C => 0,
            Language::Japanese => 0x411,
            Language::Chinese => 0x804,
            Language::English => 0x409,
        }
    }
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
    multibyte_codepage: Option<u32>,
    // VC80's exported case functions enter a different EOF/error path after
    // any non-C locale has been installed, even if LC_CTYPE later returns C.
    // This latch limits the supported EOF profile; it does not emulate that
    // CRT-version-specific slow-path result or introduce another locale owner.
    ever_non_c: bool,
    buffer: u32,
    ctype_tables: HashMap<u32, u32>,
    monetary_codepage: u32,
    conversions: HashMap<conv::Profile, u32>,
}
pub(super) fn register(registry: &mut Registry) {
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        registry.register(adv_abi::Api {
            module,
            ..setlocale_ABI
        });
    }
    registry.register(lc_handles_ABI);
    registry.register(lc_codepage_ABI);
    ctype::register(registry);
    conv::register(registry);
}

/// Formatting observes the CRT module's LC_CTYPE, independently of Win32 ACP.
pub(super) fn formatting_codepage(env: &Env<'_>) -> u32 {
    let module = crt_module(&env.sys.thunks.get(env.thunk).module);
    env.ext
        .try_get::<CrtState>()
        .and_then(|state| state.locales.get(module))
        .map_or(0, |state| state.categories[1].codepage)
}

/// Multibyte string traversal has a separate code page, initially the guest ACP.
pub(super) fn multibyte_codepage(env: &Env<'_>) -> u32 {
    let module = crt_module(&env.sys.thunks.get(env.thunk).module);
    env.ext
        .try_get::<CrtState>()
        .and_then(|state| state.locales.get(module))
        .and_then(|state| state.multibyte_codepage)
        .unwrap_or(env.ext.get::<crate::Win32>().codec.acp)
}

pub(super) fn set_multibyte_codepage(env: &mut Env<'_>, codepage: u32) {
    let module = crt_module(&env.sys.thunks.get(env.thunk).module).to_owned();
    env.ext
        .get_or_default::<CrtState>()
        .locales
        .entry(module)
        .or_default()
        .multibyte_codepage = Some(codepage);
}

/// Buffer comparisons consume the same module-owned LC_CTYPE byte conversion.
pub(super) fn lowercase_bytes(env: &Env<'_>) -> Option<[u8; 256]> {
    ctype::lowercase_bytes(formatting_codepage(env))
}

/// Decimal conversions observe NUMERIC and CTYPE independently. The existing
/// four language profiles all publish decimal dot through localeconv.
pub(super) fn scalar_float_profile(env: &Env<'_>) -> Option<([bool; 256], u8)> {
    let module = crt_module(&env.sys.thunks.get(env.thunk).module);
    let state = env
        .ext
        .try_get::<CrtState>()
        .and_then(|s| s.locales.get(module));
    let numeric = state.map_or(Locale::default(), |s| s.categories[3]);
    if !matches!(numeric.codepage, 0 | 932 | 936 | 1252) {
        return None;
    }
    let codepage = state.map_or(0, |s| s.categories[1].codepage);
    ctype::byte_spaces(codepage).map(|spaces| (spaces, b'.'))
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
    // The category names and LCID array share one module-owned process block.
    // Publish them together so no successful set exposes stale category IDs.
    let label = name(&categories, category as usize);
    debug_assert!(label.len() < LCID_OFFSET as usize);
    let mut bytes = [0u8; BUFFER_BYTES as usize];
    bytes[..label.len()].copy_from_slice(label.as_bytes());
    for (index, locale) in categories.iter().enumerate() {
        let at = LCID_OFFSET as usize + (index + 1) * 4;
        bytes[at..at + 4].copy_from_slice(&locale.lcid().to_le_bytes());
    }
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
    let monetary_codepage = conv::updated_monetary_codepage(&state, &categories);
    let ever_non_c = state.ever_non_c
        || (!requested.0.is_null()
            && categories
                .iter()
                .any(|locale| locale.language != Language::C));
    env.ext.get_mut::<CrtState>().locales.insert(
        module,
        LocaleState {
            categories,
            buffer: pointer,
            monetary_codepage,
            ever_non_c,
            ..state
        },
    );
    Outcome::Return(pointer)
}

#[win32(msvcr90, name = "___lc_handle_func", level = state)]
fn lc_handles(env: &mut Env<'_>) -> Outcome {
    let module = crt_module(&env.sys.thunks.get(env.thunk).module);
    if let Some(state) = env
        .ext
        .try_get::<CrtState>()
        .and_then(|s| s.locales.get(module))
        && state.buffer != 0
    {
        // Already-published data stays readable even if the guest made its
        // allocation read-only. A getter does not rewrite category storage.
        return Outcome::Return(state.buffer + LCID_OFFSET);
    }
    // Before any setlocale call, all categories are C. Initializing the shared
    // return block also gives this pointer a real owner and detach lifecycle.
    match setlocale(env, 0, AnsiStr(GuestPtr::new(0))) {
        Outcome::Return(pointer) if pointer != 0 => Outcome::Return(pointer + LCID_OFFSET),
        other => other,
    }
}

#[win32(msvcr90, name = "___lc_codepage_func", level = state)]
fn lc_codepage(env: &mut Env<'_>) -> u32 {
    let module = crt_module(&env.sys.thunks.get(env.thunk).module);
    env.ext
        .try_get::<CrtState>()
        .and_then(|state| state.locales.get(module))
        .map_or(0, |state| state.categories[1].codepage)
}

pub(super) fn release_process_tables(env: &mut Env<'_>) -> Result<(), String> {
    ctype::release_tables(env)?;
    conv::release_tables(env)?;
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
