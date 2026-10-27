//! Native presentation and deadline-driven input for a guest PE process.
mod audio;
mod control;
mod debug;
#[cfg(host_view)]
mod execution;
mod frame_pacer;
mod frontend;
#[cfg(host_view)]
pub use execution::Execution;
#[cfg(host_view)]
mod logging;
pub mod mobile;
#[cfg(target_os = "ios")]
pub use adv_runtime::{
    code_pool_state, prepare_code_pool_after_attach, prepare_code_pool_if_debugged,
    reserve_guest_window,
};
#[cfg(mobile)]
pub use logging::init_system_log;
#[cfg(host_view)]
mod native_gpu;
#[cfg(host_view)]
mod upscaling;
#[cfg(host_view)]
pub use upscaling::Upscaler;
mod scaling;
pub use scaling::Scaling;
pub mod overlay;
mod performance;
#[cfg(host_view)]
mod session;
#[cfg(any(host_view, all(test, feature = "gpu")))]
mod software_frame;
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
}

use adv_runtime::{CompileStrategy, Engine, EngineOptions, Snapshot};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The ANSI code page `--codepage auto` would pick for this executable.
pub fn detect_codepage(exe: &Path) -> std::io::Result<u32> {
    let bytes = std::fs::read(exe)?;
    Ok(adv_runtime::codepage::detect(
        &exe.file_name().unwrap_or_default().to_string_lossy(),
        &bytes,
    ))
}

pub struct Options {
    pub font_families: std::collections::BTreeMap<String, String>,
    pub font_files: Vec<PathBuf>,
    pub font_smoothing: Option<bool>,
    row_workers: Option<usize>,
    row_batching: bool,
    exe: PathBuf,
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
    d3d9_shader_cache: Option<PathBuf>,
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
    /// Serve `tools/debug/hitode_debug.py`; also `ADV_DEBUG=1`.
    pub debug: bool,
    frame_budget: bool,
    pub audio: bool,
    codec_kernels: adv_runtime::wine::codec::Mode,
    time_skip: bool,
}

