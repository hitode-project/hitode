//! Virtual time reaches a timer armed after its waiter blocked, and alerts
//! persist and coalesce before the waiting thread resumes.
#![cfg(target_vendor = "apple")]

mod common;

use adv_machine::RunResult;

// spec: WINE-THR-1, WINE-THR-2
#[test]
fn alerts_and_timer_apcs_keep_their_wakeups() {
    let Some((result, output)) = common::run_probe("sync-wake.exe", false) else {
        return;
    };
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(output.trim(), "alerts=1 timer=1 apcs=3");
}
