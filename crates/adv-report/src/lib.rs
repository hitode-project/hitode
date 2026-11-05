//! The anonymous compatibility report (schema `hitode.report/2`) that the
//! launcher saves or opens in an issue, and the run summary the shell derives it from.
//!
//! Field names and order match `apps/adv-launcher/src/lib/report.ts`.
//! Executable identity and structured diagnostics are automatic; the player
//! chooses whether to include the matched title, edition and note.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};

pub const SCHEMA: &str = "hitode.report/2";

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
    pub failure: Option<Failure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perf: Option<Perf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_gaps: Option<ApiGaps>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline: Option<Timeline>,
}
impl Report {
    /// Checks bounded player text, title identifiers, the schema and API names.
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
        if let Some(gaps) = &self.api_gaps {
            gaps.validate()?;
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
    pub graphics: Graphics,
    pub settings: Settings,
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
}
impl Game {
    pub fn identify(exe: &Path, codepage: u32) -> std::io::Result<Self> {
        let bytes = std::fs::read(exe)?;
        let pe = adv_pe::Pe::parse(&bytes).map_err(std::io::Error::other)?;
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
        Ok(Self {
            exe_sha256: sha256(&bytes),
            exe_size: bytes.len() as u64,
            codepage,
            timestamp: pe.timestamp,
            image_size: pe.size_of_image,
            file_version,
        })
    }
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
pub enum FailureKind {
    /// The guest called an API the runtime does not implement.
    Api,
    /// An unhandled guest CPU fault ended the game.
    Cpu,
    /// Any other runtime failure; its message may name host paths, so only
    /// the kind is reported.
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub kind: FailureKind,
    /// `module!name` of the unimplemented API.
    pub api: Option<String>,
    /// Fault kind, address and guest PC, e.g. `read 0x00000000 at 0x00401a2c`.
    pub cpu_fault: Option<String>,
    pub after: FailureAfter,
}

/// Guest execution time per presented frame, rounded to 0.1 ms.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Perf {
    pub frame_ms_p50: f64,
    pub frame_ms_p95: f64,
    pub frame_ms_p99: f64,
}
impl Perf {
    pub fn from_samples(samples: impl IntoIterator<Item = f64>) -> Option<Self> {
        let mut values: Vec<f64> = samples.into_iter().collect();
        if values.is_empty() {
            return None;
        }
        values.sort_by(f64::total_cmp);
        let at = |percent: usize| {
            let rank = (values.len() * percent).div_ceil(100).saturating_sub(1);
            (values[rank] * 10.0).round() / 10.0
        };
        Some(Self {
            frame_ms_p50: at(50),
            frame_ms_p95: at(95),
            frame_ms_p99: at(99),
        })
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

/// `module!name` in printable ASCII, so a gap cannot carry free text.
fn api_name(name: &str) -> bool {
    name.len() <= ApiGaps::MAX_NAME_BYTES
        && name.bytes().all(|b| b.is_ascii_graphic())
        && name
            .split_once('!')
            .is_some_and(|(module, export)| !module.is_empty() && !export.is_empty())
}

/// What a finished run contributes to a report, before the user describes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub failure: Option<Failure>,
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
            },
            title: None,
            outcome: Outcome::Failed,
            issues: vec![],
            perf_issues: None,
            note: None,
            failure: Some(Failure {
                kind: FailureKind::Api,
                api: Some("d3d9.dll!IDirect3DDevice9::CreateVolumeTexture".into()),
                cpu_fault: None,
                after: FailureAfter::Minutes,
            }),
            perf: Some(Perf {
                frame_ms_p50: 16.8,
                frame_ms_p95: 41.2,
                frame_ms_p99: 88.5,
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
            r#"{"schema":"hitode.report/2","dedupe_key":"0123456789abcdef","#,
            r#""app":{"version":"0.1.0","build":"8c048ed6","channel":"preview"},"#,
            r#""platform":{"os":"macos","os_major":"26","model":"Example1,1","graphics":{"api":"metal","name":"Example GPU","vendor":"apple","device":"Example GPU","driver":"26","tier":"metal"},"settings":{"upscaler":"off","scaling":"fit","fast_forward":"hold","touch_mode":"direct","font_files":0,"font_overrides":0,"font_smoothing":null,"discs":0}},"#,
            &format!(r#""game":{{"exe_sha256":"{}","exe_size":1887437,"codepage":932,"timestamp":123,"image_size":8192}},"#, "ab".repeat(32)),
            r#""outcome":"failed","issues":[],"#,
            r#""failure":{"kind":"api","api":"d3d9.dll!IDirect3DDevice9::CreateVolumeTexture","cpu_fault":null,"after":"1m_10m"},"#,
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
            schema: "hitode.report/1".into(),
            ..report()
        };
        assert!(to_json(&other).is_err());
        assert!(serde_json::from_str::<Report>(&serde_json::to_string(&other).unwrap()).is_err());
        assert!(to_json(&report()).unwrap().starts_with("{\n  \"schema\""));
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
                frame_ms_p99: 99.0
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
        let key = dedupe_key(&[1; 32], &game.exe_sha256);
        assert_eq!(key.len(), 16);
        assert_eq!(key, dedupe_key(&[1; 32], &game.exe_sha256));
        assert_ne!(key, dedupe_key(&[2; 32], &game.exe_sha256));
    }
}
