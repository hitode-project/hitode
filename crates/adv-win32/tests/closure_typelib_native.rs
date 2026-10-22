#![cfg(windows)]
//! Generate only authored x86 MSFT metadata with the official SDK and native serializer.
#[path = "support/native_sdk.rs"]
mod native_sdk;
#[test]
fn native_authored_msft_record_enum_alias_and_function_layout() {
    let fixture = native_sdk::build(
        "closure-typelib-native.cpp",
        &["kernel32.lib", "ole32.lib", "oleaut32.lib"],
    );
    assert!(fixture.output.stdout.is_empty());
    let root = &fixture.root;
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
}
