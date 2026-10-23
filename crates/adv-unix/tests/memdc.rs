//! The memory-DC probe: gdi32 and win32u.so draw into caller-owned pixels
//! through `D3DKMTCreateDCFromMemory`.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

#[path = "../../adv-nt/tests/common/mod.rs"]
mod common;

use adv_machine::RunResult;

// spec: WINE-SYS-3, WINE-SYS-4, WINE-UNIX-1, WINE-UNIX-2, WINE-UNIX-3
#[test]
fn gdi_fills_a_memory_dc() {
    let Some((result, output)) =
        common::run_probe_on("memdc.exe", false, adv_unix::Platform, |machine| {
            adv_unix::install(machine)
        })
    else {
        return;
    };
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        ["created=0 filled=1 inside=1 outside=1 destroyed=0"]
    );
}
