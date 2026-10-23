//! A Wine session: the prefix, the in-process server and the process, and a
//! run loop that moves the server and host services along with the guest.
//!
//! The prefix keeps Wine's state between runs. A new one starts from the
//! installation's prefix template, what `wineboot --init` makes of an empty
//! prefix. Its system32 links to the Wine installation's modules, as a prefix
//! holds them, and drive D: is the game's directory. As Wine runs wineboot
//! whenever its server starts, a session first runs it for the keys that
//! last until the server stops.
use std::cell::{Cell, RefCell};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adv_abi::Registry;
use adv_cpu::Cpu;
use adv_device::surface::SurfaceStore;
use adv_machine::{HostFn, Machine, RunResult, SliceResult, layout::PARK_ADDR};
use adv_nt::server::{Progress, Time};
use adv_nt::{params::Startup, process};
use adv_sync::WakeEvent;
use adv_wineserver::{Clock, Server, State};

/// What a session starts.
pub struct Config<'a> {
    /// The Wine installation: `lib/wine/i386-windows`, `i386-unix` and
    /// `share/wine/nls`.
    pub dist: &'a Path,
    /// The player's prefix, which every game shares as one Windows user's
    /// machine, and the server's runtime files.
    pub prefix: &'a Path,
    /// Drive C:, where the player finds what games write outside their
    /// directory; the prefix's `drive_c` when none.
    pub drive: Option<&'a Path>,
    pub exe: &'a Path,
    pub width: u32,
    pub height: u32,
    pub jit: bool,
    /// Wine's debug channels, as in `WINEDEBUG`.
    pub debug: &'a str,
    /// Where the guest's standard output goes.
    pub stdout: Option<RawFd>,
}

/// Where a run of the session stopped.
#[derive(Debug)]
pub enum Stop {
    Exited(u32),
    /// The time given to the run passed.
    Paused,
    Failed(String),
}

pub struct Session {
    pub machine: Machine<Cpu>,
    wake: WakeEvent,
    audio: adv_device::AudioControl,
}

/// Wine's Unix side, then the graphics fronts and the user driver.
struct Resolver(Registry);

impl adv_machine::Resolver for Resolver {
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        adv_unix::Platform
            .host_fn(module, name)
            .or_else(|| self.0.get(module, name))
    }
    fn provides(&self, module: &str) -> bool {
        adv_unix::Platform.provides(module) || self.0.provides(module)
    }
}

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

struct Driver {
    server: Server,
    clock: GuestClock,
}

/// The server, which the session's processes share.
#[derive(Clone)]
struct Shared(Rc<RefCell<Driver>>);

impl Progress for Shared {
    fn run_server(&mut self, time: Time) {
        let driver = &mut *self.0.borrow_mut();
        driver.clock.0.set(time);
        if let State::Exited(status) = driver.server.step(&driver.clock) {
            log::warn!("wineserver stopped with status {status}");
        }
    }
}

/// Where the prefix template is in a Wine installation; see
/// `tools/wine/prefix-template.sh`.
const TEMPLATE: &str = "share/hitode/prefix";
const MODULES: &str = "lib/wine/i386-windows";
/// How long wineboot may take.
const BOOT: Duration = Duration::from_secs(120);

/// Copy the template's files and directories under `from` to `to`.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let entries = std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let target = to.join(entry.file_name());
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .map_err(|e| format!("{}: {e}", target.display()))?;
        }
    }
    Ok(())
}

