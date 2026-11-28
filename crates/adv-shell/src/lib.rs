//! Native presentation and deadline-driven input for a guest PE process.
mod audio;
mod control;
mod debug;
#[cfg(host_view)]
mod execution;
mod frame_pacer;
mod frontend;
#[cfg(target_os = "android")]
pub use adv_device::android_asset;
pub mod gamepad;
mod heap;
mod panic_report;
#[cfg(host_view)]
pub use adv_runtime::system_font_families;
#[cfg(host_view)]
pub use adv_runtime::wine::session::{
    RegistryReport, notify_names_changed as wine_names_changed,
    server_started as wine_server_started,
};
#[cfg(host_view)]
pub use execution::Execution;
pub use heap::CountingAllocator;
#[cfg(host_view)]
mod logging;
pub mod mobile;
#[cfg(target_os = "ios")]
pub use adv_runtime::{
    code_pool_state, guest_address_space, prepare_code_pool_after_attach,
    prepare_code_pool_if_debugged, reserve_guest_window,
};
#[cfg(mobile)]
pub use logging::init_system_log;
#[cfg(any(host_view, feature = "gpu"))]
mod display;

#[cfg(any(host_view, feature = "gpu"))]
pub use display::Upscaler;

#[cfg(not(any(host_view, feature = "gpu")))]
mod scaling;
#[cfg(any(host_view, feature = "gpu"))]
pub use adv_display::plan::Placement as Scaling;
#[cfg(not(any(host_view, feature = "gpu")))]
pub use scaling::Scaling;
pub mod overlay;
mod performance;
mod record;
#[cfg(host_view)]
mod session;
#[cfg(host_view)]
mod text_log;

pub mod window;
#[cfg(host_view)]
mod wine;
#[cfg(host_view)]
pub use session::Session;

/// How a game run ended.
#[derive(Debug)]
pub struct Finished {
    pub exit_code: u8,
    pub summary: adv_report::RunSummary,
    /// What a script wrote to the registry (WINE-SES-11).
    pub registry: Option<adv_runtime::wine::session::RegistryReport>,
}

/// A run failure that still carries whatever anonymous summary was collected
/// before the error stopped execution.
#[derive(Debug)]
pub struct RunError {
    pub message: String,
    pub summary: Box<adv_report::RunSummary>,
}

impl RunError {
    pub fn new(message: impl Into<String>, summary: adv_report::RunSummary) -> Self {
        Self {
            message: message.into(),
            summary: Box::new(summary),
        }
    }

