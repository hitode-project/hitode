use super::*;

// spec: WIN32-GDI-15
#[test]
fn resource_entry_points_use_the_single_generated_abi() {
    let mut registry = Registry::default();
    register(&mut registry);
    for name in [
        "AddFontResourceA",
        "AddFontResourceW",
        "RemoveFontResourceA",
        "RemoveFontResourceW",
        "AddFontResourceExA",
        "AddFontResourceExW",
        "RemoveFontResourceExA",
        "RemoveFontResourceExW",
    ] {
        assert_eq!(
            registry.level("gdi32.dll", name),
            Some(adv_abi::ImplementationLevel::Implement)
        );
    }
    assert!(registry.signature_conflicts().is_empty());
}
