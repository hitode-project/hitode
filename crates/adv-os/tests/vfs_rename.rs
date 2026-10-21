//! Authored file renames across writable and read-only VFS layers.
use adv_os::{OsConfig, Vfs, vfs::host};
use std::path::Path;

#[test]
// spec: WIN32-OBJECT-8
fn file_renames_preserve_read_only_layers_and_persist_old_name_whiteouts() {
    let tmp = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/vfs-renames")
        .join(std::process::id().to_string());
    for replacement in [false, true] {
        for shadow in [false, true] {
            let root = tmp.join(format!("{replacement}-{shadow}"));
            host::create_dir_all(&root.join("game/Save")).unwrap();
            host::create_dir_all(&root.join("resources/Save")).unwrap();
            host::write(&root.join("game/Save/old.dat"), b"game").unwrap();
            host::write(&root.join("resources/Save/old.dat"), b"resource").unwrap();
            host::write(&root.join("game/Save/new.dat"), b"previous").unwrap();
            let mut cfg = OsConfig::new(root.join("game"), "authored.exe", root.join("data"));
            cfg.resource_dir = replacement.then(|| root.join("resources"));
            let v = Vfs::new(&cfg);
            if shadow {
                host::write(&v.write_path("Save/old.dat").unwrap(), b"shadow").unwrap();
                host::write(&v.write_path("Save/New.DAT").unwrap(), b"old shadow").unwrap();
            }
            let expected: &[u8] = if shadow {
                b"shadow"
            } else if replacement {
                b"resource"
            } else {
                b"game"
            };
            assert!(v.lookup("Save/old.dat").is_some());
            assert!(v.lookup("Save/new.dat").is_some());
            v.rename_file("Save/old.dat", "Save/new.dat").unwrap();
            assert!(v.lookup("Save/old.dat").is_none());
            assert_eq!(
                std::fs::read(v.lookup("Save/new.dat").unwrap()).unwrap(),
                expected
            );
            assert_eq!(v.list("Save").unwrap().len(), 1);
            drop(v);
            let v = Vfs::new(&cfg);
            assert!(v.lookup("Save/old.dat").is_none());
            assert_eq!(
                std::fs::read(v.lookup("Save/new.dat").unwrap()).unwrap(),
                expected
            );
            v.delete_file("Save/new.dat").unwrap();
            drop(v);
            let v = Vfs::new(&cfg);
            assert!(v.lookup("Save/new.dat").is_none());
            host::write(&v.write_path("Save/old.dat").unwrap(), b"new save").unwrap();
            assert_eq!(
                std::fs::read(v.lookup("Save/old.dat").unwrap()).unwrap(),
                b"new save"
            );
            assert_eq!(
                std::fs::read(root.join("game/Save/old.dat")).unwrap(),
                b"game"
            );
            assert_eq!(
                std::fs::read(root.join("resources/Save/old.dat")).unwrap(),
                b"resource"
            );
            assert_eq!(
                std::fs::read(root.join("game/Save/new.dat")).unwrap(),
                b"previous"
            );
        }
    }
    std::fs::remove_dir_all(tmp).unwrap();
}

#[test]
// spec: WIN32-OBJECT-8
fn file_rename_failures_keep_whiteouts_and_case_only_renames_keep_one_visible_name() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/vfs-rename-errors")
        .join(std::process::id().to_string());
    host::create_dir_all(&root.join("game/blocked")).unwrap();
    host::write(&root.join("game/Slot.dat"), b"source").unwrap();
    host::write(&root.join("game/blocked/target.dat"), b"target").unwrap();
    let cfg = OsConfig::new(root.join("game"), "authored.exe", root.join("data"));
    let v = Vfs::new(&cfg);
    v.delete_file("blocked/target.dat").unwrap();
    host::create_dir_all(&root.join("data/overlay")).unwrap();
    host::write(&root.join("data/overlay/blocked"), b"not a directory").unwrap();
    assert!(v.rename_file("Slot.dat", "blocked/target.dat").is_err());
    assert!(v.lookup("Slot.dat").is_some());
    assert!(v.lookup("blocked/target.dat").is_none());
    assert!(v.has_read_only_underlay("Slot.dat"));
    drop(v);
    let v = Vfs::new(&cfg);
    assert!(v.lookup("blocked/target.dat").is_none());
    v.rename_file("Slot.dat", "sLOT.DAT").unwrap();
    assert_eq!(
        std::fs::read(v.lookup("SLOT.dat").unwrap()).unwrap(),
        b"source"
    );
    let slots: Vec<_> = v
        .list(".")
        .unwrap()
        .into_iter()
        .filter(|e| e.name.eq_ignore_ascii_case("slot.dat"))
        .map(|e| e.name)
        .collect();
    assert_eq!(slots, ["sLOT.DAT"]);
    v.delete_file("slot.dat").unwrap();
    drop(v);
    let v = Vfs::new(&cfg);
    assert!(v.lookup("slot.dat").is_none());
    assert_eq!(
        std::fs::read(root.join("game/Slot.dat")).unwrap(),
        b"source"
    );
    std::fs::remove_dir_all(root).unwrap();
}
