//! Wine's ntdll.dll starts the main thread and reaches its first system call.
//!
//! The test needs a Wine 11.0 i386 build: `ADV_WINE_DIST` names the directory
//! that holds `lib/wine/i386-windows`. Without it the test is skipped.
#![cfg(unix)]

use std::path::PathBuf;

use adv_cpu::Cpu;
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_nt::{dispatch::Platform, process};

fn ntdll() -> Option<Vec<u8>> {
    let Some(dist) = std::env::var_os("ADV_WINE_DIST") else {
        eprintln!("skipped: ADV_WINE_DIST is not set");
        return None;
    };
    let path = PathBuf::from(dist).join("lib/wine/i386-windows/ntdll.dll");
    Some(std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())))
}

#[test]
fn loader_reaches_its_first_system_call() {
    let Some(ntdll) = ntdll() else { return };
    let cpu = Cpu::new(PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, Platform, Clock::virtual_time()).unwrap();
    let started = process::start(&mut machine, &ntdll).unwrap();
    assert_ne!(started.peb, 0);

    // The loader creates the process heap before anything else.
    match machine.run() {
        RunResult::Failed(error) => assert_eq!(
            error.message, "system call NtAllocateVirtualMemory is not implemented",
            "{error}"
        ),
        other => panic!("the loader did not stop at a system call: {other:?}"),
    }
}
