//! Authored x86 Windows exception oracle, no games, windows or audio devices.
#![cfg(windows)]
use adv_machine::{Clock, RunResult};
use adv_runtime::boot::Runtime;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};
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

#[test]
fn vectored_and_stack_handlers_resume_full_context_after_yield_like_native() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/exception-dispatch-tests");
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-exception-dispatch.ps1"))
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
    let executable = directory.join("fixture_exception_dispatch.exe");
    let native = directory.join("native");
    std::fs::create_dir_all(&native).unwrap();
    let output = native.join("exception-dispatch.bin");
    if output.exists() {
        std::fs::remove_file(&output).unwrap();
    }
    let mut child = OwnedChild(
        Command::new(&executable)
            .current_dir(&native)
            .creation_flags(0x08000000)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "native {status:?}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned exception oracle timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let expected = std::fs::read(output).unwrap();
    assert_eq!(expected.len(), 237 * 4);
    for backend in ["interpreter", "jit", "unicorn"] {
        let profile = directory.join(backend);
        let output = profile.join("overlay/exception-dispatch.bin");
        if output.exists() {
            std::fs::remove_file(&output).unwrap();
        }
        let mut runtime = Runtime::with_cpu(
            &executable,
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
        let result = runtime.machine.run_virtual(5_000_000);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "{backend}: {result:?}\n{}",
            runtime.report()
        );
        assert!(
            runtime.api_acceptance_eligible(),
            "{backend}: unexpected ABI stub"
        );
        let actual = std::fs::read(output).unwrap();
        assert_eq!(actual.len(), expected.len());
        for (index, (a, b)) in actual
            .as_chunks::<4>()
            .0
            .iter()
            .zip(expected.as_chunks::<4>().0)
            .enumerate()
        {
            assert_eq!(
                u32::from_le_bytes(*a),
                u32::from_le_bytes(*b),
                "{backend} native word {index}"
            );
        }
    }
}
