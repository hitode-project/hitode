//! The anonymous compatibility report (schema `hitode.report/3`) that the
//! launcher saves or opens in an issue, and the run summary the shell derives it from.
//!
//! Field names and order match `apps/adv-launcher/src/lib/report.ts`.
//! Executable identity and structured diagnostics are automatic; the player
//! chooses whether to include the matched title, edition and note.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, time::Duration};

pub const SCHEMA: &str = "hitode.report/3";
/// Maximum anonymized stack frames a failure carries.
pub const MAX_STACK_FRAMES: usize = 16;
/// Maximum missing DLL names a failure carries.
pub const MAX_MISSING_DLLS: usize = 16;
/// Maximum standard system imports a game identity carries.
pub const MAX_SYSTEM_IMPORTS: usize = 64;
/// Maximum thread entries in a hang snapshot.
pub const MAX_HANG_THREADS: usize = 16;
/// Maximum boundary entries in a hang snapshot.
pub const MAX_HANG_BOUNDARIES: usize = 12;
/// Maximum diagnostic warnings or Wine error entries.
pub const MAX_DIAG_ENTRIES: usize = 24;
/// Maximum rejected probes, exception kinds, or font names.
pub const MAX_PROBE_ENTRIES: usize = 16;
/// Maximum performance cliff entries.
pub const MAX_CLIFF_ENTRIES: usize = 8;

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    #[serde(deserialize_with = "schema")]
    pub schema: String,
    pub dedupe_key: String,
    pub app: App,
    pub platform: Platform,
    pub game: Game,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Title>,
    pub outcome: Outcome,
    pub issues: Vec<Issue>,
    /// Present only for the `stutter` symptom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perf_issues: Option<Vec<PerfIssue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hang: Option<HangSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Diagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perf: Option<Perf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_gaps: Option<ApiGaps>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline: Option<Timeline>,
}
impl Report {
    /// Checks bounded player text, title identifiers, the schema, failure
    /// diagnostics, PE import allowlist and API names.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!("unsupported report schema {:?}", self.schema));
        }
        if self
            .note
            .as_ref()
            .is_some_and(|note| note.chars().count() > 2000)
        {
            return Err("report note exceeds 2000 characters".into());
        }
        if let Some(title) = &self.title {
            if !title
                .vndb
                .strip_prefix('v')
                .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            {
                return Err("report VNDB ID must be v followed by digits".into());
            }
            if title.name.is_empty()
                || title.name.chars().count() > 160
                || title.name.chars().any(char::is_control)
            {
                return Err("report title must contain 1–160 characters without controls".into());
            }
            if title.edition.chars().count() > 80 {
                return Err("report edition exceeds 80 characters".into());
            }
        }
        self.game.validate()?;
        if let Some(run) = &self.run {
            run.validate()?;
        }
        if let Some(failure) = &self.failure {
            failure.validate()?;
        }
        if let Some(hang) = &self.hang {
            hang.validate()?;
        }
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.validate()?;
        }
        if let Some(perf) = &self.perf {
            perf.validate()?;
        }
        if let Some(gaps) = &self.api_gaps {
            gaps.validate()?;
        }
        if let Some(space) = &self.platform.address_space {
            space.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub version: String,
    pub build: String,
    pub channel: String,
}

fn schema<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value != SCHEMA {
        return Err(serde::de::Error::custom("unsupported report schema"));
    }
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform {
    pub os: String,
    pub os_major: String,
    pub model: String,
    /// iOS: whether the signature raises the memory limit (IOS-MEM-4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub increased_memory_limit: Option<bool>,
    /// iOS: the address space the launch reservation saw (IOS-MEM-5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_space: Option<AddressSpace>,
    pub graphics: Graphics,
    pub settings: Settings,
}

/// Mapped ranges an address space record holds at most.
pub const MAPPED_RANGES: usize = 16;

