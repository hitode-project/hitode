//! Native presentation and deadline-driven input for a guest PE process.
mod audio;
mod control;
mod frontend;
mod performance;
mod window;

use adv_machine::{RunResult, SliceResult};
use adv_runtime::{CompileStrategy, Snapshot, boot::Runtime};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SOFT_SLICE: Duration = Duration::from_millis(8);

struct Options {
    exe: PathBuf,
    data_dir: PathBuf,
    jit: bool,
    compile_strategy: CompileStrategy,
    region_linking: bool,
    async_compilation: bool,
    codepage: u32,
    headless: bool,
    hidden: bool,
    width: u32,
    height: u32,
    frames: Option<u64>,
    seconds: Option<f64>,
    report: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    trace: Option<PathBuf>,
    auto_dialog_ms: Option<u32>,
    self_test_input: bool,
    profile: bool,
    control_stdin: bool,
    audio: bool,
    codec: adv_runtime::vorbis::Policy,
    api_mode: adv_runtime::RunMode,
}

const HELP: &str = "adv-run <EXE> [options]\n\
  --exe PATH              guest executable (also accepted positionally)\n\
  --backend jit|interpreter   shared guest backend (default: jit)\n\
  --api-mode strict|discovery   stop at stubs, or collect reviewed API failures (default: strict)\n\
  --compile fast|speed    automatic JIT region policy (default: fast)\n\
  --no-region-linking     return to the dispatcher between native regions\n\
  --sync-compilation      compile automatic regions on the execution thread\n\
  --codepage 932|936      guest ANSI encoding: Japanese or GBK (default: 932)\n\
  --headless              run without a host window\n\
  --codec guest|native    whole-module Vorbis provider (default: native)\n\
  --audio                 enable PCM audio (headless simulates without a device)\n\
  --hidden                create a hidden host window for frontend tests\n\
  --width N --height N    initial guest canvas (default: 640 x 480)\n\
  --frames N              stop after N presented guest frames\n\
  --seconds N             stop after N seconds of host wall time\n\
  --data-dir PATH         guest write overlay (default: target/adv-data/<exe>)\n\
  --report PATH           write a JSON execution report\n\
  --screenshot PATH       write the final frame as PNG\n\
  --trace PATH            write every completed guest API call to a local trace\n\
  --auto-dialog-ms N      explicitly auto-select dialog defaults after N guest ms\n\
  --self-test-input       post A key and left-click after the first idle\n\
  --profile               collect execution timings by guest address\n\
  --control-stdin         explicitly enable line-based diagnostic commands\n\
  --help                  show this help\n\
All guest code, including DLLs and callbacks, executes inside the guest CPU.";

