use super::*;
use crate::Win32Config;

#[test]
fn per_load_cache_preserves_guest_absence_and_never_falls_back_to_vfs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/loader-file-cache-tests");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("existing.dll"), b"backing file").unwrap();
    let config = Win32Config::new(&root, "fixture.exe", root.join("data"));
    let vfs = Vfs::new(&config.os);
    assert!(FileSource::read(&vfs, r"C:\Game\existing.dll").is_some());
    let mut files = Files::default();
    {
        let source = Source {
            files: &files,
            vfs: &vfs,
        };
        assert!(matches!(
            source.read_for_loader(r"C:\Game\existing.dll"),
            Err(LoadError::DeferredRead(_))
        ));
    }
    files.insert(r"C:\Game\existing.dll".into(), None);
    files.insert(r"C:\Game\guest.dll".into(), Some(b"guest file".to_vec()));
    let source = Source {
        files: &files,
        vfs: &vfs,
    };
    assert_eq!(
        source.read_for_loader(r"c:\game\EXISTING.DLL").unwrap(),
        None
    );
    assert_eq!(
        source.read_for_loader(r"c:\game\GUEST.DLL").unwrap(),
        Some(b"guest file".to_vec())
    );
}
