//! A Wine session: the prefix, the in-process server and the process, and a
//! run loop that moves the server and host services along with the guest.
//!
//! The prefix keeps Wine's state between runs. A new one starts from the
//! installation's prefix template, what `wineboot --init` makes of an empty
//! prefix. Its system32 links to the Wine installation's modules, as a prefix
//! holds them. The program directory uses D: or the next letter free of optical
//! mounts. A Host boots the prefix server once and keeps its volatile state
//! between games. Game fonts are installed after wineboot has finished.
mod debug;
mod files;
mod fonts;
mod host;
pub use host::Host;

use adv_drv::user::project;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adv_abi::Registry;
use adv_cpu::{Cpu, backend::Guest};
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
    /// The LCID of the system and user locales, which set the ANSI code page.
    pub locale: u32,
    /// Host font directories as `;`-separated NT Unix paths. [`HOST_FONTS`]
    /// selects the portable backend's full system discovery; empty isolates
    /// the session to fonts installed in its prefix and Wine distribution.
    pub fonts: &'a str,
    pub exe: &'a Path,
    pub width: u32,
    pub height: u32,
    pub jit: bool,
    /// Wine's debug channels, as in `WINEDEBUG`.
    pub debug: &'a str,
    /// Where the guest's standard output goes.
    pub stdout: Option<RawFd>,
}

/// User settings applied before process startup, independent of the shell.
#[derive(Default)]
pub struct SessionOptions {
    pub fonts: crate::FontOverrides,
    pub font_files: Vec<PathBuf>,
    pub cdroms: Vec<(char, PathBuf)>,
    pub install_dir: Option<PathBuf>,
}

/// Where a run of the session stopped.
#[derive(Debug)]
pub enum Stop {
    Exited(u32),
    /// The time given to the run passed.
    Paused,
    Failed(String),
}

/// Host observations of the session's execution and idle waits.
#[derive(Default)]
pub struct ControlMetrics {
    pub idle_waits: u64,
    pub wait_time: Duration,
    pub run_time: Duration,
}

pub struct Session {
    pub machine: Machine<Cpu>,
    // Retires clients after the active machine releases its connection.
    _lease: host::Lease,
    wake: WakeEvent,
    audio: adv_device::AudioControl,
    metrics: ControlMetrics,
    shared: Shared,
    settings: Settings,
    fonts: fonts::Fonts,
    finished: Option<u32>,
    idle: BTreeMap<i32, Option<u64>>,
    foreground: i32,
    delivered: u64,
    retired_debug: [u64; 3],
    retired_boundaries: adv_nt::counters::Boundaries,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(error) = self.fonts.restore(&mut self.machine) {
            eprintln!("{error}");
        }
    }
}

struct Settings {
    dist: PathBuf,
    prefix: PathBuf,
    drive: PathBuf,
    locale: u32,
    fonts: String,
    debug: String,
    width: u32,
    height: u32,
    jit: bool,
    ntdll: Vec<u8>,
    apisets: Vec<u8>,
}

/// Wine's Unix side, then the graphics fronts and the user driver.
struct Resolver(Registry);

impl adv_machine::Resolver for Resolver {
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        adv_unix::Platform
            .host_fn(module, name)
            .or_else(|| adv_drv::codec::host_fn(module, name))
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
    parked: BTreeMap<i32, Machine<Cpu>>,
    signals: Vec<(i32, i32, i32)>,
    generation: u64,
    deadline: Option<Instant>,
    guest_deadline: Option<u64>,
    input: u64,
    mounts: adv_nt::file::volume::Mounts,
    #[cfg(test)]
    resumes: usize,
}

/// The server, which the session's processes share.
#[derive(Clone)]
struct Shared {
    driver: Rc<RefCell<Driver>>,
    id: Rc<Cell<i32>>,
}

