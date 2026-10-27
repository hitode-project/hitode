//! The same embedded Wine installation on desktop and mobile. Only file
//! preparation lives here; execution and prefix policy belong to adv-shell.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

include!(concat!(env!("OUT_DIR"), "/wine.rs"));

static PREPARING: Mutex<()> = Mutex::new(());

pub(crate) fn path(data: &Path) -> PathBuf {
    data.join("runtime").join(ID)
}

pub(crate) fn prepare(root: &Path) -> Result<(), String> {
    if FILES.is_empty() {
        return Err("Wine runtime is missing from this player build".into());
    }
    let _guard = PREPARING.lock().unwrap();
    unpack(root, FILES, ARCHIVE).map_err(|e| format!("prepare Wine distribution: {e}"))
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
        let root = std::env::temp_dir().join(format!("adv-wine-embedded-{}", std::process::id()));
        prepare(&root).unwrap();
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
        std::fs::remove_dir_all(root).unwrap();
    }
}
