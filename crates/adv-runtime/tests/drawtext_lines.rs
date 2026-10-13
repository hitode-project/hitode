//! Authored A/W paragraph output matches native Windows DIB pixels and heights.
#![cfg(windows)]
use adv_machine::{Clock, RunResult};
use adv_runtime::boot::Runtime;
use std::{os::windows::process::CommandExt, path::Path, process::Command};
#[path = "support/cpu.rs"]
mod cpu;

#[test]
// spec: WIN32-GDI-14
fn paragraph_dib_pixels_and_heights_match_native() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("target/investigations/drawtext-lines/native-differential");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-drawtext-lines.ps1"))
        .arg("-OutputDirectory")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let exe = output.join("fixture_drawtext_lines.exe");
    assert!(
        Command::new(&exe)
            .current_dir(&output)
            .creation_flags(0x08000000)
            .status()
            .unwrap()
            .success()
    );
    let expected = std::fs::read(output.join("drawtext-lines.bin")).unwrap();
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
        let actual = std::fs::read(data.join("overlay/drawtext-lines.bin")).unwrap();
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