struct Trace<'a, 'b> {
    active: i32,
    cpu: Option<&'a mut dyn Guest>,
    parked: &'b mut BTreeMap<i32, Machine<Cpu>>,
    signals: &'b mut Vec<(i32, i32, i32)>,
    mounts: &'b adv_nt::file::volume::Mounts,
}
impl adv_wineserver::Tracing for Trace<'_, '_> {
    fn read_file(
        &mut self,
        fd: i32,
        offset: u64,
        bytes: &mut [u8],
    ) -> Option<std::io::Result<usize>> {
        self.mounts
            .reader(fd)
            .map(|reader| reader.and_then(|reader| reader.read_at(offset, bytes)))
    }
    fn signal(&mut self, pid: i32, tid: i32, sig: i32) -> bool {
        self.signals.push((pid, tid, sig));
        true
    }
    fn read_memory(&mut self, pid: i32, address: u64, bytes: &mut [u8]) -> u32 {
        let cpu = if pid == self.active {
            self.cpu.as_deref_mut()
        } else {
            self.parked
                .get_mut(&pid)
                .map(|m| &mut m.cpu as &mut dyn Guest)
        };
        let Some(cpu) = cpu else { return 0xc000_000b };
        if address > u32::MAX as u64 || bytes.len() > 64 * 1024 * 1024 {
            return 0xc000_000d;
        }
        if cpu.read(address as u32, bytes).is_ok() {
            0
        } else {
            0x8000_000d
        }
    }
    fn write_memory(&mut self, pid: i32, address: u64, bytes: &[u8]) -> u32 {
        let cpu = if pid == self.active {
            self.cpu.as_deref_mut()
        } else {
            self.parked
                .get_mut(&pid)
                .map(|m| &mut m.cpu as &mut dyn Guest)
        };
        let Some(cpu) = cpu else { return 0xc000_000b };
        if address > u32::MAX as u64 || bytes.len() > 64 * 1024 * 1024 {
            return 0xc000_000d;
        }
        if !cpu.accessible(
            address as u32,
            bytes.len() as u32,
            adv_cpu::backend::Prot::WRITE,
        ) {
            return 0x8000_000d;
        }
        if cpu.write(address as u32, bytes).is_ok() {
            0
        } else {
            0x8000_000d
        }
    }
}
impl Shared {
    fn step(&mut self, time: Time, cpu: Option<&mut dyn Guest>) {
        let mut driver = self.driver.borrow_mut();
        // spec: SRV-EVENT-2
        if matches!(driver.server.state(), State::Waiting(delay) if delay != Some(Duration::ZERO))
            && driver.input == driver.server.input_generation()
            && driver
                .deadline
                .is_none_or(|deadline| deadline > Instant::now())
            && driver
                .guest_deadline
                .is_none_or(|deadline| time.monotonic < deadline)
            && !driver.server.kernel_ready()
        {
            return;
        }
        #[cfg(test)]
        {
            driver.resumes += 1;
        }
        let Driver {
            server,
            clock,
            parked,
            signals,
            generation,
            deadline,
            guest_deadline,
            mounts,
            input,
            ..
        } = &mut *driver;
        clock.0.set(time);
        let state = server.step_with_tracing(
            clock,
            &mut Trace {
                active: self.id.get(),
                cpu,
                parked,
                signals,
                mounts,
            },
        );
        *input = server.input_generation();
        *generation += 1;
        *guest_deadline = match state {
            State::Waiting(Some(delay)) => {
                Some(time.monotonic.saturating_add(delay.as_nanos() as u64 / 100))
            }
            _ => None,
        };
        *deadline = match state {
            State::Waiting(Some(delay)) => Some(Instant::now() + delay),
            _ => None,
        };
    }
    fn process(&self) -> Self {
        Self {
            driver: self.driver.clone(),
            id: Rc::new(Cell::new(0)),
        }
    }
}
impl Progress for Shared {
    fn interrupt_pending(&self) -> bool {
        !self.driver.borrow().signals.is_empty()
    }
    fn run_server(&mut self, time: Time) {
        self.step(time, None);
    }
    fn run_server_for(&mut self, time: Time, cpu: &mut dyn Guest) {
        self.step(time, Some(cpu));
    }
}

/// Where the prefix template is in a Wine installation; see
/// `tools/wine/prefix-template.sh`.
const TEMPLATE: &str = "share/hitode/prefix";
const MODULES: &str = "lib/wine/i386-windows";
const NTDLL: &str = "lib/wine/i386-windows/ntdll.dll";
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

/// Link `target` to `source`, retaining an existing matching link.
fn link(source: &Path, target: &Path) -> Result<(), String> {
    if std::fs::read_link(target).is_ok_and(|existing| existing == source) {
        return Ok(());
    }
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
    drive: &Path,
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
    machine.sys.poll_wake = Some(wake.clone());
    adv_unix::install(&mut machine);
    Session::install_codec_kernels(&mut machine, adv_drv::codec::Mode::Native);
    machine.ext.insert(audio.clone());
    adv_drv::media::set_wake(&mut machine.ext, wake.clone());
    super::install(&mut machine, drive, config.width, config.height)?;
    // Sessions replace this with their configured fonts; booting needs none
    // and must not wait for system discovery.
    super::fonts::install(
        &mut machine,
        adv_device::text::Text::with_font_directories(&[]),
        config.locale,
    );
    Ok(machine)
}

/// What every process of the session starts with but its program.
const ENVIRONMENT: &[(&str, &str)] = &[("SystemDrive", "C:"), ("SystemRoot", r"C:\windows")];

