//! Personality invariant 1 (PERS-EXPORT-2, PERS-EXPORT-3) over the production
//! fronts: discovery exposes only implemented or reviewed names, and every
//! review names a callable ABI with a registered failure.
use adv_abi::{personality, signature, stub, system_exports};

#[test]
// spec: PERS-EXPORT-2
fn later_client_package_b_exports_have_no_registered_frontend_or_review() {
    let registry = adv_runtime::boot::api_registry(adv_runtime::vorbis::Policy::Guest);
    let scope = [
        "user32.dll",
        "comctl32.dll",
        "comdlg32.dll",
        "imm32.dll",
        "gdi32.dll",
        "msimg32.dll",
        "gdiplus.dll",
        "ddraw.dll",
        "d3d9.dll",
        "winmm.dll",
        "avifil32.dll",
        "msacm32.dll",
        "msvfw32.dll",
        "dsound.dll",
    ];
    let later: Vec<_> = include_str!("../../../data/abi/post-win7.tsv")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            assert_eq!(fields.len(), 4);
            assert!(fields.iter().all(|field| !field.is_empty()));
            (fields[0], fields[1])
        })
        .collect();
    assert!(later.windows(2).all(|rows| rows[0] < rows[1]));
    for (module, name) in later
        .into_iter()
        .filter(|(module, _)| scope.contains(module))
    {
        assert!(
            !registry.is_implemented(module, name),
            "later API registered: {module}!{name}"
        );
        assert!(
            !personality::is_reviewed(module, name),
            "later API reviewed: {module}!{name}"
        );
        assert!(
            signature::lookup(module, name).is_some(),
            "uncatalogued exclusion: {module}!{name}"
        );
        assert!(!registry.dynamic_available(module, name));
    }
}

#[test]
// spec: PERS-EXPORT-2, PERS-EXPORT-3
fn discovery_exposes_only_implemented_or_completely_reviewed_names() {
    for policy in [
        adv_runtime::vorbis::Policy::Guest,
        adv_runtime::vorbis::Policy::Native,
    ] {
        let mut registry = adv_runtime::boot::api_registry(policy);
        registry.set_expose_fixed_exports(false);
        let core = system_exports::CORE_MODULES
            .iter()
            .flat_map(|module| system_exports::exports(module))
            .map(|export| (export.module, export.name));
        let sdk = signature::signatures()
            .iter()
            .map(|signature| (signature.module, signature.name));
        for (module, name) in core.chain(sdk) {
            if registry.dynamic_available(module, name) {
                assert!(
                    registry
                        .classify(module, name)
                        .is_some_and(|class| class.discoverable()),
                    "{module}!{name} is discoverable without implementation or review"
                );
            }
        }
        for (module, name) in personality::reviewed_exports() {
            let signature = signature::lookup(module, name)
                .unwrap_or_else(|| panic!("reviewed {module}!{name} has no ABI declaration"));
            assert!(
                !registry.is_implemented(module, name),
                "{module}!{name} is implemented; remove its review"
            );
            assert!(
                stub::reviewed_failure(signature).is_some(),
                "reviewed {module}!{name} lacks a callable ABI or registered failure"
            );
        }
    }
}
