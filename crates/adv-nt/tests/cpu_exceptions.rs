//! CPU exceptions reach a vectored handler with the codes, addresses, saved
//! EIPs and parameters measured on Windows 11 WoW64, on both engines.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

mod common;
#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

use adv_machine::RunResult;

/// The probe's output on Windows 11 (10.0.26300) x86, measured 2026-11-24.
const WINDOWS: &str = "probe free
bound_in none
bound_out c000008c addr=+0 eip=+0 n=0
int1 c0000005 addr=+0 eip=+0 n=2 0 ffffffff
int3 80000003 addr=+1 eip=+1 n=1 0
int4 c0000095 addr=+1 eip=+2 n=0
int20 c0000005 addr=+0 eip=+0 n=2 0 ffffffff
int2c c0000420 addr=+0 eip=+0 n=0
int80 c0000005 addr=+0 eip=+0 n=2 0 ffffffff
";

fn engine(native: bool) {
    let (result, output) = common::run_probe_engine(
        "cpu_exceptions.exe",
        false,
        "null",
        adv_nt::dispatch::Platform,
        |_, _| {},
        |_| {},
        native,
    )
    .expect("the parent checked the installation");
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(output.replace("\r\n", "\n"), WINDOWS);
}

#[test]
#[ignore = "subprocess fixture: one Wine server per process"]
fn interpreted() {
    engine(false);
}
#[test]
#[ignore = "subprocess fixture: one Wine server per process"]
fn native() {
    engine(true);
}

// spec: NT-SYS-7
#[test]
fn bound_and_interrupt_gates_raise_what_windows_raises() {
    if wine::wine().is_none() {
        return;
    }
    for name in ["interpreted", "native"] {
        let output = std::process::Command::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/test-timeout.sh"),
        )
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .env("ADV_TEST_LIMIT", "120")
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "{name}\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
