//! Balanced AVI library initialization compared with native 32-bit Windows.
//! Void EAX is deliberately excluded; file/stream codecs are not claimed.
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
fn avi_initialization_and_teardown_preserve_the_native_abi() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/avi-tests");
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-avi.ps1"))
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
    let native_status = Command::new(directory.join("fixture_avi_lifecycle.exe"))
        .current_dir(&native)
        .creation_flags(0x0800_0000)
        .status()
        .unwrap();
    assert_eq!(native_status.code(), Some(0));
    let reference = std::fs::read(native.join("avi-report.bin")).unwrap();
    assert_eq!(
        reference, [0; 24],
        "six stdcall void calls leave the stack intact"
    );
    for backend in ["jit", "interpreter", "unicorn"] {
        let profile = directory.join(backend);
        let mut runtime = Runtime::with_cpu(
            directory.join("fixture_avi_lifecycle.exe"),
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
                other => panic!("AVI {backend}: {other:?}"),
            }
        }
        let actual = std::fs::read(profile.join("overlay/avi-report.bin")).unwrap();
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
                "AVI word {i}, {backend}"
            );
        }
    }
}
