//! Native x86 thread creation establishes the flag and completion oracle.
#![cfg(windows)]
use adv_machine::{Clock, RunResult};
use adv_runtime::boot::Runtime;
use std::{os::windows::process::CommandExt, path::Path, process::Command};
#[path = "support/cpu.rs"]
mod cpu;

#[test]
fn win32_creation_flags_match_native_for_local_and_remote_threads() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("target/thread-create-flags-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-module-lifecycle.ps1"))
        .arg("-OutputDirectory")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let exe = output.join("fixture_thread_create_flags.exe");
    let status = Command::new(&exe)
        .current_dir(&output)
        .creation_flags(0x08000000)
        .status()
        .unwrap();
    assert!(status.success(), "native thread flag oracle: {status}");
    let expected = std::fs::read(output.join("thread-create-flags.bin")).unwrap();
    for backend in ["interpreter", "jit", "unicorn"] {
        let data = output.join(backend);
        let mut runtime =
            Runtime::with_cpu(&exe, &data, cpu::Cpu::new(backend).unwrap(), 32, 32).unwrap();
        runtime.machine.sys.clock = Clock::virtual_time();
        assert!(
            matches!(
                runtime.machine.run_virtual(10_000_000),
                RunResult::Exited(0)
            ),
            "{backend}"
        );
        assert_eq!(
            std::fs::read(data.join("overlay/thread-create-flags.bin")).unwrap(),
            expected,
            "{backend}"
        );
    }
}
