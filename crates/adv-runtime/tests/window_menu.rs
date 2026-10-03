//! Native x86 window creation timing and editable system-menu state.
#![cfg(windows)]
use adv_runtime::boot::Runtime;
use pal_machine::{Clock, RunResult};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
#[path = "support/cpu.rs"]
mod cpu;

#[test]
fn window_geometry_and_system_menu_match_native_windows() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/window-menu-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-window-menu.ps1"))
        .arg("-OutputDirectory")
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let exe = directory.join("fixture_window_menu.exe");
    let native = directory.join("native");
    std::fs::create_dir_all(&native).unwrap();
    assert!(
        Command::new(&exe)
            .current_dir(&native)
            .creation_flags(0x08000000)
            .status()
            .unwrap()
            .success()
    );
    let reference = std::fs::read(native.join("window-menu.bin")).unwrap();
    let words: Vec<_> = reference
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    assert_eq!(
        &words[..16],
        &[0, 0, 1, 1, 1, 1, 0, 0, 0, 0, 1, 1, 1, 1, 0, 0]
    );
    for backend in ["jit", "interpreter", "unicorn"] {
        let profile = directory.join(backend);
        let mut runtime =
            Runtime::with_cpu(&exe, &profile, cpu::Cpu::new(backend).unwrap(), 32, 32).unwrap();
        runtime.machine.sys.clock = Clock::virtual_time();
        runtime
            .machine
            .set_execution_budget(Some(Duration::from_secs(5)));
        assert!(
            matches!(runtime.machine.run_virtual(5_000_000), RunResult::Exited(0)),
            "{backend}: {}",
            runtime.report()
        );
        let actual = std::fs::read(profile.join("overlay/window-menu.bin")).unwrap();
        assert_eq!(actual.len(), reference.len());
        for (i, (a, b)) in actual
            .chunks_exact(4)
            .zip(reference.chunks_exact(4))
            .enumerate()
        {
            assert_eq!(a, b, "{backend}, result word {i}");
        }
    }
}
