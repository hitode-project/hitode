//! Authored menu graph/resources with invisible native windows, no game/device.
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
fn menus_attach_load_resources_and_release_recursive_graphs_like_native() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/menu-graph-tests");
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-menu-graph.ps1"))
        .arg("-OutputDirectory")
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let native = directory.join("native");
    std::fs::create_dir_all(&native).unwrap();
    let exe = directory.join("fixture_menu_graph.exe");
    assert!(
        Command::new(&exe)
            .current_dir(&native)
            .creation_flags(0x08000000)
            .status()
            .unwrap()
            .success()
    );
    let reference = std::fs::read(native.join("menu-graph.bin")).unwrap();
    assert!(reference.len() > 250 * 4);
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
        let actual = std::fs::read(profile.join("overlay/menu-graph.bin")).unwrap();
        assert_eq!(
            actual.len(),
            reference.len(),
            "{backend}: observation count"
        );
        for (at, (actual, native)) in actual
            .as_chunks::<4>()
            .0
            .iter()
            .zip(reference.as_chunks::<4>().0)
            .enumerate()
        {
            assert_eq!(
                u32::from_le_bytes(*actual),
                u32::from_le_bytes(*native),
                "{backend}: menu graph word {at}"
            );
        }
    }
}
