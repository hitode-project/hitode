//! Android link fixes for Unicorn's QEMU backend (same requirements as pal-cpu).
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_HOME");
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_ROOT");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    // bionic provides pthread in libc; satisfy Unicorn's extra -lpthread request.
    std::fs::write(out.join("libpthread.a"), b"!<arch>\n").unwrap();
    println!("cargo:rustc-link-search=native={}", out.display());
    let ndk = PathBuf::from(
        std::env::var_os("ANDROID_NDK_HOME")
            .or_else(|| std::env::var_os("ANDROID_NDK_ROOT"))
            .expect("set ANDROID_NDK_HOME"),
    );
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let name = format!("libclang_rt.builtins-{arch}-android.a");
    let mut found = Vec::new();
    for host in ndk
        .join("toolchains/llvm/prebuilt")
        .read_dir()
        .unwrap()
        .flatten()
    {
        for version in host.path().join("lib/clang").read_dir().unwrap().flatten() {
            let dir = version.path().join("lib/linux");
            if dir.join(&name).is_file() {
                found.push(dir);
            }
        }
    }
    found.sort();
    let dir = found.pop().expect("NDK compiler-rt builtins missing");
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=clang_rt.builtins-{arch}-android");
}
