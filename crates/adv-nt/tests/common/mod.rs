//! Runs a probe from `tests/probe` on Wine's ntdll.dll with the
//! in-process server.
//!
//! The tests need a Wine 11.0 i386 build and the probes built from
//! `tests/probe`: `ADV_WINE_DIST` names the directory that holds
//! `lib/wine/i386-windows` and `share/wine/nls`, and `ADV_WINE_PROBES` the
//! directory that holds the probes. Without them the tests are skipped.
//! The server keeps its state in globals, so each test binary runs one
//! probe.

use std::cell::Cell;
use std::path::PathBuf;

use adv_cpu::Cpu;
use adv_machine::{Machine, RunResult, layout::PARK_ADDR};
use adv_nt::server::{Progress, Time};
use adv_nt::{dispatch::Platform, params::Startup, process};
use adv_wineserver::{Clock, Config, Server, State};

/// The guest's clocks, as the client last showed them to the server.
struct GuestClock(Cell<Time>);

impl Clock for GuestClock {
    fn monotonic(&self) -> u64 {
        self.0.get().monotonic
    }
    fn unix(&self) -> u64 {
        self.0.get().unix
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
    clock: GuestClock,
}

impl Progress for Runtime {
    fn run_server(&mut self, time: Time) {
        self.clock.0.set(time);
        #[cfg(target_os = "macos")]
        let cwd = std::env::current_dir().unwrap();
        let state = self.server.step(&self.clock);
        #[cfg(target_os = "macos")]
        assert_eq!(std::env::current_dir().unwrap(), cwd);
        assert!(
            matches!(state, State::Waiting(_)),
            "server stopped: {state:?}"
        );
    }
}

/// Runs `name` from the probe directory to its end; none when the test
/// is skipped.
pub fn run_probe(name: &str) -> Option<RunResult> {
    let _ = log::set_logger(&WineLog).map(|()| log::set_max_level(log::LevelFilter::Warn));
    let Some(dist) = std::env::var_os("ADV_WINE_DIST").map(PathBuf::from) else {
        eprintln!("skipped: ADV_WINE_DIST is not set");
        return None;
    };
    let Some(probes) = std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from) else {
        eprintln!("skipped: ADV_WINE_PROBES is not set");
        return None;
    };
    let ntdll_path = dist.join("lib/wine/i386-windows/ntdll.dll");
    let ntdll = std::fs::read(&ntdll_path)
        .unwrap_or_else(|error| panic!("{}: {error}", ntdll_path.display()));

    #[cfg(target_os = "macos")]
    let cwd = std::env::current_dir().unwrap();
    let root = std::env::temp_dir().join(format!("adv-nt-{name}-{}", std::process::id()));
    #[cfg(target_os = "ios")]
    let root = root.join(
        "container-path-for-prefix-and-server-runtime-files-longer-than-the-unix-socket-address-capacity-of-104-bytes",
    );
    #[cfg(target_os = "ios")]
    assert!(root.as_os_str().len() > 104);
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
    std::fs::copy(probes.join(name), prefix.join("drive_c").join(name)).unwrap();
    let mut machine_clock = adv_machine::Clock::virtual_time();
    let clock = GuestClock(Cell::new(Time::now(&mut machine_clock)));
    let config = Config {
        prefix: prefix.clone(),
        server_dir: root.join("server"),
        install: dist,
        debug: 0,
    };
    let server = Server::start(&config, &clock).unwrap();
    #[cfg(target_os = "macos")]
    assert_eq!(std::env::current_dir().unwrap(), cwd);
    let socket = server.socket_dir().unwrap();
    let runtime = Runtime { server, clock };

    let cpu = Cpu::new(PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, Platform, machine_clock).unwrap();
    let startup = Startup {
        image: &format!(r"C:\{name}"),
        command_line: name,
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

    // Time is virtual: when every context waits, it moves to the earliest
    // deadline.
    let result = loop {
        match machine.run() {
            RunResult::Idle {
                next_deadline: Some(deadline),
            } => machine.sys.clock.advance_to(deadline),
            other => break other,
        }
    };
    std::fs::remove_dir_all(&root).unwrap();
    #[cfg(target_os = "macos")]
    assert_eq!(std::env::current_dir().unwrap(), cwd);
    Some(result)
}
