#![cfg(windows)]
//! Generate only authored x86 MSFT metadata with the official SDK and native serializer.
use std::{path::PathBuf, process::Command};
fn execute(command: &mut Command) {
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
}
#[test]
fn native_authored_msft_record_enum_alias_and_function_layout() {
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
        "hitode-authored-typelib-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/fixtures/closure-typelib-native.cpp");
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
                "kernel32.lib",
                "ole32.lib",
                "oleaut32.lib",
            ])
            .arg(format!("/libpath:{}", library.display())),
    );
    execute(Command::new(root.join("authored.exe")).current_dir(&root));
    let bytes = std::fs::read(root.join("authored-closure.tlb")).unwrap();
    assert_eq!(&bytes[..4], b"MSFT");
    // Never record a temporary path or arbitrary host contents in the generated sample.
    let path = root.to_string_lossy();
    assert!(
        !bytes
            .windows(path.len())
            .any(|part| part == path.as_bytes())
    );
    let wide = path
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    assert!(!bytes.windows(wide.len()).any(|part| part == wide));
    eprintln!(
        "authored-msft length={} hex={}",
        bytes.len(),
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    std::fs::remove_dir_all(root).unwrap();
}
