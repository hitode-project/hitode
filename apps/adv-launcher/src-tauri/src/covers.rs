//! Local VNDB cover image storage outside `library.json`.
//!
//! Covers are stored as raw binary image files under `<app_data_dir>/covers/`
//! and served to the webview through the custom `cover` URI scheme, avoiding
//! Base64 inflation and repeated full-image rewrites in `library.json`.
use base64::Engine;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) const DIR: &str = "covers";
const MAX_COVER_BYTES: usize = 2 * 1024 * 1024;
const PREFIXES: &[&str] = &[
    "cover://localhost/",
    "http://cover.localhost/",
    "https://cover.localhost/",
];

pub(crate) fn directory(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(DIR)
}

pub(crate) fn valid_name(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && stem.len() <= 64
        && stem
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && matches!(ext, "jpg" | "png" | "webp")
}

pub(crate) fn content_type(name: &str) -> Option<&'static str> {
    if !valid_name(name) {
        return None;
    }
    match name.rsplit_once('.')?.1 {
        "jpg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn extension_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// Converts a stored cover filename into the platform's custom-protocol URL.
pub(crate) fn url_for(value: &str) -> String {
    let name = normalize_stored(value);
    if !valid_name(&name) {
        return value.to_owned();
    }
    #[cfg(any(target_os = "android", target_os = "windows"))]
    return format!("http://cover.localhost/{name}");
    #[cfg(not(any(target_os = "android", target_os = "windows")))]
    format!("cover://localhost/{name}")
}

/// Strips a `cover` protocol prefix back to the bare filename stored in `library.json`.
pub(crate) fn normalize_stored(value: &str) -> String {
    for prefix in PREFIXES {
        if let Some(rest) = value.strip_prefix(prefix)
            && valid_name(rest)
        {
            return rest.to_owned();
        }
    }
    value.to_owned()
}

/// Stores raw image bytes under `<app_data_dir>/covers/` and returns the filename.
pub(crate) fn store(
    app_data_dir: &Path,
    vn_id: &str,
    mime: &str,
    bytes: &[u8],
) -> io::Result<String> {
    let ext = extension_for_mime(mime)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unsupported cover format"))?;
    if bytes.is_empty() || bytes.len() > MAX_COVER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid cover size",
        ));
    }
    let prefix: String = vn_id
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .take(24)
        .map(|b| b.to_ascii_lowercase() as char)
        .collect();
    let prefix = if prefix.is_empty() {
        "cover"
    } else {
        prefix.as_str()
    };
    let digest = Sha256::digest(bytes);
    let mut hash = String::with_capacity(16);
    for byte in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(hash, "{byte:02x}");
    }
    let name = format!("{prefix}-{hash}.{ext}");
    let dir = directory(app_data_dir);
    fs::create_dir_all(&dir)?;
    #[cfg(target_os = "ios")]
    crate::game_storage::exclude_from_backup(&dir);
    let target = dir.join(&name);
    if fs::symlink_metadata(&target).is_ok_and(|meta| meta.is_file()) {
        return Ok(name);
    }
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let write = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, &target));
    if write.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write?;
    Ok(name)
}

/// Migrates a legacy `data:image/...;base64,...` string into a cover file.
pub(crate) fn migrate_data_uri(app_data_dir: &Path, vn_id: &str, value: &str) -> Option<String> {
    let rest = value.strip_prefix("data:")?;
    let (header, encoded) = rest.split_once(',')?;
    let mime = header.strip_suffix(";base64")?;
    extension_for_mime(mime)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    store(app_data_dir, vn_id, mime, &bytes).ok()
}

/// Reads a validated cover image file for the `cover` URI scheme without following symlinks.
pub(crate) fn read_for_protocol(
    app_data_dir: &Path,
    request_path: &str,
) -> Option<(&'static str, Vec<u8>)> {
    let name = request_path.trim_start_matches('/');
    let mime = content_type(name)?;
    let dir = directory(app_data_dir);
    if !fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
        return None;
    }
    let path = dir.join(name);
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_COVER_BYTES as u64 {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    Some((mime, bytes))
}

/// Removes a cover file immediately when no remaining library game references it.
pub(crate) fn remove_if_unused(app_data_dir: &Path, value: &str, referenced: &HashSet<String>) {
    let name = normalize_stored(value);
    if !valid_name(&name) || referenced.contains(&name) {
        return;
    }
    let dir = directory(app_data_dir);
    if !fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
        return;
    }
    let path = dir.join(name);
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
        let _ = fs::remove_file(path);
    }
}

/// Removes unreferenced cover files and leftover temp files without following symlinks.
pub(crate) fn prune_orphans(app_data_dir: &Path, referenced: &HashSet<String>) -> io::Result<()> {
    let dir = directory(app_data_dir);
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() || !kind.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !referenced.contains(name) && (valid_name(name) || name.ends_with(".tmp")) {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hitode-covers-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // spec: HOST-VNDB-1
    #[test]
    fn stores_migrates_serves_and_prunes_cover_files() {
        let dir = temp_dir("lifecycle");
        let name = store(&dir, "v17", "image/png", b"png-bytes").unwrap();
        assert!(valid_name(&name));
        assert!(name.starts_with("v17-"));
        assert!(name.ends_with(".png"));

        let url = url_for(&name);
        assert!(url.ends_with(&format!("/{name}")));
        assert_eq!(normalize_stored(&url), name);

        let (mime, bytes) = read_for_protocol(&dir, &format!("/{name}")).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, b"png-bytes");

        // Reject traversal and invalid names.
        assert!(read_for_protocol(&dir, "../library.json").is_none());
        assert!(read_for_protocol(&dir, "sub/v17.png").is_none());
        assert!(read_for_protocol(&dir, "v17.svg").is_none());

        // Migrate legacy inline Base64 data URI.
        let migrated = migrate_data_uri(&dir, "v42", "data:image/jpeg;base64,/9j/4AAQ").unwrap();
        assert!(migrated.starts_with("v42-"));
        assert!(migrated.ends_with(".jpg"));
        assert!(directory(&dir).join(&migrated).is_file());

        // Pruning keeps referenced covers and drops orphans and temp files.
        fs::write(directory(&dir).join(".leftover.tmp"), b"tmp").unwrap();
        let keep = HashSet::from([name.clone()]);
        prune_orphans(&dir, &keep).unwrap();
        assert!(directory(&dir).join(&name).is_file());
        assert!(!directory(&dir).join(&migrated).exists());
        assert!(!directory(&dir).join(".leftover.tmp").exists());

        #[cfg(unix)]
        {
            let outside = dir.join("secret.png");
            fs::write(&outside, b"secret").unwrap();
            std::os::unix::fs::symlink(&outside, directory(&dir).join("v99-0000000000000000.png"))
                .unwrap();
            assert!(read_for_protocol(&dir, "v99-0000000000000000.png").is_none());
        }

        fs::remove_dir_all(dir).unwrap();
    }
}
