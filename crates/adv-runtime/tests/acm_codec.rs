//! The same authored x86 program exercises native Windows ACM and the three
//! test CPUs. Built-in decoding uses Symphonia; application-local function
//! drivers execute their public DriverProc callbacks in the guest.
#![cfg(windows)]
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use adv_machine::{Clock, RunResult, SliceResult};
use adv_runtime::boot::Runtime;
#[path = "support/cpu.rs"]
mod cpu;

fn run_guest(exe: &Path, profile: &Path, backend: &str) {
    let mut runtime =
        Runtime::with_cpu(exe, profile, cpu::Cpu::new(backend).unwrap(), 32, 32).unwrap();
    runtime.machine.sys.clock = Clock::virtual_time();
    runtime
        .machine
        .set_execution_budget(Some(Duration::from_secs(10)));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            Instant::now() < deadline,
            "ACM fixture wall deadline {backend}"
        );
        match runtime.step_for(Duration::from_millis(8)) {
            SliceResult::Yielded => {}
            SliceResult::Complete(RunResult::Idle {
                next_deadline: Some(at),
            }) => runtime.machine.sys.clock.advance_to(at),
            SliceResult::Complete(RunResult::Exited(code)) => {
                assert_eq!(code, 0, "ACM fixture {}/{backend}", exe.display());
                break;
            }
            other => panic!("ACM fixture {}/{backend}: {other:?}", exe.display()),
        }
    }
}

#[test]
fn application_function_drivers_convert_and_release_like_windows() {
    use std::os::windows::process::CommandExt;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("target/acm-function-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-mmio.ps1"))
        .arg("-OutputDirectory")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let exe = output.join("fixture_acm_function.exe");
    let native = Command::new(&exe)
        .current_dir(&output)
        .creation_flags(0x08000000)
        .status()
        .unwrap();
    assert_eq!(native.code(), Some(0), "native function-driver fixture");
    let reference = std::fs::read(output.join("acm-function.bin")).unwrap();
    for backend in ["interpreter", "jit", "unicorn"] {
        let data = output.join(backend);
        run_guest(&exe, &data, backend);
        let actual = std::fs::read(data.join("overlay/acm-function.bin")).unwrap();
        assert_eq!(reference, actual, "ACM function driver {backend}");
    }
}

#[test]
fn pcm_and_adpcm_stream_headers_lifetime_and_samples_match_windows() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("target/acm-codec-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-mmio.ps1"))
        .arg("-OutputDirectory")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let exe = output.join("fixture_acm_codec.exe");
    let native = Command::new(&exe).current_dir(&output).status().unwrap();
    assert_eq!(native.code(), Some(0), "native Windows ACM fixture");
    let report = std::fs::read(output.join("acm-results.bin")).unwrap();
    let names = [
        "acm-pcm8.pcm",
        "acm-pcm16.pcm",
        "acm-ima-mono.pcm",
        "acm-ima-stereo.pcm",
        "acm-ms-mono.pcm",
        "acm-ms-stereo.pcm",
        "acm-ms-edges.pcm",
        "acm-ima-edges.pcm",
    ];
    let references: Vec<_> = names
        .iter()
        .map(|name| std::fs::read(output.join(name)).unwrap())
        .collect();
    for backend in ["interpreter", "jit", "unicorn"] {
        let profile = output.join(backend);
        run_guest(&exe, &profile, backend);
        let actual_report = std::fs::read(profile.join("overlay/acm-results.bin")).unwrap();
        assert_eq!(
            actual_report, report,
            "MMRESULT, format suggestion, size and header lifecycle {backend}"
        );
        for (name, reference) in names.iter().zip(&references) {
            let actual = std::fs::read(profile.join("overlay").join(name)).unwrap();
            assert_eq!(actual.len(), reference.len(), "PCM length {name}/{backend}");
            let mismatch = actual.iter().zip(reference).position(|(a, b)| a != b);
            assert!(
                mismatch.is_none(),
                "native PCM samples {name}/{backend}: first different byte {mismatch:?}"
            );
            eprintln!(
                "ACM {name}/{backend}: {} bytes match Windows exactly",
                actual.len()
            );
            if backend != "interpreter" {
                assert_eq!(
                    actual,
                    std::fs::read(output.join("interpreter/overlay").join(name)).unwrap(),
                    "all production codec paths emit identical samples {name}/{backend}"
                );
            }
        }
        run_guest(
            &output.join("fixture_acm_codec_boundaries.exe"),
            &output.join(format!("{backend}-boundaries")),
            backend,
        );
        eprintln!(
            "ACM errors, permissions, truncated blocks and four bounded 64 KiB conversions/{backend}: passed"
        );
    }
}
