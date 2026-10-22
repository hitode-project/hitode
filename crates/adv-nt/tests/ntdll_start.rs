//! Wine's ntdll.dll starts the main thread, with the in-process server, and
//! starts a Wine program until it reads the registry.
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
fn loader_starts_a_wine_program_until_it_reads_the_registry() {
    let Some(dist) = std::env::var_os("ADV_WINE_DIST").map(PathBuf::from) else {
        eprintln!("skipped: ADV_WINE_DIST is not set");
        return;
    };
    let ntdll_path = dist.join("lib/wine/i386-windows/ntdll.dll");
    let ntdll = std::fs::read(&ntdll_path)
        .unwrap_or_else(|error| panic!("{}: {error}", ntdll_path.display()));

    let root = std::env::temp_dir().join(format!("adv-nt-ntdll-start-{}", std::process::id()));
    let prefix = root.join("prefix");
    std::fs::create_dir_all(prefix.join("drive_c/windows/system32")).unwrap();
    std::fs::create_dir_all(prefix.join("dosdevices")).unwrap();
    std::os::unix::fs::symlink("../drive_c", prefix.join("dosdevices/c:")).unwrap();
    // A real Wine program is the main image.
    std::fs::copy(
        dist.join("lib/wine/i386-windows/hostname.exe"),
        prefix.join("drive_c/windows/system32/hostname.exe"),
    )
    .unwrap();
    let clock = TestClock(Cell::new(0));
    let config = Config {
        prefix: prefix.clone(),
        install: dist,
        debug: 0,
    };
    let server = Server::start(&config, &clock).unwrap();
    let socket = server.socket_path().unwrap();
    let runtime = Runtime { server, clock };

    let cpu = Cpu::new(PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, Platform, adv_machine::Clock::virtual_time()).unwrap();
    let startup = Startup {
        image: r"C:\windows\system32\hostname.exe",
        command_line: "hostname.exe",
        directory: r"C:\windows\system32\",
        environment: &[("SystemDrive", "C:"), ("SystemRoot", r"C:\windows")],
    };
    let started = process::start(
        &mut machine,
        &ntdll,
        &startup,
        &prefix,
        &socket,
        Box::new(runtime),
    )
    .unwrap();
    assert_ne!(started.thread.pid, 0);

    // The loader creates the process heap, moves the process parameters into
    // it, opens the current directory, asks what device it is on and reads
    // the main image's headers. Then it reads its options from the
    // registry.
    let result = machine.run();
    std::fs::remove_dir_all(&root).unwrap();
    match result {
        RunResult::Failed(error) => assert_eq!(
            error.message, "system call NtOpenKey is not implemented",
            "{error}"
        ),
        other => panic!("the loader did not stop: {other:?}"),
    }
}
