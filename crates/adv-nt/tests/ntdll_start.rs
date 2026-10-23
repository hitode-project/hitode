//! Wine's ntdll.dll starts the gate-1 probe: the loader loads its imports
//! and a library and runs its TLS callback; it sleeps, waits, and resumes
//! from a fault.
#![cfg(target_vendor = "apple")]

mod common;

use adv_machine::RunResult;

// spec: WINE-SRV-2, WINE-SRV-3, WINE-SYS-2
#[test]
fn the_probe_starts_waits_and_resumes() {
    let Some((result, output)) = common::run_probe("gate1.exe", false) else {
        return;
    };
    // It exits with 0 only when every step did what Windows does.
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines.len(), 1, "{output:?}");
    let addresses = lines[0]
        .strip_prefix("tls=00001234 callbacks=1 module=")
        .and_then(|line| line.strip_suffix(" waits=258,0 faults=1"))
        .expect("probe output");
    let (module, proc) = addresses.split_once(" proc=").expect("probe addresses");
    assert!(u32::from_str_radix(module, 16).is_ok(), "{module:?}");
    assert!(u32::from_str_radix(proc, 16).is_ok(), "{proc:?}");
}
