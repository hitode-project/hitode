//! `cfg(native)`: hosts that run translated code. Elsewhere the executor
//! interprets.
//! `cfg(emulator)`: hosts whose tests run translated code under Unicorn,
//! everywhere except Android.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(native, emulator)");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if arch == "aarch64" && matches!(os.as_str(), "linux" | "android" | "macos" | "ios") {
        println!("cargo::rustc-cfg=native");
    }
    if os != "android" {
        println!("cargo::rustc-cfg=emulator");
    }
}
