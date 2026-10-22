//! Wine's ntdll.dll starts a program, the gate-1 probe, with the in-process
//! server.
//!
//! The test needs a Wine 11.0 i386 build and the probe built from
//! `tests/probe/gate1.c`: `ADV_WINE_DIST` names the directory that holds
//! `lib/wine/i386-windows` and `share/wine/nls`, and `ADV_WINE_PROBE` the
//! probe. Without them the test is skipped.
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

/// Wine's debug output, on standard error.
struct WineLog;

impl log::Log for WineLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == "wine"
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{}", record.args());
        }
    }
    fn flush(&self) {}
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
fn loader_starts_the_probe() {
    let _ = log::set_logger(&WineLog).map(|()| log::set_max_level(log::LevelFilter::Warn));
    let Some(dist) = std::env::var_os("ADV_WINE_DIST").map(PathBuf::from) else {
        eprintln!("skipped: ADV_WINE_DIST is not set");
        return;
    };
    let Some(probe) = std::env::var_os("ADV_WINE_PROBE").map(PathBuf::from) else {
        eprintln!("skipped: ADV_WINE_PROBE is not set");
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
    // system32 links to Wine's modules and locale data, as a prefix holds
    // them.
    let windows = prefix.join("drive_c/windows");
    std::fs::create_dir_all(windows.join("globalization/sorting")).unwrap();
    for directory in ["lib/wine/i386-windows", "share/wine/nls"] {
        for entry in std::fs::read_dir(dist.join(directory)).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let target = if name == "sortdefault.nls" {
                windows.join("globalization/sorting").join(name)
            } else {
                windows.join("system32").join(name)
            };
            std::os::unix::fs::symlink(entry.path(), target).unwrap();
        }
    }
    std::fs::copy(&probe, prefix.join("drive_c/gate1.exe")).unwrap();
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
        image: r"C:\gate1.exe",
        command_line: "gate1.exe",
        directory: r"C:\",
        environment: &[("SystemDrive", "C:"), ("SystemRoot", r"C:\windows")],
        // en-US
        locale: 0x409,
        debug: &std::env::var("WINEDEBUG").unwrap_or_default(),
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

    // The probe starts through the loader, which loads its imports and a
    // library and runs its TLS callback, then sleeps.
    let result = machine.run();
    std::fs::remove_dir_all(&root).unwrap();
    match result {
        RunResult::Failed(error) => assert_eq!(
            error.message, "system call NtDelayExecution is not implemented",
            "{error}"
        ),
        other => panic!("the probe did not stop: {other:?}"),
    }
}
