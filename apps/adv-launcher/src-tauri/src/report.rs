//! The anonymous compatibility report: the fields that identify build,
//! platform and game, and saving the report the overlay assembled. Submission
//! waits for the report service.
use adv_report::{App, Game, Platform, Report};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, Runtime};

/// The report fields the user cannot remove (`ReportBase` in report.ts).
#[derive(Serialize)]
pub struct ReportBase {
    dedupe_key: String,
    app: App,
    platform: Platform,
    game: Game,
}

/// A random key per installation, created on first use; it never leaves the
/// app data folder and only salts the dedupe key.
fn installation_secret(dir: &Path) -> Result<[u8; 32], String> {
    let path = dir.join("report-key");
    if let Ok(bytes) = std::fs::read(&path)
        && let Ok(secret) = bytes.try_into()
    {
        return Ok(secret);
    }
    let mut secret = [0; 32];
    getrandom::fill(&mut secret).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(&path, secret).map_err(|e| e.to_string())?;
    Ok(secret)
}

#[tauri::command]
pub async fn report_base<R: Runtime>(
    app: AppHandle<R>,
    exe: PathBuf,
    codepage: u32,
) -> Result<ReportBase, String> {
    let data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let game = Game::identify(&exe, codepage).map_err(|e| format!("{}: {e}", exe.display()))?;
        Ok(ReportBase {
            dedupe_key: adv_report::dedupe_key(&installation_secret(&data)?, &game.exe_sha256),
            app: App {
                version: env!("CARGO_PKG_VERSION").into(),
                build: env!("ADV_BUILD").into(),
                channel: "preview".into(),
            },
            platform: Platform::current("gpu"),
            game,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Civil date (UTC) of a Unix day number.
fn date(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// Writes the report to the downloads folder as `adv-report-DATE[-N].json`.
#[tauri::command]
pub fn save_report<R: Runtime>(app: AppHandle<R>, report: Report) -> Result<PathBuf, String> {
    let json = adv_report::to_json(&report)?;
    let dir = app.path().download_dir().map_err(|e| e.to_string())?;
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let (year, month, day) = date((seconds / 86_400) as i64);
    let stem = format!("adv-report-{year:04}-{month:02}-{day:02}");
    for n in 1.. {
        let name = if n == 1 {
            format!("{stem}.json")
        } else {
            format!("{stem}-{n}.json")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                std::io::Write::write_all(&mut &file, json.as_bytes())
                    .map_err(|e| e.to_string())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    #[test]
    fn unix_days_become_civil_dates() {
        assert_eq!(super::date(0), (1970, 1, 1));
        assert_eq!(super::date(11_016), (2000, 2, 29));
        assert_eq!(super::date(20_739), (2026, 10, 13));
    }
}
