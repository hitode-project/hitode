//! The gate-2 probe: two threads wake each other, a wait times out, and a
//! queued APC ends an alertable sleep.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

mod common;

use adv_machine::RunResult;

// spec: WINE-THR-1, WINE-THR-2
#[test]
fn threads_wait_wake_and_run_an_apc() {
    let Some((result, output)) = common::run_probe("gate2.exe", false) else {
        return;
    };
    // It exits with 0 only when every step did what Windows does.
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        ["ready=0 joined=0 code=7 apcs=1 argument=42"]
    );
}