/// Host addresses as `0x` lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddressSpace {
    /// The end of the range the app may map.
    pub max: String,
    /// The guest window's base; absent when no window fit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// When no window fit, the mapped ranges above the lowest candidate
    /// window, as `start-end`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mapped: Vec<String>,
}
impl AddressSpace {
    pub fn new(
        max: u64,
        window: Option<u64>,
        mapped: impl IntoIterator<Item = std::ops::Range<u64>>,
    ) -> Self {
        Self {
            max: format!("{max:#x}"),
            window: window.map(|base| format!("{base:#x}")),
            mapped: mapped
                .into_iter()
                .map(|range| format!("{:#x}-{:#x}", range.start, range.end))
                .collect(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        let ranges = self.mapped.iter().all(|range| {
            range
                .split_once('-')
                .is_some_and(|(start, end)| is_hex_u64(start) && is_hex_u64(end))
        });
        if !is_hex_u64(&self.max)
            || !self.window.as_deref().is_none_or(is_hex_u64)
            || !ranges
            || self.mapped.len() > MAPPED_RANGES
        {
            return Err(
                "report address space must hold hex addresses and at most 16 ranges".into(),
            );
        }
        Ok(())
    }
}

fn is_hex_u64(text: &str) -> bool {
    text.strip_prefix("0x").is_some_and(|digits| {
        (1..=16).contains(&digits.len())
            && digits
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn is_hex_u32(text: &str) -> bool {
    text.strip_prefix("0x").is_some_and(|digits| {
        (1..=8).contains(&digits.len())
            && digits
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graphics {
    pub api: GraphicsApi,
    pub name: String,
    pub vendor: String,
    pub device: String,
    pub driver: String,
    pub tier: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphicsApi {
    Metal,
    Vulkan,
}

/// Host code-path choices, without file names, font names or paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upscaler: Option<Upscaler>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scaling: Option<Scaling>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_forward: Option<FastForward>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touch_mode: Option<TouchMode>,
    pub font_files: u32,
    pub font_overrides: u32,
    pub font_smoothing: Option<bool>,
    pub discs: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Upscaler {
    Off,
    Anime4k,
    Cunny,
    Artcnn,
    K7,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scaling {
    Fit,
    Integer,
    Stretch,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FastForward {
    Hold,
    Toggle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchMode {
    Direct,
    Trackpad,
}

impl Platform {
    pub fn current(graphics: Graphics, settings: Settings) -> Self {
        Self {
            os: std::env::consts::OS.into(),
            os_major: host_property(false)
                .split('.')
                .next()
                .unwrap_or("unknown")
                .into(),
            model: host_property(true),
            increased_memory_limit: None,
            address_space: None,
            graphics,
            settings,
        }
    }
}

#[cfg(target_vendor = "apple")]
fn host_property(model: bool) -> String {
    let name = if model {
        if cfg!(target_os = "ios") {
            c"hw.machine"
        } else {
            c"hw.model"
        }
    } else {
        c"kern.osproductversion"
    };
    let mut bytes = [0u8; 256];
    let mut size = bytes.len();
    // sysctl reports system identifiers, never a user-assigned device name.
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return "unknown".into();
    }
    String::from_utf8_lossy(&bytes[..size.saturating_sub(1)]).into_owned()
}
#[cfg(target_os = "android")]
fn host_property(model: bool) -> String {
    let name = if model {
        c"ro.product.model"
    } else {
        c"ro.build.version.release"
    };
    let mut bytes = [0u8; 92];
    unsafe extern "C" {
        fn __system_property_get(
            name: *const libc::c_char,
            value: *mut libc::c_char,
        ) -> libc::c_int;
    }
    let size = unsafe { __system_property_get(name.as_ptr(), bytes.as_mut_ptr().cast()) } as usize;
    String::from_utf8_lossy(&bytes[..size]).into_owned()
}
#[cfg(not(any(target_vendor = "apple", target_os = "android")))]
fn host_property(_: bool) -> String {
    "unknown".into()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Title {
    pub vndb: String,
    pub name: String,
    pub edition: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeArch {
    X86,
    X64,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeSubsystem {
    Gui,
    Cui,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Game {
    pub exe_sha256: String,
    pub exe_size: u64,
    /// Guest ANSI code page.
    pub codepage: u32,
    pub timestamp: u32,
    pub image_size: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<PeArch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subsystem: Option<PeSubsystem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub system_imports: Vec<String>,
}
impl Game {
    pub fn identify(exe: &Path, codepage: u32) -> std::io::Result<Self> {
        let bytes = std::fs::read(exe)?;
        let exe_sha256 = sha256(&bytes);
        let exe_size = bytes.len() as u64;
        let pe = match adv_pe::Pe::parse(&bytes) {
            Ok(pe) => pe,
            Err(adv_pe::PeError::NotI386(machine)) => {
                let arch = match machine {
                    0x8664 => PeArch::X64,
                    _ => PeArch::Other,
                };
                return Ok(Self {
                    exe_sha256,
                    exe_size,
                    codepage,
                    timestamp: 0,
                    image_size: 0,
                    file_version: None,
                    arch: Some(arch),
                    subsystem: None,
                    sections: None,
                    overlay_bytes: None,
                    system_imports: Vec::new(),
                });
            }
            Err(error) => return Err(std::io::Error::other(error)),
        };
        let file_version = pe
            .resources()
            .map_err(std::io::Error::other)?
            .and_then(|resources| {
                resources.leaves().into_iter().find_map(|leaf| {
                    if leaf.ty != adv_pe::ResId::Id(16) {
                        return None;
                    }
                    let data = pe.slice(leaf.data.rva, leaf.data.size).ok()?;
                    // VS_VERSION_INFO's fixed value follows its UTF-16 key, aligned to a DWORD.
                    let key: Vec<u8> = "VS_VERSION_INFO\0"
                        .encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect();
                    if data.get(6..6 + key.len())? != key {
                        return None;
                    }
                    let offset = (6 + key.len()).next_multiple_of(4);
                    let fixed = data.get(offset..offset + 16)?;
                    if fixed[..4] != 0xfeef04bdu32.to_le_bytes() {
                        return None;
                    }
                    let major = u32::from_le_bytes(fixed[8..12].try_into().ok()?);
                    let minor = u32::from_le_bytes(fixed[12..16].try_into().ok()?);
                    Some(format!(
                        "{}.{}.{}.{}",
                        major >> 16,
                        major & 0xffff,
                        minor >> 16,
                        minor & 0xffff
                    ))
                })
            });
        let subsystem = match pe.subsystem {
            2 => PeSubsystem::Gui,
            3 => PeSubsystem::Cui,
            _ => PeSubsystem::Other,
        };
        let end_of_sections = pe
            .sections
            .iter()
            .map(|s| u64::from(s.raw_offset) + u64::from(s.raw_size))
            .max()
            .unwrap_or(u64::from(pe.size_of_headers));
        let overlay = exe_size.saturating_sub(end_of_sections);
        let mut system_imports: Vec<String> = pe
            .imports()
            .unwrap_or_default()
            .into_iter()
            .map(|module| module.dll.to_ascii_lowercase())
            .filter(|dll| is_allowed_system_dll(dll))
            .collect();
        system_imports.sort();
        system_imports.dedup();
        system_imports.truncate(MAX_SYSTEM_IMPORTS);
        Ok(Self {
            exe_sha256,
            exe_size,
            codepage,
            timestamp: pe.timestamp,
            image_size: pe.size_of_image,
            file_version,
            arch: Some(PeArch::X86),
            subsystem: Some(subsystem),
            sections: u16::try_from(pe.sections.len()).ok(),
            overlay_bytes: (overlay > 0).then_some(overlay),
            system_imports,
        })
    }

    fn validate(&self) -> Result<(), String> {
        if self.system_imports.len() > MAX_SYSTEM_IMPORTS {
            return Err(format!(
                "report lists over {MAX_SYSTEM_IMPORTS} system imports"
            ));
        }
        if let Some(dll) = self
            .system_imports
            .iter()
            .find(|dll| !is_allowed_system_dll(dll))
        {
            return Err(format!("report system import {dll:?} is not allowlisted"));
        }
        Ok(())
    }
}

/// Notable standard Windows, DirectX, media and runtime DLLs; omits ubiquitous
/// kernel/user/gdi/advapi/ntdll and never admits game-specific filenames.
pub fn is_allowed_system_dll(name: &str) -> bool {
    if !is_bare_dll_name(name) || !name.ends_with(".dll") {
        return false;
    }
    let stem = &name[..name.len() - 4];
    const EXACT: &[&str] = &[
        "amstream",
        "avicap32",
        "avifil32",
        "bcrypt",
        "clr",
        "comctl32",
        "comdlg32",
        "crypt32",
        "d2d1",
        "d3d8",
        "d3d9",
        "d3d10",
        "d3d10_1",
        "d3d11",
        "d3d12",
        "d3dim",
        "d3dim700",
        "dbghelp",
        "ddraw",
        "devenum",
        "dinput",
        "dinput8",
        "dmime",
        "dmloader",
        "dmscript",
        "dmstyle",
        "dmsynth",
        "dmusic",
        "dplayx",
        "dsdmo",
        "dsound",
        "dwmapi",
        "dwrite",
        "dxgi",
        "dxva2",
        "evr",
        "gdiplus",
        "glu32",
        "imm32",
        "iphlpapi",
        "jscript",
        "mf",
        "mfplat",
        "mfreadwrite",
        "mpr",
        "msacm32",
        "mscoree",
        "msdmo",
        "mshtml",
        "msimg32",
        "msvfw32",
        "msvcrt",
        "ncrypt",
        "netapi32",
        "ole32",
        "oleacc",
        "oleaut32",
        "opengl32",
        "powrprof",
        "propsys",
        "psapi",
        "quartz",
        "riched20",
        "riched32",
        "rpcrt4",
        "secur32",
        "setupapi",
        "shell32",
        "shfolder",
        "shlwapi",
        "ucrtbase",
        "uiautomationcore",
        "urlmon",
        "userenv",
        "usp10",
        "uxtheme",
        "vbscript",
        "version",
        "vulkan-1",
        "winhttp",
        "wininet",
        "winmm",
        "winspool",
        "wintrust",
        "wmvcore",
        "ws2_32",
        "wsock32",
        "wtsapi32",
    ];
    if EXACT.contains(&stem) {
        return true;
    }
    const VERSIONED_PREFIXES: &[&str] = &[
        "api-ms-win-",
        "d3dcompiler_",
        "d3dx9_",
        "d3dx10_",
        "d3dx11_",
        "mfc",
        "msvcp",
        "msvcr",
        "vcomp",
        "vcruntime",
        "x3daudio",
        "xapofx",
        "xaudio2_",
        "xinput",
    ];
    VERSIONED_PREFIXES.iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|rest| {
            !rest.is_empty()
                && rest.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_')
                })
        })
    })
}

/// A key that lets the server merge repeated reports for one game from one
/// installation without learning anything else: `secret` is random per
/// installation and never leaves it.
pub fn dedupe_key(secret: &[u8; 32], exe_sha256: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"hitode.report/2 dedupe\0");
    hasher.update(secret);
    hasher.update(exe_sha256.as_bytes());
    hex(&hasher.finalize()[..8])
}

pub fn sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Issues,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Issue {
    NoStart,
    Freeze,
    Crash,
    Stutter,
    Graphics,
    SoundVideo,
    Input,
    Save,
    Heat,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerfIssue {
    Stutter,
    FastForward,
    FirstLaunch,
    Loading,
    AudioDropout,
    InputLatency,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    LiveSnapshot,
    GuestExit,
    GuestFailure,
    HostError,
    Panic,
    UserClose,
    UserForceClose,
}

/// Execution mode and termination metadata for a single run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunInfo {
    pub jit: bool,
    pub stop: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<String>,
    pub processes: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed_is_child: bool,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub dialogs_shown: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub present_size: Option<[u32; 2]>,
}
impl RunInfo {
    fn validate(&self) -> Result<(), String> {
        if let Some(code) = &self.exit_code
            && !is_hex_u32(code)
        {
            return Err(format!("report exit_code {code:?} is not 0x hex u32"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The guest called an API the runtime does not implement.
    Api,
    /// The NT, Unix or driver layer encountered an unsupported request or class.
    NtUnsupported,
    /// A required DLL could not be loaded (`STATUS_DLL_NOT_FOUND`).
    MissingDll,
    /// An unhandled guest CPU fault or terminal exception ended the game.
    Cpu,
    /// A classified host-side error (address space, GPU, audio, server, loader).
    HostError,
    /// A Rust panic occurred in the runtime or a worker thread.
    HostPanic,
    /// Any other unclassified runtime failure; its message may name host paths,
    /// so only the kind is reported.
    Runtime,
}

/// How long the game ran before it failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureAfter {
    #[serde(rename = "lt_1m")]
    LessThanMinute,
    #[serde(rename = "1m_10m")]
    Minutes,
    #[serde(rename = "10m_1h")]
    UnderHour,
    #[serde(rename = "gt_1h")]
    Hours,
}
impl FailureAfter {
    pub fn from_elapsed(elapsed: Duration) -> Self {
        match elapsed.as_secs() {
            0..60 => Self::LessThanMinute,
            60..600 => Self::Minutes,
            600..3600 => Self::UnderHour,
            _ => Self::Hours,
        }
    }
}

/// Sanitized metadata of a host Rust panic, without host paths or guest bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPanic {
    pub thread: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translating: Option<String>,
}
impl HostPanic {
    fn validate(&self) -> Result<(), String> {
        if self.thread.is_empty()
            || self.thread.len() > 64
            || !self
                .thread
                .bytes()
                .all(|b| b.is_ascii_graphic() || b == b' ')
            || self.thread.contains(['/', '\\'])
        {
            return Err("report panic thread must be bounded printable ASCII without paths".into());
        }
        if let Some(location) = &self.location {
            let valid_prefix = location.starts_with("crates/") || location.starts_with("apps/");
            if !valid_prefix
                || location.len() > 160
                || location.contains("..")
                || location.contains('\\')
                || !location.bytes().all(|b| b.is_ascii_graphic())
            {
                return Err(format!(
                    "report panic location {location:?} must be a workspace-relative path"
                ));
            }
        }
        if let Some(reason) = &self.reason
            && (reason.chars().count() > 200
                || reason.chars().any(char::is_control)
                || reason.contains(['/', '\\']))
        {
            return Err("report panic reason must be bounded text without paths".into());
        }
        if let Some(translating) = &self.translating
            && !is_safe_token(translating, 96)
        {
            return Err(format!(
                "report panic translating {translating:?} must be a safe ASCII token"
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub kind: FailureKind,
    /// `module!name` of the unimplemented API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// Origin or subsystem tag, e.g. `wine_stub`, `nt_info`, `wineserver`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Operation or error tag, e.g. `NtQueryInformationToken`, `event_watcher_io`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    /// Status, exception, class or errno tag, e.g. `0xc0000005`, `class:3`, `errno:9`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Missing DLL names (bare ASCII filenames, no paths).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dlls: Vec<String>,
    /// Fault kind, address and guest PC, e.g. `read 0x00000000 at exe+0x1a2c`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_fault: Option<String>,
    /// Single `iced_x86::Code` enum name at the faulting PC; never bytes or disassembly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault_insn: Option<String>,
    /// Anonymized guest call stack frames or relative host offsets (`+0x...`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stack: Vec<String>,
    /// Sanitized host panic metadata when `kind == HostPanic`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panic: Option<HostPanic>,
    pub after: FailureAfter,
}
impl Failure {
    fn validate(&self) -> Result<(), String> {
        if let Some(api) = &self.api
            && !api_name(api)
        {
            return Err(format!("report failure api {api:?} is not module!name"));
        }
        if let Some(source) = &self.source
            && !is_safe_token(source, 64)
        {
            return Err(format!("report failure source {source:?} is invalid"));
        }
        if let Some(op) = &self.op
            && !is_safe_token(op, 96)
        {
            return Err(format!("report failure op {op:?} is invalid"));
        }
        if let Some(code) = &self.code
            && !is_safe_token(code, 64)
        {
            return Err(format!("report failure code {code:?} is invalid"));
        }
        if self.dlls.len() > MAX_MISSING_DLLS {
            return Err(format!("report lists over {MAX_MISSING_DLLS} missing DLLs"));
        }
        if let Some(dll) = self.dlls.iter().find(|dll| !is_bare_dll_name(dll)) {
            return Err(format!("report missing DLL {dll:?} is invalid"));
        }
        if let Some(fault) = &self.cpu_fault
            && (fault.is_empty()
                || fault.len() > 240
                || !fault.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
                || fault.contains(['/', '\\']))
        {
            return Err("report cpu_fault must be bounded printable ASCII without paths".into());
        }
        if let Some(insn) = &self.fault_insn
            && !is_safe_token(insn, 64)
        {
            return Err(format!("report fault_insn {insn:?} is invalid"));
        }
        if self.stack.len() > MAX_STACK_FRAMES {
            return Err(format!("report lists over {MAX_STACK_FRAMES} stack frames"));
        }
        if let Some(frame) = self.stack.iter().find(|frame| !is_stack_frame(frame)) {
            return Err(format!("report stack frame {frame:?} is invalid"));
        }
        if let Some(panic) = &self.panic {
            panic.validate()?;
        }
        Ok(())
    }
}

/// Snapshot of guest threads and host boundaries captured when a run hangs,
/// is force-closed, or is reported live from the pause menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HangSnapshot {
    pub exec_thread_responsive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_wait_pct: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<ThreadSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_boundaries: Vec<BoundaryStat>,
}
impl HangSnapshot {
    fn validate(&self) -> Result<(), String> {
        if self.idle_wait_pct.is_some_and(|pct| pct > 100) {
            return Err("report idle_wait_pct must be 0..=100".into());
        }
        if self.threads.len() > MAX_HANG_THREADS {
            return Err(format!("report lists over {MAX_HANG_THREADS} threads"));
        }
        for thread in &self.threads {
            thread.validate()?;
        }
        if self.top_boundaries.len() > MAX_HANG_BOUNDARIES {
            return Err(format!(
                "report lists over {MAX_HANG_BOUNDARIES} boundary entries"
            ));
        }
        for boundary in &self.top_boundaries {
            boundary.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadSnapshot {
    pub tid: u32,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_api: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stack: Vec<String>,
}
impl ThreadSnapshot {
    fn validate(&self) -> Result<(), String> {
        if !is_safe_token(&self.state, 32) {
            return Err(format!("report thread state {:?} is invalid", self.state));
        }
        if let Some(api) = &self.blocked_api
            && !(api_name(api) || is_safe_token(api, 96))
        {
            return Err(format!("report blocked_api {api:?} is invalid"));
        }
        if self.stack.len() > 8 {
            return Err("report thread stack exceeds 8 frames".into());
        }
        if let Some(frame) = self.stack.iter().find(|f| !is_stack_frame(f)) {
            return Err(format!("report thread stack frame {frame:?} is invalid"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundaryStat {
    pub kind: String,
    pub name: String,
    pub calls: u64,
    pub host_ms: u64,
}
impl BoundaryStat {
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.kind.as_str(), "syscall" | "unix" | "server") {
            return Err(format!("report boundary kind {:?} is invalid", self.kind));
        }
        if !is_safe_token(&self.name, 96) {
            return Err(format!("report boundary name {:?} is invalid", self.name));
        }
        Ok(())
    }
}

/// Non-fatal subsystem diagnostics, capability probes, Wine channel counts and
/// detected hooks, strictly bounded and stripped of paths and guest text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostics {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<WarningStat>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected_probes: Vec<ProbeRejection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wine_errors: Vec<WineLogStat>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooked_exports: Vec<HookedExport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exceptions_seen: Vec<ExceptionStat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphics: Option<GraphicsDiag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_video: Option<AudioVideoDiag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fonts: Option<FontDiag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<WindowDiag>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_panics: Vec<HostPanic>,
}
impl Diagnostics {
    pub fn is_empty(&self) -> bool {
        self.warnings.is_empty()
            && self.rejected_probes.is_empty()
            && self.wine_errors.is_empty()
            && self.hooked_exports.is_empty()
            && self.exceptions_seen.is_empty()
            && self.graphics.is_none()
            && self.audio_video.is_none()
            && self.fonts.is_none()
            && self.windows.is_none()
            && self.host_panics.is_empty()
    }

    fn validate(&self) -> Result<(), String> {
        if self.warnings.len() > MAX_DIAG_ENTRIES {
            return Err(format!("report lists over {MAX_DIAG_ENTRIES} warnings"));
        }
        for w in &self.warnings {
            if !(api_name(&w.api) || is_safe_token(&w.api, 96)) {
                return Err(format!("report warning api {:?} is invalid", w.api));
            }
            if !is_safe_reason(&w.reason, 140) {
                return Err("report warning reason must be bounded ASCII without paths".into());
            }
        }
        if self.rejected_probes.len() > MAX_PROBE_ENTRIES {
            return Err(format!(
                "report lists over {MAX_PROBE_ENTRIES} rejected probes"
            ));
        }
        for p in &self.rejected_probes {
            if !(api_name(&p.api) || is_safe_token(&p.api, 96)) {
                return Err(format!("report probe api {:?} is invalid", p.api));
            }
            if !is_safe_reason(&p.detail, 120) {
                return Err("report probe detail must be bounded ASCII without paths".into());
            }
        }
        if self.wine_errors.len() > MAX_DIAG_ENTRIES {
            return Err(format!("report lists over {MAX_DIAG_ENTRIES} wine errors"));
        }
        for e in &self.wine_errors {
            if !matches!(e.level.as_str(), "err" | "fixme")
                || !is_safe_token(&e.channel, 32)
                || !is_safe_token(&e.func, 64)
            {
                return Err("report wine_error entry is invalid".into());
            }
        }
        if self.hooked_exports.len() > MAX_DIAG_ENTRIES {
            return Err(format!(
                "report lists over {MAX_DIAG_ENTRIES} hooked exports"
            ));
        }
        for h in &self.hooked_exports {
            if !api_name(&h.api) || !is_safe_token(&h.form, 32) {
                return Err("report hooked_export entry is invalid".into());
            }
        }
        if self.exceptions_seen.len() > MAX_PROBE_ENTRIES {
            return Err(format!(
                "report lists over {MAX_PROBE_ENTRIES} exception stats"
            ));
        }
        for ex in &self.exceptions_seen {
            if !is_hex_u32(&ex.code) || !ex.last_pc.as_deref().is_none_or(is_stack_frame) {
                return Err("report exception stat is invalid".into());
            }
        }
        if let Some(g) = &self.graphics
            && (g.fronts.len() > 8 || g.fronts.iter().any(|f| !is_safe_token(f, 32)))
        {
            return Err("report graphics fronts are invalid".into());
        }
        if let Some(av) = &self.audio_video
            && (av.audio_fronts.len() > 8
                || av.audio_fronts.iter().any(|f| !is_safe_token(f, 32))
                || av.video_fronts.len() > 8
                || av.video_fronts.iter().any(|f| !is_safe_token(f, 32))
                || av.dsound_create_failures.len() > MAX_PROBE_ENTRIES
                || av
                    .dsound_create_failures
                    .keys()
                    .any(|k| !is_safe_token(k, 64)))
        {
            return Err("report audio_video diagnostics are invalid".into());
        }
        if let Some(f) = &self.fonts
            && (f.missing_standard.len() > MAX_PROBE_ENTRIES
                || f.missing_standard
                    .iter()
                    .any(|name| !is_standard_font_family(name))
                || f.charsets.len() > MAX_PROBE_ENTRIES
                || f.gdi_codepages.len() > 8)
        {
            return Err("report font diagnostics are invalid".into());
        }
        if self.host_panics.len() > 4 {
            return Err("report lists over 4 host panics".into());
        }
        for panic in &self.host_panics {
            panic.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarningStat {
    pub api: String,
    pub reason: String,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeRejection {
    pub api: String,
    pub detail: String,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WineLogStat {
    pub level: String,
    pub channel: String,
    pub func: String,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookedExport {
    pub api: String,
    pub form: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExceptionStat {
    pub code: String,
    pub count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pc: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphicsDiag {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fronts: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub d3d9_draws: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub d3d9_readback_kb: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioVideoDiag {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_fronts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub video_fronts: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dsound_create_failures: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontDiag {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_standard: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub substitutions: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub charsets: Vec<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gdi_codepages: Vec<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowDiag {
    pub top_level: u32,
    pub visible: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub dialogs: u32,
}

/// Standard Windows font family allowlist (ASCII canonical names) so reports
/// can flag missing CJK/system faces without leaking custom font filenames.
pub fn canonical_standard_font(face: &str) -> Option<&'static str> {
    let trimmed = face.trim().trim_start_matches('@');
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "simsun" | "nsimsun" | "宋体" | "新宋体" => Some("SimSun"),
        "simhei" | "黑体" => Some("SimHei"),
        "fangsong" | "仿宋" => Some("FangSong"),
        "kaiti" | "楷体" => Some("KaiTi"),
        "microsoft yahei" | "微软雅黑" => Some("Microsoft YaHei"),
        "ms gothic" | "ms pgothic" | "ms ui gothic" | "ｍｓ ゴシック" | "ｍｓ ｐゴシック" => {
            Some("MS Gothic")
        }
        "ms mincho" | "ms pmincho" | "ｍｓ 明朝" | "ｍｓ ｐ明朝" => Some("MS Mincho"),
        "meiryo" | "meiryo ui" | "メイリオ" => Some("Meiryo"),
        "yu gothic" | "游ゴシック" => Some("Yu Gothic"),
        "yu mincho" | "游明朝" => Some("Yu Mincho"),
        "mingliu" | "pmingliu" | "細明體" | "新細明體" => Some("MingLiU"),
        "microsoft jhenghei" | "微軟正黑體" => Some("Microsoft JhengHei"),
        "gulim" | "batang" | "dotum" | "gungsuh" => Some("Gulim"),
        "arial" => Some("Arial"),
        "tahoma" => Some("Tahoma"),
        "verdana" => Some("Verdana"),
        "times new roman" => Some("Times New Roman"),
        "courier new" => Some("Courier New"),
        "segoe ui" => Some("Segoe UI"),
        _ => None,
    }
}

pub fn is_standard_font_family(name: &str) -> bool {
    canonical_standard_font(name).is_some_and(|canon| canon == name)
}

fn is_safe_reason(text: &str, max_len: usize) -> bool {
    !text.is_empty()
        && text.len() <= max_len
        && text
            .bytes()
            .all(|b| (b.is_ascii_graphic() || b == b' ') && !matches!(b, b'/' | b'\\'))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuShare {
    pub translated_pct: u8,
    pub apply_pct: u8,
    pub interpret_pct: u8,
    pub interpreter_pct: u8,
    pub rep_pct: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliffStat {
    pub name: String,
    pub count: u64,
}

/// Guest execution time per presented frame, rounded to 0.1 ms, plus optional
/// low-cost execution attribution counters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Perf {
    pub frame_ms_p50: f64,
    pub frame_ms_p95: f64,
    pub frame_ms_p99: f64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub long_frames_50ms: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub long_frames_250ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_share: Option<CpuShare>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_cliffs: Vec<CliffStat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_heap_mb: Option<u32>,
}
impl Perf {
    pub fn from_samples(samples: impl IntoIterator<Item = f64>) -> Option<Self> {
        let mut values: Vec<f64> = samples.into_iter().collect();
        if values.is_empty() {
            return None;
        }
        let long_frames_50ms = values.iter().filter(|&&ms| ms >= 50.0).count() as u64;
        let long_frames_250ms = values.iter().filter(|&&ms| ms >= 250.0).count() as u64;
        values.sort_by(f64::total_cmp);
        let at = |percent: usize| {
            let rank = (values.len() * percent).div_ceil(100).saturating_sub(1);
            (values[rank] * 10.0).round() / 10.0
        };
        Some(Self {
            frame_ms_p50: at(50),
            frame_ms_p95: at(95),
            frame_ms_p99: at(99),
            long_frames_50ms,
            long_frames_250ms,
            cpu_share: None,
            top_cliffs: Vec::new(),
            peak_heap_mb: None,
        })
    }

    fn validate(&self) -> Result<(), String> {
        if let Some(share) = &self.cpu_share
            && [
                share.translated_pct,
                share.apply_pct,
                share.interpret_pct,
                share.interpreter_pct,
                share.rep_pct,
            ]
            .into_iter()
            .any(|p| p > 100)
        {
            return Err("report cpu_share percentages must be 0..=100".into());
        }
        if self.top_cliffs.len() > MAX_CLIFF_ENTRIES {
            return Err(format!(
                "report lists over {MAX_CLIFF_ENTRIES} cliff entries"
            ));
        }
        if let Some(cliff) = self
            .top_cliffs
            .iter()
            .find(|c| !is_safe_reason(&c.name, 64))
        {
            return Err(format!("report cliff name {:?} is invalid", cliff.name));
        }
        Ok(())
    }
}

/// Unimplemented APIs a run met, as sorted `module!name` lists.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiGaps {
    /// Unimplemented APIs called by the guest.
    pub called: Vec<String>,
    /// Imported by a loaded module but not called.
    pub imported: Vec<String>,
}
impl ApiGaps {
    pub const MAX_NAMES: usize = 256;
    const MAX_NAME_BYTES: usize = 160;

    /// The gaps of a run, sorted and bounded; `None` when there are none.
    pub fn new(
        called: impl IntoIterator<Item = String>,
        imported: impl IntoIterator<Item = String>,
    ) -> Option<Self> {
        let list = |names: &mut dyn Iterator<Item = String>, skip: &[String]| {
            let mut names: Vec<String> = names
                .filter(|name| api_name(name))
                .map(|name| match name.split_once('!') {
                    Some((module, export)) => format!("{}!{export}", module.to_ascii_lowercase()),
                    None => name,
                })
                .filter(|name| !skip.contains(name))
                .collect();
            names.sort();
            names.dedup();
            names.truncate(Self::MAX_NAMES);
            names
        };
        let called = list(&mut called.into_iter(), &[]);
        let imported = list(&mut imported.into_iter(), &called);
        (!called.is_empty() || !imported.is_empty()).then_some(Self { called, imported })
    }

    fn validate(&self) -> Result<(), String> {
        for names in [&self.called, &self.imported] {
            if names.len() > Self::MAX_NAMES {
                return Err(format!("report lists over {} API gaps", Self::MAX_NAMES));
            }
            if let Some(name) = names.iter().find(|name| !api_name(name)) {
                return Err(format!("report API gap {name:?} is not module!name"));
            }
        }
        Ok(())
    }
}

/// `module!name` in printable ASCII without path or debug-format characters.
pub fn api_name(name: &str) -> bool {
    name.len() <= ApiGaps::MAX_NAME_BYTES
        && name.bytes().all(|b| {
            b.is_ascii_graphic() && !matches!(b, b'/' | b'\\' | b'"' | b'\'' | b'{' | b'}' | b',')
        })
        && name
            .split_once('!')
            .is_some_and(|(module, export)| !module.is_empty() && !export.is_empty())
}

/// Printable ASCII identifier token without whitespace, quotes or path separators.
pub fn is_safe_token(text: &str, max_len: usize) -> bool {
    !text.is_empty()
        && text.len() <= max_len
        && text.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':' | b'+' | b'#')
        })
}

/// Bare ASCII DLL filename without path separators.
pub fn is_bare_dll_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 80
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
}

/// Anonymized guest frame (`exe+0x12`, `ntdll.dll!Func+0x12`, `game#12345678+0x12`,
/// `0x00401234`) or host offset (`+0x1234`, `crate::fn+0x12`).
fn is_stack_frame(frame: &str) -> bool {
    !frame.is_empty()
        && frame.len() <= 160
        && frame.bytes().all(|b| {
            b.is_ascii_graphic() && !matches!(b, b'/' | b'\\' | b'"' | b'\'' | b'{' | b'}')
        })
}

/// What a finished run contributes to a report, before the user describes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    #[serde(default)]
    pub run: Option<RunInfo>,
    pub failure: Option<Failure>,
    #[serde(default)]
    pub hang: Option<HangSnapshot>,
    #[serde(default)]
    pub diagnostics: Option<Diagnostics>,
    pub perf: Option<Perf>,
    #[serde(default)]
    pub api_gaps: Option<ApiGaps>,
    #[serde(default)]
    pub timeline: Option<Timeline>,
    #[serde(default)]
    pub settings: Option<Settings>,
    #[serde(default)]
    pub graphics: Option<Graphics>,
}

/// Milliseconds since the guest started; absent events never happened.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<u64>,
    pub frames: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_frame: Option<u64>,
    pub end: u64,
}

/// The report file, pretty-printed in wire order.
pub fn to_json(report: &Report) -> Result<String, String> {
    report.validate()?;
    serde_json::to_string_pretty(report).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report {
            schema: SCHEMA.into(),
            dedupe_key: "0123456789abcdef".into(),
            app: App {
                version: "0.1.0".into(),
                build: "8c048ed6".into(),
                channel: "preview".into(),
            },
            platform: Platform {
                os: "macos".into(),
                os_major: "26".into(),
                model: "Example1,1".into(),
                increased_memory_limit: None,
                address_space: None,
                graphics: Graphics {
                    api: GraphicsApi::Metal,
                    name: "Example GPU".into(),
                    vendor: "apple".into(),
                    device: "Example GPU".into(),
                    driver: "26".into(),
                    tier: "metal".into(),
                },
                settings: Settings {
                    upscaler: Some(Upscaler::Off),
                    scaling: Some(Scaling::Fit),
                    fast_forward: Some(FastForward::Hold),
                    touch_mode: Some(TouchMode::Direct),
                    font_files: 0,
                    font_overrides: 0,
                    font_smoothing: None,
                    discs: 0,
                },
            },
            game: Game {
                exe_sha256: "ab".repeat(32),
                exe_size: 1887437,
                codepage: 932,
                timestamp: 123,
                image_size: 8192,
                file_version: None,
                arch: Some(PeArch::X86),
                subsystem: Some(PeSubsystem::Gui),
                sections: Some(4),
                overlay_bytes: None,
                system_imports: vec!["d3d9.dll".into()],
            },
            title: None,
            outcome: Outcome::Failed,
            issues: vec![],
            perf_issues: None,
            note: None,
            run: Some(RunInfo {
                jit: true,
                stop: StopReason::GuestFailure,
                exit_code: None,
                processes: 1,
                failed_is_child: false,
                dialogs_shown: 0,
                present_size: None,
            }),
            failure: Some(Failure {
                kind: FailureKind::Api,
                api: Some("d3d9.dll!IDirect3DDevice9::CreateVolumeTexture".into()),
                source: Some("abi_stub".into()),
                op: None,
                code: None,
                dlls: vec![],
                cpu_fault: None,
                fault_insn: None,
                stack: vec!["exe+0x1234".into()],
                panic: None,
                after: FailureAfter::Minutes,
            }),
            hang: None,
            diagnostics: None,
            perf: Some(Perf {
                frame_ms_p50: 16.8,
                frame_ms_p95: 41.2,
                frame_ms_p99: 88.5,
                long_frames_50ms: 0,
                long_frames_250ms: 0,
                cpu_share: None,
                top_cliffs: vec![],
                peak_heap_mb: None,
            }),
            timeline: None,
            api_gaps: ApiGaps::new(
                ["gdiplus.dll!GdipDrawCurve".into()],
                [
                    "gdiplus.dll!GdipDrawCurve".into(),
                    "ole32.dll!CoCreateGuid".into(),
                ],
            ),
        }
    }

    // spec: REPORT-FMT-2
    #[test]
    fn wire_format_matches_the_launcher_schema() {
        let json = serde_json::to_string(&report()).unwrap();
        let expected = [
            r#"{"schema":"hitode.report/3","dedupe_key":"0123456789abcdef","#,
            r#""app":{"version":"0.1.0","build":"8c048ed6","channel":"preview"},"#,
            r#""platform":{"os":"macos","os_major":"26","model":"Example1,1","graphics":{"api":"metal","name":"Example GPU","vendor":"apple","device":"Example GPU","driver":"26","tier":"metal"},"settings":{"upscaler":"off","scaling":"fit","fast_forward":"hold","touch_mode":"direct","font_files":0,"font_overrides":0,"font_smoothing":null,"discs":0}},"#,
            &format!(
                r#""game":{{"exe_sha256":"{}","exe_size":1887437,"codepage":932,"timestamp":123,"image_size":8192,"arch":"x86","subsystem":"gui","sections":4,"system_imports":["d3d9.dll"]}},"#,
                "ab".repeat(32)
            ),
            r#""outcome":"failed","issues":[],"#,
            r#""run":{"jit":true,"stop":"guest_failure","processes":1},"#,
            r#""failure":{"kind":"api","api":"d3d9.dll!IDirect3DDevice9::CreateVolumeTexture","source":"abi_stub","stack":["exe+0x1234"],"after":"1m_10m"},"#,
            r#""perf":{"frame_ms_p50":16.8,"frame_ms_p95":41.2,"frame_ms_p99":88.5},"#,
            r#""api_gaps":{"called":["gdiplus.dll!GdipDrawCurve"],"imported":["ole32.dll!CoCreateGuid"]}}"#,
        ]
        .concat();
        assert_eq!(json, expected);
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), report());
        let issues = Report {
            outcome: Outcome::Issues,
            issues: vec![Issue::Stutter],
            perf_issues: Some(vec![PerfIssue::FastForward, PerfIssue::AudioDropout]),
            note: Some("BGM".into()),
            ..report()
        };
        let json = serde_json::to_string(&issues).unwrap();
        assert!(json.contains(
            r#""outcome":"issues","issues":["stutter"],"perf_issues":["fast_forward","audio_dropout"],"note":"BGM""#
        ));
    }

    // spec: REPORT-FMT-1
    #[test]
    fn unknown_fields_and_other_schemas_are_rejected() {
        let mut value = serde_json::to_value(report()).unwrap();
        value["game"]["path"] = "C:\\Games\\AQUA.exe".into();
        assert!(serde_json::from_value::<Report>(value).is_err());
        let other = Report {
            schema: "hitode.report/2".into(),
            ..report()
        };
        assert!(to_json(&other).is_err());
        assert!(serde_json::from_str::<Report>(&serde_json::to_string(&other).unwrap()).is_err());
        assert!(to_json(&report()).unwrap().starts_with("{\n  \"schema\""));
    }

    // spec: REPORT-FAIL-1
    #[test]
    fn structured_failures_reject_paths_and_debug_garbage() {
        let mut polluted = report();
        polluted.failure.as_mut().unwrap().api =
            Some(r#"libc.so.6!__res_state", recent: [] }"#.into());
        assert!(to_json(&polluted).is_err());

        let mut panic_report = report();
        panic_report.failure = Some(Failure {
            kind: FailureKind::HostPanic,
            api: None,
            source: None,
            op: None,
            code: None,
            dlls: vec![],
            cpu_fault: None,
            fault_insn: None,
            stack: vec!["+0x1a4f20".into()],
            panic: Some(HostPanic {
                thread: "guest compile".into(),
                location: Some("crates/adv-cpu-a64/src/encode.rs:412".into()),
                reason: Some("index out of bounds".into()),
                translating: Some("Cvttss2si_r32_xmmm32".into()),
            }),
            after: FailureAfter::LessThanMinute,
        });
        assert!(to_json(&panic_report).is_ok());

        panic_report
            .failure
            .as_mut()
            .unwrap()
            .panic
            .as_mut()
            .unwrap()
            .location = Some("/abs/path/crates/adv-cpu-a64/src/encode.rs:412".into());
        assert!(to_json(&panic_report).is_err());

        let mut bad_dll = report();
        bad_dll.failure = Some(Failure {
            kind: FailureKind::MissingDll,
            api: None,
            source: None,
            op: None,
            code: Some("0xc0000135".into()),
            dlls: vec!["C:\\Windows\\mfc42.dll".into()],
            cpu_fault: None,
            fault_insn: None,
            stack: vec![],
            panic: None,
            after: FailureAfter::LessThanMinute,
        });
        assert!(to_json(&bad_dll).is_err());

        let mut bad_import = report();
        bad_import.game.system_imports = vec!["my_secret_game_patch.dll".into()];
        assert!(to_json(&bad_import).is_err());
    }

    // spec: REPORT-PLAT-1, IOS-MEM-5
    #[test]
    fn address_spaces_are_hex_and_bounded() {
        let mut report = report();
        report.platform.increased_memory_limit = Some(true);
        report.platform.address_space = Some(AddressSpace::new(
            0x4_5800_0000,
            None,
            [0x2_fffe_0000..0x3_0002_0000, 0x3_8000_0000..0x3_9000_0000],
        ));
        let json = to_json(&report).unwrap();
        assert!(json.contains(
            r#""increased_memory_limit": true,
    "address_space": {
      "max": "0x458000000",
      "mapped": [
        "0x2fffe0000-0x300020000",
        "0x380000000-0x390000000"
      ]
    },"#
        ));
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), report);
        let space = report.platform.address_space.as_mut().unwrap();
        space.mapped = vec!["0x1-0x2".into(); MAPPED_RANGES + 1];
        assert!(to_json(&report).is_err());
        let space = report.platform.address_space.as_mut().unwrap();
        space.mapped.clear();
        space.window = Some("C:\\Games".into());
        assert!(to_json(&report).is_err());
    }

    // spec: REPORT-FMT-3
    #[test]
    fn notes_over_2000_characters_are_rejected() {
        let note = |n| Report {
            note: Some("字".repeat(n)),
            ..report()
        };
        assert!(to_json(&note(2000)).is_ok());
        assert!(to_json(&note(2001)).is_err());
    }

    // spec: REPORT-FMT-1, REPORT-FMT-2, REPORT-PLAT-1
    #[test]
    fn settings_are_typed_and_unobserved_choices_are_absent() {
        let mut report = report();
        for (field, value) in [
            ("upscaler", "Off"),
            ("scaling", "int"),
            ("fast_forward", "rapid"),
            ("touch_mode", "relative"),
        ] {
            let mut invalid = serde_json::to_value(&report).unwrap();
            invalid["platform"]["settings"][field] = value.into();
            assert!(
                serde_json::from_value::<Report>(invalid).is_err(),
                "{field}"
            );
        }
        report.platform.settings.upscaler = Some(Upscaler::Anime4k);
        report.platform.settings.scaling = Some(Scaling::Integer);
        report.platform.settings.fast_forward = Some(FastForward::Toggle);
        report.platform.settings.touch_mode = Some(TouchMode::Trackpad);
        let json = to_json(&report).unwrap();
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), report);
        let value = serde_json::to_value(&report.platform.settings).unwrap();
        assert_eq!(value["upscaler"], "anime4k");
        assert_eq!(value["scaling"], "integer");
        assert_eq!(value["fast_forward"], "toggle");
        assert_eq!(value["touch_mode"], "trackpad");
        report.platform.settings.scaling = Some(Scaling::Custom);
        let json = to_json(&report).unwrap();
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), report);
        assert_eq!(
            serde_json::to_value(&report.platform.settings).unwrap()["scaling"],
            "custom"
        );
        report.platform.settings.upscaler = None;
        report.platform.settings.scaling = None;
        report.platform.settings.fast_forward = None;
        report.platform.settings.touch_mode = None;
        let value = serde_json::to_value(&report.platform.settings).unwrap();
        for field in ["upscaler", "scaling", "fast_forward", "touch_mode"] {
            assert!(value.get(field).is_none(), "{field}");
        }
    }

    // spec: REPORT-ID-1, REPORT-FMT-3
    #[test]
    fn title_is_optional_and_edition_is_bounded() {
        let mut report = report();
        assert!(!to_json(&report).unwrap().contains("\"title\""));
        report.title = Some(Title {
            vndb: "v123".into(),
            name: "Example".into(),
            edition: "字".repeat(80),
        });
        let json = to_json(&report).unwrap();
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), report);
        report.title.as_mut().unwrap().edition.push('字');
        assert!(to_json(&report).is_err());
        report.title.as_mut().unwrap().edition.clear();
        report.title.as_mut().unwrap().vndb = "123".into();
        assert!(to_json(&report).is_err());
        report.title.as_mut().unwrap().vndb = "v123".into();
        report.title.as_mut().unwrap().name = "bad\nname".into();
        assert!(to_json(&report).is_err());
    }

    // spec: REPORT-GAP-1
    #[test]
    fn api_gaps_are_sorted_bounded_names() {
        assert_eq!(ApiGaps::new([], []), None);
        let gaps = ApiGaps::new(
            ["b.dll!Two".into(), "A.dll!One".into(), "a.dll!One".into()],
            [
                "a.dll!One".into(),
                "C:\\Games\\x.dll!Path is text".into(),
                "no-module".into(),
                "c.dll!?Name@@YAXXZ".into(),
            ],
        )
        .unwrap();
        assert_eq!(gaps.called, ["a.dll!One", "b.dll!Two"]);
        assert_eq!(gaps.imported, ["c.dll!?Name@@YAXXZ"]);
        let many = ApiGaps::new((0..300).map(|i| format!("a.dll!F{i:03}")), []).unwrap();
        assert_eq!(many.called.len(), ApiGaps::MAX_NAMES);
        let with = |gaps: ApiGaps| Report {
            api_gaps: Some(gaps),
            ..report()
        };
        assert!(to_json(&with(many.clone())).is_ok());
        let mut over = many;
        over.called.push("a.dll!F999".into());
        assert!(to_json(&with(over)).is_err());
        let text = ApiGaps {
            called: vec!["user note".into()],
            imported: vec![],
        };
        assert!(to_json(&with(text)).is_err());
    }

    // spec: REPORT-FAIL-2
    #[test]
    fn failure_time_buckets() {
        let after = |s| FailureAfter::from_elapsed(Duration::from_secs(s));
        assert_eq!(after(59), FailureAfter::LessThanMinute);
        assert_eq!(after(60), FailureAfter::Minutes);
        assert_eq!(after(599), FailureAfter::Minutes);
        assert_eq!(after(600), FailureAfter::UnderHour);
        assert_eq!(after(3600), FailureAfter::Hours);
    }

    // spec: REPORT-PERF-1
    #[test]
    fn percentiles_use_nearest_rank_and_round() {
        assert_eq!(Perf::from_samples([]), None);
        let perf = Perf::from_samples((1..=100).rev().map(|ms| f64::from(ms) + 0.04)).unwrap();
        assert_eq!(
            perf,
            Perf {
                frame_ms_p50: 50.0,
                frame_ms_p95: 95.0,
                frame_ms_p99: 99.0,
                long_frames_50ms: 51,
                long_frames_250ms: 0,
                cpu_share: None,
                top_cliffs: vec![],
                peak_heap_mb: None,
            }
        );
    }

    // spec: REPORT-ID-1, REPORT-ID-2
    #[test]
    fn identity_and_dedupe_key() {
        let dir = std::env::temp_dir().join(format!("adv-report-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("game.exe");
        let mut writer = adv_pe::write::DllWriter::new("fixture.exe", 0x400000);
        let mut resource = vec![0u8; 188];
        let put16 = |bytes: &mut [u8], at: usize, value: u16| {
            bytes[at..at + 2].copy_from_slice(&value.to_le_bytes())
        };
        let put32 = |bytes: &mut [u8], at: usize, value: u32| {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes())
        };
        for (at, id, child) in [(0, 16, 0x80000018), (24, 1, 0x80000030), (48, 0, 72)] {
            put16(&mut resource, at + 14, 1);
            put32(&mut resource, at + 16, id);
            put32(&mut resource, at + 20, child);
        }
        put32(&mut resource, 72, 0x1060);
        put32(&mut resource, 76, 92);
        put16(&mut resource, 96, 92);
        put16(&mut resource, 98, 52);
        let key: Vec<u8> = "VS_VERSION_INFO\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        resource[102..102 + key.len()].copy_from_slice(&key);
        put32(&mut resource, 136, 0xfeef04bd);
        put32(&mut resource, 140, 0x10000);
        put32(&mut resource, 144, 0x00010002);
        put32(&mut resource, 148, 0x00030004);
        writer
            .section(".rsrc", resource, 188, adv_pe::scn::MEM_READ)
            .unwrap();
        let mut fixture = writer.finish().unwrap();
        put32(&mut fixture, 0x88, 123);
        put32(&mut fixture, 0x108, 0x1000);
        put32(&mut fixture, 0x10c, 188);
        std::fs::write(&exe, &fixture).unwrap();
        let game = Game::identify(&exe, 932).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(game.exe_sha256, sha256(&fixture));
        assert_eq!(game.exe_size, fixture.len() as u64);
        assert_eq!((game.timestamp, game.image_size), (123, 8192));
        assert_eq!(game.file_version.as_deref(), Some("1.2.3.4"));
        assert_eq!(game.arch, Some(PeArch::X86));
        assert_eq!(game.sections, Some(1));
        let key = dedupe_key(&[1; 32], &game.exe_sha256);
        assert_eq!(key.len(), 16);
        assert_eq!(key, dedupe_key(&[1; 32], &game.exe_sha256));
        assert_ne!(key, dedupe_key(&[2; 32], &game.exe_sha256));
    }

    #[test]
    fn legacy_v2_run_summary_still_deserializes_for_library_compatibility() {
        let legacy = r#"{
            "failure": {"kind": "api", "api": "d3d9.dll!Foo", "cpu_fault": null, "after": "lt_1m"},
            "perf": {"frame_ms_p50": 16.0, "frame_ms_p95": 20.0, "frame_ms_p99": 25.0}
        }"#;
        let summary: RunSummary = serde_json::from_str(legacy).unwrap();
        assert_eq!(summary.failure.unwrap().kind, FailureKind::Api);
        assert!(summary.run.is_none());
    }

    // spec: REPORT-DIAG-1, REPORT-PERF-1
    #[test]
    fn diagnostics_hang_and_perf_cliffs_validate_and_reject_paths() {
        let mut r = report();
        r.hang = Some(HangSnapshot {
            exec_thread_responsive: true,
            idle_wait_pct: Some(98),
            threads: vec![ThreadSnapshot {
                tid: 1,
                state: "waiting".into(),
                blocked_api: Some("ntdll.dll!NtWaitForMultipleObjects".into()),
                stack: vec![
                    "ntdll.dll!NtWaitForMultipleObjects+0xc".into(),
                    "exe+0x1234".into(),
                ],
            }],
            top_boundaries: vec![BoundaryStat {
                kind: "syscall".into(),
                name: "NtWaitForSingleObject".into(),
                calls: 5000,
                host_ms: 420,
            }],
        });
        r.diagnostics = Some(Diagnostics {
            warnings: vec![WarningStat {
                api: "d3d9.dll!IDirect3DDevice9::Reset".into(),
                reason: "device_lost".into(),
                count: 2,
            }],
            rejected_probes: vec![ProbeRejection {
                api: "d3d9.dll!IDirect3D9::CheckDeviceFormat".into(),
                detail: "fmt:21:usage:1:type:3".into(),
                count: 1,
            }],
            wine_errors: vec![WineLogStat {
                level: "err".into(),
                channel: "module".into(),
                func: "import_dll".into(),
                count: 3,
            }],
            hooked_exports: vec![HookedExport {
                api: "gdi32.dll!CreateFontIndirectA".into(),
                form: "jmp_rel32".into(),
            }],
            exceptions_seen: vec![ExceptionStat {
                code: "0xc0000005".into(),
                count: 4,
                last_pc: Some("exe+0x1234".into()),
            }],
            graphics: Some(GraphicsDiag {
                fronts: vec!["d3d9".into()],
                d3d9_draws: 100,
                d3d9_readback_kb: 256,
            }),
            audio_video: Some(AudioVideoDiag {
                audio_fronts: vec!["dsound".into()],
                video_fronts: vec!["dshow".into()],
                dsound_create_failures: BTreeMap::from([("DSERR_UNSUPPORTED".into(), 1)]),
            }),
            fonts: Some(FontDiag {
                missing_standard: vec!["MS Gothic".into()],
                substitutions: 2,
                charsets: vec![128],
                gdi_codepages: vec![932],
            }),
            windows: Some(WindowDiag {
                top_level: 1,
                visible: 1,
                dialogs: 0,
            }),
            host_panics: vec![],
        });
        r.perf = Some(Perf {
            frame_ms_p50: 16.5,
            frame_ms_p95: 33.1,
            frame_ms_p99: 60.2,
            long_frames_50ms: 3,
            long_frames_250ms: 0,
            cpu_share: Some(CpuShare {
                translated_pct: 70,
                apply_pct: 5,
                interpret_pct: 10,
                interpreter_pct: 10,
                rep_pct: 5,
            }),
            top_cliffs: vec![CliffStat {
                name: "x87_exact:fpatan".into(),
                count: 1024,
            }],
            peak_heap_mb: Some(512),
        });
        let json = to_json(&r).unwrap();
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), r);

        // Non-allowlisted requested font is rejected.
        let mut bad_font = r.clone();
        bad_font
            .diagnostics
            .as_mut()
            .unwrap()
            .fonts
            .as_mut()
            .unwrap()
            .missing_standard = vec!["MyCustomGameFont".into()];
        assert!(to_json(&bad_font).is_err());

        // Unexpected Wine log level is rejected.
        let mut bad_wine = r.clone();
        bad_wine.diagnostics.as_mut().unwrap().wine_errors[0].level = "trace".into();
        assert!(to_json(&bad_wine).is_err());

        // Path separator in warning reason is rejected.
        let mut bad_warn = r.clone();
        bad_warn.diagnostics.as_mut().unwrap().warnings[0].reason = "C:\\Games\\x.dat".into();
        assert!(to_json(&bad_warn).is_err());

        // Percentage over 100 in cpu_share is rejected.
        let mut bad_perf = r;
        bad_perf
            .perf
            .as_mut()
            .unwrap()
            .cpu_share
            .as_mut()
            .unwrap()
            .translated_pct = 101;
        assert!(to_json(&bad_perf).is_err());
    }
}
