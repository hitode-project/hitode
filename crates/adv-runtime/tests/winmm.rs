//! Multimedia callback/event lifetime compared with native Windows.
//! The fixture never opens a window, device, or socket.
#![cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use adv_machine::{Clock, Conv, Outcome, RunResult, SliceResult, WakePriority};
use adv_runtime::boot::Runtime;
#[path = "support/cpu.rs"]
mod cpu;

fn fixtures() -> &'static PathBuf {
    static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
    DIRECTORY.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let directory = root.join("target/winmm-tests");
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(root.join("tools/fixtures/build-winmm.ps1"))
            .arg("-OutputDirectory")
            .arg(&directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        directory
    })
}

fn run(backend: &str, executable: &str, profile: &Path, budget_probe: bool) {
    let mut runtime = Runtime::with_cpu(
        fixtures().join(executable),
        profile,
        cpu::Cpu::new(backend).unwrap(),
        32,
        32,
    )
    .unwrap();
    runtime.machine.sys.clock = Clock::virtual_time();
    if budget_probe {
        // Runtime loading has already finalized winmm.dll's live EAT. These
        // private probes belong to a fixture module that the guest loads only
        // after registration; never fabricate late exports in a published DLL.
        let registry = runtime
            .machine
            .sys
            .thunks
            .resolver_mut::<adv_abi::Registry>()
            .unwrap();
        registry.add(
            "adv_winmm_test.dll",
            "__test_audio_ready",
            Conv::Stdcall { argc: 1 },
            |env| {
                env.sys
                    .set_event_with_priority(env.arg(0), WakePriority::Audio);
                Outcome::Return(0)
            },
        );
        registry.add(
            "adv_winmm_test.dll",
            "__test_advance_guest",
            Conv::Stdcall { argc: 1 },
            |env| {
                let at = env.sys.clock.guest_now_us() + u64::from(env.arg(0));
                env.sys.clock.advance_to(at);
                Outcome::Return(0)
            },
        );
    }
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
            other => panic!("WinMM {backend}: {other:?}"),
        }
    }
    if budget_probe {
        // Main, the sole persistent multimedia worker, and the refill waiter.
        assert_eq!(runtime.machine.sys.contexts.len(), 3);
    }
}

#[test]
fn multimedia_timers_callbacks_events_and_synchronous_kill_match_native() {
    let directory = fixtures();
    let native = directory.join("native");
    std::fs::create_dir_all(&native).unwrap();
    let native_status = Command::new(directory.join("fixture_winmm.exe"))
        .current_dir(&native)
        .creation_flags(0x0800_0000)
        .status()
        .unwrap();
    assert_eq!(native_status.code(), Some(0));
    let reference = std::fs::read(native.join("winmm-report.bin")).unwrap();
    for backend in ["jit", "interpreter", "unicorn"] {
        let profile = directory.join(backend);
        run(backend, "fixture_winmm.exe", &profile, false);
        let actual = std::fs::read(profile.join("overlay/winmm-report.bin")).unwrap();
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
                "WinMM word {i}, {backend}"
            );
        }
    }
}

#[test]
fn multimedia_timer_budget_reuse_and_audio_refill_fairness() {
    // Returns are ordered by the authored guest-only fixture; it does not make
    // any native system timer-resolution request or create an audio device.
    let expected: [u32; 24] = [
        97, 0, 0, 0, 0, 97, 97, 97, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 1, 1, 1,
    ];
    for backend in ["jit", "interpreter", "unicorn"] {
        let profile = fixtures().join(format!("budget-{backend}"));
        run(backend, "fixture_winmm_budget.exe", &profile, true);
        let bytes = std::fs::read(profile.join("overlay/winmm-budget.bin")).unwrap();
        let words: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect();
        assert_eq!(words, expected, "WinMM budget/fairness {backend}");
    }
}
