//! Runs a probe from `tests/probe` on Wine's ntdll.dll with the
//! in-process server.
//!
//! The tests need a Wine 11.0 i386 build and the probes built from
//! `tests/probe`: `ADV_WINE_DIST` names the directory that holds
//! `lib/wine/i386-windows` (and `i386-unix` for the Unix libraries) and
//! `share/wine/nls`, and `ADV_WINE_PROBES` the
//! directory that holds the probes. Without them the tests are skipped.
//! The server keeps its state in globals, so each test binary runs one
//! probe.
//!
//! Time is virtual unless a probe needs threads that compute without
//! waiting: then it is real, and the machine runs in slices so that its
//! quantum interleaves them as a host scheduler would.

use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adv_cpu::Cpu;
use adv_machine::{Machine, Resolver, RunResult, SliceResult, layout::PARK_ADDR};
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
    signals: Rc<RefCell<Vec<(i32, i32)>>>,
}

struct Trace<'a> {
    signals: Rc<RefCell<Vec<(i32, i32)>>>,
    cpu: Option<&'a mut dyn adv_cpu::backend::Guest>,
}
impl adv_wineserver::Tracing for Trace<'_> {
    fn signal(&mut self, _: i32, tid: i32, sig: i32) -> bool {
        self.signals.borrow_mut().push((tid, sig));
        true
    }
    fn read_memory(&mut self, _: i32, at: u64, data: &mut [u8]) -> u32 {
        if self
            .cpu
            .as_deref_mut()
            .is_some_and(|cpu| cpu.read(at as u32, data).is_ok())
        {
            0
        } else {
            0xc0000022
        }
    }
    fn write_memory(&mut self, _: i32, at: u64, data: &[u8]) -> u32 {
        if self
            .cpu
            .as_deref_mut()
            .is_some_and(|cpu| cpu.write(at as u32, data).is_ok())
        {
            0
        } else {
            0xc0000022
        }
    }
}
impl Runtime {
    fn step(&mut self, time: Time, cpu: Option<&mut dyn adv_cpu::backend::Guest>) {
        self.clock.0.set(time);
        #[cfg(target_os = "macos")]
        let cwd = std::env::current_dir().unwrap();
        let state = self.server.step_with_tracing(
            &self.clock,
            &mut Trace {
                signals: self.signals.clone(),
                cpu,
            },
        );
        #[cfg(target_os = "macos")]
        assert_eq!(std::env::current_dir().unwrap(), cwd);
        assert!(
            matches!(state, State::Waiting(_)),
            "server stopped: {state:?}"
        );
    }
}
impl Progress for Runtime {
    fn interrupt_pending(&self) -> bool {
        !self.signals.borrow().is_empty()
    }
    fn run_server(&mut self, time: Time) {
        self.step(time, None);
    }
    fn run_server_for(&mut self, time: Time, cpu: &mut dyn adv_cpu::backend::Guest) {
        self.step(time, Some(cpu));
    }
}

/// Runs `name` from the probe directory to its end, on real time when
/// `real_time`; none when the test is skipped.
#[allow(dead_code, reason = "each test binary uses one of the entries")]
pub fn run_probe(name: &str, real_time: bool) -> Option<(RunResult, String)> {
    run_probe_on(name, real_time, Platform, |_| {})
}

/// [`run_probe`] on a machine with `resolver`, which `prepare` sets up
/// before the process starts.
#[allow(dead_code, reason = "each test binary uses one of the entries")]
pub fn run_probe_on(
    name: &str,
    real_time: bool,
    resolver: impl Resolver,
    prepare: impl FnOnce(&mut Machine<Cpu>),
) -> Option<(RunResult, String)> {
    run_probe_with_prefix(name, real_time, resolver, |machine, _| prepare(machine))
}

/// Prepares machine bindings and files in the session's prefix before startup.
#[allow(dead_code, reason = "each test binary uses one of the entries")]
pub fn run_probe_with_prefix(
    name: &str,
    real_time: bool,
    resolver: impl Resolver,
    prepare: impl FnOnce(&mut Machine<Cpu>, &std::path::Path),
) -> Option<(RunResult, String)> {
    run_probe_inspect(name, real_time, "null", resolver, prepare, |_| {})
}

/// The host services a run loop serves when every context waits, as the
/// runtime's step does; true when guest work may continue.
pub struct HostServices(pub fn(&mut Machine<Cpu>) -> bool);

