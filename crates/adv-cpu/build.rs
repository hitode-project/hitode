//! Portable exact guest floating-point arithmetic.
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_HOME");
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_ROOT");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64")
    {
        link_android_cache_builtins();
    }
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
    if matches!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("linux" | "android")
    ) && matches!(
        std::env::var("CARGO_CFG_TARGET_ARCH").as_deref(),
        Ok("x86_64" | "aarch64")
    ) {
        cc::Build::new()
            .file("src/host_fault/posix.c")
            .file("src/host_fault/probe.S")
            .opt_level(3)
            .compile("adv_posix_fault");
        println!("cargo:rerun-if-changed=src/host_fault/posix.c");
        println!("cargo:rerun-if-changed=src/host_fault/probe.S");
    }
    let mut build = cc::Build::new();
    build
        .include("softfloat/include")
        .define("SOFTFLOAT_FAST_INT64", None)
        .opt_level(3)
        .warnings(false)
        .file("softfloat/shim.c");
    let mut sources: Vec<_> = std::fs::read_dir("softfloat/source")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "c"))
        .collect();
    sources.sort();
    build.files(sources).compile("adv_softfloat");
    println!("cargo:rerun-if-changed=softfloat");
}

/// Android's libc does not export `__clear_cache`; use the NDK implementation.
fn link_android_cache_builtins() {
    let ndk = std::env::var_os("ANDROID_NDK_HOME")
        .or_else(|| std::env::var_os("ANDROID_NDK_ROOT"))
        .map(PathBuf::from)
        .expect("ANDROID_NDK_HOME (or ANDROID_NDK_ROOT) must point at the NDK");
    let directory = find_android_cache_builtins(&ndk).unwrap_or_else(|| {
        panic!(
            "libclang_rt.builtins-aarch64-android.a not found under {}",
            ndk.display()
        )
    });
    println!("cargo:rustc-link-search=native={}", directory.display());
    println!("cargo:rustc-link-lib=static=clang_rt.builtins-aarch64-android");
}

/// Standard NDK layout, choosing the newest available Clang resource directory.
fn find_android_cache_builtins(ndk: &Path) -> Option<PathBuf> {
    let mut found = Vec::new();
    for host in ndk
        .join("toolchains/llvm/prebuilt")
        .read_dir()
        .ok()?
        .flatten()
    {
        let clang = host.path().join("lib/clang");
        for version in clang.read_dir().into_iter().flatten().flatten() {
            let directory = version.path().join("lib/linux");
            if !directory
                .join("libclang_rt.builtins-aarch64-android.a")
                .is_file()
            {
                continue;
            }
            let Ok(version) = version
                .file_name()
                .to_string_lossy()
                .split('.')
                .map(str::parse)
                .collect::<Result<Vec<u32>, _>>()
            else {
                continue;
            };
            found.push((version, directory));
        }
    }
    found.sort();
    found.pop().map(|(_, directory)| directory)
}
