//! Wine's ntdll.dll starts the gate-1 probe: the loader loads its imports
//! and a library and runs its TLS callback; it sleeps, waits, and resumes
//! from a fault.
#![cfg(target_vendor = "apple")]

mod common;

use adv_machine::RunResult;

#[test]
fn the_probe_starts_waits_and_resumes() {
    let Some(result) = common::run_probe("gate1.exe") else {
        return;
    };
    // It exits with 0 only when every step did what Windows does.
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}