/// The locale whose ANSI code page is `codepage`: ja-JP for 932, zh-CN for
/// 936, en-US otherwise.
pub fn locale(codepage: u32) -> u32 {
    match codepage {
        932 => 0x411,
        936 => 0x804,
        _ => 0x409,
    }
}
/// Default host roots, selecting the portable backend's system discovery.
#[cfg(target_os = "macos")]
pub const HOST_FONTS: &str = r"\??\unix\System\Library\Fonts"; // privacy: allow the system font directory
#[cfg(target_os = "ios")]
pub const HOST_FONTS: &str = r"\??\unix\System\Library\Fonts\Core;\??\unix\System\Library\Fonts\CoreAddition;\??\unix\System\Library\Fonts\LanguageSupport"; // privacy: allow system font directories
#[cfg(target_os = "android")]
pub const HOST_FONTS: &str = r"\??\unix\system\fonts"; // privacy: allow the system font directory
const WINEBOOT: &str = r"C:\windows\system32\wineboot.exe";

/// Run `machine` until it exits or `until` passes. While every context
/// waits, it serves the host with `serve`: finished GPU work, then the
/// earliest deadline or a wake.
fn run(
    machine: &mut Machine<Cpu>,
    wake: &WakeEvent,
    until: Instant,
    serve: fn(&mut Machine<Cpu>) -> Result<bool, String>,
    frame: Option<u64>,
    mut metrics: Option<&mut ControlMetrics>,
) -> Stop {
    // Whether a frame was presented since `frame`, when stopping at one.
    let presented = |machine: &Machine<Cpu>| {
        frame.is_some_and(|serial| machine.ext.get::<SurfaceStore>().stats.presentations != serial)
    };
    loop {
        // Before draining host work, so a wake that comes during the
        // slice interrupts the next one.
        wake.take_interrupt();
        let now = Instant::now();
        if now >= until || presented(machine) {
            return Stop::Paused;
        }
        let step = Instant::now();
        let result = machine.run_slice(Duration::from_millis(10));
        if let Some(metrics) = metrics.as_deref_mut() {
            metrics.run_time += step.elapsed();
        }
        let result = match result {
            SliceResult::Yielded => {
                if presented(machine) {
                    return Stop::Paused;
                }
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
            RunResult::Idle { next_deadline } => {
                if presented(machine) {
                    return Stop::Paused;
                }
                next_deadline
            }
        };
        match serve(machine) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => return Stop::Failed(error),
        }
        let deadline = with_notices(machine, deadline);
        if presented(machine) {
            return Stop::Paused;
        }
        let mut wait = until.saturating_duration_since(now);
        if let Some(deadline) = deadline {
            let us = deadline.saturating_sub(machine.sys.clock.now_us());
            wait = wait.min(Duration::from_micros(us));
        }
        if let Some(deadline) = machine.ext.get::<Shared>().driver.borrow().deadline {
            wait = wait.min(deadline.saturating_duration_since(Instant::now()));
        }
        let started = Instant::now();
        let result = wake.wait(Some(wait));
        if let Some(metrics) = metrics.as_deref_mut() {
            metrics.idle_waits += 1;
            metrics.wait_time += started.elapsed();
        }
        if let Err(error) = result {
            return Stop::Failed(error.to_string());
        }
        let _ = machine.visit_context(None, adv_nt::server::run);
    }
}

/// When DirectSound next has position notifications to deliver.
struct SoundDeadline(Option<u64>);
struct VideoDeadline(Option<u64>);

