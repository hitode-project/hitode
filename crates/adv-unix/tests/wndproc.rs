//! The window procedure probe: user32 calls window procedures back
//! through win32u.so, nested, while they draw and wait.
#![cfg(target_vendor = "apple")]

#[path = "../../adv-nt/tests/common/mod.rs"]
mod common;

use adv_machine::RunResult;

// spec: WINE-SYS-6, WINE-UNIX-3
#[test]
fn window_procedures_nest_and_return() {
    let Some((result, output)) =
        common::run_probe_on("wndproc.exe", false, adv_unix::Platform, |machine| {
            adv_unix::install(machine)
        })
    else {
        return;
    };
    assert!(
        matches!(result, RunResult::Exited(0)),
        "{result:?}\n{output}"
    );
    // The numbers Wine prints for the probe on its null driver.
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        ["created=1 sent=47 nested=7 deepest=2 drawn=1 waited=258 posted=21 quit=5 destroyed=1"]
    );
}
