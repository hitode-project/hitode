//! Authored RIFF/memory file behavior compared with native 32-bit Windows.
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
fn memory_and_vfs_riff_io_matches_native_windows() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/mmio-tests");
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-mmio.ps1"))
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
    let native_status = Command::new(directory.join("fixture_mmio.exe"))
        .current_dir(&native)
        .creation_flags(0x0800_0000)
        .status()
        .unwrap();
    assert_eq!(native_status.code(), Some(0));
    let reference = std::fs::read(native.join("mmio-report.bin")).unwrap();
    for backend in ["jit", "interpreter", "unicorn"] {
        let profile = directory.join(backend);
        let mut runtime = Runtime::with_cpu(
            directory.join("fixture_mmio.exe"),
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
                other => panic!("MMIO {backend}: {other:?}"),
            }
        }
        let actual = std::fs::read(profile.join("overlay/mmio-report.bin")).unwrap();
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
                "MMIO word {i}, {backend}"
            );
        }
    }
}