/// The earlier of a guest deadline and the next notification.
fn with_notices(machine: &Machine<Cpu>, deadline: Option<u64>) -> Option<u64> {
    let notices = machine.ext.try_get::<SoundDeadline>().and_then(|d| d.0);
    let video = machine.ext.try_get::<VideoDeadline>().and_then(|d| d.0);
    let deadline = match (deadline, video) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    match (deadline, notices) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Take finished host work; true when guest work may continue.
fn serve(machine: &mut Machine<Cpu>) -> Result<bool, String> {
    let now = machine.sys.clock.now_us();
    machine
        .ext
        .get::<adv_device::AudioControl>()
        .sync_clock(now);
    let notices = adv_dx::dsound::pump(machine)?;
    let winmm = super::winmm::pump(machine)?;
    let xaudio = adv_dx::xaudio2::pump(machine)?;
    let notices = notices.into_iter().chain(winmm).chain(xaudio).min();
    machine.ext.insert(SoundDeadline(notices));
    let video = adv_dx::dshow::pump_video(machine)?;
    machine.ext.insert(VideoDeadline(video));
    let media = adv_drv::media::pump(machine)?;
    let ready = machine
        .sys
        .contexts
        .iter()
        .any(|context| context.is_ready());
    #[cfg(feature = "gpu")]
    return adv_dx::d3d9::pump(machine).map(|gpu| gpu || media || ready);
    #[cfg(not(feature = "gpu"))]
    Ok(media || ready)
}

impl Session {
    /// Select the shell's codec mode in `prepare`, before images are mapped.
    pub fn install_codec_kernels(machine: &mut Machine<Cpu>, mode: adv_drv::codec::Mode) {
        adv_drv::codec::install(&mut machine.ext, mode);
    }

    /// Configure the native graphics front before guest code runs.
    #[cfg(feature = "gpu")]
    pub fn configure_graphics(machine: &mut Machine<Cpu>, sm3: bool, cache: Option<PathBuf>) {
        adv_dx::d3d9::set_shader_caps(
            &mut machine.ext,
            adv_dx::d3d9::ShaderCaps {
                sm3,
                ..Default::default()
            },
        );
        adv_dx::d3d9::set_shader_cache(&mut machine.ext, cache);
    }

    /// Start `config.exe` on real time; `prepare` sets up the machine before
    /// the process starts, after the graphics fronts are installed.
    pub fn start(
        config: &Config<'_>,
        wake: WakeEvent,
        prepare: impl FnOnce(&mut Machine<Cpu>) -> Result<(), String>,
    ) -> Result<Session, String> {
        Self::start_with_options(config, wake, &SessionOptions::default(), prepare)
    }

    /// Apply host settings before Wine initializes its devices and file namespace.
    pub fn start_with_options(
        config: &Config<'_>,
        wake: WakeEvent,
        options: &SessionOptions,
        prepare: impl FnOnce(&mut Machine<Cpu>) -> Result<(), String>,
    ) -> Result<Session, String> {
        Host::default().start(config, wake, options, prepare)
    }

    fn start_on(
        host: &mut Host,
        config: &Config<'_>,
        wake: WakeEvent,
        options: &SessionOptions,
        prepare: impl FnOnce(&mut Machine<Cpu>) -> Result<(), String>,
    ) -> Result<Session, String> {
        host.check(config)?;
        let exe = std::path::absolute(config.exe).map_err(|e| e.to_string())?;
        let config = Config {
            exe: &exe,
            ..*config
        };
        let name = exe
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("the program name is not Unicode")?;
        let module = |name: &str| {
            let path = config.dist.join(MODULES).join(name);
            std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
        };
        let (ntdll, apisets) = (module("ntdll.dll")?, module("apisetschema.dll")?);
        let (prefix, drive) = host.prefix(&config)?;
        let (mounts, game_drive) = files::prepare(&config, options, &prefix)?;
        let process_config = Config {
            fonts: "",
            ..config
        };

        let mut clock = adv_machine::Clock::real();
        let shared = host.connect(&config, &mut clock, &wake, mounts.clone())?;
        let lease = host::Lease(shared.clone());
        let socket = shared
            .driver
            .borrow()
            .server
            .socket_dir()
            .ok_or("the server has no socket")?;

        // As Wine's `run_wineboot`, but restarting: the template stands for
        // what `--init` installs, and no services run.
        let audio = adv_device::AudioControl::new(true);
        if !host.booted() {
            let mut boot = new_machine(&process_config, &drive, clock.clone(), &wake, &audio)?;
            boot.ext.insert(mounts.clone());
            let command_line = format!("\"{WINEBOOT}\" --restart");
            let startup = Startup {
                image: WINEBOOT,
                command_line: &command_line,
                directory: r"C:\windows\system32\",
                environment: ENVIRONMENT,
                locale: config.locale,
                debug: config.debug,
                stdin: None,
                stdout: None,
                stderr: None,
                graphics_driver: super::GRAPHICS_DRIVER,
                fonts: process_config.fonts,
            };
            process::start(
                &mut boot,
                &ntdll,
                &apisets,
                &startup,
                &prefix,
                &socket,
                Box::new(shared.clone()),
            )
            .map_err(|e| format!("wineboot: {e}"))?;
            boot.ext.insert(shared.clone());
            adv_nt::identity::note_ntdll(&mut boot, &config.dist.join(NTDLL), &ntdll);
            match run(
                &mut boot,
                &wake,
                Instant::now() + BOOT,
                |_| Ok(false),
                None,
                None,
            ) {
                Stop::Exited(0) => {}
                stop => return Err(format!("wineboot: {stop:?}")),
            }
            drop(boot);
            host.finish_boot();
        }

        let mut fonts = host.fonts.prepare(&config, &drive, options)?;
        let config = process_config;
        let mut machine = new_machine(&config, &drive, clock, &wake, &audio)?;
        machine.ext.insert(mounts);
        prepare(&mut machine)?;
        let startup = Startup {
            image: &format!(r"{game_drive}:\{name}"),
            command_line: name,
            directory: &format!(r"{game_drive}:\"),
            environment: ENVIRONMENT,
            locale: config.locale,
            debug: config.debug,
            stdin: None,
            stdout: config.stdout,
            stderr: None,
            graphics_driver: super::GRAPHICS_DRIVER,
            fonts: config.fonts,
        };
        adv_drv::codec::vorbisfile::prepare(
            &mut machine.ext,
            &prefix.join("dosdevices"),
            startup.image,
        );
        let started = process::start(
            &mut machine,
            &ntdll,
            &apisets,
            &startup,
            &prefix,
            &socket,
            Box::new(shared.clone()),
        )
        .map_err(|e| e.to_string())?;
        let configured = fonts
            .install(&mut machine)
            .and_then(|_| fonts.apply(&mut machine))
            .and_then(|_| super::xaudio2::activate(&mut machine, started.main));
        if let Err(error) = configured {
            fonts.restore(&mut machine)?;
            return Err(error);
        }
        adv_nt::identity::note_ntdll(&mut machine, &config.dist.join(NTDLL), &ntdll);
        shared.id.set(started.thread.unix_pid);
        machine.ext.insert(process::Children::default());
        machine.ext.insert(shared.clone());
        Ok(Session {
            machine,
            _lease: lease,
            wake,
            audio,
            metrics: ControlMetrics::default(),
            fonts,
            foreground: shared.id.get(),
            delivered: 0,
            retired_debug: [0; 3],
            retired_boundaries: Default::default(),
            shared,
            finished: None,
            idle: BTreeMap::new(),
            settings: Settings {
                dist: config.dist.to_owned(),
                prefix: prefix.clone(),
                drive: drive.clone(),
                locale: config.locale,
                fonts: config.fonts.to_owned(),
                debug: config.debug.to_owned(),
                width: config.width,
                height: config.height,
                jit: config.jit,
                ntdll,
                apisets,
            },
        })
    }

    /// Run the family on its single execution owner.
    pub fn run(&mut self, until: Instant) -> Stop {
        self.run_family(until, false)
    }
    pub fn run_ui(&mut self, until: Instant) -> Stop {
        self.run_family(until, true)
    }

    /// Stop the host audio callback before suspending the family's clocks and PCM.
    pub fn set_suspended(&mut self, suspended: bool) -> bool {
        let set = |machine: &mut Machine<Cpu>| {
            if suspended {
                machine.sys.clock.pause();
            } else {
                machine.sys.clock.resume();
            }
        };
        set(&mut self.machine);
        for machine in self.shared.driver.borrow_mut().parked.values_mut() {
            set(machine);
        }
        self.audio.suspend(suspended)
    }

    fn run_family(&mut self, until: Instant, stop_frame: bool) -> Stop {
        let result = self.run_family_inner(until, stop_frame);
        if self.shared.id.get() != self.foreground
            && self
                .shared
                .driver
                .borrow()
                .parked
                .contains_key(&self.foreground)
            && let Err(error) = self.select(self.foreground)
        {
            return Stop::Failed(error);
        }
        result
    }

    fn run_family_inner(&mut self, until: Instant, stop_frame: bool) -> Stop {
        if let Some(code) = self.finished {
            return Stop::Exited(code);
        }
        let frame = self.presented_frames();
        loop {
            self.wake.take_interrupt();
            if Instant::now() >= until {
                return Stop::Paused;
            }
            if let Err(error) = self.spawn_children().and_then(|_| self.deliver()) {
                return Stop::Failed(error);
            }
            let before = self.presented_frames();
            let started = Instant::now();
            let result =
                if let Some(code) = self.machine.sys.process_identity.completion.exit_code() {
                    SliceResult::Complete(RunResult::Exited(code))
                } else {
                    self.machine.run_slice(Duration::from_millis(10))
                };
            self.metrics.run_time += started.elapsed();
            if let Err(error) = serve(&mut self.machine) {
                return Stop::Failed(error);
            }
            let id = self.shared.id.get();
            // A single-process game is already foreground: skip the window view.
            if self.presented_frames() != before
                && self.foreground != id
                && project::view(&self.machine.ext).top != 0
                && let Err(error) = self.set_foreground(id)
            {
                return Stop::Failed(error);
            }
            match result {
                SliceResult::Complete(RunResult::Exited(code)) => {
                    self.machine.ext.get::<process::Children>().exited();
                    if !self.audio.remove_owner(self.machine.sys.pid) {
                        return Stop::Failed("audio command limit".into());
                    }
                    let shown = self.foreground == id && project::view(&self.machine.ext).top != 0;
                    let visible = self
                        .shared
                        .driver
                        .borrow()
                        .parked
                        .values()
                        .any(|m| project::view(&m.ext).windows.iter().any(|w| w.visible));
                    if shown && !visible {
                        for machine in self.shared.driver.borrow().parked.values() {
                            let counts = debug::counters(machine);
                            for (total, count) in self.retired_debug.iter_mut().zip(counts) {
                                *total += count;
                            }
                            self.retired_boundaries
                                .merge(&adv_nt::counters::Boundaries::of(&machine.ext));
                        }
                        self.shared.driver.borrow_mut().parked.clear();
                        self.finished = Some(code);
                        project::stopped(&mut self.machine.ext);
                        return Stop::Exited(code);
                    }
                    let next = self.shared.driver.borrow().parked.keys().next().copied();
                    if let Some(next) = next {
                        if let Err(error) = self.select(next) {
                            return Stop::Failed(error);
                        }
                        if self.foreground == id {
                            self.foreground = next;
                        }
                        let retired = self.shared.driver.borrow_mut().parked.remove(&id).unwrap();
                        for (total, count) in
                            self.retired_debug.iter_mut().zip(debug::counters(&retired))
                        {
                            *total += count;
                        }
                        self.retired_boundaries
                            .merge(&adv_nt::counters::Boundaries::of(&retired.ext));
                        self.idle.remove(&id);
                    } else {
                        self.finished = Some(code);
                        project::stopped(&mut self.machine.ext);
                        return Stop::Exited(code);
                    }
                }
                SliceResult::Complete(RunResult::Failed(error)) => {
                    return Stop::Failed(format!("{error:?}"));
                }
                SliceResult::Complete(RunResult::Idle { next_deadline }) => {
                    let deadline = with_notices(&self.machine, next_deadline);
                    self.idle.insert(id, deadline);
                }
                _ => {
                    self.idle.remove(&id);
                }
            }
            if stop_frame && self.presented_frames() != frame {
                return Stop::Paused;
            }
            if let Err(error) = self.spawn_children() {
                return Stop::Failed(error);
            }
            // Server pipes wake waiting clients; parked clients do no guest work.
            let ids: Vec<_> = self.shared.driver.borrow().parked.keys().copied().collect();
            for pid in &ids {
                let mut machine = self.shared.driver.borrow_mut().parked.remove(pid).unwrap();
                if machine
                    .visit_context(None, adv_nt::server::deliver)
                    .unwrap_or(false)
                {
                    self.idle.remove(pid);
                }
                self.shared.driver.borrow_mut().parked.insert(*pid, machine);
            }
            let now = self.machine.sys.clock.now_us();
            let ready = ids.into_iter().find(|pid| {
                !self.idle.contains_key(pid)
                    || self.idle[pid].is_some_and(|deadline| deadline <= now)
            });
            if let Some(pid) = ready {
                if let Err(error) = self.select(pid) {
                    return Stop::Failed(error);
                }
                continue;
            }
            if !self.idle.contains_key(&self.shared.id.get()) {
                continue;
            }
            // Let server timers fire even when every client is waiting.
            let mut wait = until.saturating_duration_since(Instant::now());
            for deadline in self.idle.values().flatten() {
                wait = wait.min(Duration::from_micros(deadline.saturating_sub(now)));
            }
            if let Some(deadline) = self.shared.driver.borrow().deadline {
                wait = wait.min(deadline.saturating_duration_since(Instant::now()));
            }
            self.machine
                .sys
                .clock
                .begin_wait(Some(now + wait.as_micros() as u64));
            let started = Instant::now();
            let signaled = match self.wake.wait(Some(wait)) {
                Ok(signaled) => signaled,
                Err(error) => return Stop::Failed(error.to_string()),
            };
            let waited = started.elapsed();
            self.metrics.idle_waits += 1;
            self.metrics.wait_time += waited;
            let sys = &mut self.machine.sys;
            if let Some(diagnostics) = &mut sys.stats.wait_diagnostics {
                diagnostics.host_wait(
                    Some(wait),
                    waited,
                    !signaled,
                    sys.contexts.iter().any(|context| context.is_ready()),
                );
            }
            if let Some(budget) = &mut sys.stats.frame_budget {
                budget.wait(Some(wait), waited, !signaled);
            }
            let _ = self.machine.visit_context(None, adv_nt::server::run);
            if let Err(error) = self.deliver() {
                return Stop::Failed(error);
            }
            self.idle.clear();
        }
    }

    fn deliver(&mut self) -> Result<(), String> {
        let generation = self.shared.driver.borrow().generation;
        if generation == self.delivered {
            return Ok(());
        }
        self.delivered = generation;
        let signals = std::mem::take(&mut self.shared.driver.borrow_mut().signals);
        for (pid, tid, sig) in signals {
            if pid == self.shared.id.get() {
                process::signals::deliver(&mut self.machine, tid, sig)?;
                self.idle.remove(&pid);
            } else {
                let target = self.shared.driver.borrow_mut().parked.remove(&pid);
                if let Some(mut machine) = target {
                    process::signals::deliver(&mut machine, tid, sig)?;
                    self.shared.driver.borrow_mut().parked.insert(pid, machine);
                    self.idle.remove(&pid);
                }
            }
        }
        if process::signals::wake(&mut self.machine)? {
            self.idle.remove(&self.shared.id.get());
        }
        let ids: Vec<_> = self.shared.driver.borrow().parked.keys().copied().collect();
        for pid in ids {
            let mut machine = self.shared.driver.borrow_mut().parked.remove(&pid).unwrap();
            if process::signals::wake(&mut machine)? {
                self.idle.remove(&pid);
            }
            self.shared.driver.borrow_mut().parked.insert(pid, machine);
        }
        Ok(())
    }

    fn set_foreground(&mut self, id: i32) -> Result<(), String> {
        if self.foreground == id {
            return Ok(());
        }
        let current = self.shared.id.get();
        self.select(self.foreground)?;
        let mut target = self
            .shared
            .driver
            .borrow_mut()
            .parked
            .remove(&id)
            .ok_or("foreground process disappeared")?;
        let memory = crate::graphics::guest_memory(&mut self.machine.cpu);
        self.machine
            .ext
            .get_mut::<SurfaceStore>()
            .transfer_frame_mailbox_to(target.ext.get_mut::<SurfaceStore>(), &memory)?;
        self.shared.driver.borrow_mut().parked.insert(id, target);
        self.foreground = id;
        self.select(current)
    }

    fn select(&mut self, id: i32) -> Result<(), String> {
        let mut next = self
            .shared
            .driver
            .borrow_mut()
            .parked
            .remove(&id)
            .ok_or("unknown process")?;
        next.sys.clock = self.machine.sys.clock.clone();
        std::mem::swap(&mut self.machine, &mut next);
        self.shared
            .driver
            .borrow_mut()
            .parked
            .insert(self.shared.id.get(), next);
        self.shared.id = self.machine.ext.get::<Shared>().id.clone();
        Ok(())
    }

    fn spawn_children(&mut self) -> Result<(), String> {
        let pending = std::mem::take(&mut self.machine.ext.get_mut::<process::Children>().pending);
        for child in pending {
            // spec: PROC-CLOCK-2
            // This family stays in real time even after its children exit.
            self.machine.set_time_skip_source(None);
            for machine in self.shared.driver.borrow_mut().parked.values_mut() {
                machine.set_time_skip_source(None);
            }
            let shared = self.shared.process();
            let c = &self.settings;
            let config = Config {
                dist: &c.dist,
                prefix: &c.prefix,
                drive: None,
                locale: c.locale,
                fonts: &c.fonts,
                exe: Path::new(&child.image),
                width: c.width,
                height: c.height,
                jit: c.jit,
                debug: &c.debug,
                stdout: None,
            };
            let mut machine = new_machine(
                &config,
                &c.drive,
                self.machine.sys.clock.clone(),
                &self.wake,
                &self.audio,
            )?;
            self.fonts.configure(&mut machine)?;
            adv_drv::codec::install(&mut machine.ext, adv_drv::codec::mode(&self.machine.ext));
            machine.inherit_policy(&self.machine);
            machine.sys.frame_pacer = self.machine.sys.frame_pacer.clone();
            if let Some(vsync) = self
                .machine
                .ext
                .try_get::<std::sync::Arc<adv_sync::vsync::Vsync>>()
            {
                machine.ext.insert(vsync.clone());
            }
            machine
                .ext
                .insert(self.shared.driver.borrow().mounts.clone());
            machine
                .cpu
                .set_background_sender(self.machine.cpu.background_sender()?)
                .map_err(|e| e.to_string())?;
            #[cfg(feature = "gpu")]
            adv_dx::d3d9::fork_host_state(&self.machine.ext, &mut machine.ext)?;
            // Until foreground handoff, a child queues frames for the session's
            // consumer; Snapshot cannot present native GPU frames directly.
            machine
                .ext
                .get_mut::<SurfaceStore>()
                .set_deferred_presentation(true)?;
            let mut progress = shared.clone();
            let (connection, registered) = adv_nt::server::Connection::from_socket(
                child.socket,
                &mut progress,
                Time::now(&mut machine.sys.clock),
            )
            .map_err(|e| e.to_string())?;
            shared.id.set(registered.unix_pid);
            let startup = Startup {
                image: &child.image,
                command_line: "",
                directory: "",
                environment: &[],
                locale: c.locale,
                debug: &c.debug,
                stdin: None,
                stdout: None,
                stderr: None,
                graphics_driver: super::GRAPHICS_DRIVER,
                fonts: &c.fonts,
            };
            adv_drv::codec::vorbisfile::prepare(
                &mut machine.ext,
                &c.prefix.join("dosdevices"),
                startup.image,
            );
            process::start_connected(
                &mut machine,
                &c.ntdll,
                &c.apisets,
                &startup,
                &c.prefix,
                Box::new(shared.clone()),
                (connection, registered),
            )
            .map_err(|e| e.to_string())?;
            adv_nt::identity::note_ntdll(&mut machine, &c.dist.join(NTDLL), &c.ntdll);
            machine
                .ext
                .insert(self.machine.ext.get::<process::Children>().fork());
            machine.ext.insert(shared.clone());
            self.shared
                .driver
                .borrow_mut()
                .parked
                .insert(shared.id.get(), machine);
        }
        Ok(())
    }

    pub fn control_metrics(&self) -> &ControlMetrics {
        &self.metrics
    }

    /// Read the presented process's guest address space.
    pub fn inspect_memory(&self, address: u32, len: u32) -> Result<Vec<u8>, String> {
        let mut bytes = vec![0; len as usize];
        self.machine
            .cpu
            .read(address, &mut bytes)
            .map_err(|error| error.to_string())?;
        Ok(bytes)
    }

    /// Reset the wait and frame statistics supported by this stack.
    pub fn reset_metrics(&mut self) {
        self.metrics = ControlMetrics::default();
        if let Some(diagnostics) = &mut self.machine.sys.stats.wait_diagnostics {
            diagnostics.reset();
        }
        let total = self.machine.sys.stats.total;
        if let Some(budget) = &mut self.machine.sys.stats.frame_budget {
            use crate::Engine;
            budget.reset(total, self.machine.cpu.frame_counters());
        }
    }

    /// Post host input in coordinates of the last presented frame and wake
    /// the guest through Wine's queue fd, including when every thread waits.
    /// A press, or the host window's restore, brings back a window the
    /// guest minimized, as its taskbar button would.
    pub fn input(&mut self, message: u32, wparam: u32, lparam: u32) -> Result<(), String> {
        let restoring = matches!((message, wparam), (5, 0) | (0x201 | 0x204, _));
        if !(restoring && project::restore(&mut self.machine.ext)) {
            adv_drv::user::input::post(&mut self.machine.ext, message, wparam, lparam)?;
        }
        self.poke()
    }

    /// Wake the guest for what the driver queued.
    fn poke(&mut self) -> Result<(), String> {
        self.machine.visit_context(None, adv_nt::server::run)?;
        self.wake.signal();
        Ok(())
    }

    fn queued(&mut self, queued: bool) -> bool {
        queued && self.poke().is_ok()
    }

    /// Committed IME text for the presented window; returns the characters
    /// queued.
    pub fn text(&mut self, text: &str) -> usize {
        let typed = adv_drv::user::input::text(&mut self.machine.ext, text);
        if self.poke().is_ok() { typed } else { 0 }
    }

    /// Release what the host holds down, as when its window loses focus.
    pub fn release_input(&mut self) -> usize {
        let released = adv_drv::user::input::release(&mut self.machine.ext);
        if self.poke().is_ok() { released } else { 0 }
    }

    /// Ask the presented window to close.
    pub fn close(&mut self) -> bool {
        let queued = project::close(&mut self.machine.ext);
        self.queued(queued)
    }

    /// The host application gained or lost activation.
    pub fn activate(&mut self, active: bool) -> bool {
        let queued = project::activate(&mut self.machine.ext, active);
        self.queued(queued)
    }

    /// Read the guest's windows again for the native views below.
    pub fn refresh_ui(&mut self) {
        let queued = project::refresh(&mut self.machine.ext);
        self.queued(queued);
    }

    pub fn dialogs(&self) -> crate::DialogSet {
        project::dialogs(&self.machine.ext)
    }

    pub fn dialog_event(&mut self, event: crate::DialogEvent) -> bool {
        let queued = project::dialog_event(&mut self.machine.ext, event);
        self.queued(queued)
    }

    pub fn menu_bar(&self) -> crate::MenuBar {
        project::menu_bar(&self.machine.ext)
    }

    pub fn menu_event(&mut self, event: crate::MenuEvent) -> bool {
        let queued = project::menu_event(&mut self.machine.ext, event);
        self.queued(queued)
    }

    pub fn input_view(&self) -> crate::InputView {
        project::view(&self.machine.ext)
    }

    pub fn window_at(&self, x: i32, y: i32) -> Option<crate::WindowView> {
        project::window_at(&self.machine.ext, x, y)
    }

    /// Whether the guest minimized the presented window.
    pub fn minimized(&self) -> bool {
        project::minimized(&self.machine.ext)
    }

    /// Frames presented so far.
    pub fn presented_frames(&self) -> u64 {
        self.machine.ext.get::<SurfaceStore>().stats.presentations
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

#[cfg(test)]
#[path = "../../tests/wine/server_quiet.rs"]
mod server_quiet;
