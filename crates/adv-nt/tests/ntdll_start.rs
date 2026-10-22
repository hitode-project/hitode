//! Wine's ntdll.dll starts the main thread and runs until it needs a file.
//!
//! The test needs a Wine 11.0 i386 build: `ADV_WINE_DIST` names the directory
//! that holds `lib/wine/i386-windows`. Without it the test is skipped.
#![cfg(unix)]

use std::path::PathBuf;

use adv_cpu::Cpu;
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_nt::{dispatch::Platform, params::Startup, process};

fn ntdll() -> Option<Vec<u8>> {
    let Some(dist) = std::env::var_os("ADV_WINE_DIST") else {
        eprintln!("skipped: ADV_WINE_DIST is not set");
        return None;
    };
    let path = PathBuf::from(dist).join("lib/wine/i386-windows/ntdll.dll");
    Some(std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())))
}

#[test]
fn loader_runs_until_it_opens_a_file() {
    let Some(ntdll) = ntdll() else { return };
    let cpu = Cpu::new(PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, Platform, Clock::virtual_time()).unwrap();
    let startup = Startup {
        image: r"C:\windows\system32\probe.exe",
        command_line: "probe.exe",
        directory: r"C:\windows\system32\",
        environment: &[("SystemDrive", "C:"), ("SystemRoot", r"C:\windows")],
    };
    let started = process::start(&mut machine, &ntdll, &startup).unwrap();
    assert_ne!(started.peb, 0);

    // The loader creates the process heap and moves the process parameters
    // into it, then opens the current directory: the first file it needs.
    match machine.run() {
        RunResult::Failed(error) => assert_eq!(
            error.message, "system call NtOpenFile is not implemented",
            "{error}"
        ),
        other => panic!("the loader did not stop at a system call: {other:?}"),
    }
}
