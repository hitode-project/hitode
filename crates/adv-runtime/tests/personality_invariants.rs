//! Personality invariant 1 (docs/personality-contracts.md) over the production
//! fronts: discovery exposes only implemented or reviewed names, and every
//! review names a callable ABI with a registered failure.
use adv_abi::{personality, signature, stub, system_exports};

#[test]
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