    pub fn from_message(message: impl Into<String>, elapsed: Duration) -> Self {
        let message = message.into();
        let summary = Box::new(error_summary(&message, elapsed));
        Self { message, summary }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<String> for RunError {
    fn from(message: String) -> Self {
        Self::from_message(message, Duration::ZERO)
    }
}

/// A program's console, piped to the host: what it reads, and where its
/// output and errors go.
#[derive(Debug)]
pub struct Console {
    pub input: std::os::fd::OwnedFd,
    pub output: std::os::fd::OwnedFd,
}

/// The host's side of a piped [`Console`]: lines it sends, in the program's
/// code page.
pub struct ConsoleInput {
    writer: std::io::PipeWriter,
    codepage: u32,
}

impl Console {
    /// A console in `codepage`. `on_output` receives everything the program
    /// wrote so far, decoded, its oldest lines dropped past 64 KiB.
    pub fn piped(
        codepage: u32,
        on_output: impl Fn(String) + Send + 'static,
    ) -> std::io::Result<(Self, ConsoleInput)> {
        use std::io::Read;
        const KEPT: usize = 64 * 1024;
        let (mut reader, output) = std::io::pipe()?;
        let (input, writer) = std::io::pipe()?;
        std::thread::Builder::new()
            .name("console".into())
            .spawn(move || {
                let codec = adv_os::nls::Codec::new(codepage);
                let mut text = Vec::new();
                let mut chunk = [0; 4096];
                while let Ok(count @ 1..) = reader.read(&mut chunk) {
                    text.extend_from_slice(&chunk[..count]);
                    if text.len() > KEPT {
                        let cut = text.len() - KEPT;
                        let line = text[cut..]
                            .iter()
                            .position(|&b| b == b'\n')
                            .map_or(cut, |at| cut + at + 1);
                        text.drain(..line);
                    }
                    on_output(String::from_utf16_lossy(&codec.to_utf16(codepage, &text)));
                }
            })?;
        Ok((
            Self {
                input: input.into(),
                output: output.into(),
            },
            ConsoleInput { writer, codepage },
        ))
    }
}

impl ConsoleInput {
    /// Sends `line` and the end of a line, as Enter does.
    pub fn line(&mut self, line: &str) -> std::io::Result<()> {
        use std::io::Write;
        let units: Vec<u16> = line.encode_utf16().chain([13, 10]).collect();
        let bytes = adv_os::nls::Codec::new(self.codepage)
            .from_utf16(self.codepage, &units)
            .0;
        self.writer.write_all(&bytes)
    }
}

use adv_runtime::{CompileStrategy, Engine, EngineOptions, Snapshot};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use adv_runtime::disc::Disc;

/// The ANSI code page `--codepage auto` would pick for this executable.
pub fn detect_codepage(exe: &Path) -> std::io::Result<u32> {
    adv_runtime::codepage::detect_file(exe)
}

/// A program started instead of the game, on the game's drives and prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    /// A DOS path such as `D:\SETUP.EXE`, or a host file. // privacy: allow authored guest path
    pub image: ProgramImage,
    /// The command line after the program's path.
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgramImage {
    Guest(String),
    Host(PathBuf),
}

impl ProgramImage {
    /// A path with a drive letter and a backslash is the guest's; anything
    /// else names a host file.
    pub fn parse(path: &str) -> Self {
        let bytes = path.as_bytes();
        if bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b":\\" {
            Self::Guest(path.to_owned())
        } else {
            Self::Host(PathBuf::from(path))
        }
    }

    fn stem(&self) -> std::ffi::OsString {
        match self {
            Self::Guest(path) => {
                let name = path.rsplit('\\').next().unwrap_or(path);
                name.rsplit_once('.').map_or(name, |(stem, _)| stem).into()
            }
            Self::Host(path) => path.file_stem().unwrap_or_default().to_owned(),
        }
    }
}

pub struct Options {
    pub font_families: std::collections::BTreeMap<String, String>,
    pub font_files: Vec<PathBuf>,
    pub font_smoothing: Option<bool>,
    row_workers: Option<usize>,
    row_batching: bool,
    /// The game; none when only a program such as a disc's setup runs.
    exe: Option<PathBuf>,
    /// Started instead of the game.
    pub program: Option<Program>,
    /// The program's console; none leaves its standard handles empty.
    pub console: Option<Console>,
    data_dir: PathBuf,
    no_code_cache: bool,
    pub cdroms: Vec<(char, PathBuf)>,
    pub install_dir: Option<PathBuf>,
    jit: bool,
    /// The Wine installation to run on.
    wine: Option<PathBuf>,
    /// The player's Wine prefix, which every game shares; next to the data
    /// directory when none.
    pub wine_prefix: Option<PathBuf>,
    /// Where the prefix keeps drive C:, when not inside it.
    pub wine_drive: Option<PathBuf>,
    compile_strategy: CompileStrategy,
    region_linking: bool,
    async_compilation: bool,
    tiered_compilation: bool,
    /// Guest ANSI code page; `None` keeps the one detected from the executable.
    pub codepage: Option<u32>,
    headless: bool,
    hidden: bool,
    gpu_software: bool,
    headless_gpu: bool,
    d3d9_sm3: bool,
    /// Per-game D3D9 shader and native GPU pipeline cache directory.
    pub d3d9_shader_cache: Option<PathBuf>,
    /// Shared GPU adapter health and crash-sentinel directory; defaults to `gpu_cache_dir()`.
    pub gpu_health_dir: Option<PathBuf>,
    width: u32,
    height: u32,
    frames: Option<u64>,
    seconds: Option<f64>,
    report: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    trace: Option<PathBuf>,
    host_profile: Option<PathBuf>,
    schedule_profile: Option<PathBuf>,
    auto_dialog_ms: Option<u32>,
    profile: bool,
    control_stdin: bool,
    /// Record the player's input as a replay schedule; also `ADV_RECORD_INPUT=PATH`.
    record_input: Option<PathBuf>,
    /// Write the session's text records as JSON lines.
    text_log: Option<PathBuf>,
    /// Serve `tools/debug/hitode_debug.py`; also `ADV_DEBUG=1`.
    pub debug: bool,
    frame_budget: bool,
    pub audio: bool,
    codec_kernels: adv_runtime::wine::codec::Mode,
    time_skip: bool,
}

impl Options {
    #[cfg(any(host_view, feature = "gpu"))]
    pub(crate) fn gpu_cache_dir(&self) -> PathBuf {
        self.d3d9_shader_cache
            .clone()
            .unwrap_or_else(|| self.data_dir.join("artifacts/gpu"))
    }

    #[cfg(any(host_view, feature = "gpu"))]
    pub(crate) fn gpu_health_dir(&self) -> PathBuf {
        self.gpu_health_dir
            .clone()
            .unwrap_or_else(|| self.gpu_cache_dir())
    }

    /// Host execution policy, shared by both personalities before guest execution.
    fn configure_machine(
        &self,
        machine: &mut adv_machine::Machine<adv_runtime::Cpu>,
        wake: &adv_sync::WakeEvent,
        keys: &std::sync::Arc<adv_sync::input::KeyState>,
    ) -> Result<(), String> {
        if self.time_skip {
            let skip_keys = keys.clone();
            let pending_input = wake.interrupt_flag();
            machine.set_time_skip_source(Some(std::sync::Arc::new(move |startup_poll| {
                // Pending input/audio notifications must be drained before jumping.
                pending_input.load(std::sync::atomic::Ordering::Acquire)
                    & adv_sync::pending::INTERRUPT
                    == 0
                    && (startup_poll
                        || [0x11, 0xa2, 0xa3]
                            .iter()
                            .any(|key| skip_keys.get(*key) & 0x80 != 0))
            })));
        }
        machine.cpu.configure(&EngineOptions {
            compile_strategy: self.compile_strategy,
            tiered_compilation: self.tiered_compilation,
            region_linking: self.region_linking,
            async_compilation: self.async_compilation,
            row_batching: self.row_batching,
            row_workers: self.row_workers,
            compile_threads: None,
            artifacts: Some(self.data_dir.clone()),
            code_cache: !self.no_code_cache,
            precompile: !self.no_code_cache,
            frame_timing: self.frame_budget || debug::requested(self.debug),
            profiling: self.profile,
        })?;
        if self.frame_budget || self.schedule_profile.is_some() {
            machine.sys.stats.wait_diagnostics =
                Some(adv_machine::wait_diagnostics::WaitDiagnostics::default());
        }
        if self.frame_budget {
            machine.sys.stats.frame_budget = Some(adv_machine::frame_budget::FrameBudget::new(
                machine.sys.stats.total,
                machine.cpu.frame_counters(),
            ));
        }
        machine.sys.stats.services_enabled =
            self.host_profile.is_some() || self.schedule_profile.is_some();
        if let Some(path) = &self.schedule_profile {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            machine.sys.schedule_trace = adv_machine::schedule_trace::ScheduleTrace::to_writer(
                Box::new(std::io::BufWriter::new(
                    std::fs::File::create(path).map_err(|e| e.to_string())?,
                )),
            )
            .map_err(|e| e.to_string())?;
        }
        if let Some(path) = &self.trace {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            machine.sys.trace = adv_machine::Trace::to_writer(Box::new(std::io::BufWriter::new(
                std::fs::File::create(path).map_err(|e| e.to_string())?,
            )));
            machine.sys.trace.all = true;
            // Every call must reach the host to be traced.
            machine
                .sys
                .thunks
                .set_guest_code(&mut machine.cpu, false)
                .map_err(|e| format!("{e:?}"))?;
        }
        Ok(())
    }
    fn write_profiles(
        &self,
        machine: &adv_machine::Machine<adv_runtime::Cpu>,
    ) -> Result<(), String> {
        if self.profile {
            let mut entries: Vec<_> = machine.cpu.profile().into_iter().collect();
            entries.sort_unstable_by_key(|(_, sample)| {
                std::cmp::Reverse(sample.native_ns.saturating_add(sample.interpreter_ns))
            });
            if let Some(report) = &self.report {
                use std::io::Write;
                let path = report.with_extension("cpu.tsv");
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut output = std::io::BufWriter::new(
                    std::fs::File::create(&path).map_err(|e| e.to_string())?,
                );
                writeln!(output, "pc\tnative_entries\tnative_ns\tinterpreter_entries\tinterpreter_instructions\tinterpreter_ns")
                .map_err(|e| e.to_string())?;
                for (pc, sample) in &entries {
                    writeln!(
                        output,
                        "{pc:#010x}\t{}\t{}\t{}\t{}\t{}",
                        sample.native_entries,
                        sample.native_ns,
                        sample.interpreter_entries,
                        sample.interpreter_instructions,
                        sample.interpreter_ns,
                    )
                    .map_err(|e| e.to_string())?;
                }
                output.flush().map_err(|e| e.to_string())?;
                eprintln!(
                    "CPU execution profile: {} (region linking is disabled while profiling)",
                    path.display()
                );
            } else {
                for (pc, sample) in entries.iter().take(20) {
                    eprintln!(
                        "CPU {pc:#010x}: native {} ns / {} entries; interpreter {} ns / {} instructions",
                        sample.native_ns,
                        sample.native_entries,
                        sample.interpreter_ns,
                        sample.interpreter_instructions
                    );
                }
            }
        }
        if let Some(path) = &self.host_profile {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut file =
                std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
            machine
                .sys
                .stats
                .write_host_csv(&machine.cpu, &machine.sys.thunks, &mut file)
                .map_err(|e| e.to_string())?;
            std::io::Write::flush(&mut file).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

const HELP: &str = "adv-run <EXE> [options]\n\
  --exe PATH              guest executable (also accepted positionally)\n\
  --program PATH          start PATH instead, on the EXE's drives: a DOS path on a\n\
                          --cdrom disc, or a host file; the EXE may then be left out\n\
  --arguments TEXT        the program's command line after its path\n\
  --backend jit|interpreter   shared guest backend (default: jit)\n\
  --row-workers N         additional row helpers, 0 runs batches inline (default: up to 3)\n\
  --no-row-batching       disable proven row batches for comparison\n\
  --no-time-skip           disable startup clock-poll and Ctrl time skipping\n\
  --compile fast|speed    automatic JIT region policy (default: fast)\n\
  --no-tiering            keep automatic regions at the selected compile strategy\n\
  --no-region-linking     return to the dispatcher between native regions\n\
  --sync-compilation      compile automatic regions on the execution thread\n\
  --codepage auto|932|936|950\n\
                          guest ANSI encoding: Japanese, GBK or Big5 (default: auto)\n\
  --headless              run without a host window\n\
  --codec-kernels guest|verify|native  codec library acceleration (default: native; Vorbis permits FP rounding differences)\n\
  --audio                 enable PCM audio (headless simulates without a device)\n\
  --hidden                create a hidden host window for frontend tests\n\
  --gpu-software          select a software GPU adapter for diagnostics\n\
  --headless-gpu         opt in to a GPU frontend without a window\n\
  --wine DIST             the Wine userland installed in DIST (required)\n\
  --wine-prefix PATH      the Wine prefix games share (default: beside the data dir)\n\
  --d3d9-sm3             opt in to the SM3 capability milestone (default: off)\n\
  --d3d9-shader-cache PATH opt in to a bounded local WGSL disk cache\n\
  --width N --height N    initial guest canvas (default: 640 x 480)\n\
  --frames N              stop after N presented guest frames\n\
  --seconds N             stop after N seconds of host wall time\n\
  --data-dir PATH         guest write overlay (default: target/adv-data/<exe>)\n\
  --no-code-cache         troubleshoot startup: bypass native cache and image precompilation\n\
  --cdrom D=PATH          attach a read-only disc image to guest drive D (repeatable)\n\
  --install-dir DIR       back guest C:\\Program Files with DIR, written in place\n\
  --report PATH           write a JSON execution report\n\
  --screenshot PATH       write the final frame as PNG\n\
  --trace PATH            write every completed guest API call to a local trace\n\
  --host-profile PATH     opt-in API service time CSV (also supports N=2)\n\
  --schedule-profile PATH opt-in scheduling and lock boundary TSV\n\
  --auto-dialog-ms N      explicitly auto-select dialog defaults after N guest ms\n\
  --profile               collect execution timings; with --report writes .cpu.tsv (disables region links)\n\
  --frame-budget          opt-in bounded per-frame accounting (headless, one worker)\n\
  --control-stdin         explicitly enable line-based diagnostic commands\n\
  --record-input PATH     record the player's input as a play_session.py schedule\n\
  --text-log PATH         write drawn text and produced glyphs as JSON lines\n\
  --debug                 serve the interactive debugger (tools/debug/hitode_debug.py)\n\
  --help                  show this help\n\
All guest code, including DLLs and callbacks, executes inside the guest CPU.";

impl Options {
    /// CLI defaults with a data directory supplied by the host.
    pub fn new(exe: PathBuf, data_dir: PathBuf) -> Self {
        Self::with(Some(exe), data_dir)
    }

    /// Defaults for `program` started on its own, as a disc's setup.
    pub fn program(program: Program, data_dir: PathBuf) -> Self {
        Self {
            program: Some(program),
            ..Self::with(None, data_dir)
        }
    }

    /// Starts `program` alone instead of the game, keeping its settings.
    pub fn run_alone(&mut self, program: Program) {
        self.exe = None;
        self.program = Some(program);
    }

    fn with(exe: Option<PathBuf>, data_dir: PathBuf) -> Self {
        Self {
            font_families: std::collections::BTreeMap::new(),
            font_files: Vec::new(),
            font_smoothing: None,
            row_workers: None,
            row_batching: true,
            exe,
            program: None,
            console: None,
            data_dir,
            no_code_cache: false,
            cdroms: Vec::new(),
            install_dir: None,
            jit: true,
            wine: None,
            wine_prefix: None,
            wine_drive: None,
            compile_strategy: CompileStrategy::Fast,
            region_linking: true,
            async_compilation: true,
            tiered_compilation: true,
            codepage: None,
            headless: false,
            hidden: false,
            gpu_software: false,
            headless_gpu: false,
            d3d9_sm3: false,
            d3d9_shader_cache: None,
            gpu_health_dir: None,
            width: 640,
            height: 480,
            frames: None,
            seconds: None,
            report: None,
            screenshot: None,
            trace: None,
            host_profile: None,
            schedule_profile: None,
            auto_dialog_ms: None,
            profile: false,
            control_stdin: false,
            record_input: None,
            text_log: None,
            debug: false,
            frame_budget: false,
            audio: false,
            codec_kernels: adv_runtime::wine::codec::Mode::Native,
            time_skip: true,
        }
    }

    /// What runs, for titles and reports: the game, else the program.
    pub(crate) fn started(&self) -> String {
        match (
            &self.exe,
            self.program.as_ref().map(|program| &program.image),
        ) {
            (Some(exe), _) => exe.display().to_string(),
            (None, Some(ProgramImage::Host(path))) => path.display().to_string(),
            (None, Some(ProgramImage::Guest(path))) => path.clone(),
            (None, None) => String::new(),
        }
    }

    /// The code page `--codepage auto` picks: the game's, else the program's,
    /// read from its disc when it lies on one.
    fn detect_codepage(&self) -> std::io::Result<u32> {
        if let Some(exe) = &self.exe {
            return detect_codepage(exe);
        }
        match self.program.as_ref().map(|program| &program.image) {
            Some(ProgramImage::Host(path)) => detect_codepage(path),
            Some(ProgramImage::Guest(path)) => {
                let letter = path.as_bytes()[0].to_ascii_uppercase() as char;
                match self.cdroms.iter().find(|(drive, _)| *drive == letter) {
                    Some((_, image)) => Disc::open(image, letter)?.codepage(path),
                    None => Ok(adv_runtime::codepage::detect(path, &[])),
                }
            }
            None => Err(std::io::ErrorKind::InvalidInput.into()),
        }
    }

    fn parse() -> Result<Option<Self>, String> {
        Self::parse_args(std::env::args().skip(1))
    }

    fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut exe = None;
        let mut data_dir = None;
        let mut arguments = None;
        let mut options = Self::with(None, PathBuf::new());
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("{arg} requires a value"));
            match arg.as_str() {
                "--help" | "-h" => {
                    println!("{HELP}");
                    return Ok(None);
                }
                "--exe" => exe = Some(PathBuf::from(value()?)),
                "--program" => {
                    options.program = Some(Program {
                        image: ProgramImage::parse(&value()?),
                        arguments: String::new(),
                    })
                }
                "--arguments" => arguments = Some(value()?),
                "--data-dir" => data_dir = Some(PathBuf::from(value()?)),
                "--no-code-cache" => options.no_code_cache = true,
                "--cdrom" => {
                    let mount = value()?;
                    let (drive, path) = mount.split_once('=').ok_or("--cdrom requires D=PATH")?;
                    let letter = drive.trim_end_matches(':').to_ascii_uppercase();
                    if letter.len() != 1
                        || !letter.as_bytes()[0].is_ascii_uppercase()
                        || path.is_empty()
                    {
                        return Err(
                            "--cdrom requires one drive letter and a disc image path".into()
                        );
                    }
                    let letter = letter.as_bytes()[0] as char;
                    if letter == 'C' || options.cdroms.iter().any(|(d, _)| *d == letter) {
                        return Err("--cdrom drive is reserved or already configured".into());
                    }
                    options.cdroms.push((letter, PathBuf::from(path)));
                }
                "--wine" => options.wine = Some(PathBuf::from(value()?)),
                "--wine-prefix" => options.wine_prefix = Some(PathBuf::from(value()?)),
                "--install-dir" => options.install_dir = Some(PathBuf::from(value()?)),
                "--no-time-skip" => options.time_skip = false,
                "--backend" => {
                    options.jit = match value()?.as_str() {
                        "jit" => true,
                        "interpreter" => false,
                        other => return Err(format!("unknown backend: {other}")),
                    }
                }
                "--codepage" => {
                    options.codepage = match value()?.as_str() {
                        "auto" => None,
                        "932" => Some(932),
                        "936" => Some(936),
                        "950" => Some(950),
                        _ => return Err("--codepage must be auto, 932, 936 or 950".into()),
                    };
                }
                "--row-workers" => {
                    let workers: usize =
                        value()?.parse().map_err(|_| "invalid row worker count")?;
                    if workers > 64 {
                        return Err("--row-workers must be between 0 and 64".into());
                    }
                    options.row_workers = Some(workers);
                }
                "--no-row-batching" => options.row_batching = false,
                "--compile" => {
                    options.compile_strategy = match value()?.as_str() {
                        "fast" => CompileStrategy::Fast,
                        "speed" => CompileStrategy::Speed,
                        _ => return Err("--compile must be fast or speed".into()),
                    };
                }
                "--no-tiering" => options.tiered_compilation = false,
                "--no-region-linking" => options.region_linking = false,
                "--sync-compilation" => options.async_compilation = false,
                "--headless" => options.headless = true,
                "--hidden" => options.hidden = true,
                "--gpu-software" => options.gpu_software = true,
                "--headless-gpu" => options.headless_gpu = true,
                "--d3d9-sm3" => options.d3d9_sm3 = true,
                "--d3d9-shader-cache" => options.d3d9_shader_cache = Some(value()?.into()),
                "--profile" => options.profile = true,
                "--control-stdin" => options.control_stdin = true,
                "--record-input" => options.record_input = Some(value()?.into()),
                "--text-log" => options.text_log = Some(value()?.into()),
                "--debug" => options.debug = true,
                "--frame-budget" => options.frame_budget = true,
                "--audio" => options.audio = true,
                "--width" => options.width = value()?.parse().map_err(|_| "invalid width")?,
                "--codec-kernels" => {
                    options.codec_kernels = match value()?.as_str() {
                        "guest" => adv_runtime::wine::codec::Mode::Guest,
                        "verify" => adv_runtime::wine::codec::Mode::Verify,
                        "native" => adv_runtime::wine::codec::Mode::Native,
                        _ => return Err("--codec-kernels must be guest, verify or native".into()),
                    };
                }
                "--height" => options.height = value()?.parse().map_err(|_| "invalid height")?,
                "--frames" => {
                    options.frames = Some(value()?.parse().map_err(|_| "invalid frame limit")?)
                }
                "--seconds" => {
                    options.seconds = Some(value()?.parse().map_err(|_| "invalid time limit")?)
                }
                "--report" => options.report = Some(value()?.into()),
                "--screenshot" => options.screenshot = Some(value()?.into()),
                "--trace" => options.trace = Some(value()?.into()),
                "--host-profile" => options.host_profile = Some(value()?.into()),
                "--schedule-profile" => options.schedule_profile = Some(value()?.into()),
                "--auto-dialog-ms" => {
                    options.auto_dialog_ms =
                        Some(value()?.parse().map_err(|_| "invalid dialog delay")?)
                }
                _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
                _ if exe.is_none() => exe = Some(arg.into()),
                _ => return Err(format!("unexpected argument: {arg}")),
            }
        }
        if exe.is_none() && options.program.is_none() {
            return Err(format!("an EXE path or --program is required\n{HELP}"));
        }
        options.exe = exe;
        if let Some(arguments) = arguments {
            options
                .program
                .as_mut()
                .ok_or("--arguments requires --program")?
                .arguments = arguments;
        }
        #[cfg(not(any(host_view, feature = "gpu")))]
        if options.headless_gpu || options.d3d9_sm3 || options.d3d9_shader_cache.is_some() {
            return Err("D3D9 shader capability switches require the gpu feature".into());
        }
        if options.headless_gpu && !options.headless {
            return Err("--headless-gpu requires --headless".into());
        }
        options.data_dir = data_dir.unwrap_or_else(|| {
            let stem = match (&options.exe, &options.program) {
                (Some(exe), _) => exe.file_stem().unwrap_or_default().to_owned(),
                (None, Some(program)) => program.image.stem(),
                (None, None) => unreachable!("checked above"),
            };
            PathBuf::from("target/adv-data").join(stem)
        });
        if options.width == 0
            || options.height == 0
            || options.width > 8192
            || options.height > 8192
        {
            return Err("canvas dimensions must be in 1..=8192".into());
        }
        if options.seconds.is_some_and(|n| !n.is_finite() || n <= 0.0) {
            return Err("--seconds must be a positive finite number".into());
        }
        if options
            .seconds
            .is_some_and(|n| Duration::try_from_secs_f64(n).is_err())
        {
            return Err("--seconds is too large to represent as a duration".into());
        }
        if options.frames == Some(0) {
            return Err("--frames must be positive".into());
        }
        Ok(Some(options))
    }
}

/// Alt+Shift+F12 (Option on a Mac) asks the debugger for a mark and
/// snapshot. Ctrl is left alone: visual novels skip text while it is held.
fn debug_hotkey(keys: &adv_sync::input::KeyState, message: u32, key: u32) -> bool {
    let held = |codes: &[u32]| codes.iter().any(|&code| keys.get(code) & 0x80 != 0);
    // With Alt held the key arrives as WM_SYSKEYDOWN.
    matches!(message, 0x100 | 0x104)
        && key == 0x7b
        && held(&[0x12, 0xa4, 0xa5])
        && held(&[0x10, 0xa0, 0xa1])
}

/// What `info` reports about this session; fixed for its lifetime.
fn debug_info(options: &Options, windowed: bool) -> String {
    format!(
        "pid={}\nstack={}\nbuild={}\nplatform={}-{}\nmedia={}\ngpu={}\nwindow={}\nexe={}\ndata_dir={}\nstarted_unix_us={}",
        std::process::id(),
        if options.wine.is_some() {
            "wine"
        } else {
            "builtin"
        },
        env!("ADV_SHELL_BUILD"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(feature = "media"),
        cfg!(any(host_view, feature = "gpu")),
        windowed,
        options.started(),
        options.data_dir.display(),
        debug::unix_us()
    )
}

fn window_json(window: &adv_runtime::WindowView) -> String {
    format!(
        "{{\"hwnd\": {}, \"parent\": {}, \"id\": {}, \"class\": {}, \"text\": {}, \"visible\": {}, \"enabled\": {}}}",
        window.hwnd,
        window.parent,
        window.id,
        json(&window.class),
        json(&window.text),
        window.visible,
        window.enabled
    )
}

fn json(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn save_frame(path: &std::path::Path, frame: &Snapshot) -> Result<(), String> {
    if frame.frames == 0 || frame.width == 0 || frame.height == 0 {
        return Err("guest has not presented a frame".into());
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut rgba = frame.bgra.clone();
    for p in rgba.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
        p[3] = 255;
    }
    let mut encoder = png::Encoder::new(
        std::fs::File::create(path).map_err(|e| e.to_string())?,
        frame.width,
        frame.height,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .map_err(|e| e.to_string())?
        .write_image_data(&rgba)
        .map_err(|e| e.to_string())
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

/// The fault as a report shows it; backend text may name host state.
fn cpu_fault(
    snapshot: &adv_machine::exception::CpuFaultSnapshot,
    caller: Option<u32>,
    mut place: impl FnMut(u32) -> String,
) -> String {
    let pc = snapshot.fault.pc;
    let pc = place(pc);
    let mut text = match snapshot.access() {
        Some((access, at)) => format!("{access} {} at {pc}", place(at)),
        None => format!("{} at {pc}", snapshot.kind_name()),
    };
    if let Some(address) = caller {
        text.push_str(&format!(" return {}", place(address)));
    }
    text
}

#[cfg(host_view)]
pub(crate) fn anonymize_site(
    machine: &adv_machine::Machine<adv_runtime::Cpu>,
    address: u32,
) -> String {
    use adv_runtime::wine::session::GuestExt;
    let main = machine.cpu.read_u32(machine.sys.peb + 8).ok().unwrap_or(0);
    let images = machine.ext.try_get::<adv_runtime::wine::session::Images>();
    match images.and_then(|images| images.containing(address)) {
        Some(module) => {
            let table = module.table();
            let wine = table.as_ref().map(|table| {
                let rva = address - module.base;
                let symbol = table
                    .at(rva)
                    .map(|func| (func.name.as_ref(), rva - func.rva));
                (module.name.as_str(), symbol)
            });
            report_site(
                address,
                module.base,
                module.size,
                main,
                wine,
                module.path.as_deref(),
            )
        }
        None => format!("{address:#010x}"),
    }
}

/// Strips path-like tokens and non-ASCII characters from an internal diagnostic
/// reason so it always passes strict anonymous report validation.
pub(crate) fn sanitize_diag_reason(raw: &str, max_len: usize) -> Option<String> {
    let words: Vec<&str> = raw
        .split_whitespace()
        .filter(|w| !w.contains(['/', '\\']))
        .collect();
    if words.is_empty() {
        return None;
    }
    let cleaned: String = words
        .join(" ")
        .bytes()
        .filter(|b| b.is_ascii_graphic() || *b == b' ')
        .take(max_len)
        .map(char::from)
        .collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(host_view)]
fn report_site(
    address: u32,
    base: u32,
    size: u32,
    main: u32,
    wine: Option<(&str, Option<(&str, u32)>)>,
    path: Option<&Path>,
) -> String {
    if address.wrapping_sub(base) >= size {
        return format!("{address:#010x}");
    }
    let offset = address - base;
    if base == main {
        return format!("exe+0x{offset:x}");
    }
    if let Some((name, symbol)) = wine {
        if let Some((func, func_off)) = symbol {
            return format!("{name}!{func}+0x{func_off:x}");
        }
        return format!("{name}+0x{offset:x}");
    }
    let name = path.and_then(|path| std::fs::read(path).ok()).map_or_else(
        || "game".into(),
        |bytes| format!("game#{}", &adv_report::sha256(&bytes)[..8]),
    );
    format!("{name}+0x{offset:x}")
}

fn empty_failure(kind: adv_report::FailureKind, elapsed: Duration) -> adv_report::Failure {
    adv_report::Failure {
        kind,
        api: None,
        source: None,
        op: None,
        code: None,
        dlls: Vec::new(),
        cpu_fault: None,
        fault_insn: None,
        stack: Vec::new(),
        panic: None,
        after: adv_report::FailureAfter::from_elapsed(elapsed),
    }
}

fn extract_errno(message: &str) -> Option<String> {
    let lower = message.to_ascii_lowercase();
    let (_, tail) = lower.split_once("os error ")?;
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    (!digits.is_empty() && digits.len() <= 8).then(|| format!("errno:{digits}"))
}

/// Classifies known host-side errors into privacy-safe `(source, op, code)` tags.
fn classify_host_error(message: &str) -> Option<(&'static str, &'static str, Option<String>)> {
    let lower = message.to_ascii_lowercase();
    let errno = extract_errno(message);
    if lower.contains("address space") || lower.contains("guest window") || lower.contains("4 gib")
    {
        return Some(("address_space", "window_reservation_failed", errno));
    }
    if lower.contains("not an x86 pe32") || lower.contains("0x8664") {
        return Some(("loader", "pe_not_x86", None));
    }
    if lower.contains("launch_missing_file") {
        return Some(("loader", "missing_exe", errno));
    }
    if lower.contains("launch_folder_access") {
        return Some(("fs", "folder_access_denied", errno));
    }
    if lower.contains("server event watcher") {
        return Some(("wineserver", "event_watcher_io", errno));
    }
    if lower.contains("wineserver") || lower.contains("server:") || lower.contains("no server") {
        return Some(("wineserver", "server_io", errno));
    }
    if lower.contains("vulkan") || lower.contains("vkresult") || lower.contains("vk_") {
        return Some(("vulkan", "device_error", errno));
    }
    if lower.contains("metal") {
        return Some(("metal", "device_error", errno));
    }
    if lower.contains("gpu") || lower.contains("swapchain") || lower.contains("display") {
        return Some(("gpu", "display_error", errno));
    }
    if lower.contains("audio") || lower.contains("sound") || lower.contains("mixer") {
        return Some(("audio", "audio_error", errno));
    }
    if lower.contains("ui stopped while the guest was running") {
        return Some(("ui", "ui_stopped", None));
    }
    if lower.contains("execution budget") || lower.contains("time limit") {
        return Some(("scheduler", "execution_budget", None));
    }
    if lower.contains("no wine distribution") {
        return Some(("loader", "missing_wine_dist", None));
    }
    if errno.is_some() {
        return Some(("host_io", "os_error", errno));
    }
    None
}

fn sanitize_missing_dll_list(raw: &str) -> Vec<String> {
    let name = raw
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if adv_report::is_bare_dll_name(&name) {
        vec![name]
    } else {
        Vec::new()
    }
}

fn clean_api_name(raw: &str) -> Option<String> {
    let trimmed = raw
        .split(" (")
        .next()
        .unwrap_or(raw)
        .split(['"', '\'', ' ', '\t', '\r', '\n', ',', '}'])
        .next()
        .unwrap_or("")
        .trim();
    let normalized = match trimmed.split_once('!') {
        Some((module, export)) => format!("{}!{export}", module.to_ascii_lowercase()),
        None => return None,
    };
    adv_report::api_name(&normalized).then_some(normalized)
}

#[cfg(host_view)]
fn report_machine_failure(
    machine: &adv_machine::Machine<adv_runtime::Cpu>,
    message: Option<&str>,
    exit_code: Option<u32>,
    missing_dll: Option<&str>,
    gaps: &adv_runtime::wine::session::StubGaps,
    elapsed: Duration,
) -> Option<adv_report::Failure> {
    const STATUS_ACCESS_VIOLATION: u32 = 0xc000_0005;
    const STATUS_DLL_NOT_FOUND: u32 = 0xc000_0135;
    use adv_runtime::wine::session::GuestExt;

    let main = machine.cpu.read_u32(machine.sys.peb + 8).ok();
    let images = machine.ext.try_get::<adv_runtime::wine::session::Images>();
    let place = |address: u32| match images.and_then(|images| images.containing(address)) {
        Some(module) => {
            let table = module.table();
            let wine = table.as_ref().map(|table| {
                let rva = address - module.base;
                let symbol = table
                    .at(rva)
                    .map(|func| (func.name.as_ref(), rva - func.rva));
                (module.name.as_str(), symbol)
            });
            report_site(
                address,
                module.base,
                module.size,
                main.unwrap_or(0),
                wine,
                module.path.as_deref(),
            )
        }
        None => format!("{address:#010x}"),
    };
    let current_stack = || -> Vec<String> {
        adv_runtime::wine::session::current_stack_addresses(
            machine,
            adv_report::MAX_STACK_FRAMES.min(12),
        )
        .into_iter()
        .map(&place)
        .collect()
    };

    if let Some((panic, stack)) = crate::panic_report::take_last_panic() {
        let mut failure = empty_failure(adv_report::FailureKind::HostPanic, elapsed);
        failure.panic = Some(panic);
        failure.stack = stack;
        return Some(failure);
    }

    if let Some(dll) = missing_dll {
        let mut failure = empty_failure(adv_report::FailureKind::MissingDll, elapsed);
        failure.code = Some(format!("{STATUS_DLL_NOT_FOUND:#010x}"));
        failure.dlls = sanitize_missing_dll_list(dll);
        return Some(failure);
    }

    let stub = gaps.failure.clone().or_else(|| {
        (exit_code == Some(adv_runtime::wine::session::WINE_STUB))
            .then(|| gaps.last.clone())
            .flatten()
    });
    if let Some(api) = stub.as_deref().and_then(clean_api_name) {
        let mut failure = empty_failure(adv_report::FailureKind::Api, elapsed);
        failure.api = Some(api);
        failure.source = Some("wine_stub".into());
        failure.stack = current_stack();
        return Some(failure);
    }
    if let Some(msg) = message
        && let Some((_, tail)) = msg.split_once("unimplemented: ")
        && let Some(api) = clean_api_name(tail)
    {
        let mut failure = empty_failure(adv_report::FailureKind::Api, elapsed);
        failure.api = Some(api);
        failure.source = Some("abi_stub".into());
        failure.stack = current_stack();
        return Some(failure);
    }

    if let Some(unsupported) = machine
        .ext
        .try_get::<adv_runtime::wine::session::LastUnsupported>()
        && message.is_some()
    {
        let mut failure = empty_failure(adv_report::FailureKind::NtUnsupported, elapsed);
        failure.source = Some(unsupported.subsystem.into());
        failure.op = adv_report::is_safe_token(&unsupported.op, 96).then(|| unsupported.op.clone());
        failure.code = unsupported
            .code
            .as_ref()
            .filter(|code| adv_report::is_safe_token(code, 64))
            .cloned();
        failure.stack = current_stack();
        return Some(failure);
    }

    let terminal = machine.ext.try_get::<adv_runtime::wine::session::Record>();
    let dispatch_failure = machine
        .ext
        .try_get::<adv_runtime::wine::session::DispatchFailure>();
    let snapshot = machine
        .sys
        .latest_cpu_fault
        .as_ref()
        .filter(|snapshot| terminal.is_none_or(|record| record.address == snapshot.fault.pc));

    if let Some(record) = terminal {
        let mut failure = empty_failure(adv_report::FailureKind::Cpu, elapsed);
        failure.code = Some(format!("{:#010x}", record.code));
        failure.op = dispatch_failure.map(|d| d.tag.to_owned());
        let text = if record.code == STATUS_ACCESS_VIOLATION && record.count >= 2 {
            let access = match record.information[0] {
                0 => "read",
                1 => "write",
                8 => "execute",
                _ => "access",
            };
            format!(
                "{access} {} at {}",
                place(record.information[1]),
                place(record.address)
            )
        } else {
            format!(
                "exception {:#010x} at {}",
                record.code,
                place(record.address)
            )
        };
        failure.cpu_fault = Some(text);
        failure.fault_insn = adv_runtime::wine::session::decode_fault_insn(machine, record.address);
        if let Some(checkpoint) = machine
            .ext
            .try_get::<adv_runtime::wine::session::Checkpoint>()
        {
            failure.stack = adv_runtime::wine::session::guest_stack_addresses(
                machine,
                machine.sys.current,
                record.address,
                checkpoint.state.regs[4],
                checkpoint.state.regs[5],
                adv_report::MAX_STACK_FRAMES.min(12),
            )
            .into_iter()
            .map(&place)
            .collect();
        }
        return Some(failure);
    }

    if let Some(snapshot) = snapshot
        && (message.is_some_and(|m| m.contains("unhandled CPU"))
            || exit_code.is_some_and(|code| code >= 0xc000_0000))
    {
        let mut failure = empty_failure(adv_report::FailureKind::Cpu, elapsed);
        failure.code = exit_code.map(|code| format!("{code:#010x}"));
        failure.cpu_fault = Some(cpu_fault(snapshot, None, &place));
        failure.fault_insn =
            adv_runtime::wine::session::decode_fault_insn(machine, snapshot.fault.pc);
        failure.stack = adv_runtime::wine::session::guest_stack_addresses(
            machine,
            snapshot.context,
            snapshot.fault.pc,
            snapshot.stack_address,
            snapshot.registers[2].1,
            adv_report::MAX_STACK_FRAMES.min(12),
        )
        .into_iter()
        .map(&place)
        .collect();
        return Some(failure);
    }

    if let Some(code) = exit_code
        && code >= 0xc000_0000
    {
        if code == STATUS_DLL_NOT_FOUND {
            let mut failure = empty_failure(adv_report::FailureKind::MissingDll, elapsed);
            failure.code = Some(format!("{code:#010x}"));
            return Some(failure);
        }
        let mut failure = empty_failure(adv_report::FailureKind::Cpu, elapsed);
        failure.code = Some(format!("{code:#010x}"));
        return Some(failure);
    }

    report_failure(message, exit_code, None, None, elapsed)
}

#[cfg(host_view)]
fn report_settings(
    options: &Options,
    display: Option<(adv_report::Upscaler, adv_report::Scaling)>,
    fast_forward: Option<adv_report::FastForward>,
) -> adv_report::Settings {
    adv_report::Settings {
        upscaler: display.map(|(upscaler, _)| upscaler),
        scaling: display.map(|(_, scaling)| scaling),
        fast_forward,
        touch_mode: None,
        font_files: options.font_files.len() as u32,
        font_overrides: options.font_families.len() as u32,
        font_smoothing: options.font_smoothing,
        discs: options.cdroms.len() as u32,
    }
}

/// The report summary of a run that ended with a host-side error after
/// `elapsed`, classified like a guest failure; no frame timing is kept.
pub fn error_summary(error: &str, elapsed: Duration) -> adv_report::RunSummary {
    let failure = if let Some((panic, stack)) = crate::panic_report::take_last_panic() {
        let mut failure = empty_failure(adv_report::FailureKind::HostPanic, elapsed);
        failure.panic = Some(panic);
        failure.stack = stack;
        Some(failure)
    } else {
        report_failure(Some(error), None, None, None, elapsed)
    };
    let stop = match failure.as_ref().map(|f| f.kind) {
        Some(adv_report::FailureKind::HostPanic) => adv_report::StopReason::Panic,
        _ => adv_report::StopReason::HostError,
    };
    adv_report::RunSummary {
        run: Some(adv_report::RunInfo {
            jit: true,
            stop,
            exit_code: None,
            processes: 0,
            failed_is_child: false,
            dialogs_shown: 0,
            present_size: None,
        }),
        failure,
        hang: None,
        diagnostics: None,
        perf: None,
        api_gaps: None,
        timeline: None,
        settings: None,
        graphics: None,
    }
}

/// How a run failed, for an anonymous report. `message` is the guest or host
/// failure, `fault` the latest CPU fault as `cpu_fault` formats it.
fn report_failure(
    message: Option<&str>,
    exit_code: Option<u32>,
    fault: Option<String>,
    stub: Option<String>,
    elapsed: Duration,
) -> Option<adv_report::Failure> {
    use adv_report::FailureKind;
    if let Some(api) = stub.as_deref().and_then(clean_api_name) {
        let mut failure = empty_failure(FailureKind::Api, elapsed);
        failure.api = Some(api);
        failure.source = Some("wine_stub".into());
        return Some(failure);
    }
    if let Some(message) = message {
        if let Some(dll) = message.strip_prefix("missing_dll:") {
            let mut failure = empty_failure(FailureKind::MissingDll, elapsed);
            failure.code = Some("0xc0000135".into());
            failure.dlls = sanitize_missing_dll_list(dll);
            return Some(failure);
        }
        if let Some((_, tail)) = message.split_once("unimplemented: ")
            && let Some(api) = clean_api_name(tail)
        {
            let mut failure = empty_failure(FailureKind::Api, elapsed);
            failure.api = Some(api);
            failure.source = Some("abi_stub".into());
            return Some(failure);
        }
        if message.contains("unhandled CPU") {
            let mut failure = empty_failure(FailureKind::Cpu, elapsed);
            failure.cpu_fault = fault;
            return Some(failure);
        }
        if let Some((source, op, code)) = classify_host_error(message) {
            let mut failure = empty_failure(FailureKind::HostError, elapsed);
            failure.source = Some(source.into());
            failure.op = Some(op.into());
            failure.code = code;
            return Some(failure);
        }
        return Some(empty_failure(FailureKind::Runtime, elapsed));
    }
    if let Some(code) = exit_code
        && code >= 0xc000_0000
    {
        if code == 0xc000_0135 {
            let mut failure = empty_failure(FailureKind::MissingDll, elapsed);
            failure.code = Some(format!("{code:#010x}"));
            return Some(failure);
        }
        let mut failure = empty_failure(FailureKind::Cpu, elapsed);
        failure.code = Some(format!("{code:#010x}"));
        failure.cpu_fault = fault;
        return Some(failure);
    }
    None
}

fn write_report(path: &std::path::Path, report: &str, cpu: &impl Engine) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, report).map_err(|e| e.to_string())?;
    cpu.write_report_extras(path)
}

fn run(options: Options) -> Result<u8, String> {
    if options.frame_budget && !options.headless {
        return Err("--frame-budget requires headless execution".into());
    }
    let Some(dist) = options.wine.clone() else {
        return Err(format!("--wine DIST is required\n{HELP}"));
    };
    #[cfg(target_os = "macos")]
    {
        if options.headless {
            return wine::run(&options, &dist);
        }
        session::run_cli(options)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = dist;
        Err("this platform runs games in a host GameView and Session".into())
    }
}

// spec: REPORT-SITE-1
#[cfg(host_view)]
#[test]
fn report_crash_sites_use_anonymous_module_names() {
    let directory = std::env::temp_dir().join(format!("adv-report-site-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("module.dll");
    std::fs::write(&path, b"abc").unwrap();
    assert_eq!(
        report_site(0x401234, 0x400000, 0x2000, 0x400000, None, Some(&path)),
        "exe+0x1234"
    );
    assert_eq!(
        report_site(
            0x601234,
            0x600000,
            0x2000,
            0x400000,
            Some(("ntdll.dll", None)),
            Some(&path)
        ),
        "ntdll.dll+0x1234"
    );
    assert_eq!(
        report_site(
            0x601234,
            0x600000,
            0x2000,
            0x400000,
            Some(("ntdll.dll", Some(("LdrpLoadDll", 0x34)))),
            Some(&path)
        ),
        "ntdll.dll!LdrpLoadDll+0x34"
    );
    assert_eq!(
        report_site(0x601234, 0x600000, 0x2000, 0x400000, None, Some(&path)),
        "game#ba7816bf+0x1234"
    );
    assert_eq!(
        report_site(0x601234, 0x600000, 0x2000, 0x400000, None, None),
        "game+0x1234"
    );
    assert_eq!(
        report_site(0x1234, 0x600000, 0x2000, 0x400000, None, Some(&path)),
        "0x00001234"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

// spec: REPORT-FAIL-1
#[test]
fn report_failures_are_classified_without_host_text() {
    use adv_report::{FailureAfter, FailureKind};
    let minute = Duration::from_secs(90);
    let fault = || Some("read 0x00000000 at 0x00401a2c".to_owned());
    let api = report_failure(
        Some("process 2: unimplemented: d3d9.dll!IDirect3DDevice9::CreateVolumeTexture (strict ABI stub)"),
        None,
        fault(),
        None,
        minute,
    )
    .unwrap();
    assert_eq!(api.kind, FailureKind::Api);
    assert_eq!(
        api.api.as_deref(),
        Some("d3d9.dll!IDirect3DDevice9::CreateVolumeTexture")
    );
    assert_eq!((api.cpu_fault, api.after), (None, FailureAfter::Minutes));
    let dirty_api = report_failure(
        Some(r#"unimplemented: libc.so.6!__res_state", recent: [] }"#),
        None,
        None,
        None,
        minute,
    )
    .unwrap();
    assert_eq!(dirty_api.kind, FailureKind::Api);
    assert_eq!(dirty_api.api.as_deref(), Some("libc.so.6!__res_state"));
    let missing = report_failure(Some("missing_dll:MFC42.DLL"), None, None, None, minute).unwrap();
    assert_eq!(missing.kind, FailureKind::MissingDll);
    assert_eq!(missing.dlls, ["mfc42.dll"]);
    let watcher = report_failure(
        Some("server event watcher: Bad file descriptor (os error 9)"),
        None,
        None,
        None,
        minute,
    )
    .unwrap();
    assert_eq!(watcher.kind, FailureKind::HostError);
    assert_eq!(watcher.source.as_deref(), Some("wineserver"));
    assert_eq!(watcher.op.as_deref(), Some("event_watcher_io"));
    assert_eq!(watcher.code.as_deref(), Some("errno:9"));
    let cpu = report_failure(
        Some("unhandled CPU stop: Fault(..)"),
        None,
        fault(),
        None,
        minute,
    )
    .unwrap();
    assert_eq!(
        (cpu.kind, cpu.cpu_fault, cpu.api),
        (FailureKind::Cpu, fault(), None)
    );
    let exited = report_failure(None, Some(0xc000_0005), fault(), None, minute).unwrap();
    assert_eq!((exited.kind, exited.cpu_fault), (FailureKind::Cpu, fault()));
    let fastfail = report_failure(None, Some(0xc000_0409), None, None, minute).unwrap();
    assert_eq!(fastfail.kind, FailureKind::Cpu);
    assert_eq!(fastfail.code.as_deref(), Some("0xc0000409"));
    // Unclassified messages may name host paths; only the kind is kept.
    let runtime = report_failure(
        Some("open save/data.bin: denied"),
        None,
        fault(),
        None,
        minute,
    )
    .unwrap();
    // spec: REPORT-FAIL-3
    let ended = error_summary("UI stopped while the guest was running", minute);
    assert_eq!(ended.perf, None);
    assert_eq!(
        ended.failure.map(|f| (f.kind, f.source, f.op, f.after)),
        Some((
            FailureKind::HostError,
            Some("ui".into()),
            Some("ui_stopped".into()),
            FailureAfter::Minutes
        ))
    );
    assert_eq!(
        (runtime.kind, runtime.api, runtime.cpu_fault),
        (FailureKind::Runtime, None, None)
    );
    assert_eq!(report_failure(None, Some(0), fault(), None, minute), None);
}

#[test]
fn native_cache_is_default_and_has_one_diagnostic_bypass() {
    let parse = |values: &[&str]| {
        Options::parse_args(values.iter().map(|s| s.to_string()))
            .unwrap()
            .unwrap()
    };
    assert!(!parse(&["fixture.exe"]).no_code_cache);
    assert!(parse(&["fixture.exe", "--no-code-cache"]).no_code_cache);
}

/// Host reasons the game is not playing. System sleep only silences the
/// output; a launcher pause also stops guest execution and guest time.
#[cfg(host_view)]
#[derive(Default)]
struct Suspension {
    sleeping: bool,
    paused: bool,
}

// spec: HOST-SES-7
#[cfg(all(test, host_view))]
#[test]
fn frontend_sleep_and_pause_resume_only_their_own_state() {
    let mut state = Suspension::default();
    let mut audio =
        audio::AudioOutput::from_stream(false, true, adv_device::AudioControl::new(true).stream());
    let mut clocks = Vec::new();
    state.apply(&mut audio, Some(true), None, |paused| clocks.push(paused));
    state.apply(&mut audio, None, Some(true), |paused| clocks.push(paused));
    state.apply(&mut audio, Some(false), None, |paused| clocks.push(paused));
    assert!(state.paused);
    assert_eq!(clocks, [true]);
    state.apply(&mut audio, None, Some(false), |paused| clocks.push(paused));
    assert_eq!(clocks, [true, false]);
    assert!(!state.sleeping && !state.paused);
}
#[cfg(host_view)]
impl Suspension {
    fn apply(
        &mut self,
        audio: &mut audio::AudioOutput,
        sleeping: Option<bool>,
        paused: Option<bool>,
        suspend: impl FnOnce(bool),
    ) {
        let was_paused = self.paused;
        self.sleeping = sleeping.unwrap_or(self.sleeping);
        self.paused = paused.unwrap_or(self.paused);
        // Stop the device callback before pausing guest clocks and PCM;
        // reopen it only after resuming them.
        if self.paused != was_paused {
            if self.paused {
                audio.set_suspended(true);
            }
            suspend(self.paused);
        }
        audio.set_suspended(self.sleeping || self.paused);
    }
}

#[cfg(host_view)]
fn run_execution(
    options: &Options,
    worker: frontend::Worker,
    wake: window::WakeEvent,
    keys: std::sync::Arc<adv_sync::input::KeyState>,
    background: adv_sync::background::Sender,
) -> Result<Finished, RunError> {
    let dist = options
        .wine
        .clone()
        .ok_or_else(|| RunError::from("no Wine distribution to run on".to_owned()))?;
    wine::run_execution(
        &mut adv_runtime::wine::session::Host::default(),
        options,
        &dist,
        worker,
        wake,
        keys,
        background,
    )
}

/// Run the desktop/headless command-line shell with the process arguments.
pub fn run_cli() -> std::process::ExitCode {
    #[cfg(host_view)]
    logging::init();
    if std::env::var("ADV_HOST_FEATURES_REPORT").is_ok_and(|value| value == "1") {
        eprintln!(
            "host_features detected={} enabled={}",
            adv_runtime::HostFeatures::detected(),
            adv_runtime::HostFeatures::get()
        );
    }
    match Options::parse().and_then(|options| options.map_or(Ok(0), run)) {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("adv-run: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "android")]
fn configure_execution_thread() -> String {
    let (result, error, nice, query_error) = unsafe {
        let tid = libc::gettid();
        let result = libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, -2);
        let error = if result == 0 { 0 } else { *libc::__errno() };
        *libc::__errno() = 0;
        let nice = libc::getpriority(libc::PRIO_PROCESS, tid as libc::id_t);
        (result, error, nice, *libc::__errno())
    };
    format!(
        "{{\"requested_nice\": -2, \"actual_nice\": {}, \"applied\": {}, \"set_errno\": {}, \"query_errno\": {}}}",
        if query_error == 0 {
            nice.to_string()
        } else {
            "null".into()
        },
        result == 0 && query_error == 0 && nice == -2,
        error,
        query_error
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn configure_execution_thread() -> String {
    // The guest produces the next interactive frame on this worker.
    let error = unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
    };
    format!(
        "{{\"requested_qos\": \"user_interactive\", \"applied\": {}, \"set_errno\": {error}}}",
        error == 0
    )
}

#[cfg(test)]
mod console_tests {
    use super::Console;
    use std::io::{Read, Write};
    use std::sync::mpsc;
    use std::time::Duration;

    // spec: WINE-SES-11
    #[test]
    fn a_piped_console_speaks_the_programs_code_page() {
        let (shown, seen) = mpsc::channel();
        let (console, mut input) = Console::piped(936, move |text| {
            let _ = shown.send(text);
        })
        .unwrap();
        let mut output = std::fs::File::from(console.output);
        // "名字" in code page 936, split inside a character.
        output.write_all(b"\xc3\xfb").unwrap();
        output.write_all(b"\xd7\xd6: ").unwrap();
        let mut text = String::new();
        while text != "名字: " {
            text = seen.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        input.line("测试").unwrap();
        drop(input);
        let mut read = Vec::new();
        std::fs::File::from(console.input)
            .read_to_end(&mut read)
            .unwrap();
        assert_eq!(read, b"\xb2\xe2\xca\xd4\r\n");
    }
}
