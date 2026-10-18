//! Update check against the latest GitHub release; it runs only when the user asks.
use serde::{Deserialize, Serialize};
use std::io::Read;

const LATEST: &str = "https://api.github.com/repos/hitode-project/hitode/releases/latest";
const RELEASES: &str = "https://github.com/hitode-project/hitode/releases";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
}
#[derive(Serialize)]
pub struct Update {
    pub current: &'static str,
    /// The newer release, absent when this build is current or nothing is released yet.
    pub latest: Option<String>,
    pub url: String,
}

/// `v0.2.0` or `0.2.0` as a comparable triple; pre-release suffixes are not published as latest.
fn version(tag: &str) -> Option<(u64, u64, u64)> {
    let mut parts = tag.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse().ok();
    Some((next()?, next()?, next()?))
}

fn check() -> Result<Update, String> {
    let current = env!("CARGO_PKG_VERSION");
    let response = crate::vndb::client()?
        .get(LATEST)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .map_err(|e| e.to_string())?;
    if response.status().as_u16() == 404 {
        return Ok(Update {
            current,
            latest: None,
            url: RELEASES.into(),
        });
    }
    let response = response.error_for_status().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    response
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("Release response is too large".into());
    }
    let release: Release = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let newer = version(&release.tag_name)
        .zip(version(current))
        .is_some_and(|(latest, current)| latest > current);
    let url = if release.html_url.starts_with(RELEASES) {
        release.html_url
    } else {
        RELEASES.into()
    };
    Ok(Update {
        current,
        latest: newer.then_some(release.tag_name),
        url,
    })
}

#[tauri::command]
pub async fn check_update() -> Result<Update, String> {
    tauri::async_runtime::spawn_blocking(check)
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::version;

    // spec: HOST-UPDATE-1
    #[test]
    fn compares_release_tags() {
        assert_eq!(version("v0.1.0"), Some((0, 1, 0)));
        assert_eq!(version("0.10.2"), Some((0, 10, 2)));
        assert!(version("v0.10.0") > version("v0.9.9"));
        assert_eq!(version("nightly"), None);
    }
}