/// [`run_probe_with_prefix`] with Wine's display driver `graphics_driver`,
/// and `inspect` looking at the machine after the process ends.
pub fn run_probe_inspect(
    name: &str,
    real_time: bool,
    graphics_driver: &str,
    resolver: impl Resolver,
    prepare: impl FnOnce(&mut Machine<Cpu>, &std::path::Path),
    inspect: impl FnOnce(&mut Machine<Cpu>),
) -> Option<(RunResult, String)> {
    run_probe_engine(
        name,
        real_time,
        graphics_driver,
        resolver,
        prepare,
        inspect,
        false,
    )
}

#[allow(dead_code, reason = "only native-body fixtures select an engine")]
pub fn run_probe_engine(
    name: &str,
    real_time: bool,
    graphics_driver: &str,
    resolver: impl Resolver,
    prepare: impl FnOnce(&mut Machine<Cpu>, &std::path::Path),
    inspect: impl FnOnce(&mut Machine<Cpu>),
    native: bool,
) -> Option<(RunResult, String)> {
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
    let apisets_path = dist.join("lib/wine/i386-windows/apisetschema.dll");
    let apisets = std::fs::read(&apisets_path)
        .unwrap_or_else(|error| panic!("{}: {error}", apisets_path.display()));

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
    let mut machine_clock = if real_time {
        adv_machine::Clock::real()
    } else {
        adv_machine::Clock::virtual_time()
    };
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
    let signals = Rc::new(RefCell::new(Vec::new()));
    let runtime = Runtime {
        server,
        clock,
        signals: signals.clone(),
    };

    let cpu = Cpu::new(PARK_ADDR, native).unwrap();
    let mut machine = Machine::new(cpu, resolver, machine_clock).unwrap();
    prepare(&mut machine, &prefix);
    let mut fds = [0; 2];
    // SAFETY: fds has room for both ends.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: both descriptors are new and owned by nobody else.
    let (mut output, stdout) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    let startup = Startup {
        image: &format!(r"C:\{name}"),
        command_line: name,
        directory: r"C:\",
        environment: &[("SystemDrive", "C:"), ("SystemRoot", r"C:\windows")],
        // en-US
        locale: 0x409,
        debug: &std::env::var("WINEDEBUG").unwrap_or_default(),
        stdin: None,
        stdout: Some(stdout.as_raw_fd()),
        stderr: None,
        graphics_driver,
        fonts: "",
    };
    let started = process::start(
        &mut machine,
        &ntdll,
        &apisets,
        &startup,
        &prefix,
        &socket,
        Box::new(runtime),
    )
    .unwrap();
    drop(stdout);
    assert_ne!(started.thread.pid, 0);

    // When every context waits, virtual time moves to the earliest deadline
    // and real time sleeps until it.
    let result = loop {
        let pending = std::mem::take(&mut *signals.borrow_mut());
        for (tid, sig) in pending {
            adv_nt::process::signals::deliver(&mut machine, tid, sig).unwrap();
        }
        adv_nt::process::signals::wake(&mut machine).unwrap();
        let result = if real_time {
            match machine.run_slice(Duration::from_millis(10)) {
                SliceResult::Yielded => continue,
                SliceResult::Complete(result) => result,
            }
        } else {
            machine.run()
        };
        adv_nt::process::signals::wake(&mut machine).unwrap();
        if !signals.borrow().is_empty()
            || machine
                .sys
                .contexts
                .iter()
                .any(|context| context.is_ready())
        {
            continue;
        }
        match result {
            RunResult::HostRequest => continue,
            RunResult::Idle {
                next_deadline: Some(deadline),
            } if real_time => std::thread::sleep(Duration::from_micros(
                deadline.saturating_sub(machine.sys.clock.now_us()),
            )),
            RunResult::Idle {
                next_deadline: Some(deadline),
            } => machine.sys.clock.advance_to(deadline),
            RunResult::Idle {
                next_deadline: None,
            } if machine
                .ext
                .try_get::<HostServices>()
                .map(|services| services.0)
                .is_some_and(|serve| serve(&mut machine)) => {}
            other => break other,
        }
    };
    inspect(&mut machine);
    drop(machine);
    // The server keeps its descriptors after the guest exits.
    // SAFETY: output owns the read end of the pipe.
    assert_eq!(
        unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let mut text = String::new();
    match output.read_to_string(&mut text) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(error) => panic!("reading probe output: {error}"),
    }
    std::fs::remove_dir_all(&root).unwrap();
    #[cfg(target_os = "macos")]
    assert_eq!(std::env::current_dir().unwrap(), cwd);
    Some((result, text))
}
