//! Guest-only clipboard contracts. NEVER execute this PE as a host process.
#![cfg(windows)]
use adv_machine::{Clock, RunResult, SliceResult};
use adv_runtime::{RunMode, boot::Runtime};
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
#[path = "support/cpu.rs"]
mod cpu;
#[test]
fn isolated_clipboard_shared_global_ownership_callbacks_and_thread_cleanup_on_three_cpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/clipboard-input-tests");
    let built = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-clipboard-guest.ps1"))
        .arg("-OutputDirectory")
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let mut reference = None;
    for backend in ["unicorn", "interpreter", "jit"] {
        let profile = directory.join(backend);
        let output = profile.join("overlay/clipboard-guest.bin");
        if output.exists() {
            std::fs::remove_file(&output).unwrap();
        }
        let mut runtime = Runtime::with_cpu(
            directory.join("fixture_clipboard_guest.exe"),
            &profile,
            cpu::Cpu::new(backend).unwrap(),
            64,
            48,
        )
        .unwrap();
        runtime.set_api_mode(RunMode::Strict);
        runtime.machine.sys.clock = Clock::virtual_time();
        runtime
            .machine
            .set_execution_budget(Some(Duration::from_secs(5)));
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = loop {
            assert!(Instant::now() < deadline, "clipboard {backend} wall limit");
            match runtime.step_for(Duration::from_millis(4)) {
                SliceResult::Yielded => {}
                SliceResult::Complete(RunResult::Idle {
                    next_deadline: Some(at),
                }) => runtime.machine.sys.clock.advance_to(at),
                SliceResult::Complete(result) => break result,
            }
        };
        let bytes = std::fs::read(output).unwrap_or_default();
        let words: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| u32::from_le_bytes(*b))
            .collect();
        for (i, &word) in words.iter().enumerate() {
            assert_eq!(word, 1, "{backend} clipboard contract {i}");
        }
        assert!(
            matches!(result, RunResult::Exited(0)),
            "{backend}: {result:?}\n{}",
            runtime.report()
        );
        assert!(runtime.api_acceptance_eligible());
        assert_eq!(bytes.len(), 76 * 4, "{backend}: complete clipboard oracle");
        if let Some(reference) = &reference {
            assert_eq!(&bytes, reference);
        } else {
            reference = Some(bytes);
        }
    }
}
