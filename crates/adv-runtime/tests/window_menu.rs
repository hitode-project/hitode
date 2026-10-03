//! Native x86 window creation timing and editable system-menu state.
//! Built-in label contents are tested separately under the virtual en-US locale.
#![cfg(windows)]
use adv_machine::{Clock, RunResult};
use adv_runtime::boot::Runtime;
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
    assert_eq!(reference.len() % 4, 0);
    let words: Vec<_> = reference
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| u32::from_le_bytes(*b))
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
            .as_chunks::<4>()
            .0
            .iter()
            .zip(reference.as_chunks::<4>().0.iter())
            .enumerate()
        {
            assert_eq!(a, b, "{backend}, result word {i}");
        }
        let labels = std::fs::read(profile.join("overlay/window-menu-labels.bin")).unwrap();
        let expected = [
            "&Restore",
            "&Move",
            "&Size",
            "Mi&nimize",
            "Ma&ximize",
            "&Close\tAlt+F4",
        ];
        assert_eq!(labels.len(), expected.len() * 64);
        for (bytes, expected) in labels.as_chunks::<64>().0.iter().zip(expected) {
            let units: Vec<_> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .take_while(|&unit| unit != 0)
                .collect();
            assert_eq!(String::from_utf16(&units).unwrap(), expected, "{backend}");
        }
    }
}
