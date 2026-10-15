//! VNDB Kana lookup keeps downloaded metadata separate from the library display title.
use crate::library::{Library, LibraryGame};
use base64::Engine;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    io::Read,
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::Manager;

const FIELDS: &str =
    "title,alttitle,aliases,released,description,developers.name,image.thumbnail,image.sexual";
static NEXT_REQUEST: Mutex<Option<Instant>> = Mutex::new(None);
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct Developer {
    pub name: String,
}
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct Image {
    pub thumbnail: String,
    pub sexual: f64,
}
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct Novel {
    pub id: String,
    pub title: String,
    pub alttitle: Option<String>,
    pub aliases: Vec<String>,
    pub released: Option<String>,
    pub description: Option<String>,
    pub developers: Vec<Developer>,
    pub image: Option<Image>,
    #[serde(default)]
    pub cover: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct Results {
    pub results: Vec<Novel>,
    pub more: bool,
}

fn identifier(value: &str) -> Result<Option<String>, String> {
    let value = value.trim();
    let valid = |s: &str| {
        s.strip_prefix('v').is_some_and(|n| {
            !n.is_empty() && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit())
        })
    };
    if valid(value) {
        return Ok(Some(value.into()));
    }
    if value.contains("://") || value.starts_with("vndb.org/") || value.starts_with("www.vndb.org/")
    {
        let input = if value.contains("://") {
            value.to_owned()
        } else {
            format!("https://{value}")
        };
        let url = reqwest::Url::parse(&input).map_err(|_| "Invalid VNDB link")?;
        let id = url.path().trim_matches('/');
        if matches!(url.scheme(), "https" | "http")
            && matches!(url.host_str(), Some("vndb.org" | "www.vndb.org"))
            && valid(id)
        {
            return Ok(Some(id.into()));
        }
        return Err("Use a VNDB visual novel link such as https://vndb.org/v17".into());
    }
    Ok(None)
}
fn client() -> Result<Client, String> {
    let builder = Client::builder()
        .timeout(Duration::from_secs(25))
        .user_agent(concat!("Hitode/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none());
    #[cfg(target_os = "android")]
    let builder = builder.tls_certs_only(system_certificates()?);
    builder.build().map_err(|e| e.to_string())
}

/// Android's platform verifier needs JNI setup and a Gradle component. Trust
/// the system store directly instead, as Android apps do by default.
#[cfg(target_os = "android")]
fn system_certificates() -> Result<Vec<reqwest::Certificate>, String> {
    for dir in [
        "/apex/com.android.conscrypt/cacerts",
        "/system/etc/security/cacerts",
    ] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let certificates: Vec<_> = entries
            .flatten()
            .filter_map(|entry| std::fs::read(entry.path()).ok())
            .filter_map(|pem| reqwest::Certificate::from_pem(&pem).ok())
            .collect();
        if !certificates.is_empty() {
            return Ok(certificates);
        }
    }
    Err("No system certificates are available".into())
}
fn lookup(query: &str, page: u32) -> Result<Results, String> {
    let query = query.trim();
    if query.is_empty() || query.chars().count() > 200 || page == 0 {
        return Err("Enter a name, VNDB ID or visual novel link (up to 200 characters)".into());
    }
    let id = identifier(query)?;
    let body = json!({"filters": if let Some(id) = &id { json!(["id", "=", id]) } else { json!(["search", "=", query]) },
        "fields": FIELDS, "sort": if id.is_some() { "id" } else { "searchrank" }, "results": 10, "page": page });
    {
        let mut next = NEXT_REQUEST.lock().unwrap();
        if let Some(delay) = next.and_then(|at| at.checked_duration_since(Instant::now())) {
            std::thread::sleep(delay);
        }
        *next = Some(Instant::now() + Duration::from_millis(1600));
    }
    let response = client()?
        .post("https://api.vndb.org/kana/vn")
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?;
    if response.status().as_u16() == 429 {
        return Err("VNDB rate limit reached. Try again later.".into());
    }
    let response = response.error_for_status().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    response
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("VNDB response is too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
fn cache_cover(novel: &mut Novel) -> Result<(), String> {
    let Some(image) = &novel.image else {
        return Ok(());
    };
    let url = reqwest::Url::parse(&image.thumbnail).map_err(|e| e.to_string())?;
    if url.scheme() != "https" || url.host_str() != Some("t.vndb.org") {
        return Err("Unexpected VNDB image host".into());
    }
    let response = client()?
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    if !matches!(mime.as_str(), "image/jpeg" | "image/png" | "image/webp") {
        return Err("Unsupported VNDB cover format".into());
    }
    let mut bytes = Vec::new();
    response
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("VNDB cover is too large".into());
    }
    novel.cover = Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ));
    Ok(())
}
#[tauri::command]
pub async fn vndb_search(query: String, page: u32) -> Result<Results, String> {
    tauri::async_runtime::spawn_blocking(move || lookup(&query, page))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn vndb_apply(
    app: tauri::AppHandle,
    id: String,
    vn: String,
    title: String,
) -> Result<LibraryGame, String> {
    let title = title.trim().to_owned();
    if title.is_empty() {
        return Err("Display name cannot be empty".into());
    }
    if identifier(&vn)?.is_none() {
        return Err("Select a VNDB entry".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut novel = lookup(&vn, 1)?
            .results
            .into_iter()
            .next()
            .ok_or("VNDB entry not found")?;
        cache_cover(&mut novel)?;
        app.state::<Mutex<Library>>()
            .lock()
            .unwrap()
            .set_metadata(&id, Some(novel), Some(title))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn vndb_unlink(
    state: tauri::State<'_, Mutex<Library>>,
    id: String,
) -> Result<LibraryGame, String> {
    state
        .lock()
        .unwrap()
        .set_metadata(&id, None, None)
        .map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requests public VNDB metadata and a thumbnail"]
    fn public_api_metadata_and_cover_round_trip() {
        let mut results = lookup("https://vndb.org/v17", 1).unwrap();
        assert!(!results.more);
        let novel = &mut results.results[0];
        assert_eq!(novel.id, "v17");
        cache_cover(novel).unwrap();
        assert!(novel.cover.as_ref().unwrap().starts_with("data:image/"));
        let by_name = lookup(&novel.title, 1).unwrap();
        assert!(by_name.results.iter().any(|vn| vn.id == "v17"));
    }

    // spec: HOST-VNDB-1
    #[test]
    fn links_only_select_visual_novels_on_vndb() {
        for q in [
            "v17",
            "https://vndb.org/v17#main",
            "vndb.org/v17",
            "https://www.vndb.org/v17/",
        ] {
            assert_eq!(identifier(q).unwrap(), Some("v17".into()));
        }
        for q in [
            "https://example.org/v17",
            "https://vndb.org/r17",
            "https://vndb.org.evil/v17",
        ] {
            assert!(identifier(q).is_err());
        }
        assert_eq!(identifier("a title").unwrap(), None);
    }
}
