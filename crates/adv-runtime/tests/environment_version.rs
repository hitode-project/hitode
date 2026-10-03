#![cfg(windows)]
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use adv_machine::{Clock, RunResult, SliceResult};
use adv_runtime::boot::Runtime;
#[path = "support/cpu.rs"]
mod cpu;

#[test]
fn native_windows_environment_and_relative_version_queries_match_three_cpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/environment-version-tests");
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-environment-version.ps1"))
        .arg("-OutputDirectory")
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status = Command::new(directory.join("fixture_environment_version.exe"))
        .current_dir(&directory)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let reference = std::fs::read(directory.join("environment-version.bin")).unwrap();
    assert!(reference.len() > 200 * 4);
    for name in ["unicorn", "interpreter", "jit"] {
        let overlay = directory.join(name);
        let mut runtime = Runtime::with_cpu(
            directory.join("fixture_environment_version.exe"),
            &overlay,
            cpu::Cpu::new(name).unwrap(),
            32,
            32,
        )
        .unwrap();
        runtime.machine.sys.clock = Clock::virtual_time();
        runtime
            .machine
            .set_execution_budget(Some(Duration::from_secs(10)));
        loop {
            match runtime.step_for(Duration::from_millis(8)) {
                SliceResult::Yielded => {}
                SliceResult::Complete(RunResult::Exited(code)) => {
                    assert_eq!(code, 0, "{name}");
                    break;
                }
                SliceResult::Complete(RunResult::Idle {
                    next_deadline: Some(at),
                }) => runtime.machine.sys.clock.advance_to(at),
                other => panic!("{name}: {other:?}"),
            }
        }
        let actual = std::fs::read(overlay.join("overlay/environment-version.bin")).unwrap();
        let expected_words: Vec<_> = reference
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
        let actual_words: Vec<_> = actual
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
        assert_eq!(
            actual_words.len(),
            expected_words.len(),
            "{name} observation count"
        );
        for (index, (actual, expected)) in actual_words.iter().zip(&expected_words).enumerate() {
            assert_eq!(
                actual, expected,
                "{name} native environment/version observation {index}"
            );
        }
    }
}
