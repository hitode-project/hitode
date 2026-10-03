//! Android link fixups for unicorn-engine-sys.
//!
//! * bionic has pthread inside libc, but unicorn-engine-sys still emits `-lpthread`:
//!   point the linker at an empty stub archive shipped in tools/android/stubs.
//! * QEMU's TCG calls `__clear_cache`, which lives in compiler-rt builtins; the Rust link
//!   line for Android doesn't pull that archive in, so link it explicitly from the NDK.
//!
//! Link-search paths and link-lib from this build script propagate to the final binary.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_HOME");
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_ROOT");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os != "android" || env::var_os("CARGO_FEATURE_UNICORN").is_none() {
        return;
    }
    let target = env::var("TARGET").unwrap();
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        "arm" => "arm",
        "x86" => "i686",
        other => panic!("unsupported Android arch {other}"),
    };

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let stubs = manifest.join("../../tools/android/stubs").join(&target);
    assert!(stubs.join("libpthread.a").is_file(), "missing empty libpthread.a stub in {}", stubs.display());
    println!("cargo:rustc-link-search=native={}", stubs.display());

    let ndk = env::var_os("ANDROID_NDK_HOME")
        .or_else(|| env::var_os("ANDROID_NDK_ROOT"))
        .map(PathBuf::from)
        .expect("ANDROID_NDK_HOME (or ANDROID_NDK_ROOT) must point at the NDK");
    let builtins = find_builtins(&ndk, arch)
        .unwrap_or_else(|| panic!("libclang_rt.builtins-{arch}-android.a not found under {}", ndk.display()));
    println!("cargo:rustc-link-search=native={}", builtins.display());
    println!("cargo:rustc-link-lib=static=clang_rt.builtins-{arch}-android");
}

/// `<ndk>/toolchains/llvm/prebuilt/<host>/lib/clang/<ver>/lib/linux`, newest clang first.
fn find_builtins(ndk: &Path, arch: &str) -> Option<PathBuf> {
    let prebuilt = ndk.join("toolchains/llvm/prebuilt");
    let file = format!("libclang_rt.builtins-{arch}-android.a");
    let mut found = Vec::new();
    for host in prebuilt.read_dir().ok()?.flatten() {
        let clang = host.path().join("lib/clang");
        for ver in clang.read_dir().into_iter().flatten().flatten() {
            let dir = ver.path().join("lib/linux");
            if dir.join(&file).is_file() {
                let major: u32 = ver.file_name().to_string_lossy().split('.').next()?.parse().ok()?;
                found.push((major, dir));
            }
        }
    }
    found.sort();
    found.pop().map(|(_, dir)| dir)
}
