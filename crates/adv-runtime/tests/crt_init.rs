//! Native and guest x86 CRT callbacks, including suspension and error propagation.
#![cfg(windows)]
use adv_machine::{Clock, RunResult, SliceResult};
use adv_runtime::boot::Runtime;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
#[path = "support/cpu.rs"]
mod cpu;

#[test]
fn constructor_walkers_match_native_short_circuit_and_callback_mutation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("target/crt-init-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-crt.ps1"))
        .arg("-OutputDirectory")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert_eq!(
        Command::new(output.join("fixture_crt_native.exe"))
            .current_dir(&output)
            .status()
            .unwrap()
            .code(),
        Some(0)
    );
    let reference = std::fs::read(output.join("crt-init.bin")).unwrap();
    for backend in ["interpreter", "jit", "unicorn"] {
        for variant in ["native", "aliases"] {
            let profile = output.join(format!("{backend}-{variant}"));
            let mut runtime = Runtime::with_cpu(
                output.join(format!("fixture_crt_{variant}.exe")),
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
            loop {
                match runtime.step_for(Duration::from_millis(8)) {
                    SliceResult::Yielded => {}
                    SliceResult::Complete(RunResult::Idle {
                        next_deadline: Some(at),
                    }) => runtime.machine.sys.clock.advance_to(at),
                    SliceResult::Complete(RunResult::Exited(code)) => {
                        assert_eq!(code, 0, "{backend}/{variant}");
                        break;
                    }
                    other => panic!("CRT {backend}/{variant}: {other:?}"),
                }
            }
            let actual = std::fs::read(profile.join("overlay/crt-init.bin")).unwrap();
            assert_eq!(
                actual,
                reference.repeat(if variant == "aliases" { 3 } else { 1 }),
                "CRT results and callback order {backend}/{variant}"
            );
        }
    }
}
