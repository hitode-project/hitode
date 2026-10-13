//! Authored x86 D3DX axis rotations and aliased Vec2 operations match native Windows.
#![cfg(windows)]
use adv_machine::{Clock, RunResult};
use adv_runtime::boot::Runtime;
use std::{os::windows::process::CommandExt, path::Path, process::Command};
#[path = "support/cpu.rs"]
mod cpu;

#[test]
// spec: GFX-D3DX-2
fn d3dx_math2_fronts_match_native() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("target/investigations/d3dx-math2/native-differential");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-d3d9.ps1"))
        .args(["-Name", "d3dx9-math2"])
        .arg("-OutputDirectory")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let exe = output.join("fixture_d3dx9-math2.exe");
    assert!(
        Command::new(&exe)
            .current_dir(&output)
            .creation_flags(0x08000000)
            .status()
            .unwrap()
            .success()
    );
    let expected = std::fs::read(output.join("d3dx9-math2.bin")).unwrap();
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
        let actual = std::fs::read(data.join("overlay/d3dx9-math2.bin")).unwrap();
        assert_eq!(actual.len(), expected.len(), "{backend}");
        let mut index = 0;
        let mut calls = 0;
        while index < expected.len() {
            let words = u32::from_le_bytes(expected[index..index + 4].try_into().unwrap()) as usize;
            assert_eq!(
                &actual[index..index + 8],
                &expected[index..index + 8],
                "{backend}: record {calls}"
            );
            assert_eq!(
                u32::from_le_bytes(actual[index + 4..index + 8].try_into().unwrap()),
                1
            );
            index += 8;
            for component in 0..words {
                let a = f32::from_le_bytes(actual[index..index + 4].try_into().unwrap());
                let e = f32::from_le_bytes(expected[index..index + 4].try_into().unwrap());
                assert!(
                    (a.is_nan() && e.is_nan()) || a == e || (a - e).abs() <= 2e-6 * e.abs().max(1.),
                    "{backend}: record {calls} component {component}: {a} != {e}"
                );
                index += 4;
            }
            calls += 1;
        }
        assert_eq!(calls, 44);
    }
}
