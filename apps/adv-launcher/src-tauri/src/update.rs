//! Update check against the latest GitHub release and signed macOS in-app installation.
use serde::{Deserialize, Serialize};
use std::io::Read;
#[cfg(target_os = "macos")]
use std::{
    ffi::CString,
    fs,
    io::Write,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const LATEST: &str = "https://api.github.com/repos/hitode-project/hitode/releases/latest";
const RELEASES: &str = "https://github.com/hitode-project/hitode/releases";
#[cfg(target_os = "macos")]
const DOWNLOAD_PREFIX: &str = "https://github.com/hitode-project/hitode/releases/download/";
#[cfg(target_os = "macos")]
const SIGNATURE_DOMAIN: &str = "hitode-macos-update-v1";
#[cfg(target_os = "macos")]
const BUNDLE_IDENTIFIER: &str = "org.hitode.launcher";
#[cfg(target_os = "macos")]
const MAX_SIG_BYTES: u64 = 4 * 1024;
#[cfg(target_os = "macos")]
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;

/// Raw 32-byte Ed25519 public key corresponding to the release signing key
/// (`tools/release/sign_macos_update.py`).
#[cfg(target_os = "macos")]
const UPDATE_PUBLIC_KEY: [u8; 32] = [
    0x52, 0xfd, 0x17, 0xef, 0x94, 0x74, 0x20, 0xb8, 0xf7, 0xb0, 0xfa, 0x56, 0x2e, 0x8d, 0x4d, 0x7e,
    0x03, 0xd1, 0x6a, 0x9b, 0x16, 0x52, 0x6b, 0x00, 0x50, 0x43, 0x36, 0xab, 0x7c, 0x9d, 0x7f, 0xae,
];

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    pub current: &'static str,
    /// The newer release, absent when this build is current or nothing is released yet.
    pub latest: Option<String>,
    pub url: String,
    /// True on macOS when the app runs from a writable `.app` bundle and the
    /// latest release includes both a macOS update archive and its signature.
    pub can_install: bool,
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
struct PendingInstall {
    tag: String,
    expected_version: String,
    archive_url: String,
    sig_url: String,
}

#[cfg(target_os = "macos")]
static PENDING: Mutex<Option<PendingInstall>> = Mutex::new(None);

#[cfg(target_os = "macos")]
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProgress {
    stage: &'static str,
    downloaded: u64,
    total: Option<u64>,
}

/// `v0.2.0` or `0.2.0` as a comparable triple; pre-release suffixes are not published as latest.
fn version(tag: &str) -> Option<(u64, u64, u64)> {
    let mut parts = tag.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse().ok();
    let triple = (next()?, next()?, next()?);
    if parts.next().is_some() {
        return None;
    }
    Some(triple)
}

#[cfg(target_os = "macos")]
fn select_macos_assets(tag: &str, assets: &[ReleaseAsset]) -> Option<PendingInstall> {
    let bare = tag.trim().trim_start_matches('v');
    version(tag)?;
    let archive_name = format!("Hitode-{bare}-macos-arm64.app.tar.gz");
    let sig_name = format!("{archive_name}.sig");
    let mut archive_url = None;
    let mut sig_url = None;
    for asset in assets {
        if !asset.browser_download_url.starts_with(DOWNLOAD_PREFIX) {
            continue;
        }
        if asset.name == archive_name {
            archive_url = Some(asset.browser_download_url.clone());
        } else if asset.name == sig_name {
            sig_url = Some(asset.browser_download_url.clone());
        }
    }
    Some(PendingInstall {
        tag: tag.trim().to_owned(),
        expected_version: bare.to_owned(),
        archive_url: archive_url?,
        sig_url: sig_url?,
    })
}

#[cfg(target_os = "macos")]
fn macos_app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name().and_then(|n| n.to_str()) != Some("MacOS") {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name().and_then(|n| n.to_str()) != Some("Contents") {
        return None;
    }
    let app_bundle = contents_dir.parent()?;
    if app_bundle.extension().and_then(|e| e.to_str()) != Some("app") {
        return None;
    }
    if app_bundle.starts_with("/Volumes")
        || app_bundle
            .components()
            .any(|c| c.as_os_str() == "AppTranslocation")
    {
        return None;
    }
    if !contents_dir.join("Info.plist").is_file() {
        return None;
    }
    Some(app_bundle.to_path_buf())
}

#[cfg(target_os = "macos")]
fn is_writable_path(path: &Path) -> bool {
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c_path` is a valid NUL-terminated path passed to POSIX `access`.
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

#[cfg(target_os = "macos")]
fn current_installable_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundle = macos_app_bundle(&exe)?;
    let parent = bundle.parent()?;
    (is_writable_path(&bundle) && is_writable_path(parent)).then_some(bundle)
}

fn fetch_latest() -> Result<Option<Release>, String> {
    let response = crate::vndb::client()?
        .get(LATEST)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .map_err(|e| e.to_string())?;
    if response.status().as_u16() == 404 {
        return Ok(None);
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
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn check() -> Result<Update, String> {
    let current = env!("CARGO_PKG_VERSION");
    let Some(release) = fetch_latest()? else {
        #[cfg(target_os = "macos")]
        {
            *PENDING.lock().unwrap() = None;
        }
        return Ok(Update {
            current,
            latest: None,
            url: RELEASES.into(),
            can_install: false,
        });
    };
    let newer = version(&release.tag_name)
        .zip(version(current))
        .is_some_and(|(latest, current)| latest > current);
    let url = if release.html_url.starts_with(RELEASES) {
        release.html_url
    } else {
        RELEASES.into()
    };

    #[cfg(target_os = "macos")]
    let can_install = {
        let pending = if newer {
            select_macos_assets(&release.tag_name, &release.assets)
        } else {
            None
        };
        let installable = pending.is_some() && current_installable_bundle().is_some();
        *PENDING.lock().unwrap() = if installable { pending } else { None };
        installable
    };
    #[cfg(not(target_os = "macos"))]
    let can_install = {
        let _ = &release.assets;
        false
    };

    Ok(Update {
        current,
        latest: newer.then_some(release.tag_name),
        url,
        can_install,
    })
}

#[cfg(target_os = "macos")]
fn allowed_download_url(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    host == "github.com" || host.ends_with(".githubusercontent.com")
}

#[cfg(target_os = "macos")]
fn download_client() -> Result<reqwest::blocking::Client, String> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 {
            attempt.error("Too many redirects while downloading update")
        } else if !allowed_download_url(attempt.url()) {
            attempt.error("Blocked redirect outside GitHub release hosts")
        } else {
            attempt.follow()
        }
    });
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(25))
        .timeout(Duration::from_secs(600))
        .user_agent(concat!("Hitode/", env!("CARGO_PKG_VERSION")))
        .redirect(policy)
        .build()
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn signature_message(tag: &str, sha256_hex: &str, size: u64) -> String {
    format!("{SIGNATURE_DOMAIN}\n{tag}\n{sha256_hex}\n{size}\n")
}

#[cfg(target_os = "macos")]
fn verify_archive_signature(
    public_key: &[u8; 32],
    tag: &str,
    sha256_hex: &str,
    size: u64,
    sig_b64: &str,
) -> Result<(), String> {
    use base64::Engine as _;
    use ed25519_dalek::{Signature, VerifyingKey};

    let raw_sig = base64::engine::general_purpose::STANDARD
        .decode(sig_b64.trim())
        .map_err(|_| "Update signature is not valid base64".to_owned())?;
    let sig_bytes: [u8; 64] = raw_sig
        .try_into()
        .map_err(|_| "Update signature has unexpected length".to_owned())?;
    let key = VerifyingKey::from_bytes(public_key)
        .map_err(|_| "Invalid built-in update public key".to_owned())?;
    let signature = Signature::from_bytes(&sig_bytes);
    let message = signature_message(tag, sha256_hex, size);
    key.verify_strict(message.as_bytes(), &signature)
        .map_err(|_| "Update signature verification failed".to_owned())
}

#[cfg(target_os = "macos")]
struct StagingGuard(PathBuf);

#[cfg(target_os = "macos")]
impl Drop for StagingGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(target_os = "macos")]
fn create_staging_dir(parent: &Path) -> Result<StagingGuard, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = parent.join(format!(".Hitode-update-{}-{nonce}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| format!("Could not prepare update folder: {e}"))?;
    Ok(StagingGuard(dir))
}

#[cfg(target_os = "macos")]
fn verify_extracted_bundle(bundle: &Path, expected_version: &str) -> Result<(), String> {
    let meta = fs::symlink_metadata(bundle)
        .map_err(|_| "Update archive did not contain Hitode.app".to_owned())?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("Extracted Hitode.app is not a directory".into());
    }
    let exe = bundle.join("Contents/MacOS/adv-launcher");
    let exe_meta = fs::symlink_metadata(&exe)
        .map_err(|_| "Extracted Hitode.app is missing its executable".to_owned())?;
    if !exe_meta.is_file() || exe_meta.file_type().is_symlink() {
        return Err("Extracted Hitode.app executable is invalid".into());
    }
    let plist_val = plist::Value::from_file(bundle.join("Contents/Info.plist"))
        .map_err(|e| format!("Extracted Info.plist is unreadable: {e}"))?;
    let dict = plist_val
        .as_dictionary()
        .ok_or_else(|| "Extracted Info.plist is not a dictionary".to_owned())?;
    let id = dict
        .get("CFBundleIdentifier")
        .and_then(plist::Value::as_string);
    if id != Some(BUNDLE_IDENTIFIER) {
        return Err("Extracted bundle identifier does not match Hitode".into());
    }
    let ver = dict
        .get("CFBundleShortVersionString")
        .and_then(plist::Value::as_string);
    if ver != Some(expected_version) {
        return Err(format!(
            "Extracted version ({}) does not match expected {expected_version}",
            ver.unwrap_or("missing")
        ));
    }
    let status = Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(bundle)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Could not verify code signature: {e}"))?;
    if !status.success() {
        return Err("Extracted Hitode.app failed macOS codesign verification".into());
    }
    let _ = Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(bundle)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(())
}

#[cfg(target_os = "macos")]
fn swap_bundle(target: &Path, staged: &Path, backup: &Path) -> Result<(), String> {
    if backup.exists() {
        fs::remove_dir_all(backup).map_err(|e| format!("Could not clean previous backup: {e}"))?;
    }
    fs::rename(target, backup).map_err(|e| format!("Could not stage existing Hitode.app: {e}"))?;
    if let Err(error) = fs::rename(staged, target) {
        let _ = fs::rename(backup, target);
        return Err(format!(
            "Could not move updated Hitode.app into place: {error}"
        ));
    }
    let _ = fs::remove_dir_all(backup);
    Ok(())
}

#[cfg(target_os = "macos")]
fn spawn_relaunch(app_bundle: &Path) -> Result<(), String> {
    let pid = std::process::id().to_string();
    Command::new("/bin/sh")
        .arg("-c")
        .arg("pid=\"$1\"; app=\"$2\"; while kill -0 \"$pid\" 2>/dev/null; do sleep 0.1; done; /usr/bin/open -n \"$app\"")
        .arg("_")
        .arg(pid)
        .arg(app_bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not relaunch Hitode: {e}"))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn emit_progress(app: &tauri::AppHandle, stage: &'static str, downloaded: u64, total: Option<u64>) {
    use tauri::Emitter;
    let _ = app.emit(
        "update-progress",
        UpdateProgress {
            stage,
            downloaded,
            total,
        },
    );
}

#[cfg(target_os = "macos")]
fn install_macos(app: &tauri::AppHandle) -> Result<(), String> {
    use sha2::{Digest, Sha256};

    let target_bundle = current_installable_bundle()
        .ok_or_else(|| "Hitode must be installed in a writable folder such as Applications to update automatically.".to_owned())?;
    let parent = target_bundle
        .parent()
        .ok_or_else(|| "Could not determine application folder".to_owned())?;

    let pending = {
        let cached = PENDING.lock().unwrap().clone();
        match cached {
            Some(p) => p,
            None => {
                let updated = check()?;
                if !updated.can_install {
                    return Err("No installable macOS update is available.".into());
                }
                PENDING
                    .lock()
                    .unwrap()
                    .clone()
                    .ok_or_else(|| "No installable macOS update is available.".to_owned())?
            }
        }
    };

    if version(&pending.tag)
        .zip(version(env!("CARGO_PKG_VERSION")))
        .is_none_or(|(latest, current)| latest <= current)
    {
        return Err("This build is already up to date.".into());
    }

    let client = download_client()?;
    emit_progress(app, "downloading", 0, None);

    let sig_response = client
        .get(&pending.sig_url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Could not download update signature: {e}"))?;
    let mut sig_bytes = Vec::new();
    sig_response
        .take(MAX_SIG_BYTES + 1)
        .read_to_end(&mut sig_bytes)
        .map_err(|e| format!("Could not read update signature: {e}"))?;
    if sig_bytes.len() as u64 > MAX_SIG_BYTES {
        return Err("Update signature is too large".into());
    }
    let sig_text =
        String::from_utf8(sig_bytes).map_err(|_| "Update signature is not UTF-8".to_owned())?;

    let staging = create_staging_dir(parent)?;
    let archive_path = staging.0.join("update.tar.gz");
    let mut response = client
        .get(&pending.archive_url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Could not download update archive: {e}"))?;
    let total = response.content_length();
    if let Some(len) = total
        && len > MAX_ARCHIVE_BYTES
    {
        return Err("Update archive exceeds maximum allowed size".into());
    }

    let mut file = fs::File::create(&archive_path)
        .map_err(|e| format!("Could not write update archive: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded = 0u64;
    let mut last_emit = Instant::now();
    loop {
        let n = response
            .read(&mut buf)
            .map_err(|e| format!("Update download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        downloaded = downloaded
            .checked_add(n as u64)
            .ok_or_else(|| "Update size overflow".to_owned())?;
        if downloaded > MAX_ARCHIVE_BYTES {
            return Err("Update archive exceeds maximum allowed size".into());
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("Could not write update archive: {e}"))?;
        hasher.update(&buf[..n]);
        if last_emit.elapsed() >= Duration::from_millis(100) {
            emit_progress(app, "downloading", downloaded, total);
            last_emit = Instant::now();
        }
    }
    file.flush()
        .map_err(|e| format!("Could not flush update archive: {e}"))?;
    drop(file);
    emit_progress(app, "verifying", downloaded, total);

    let digest = hasher.finalize();
    let mut sha256_hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut sha256_hex, "{byte:02x}");
    }
    verify_archive_signature(
        &UPDATE_PUBLIC_KEY,
        &pending.tag,
        &sha256_hex,
        downloaded,
        &sig_text,
    )?;

    emit_progress(app, "installing", downloaded, total);
    let unpack_dir = staging.0.join("unpack");
    fs::create_dir_all(&unpack_dir)
        .map_err(|e| format!("Could not create unpack directory: {e}"))?;
    let tar_status = Command::new("/usr/bin/tar")
        .args(["-xzf"])
        .arg(&archive_path)
        .arg("-C")
        .arg(&unpack_dir)
        .args(["--no-same-owner", "--no-xattrs"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Could not unpack update archive: {e}"))?;
    if !tar_status.success() {
        return Err("Could not unpack update archive".into());
    }

    let staged_bundle = unpack_dir.join("Hitode.app");
    verify_extracted_bundle(&staged_bundle, &pending.expected_version)?;

    let backup_bundle = staging.0.join("Hitode-previous.app");
    swap_bundle(&target_bundle, &staged_bundle, &backup_bundle)?;
    drop(staging);

    emit_progress(app, "restarting", downloaded, total);
    spawn_relaunch(&target_bundle)?;
    let exit_handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        exit_handle.exit(0);
    });
    Ok(())
}

#[tauri::command]
pub async fn check_update() -> Result<Update, String> {
    tauri::async_runtime::spawn_blocking(check)
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let _lease = app.state::<crate::game_storage::Activity>().acquire()?;
    tauri::async_runtime::spawn_blocking(move || install_macos(&app))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub async fn install_update() -> Result<(), String> {
    Err("In-app updates are only supported on macOS.".into())
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
        assert_eq!(version("v0.3.0-rc.1"), None);
        assert_eq!(version("nightly"), None);
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use super::super::*;
        use base64::Engine as _;
        use ed25519_dalek::{Signer, SigningKey};

        // spec: HOST-UPDATE-2
        #[test]
        fn ed25519_signature_accepts_valid_and_rejects_tampered_or_wrong_tag() {
            let signing_key = SigningKey::from_bytes(&[7u8; 32]);
            let public_key = signing_key.verifying_key().to_bytes();
            let sha = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
            let msg = signature_message("v0.3.1", sha, 12345);
            let sig = signing_key.sign(msg.as_bytes());
            let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());

            assert!(verify_archive_signature(&public_key, "v0.3.1", sha, 12345, &sig_b64).is_ok());
            // Wrong tag (downgrade / replay) must fail.
            assert!(verify_archive_signature(&public_key, "v0.3.2", sha, 12345, &sig_b64).is_err());
            // Wrong size must fail.
            assert!(verify_archive_signature(&public_key, "v0.3.1", sha, 12346, &sig_b64).is_err());
            // Wrong digest must fail.
            let bad_sha = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
            assert!(
                verify_archive_signature(&public_key, "v0.3.1", bad_sha, 12345, &sig_b64).is_err()
            );
            // Wrong public key must fail.
            let other_pub = SigningKey::from_bytes(&[8u8; 32])
                .verifying_key()
                .to_bytes();
            assert!(verify_archive_signature(&other_pub, "v0.3.1", sha, 12345, &sig_b64).is_err());
        }

        // spec: HOST-UPDATE-1 HOST-UPDATE-2
        #[test]
        fn release_assets_require_archive_and_sig_from_hitode_repo() {
            let assets = vec![
                ReleaseAsset {
                    name: "Hitode-0.3.1-macos-arm64.app.tar.gz".into(),
                    browser_download_url:
                        "https://github.com/hitode-project/hitode/releases/download/v0.3.1/Hitode-0.3.1-macos-arm64.app.tar.gz"
                            .into(),
                },
                ReleaseAsset {
                    name: "Hitode-0.3.1-macos-arm64.app.tar.gz.sig".into(),
                    browser_download_url:
                        "https://github.com/hitode-project/hitode/releases/download/v0.3.1/Hitode-0.3.1-macos-arm64.app.tar.gz.sig"
                            .into(),
                },
            ];
            let picked = select_macos_assets("v0.3.1", &assets).expect("both assets present");
            assert_eq!(picked.expected_version, "0.3.1");

            // Missing signature refuses installation.
            assert!(select_macos_assets("v0.3.1", &assets[..1]).is_none());

            // Non-hitode host is refused.
            let evil = vec![
                ReleaseAsset {
                    name: "Hitode-0.3.1-macos-arm64.app.tar.gz".into(),
                    browser_download_url:
                        "https://evil.example/Hitode-0.3.1-macos-arm64.app.tar.gz".into(),
                },
                assets[1].clone_for_test(),
            ];
            assert!(select_macos_assets("v0.3.1", &evil).is_none());
        }

        impl ReleaseAsset {
            fn clone_for_test(&self) -> Self {
                Self {
                    name: self.name.clone(),
                    browser_download_url: self.browser_download_url.clone(),
                }
            }
        }

        // spec: HOST-UPDATE-2
        #[test]
        fn redirect_policy_allows_only_https_github_hosts() {
            assert!(allowed_download_url(
                &reqwest::Url::parse(
                    "https://github.com/hitode-project/hitode/releases/download/v0.3.1/a"
                )
                .unwrap()
            ));
            assert!(allowed_download_url(
                &reqwest::Url::parse("https://objects.githubusercontent.com/some-asset").unwrap()
            ));
            assert!(allowed_download_url(
                &reqwest::Url::parse("https://release-assets.githubusercontent.com/some-asset")
                    .unwrap()
            ));
            assert!(!allowed_download_url(
                &reqwest::Url::parse("http://objects.githubusercontent.com/some-asset").unwrap()
            ));
            assert!(!allowed_download_url(
                &reqwest::Url::parse("https://evil.example/a").unwrap()
            ));
        }

        // spec: HOST-UPDATE-2
        #[test]
        fn bundle_path_rejects_dev_volumes_and_translocation() {
            let tmp = std::env::temp_dir()
                .join(format!("hitode-update-bundle-test-{}", std::process::id()));
            let _ = fs::remove_dir_all(&tmp);
            let good_exe = tmp.join("Hitode.app/Contents/MacOS/adv-launcher");
            fs::create_dir_all(good_exe.parent().unwrap()).unwrap();
            fs::write(tmp.join("Hitode.app/Contents/Info.plist"), b"").unwrap();
            assert_eq!(macos_app_bundle(&good_exe), Some(tmp.join("Hitode.app")));

            let dev_exe = tmp.join("target/debug/adv-launcher");
            assert_eq!(macos_app_bundle(&dev_exe), None);

            let dmg_exe = PathBuf::from("/Volumes/Hitode/Hitode.app/Contents/MacOS/adv-launcher");
            assert_eq!(macos_app_bundle(&dmg_exe), None);

            let trans_exe =
                tmp.join("AppTranslocation/ABC/d/Hitode.app/Contents/MacOS/adv-launcher");
            fs::create_dir_all(trans_exe.parent().unwrap()).unwrap();
            fs::write(
                tmp.join("AppTranslocation/ABC/d/Hitode.app/Contents/Info.plist"),
                b"",
            )
            .unwrap();
            assert_eq!(macos_app_bundle(&trans_exe), None);

            let _ = fs::remove_dir_all(&tmp);
        }

        // spec: HOST-UPDATE-2
        #[test]
        fn atomic_swap_replaces_bundle_and_rolls_back_on_error() {
            let tmp = std::env::temp_dir()
                .join(format!("hitode-update-swap-test-{}", std::process::id()));
            let _ = fs::remove_dir_all(&tmp);
            let target = tmp.join("Hitode.app");
            let staged = tmp.join("staged.app");
            let backup = tmp.join("backup.app");
            fs::create_dir_all(&target).unwrap();
            fs::write(target.join("marker"), b"old").unwrap();
            fs::create_dir_all(&staged).unwrap();
            fs::write(staged.join("marker"), b"new").unwrap();

            swap_bundle(&target, &staged, &backup).unwrap();
            assert_eq!(fs::read(target.join("marker")).unwrap(), b"new");
            assert!(!backup.exists());

            // Missing staged path triggers rollback keeping the existing bundle intact.
            let missing = tmp.join("does-not-exist.app");
            assert!(swap_bundle(&target, &missing, &backup).is_err());
            assert_eq!(fs::read(target.join("marker")).unwrap(), b"new");

            let _ = fs::remove_dir_all(&tmp);
        }
    }
}