impl Options {
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
            machine.set_time_skip_source(Some(std::sync::Arc::new(move |startup| {
                // Pending input/audio notifications must be drained before jumping.
                // Until the first frame nothing is on screen to pace, so timed waits
                // only delay first entry; afterwards skipping follows the Ctrl keys.
                !pending_input.load(std::sync::atomic::Ordering::Acquire)
                    && (startup
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
            frame_timing: self.frame_budget,
            profiling: self.profile,
        })?;
        if self.frame_budget {
            machine.sys.stats.wait_diagnostics =
                Some(adv_machine::wait_diagnostics::WaitDiagnostics::default());
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
  --backend jit|interpreter   shared guest backend (default: jit)\n\
  --row-workers N         additional row helpers, 0 runs batches inline (default: up to 3)\n\
  --no-row-batching       disable proven row batches for comparison\n\
  --no-time-skip           disable timed-wait acceleration while Ctrl is held\n\
  --compile fast|speed    automatic JIT region policy (default: fast)\n\
  --no-tiering            keep automatic regions at the selected compile strategy\n\
  --no-region-linking     return to the dispatcher between native regions\n\
  --sync-compilation      compile automatic regions on the execution thread\n\
  --codepage auto|932|936 guest ANSI encoding: Japanese or GBK (default: auto)\n\
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
  --cdrom D=PATH          attach a read-only ISO to guest drive D (repeatable)\n\
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
  --debug                 serve the interactive debugger (tools/debug/hitode_debug.py)\n\
  --help                  show this help\n\
All guest code, including DLLs and callbacks, executes inside the guest CPU.";

impl Options {
    /// CLI defaults with a data directory supplied by the host.
    pub fn new(exe: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            font_families: std::collections::BTreeMap::new(),
            font_files: Vec::new(),
            font_smoothing: None,
            row_workers: None,
            row_batching: true,
            exe,
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
            debug: false,
            frame_budget: false,
            audio: false,
            codec_kernels: adv_runtime::wine::codec::Mode::Native,
            time_skip: true,
        }
    }

    fn parse() -> Result<Option<Self>, String> {
        Self::parse_args(std::env::args().skip(1))
    }

    fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut exe = None;
        let mut data_dir = None;
        let mut options = Self::new(PathBuf::new(), PathBuf::new());
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("{arg} requires a value"));
            match arg.as_str() {
                "--help" | "-h" => {
                    println!("{HELP}");
                    return Ok(None);
                }
                "--exe" => exe = Some(PathBuf::from(value()?)),
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
                        return Err("--cdrom requires one drive letter and an ISO path".into());
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
                        _ => return Err("--codepage must be auto, 932 or 936".into()),
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
        options.exe = exe.ok_or_else(|| format!("an EXE path is required\n{HELP}"))?;
        #[cfg(not(any(host_view, feature = "gpu")))]
        if options.headless_gpu || options.d3d9_sm3 || options.d3d9_shader_cache.is_some() {
            return Err("D3D9 shader capability switches require the gpu feature".into());
        }
        if options.headless_gpu && !options.headless {
            return Err("--headless-gpu requires --headless".into());
        }
        options.data_dir = data_dir.unwrap_or_else(|| {
            PathBuf::from("target/adv-data").join(options.exe.file_stem().unwrap_or_default())
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
        options.exe.display(),
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
fn cpu_fault(snapshot: &adv_machine::exception::CpuFaultSnapshot) -> String {
    let pc = snapshot.fault.pc;
    match snapshot.access() {
        Some((access, at)) => format!("{access} {at:#010x} at {pc:#010x}"),
        None => format!("{} at {pc:#010x}", snapshot.kind_name()),
    }
}

/// The report summary of a run that ended with a host-side error after
/// `elapsed`, classified like a guest failure; no frame timing is kept.
pub fn error_summary(error: &str, elapsed: Duration) -> adv_report::RunSummary {
    adv_report::RunSummary {
        failure: report_failure(Some(error), None, None, elapsed),
        perf: None,
        api_gaps: None,
    }
}

/// How a run failed, for an anonymous report. `message` is the guest
/// failure, `fault` the latest CPU fault as `cpu_fault` formats it.
fn report_failure(
    message: Option<&str>,
    exit_code: Option<u32>,
    fault: Option<String>,
    elapsed: Duration,
) -> Option<adv_report::Failure> {
    use adv_report::FailureKind;
    let (kind, api) = match message {
        // Strict ABI stubs fail with "unimplemented: module!name (...)"
        // (adv-abi/src/stub.rs); a child process prefixes its pid.
        Some(message) if message.contains("unimplemented: ") => (
            FailureKind::Api,
            message
                .split("unimplemented: ")
                .nth(1)
                .and_then(|api| api.split(" (").next())
                .map(str::to_owned),
        ),
        Some(message) if message.contains("unhandled CPU") => (FailureKind::Cpu, None),
        Some(_) => (FailureKind::Runtime, None),
        // An unhandled guest exception ends the process with its NTSTATUS.
        None if exit_code.is_some_and(|code| code >= 0xc000_0000) && fault.is_some() => {
            (FailureKind::Cpu, None)
        }
        None => return None,
    };
    Some(adv_report::Failure {
        kind,
        api,
        cpu_fault: fault.filter(|_| kind == FailureKind::Cpu),
        after: adv_report::FailureAfter::from_elapsed(elapsed),
    })
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
        minute,
    )
    .unwrap();
    assert_eq!(api.kind, FailureKind::Api);
    assert_eq!(
        api.api.as_deref(),
        Some("d3d9.dll!IDirect3DDevice9::CreateVolumeTexture")
    );
    assert_eq!((api.cpu_fault, api.after), (None, FailureAfter::Minutes));
    let cpu = report_failure(Some("unhandled CPU stop: Fault(..)"), None, fault(), minute).unwrap();
    assert_eq!(
        (cpu.kind, cpu.cpu_fault, cpu.api),
        (FailureKind::Cpu, fault(), None)
    );
    let exited = report_failure(None, Some(0xc000_0005), fault(), minute).unwrap();
    assert_eq!((exited.kind, exited.cpu_fault), (FailureKind::Cpu, fault()));
    // Other messages may name host paths; only the kind is kept.
    let runtime =
        report_failure(Some("open save/data.bin: denied"), None, fault(), minute).unwrap();
    // spec: REPORT-FAIL-3
    let ended = error_summary("UI stopped while the guest was running", minute);
    assert_eq!(ended.perf, None);
    assert_eq!(
        ended.failure.map(|f| (f.kind, f.after)),
        Some((FailureKind::Runtime, FailureAfter::Minutes))
    );
    assert_eq!(
        (runtime.kind, runtime.api, runtime.cpu_fault),
        (FailureKind::Runtime, None, None)
    );
    assert_eq!(report_failure(None, Some(0), fault(), minute), None);
    assert_eq!(report_failure(None, Some(0xc000_0005), None, minute), None);
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
    options: Options,
    worker: frontend::Worker,
    wake: window::WakeEvent,
    keys: std::sync::Arc<adv_sync::input::KeyState>,
    background: adv_sync::background::Sender,
) -> Result<Finished, String> {
    let dist = options
        .wine
        .clone()
        .ok_or("no Wine distribution to run on")?;
    wine::run_execution(
        &mut adv_runtime::wine::session::Host::default(),
        &options,
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

#[cfg(test)]
mod deadline_tests;

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
