//! Authored SRW lock exclusion, shared ownership and blocking match native Windows.
#![cfg(windows)]
use adv_machine::{Clock, RunResult};
use adv_runtime::boot::Runtime;
use std::{os::windows::process::CommandExt, path::Path, process::Command};
#[path = "support/cpu.rs"]
mod cpu;

#[test]
// spec: WIN32-OBJECT-2
fn srw_api_pairs_and_contention_match_windows() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("target/investigations/srw/native-differential");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-srw.ps1"))
        .arg("-OutputDirectory")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let exe = output.join("fixture_srw.exe");
    assert!(
        Command::new(&exe)
            .current_dir(&output)
            .creation_flags(0x08000000)
            .status()
            .unwrap()
            .success()
    );
    let expected = std::fs::read(output.join("srw.bin")).unwrap();
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
        let actual = std::fs::read(data.join("overlay/srw.bin")).unwrap();
        assert_eq!(actual.len(), expected.len(), "{backend}");
        for (index, (actual, expected)) in actual
            .chunks_exact(4)
            .zip(expected.chunks_exact(4))
            .enumerate()
        {
            assert_eq!(actual, expected, "{backend}: scalar {index}");
        }
    }
}
