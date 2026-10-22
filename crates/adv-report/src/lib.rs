//! The anonymous compatibility report (schema `hitode.report/1`) that the
//! launcher saves or submits, and the run summary the shell derives it from.
//!
//! Field names and order are the wire format shared with
//! `apps/adv-launcher/src/lib/report.ts`. A report identifies the game only by
//! its executable hash and size, and the user only by a key that is stable per
//! installation and game; nothing here carries paths, names or guest text
//! except the user's own note.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, time::Duration};

pub const SCHEMA: &str = "hitode.report/1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema: String,
    pub dedupe_key: String,
    pub app: App,
    pub platform: Platform,
    pub game: Game,
    pub outcome: Outcome,
    pub issues: Vec<Issue>,
    /// Present only when the outcome is `issues` with a performance problem.
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
}
impl Report {
    /// Checks what serde cannot: the schema, the note length and API names.
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform {
    /// `windows`, `macos`, `linux`, `android` or `ios`.
    pub os: String,
    pub os_major: String,
    /// `x86_64` or `aarch64`.
    pub arch: String,
    /// `x64` or `a64`; the engine follows the host architecture.
    pub cpu_engine: String,
    pub graphics: String,
}
impl Platform {
    /// The running host; `os_major` is the major OS version, or `unknown`.
    pub fn current(graphics: &str) -> Self {
        let arch = std::env::consts::ARCH;
        Self {
            os: std::env::consts::OS.into(),
            os_major: os_major().unwrap_or_else(|| "unknown".into()),
            arch: arch.into(),
            cpu_engine: if arch == "aarch64" { "a64" } else { "x64" }.into(),
            graphics: graphics.into(),
        }
    }
}

#[cfg(target_os = "macos")]
fn os_major() -> Option<String> {
    let plist = std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist").ok()?;
    let value = plist.split("<key>ProductVersion</key>").nth(1)?;
    let version = value.split("<string>").nth(1)?.split("</string>").next()?;
    Some(version.split('.').next()?.trim().to_owned())
}
#[cfg(not(target_os = "macos"))]
fn os_major() -> Option<String> {
    None
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Game {
    pub exe_sha256: String,
    pub exe_size: u64,
    /// Guest ANSI code page.
    pub codepage: u32,
}
impl Game {
    pub fn identify(exe: &Path, codepage: u32) -> std::io::Result<Self> {
        let mut file = std::fs::File::open(exe)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 1 << 16];
        let mut size = 0u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            size += read as u64;
        }
        Ok(Self {
            exe_sha256: hex(&hasher.finalize()),
            exe_size: size,
            codepage,
        })
    }
}

/// A key that lets the server merge repeated reports for one game from one
/// installation without learning anything else: `secret` is random per
/// installation and never leaves it.
pub fn dedupe_key(secret: &[u8; 32], exe_sha256: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"hitode.report/1 dedupe\0");
    hasher.update(secret);
    hasher.update(exe_sha256.as_bytes());
    hex(&hasher.finalize()[..8])
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
    Crash,
    Performance,
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
    /// Called by the guest; discovery mode returned their failure.
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
                arch: "aarch64".into(),
                cpu_engine: "a64".into(),
                graphics: "gpu".into(),
            },
            game: Game {
                exe_sha256: "ab".repeat(32),
                exe_size: 1887437,
                codepage: 932,
            },
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
            r#"{"schema":"hitode.report/1","dedupe_key":"0123456789abcdef","#,
            r#""app":{"version":"0.1.0","build":"8c048ed6","channel":"preview"},"#,
            r#""platform":{"os":"macos","os_major":"26","arch":"aarch64","cpu_engine":"a64","graphics":"gpu"},"#,
            &format!(r#""game":{{"exe_sha256":"{}","exe_size":1887437,"codepage":932}},"#, "ab".repeat(32)),
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
            issues: vec![Issue::Performance],
            perf_issues: Some(vec![PerfIssue::FastForward, PerfIssue::AudioDropout]),
            note: Some("BGM".into()),
            ..report()
        };
        let json = serde_json::to_string(&issues).unwrap();
        assert!(json.contains(
            r#""outcome":"issues","issues":["performance"],"perf_issues":["fast_forward","audio_dropout"],"note":"BGM""#
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
        std::fs::write(&exe, b"abc").unwrap();
        let game = Game::identify(&exe, 932).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            game.exe_sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(game.exe_size, 3);
        let key = dedupe_key(&[1; 32], &game.exe_sha256);
        assert_eq!(key.len(), 16);
        assert_eq!(key, dedupe_key(&[1; 32], &game.exe_sha256));
        assert_ne!(key, dedupe_key(&[2; 32], &game.exe_sha256));
    }
}
