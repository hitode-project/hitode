//! Platform groups: `host_view` hosts supply the native game view (macOS,
//! Android, iOS); `mobile` hosts are the touch-first ones (Android, iOS).
fn main() {
    println!("cargo::rustc-check-cfg=cfg(host_view)");
    println!("cargo::rustc-check-cfg=cfg(mobile)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(os.as_str(), "macos" | "android" | "ios") {
        println!("cargo::rustc-cfg=host_view");
    }
    if matches!(os.as_str(), "android" | "ios") {
        println!("cargo::rustc-cfg=mobile");
    }
}
