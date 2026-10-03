//! Keep legacy mismatches observable while individual behavior functions are migrated.
use adv_abi::Registry;

#[test]
fn legacy_signature_audit() {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let conflicts = registry.signature_conflicts();
    let unknown = registry.missing_signatures();
    eprintln!("known ABI conflicts: {}", conflicts.len());
    for conflict in &conflicts {
        eprintln!(
            "{}!{}: registered {:?}, metadata {:?}",
            conflict.module, conflict.name, conflict.registered, conflict.metadata
        );
    }
    eprintln!("no metadata signature: {}", unknown.len());
    for name in &unknown {
        eprintln!("{name}");
    }
    assert!(
        conflicts.is_empty(),
        "known x86 ABI registration conflicts: {conflicts:?}"
    );
    assert!(registry.coverage().implement >= 5);
}
