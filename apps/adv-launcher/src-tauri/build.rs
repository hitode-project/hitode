use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output.status.success().then_some(())?;
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

fn main() {
    // The report's app.build: the commit the launcher was built from. The
    // reflog changes with every commit and checkout of this worktree.
    if let Some(log) = git(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "logs/HEAD",
    ]) {
        println!("cargo:rerun-if-changed={log}");
    }
    let build = git(&["rev-parse", "--short=8", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=ADV_BUILD={build}");
    // Xcode links only the staticlib, which carries static libraries but not
    // the dylib-kind links ffmpeg-sys-next emits. Its own static feature would
    // also link macOS-only frameworks on iOS.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("ios") {
        println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
        let dir = std::env::var("FFMPEG_DIR").expect("FFMPEG_DIR: run tools/macos/build.py --ios");
        println!("cargo:rustc-link-search=native={dir}/lib");
        for lib in ["avformat", "avcodec", "swscale", "swresample", "avutil"] {
            println!("cargo:rustc-link-lib=static={lib}");
        }
    }
    tauri_build::build();
}
