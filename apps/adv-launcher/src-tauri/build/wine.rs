//! Bundle portable Wine files and their manifest without recording host paths.
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

fn stage_asset(source: &Path, asset: &Path) {
    let bytes = std::fs::read(source).unwrap();
    // Keep the timestamp stable when Cargo reruns with identical inputs.
    if !std::fs::read(asset).is_ok_and(|existing| existing == bytes) {
        std::fs::write(asset, bytes).unwrap();
    }
}

pub fn bundle() {
    println!("cargo:rerun-if-env-changed=ADV_WINE_DIST");
    println!("cargo:rerun-if-changed=build/wine.rs");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let android = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android");
    let assets = if android {
        println!("cargo:rerun-if-env-changed=HITODE_ANDROID_ASSET_DIR");
        std::env::var_os("HITODE_ANDROID_ASSET_DIR").map(PathBuf::from)
    } else {
        None
    };
    let archive_path = output.join("wine.xz");
    let root = std::env::var_os("ADV_WINE_DIST").map(PathBuf::from);
    let mut generated = String::new();
    if let Some(root) = root.as_deref() {
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
        files(root, &mut paths);
        paths.sort();
        // Tauri also compiles before invoking Gradle. Only the Gradle build
        // needs to generate an Android archive.
        let mut compressed = (!android || assets.is_some()).then(|| {
            let mut options = XzOptions::with_preset(9);
            options.lzma_options.nice_len = 273;
            options.lzma_options.depth_limit = 512;
            let archive = std::fs::File::create(&archive_path).unwrap();
            XzWriter::new(archive, options).unwrap()
        });
        let mut hash = Sha256::new();
        generated.push_str("pub const FILES: &[(&str, Option<u64>)] = &[\n");
        for path in &paths {
            let name = path
                .strip_prefix(root)
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
            if let Some(compressed) = &mut compressed {
                compressed.write_all(&bytes).unwrap();
            }
            writeln!(generated, "({name:?}, Some({})),", bytes.len()).unwrap();
        }
        if let Some(compressed) = compressed {
            compressed.finish().unwrap();
        }
        writeln!(
            generated,
            "];\n#[cfg(not(target_os = \"android\"))]\npub const ARCHIVE: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/wine.xz\"));\npub const ID: &str = \"{:x}\";",
            hash.finalize()
        )
        .unwrap();
    } else {
        // Source-only CI can check the launcher without a distribution. Player
        // startup reports the missing build input instead of selecting old Win32.
        generated.push_str(
            "pub const FILES: &[(&str, Option<u64>)] = &[];\n#[cfg(not(target_os = \"android\"))]\npub const ARCHIVE: &[u8] = &[];\npub const ID: &str = \"unavailable\";\n",
        );
    }
    if let Some(assets) = assets {
        let soundfont = Path::new("../../../vendor/fluidr3mono/FluidR3Mono_GM.sf3");
        let wine_asset = assets.join("wine.xz");
        let soundfont_asset = assets.join("FluidR3Mono_GM.sf3");
        for input in [soundfont, &wine_asset, &soundfont_asset] {
            println!("cargo:rerun-if-changed={}", input.display());
        }
        std::fs::create_dir_all(&assets).unwrap();
        stage_asset(soundfont, &soundfont_asset);
        if root.is_some() {
            stage_asset(&archive_path, &wine_asset);
        } else if wine_asset.exists() {
            std::fs::remove_file(wine_asset).unwrap();
        }
    }
    std::fs::write(output.join("wine.rs"), generated).unwrap();
}
