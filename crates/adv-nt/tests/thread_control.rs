//! The thread-control probe: a busy-wait on the tick count, and threads
//! that are suspended, resumed and terminated by another.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

mod common;

use adv_machine::RunResult;

// spec: WINE-THR-3, WINE-THR-4, WINE-TIME-1
#[test]
fn threads_are_suspended_resumed_and_terminated() {
    let Some((result, output)) = common::run_probe("control.exe", true) else {
        return;
    };
    // It exits with 0 only when every step did what Windows does.
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        ["ticked=1 suspended=0 frozen=1 resumed=1 ran=1 joined=0 code=3 waited=0 waiter=4"]
    );
}
