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
    // The debugger's `info`: the commit this was built from, and whether
    // tracked files differed. Reruns when HEAD or the index moves.
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    for path in ["logs/HEAD", "index"] {
        if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", path]) {
            println!("cargo::rerun-if-changed={path}");
        }
    }
    let commit = git(&["rev-parse", "--short=8", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|status| !status.is_empty());
    println!(
        "cargo::rustc-env=ADV_SHELL_BUILD={commit}{}",
        if dirty { "+dirty" } else { "" }
    );
}
