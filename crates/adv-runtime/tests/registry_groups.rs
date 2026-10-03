#![cfg(windows)]
//! Native oracle touches only the authored fixture's newly created volatile
//! HKCU subtree; the fixture closes its handles and removes that exact tree.
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use adv_runtime::boot::Runtime;
use pal_machine::{Clock, RunResult, SliceResult};
#[path = "support/cpu.rs"]
mod cpu;

fn words(bytes: &[u8]) -> Vec<u32> {
    assert_eq!(bytes.len() % 4, 0);
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .collect()
}

#[test]
fn registry_enumeration_metadata_and_deletion_match_native_windows_on_three_cpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/registry-groups-tests");
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-registry.ps1"))
        .arg("-OutputDirectory")
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status = Command::new(directory.join("fixture_registry_groups.exe"))
        .current_dir(&directory)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let reference = words(&std::fs::read(directory.join("registry-groups.bin")).unwrap());
    assert!(reference.len() > 60);
    assert_eq!(
        reference.last(),
        Some(&0),
        "native fixture must delete its owned subtree"
    );
    for name in ["unicorn", "interpreter", "jit"] {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let overlay = directory.join(format!("{name}-{unique}"));
        let mut runtime = Runtime::with_cpu(
            directory.join("fixture_registry_groups.exe"),
            &overlay,
            cpu::Cpu::new(name).unwrap(),
            32,
            32,
        )
        .unwrap();
        runtime.machine.sys.clock = Clock::virtual_time();
        runtime
            .machine
            .set_execution_budget(Some(Duration::from_secs(10)));
        loop {
            match runtime.step_for(Duration::from_millis(8)) {
                SliceResult::Yielded => {}
                SliceResult::Complete(RunResult::Exited(code)) => {
                    assert_eq!(code, 0, "{name}");
                    break;
                }
                SliceResult::Complete(RunResult::Idle {
                    next_deadline: Some(at),
                }) => {
                    runtime.machine.sys.clock.advance_to(at);
                }
                other => panic!("{name}: {other:?}"),
            }
        }
        let actual = words(&std::fs::read(overlay.join("overlay/registry-groups.bin")).unwrap());
        assert_eq!(actual.len(), reference.len(), "{name}");
        for (index, (actual, expected)) in actual.iter().zip(&reference).enumerate() {
            assert_eq!(actual, expected, "{name} registry observation {index}");
        }
    }
}
