//! The gate-2 probe: two threads wake each other, a wait times out, and a
//! queued APC ends an alertable sleep.
#![cfg(target_vendor = "apple")]

mod common;

use adv_machine::RunResult;

#[test]
fn threads_wait_wake_and_run_an_apc() {
    let Some(result) = common::run_probe("gate2.exe") else {
        return;
    };
    // It exits with 0 only when every step did what Windows does.
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}
