//! Native code cache fingerprint and Windows fault hooks; nothing on AArch64
//! hosts, where the crate is empty.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        return;
    }
    cache_fingerprint();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        cc::Build::new()
            .file("src/host_fault.c")
            .opt_level(3)
            .compile("adv_host_fault");
        println!("cargo:rerun-if-changed=src/host_fault.c");
    }
}

/// Reject native artifacts after any compiler, recovery ABI or dependency edit.
fn cache_fingerprint() {
    fn visit(path: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(path).expect("CPU source directory") {
            let path = entry.expect("CPU source entry").path();
            if path.is_dir() {
                visit(&path, files);
            } else {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    // Generated code embeds core state layouts and calls core helpers.
    visit(Path::new("src"), &mut files);
    visit(Path::new("../adv-cpu-core/src"), &mut files);
    files.extend(
        [
            "build.rs",
            "Cargo.toml",
            "../adv-cpu-core/build.rs",
            "../adv-cpu-core/Cargo.toml",
            "../../Cargo.lock",
        ]
        .map(PathBuf::from),
    );
    files.sort();
    let mut hash = Sha256::new();
    let compiler = std::process::Command::new(std::env::var_os("RUSTC").expect("Rust compiler"))
        .arg("-vV")
        .output()
        .expect("Rust compiler version for native cache ABI");
    assert!(compiler.status.success(), "Rust compiler version failed");
    hash.update(compiler.stdout);
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = std::fs::read(&path).expect("cache fingerprint input");
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    for name in [
        "TARGET",
        "PROFILE",
        "CARGO_CFG_TARGET_FEATURE",
        "CARGO_ENCODED_RUSTFLAGS",
    ] {
        hash.update(name.as_bytes());
        hash.update(std::env::var(name).unwrap_or_default().as_bytes());
    }
    println!(
        "cargo:rustc-env=ADV_CPU_CODE_CACHE_FINGERPRINT={:x}",
        hash.finalize()
    );
}
