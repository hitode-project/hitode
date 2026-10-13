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
    tauri_build::build();
}