impl Options {
    fn parse() -> Result<Option<Self>, String> {
        let mut exe = None;
        let mut data_dir = None;
        let mut options = Self {
            exe: PathBuf::new(),
            data_dir: PathBuf::new(),
            jit: true,
            compile_strategy: CompileStrategy::Fast,
            region_linking: true,
            async_compilation: true,
            codepage: 932,
            headless: false,
            hidden: false,
            width: 640,
            height: 480,
            frames: None,
            seconds: None,
            report: None,
            screenshot: None,
            trace: None,
            auto_dialog_ms: None,
            self_test_input: false,
            profile: false,
            control_stdin: false,
            audio: false,
            codec: adv_runtime::vorbis::Policy::Native,
            api_mode: adv_runtime::RunMode::Strict,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("{arg} requires a value"));
            match arg.as_str() {
                "--help" | "-h" => {
                    println!("{HELP}");
                    return Ok(None);
                }
                "--exe" => exe = Some(PathBuf::from(value()?)),
                "--data-dir" => data_dir = Some(PathBuf::from(value()?)),
                "--api-mode" => {
                    options.api_mode = match value()?.as_str() {
                        "strict" => adv_runtime::RunMode::Strict,
                        "discovery" => adv_runtime::RunMode::Discovery,
                        _ => return Err("--api-mode must be strict or discovery".into()),
                    };
                }
                "--backend" => {
                    options.jit = match value()?.as_str() {
                        "jit" => true,
                        "interpreter" => false,
                        other => return Err(format!("unknown backend: {other}")),
                    }
                }
                "--codepage" => {
                    options.codepage = match value()?.as_str() {
                        "932" => 932,
                        "936" => 936,
                        _ => return Err("--codepage must be 932 or 936".into()),
                    };
                }
                "--compile" => {
                    options.compile_strategy = match value()?.as_str() {
                        "fast" => CompileStrategy::Fast,
                        "speed" => CompileStrategy::Speed,
                        _ => return Err("--compile must be fast or speed".into()),
                    };
                }
                "--no-region-linking" => options.region_linking = false,
                "--sync-compilation" => options.async_compilation = false,
                "--headless" => options.headless = true,
                "--hidden" => options.hidden = true,
                "--self-test-input" => options.self_test_input = true,
                "--profile" => options.profile = true,
                "--control-stdin" => options.control_stdin = true,
                "--audio" => options.audio = true,
                "--codec" => {
                    options.codec = match value()?.as_str() {
                        "guest" => adv_runtime::vorbis::Policy::Guest,
                        "native" => adv_runtime::vorbis::Policy::Native,
                        _ => return Err("--codec must be guest or native".into()),
                    }
                }
                "--width" => options.width = value()?.parse().map_err(|_| "invalid width")?,
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

fn api_report(runtime: &Runtime) -> String {
    let mode = match runtime.api_mode() {
        adv_runtime::RunMode::Strict => "strict",
        adv_runtime::RunMode::Discovery => "discovery",
    };
    let diagnostics = runtime.api_diagnostics();
    let gaps = diagnostics.map_or_else(String::new, |d| {
        d.gaps
            .iter()
            .map(|(name, count)| format!("{}: {count}", json(name)))
            .collect::<Vec<_>>()
            .join(", ")
    });
    let recent = diagnostics.map_or_else(String::new, |d| d.recent_stubs.iter().map(|call| {
        let args = call.arguments.iter().map(|arg| json(arg)).collect::<Vec<_>>().join(", ");
        format!("{{\"module\": {}, \"name\": {}, \"caller\": {}, \"context\": {}, \"arguments\": [{args}]}}",
            json(&call.module), json(&call.name), call.caller, call.context)
    }).collect::<Vec<_>>().join(", "));
    let conflicts = runtime
        .api_signature_conflicts()
        .iter()
        .map(|conflict| {
            format!(
                "{{\"api\": {}, \"registered\": {}, \"metadata\": {}}}",
                json(&format!("{}!{}", conflict.module, conflict.name)),
                json(&format!("{:?}", conflict.registered)),
                json(&format!("{:?}", conflict.metadata))
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let branch = runtime.last_indirect_branch().map_or_else(
        || "null".into(),
        |branch| {
            format!(
                "{{\"source\": {}, \"target\": {}, \"kind\": {}}}",
                branch.source,
                branch.target,
                json(&format!("{:?}", branch.kind))
            )
        },
    );
    let coverage = runtime.api_coverage();
    format!(
        concat!(
            "{{\"mode\": {}, \"acceptance_eligible\": {}, \"stub_calls\": {}, ",
            "\"called_gaps\": {{{}}}, \"recent_stubs\": [{}], \"signature_conflicts\": [{}], ",
            "\"last_indirect_branch\": {}, \"coverage\": {{\"generated_exports\": {}, \"implement\": {}, \"state\": {}, \"safe_fail\": {}, \"legacy\": {}}}}}"
        ),
        json(mode),
        runtime.api_acceptance_eligible(),
        diagnostics.map_or(0, |d| d.stub_calls),
        gaps,
        recent,
        conflicts,
        branch,
        coverage.generated_exports,
        coverage.implement,
        coverage.state,
        coverage.safe_fail,
        coverage.legacy
    )
}

struct ReportContext<'a> {
    options: &'a Options,
    runtime: &'a Runtime,
    started: Instant,
    first_frame_seconds: Option<f64>,
    stop_reason: &'a str,
    exit_code: Option<u32>,
    idle_waits: u64,
    wait_time: Duration,
    inputs: u64,
    failure: Option<&'a str>,
    screenshot_error: Option<&'a str>,
    performance: &'a performance::Performance,
    audio: &'a audio::AudioOutput,
}

fn build_report(context: ReportContext<'_>) -> String {
    let ReportContext {
        options,
        runtime,
        started,
        first_frame_seconds,
        stop_reason,
        exit_code,
        idle_waits,
        wait_time,
        inputs,
        failure,
        screenshot_error,
        performance,
        audio,
    } = context;
    let frame = runtime.frame();
    let stats = runtime.machine.cpu.stats();
    let mut codec = adv_runtime::vorbis::stats(&runtime.machine).map_or_else(
        || "{\"provider\": \"guest\"}".into(),
        |s| format!(concat!("{{\"provider\": \"native\", \"opens\": {}, \"failed_opens\": {}, ",
            "\"clears\": {}, \"active_streams\": {}, \"decoded_packets\": {}, \"decoded_frames\": {}, ",
            "\"guest_callbacks\": {}, \"read_callbacks\": {}, \"seek_callbacks\": {}, \"tell_callbacks\": {}, ",
            "\"close_callbacks\": {}, \"callback_bytes\": {}, \"metadata_scans\": {}}}"),
            s.opens, s.failed_opens, s.clears, s.active_streams, s.decoded_packets, s.decoded_frames,
            s.guest_callbacks, s.read_callbacks, s.seek_callbacks, s.tell_callbacks, s.close_callbacks, s.callback_bytes, s.metadata_scans),
    );
    if let Some(selection) = runtime
        .machine
        .ext
        .try_get::<adv_runtime::vorbis::Selection>()
    {
        codec.pop();
        codec.push_str(&format!(
            ", \"selection_reason\": {}}}",
            selection
                .reason
                .as_deref()
                .map_or_else(|| "null".into(), json)
        ));
    }
    let pcm = runtime.audio_pcm_metrics();
    let sources = runtime.audio_source_status();
    let pcm_metrics = format!(
        concat!(
            "{{\"queued_blocks\": {}, \"queued_source_frames\": {}, \"playback_starts\": {}, ",
            "\"flushes\": {}, \"ends\": {}, \"removals\": {}, \"max_stream_queue_us\": {}, ",
            "\"missing_stream_frames\": {}, \"missing_output_frames\": {}, ",
            "\"buffers\": {}, \"playing\": {}, \"looping\": {}, \"locked\": {}}}"
        ),
        pcm.queued_blocks,
        pcm.queued_source_frames,
        pcm.playback_starts,
        pcm.flushes,
        pcm.ends,
        pcm.removals,
        pcm.max_stream_queue_us,
        pcm.missing_stream_frames,
        pcm.missing_output_frames,
        sources.buffers,
        sources.playing,
        sources.looping,
        sources.locked,
    );
    let missing = runtime.missing_apis();
    let modules = adv_os::modules(&runtime.machine.ext).iter().map(|(_, module)| format!(
        "{{\"name\": {}, \"kind\": {}, \"base\": \"{:#010x}\", \"size\": {}, \"exports\": {}}}",
        json(&module.name), json(&format!("{:?}", module.kind)), module.base, module.size, module.exports_by_name.len()
    )).collect::<Vec<_>>().join(", ");
    format!(
        concat!(
            "{{\n  \"exe\": {},\n  \"backend\": {},\n  \"compile_strategy\": {},\n  \"region_linking\": {},\n  \"codepage\": {},\n  \"stop_reason\": {},\n",
            "  \"guest_exit_code\": {},\n  \"elapsed_seconds\": {:.6},\n  \"first_frame_seconds\": {},\n  \"frames\": {},\n",
            "  \"width\": {},\n  \"height\": {},\n  \"frame_fnv1a64\": \"{:016x}\",\n",
            "  \"idle_waits\": {},\n  \"idle_wait_seconds\": {:.6},\n  \"input_messages\": {},\n",
            "  \"host_calls\": {},\n  \"native_entries\": {},\n  \"interpreter_entries\": {},\n",
            "  \"compilations\": {},\n  \"async_compilation\": {},\n  \"compile_seconds\": {:.6},\n  \"compile_requests\": {},\n  \"pending_compilations\": {},\n  \"peak_pending_compilations\": {},\n  \"stale_compilations\": {},\n  \"compile_failures\": {},\n  \"guest_contexts\": {},\n  \"tls_images\": {},\n  \"loaded_modules\": [{}],\n",
            "  \"api\": {},\n  \"startup\": {{\"loaded\": true, \"guest_windows\": {}}},\n",
            "  \"missing_apis\": [{}],\n  \"failure\": {},\n  \"screenshot_error\": {},\n  \"performance\": {},\n  \"audio\": {},\n  \"pcm\": {},\n  \"codec\": {},\n  \"runtime\": {}\n}}\n"
        ),
        json(&options.exe.to_string_lossy()),
        json(if options.jit { "jit" } else { "interpreter" }),
        if options.jit {
            json(match options.compile_strategy {
                CompileStrategy::Fast => "fast",
                CompileStrategy::Speed => "speed",
            })
        } else {
            "null".into()
        },
        options.jit && options.region_linking && !options.profile,
        options.codepage,
        json(stop_reason),
        exit_code.map_or_else(|| "null".into(), |code| code.to_string()),
        started.elapsed().as_secs_f64(),
        first_frame_seconds.map_or_else(|| "null".into(), |seconds| format!("{seconds:.6}")),
        runtime.presented_frames(),
        frame.width,
        frame.height,
        checksum(&frame.bgra),
        idle_waits,
        wait_time.as_secs_f64(),
        inputs,
        runtime.machine.sys.host_calls,
        stats.native_entries,
        stats.interpreter_entries,
        stats.compilations,
        options.jit && options.async_compilation,
        stats.compile_time.as_secs_f64(),
        stats.compile_requests,
        stats.pending_compilations,
        stats.peak_pending_compilations,
        stats.stale_compilations,
        stats.compile_failures,
        runtime.machine.sys.contexts.len(),
        runtime.machine.sys.tls_images.len(),
        modules,
        api_report(runtime),
        runtime.guest_window_count(),
        missing
            .iter()
            .map(|s| json(s))
            .collect::<Vec<_>>()
            .join(", "),
        failure.map_or_else(|| "null".into(), json),
        screenshot_error.map_or_else(|| "null".into(), json),
        performance.json(),
        audio.json(),
        pcm_metrics,
        codec,
        json(&runtime.report_with_profile(&runtime.machine.cpu.profile()))
    )
}

fn write_report(path: &std::path::Path, report: &str) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, report).map_err(|e| e.to_string())
}

fn run(options: Options) -> Result<u8, String> {
    window::initialize_dpi()?;
    let wake = window::WakeEvent::new().map_err(|e| e.to_string())?;
    let keys = std::sync::Arc::new(adv_sync::input::KeyState::default());
    if options.headless {
        return run_execution(options, None, wake, keys);
    }
    let mut window = window::Window::new(
        &format!(
            "ADV — {}",
            options
                .exe
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ),
        options.width,
        options.height,
        options.hidden,
    )?;
    let (worker, mut ui) = frontend::channel(wake.clone(), keys.clone())?;
    let completion_wake = ui.wake();
    let (mut tx, mut rx) = adv_sync::spsc::channel(1);
    let execution = std::thread::Builder::new()
        .name("adv-execution".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_execution(options, Some(worker), wake, keys)
            }))
            .unwrap_or_else(|_| Err("execution worker panicked".into()));
            let _ = tx.push(result);
            completion_wake.signal();
        })
        .map_err(|e| e.to_string())?;
    let result = ui.run(&mut window, &mut rx);
    drop(ui);
    execution
        .join()
        .map_err(|_| "execution worker panicked".to_owned())?;
    result?
}

