//! The same bundled Wine installation on desktop and mobile. Only file
//! preparation lives here; execution and prefix policy belong to adv-shell.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

include!(concat!(env!("OUT_DIR"), "/wine.rs"));

static PREPARING: Mutex<()> = Mutex::new(());

pub(crate) fn path(data: &Path) -> PathBuf {
    data.join("runtime").join(ID)
}

pub(crate) fn current_id() -> Option<&'static str> {
    (!FILES.is_empty()).then_some(ID)
}

pub(crate) fn prepare(root: &Path) -> Result<(), String> {
    if FILES.is_empty() {
        return Err("Wine runtime is missing from this player build".into());
    }
    let _guard = PREPARING.lock().unwrap();
    #[cfg(target_os = "android")]
    let result =
        adv_shell::android_asset::with_bytes(c"wine.xz", |archive| unpack(root, FILES, archive))
            .and_then(|result| result);
    #[cfg(not(target_os = "android"))]
    let result = unpack(root, FILES, ARCHIVE);
    result.map_err(|e| format!("prepare Wine distribution: {e}"))
}

/// Prepares the bundled Wine runtime under `<data_dir>/runtime/<ID>` and
/// prunes obsolete runtime revisions beside it.
pub(crate) fn prepare_in(data_dir: &Path) -> Result<PathBuf, String> {
    let root = path(data_dir);
    prepare(&root)?;
    let runtime_root = data_dir.join("runtime");
    #[cfg(target_os = "ios")]
    crate::game_storage::exclude_from_backup(&runtime_root);
    let _ = prune_stale(&runtime_root, ID);
    Ok(root)
}

/// Removes obsolete unpacked Wine runtimes beside `keep_id` without following symlinks.
pub(crate) fn prune_stale(runtime_root: &Path, keep_id: &str) -> std::io::Result<()> {
    match std::fs::symlink_metadata(runtime_root) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    for entry in std::fs::read_dir(runtime_root)? {
        let entry = entry?;
        if entry.file_name().to_str() == Some(keep_id) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else if kind.is_file() {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn unpack(root: &Path, files: &[(&str, Option<u64>)], archive: &[u8]) -> std::io::Result<()> {
    let ready = root.join(".ready");
    if ready.is_file() {
        return Ok(());
    }
    let mut input = lzma_rust2::XzReader::new(archive, false);
    for (name, len) in files {
        let target = root.join(name);
        let Some(len) = len else {
            std::fs::create_dir_all(target)?;
            continue;
        };
        std::fs::create_dir_all(target.parent().unwrap())?;
        let mut output = std::fs::File::create(target)?;
        if std::io::copy(&mut input.by_ref().take(*len), &mut output)? != *len {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
    }
    // Consume and validate the stream checksum and footer before publishing.
    std::io::copy(&mut input, &mut std::io::sink())?;
    std::fs::write(ready, [])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // spec: HOST-SES-12
    #[test]
    fn interrupted_preparation_does_not_publish_an_incomplete_installation() {
        let root = std::env::temp_dir().join(format!("adv-distribution-{}", std::process::id()));
        let mut compressed =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(0)).unwrap();
        compressed
            .write_all(b"authored moduleother module")
            .unwrap();
        let bytes = compressed.finish().unwrap();
        let files = [
            ("empty", None),
            ("lib/module", Some(15)),
            ("lib/other", Some(12)),
            ("lib/zero", Some(0)),
        ];
        assert!(unpack(&root, &files, &bytes[..bytes.len() - 4]).is_err());
        assert!(!root.join(".ready").exists());
        unpack(&root, &files, &bytes).unwrap();
        assert!(root.join("empty").is_dir());
        assert_eq!(
            std::fs::read(root.join("lib/module")).unwrap(),
            b"authored module"
        );
        assert!(root.join(".ready").is_file());
        assert_eq!(
            std::fs::read(root.join("lib/other")).unwrap(),
            b"other module"
        );
        assert_eq!(std::fs::metadata(root.join("lib/zero")).unwrap().len(), 0);
        unpack(&root, &files, b"not read after preparation").unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    // spec: HOST-SES-12
    #[test]
    fn embedded_runtime_is_available_without_a_host_installation() {
        if FILES.is_empty() {
            return;
        }
        let base = std::env::temp_dir().join(format!("adv-wine-embedded-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let stale = base.join("runtime/old-revision");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("ntdll.dll"), b"stale").unwrap();
        let root = prepare_in(&base).unwrap();
        assert!(!stale.exists());
        assert!(
            std::fs::read(root.join("lib/wine/i386-windows/ntdll.dll"))
                .unwrap()
                .starts_with(b"MZ")
        );
        assert!(
            std::fs::read(root.join("lib/wine/i386-unix/win32u.so"))
                .unwrap()
                .starts_with(b"\x7fELF")
        );
        assert!(
            std::fs::read_to_string(root.join("share/hitode/prefix/system.reg"))
                .unwrap()
                .starts_with("WINE REGISTRY")
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    // spec: HOST-SES-12
    #[test]
    fn stale_runtime_installations_are_pruned_without_following_links() {
        let base = std::env::temp_dir().join(format!("adv-runtime-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let runtime = base.join("runtime");
        let outside = base.join("outside");
        std::fs::create_dir_all(runtime.join("current")).unwrap();
        std::fs::create_dir_all(runtime.join("old-build")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(runtime.join("current/.ready"), []).unwrap();
        std::fs::write(runtime.join("old-build/ntdll.dll"), b"old").unwrap();
        std::fs::write(outside.join("keep"), b"keep").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, runtime.join("linked")).unwrap();

        prune_stale(&runtime, "current").unwrap();

        assert!(runtime.join("current/.ready").is_file());
        assert!(!runtime.join("old-build").exists());
        assert!(outside.join("keep").is_file());
        std::fs::remove_dir_all(base).unwrap();
    }
}
