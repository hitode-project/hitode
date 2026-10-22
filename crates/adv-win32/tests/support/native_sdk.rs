//! Compile and run isolated authored x86 SDK controls without recording machine paths.
use std::{path::PathBuf, process::Command};
fn execute(command: &mut Command) -> std::process::Output {
    let output = command.output().expect("run authored SDK fixture");
    if !output.status.success() {
        for line in String::from_utf8_lossy(&output.stderr)
            .lines()
            .chain(String::from_utf8_lossy(&output.stdout).lines())
        {
            if let Some((_, message)) = line.split_once("error:") {
                eprintln!("authored SDK compile error: {message}");
            }
        }
    }
    assert!(
        output.status.success(),
        "authored SDK fixture failed with {:?}",
        output.status.code()
    );
    output
}

pub struct Fixture {
    pub root: PathBuf,
    pub output: std::process::Output,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
pub fn build(source_name: &str, libraries: &[&str]) -> Fixture {
    let sdk = PathBuf::from("C:/Program Files (x86)/Windows Kits/10");
    let version = std::fs::read_dir(sdk.join("Include"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|v| v.starts_with("10.0."))
        .max()
        .unwrap();
    let include = sdk.join("Include").join(&version);
    let library = sdk.join("Lib").join(&version).join("um/x86");
    let root = std::env::temp_dir().join(format!(
        "hitode-authored-sdk-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/fixtures")
        .join(source_name);
    let mut compiler = Command::new("C:/Program Files/LLVM/bin/clang-cl.exe");
    compiler.current_dir(&root).args([
        "/nologo",
        "/c",
        "/O2",
        "/GS-",
        "/GR-",
        "/Zl",
        "/clang:--target=i686-pc-windows-msvc",
        "/Foauthored.obj",
    ]);
    for directory in ["um", "shared", "ucrt"] {
        compiler.arg(format!("/I{}", include.join(directory).display()));
    }
    compiler.arg(source);
    execute(&mut compiler);
    execute(
        Command::new("C:/Program Files/LLVM/bin/lld-link.exe")
            .current_dir(&root)
            .args([
                "/nologo",
                "/machine:x86",
                "/nodefaultlib",
                "/safeseh:no",
                "/timestamp:0",
                "/subsystem:console",
                "/entry:mainCRTStartup",
                "/out:authored.exe",
                "authored.obj",
            ])
            .arg(format!("/libpath:{}", library.display()))
            .args(libraries),
    );
    let output = execute(Command::new(root.join("authored.exe")).current_dir(&root));
    Fixture { root, output }
}
