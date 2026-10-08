//! Authored native first/second-chance calls must not repeat handler search.
#![cfg(windows)]
use adv_machine::{Clock, RunResult};
use adv_runtime::boot::Runtime;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
#[path = "support/cpu.rs"]
mod cpu;

const CODE: u32 = 0xe0421001;

#[test]
fn second_chance_terminates_without_reentering_handlers_like_native() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/investigations/exception-chance-tests");
    let built = Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(root.join("tools/fixtures/build-exception-dispatch.ps1"))
        .args(["-Fixture", "native-exception-chance", "-OutputDirectory"])
        .arg(&directory)
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let fixture = directory.join("fixture_exception_native-exception-chance.exe");
    for first in [0, 1] {
        let native = directory.join(format!("native-{first}"));
        std::fs::create_dir_all(&native).unwrap();
        let executable = native.join(format!("chance-{first}.exe"));
        std::fs::copy(&fixture, &executable).unwrap();
        // This authored process always exits or raises the supplied fatal code.
        let native_status = Command::new(&executable)
            .current_dir(&native)
            .creation_flags(0x08000000)
            .status()
            .unwrap();
        assert_eq!(
            native_status.code().unwrap() as u32,
            if first == 0 { CODE } else { 0 }
        );
        let expected: Vec<_> = [first, CODE, first, 0x12345678u32]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        assert_eq!(std::fs::read(native.join("chance.bin")).unwrap(), expected);
        for backend in ["interpreter", "jit", "unicorn"] {
            let profile = directory.join(format!("{backend}-{first}"));
            let mut runtime = Runtime::with_cpu(
                &executable,
                &profile,
                cpu::Cpu::new(backend).unwrap(),
                32,
                32,
            )
            .unwrap();
            runtime.machine.sys.clock = Clock::virtual_time();
            let result = runtime.machine.run_virtual(5_000_000);
            if first == 0 {
                assert!(
                    matches!(result, RunResult::Failed(_)),
                    "{backend}: {result:?}"
                );
                assert_eq!(runtime.machine.sys.guest_failure_code, Some(CODE));
            } else {
                assert!(
                    matches!(result, RunResult::Exited(0)),
                    "{backend}: {result:?}"
                );
            }
            assert_eq!(
                std::fs::read(profile.join("overlay/chance.bin")).unwrap(),
                expected,
                "{backend}"
            );
        }
    }
}
