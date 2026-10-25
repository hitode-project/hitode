//! Embed portable Wine files without host paths or platform resource APIs.
use lzma_rust2::{XzOptions, XzWriter};
use sha2::{Digest, Sha256};
use std::{
    fmt::Write,
    io::Write as _,
    path::{Path, PathBuf},
};

fn files(directory: &Path, output: &mut Vec<PathBuf>) {
    println!("cargo:rerun-if-changed={}", directory.display());
    for entry in std::fs::read_dir(directory).expect("read Wine distribution") {
        let path = entry.expect("Wine directory entry").path();
        output.push(path.clone());
        if path.is_dir() {
            files(&path, output);
        }
    }
}

pub fn bundle() {
    println!("cargo:rerun-if-env-changed=ADV_WINE_DIST");
    println!("cargo:rerun-if-changed=build/wine.rs");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let mut generated = String::new();
    if let Some(root) = std::env::var_os("ADV_WINE_DIST").map(PathBuf::from) {
        for required in [
            "lib/wine/i386-windows/ntdll.dll",
            "lib/wine/i386-unix/win32u.so",
            "share/hitode/prefix/system.reg",
        ] {
            assert!(
                root.join(required).is_file(),
                "incomplete ADV_WINE_DIST: missing {required}"
            );
        }
        let mut paths = Vec::new();
        files(&root, &mut paths);
        paths.sort();
        let mut options = XzOptions::with_preset(9);
        options.lzma_options.nice_len = 273;
        options.lzma_options.depth_limit = 512;
        let archive = std::fs::File::create(output.join("wine.xz")).unwrap();
        let mut compressed = XzWriter::new(archive, options).unwrap();
        let mut hash = Sha256::new();
        generated.push_str("pub const FILES: &[(&str, Option<u64>)] = &[\n");
        for path in &paths {
            let name = path
                .strip_prefix(&root)
                .unwrap()
                .to_str()
                .expect("Unicode Wine path")
                .replace('\\', "/");
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name.as_bytes());
            hash.update([u8::from(path.is_dir())]);
            if path.is_dir() {
                writeln!(generated, "({name:?}, None),").unwrap();
                continue;
            }
            let bytes = std::fs::read(path).expect("read Wine file");
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(&bytes);
            compressed.write_all(&bytes).unwrap();
            writeln!(generated, "({name:?}, Some({})),", bytes.len()).unwrap();
        }
        compressed.finish().unwrap();
        writeln!(
            generated,
            "];\npub const ARCHIVE: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/wine.xz\"));\npub const ID: &str = \"{:x}\";",
            hash.finalize()
        )
        .unwrap();
    } else {
        // Source-only CI can check the launcher without a distribution. Player
        // startup reports the missing build input instead of selecting old Win32.
        generated.push_str(
            "pub const FILES: &[(&str, Option<u64>)] = &[];\npub const ARCHIVE: &[u8] = &[];\npub const ID: &str = \"unavailable\";\n",
        );
    }
    std::fs::write(output.join("wine.rs"), generated).unwrap();
}
