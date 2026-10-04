//! Owned x86 fixtures: native whole-string measurements and strict missing drawing.
#![cfg(windows)]
use adv_machine::{Clock, RunResult, SliceResult};
use adv_runtime::{RunMode, boot::Runtime};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    time::{Duration, Instant},
};
#[path = "support/cpu.rs"]
mod cpu;

fn fixtures() -> &'static Path {
    static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
    DIRECTORY.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let directory = root.join("target/gdi-text-query-tests");
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(root.join("tools/fixtures/build-gdi-text-query.ps1"))
            .arg("-OutputDirectory")
            .arg(&directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        directory
    })
}
fn run(runtime: &mut Runtime<cpu::Cpu>) -> RunResult {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            Instant::now() < deadline,
            "query fixture exceeded its wall-clock bound"
        );
        match runtime.step_for(Duration::from_millis(4)) {
            SliceResult::Yielded => {}
            SliceResult::Complete(RunResult::Idle {
                next_deadline: Some(at),
            }) => runtime.machine.sys.clock.advance_to(at),
            SliceResult::Complete(result) => return result,
        }
    }
}
fn runtime(directory: &Path, executable: &str, backend: &str, suffix: &str) -> Runtime<cpu::Cpu> {
    let mut runtime = Runtime::with_cpu(
        directory.join(executable),
        directory.join(suffix),
        cpu::Cpu::new(backend).unwrap(),
        64,
        32,
    )
    .unwrap();
    runtime.machine.sys.clock = Clock::virtual_time();
    runtime
        .machine
        .set_execution_budget(Some(Duration::from_secs(5)));
    runtime
}
#[test]
fn selected_fonts_and_counted_ansi_utf16_extents_match_native_and_three_cpus() {
    use std::os::windows::process::CommandExt;
    let directory = fixtures();
    let output = directory.join("gdi-text-query.bin");
    match std::fs::remove_file(&output) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("remove stale native query output: {error}"),
    }
    let mut native = Command::new(directory.join("fixture_gdi_query.exe"))
        .current_dir(directory)
        .creation_flags(0x08000000)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = native.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            native.kill().unwrap();
            native.wait().unwrap();
            panic!("owned native text query timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(status.code(), Some(0));
    let reference = std::fs::read(&output).unwrap();
    assert!(reference.len() > 700 * 4);
    for backend in ["unicorn", "interpreter", "jit"] {
        let suffix = format!("query-{backend}");
        let actual_path = directory.join(&suffix).join("overlay/gdi-text-query.bin");
        if actual_path.exists() {
            std::fs::remove_file(&actual_path).unwrap();
        }
        let mut runtime = runtime(directory, "fixture_gdi_query.exe", backend, &suffix);
        let result = run(&mut runtime);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "{backend}: {result:?}"
        );
        assert!(
            runtime.api_acceptance_eligible(),
            "{backend}: query cannot silently call stubs"
        );
        let actual = std::fs::read(actual_path).unwrap();
        assert_eq!(actual.len(), reference.len());
        for (index, (actual, expected)) in actual
            .as_chunks::<4>()
            .0
            .iter()
            .zip(reference.as_chunks::<4>().0)
            .enumerate()
        {
            assert_eq!(
                u32::from_le_bytes(*actual),
                u32::from_le_bytes(*expected),
                "{backend}: native query word {index}"
            );
        }
    }
}
#[test]
fn missing_textout_stops_strict_and_discovery_returns_only_bool_failure() {
    let directory = fixtures();
    for backend in ["interpreter", "jit"] {
        for mode in [RunMode::Strict, RunMode::Discovery] {
            let suffix = format!("stub-{backend}-{mode:?}");
            let actual_path = directory.join(&suffix).join("overlay/gdi-text-query.bin");
            if actual_path.exists() {
                std::fs::remove_file(&actual_path).unwrap();
            }
            let mut runtime = runtime(directory, "fixture_gdi_textout-stub.exe", backend, &suffix);
            runtime.set_api_mode(mode);
            let result = run(&mut runtime);
            match (&result, mode) {
                (RunResult::Failed(error), RunMode::Strict) => assert!(
                    error
                        .message
                        .contains("gdi32.dll!TextOutA (strict ABI stub)")
                ),
                (RunResult::Exited(0), RunMode::Discovery) => assert_eq!(
                    std::fs::read(actual_path).unwrap(),
                    [0u32, 1]
                        .into_iter()
                        .flat_map(u32::to_le_bytes)
                        .collect::<Vec<_>>()
                ),
                _ => panic!("{backend}/{mode:?}: {result:?}"),
            }
            let diagnostics = runtime.api_diagnostics().unwrap();
            assert_eq!(diagnostics.stub_calls, 1);
            assert_eq!(diagnostics.gaps["gdi32.dll!TextOutA"], 1);
            assert!(!runtime.api_acceptance_eligible());
            assert!(
                runtime
                    .missing_apis()
                    .iter()
                    .any(|name| name == "gdi32.dll!TextOutA")
            );
        }
    }
}