fn run_execution(
    options: Options,
    mut window: Option<frontend::Worker>,
    wake: window::WakeEvent,
    keys: std::sync::Arc<adv_sync::input::KeyState>,
) -> Result<u8, String> {
    let started = Instant::now();
    let mut runtime = Runtime::with_codec(
        &options.exe,
        &options.data_dir,
        options.jit,
        options.width,
        options.height,
        options.codec,
    )?;
    runtime.set_codepage(options.codepage);
    runtime.set_api_mode(options.api_mode);
    runtime.machine.cpu.set_waker(wake.clone())?;
    runtime
        .audio_stream()
        .set_waker(wake.clone())
        .map_err(|_| "audio wake event was already registered".to_owned())?;
    runtime
        .machine
        .cpu
        .set_auto_compile_strategy(options.compile_strategy);
    runtime
        .machine
        .cpu
        .set_region_linking(options.region_linking);
    runtime
        .machine
        .cpu
        .set_async_compilation(options.jit && options.async_compilation);
    runtime.set_frame_limit(options.frames);
    runtime.set_audio_enabled(options.audio);
    runtime.machine.cpu.set_profiling(options.profile);
    if let Some(delay) = options.auto_dialog_ms {
        runtime.set_auto_dialog_ms(Some(delay));
    }
    if let Some(path) = &options.trace {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        runtime.machine.sys.trace = adv_machine::Trace::to_writer(Box::new(
            std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?),
        ));
        runtime.machine.sys.trace.all = true;
    }
    runtime.set_host_keys(keys.clone());
    if let Some(worker) = window.as_mut() {
        let producer = worker.take_frames();
        runtime.set_frame_mailbox(producer, worker.ui_wake.clone());
    }
    let mut audio = audio::AudioOutput::new(options.audio, options.headless, &runtime);
    let mut control = options
        .control_stdin
        .then(|| control::Control::start(wake.clone(), keys.clone()))
        .transpose()?;
    if control.is_some() {
        runtime.enable_input_receipts();
    }
    let mut idle_waits = 0u64;
    let mut wait_time = Duration::ZERO;
    let mut performance = performance::Performance::default();
    let mut inputs = 0u64;
    let mut injected = false;
    let mut first_frame_seconds = None;
    let mut close_at = None;
    let mut failure = None;
    let mut exit_code = None;
    let stop_reason;
    loop {
        wake.take_interrupt();
        if window.as_ref().is_some_and(frontend::Worker::stopped) {
            return Err("UI stopped while the guest was running".into());
        }
        if let Some(w) = window.as_mut() {
            for event in w.pump(&mut performance) {
                match event {
                    window::Event::Input {
                        message,
                        wparam,
                        lparam,
                    } => {
                        inputs += u64::from(runtime.post_input(message, wparam, lparam));
                    }
                    window::Event::Close => {
                        runtime.post_input(0x10, 0, 0);
                        close_at.get_or_insert_with(Instant::now);
                    }
                    window::Event::Repaint => {}
                    window::Event::FocusLost => inputs += runtime.release_input() as u64,
                }
            }
        }
        if let Some(controller) = control.as_mut() {
            for command in controller.drain() {
                let receipt_before = runtime.latest_input_receipt();
                let result = match command {
                    control::Command::Click(x, y) => {
                        let position = u32::from(x as u16) | (u32::from(y as u16) << 16);
                        let posted = [(0x200, 0), (0x201, 1), (0x202, 0)]
                            .into_iter()
                            .filter(|&(message, wparam)| {
                                runtime.post_input(message, wparam, position)
                            })
                            .count();
                        inputs += posted as u64;
                        if posted == 3 {
                            Ok(format!("click {x} {y}"))
                        } else {
                            Err("click has no guest target window".into())
                        }
                    }
                    control::Command::Key(key) => {
                        let down = runtime.post_input(0x100, key, 1);
                        let up = runtime.post_input(0x101, key, 0xc0000001);
                        inputs += u64::from(down) + u64::from(up);
                        if down && up {
                            Ok(format!("key {key}"))
                        } else {
                            Err("key has no guest target window".into())
                        }
                    }
                    control::Command::KeyDown(key) | control::Command::KeyUp(key) => {
                        let pressed = matches!(command, control::Command::KeyDown(_));
                        let posted = runtime.post_input(
                            if pressed { 0x100 } else { 0x101 },
                            key,
                            if pressed { 1 } else { 0xc0000001 },
                        );
                        inputs += u64::from(posted);
                        if posted {
                            Ok(format!("key-{} {key}", if pressed { "down" } else { "up" }))
                        } else {
                            Err("key has no guest target window".into())
                        }
                    }
                    control::Command::Capture(path) => {
                        runtime.capture_latest_frame()?;
                        save_frame(&path, runtime.frame())
                            .map(|_| format!("capture {}", path.display()))
                    }
                    control::Command::Report(path) => {
                        runtime.capture_latest_frame()?;
                        runtime.machine.sys.trace.flush();
                        let snapshot = build_report(ReportContext {
                            options: &options,
                            runtime: &runtime,
                            started,
                            first_frame_seconds,
                            stop_reason: "running",
                            exit_code: None,
                            idle_waits,
                            wait_time,
                            inputs,
                            failure: None,
                            screenshot_error: None,
                            performance: &performance,
                            audio: &audio,
                        });
                        write_report(&path, &snapshot).map(|_| format!("report {}", path.display()))
                    }
                    control::Command::ResetMetrics => {
                        let serial = runtime.presented_frames();
                        performance.reset(serial);
                        Ok(format!("reset-metrics start_frame={serial}"))
                    }
                    control::Command::Close => {
                        runtime.post_input(0x10, 0, 0);
                        close_at.get_or_insert_with(Instant::now);
                        if let Some(window) = window.as_mut() {
                            window.hide();
                        }
                        Ok("close".into())
                    }
                    control::Command::Error(error) => Err(error),
                };
                let receipt = runtime.latest_input_receipt();
                controller.note_input_receipt(if receipt != receipt_before {
                    receipt
                } else {
                    0
                });
                control::acknowledge(result);
            }
        }
        if options
            .seconds
            .is_some_and(|limit| started.elapsed().as_secs_f64() >= limit)
        {
            stop_reason = "time_limit";
            break;
        }
        if close_at.is_some_and(|at: Instant| at.elapsed() >= Duration::from_secs(2)) {
            stop_reason = "host_window_closed";
            break;
        }
        let time_remaining = options
            .seconds
            .map(|seconds| Duration::from_secs_f64(seconds).saturating_sub(started.elapsed()));
        let close_remaining =
            close_at.map(|at| Duration::from_secs(2).saturating_sub(at.elapsed()));
        if let Some(remaining) = [time_remaining, close_remaining]
            .into_iter()
            .flatten()
            .min()
        {
            runtime
                .machine
                .set_execution_budget(Some(remaining.max(Duration::from_millis(1))));
        }
        let step_started = Instant::now();
        let interrupts_before = runtime.machine.sys.scheduling.external_interrupts;
        let frame_yields_before = runtime.machine.sys.scheduling.frame_host_yields;
        let outcome = runtime.step_for(SOFT_SLICE);
        if let Some(controller) = control.as_mut() {
            let ordinary = runtime.machine.sys.scheduling.external_interrupts == interrupts_before;
            let full_or_idle = runtime.machine.sys.scheduling.frame_host_yields
                == frame_yields_before
                || matches!(&outcome, SliceResult::Complete(RunResult::Idle { .. }));
            let input_completed = ordinary
                && controller
                    .awaited_input_receipt()
                    .is_none_or(|receipt| runtime.input_receipt_completed(receipt, full_or_idle));
            controller.after_guest_step(input_completed);
        }
        let step_elapsed = step_started.elapsed();
        audio.update();
        let presented = runtime.presented_frames();
        performance.step(step_elapsed, presented);
        if first_frame_seconds.is_none() && presented > 0 {
            first_frame_seconds = runtime
                .first_frame_time()
                .map(|at| at.saturating_duration_since(started).as_secs_f64());
        }
        let outcome = match outcome {
            SliceResult::Yielded => {
                performance.soft_yield();
                continue;
            }
            SliceResult::Complete(outcome) => outcome,
        };
        match outcome {
            RunResult::Exited(code) => {
                if options.frames.is_some_and(|limit| presented >= limit) {
                    stop_reason = "frame_limit";
                    break;
                }
                exit_code = Some(code);
                stop_reason = "guest_exit";
                break;
            }
            RunResult::Failed(error) => {
                if options
                    .seconds
                    .is_some_and(|limit| started.elapsed().as_secs_f64() >= limit)
                    && error.message.contains("execution budget exhausted")
                {
                    stop_reason = "time_limit";
                } else if close_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
                    && error.message.contains("execution budget exhausted")
                {
                    stop_reason = "host_window_closed";
                } else {
                    failure = Some(format!("{error}\n{}", error.recent.join("\n")));
                    stop_reason = "guest_failure";
                }
                break;
            }
            RunResult::Idle { next_deadline } => {
                if options
                    .frames
                    .is_some_and(|limit| runtime.presented_frames() >= limit)
                {
                    stop_reason = "frame_limit";
                    break;
                }
                if options.self_test_input && !injected {
                    // Probe for a guest window after startup/worker waits. In windowed
                    // tests the input takes the native callback and event-pump path.
                    if runtime.post_input(0, 0, 0) {
                        for (message, wparam, lparam) in [
                            (0x100, 0x41, 1),
                            (0x101, 0x41, 0xc0000001),
                            (0x200, 0, (80 << 16) | 100),
                            (0x201, 1, (80 << 16) | 100),
                            (0x202, 0, (80 << 16) | 100),
                        ] {
                            if let Some(w) = window.as_mut() {
                                w.post_input(message, wparam, lparam)?;
                            } else {
                                inputs += u64::from(runtime.post_input(message, wparam, lparam));
                            }
                        }
                        injected = true;
                        continue;
                    }
                }
                let now = runtime.machine.sys.clock.now_us();
                // Unbounded input waits still allow host limits and close requests to complete.
                let mut delay = next_deadline
                    .map(|deadline| Duration::from_micros(deadline.saturating_sub(now)));
                if let Some(seconds) = options.seconds {
                    let remaining =
                        Duration::from_secs_f64(seconds).saturating_sub(started.elapsed());
                    delay = Some(delay.map_or(remaining, |d| d.min(remaining)));
                }
                if let Some(at) = close_at {
                    let remaining = Duration::from_secs(2).saturating_sub(at.elapsed());
                    delay = Some(delay.map_or(remaining, |d| d.min(remaining)));
                }
                delay = audio.wait_duration(delay);
                let wait_started = Instant::now();
                // Every producer signals this same retained event. No UI/frame or
                // stdin-specific blocking loop can hide another channel's wake.
                let pending = control.as_ref().is_some_and(control::Control::has_pending);
                let signaled = pending || wake.wait(delay).map_err(|e| e.to_string())?;
                let wake = if signaled {
                    window::WaitOutcome::Input
                } else {
                    window::WaitOutcome::Deadline
                };
                let waited = wait_started.elapsed();
                performance.wait(delay, waited, wake);
                wait_time += waited;
                idle_waits += 1;
            }
        }
    }
    runtime.machine.sys.trace.flush();
    runtime.capture_latest_frame()?;
    let frame = runtime.frame();
    let screenshot_error = options
        .screenshot
        .as_ref()
        .and_then(|path| save_frame(path, frame).err());
    let report = build_report(ReportContext {
        options: &options,
        runtime: &runtime,
        started,
        first_frame_seconds,
        stop_reason,
        exit_code,
        idle_waits,
        wait_time,
        inputs,
        failure: failure.as_deref(),
        screenshot_error: screenshot_error.as_deref(),
        performance: &performance,
        audio: &audio,
    });
    if let Some(path) = &options.report {
        write_report(path, &report)?;
    }
    let stats = runtime.machine.cpu.stats();
    println!(
        "{stop_reason}: exit={} frames={} elapsed={:.3}s host_calls={} native_entries={} interpreter_entries={} idle_waits={}",
        exit_code.map_or_else(|| "none".into(), |code| code.to_string()),
        frame.frames,
        started.elapsed().as_secs_f64(),
        runtime.machine.sys.host_calls,
        stats.native_entries,
        stats.interpreter_entries,
        idle_waits
    );
    if let Some(error) = failure {
        eprintln!("{error}");
        return Ok(1);
    }
    if let Some(error) = screenshot_error {
        eprintln!("screenshot: {error}");
        return Ok(1);
    }
    Ok(if exit_code.is_some_and(|code| code != 0) {
        1
    } else {
        0
    })
}

fn main() -> std::process::ExitCode {
    match Options::parse().and_then(|options| options.map_or(Ok(0), run)) {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("adv-run: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
