//! Native presentation and deadline-driven input for a guest PE process.
mod audio;
mod control;
mod performance;
mod window;

use adv_runtime::{CompileStrategy, Snapshot, boot::Runtime};
use pal_machine::{RunResult, SliceResult};
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
}

const HELP: &str = "adv-run <EXE> [options]\n\
  --exe PATH              guest executable (also accepted positionally)\n\
  --backend jit|interpreter   shared guest backend (default: jit)\n\
  --compile fast|speed    automatic JIT region policy (default: fast)\n\
  --no-region-linking     return to the dispatcher between native regions\n\
  --sync-compilation      compile automatic regions on the execution thread\n\
  --codepage 932|936      guest ANSI encoding: Japanese or GBK (default: 932)\n\
  --headless              run without a host window\n\
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
    let missing = runtime.missing_apis();
    let modules = runtime.machine.sys.modules.iter().map(|(_, module)| format!(
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
            "  \"missing_apis\": [{}],\n  \"failure\": {},\n  \"screenshot_error\": {},\n  \"performance\": {},\n  \"audio\": {},\n  \"runtime\": {}\n}}\n"
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
        frame.frames,
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
        missing
            .iter()
            .map(|s| json(s))
            .collect::<Vec<_>>()
            .join(", "),
        failure.map_or_else(|| "null".into(), json),
        screenshot_error.map_or_else(|| "null".into(), json),
        performance.json(),
        audio.json(),
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
    let started = Instant::now();
    let mut runtime = Runtime::new(
        &options.exe,
        &options.data_dir,
        options.jit,
        options.width,
        options.height,
    )?;
    runtime.set_codepage(options.codepage);
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
        runtime.machine.sys.trace = pal_machine::Trace::to_writer(Box::new(
            std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?),
        ));
        runtime.machine.sys.trace.all = true;
    }
    let mut window = if options.headless {
        None
    } else {
        Some(window::Window::new(
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
        )?)
    };
    let mut serial = u64::MAX;
    let mut audio = audio::AudioOutput::new(options.audio, options.headless, &runtime);
    let mut control = options
        .control_stdin
        .then(control::Control::start)
        .transpose()?;
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
        if let Some(controller) = control.as_mut() {
            for command in controller.drain() {
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
                    control::Command::Capture(path) => save_frame(&path, runtime.frame())
                        .map(|_| format!("capture {}", path.display())),
                    control::Command::Report(path) => {
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
                        let serial = runtime.frame().frames;
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
                control::acknowledge(result);
            }
        }
        if let Some(w) = window.as_mut() {
            let pump_started = Instant::now();
            for event in w.pump() {
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
                    window::Event::Repaint => serial = u64::MAX,
                    window::Event::FocusLost => inputs += runtime.release_input() as u64,
                }
            }
            performance.pump(pump_started.elapsed());
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
        let outcome = runtime.step_for(SOFT_SLICE);
        let step_elapsed = step_started.elapsed();
        audio.update();
        let frame = runtime.frame();
        performance.step(step_elapsed, frame.frames);
        if first_frame_seconds.is_none() && frame.frames > 0 {
            first_frame_seconds = runtime
                .first_frame_time()
                .map(|at| at.saturating_duration_since(started).as_secs_f64());
        }
        if frame.frames != serial && frame.width != 0 && !frame.bgra.is_empty() {
            if let Some(w) = window.as_mut() {
                let present_started = Instant::now();
                w.present(frame)?;
                performance.present(frame.frames, present_started.elapsed(), Instant::now());
            }
            serial = frame.frames;
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
                if options.frames.is_some_and(|limit| frame.frames >= limit) {
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
                    .is_some_and(|limit| runtime.frame().frames >= limit)
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
                let wake = if let Some(w) = window.as_mut() {
                    if let Some(controller) = control.as_ref() {
                        if controller.has_pending() {
                            window::WaitOutcome::Input
                        } else {
                            w.wait_with_wake(delay, Some(controller.wake()))?
                        }
                    } else {
                        w.wait(delay)?
                    }
                } else if let Some(controller) = control.as_mut() {
                    if controller.wait(delay) {
                        window::WaitOutcome::Input
                    } else {
                        window::WaitOutcome::Deadline
                    }
                } else {
                    std::thread::sleep(delay.unwrap_or(Duration::from_secs(1)));
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
