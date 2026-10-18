//! Native Windows oracle for independent address spaces and loader handoff.
#![cfg(windows)]
use adv_machine::{RunResult, SliceResult};
use adv_runtime::boot::Runtime;
use std::os::windows::process::CommandExt;
use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
#[path = "support/cpu.rs"]
mod backend;

fn target_directory(root: &std::path::Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"))
}

// spec: PROC-ISO-1, PROC-CREATE-2, PROC-CREATE-3, PROC-MEM-1, PROC-HANDLE-1, PROC-SCHED-3
#[test]
fn isolated_processes_match_windows_and_survive_loader_exit() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = target_directory(&root).join("guest-processes-oracle");
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-guest-processes.ps1"))
        .arg("-OutputDirectory")
        .arg(&directory)
        .arg("-RunNative")
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "native fixture: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let names = [
        "process-parent.bin",
        "process-child.bin",
        "process-child-late.bin",
    ];
    let golden: Vec<_> = names
        .iter()
        .map(|name| std::fs::read(directory.join(name)).unwrap())
        .collect();
    for name in ["interpreter", "jit", "unicorn"] {
        let data = directory.join(format!("{name}-{}", std::process::id()));
        let cpu = backend::Cpu::new(name).unwrap();
        let mut runtime = Runtime::with_cpu(
            directory.join("fixture_guest_parent.exe"),
            &data,
            cpu,
            32,
            16,
        )
        .unwrap();
        runtime.machine.cpu.set_async_compilation(false);
        let trace = std::fs::File::create(directory.join(format!("{name}.trace"))).unwrap();
        runtime.machine.sys.trace = adv_machine::Trace::to_writer(Box::new(trace));
        runtime.machine.sys.trace.all = true;
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < Duration::from_secs(45),
                "{name}: process family stalled: {:?}\n{}",
                runtime.process_statuses(),
                runtime.report()
            );
            match runtime.step_for(Duration::from_millis(4)) {
                SliceResult::Complete(RunResult::Exited(code)) => {
                    assert_eq!(code, 37, "{name}");
                    break;
                }
                SliceResult::Complete(RunResult::Failed(error)) => {
                    panic!("{name}: {error}\n{}", runtime.report())
                }
                SliceResult::Complete(RunResult::Idle { .. }) => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                SliceResult::Complete(RunResult::HostRequest) => {
                    panic!("supervisor leaked a request")
                }
                SliceResult::Yielded => {}
            }
        }
        for (file, expected) in names.iter().zip(&golden) {
            let actual = std::fs::read(data.join("overlay").join(file))
                .unwrap_or_else(|e| panic!("{name} {file}: {e}; {:?}", runtime.process_statuses()));
            let words = |bytes: &[u8]| {
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|v| u32::from_le_bytes(*v))
                    .collect::<Vec<_>>()
            };
            assert_eq!(words(&actual), words(expected), "{name} {file}");
        }
        let statuses = runtime.process_statuses();
        assert_eq!(statuses.len(), 3, "{name}: {statuses:?}");
        assert_eq!(
            statuses.iter().map(|s| s.exit_code).collect::<Vec<_>>(),
            [Some(0), Some(37), Some(37)]
        );
        assert!(statuses.iter().all(|s| s.failure.is_none()));
        assert_eq!(runtime.foreground_pid(), statuses[2].pid);
        assert!(
            runtime.api_acceptance_eligible(),
            "{name}: {}",
            runtime.report()
        );
    }
}

// spec: PROC-CREATE-3
#[test]
#[ignore = "requires a local unmodified Microsoft Detours checkout and Visual Studio x86 tools"]
fn upstream_detours_injects_before_entry_and_survives_parent_exit() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = std::env::var_os("ADV_DETOURS_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/process-reference/Detours"));
    for restore in [false, true] {
        let directory = root.join(format!(
            "target/detours-processes-oracle-{}",
            u8::from(restore)
        ));
        let mut command = Command::new("powershell.exe");
        command
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(root.join("tools/fixtures/build-detours-processes.ps1"))
            .arg("-DetoursDirectory")
            .arg(&source)
            .arg("-OutputDirectory")
            .arg(&directory)
            .arg("-RunNative");
        if restore {
            command.arg("-RestoreAfterWith");
        }
        let output = command.creation_flags(0x0800_0000).output().unwrap();
        assert!(
            output.status.success(),
            "native Detours: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for variant in ["a", "w", "as", "ws"] {
            for name in ["interpreter", "jit", "unicorn"] {
                let data = directory.join(format!("{name}-{variant}-{}", std::process::id()));
                let exe = directory.join(format!("fixture_detours_parent_{variant}.exe"));
                let mut runtime =
                    Runtime::with_cpu(exe, &data, backend::Cpu::new(name).unwrap(), 32, 16)
                        .unwrap();
                runtime.machine.cpu.set_async_compilation(false);
                let trace =
                    std::fs::File::create(directory.join(format!("{name}-{variant}.trace")))
                        .unwrap();
                runtime.machine.sys.trace = adv_machine::Trace::to_writer(Box::new(trace));
                runtime.machine.sys.trace.all = true;
                let started = Instant::now();
                loop {
                    assert!(
                        started.elapsed() < Duration::from_secs(30),
                        "{name}/{variant}/restore={restore}: timeout {:?}",
                        runtime.process_statuses()
                    );
                    match runtime.step_for(Duration::from_millis(4)) {
                        SliceResult::Complete(RunResult::Exited(code)) => {
                            assert_eq!(
                                code,
                                0,
                                "{name}/{variant}/restore={restore}: {}",
                                runtime.report()
                            );
                            break;
                        }
                        SliceResult::Complete(RunResult::Failed(error)) => panic!(
                            "{name}/{variant}/restore={restore}: {error}\n{}",
                            runtime.report()
                        ),
                        SliceResult::Complete(RunResult::Idle { .. }) => {
                            std::thread::sleep(Duration::from_millis(1))
                        }
                        SliceResult::Complete(RunResult::HostRequest) => {
                            panic!("supervisor leaked a request")
                        }
                        SliceResult::Yielded => {}
                    }
                }
                for kind in ["parent", "child"] {
                    let file = format!("detours-{kind}-{variant}.bin");
                    let expected = std::fs::read(directory.join(&file)).unwrap();
                    let actual = std::fs::read(data.join("overlay").join(&file)).unwrap();
                    assert_eq!(
                        actual, expected,
                        "{name}/{variant}/restore={restore}: {file}"
                    );
                }
                assert_eq!(runtime.process_statuses().len(), 2);
                assert!(
                    runtime
                        .process_statuses()
                        .iter()
                        .all(|p| p.exit_code == Some(0) && p.failure.is_none())
                );
                assert!(runtime.api_acceptance_eligible(), "{}", runtime.report());
            }
        }
    }
}