/// Link `target` to `source`, replacing what a previous run left there.
fn link(source: &Path, target: &Path) -> Result<(), String> {
    if target.symlink_metadata().is_ok() {
        std::fs::remove_file(target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    std::os::unix::fs::symlink(source, target).map_err(|e| format!("{}: {e}", target.display()))
}

/// Create or refresh the prefix; returns it and drive C:.
fn prefix(config: &Config<'_>) -> Result<(PathBuf, PathBuf), String> {
    let prefix = config.prefix.to_owned();
    let drive = config
        .drive
        .map_or_else(|| prefix.join("drive_c"), Path::to_owned);
    let template = config.dist.join(TEMPLATE);
    if !template.join("system.reg").is_file() {
        return Err(format!(
            "{} has no prefix template; see tools/wine/prefix-template.sh",
            config.dist.display()
        ));
    }
    if !prefix.join("system.reg").exists() {
        for file in ["system.reg", "user.reg", "userdef.reg"] {
            std::fs::create_dir_all(&prefix).map_err(|e| format!("{}: {e}", prefix.display()))?;
            std::fs::copy(template.join(file), prefix.join(file))
                .map_err(|e| format!("{}: {e}", prefix.join(file).display()))?;
        }
    }
    if !drive.join("windows").exists() {
        copy_tree(&template.join("drive_c"), &drive)?;
    }
    let windows = drive.join("windows");
    for directory in [
        windows.join("system32"),
        windows.join("globalization/sorting"),
        prefix.join("dosdevices"),
    ] {
        std::fs::create_dir_all(&directory).map_err(|e| format!("{}: {e}", directory.display()))?;
    }
    match config.drive {
        Some(drive) => link(drive, &prefix.join("dosdevices/c:"))?,
        None => link(Path::new("../drive_c"), &prefix.join("dosdevices/c:"))?,
    }
    let game = config.exe.parent().ok_or("the program has no directory")?;
    link(game, &prefix.join("dosdevices/d:"))?;
    for directory in [MODULES, "share/wine/nls"] {
        let entries = std::fs::read_dir(config.dist.join(directory))
            .map_err(|e| format!("{}: {e}", config.dist.join(directory).display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let target = if name == "sortdefault.nls" {
                windows.join("globalization/sorting").join(name)
            } else {
                windows.join("system32").join(name)
            };
            link(&entry.path(), &target)?;
        }
    }
    // The modules wine.inf installs as copies elsewhere in drive_c.
    let links = template.join("links.tsv");
    let links = std::fs::read_to_string(&links).map_err(|e| format!("{}: {e}", links.display()))?;
    for line in links.lines() {
        let (path, module) = line
            .split_once('\t')
            .ok_or_else(|| format!("links.tsv: {line}"))?;
        let path = path
            .strip_prefix("drive_c/")
            .ok_or_else(|| format!("links.tsv: {line}"))?;
        link(&config.dist.join(MODULES).join(module), &drive.join(path))?;
    }
    Ok((prefix, drive))
}

/// A machine for one of the session's processes, with the graphics fronts
/// and the user driver installed.
fn new_machine(
    config: &Config<'_>,
    prefix: &Path,
    clock: adv_machine::Clock,
    wake: &WakeEvent,
    audio: &adv_device::AudioControl,
) -> Result<Machine<Cpu>, String> {
    let mut registry = Registry::default();
    super::register(&mut registry);
    let cpu = Cpu::new(PARK_ADDR, config.jit).map_err(|e| e.to_string())?;
    let mut machine = Machine::new(cpu, Resolver(registry), clock).map_err(|e| e.to_string())?;
    machine
        .cpu
        .set_waker(wake.clone())
        .map_err(|e| e.to_string())?;
    // A program may compute without waiting for as long as it likes, as on
    // the built-in stack; the backend's default watchdog is for tests.
    machine.set_execution_budget(None);
    adv_unix::install(&mut machine);
    machine.ext.insert(audio.clone());
    super::install(&mut machine, prefix, config.width, config.height)?;
    Ok(machine)
}

/// What every process of the session starts with but its program.
const ENVIRONMENT: &[(&str, &str)] = &[("SystemDrive", "C:"), ("SystemRoot", r"C:\windows")];
/// en-US
const LOCALE: u32 = 0x409;
const WINEBOOT: &str = r"C:\windows\system32\wineboot.exe";

/// Run `machine` until it exits or `until` passes. While every context
/// waits, it serves the host with `serve`: finished GPU work, then the
/// earliest deadline or a wake.
fn run(
    machine: &mut Machine<Cpu>,
    wake: &WakeEvent,
    until: Instant,
    serve: fn(&mut Machine<Cpu>) -> Result<bool, String>,
) -> Stop {
    loop {
        // Before draining host work, so a wake that comes during the
        // slice interrupts the next one.
        wake.take_interrupt();
        let now = Instant::now();
        if now >= until {
            return Stop::Paused;
        }
        let result = match machine.run_slice(Duration::from_millis(10)) {
            SliceResult::Yielded => {
                if let Err(error) = serve(machine) {
                    return Stop::Failed(error);
                }
                continue;
            }
            SliceResult::Complete(result) => result,
        };
        let deadline = match result {
            RunResult::HostRequest => continue,
            RunResult::Exited(code) => return Stop::Exited(code),
            RunResult::Failed(error) => return Stop::Failed(format!("{error:?}")),
            RunResult::Idle { next_deadline } => next_deadline,
        };
        match serve(machine) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => return Stop::Failed(error),
        }
        let mut wait = until.saturating_duration_since(now);
        if let Some(deadline) = deadline {
            let us = deadline.saturating_sub(machine.sys.clock.now_us());
            wait = wait.min(Duration::from_micros(us));
        }
        if let Err(error) = wake.wait(Some(wait)) {
            return Stop::Failed(error.to_string());
        }
    }
}

/// Take finished host work; true when guest work may continue.
fn serve(machine: &mut Machine<Cpu>) -> Result<bool, String> {
    let now = machine.sys.clock.now_us();
    machine
        .ext
        .get::<adv_device::AudioControl>()
        .sync_clock(now);
    #[cfg(feature = "gpu")]
    return adv_dx::d3d9::pump(machine);
    #[cfg(not(feature = "gpu"))]
    {
        let _ = machine;
        Ok(false)
    }
}

impl Session {
    /// Start `config.exe` on real time; `prepare` sets up the machine before
    /// the process starts, after the graphics fronts are installed.
    pub fn start(
        config: &Config<'_>,
        wake: WakeEvent,
        prepare: impl FnOnce(&mut Machine<Cpu>) -> Result<(), String>,
    ) -> Result<Session, String> {
        let exe = std::path::absolute(config.exe).map_err(|e| e.to_string())?;
        let config = Config {
            exe: &exe,
            ..*config
        };
        let name = exe
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("the program name is not Unicode")?;
        let ntdll_path = config.dist.join("lib/wine/i386-windows/ntdll.dll");
        let ntdll =
            std::fs::read(&ntdll_path).map_err(|e| format!("{}: {e}", ntdll_path.display()))?;
        let (prefix, _) = prefix(&config)?;

        let mut clock = adv_machine::Clock::real();
        let time = GuestClock(Cell::new(Time::now(&mut clock)));
        let server = Server::start(
            &adv_wineserver::Config {
                prefix: prefix.clone(),
                server_dir: config.prefix.join("server"),
                install: config.dist.to_owned(),
                debug: 0,
            },
            &time,
        )
        .map_err(|e| e.to_string())?;
        let socket = server.socket_dir().ok_or("the server has no socket")?;
        let shared = Shared(Rc::new(RefCell::new(Driver {
            server,
            clock: time,
        })));

        // As Wine's `run_wineboot`, but restarting: the template stands for
        // what `--init` installs, and no services run.
        let audio = adv_device::AudioControl::new(true);
        let mut boot = new_machine(&config, &prefix, clock.clone(), &wake, &audio)?;
        let command_line = format!("\"{WINEBOOT}\" --restart");
        let startup = Startup {
            image: WINEBOOT,
            command_line: &command_line,
            directory: r"C:\windows\system32\",
            environment: ENVIRONMENT,
            locale: LOCALE,
            debug: config.debug,
            stdin: None,
            stdout: None,
            stderr: None,
            graphics_driver: super::GRAPHICS_DRIVER,
        };
        process::start(
            &mut boot,
            &ntdll,
            &startup,
            &prefix,
            &socket,
            Box::new(shared.clone()),
        )
        .map_err(|e| format!("wineboot: {e}"))?;
        match run(&mut boot, &wake, Instant::now() + BOOT, |_| Ok(false)) {
            Stop::Exited(0) => {}
            stop => return Err(format!("wineboot: {stop:?}")),
        }
        drop(boot);

        let mut machine = new_machine(&config, &prefix, clock, &wake, &audio)?;
        prepare(&mut machine)?;
        let startup = Startup {
            image: &format!(r"D:\{name}"), // privacy: allow the guest's drive for the game
            command_line: name,
            directory: r"D:\", // privacy: allow the guest's drive for the game
            environment: ENVIRONMENT,
            locale: LOCALE,
            debug: config.debug,
            stdin: None,
            stdout: config.stdout,
            stderr: None,
            graphics_driver: super::GRAPHICS_DRIVER,
        };
        process::start(
            &mut machine,
            &ntdll,
            &startup,
            &prefix,
            &socket,
            Box::new(shared),
        )
        .map_err(|e| e.to_string())?;
        Ok(Session {
            machine,
            wake,
            audio,
        })
    }

    /// Run the guest until it exits or `until` passes; see [`run`].
    pub fn run(&mut self, until: Instant) -> Stop {
        run(&mut self.machine, &self.wake, until, serve)
    }

    /// Post host input in coordinates of the last presented frame and wake
    /// the guest through Wine's queue fd, including when every thread waits.
    pub fn input(&mut self, message: u32, wparam: u32, lparam: u32) -> Result<(), String> {
        adv_drv::user::input::post(&mut self.machine.ext, message, wparam, lparam)?;
        self.machine.visit_context(None, adv_nt::server::run)?;
        self.wake.signal();
        Ok(())
    }

    pub fn audio_stream(&self) -> adv_device::AudioStream {
        self.audio.stream()
    }

    /// The latest frame [`Session::capture`] took.
    pub fn frame(&self) -> &crate::Snapshot {
        &self.machine.ext.get::<SurfaceStore>().presenter
    }

    /// Take the latest presented frame into [`Session::frame`], reading a
    /// native one back from the GPU.
    pub fn capture(&mut self) -> Result<(), String> {
        #[cfg(feature = "gpu")]
        if let Some(composition) = self
            .machine
            .ext
            .get::<SurfaceStore>()
            .latest_frame()
            .filter(|frame| frame.native().is_some())
            .cloned()
        {
            return self.capture_native(composition);
        }
        let memory = crate::graphics::guest_memory(&mut self.machine.cpu);
        self.machine
            .ext
            .get_mut::<SurfaceStore>()
            .capture_latest(&memory)
            .map(|_| ())
    }

    #[cfg(feature = "gpu")]
    fn capture_native(
        &mut self,
        composition: adv_device::surface::PendingFrame,
    ) -> Result<(), String> {
        let image = composition.native().unwrap().image.clone();
        let mut readback = adv_dx::d3d9::readback_image(&mut self.machine.ext, &image)
            .map_err(|e| e.to_string())?;
        if let Some(job) = readback.take_completion_job() {
            job();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let bgra = loop {
            if let Some(pixels) = readback.poll() {
                break pixels.map_err(|e| e.to_string())?;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("frame readback timed out".into());
            }
            self.wake
                .wait(Some(remaining.min(Duration::from_millis(1))))
                .map_err(|e| e.to_string())?;
        };
        self.machine
            .ext
            .get_mut::<SurfaceStore>()
            .capture_native_frame_pixels(&composition, &bgra)
            .map(|_| ())
    }

    /// Attach `gpu`, whose context already signals `wake`: our Direct3D 9
    /// renders on it, and its readbacks signal `wake` too. Call from the
    /// `prepare` of [`Session::start`].
    #[cfg(feature = "gpu")]
    pub fn attach_gpu(
        machine: &mut Machine<Cpu>,
        gpu: adv_device::gpu::Gpu,
        wake: WakeEvent,
    ) -> Result<(), String> {
        machine
            .ext
            .get_mut::<SurfaceStore>()
            .set_deferred_presentation(true)?;
        adv_dx::d3d9::set_gpu(&mut machine.ext, gpu)?;
        let background = machine.cpu.background_sender()?;
        adv_dx::d3d9::set_background(&mut machine.ext, background, wake);
        Ok(())
    }
}
