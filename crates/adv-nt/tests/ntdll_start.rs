//! Wine's ntdll.dll starts the main thread, with the in-process server, and
//! runs until it needs a file.
//!
//! The test needs a Wine 11.0 i386 build: `ADV_WINE_DIST` names the directory
//! that holds `lib/wine/i386-windows`. Without it the test is skipped.
#![cfg(target_vendor = "apple")]

use std::cell::Cell;
use std::path::PathBuf;

use adv_cpu::Cpu;
use adv_machine::{Machine, RunResult, layout::PARK_ADDR};
use adv_nt::server::Progress;
use adv_nt::{dispatch::Platform, params::Startup, process};
use adv_wineserver::{Clock, Config, Server, State};

/// Time stands still: nothing in the test waits.
struct TestClock(Cell<u64>);

impl Clock for TestClock {
    fn monotonic(&self) -> u64 {
        self.0.get()
    }
    fn unix(&self) -> u64 {
        // 2026-01-01 UTC.
        17_672_256_000_000_000 + self.0.get()
    }
}

struct Runtime {
    server: Server,
    clock: TestClock,
}

impl Progress for Runtime {
    fn run_server(&mut self) {
        let state = self.server.step(&self.clock);
        assert!(
            matches!(state, State::Waiting(_)),
            "server stopped: {state:?}"
        );
    }
}

#[test]
fn loader_runs_until_it_opens_a_file() {
    let Some(dist) = std::env::var_os("ADV_WINE_DIST").map(PathBuf::from) else {
        eprintln!("skipped: ADV_WINE_DIST is not set");
        return;
    };
    let ntdll_path = dist.join("lib/wine/i386-windows/ntdll.dll");
    let ntdll = std::fs::read(&ntdll_path)
        .unwrap_or_else(|error| panic!("{}: {error}", ntdll_path.display()));

    let root = std::env::temp_dir().join(format!("adv-nt-ntdll-start-{}", std::process::id()));
    let prefix = root.join("prefix");
    std::fs::create_dir_all(&prefix).unwrap();
    let clock = TestClock(Cell::new(0));
    let config = Config {
        prefix,
        install: dist,
        debug: 0,
    };
    let server = Server::start(&config, &clock).unwrap();
    let socket = server.socket_path().unwrap();
    let runtime = Runtime { server, clock };

    let cpu = Cpu::new(PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, Platform, adv_machine::Clock::virtual_time()).unwrap();
    let startup = Startup {
        image: r"C:\windows\system32\probe.exe",
        command_line: "probe.exe",
        directory: r"C:\windows\system32\",
        environment: &[("SystemDrive", "C:"), ("SystemRoot", r"C:\windows")],
    };
    let started =
        process::start(&mut machine, &ntdll, &startup, &socket, Box::new(runtime)).unwrap();
    assert_ne!(started.thread.pid, 0);

    // The loader creates the process heap and moves the process parameters
    // into it, then opens the current directory: the first file it needs.
    let result = machine.run();
    std::fs::remove_dir_all(&root).unwrap();
    match result {
        RunResult::Failed(error) => assert_eq!(
            error.message, "system call NtOpenFile is not implemented",
            "{error}"
        ),
        other => panic!("the loader did not stop at a system call: {other:?}"),
    }
}
