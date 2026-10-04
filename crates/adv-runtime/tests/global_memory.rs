//! Global/Local ownership uses private native allocations, never host clipboard.
#![cfg(windows)]
use adv_machine::{Clock, RunResult, SliceResult};
use adv_runtime::{RunMode, boot::Runtime};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::OnceLock,
    time::{Duration, Instant},
};
#[path = "support/cpu.rs"]
mod cpu;

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < deadline {
                if matches!(self.0.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn fixtures() -> &'static Path {
    static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
    DIRECTORY.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let directory = root.join("target/global-memory-tests");
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(root.join("tools/fixtures/build-global-memory.ps1"))
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
            "global-memory fixture exceeded its wall-clock bound"
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
    runtime.set_api_mode(RunMode::Strict);
    runtime.machine.sys.clock = Clock::virtual_time();
    runtime
        .machine
        .set_execution_budget(Some(Duration::from_secs(5)));
    runtime
}
#[test]
fn private_native_global_local_locks_flags_mix_and_reallocation_match_three_cpus() {
    use std::os::windows::process::CommandExt;
    let directory = fixtures();
    let output = directory.join("global-memory.bin");
    if output.exists() {
        std::fs::remove_file(&output).unwrap();
    }
    let mut child = OwnedChild(
        Command::new(directory.join("fixture_global_memory.exe"))
            .current_dir(directory)
            .creation_flags(0x08000000)
            .spawn()
            .unwrap(),
    );
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert_eq!(status.code(), Some(0));
            break;
        }
        if Instant::now() > end {
            panic!("owned global oracle timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let reference = std::fs::read(output).unwrap();
    assert_eq!(reference.len(), 420 * 4);
    for backend in ["unicorn", "interpreter", "jit"] {
        let suffix = format!("globals-{backend}");
        let path = directory.join(&suffix).join("overlay/global-memory.bin");
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        let mut runtime = runtime(directory, "fixture_global_memory.exe", backend, &suffix);
        let result = run(&mut runtime);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "{backend}: {result:?}"
        );
        assert!(runtime.api_acceptance_eligible());
        let actual = std::fs::read(path).unwrap();
        assert_eq!(actual.len(), reference.len());
        for (i, (a, b)) in actual
            .as_chunks::<4>()
            .0
            .iter()
            .zip(reference.as_chunks::<4>().0)
            .enumerate()
        {
            assert_eq!(
                u32::from_le_bytes(*a),
                u32::from_le_bytes(*b),
                "{backend}: global native word {i}"
            );
        }
    }
}
