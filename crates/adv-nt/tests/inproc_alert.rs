#![cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_wineserver as _;
mod common;

// spec: SRV-SYNC-2, WINE-THR-1, WINE-THR-2
#[test]
fn alertable_device_wait_delivers_apc() {
    let Some((result, output)) = common::run_probe("inproc_alert.exe", false) else {
        return;
    };
    assert!(
        matches!(result, adv_machine::RunResult::Exited(0)),
        "{result:?}"
    );
    assert_eq!(output.trim(), "apc=42 code=0");
}
