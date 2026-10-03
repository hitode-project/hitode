//! Winsock lifecycle and x86 WSADATA compared with native Windows.
//! Named/ordinal APIs are equivalent; no sockets or network I/O are used.
#![cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use adv_machine::{Clock, RunResult, SliceResult};
use adv_runtime::boot::Runtime;
#[path = "support/cpu.rs"]
mod cpu;

#[test]
fn winsock_versions_data_layout_errors_and_balanced_cleanup_match_native() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/winsock-tests");
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-winsock.ps1"))
        .arg("-OutputDirectory")
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let native = directory.join("native");
    std::fs::create_dir_all(&native).unwrap();
    let native_status = Command::new(directory.join("fixture_winsock.exe"))
        .current_dir(&native)
        .creation_flags(0x0800_0000)
        .status()
        .unwrap();
    assert_eq!(native_status.code(), Some(0));
    let reference = std::fs::read(native.join("winsock-report.bin")).unwrap();
    assert_eq!(
        reference.len(),
        8512,
        "two complete named/ordinal fixture passes"
    );
    for backend in ["jit", "interpreter", "unicorn"] {
        let profile = directory.join(backend);
        let mut runtime = Runtime::with_cpu(
            directory.join("fixture_winsock.exe"),
            &profile,
            cpu::Cpu::new(backend).unwrap(),
            32,
            32,
        )
        .unwrap();
        runtime.machine.sys.clock = Clock::virtual_time();
        runtime
            .machine
            .set_execution_budget(Some(Duration::from_secs(5)));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline);
            match runtime.step_for(Duration::from_millis(8)) {
                SliceResult::Yielded => (),
                SliceResult::Complete(RunResult::Idle {
                    next_deadline: Some(at),
                }) => runtime.machine.sys.clock.advance_to(at),
                SliceResult::Complete(RunResult::Exited(code)) => {
                    assert_eq!(code, 0);
                    break;
                }
                other => panic!("Winsock {backend}: {other:?}"),
            }
        }
        let actual = std::fs::read(profile.join("overlay/winsock-report.bin")).unwrap();
        assert_eq!(reference.len(), actual.len());
        let expected_words = reference.as_chunks::<4>().0;
        for (i, (a, b)) in actual
            .as_chunks::<4>()
            .0
            .iter()
            .zip(expected_words)
            .enumerate()
        {
            assert_eq!(
                u32::from_le_bytes(*a),
                u32::from_le_bytes(*b),
                "Winsock word {i}, {backend}"
            );
        }
    }
}
