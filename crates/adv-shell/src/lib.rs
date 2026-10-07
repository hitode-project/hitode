//! Native presentation and deadline-driven input for a guest PE process.
mod audio;
mod control;
mod frontend;
#[cfg(all(windows, feature = "gpu"))]
mod native_gpu;
mod performance;
pub mod window;

use adv_machine::{RunResult, SliceResult};
use adv_runtime::{CompileStrategy, Snapshot, boot::Runtime};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SOFT_SLICE: Duration = Duration::from_millis(8);

struct Options {
    guest_workers: usize,
    row_workers: Option<usize>,
    row_batching: bool,
    exe: PathBuf,
    data_dir: PathBuf,
    cdroms: Vec<(char, PathBuf)>,
    jit: bool,
    compile_strategy: CompileStrategy,
    region_linking: bool,
    async_compilation: bool,
    tiered_compilation: bool,
    codepage: u32,
    headless: bool,
    hidden: bool,
    gpu_software: bool,
    d3d9_sm2: bool,
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
    self_test_input: bool,
    profile: bool,
    control_stdin: bool,
    audio: bool,
    codec: adv_runtime::vorbis::Policy,
    codec_kernels: adv_runtime::codec_kernels::Mode,
    codec_kernels_explicit: bool,
    codec_signatures: Option<PathBuf>,
    api_mode: adv_runtime::RunMode,
    time_skip: bool,
}

const HELP: &str = "adv-run <EXE> [options]\n\
  --exe PATH              guest executable (also accepted positionally)\n\
  --backend jit|interpreter   shared guest backend (default: jit)\n\
  --guest-workers 1|2     default: 1; experimental 2 requires Windows x64/Fast\n\
  --row-workers N         additional row helpers, 0 runs batches inline (default: up to 3)\n\
  --no-row-batching       disable proven row batches for comparison\n\
  --api-mode strict|discovery   stop at stubs, or collect reviewed API failures (default: strict)\n\
  --no-time-skip           disable timed-wait acceleration while Ctrl is held\n\
  --compile fast|speed    automatic JIT region policy (default: fast)\n\
  --no-tiering            keep automatic regions at the selected compile strategy\n\
  --no-region-linking     return to the dispatcher between native regions\n\
  --sync-compilation      compile automatic regions on the execution thread\n\
  --codepage 932|936      guest ANSI encoding: Japanese or GBK (default: 932)\n\
  --headless              run without a host window\n\
  --codec guest|native    whole-module Vorbis provider (default: native)\n\
  --codec-kernels guest|verify|native  static codec math (default: native; allows FP rounding differences)\n\
  --codec-signatures PATH reviewed library signatures (default: the bundled set)\n\
  --audio                 enable PCM audio (headless simulates without a device)\n\
  --hidden                create a hidden host window for frontend tests\n\
  --gpu-software          select a software GPU adapter for diagnostics\n\
  --d3d9-sm2             opt in to the SM2 capability milestone (default: off)\n\
  --d3d9-sm3             opt in to the SM3 capability milestone (default: off)\n\
  --d3d9-shader-cache PATH opt in to a bounded local WGSL disk cache\n\
  --width N --height N    initial guest canvas (default: 640 x 480)\n\
  --frames N              stop after N presented guest frames\n\
  --seconds N             stop after N seconds of host wall time\n\
  --data-dir PATH         guest write overlay (default: target/adv-data/<exe>)\n\
  --cdrom D=PATH          attach a read-only ISO to guest drive D (repeatable)\n\
  --report PATH           write a JSON execution report\n\
  --screenshot PATH       write the final frame as PNG\n\
  --trace PATH            write every completed guest API call to a local trace\n\
  --host-profile PATH     opt-in API service time CSV (also supports N=2)\n\
  --schedule-profile PATH opt-in scheduling and lock boundary TSV\n\
  --auto-dialog-ms N      explicitly auto-select dialog defaults after N guest ms\n\
  --self-test-input       post A key and left-click after the first idle\n\
  --profile               collect execution timings; with --report writes .cpu.tsv (disables region links)\n\
  --control-stdin         explicitly enable line-based diagnostic commands\n\
  --help                  show this help\n\
All guest code, including DLLs and callbacks, executes inside the guest CPU.";

impl Options {
    fn parse() -> Result<Option<Self>, String> {
        Self::parse_args(std::env::args().skip(1))
    }

    fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut exe = None;
        let mut data_dir = None;
        let mut options = Self {
            guest_workers: 1,
            row_workers: None,
            row_batching: true,
            exe: PathBuf::new(),
            data_dir: PathBuf::new(),
            cdroms: Vec::new(),
            jit: true,
            compile_strategy: CompileStrategy::Fast,
            region_linking: true,
            async_compilation: true,
            tiered_compilation: true,
            codepage: 932,
            headless: false,
            hidden: false,
            gpu_software: false,
            d3d9_sm2: false,
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
            self_test_input: false,
            profile: false,
            control_stdin: false,
            audio: false,
            codec: adv_runtime::vorbis::Policy::Native,
            codec_kernels: adv_runtime::codec_kernels::Mode::Native,
            codec_kernels_explicit: false,
            codec_signatures: None,
            api_mode: adv_runtime::RunMode::Strict,
            time_skip: true,
        };
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("{arg} requires a value"));
            match arg.as_str() {
                "--help" | "-h" => {
                    println!("{HELP}");
                    return Ok(None);
                }
                "--exe" => exe = Some(PathBuf::from(value()?)),
                "--data-dir" => data_dir = Some(PathBuf::from(value()?)),
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
                "--no-time-skip" => options.time_skip = false,
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
                "--guest-workers" => {
                    options.guest_workers =
                        value()?.parse().map_err(|_| "invalid guest worker count")?;
                    if !matches!(options.guest_workers, 1 | 2) {
                        return Err("--guest-workers must be 1 or 2".into());
                    }
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
                "--d3d9-sm2" => options.d3d9_sm2 = true,
                "--d3d9-sm3" => options.d3d9_sm3 = true,
                "--d3d9-shader-cache" => options.d3d9_shader_cache = Some(value()?.into()),
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
                "--codec-kernels" => {
                    options.codec_kernels = match value()?.as_str() {
                        "guest" => adv_runtime::codec_kernels::Mode::Guest,
                        "verify" => adv_runtime::codec_kernels::Mode::Verify,
                        "native" => adv_runtime::codec_kernels::Mode::Native,
                        _ => return Err("--codec-kernels must be guest, verify or native".into()),
                    };
                    options.codec_kernels_explicit = true;
                }
                "--codec-signatures" => options.codec_signatures = Some(value()?.into()),
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
        #[cfg(not(feature = "gpu"))]
        if options.d3d9_sm2 || options.d3d9_sm3 || options.d3d9_shader_cache.is_some() {
            return Err("D3D9 shader capability switches require the gpu feature".into());
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
        if options.guest_workers > 1
            && (!options.jit
                || options.compile_strategy != CompileStrategy::Fast
                || options.profile
                || !options.async_compilation)
        {
            return Err("two guest workers require JIT, Fast and asynchronous compilation; per-PC profiling is not yet supported".into());
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

fn dynamic_lookup_json(failure: &adv_runtime::DynamicLookupFailure) -> String {
    format!(
        concat!(
            "{{\"sequence\": {}, \"module\": {}, \"name\": {}, ",
            "\"failure_module\": {}, \"failure_name\": {}, ",
            "\"caller\": {}, \"context\": {}, \"kind\": {}, \"reason\": {}}}"
        ),
        failure.sequence,
        json(&failure.module),
        json(&failure.name),
        failure
            .failure_module
            .as_deref()
            .map_or_else(|| "null".into(), json),
        failure
            .failure_name
            .as_deref()
            .map_or_else(|| "null".into(), json),
        failure.caller,
        failure.context,
        json(failure.kind.as_str()),
        json(&failure.reason),
    )
}

fn message_box_json(message: &adv_runtime::MessageBoxCall) -> String {
    format!(
        concat!(
            "{{\"sequence\": {}, \"frame\": {}, \"api\": {}, \"caller\": {}, \"context\": {}, ",
            "\"caption\": {}, \"text\": {}, \"style\": {}, \"result\": {}, ",
            "\"preceding_lookup_failure\": {}}}"
        ),
        message.sequence,
        message.frame,
        json(&message.api),
        message.caller,
        message.context,
        json(&message.caption),
        json(&message.text),
        message.style,
        message.result,
        message
            .preceding_lookup_failure
            .as_ref()
            .map_or_else(|| "null".into(), dynamic_lookup_json),
    )
}

fn cpu_fault_json(fault: Option<&adv_machine::exception::CpuFaultSnapshot>) -> String {
    let Some(fault) = fault else {
        return "null".into();
    };
    let access = fault.access();
    let branch = fault.last_indirect_branch.map_or_else(
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
    let registers = fault
        .registers
        .iter()
        .map(|(register, value)| format!("{}: {value}", json(&format!("{register:?}"))))
        .collect::<Vec<_>>()
        .join(", ");
    let stack = fault
        .stack
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        concat!(
            "{{\"context\": {}, \"pc\": {}, \"kind\": {}, \"access\": {}, \"address\": {}, \"reason\": {}, ",
            "\"last_indirect_branch\": {}, \"registers\": {{{}}}, \"stack_address\": {}, \"stack\": [{}]}}"
        ),
        fault.context,
        fault.fault.pc,
        json(fault.kind_name()),
        access.map_or_else(|| "null".into(), |(access, _)| json(access)),
        access.map_or_else(|| "null".into(), |(_, address)| address.to_string()),
        fault.reason().map_or_else(|| "null".into(), json),
        branch,
        registers,
        fault.stack_address,
        stack,
    )
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
    let warnings = diagnostics.map_or_else(String::new, |d| {
        d.warnings
            .iter()
            .map(|((api, reason), count)| {
                format!(
                    "{{\"api\": {}, \"reason\": {}, \"count\": {count}}}",
                    json(api),
                    json(reason)
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    });
    let lookups = diagnostics.map_or_else(String::new, |d| {
        d.dynamic_lookup_failures
            .iter()
            .map(dynamic_lookup_json)
            .collect::<Vec<_>>()
            .join(", ")
    });
    let messages = diagnostics.map_or_else(String::new, |d| {
        d.message_boxes
            .iter()
            .map(message_box_json)
            .collect::<Vec<_>>()
            .join(", ")
    });
    let processes = runtime.process_statuses().iter().map(|process| format!(
        "{{\"pid\": {}, \"parent\": {}, \"image\": {}, \"exit_code\": {}, \"failure\": {}, \"first_cpu_fault\": {}, \"latest_cpu_fault\": {}}}",
        process.pid, process.parent.map_or_else(|| "null".into(), |v| v.to_string()), json(&process.image),
        process.exit_code.map_or_else(|| "null".into(), |v| v.to_string()),
        process.failure.as_deref().map_or_else(|| "null".into(), json),
        cpu_fault_json(process.first_cpu_fault.as_ref()),
        cpu_fault_json(process.latest_cpu_fault.as_ref())
    )).collect::<Vec<_>>().join(", ");
    format!(
        concat!(
            "{{\"mode\": {}, \"acceptance_eligible\": {}, \"stub_calls\": {}, ",
            "\"called_gaps\": {{{}}}, \"recent_stubs\": [{}], \"signature_conflicts\": [{}], ",
            "\"last_indirect_branch\": {}, \"coverage\": {{\"generated_exports\": {}, \"implement\": {}, \"state\": {}, \"safe_fail\": {}, \"legacy\": {}}}, ",
            "\"warnings\": [{}], \"unrecorded_warnings\": {}, ",
            "\"discovery_diagnostics_version\": 1, ",
            "\"dynamic_lookup_failures\": [{}], \"dynamic_lookup_failure_count\": {}, ",
            "\"dropped_dynamic_lookup_failures\": {}, \"message_boxes\": [{}], ",
            "\"message_box_count\": {}, \"dropped_message_boxes\": {}, ",
            "\"truncated_diagnostic_fields\": {}, \"processes\": [{}]}}"
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
        coverage.legacy,
        warnings,
        diagnostics.map_or(0, |d| d.unrecorded_warnings),
        lookups,
        diagnostics.map_or(0, |d| d.dynamic_lookup_failure_count),
        diagnostics.map_or(0, |d| d.dropped_dynamic_lookup_failures),
        messages,
        diagnostics.map_or(0, |d| d.message_box_count),
        diagnostics.map_or(0, |d| d.dropped_message_boxes),
        diagnostics.map_or(0, |d| d.truncated_diagnostic_fields),
        processes,
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
    scene_gate: &'a control::SceneGate,
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
        scene_gate,
    } = context;
    let frame = runtime.frame();
    let arena = runtime
        .machine
        .ext
        .get::<adv_runtime::Graphics>()
        .arena_usage();
    let graphics_memory = format!(
        "{{\"capacity_bytes\": {}, \"free_bytes\": {}, \"largest_free_bytes\": {}, \"pixel_bytes\": {}, \"live_surfaces\": {}}}",
        arena.capacity, arena.free, arena.largest_free, arena.pixel_bytes, arena.live_surfaces
    );
    let stats = runtime.machine.cpu.stats();
    let parallel = runtime.machine.cpu.parallel_stats();
    let parallel_json = parallel_report(&parallel);
    #[cfg(feature = "gpu")]
    let gpu_diagnostics = {
        let diagnostics = runtime.gpu_diagnostics();
        let versions = |counts: [u64; 7]| {
            ["1.1", "1.2", "1.3", "1.4", "2.0", "2.1", "3.0"]
                .iter()
                .zip(counts)
                .map(|(version, count)| format!("\"{version}\":{count}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let shader_versions = format!(
            "{{\"vertex\":{{{}}},\"pixel\":{{{}}},\"malformed\":{},\"untranslated\":{}}}",
            versions(diagnostics.shaders.vertex),
            versions(diagnostics.shaders.pixel),
            diagnostics.shaders.malformed,
            diagnostics.shaders.untranslated
        );
        let commands=diagnostics.gpu.map_or_else(||"null".into(),|g|format!("{{\"submissions\":{},\"draws\":{},\"vertices\":{},\"readbacks\":{},\"native_frames\":{},\"snapshot_allocations\":{},\"vertex_upload_allocations\":{}}}",g.submissions,g.draws,g.vertices,g.readbacks,g.native_frames,g.native_frame_allocations,g.vertex_buffer_allocations));
        format!(
            "{{\"live_interfaces\":{},\"pending_waits\":{},\"pending_readbacks\":{},\"commands\":{},\"shader_versions\":{}}}",
            diagnostics.live_interfaces,
            diagnostics.pending_waits,
            diagnostics.pending_readbacks,
            commands,
            shader_versions
        )
    };
    #[cfg(not(feature = "gpu"))]
    let gpu_diagnostics = "null";
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
    let codec_kernels = adv_runtime::codec_kernels::stats(&runtime.machine).map_or_else(
        || "{\"mode\":\"guest\",\"installed\":0}".into(),
        |s| {
            let reasons = s.fallback_reasons.iter()
                .map(|(reason, count)| format!("{}:{count}", json(reason)))
                .collect::<Vec<_>>().join(",");
            format!(concat!(
                "{{\"mode\":{},\"installed\":{},\"calls\":{},\"native_calls\":{},\"verify_calls\":{},",
                "\"fallback_calls\":{},\"revocations\":{},\"errors\":{},\"compared_floats\":{},",
                "\"different_floats\":{},\"nonfinite_floats\":{},\"max_abs\":{},\"rms\":{},",
                "\"relative_rms\":{},\"total_kernel_ns\":{},\"max_kernel_ns\":{},",
                "\"last_rejection\":{},\"table_mismatch\":{},\"fallback_reasons\":{{{}}}}}"
            ), json(s.mode.as_str()), s.installed, s.calls, s.native_calls, s.verify_calls,
                s.fallback_calls, s.revocations, s.errors, s.compared_floats, s.different_floats,
                s.nonfinite_floats, s.max_abs, s.rms, s.relative_rms, s.total_kernel_ns, s.max_kernel_ns,
                s.last_rejection.as_deref().map_or_else(|| "null".into(), json),
                s.table_mismatch.as_deref().map_or_else(|| "null".into(), json), reasons)
        },
    );
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
    let context_runs = runtime.machine.sys.scheduling.contexts.iter().enumerate()
        .map(|(id, r)| format!("{{\"id\":{id},\"runs\":{},\"owner_wall_ns\":{},\"host_calls\":{},\"competing_wall_ns\":{},\"parallel_dispatches\":{}}}",
            r.runs, r.owner_wall_ns, r.host_calls, r.competing_wall_ns, r.parallel_dispatches))
        .collect::<Vec<_>>().join(",");
    format!(
        concat!(
            "{{\n  \"exe\": {},\n  \"backend\": {},\n  \"compile_strategy\": {},\n  \"region_linking\": {},\n  \"codepage\": {},\n  \"stop_reason\": {},\n",
            "  \"guest_exit_code\": {},\n  \"elapsed_seconds\": {:.6},\n  \"first_frame_seconds\": {},\n  \"frames\": {},\n",
            "  \"width\": {},\n  \"height\": {},\n  \"frame_fnv1a64\": \"{:016x}\",\n",
            "  \"idle_waits\": {},\n  \"idle_wait_seconds\": {:.6},\n  \"input_messages\": {},\n",
            "  \"parallel\": {},\n  \"context_runs\": [{}],\n  \"tiered_compilation\": {},\n",
            "  \"host_calls\": {},\n  \"native_entries\": {},\n  \"interpreter_entries\": {},\n  \"cpu_cliffs\": {},\n  \"tier_verification\": {},\n  \"direct_memory\": {},\n",
            "  \"compilations\": {},\n  \"async_compilation\": {},\n  \"compile_seconds\": {:.6},\n  \"compile_requests\": {},\n  \"pending_compilations\": {},\n  \"peak_pending_compilations\": {},\n  \"stale_compilations\": {},\n  \"compile_failures\": {},\n  \"guest_contexts\": {},\n  \"tls_images\": {},\n  \"loaded_modules\": [{}],\n",
            "  \"api\": {},\n  \"first_cpu_fault\": {},\n  \"latest_cpu_fault\": {},\n  \"startup\": {{\"loaded\": true, \"guest_windows\": {}}},\n",
            "  \"time_skip\": {{\"enabled\": {}, \"jumps\": {}, \"skipped_guest_us\": {}}},\n",
            "  \"passive_stalls\": {{\"clamps\": {}, \"discarded_guest_us\": {}, \"max_step_us\": {}}},\n",
            "  \"missing_apis\": [{}],\n  \"failure\": {},\n  \"screenshot_error\": {},\n  \"performance\": {},\n  \"audio\": {},\n  \"pcm\": {},\n  \"codec\": {},\n  \"codec_kernels\": {},\n  \"gpu\": {},\n  \"graphics_memory\": {},\n  \"scene_gate\": {},\n  \"runtime\": {}\n}}\n"
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
        parallel_json,
        context_runs,
        options.jit
            && options.async_compilation
            && options.tiered_compilation
            && options.guest_workers == 1,
        runtime.machine.sys.host_calls,
        stats.native_entries,
        stats.interpreter_entries,
        cliffs_report(&stats.cliffs),
        verification_report(&stats.verification),
        format!(
            "{{\"sites\":{},\"accesses\":{},\"recoveries\":{},\"violations\":{},\"checked_fallbacks\":{},\"code_writes\":{}}}",
            stats.direct_memory.sites,
            stats.direct_memory.accesses,
            stats.direct_memory.recoveries,
            stats.direct_memory.violations,
            stats.direct_memory.checked_fallbacks,
            stats.direct_memory.code_writes
        ),
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
        cpu_fault_json(runtime.first_cpu_fault()),
        cpu_fault_json(runtime.latest_cpu_fault()),
        runtime.guest_window_count(),
        options.time_skip,
        runtime.machine.sys.scheduling.active_time_skips,
        runtime.machine.sys.scheduling.active_time_skip_us,
        runtime.machine.sys.clock.passive_stalls().0,
        runtime.machine.sys.clock.passive_stalls().1,
        adv_machine::MAX_PASSIVE_STEP_US,
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
        codec_kernels,
        gpu_diagnostics,
        graphics_memory,
        scene_gate.json(),
        json(&runtime.report_with_profile(&runtime.machine.cpu.profile()))
    )
}

fn write_report(path: &std::path::Path, report: &str) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, report).map_err(|e| e.to_string())?;
    // ADV_LOOP_COVERAGE: the loop table goes beside the report.
    if let Some(loops) = adv_runtime::loop_coverage() {
        std::fs::write(path.with_extension("loops.tsv"), loops).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn capture_diagnostic_frame(runtime: &mut Runtime, wake: &window::WakeEvent) -> Result<(), String> {
    #[cfg(all(windows, feature = "gpu"))]
    if runtime.has_native_frame() {
        return capture_native_after_stop(runtime, wake);
    }
    let _ = wake;
    runtime.capture_latest_frame().map(|_| ())
}

fn acknowledge_scene_gate(
    completion: control::GateCompletion,
    runtime: &mut Runtime,
    keys: &adv_sync::input::KeyState,
    inputs: &mut u64,
    failure: &mut Option<String>,
) {
    let release = completion.release.map(|key| {
        keys.set(key, false);
        let posted = runtime.post_input(0x101, key, 0xc0000001);
        *inputs += u64::from(posted);
        (key, posted)
    });
    let result = match (completion.result, release) {
        (Ok(message), Some((key, true))) => Ok(format!(
            "{message} release={key} release_posted=true receipt={}",
            runtime.latest_input_receipt()
        )),
        (Ok(message), None) => Ok(format!("{message} release=none")),
        (Ok(message), Some((key, false))) => Err(format!(
            "{message} release={key} release_posted=false: no guest target window"
        )),
        (Err(message), release) => Err(format!(
            "{message} release={} release_posted={}",
            release.map_or_else(|| "none".into(), |(key, _)| key.to_string()),
            release.is_some_and(|(_, posted)| posted)
        )),
    };
    if let Err(error) = &result {
        *failure = Some(error.clone());
    }
    // The physical key was released at this frame boundary. Its queued guest
    // message is explicitly reported; a frozen guest has not dispatched it yet.
    control::acknowledge(result);
}

#[cfg(all(windows, feature = "gpu"))]
fn capture_native_after_stop(
    runtime: &mut Runtime,
    wake: &window::WakeEvent,
) -> Result<(), String> {
    use adv_sync::background::{Priority, Task};
    use std::sync::mpsc::TrySendError;
    // A report followed by a capture asks for the same frame again; its pixels
    // are already materialized and the guest has not presented since.
    if runtime.native_frame_serial() == Some(runtime.frame().frames) {
        return Ok(());
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    let (serial, mut ticket) = loop {
        match runtime.begin_native_capture() {
            Ok(Some(capture)) => break capture,
            Ok(None) => return Ok(()),
            Err(adv_device::gpu::Error::Busy) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err("native capture resource wait timed out".into());
                }
                wake.wait(Some(remaining)).map_err(|e| e.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    let sender = runtime.machine.cpu.background_sender()?;
    let mut task = Task::new(ticket.take_completion_job().unwrap())
        .finish_on_shutdown()
        .without_wake();
    loop {
        match sender.borrow_mut().try_submit(Priority::Urgent, task) {
            Ok(_) => break,
            Err(TrySendError::Full(returned)) => task = returned,
            Err(TrySendError::Disconnected(_)) => {
                ticket.cancel();
                return Err("native capture background service closed".into());
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            ticket.cancel();
            return Err("native capture queue wait timed out".into());
        }
        wake.wait(Some(remaining)).map_err(|e| e.to_string())?;
    }
    loop {
        if let Some(result) = ticket.poll() {
            let pixels = result.map_err(|e| e.to_string())?;
            if !runtime.finish_native_capture(serial, ticket.width, ticket.height, &pixels)? {
                return Err("native capture was superseded".into());
            }
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            ticket.cancel();
            return Err("native capture fence wait timed out".into());
        }
        wake.wait(Some(remaining)).map_err(|e| e.to_string())?;
    }
}

fn run(options: Options) -> Result<u8, String> {
    #[cfg(not(feature = "gpu"))]
    if options.gpu_software {
        return Err("software GPU diagnostics require a GPU-enabled build".into());
    }
    window::initialize_dpi()?;
    let wake = window::WakeEvent::new().map_err(|e| e.to_string())?;
    let keys = std::sync::Arc::new(adv_sync::input::KeyState::default());
    let mut background =
        adv_sync::background::Builder::new(wake.clone()).map_err(|error| error.to_string())?;
    let execution_sender = background
        .sender_with_capacity_wake(8, 8, wake.clone())
        .map_err(|error| error.to_string())?;
    if options.headless {
        let mut service = background.start().map_err(|error| error.to_string())?;
        let result = run_execution(options, None, wake, keys, execution_sender);
        service.shutdown();
        return result;
    }
    // The shared worker cannot preempt an active compiler task. Interactive
    // compilation uses measured work slices so presentation can run between
    // jobs; headless compilation above retains the unrestricted policy.
    background = background.with_work_policy(adv_sync::background::WorkPolicy::responsive(256));
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
    #[cfg(all(windows, feature = "gpu"))]
    let mut worker = worker;
    #[cfg(all(windows, feature = "gpu"))]
    {
        // Startup is the only UI-side GPU initialization. Guest submission has
        // not begun, so configuring the initially empty queue cannot race it.
        let (gpu, presenter) = window.create_native(options.gpu_software)?;
        let context = gpu.context().clone();
        context
            .set_execution_wake(wake.clone())
            .map_err(|_| "GPU execution wake was already registered")?;
        let ui_sender = background
            .sender_with_capacity_wake(1, 1, ui.wake())
            .map_err(|error| error.to_string())?;
        ui.native(presenter, ui_sender);
        worker.gpu = Some(gpu);
    }
    // All startup ports are registered before this one common worker starts.
    // Interpretation uses it as well, even when compilation is disabled.
    let mut service = background.start().map_err(|error| error.to_string())?;
    let completion_wake = ui.wake();
    let (mut tx, mut rx) = adv_sync::spsc::channel(1);
    let execution = std::thread::Builder::new()
        .name("adv-execution".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_execution(options, Some(worker), wake, keys, execution_sender)
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
    // Accepted presenter and fence jobs return/drop their owned resources while
    // HWND is still alive. The window is destroyed only after this join.
    service.shutdown();
    result?
}

fn run_execution(
    options: Options,
    mut window: Option<frontend::Worker>,
    wake: window::WakeEvent,
    keys: std::sync::Arc<adv_sync::input::KeyState>,
    background: adv_sync::background::Sender,
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
    runtime.set_codepage(options.codepage)?;
    for (drive, image) in &options.cdroms {
        runtime.mount_iso(*drive, image)?;
    }
    runtime
        .machine
        .cpu
        .set_guest_workers(options.guest_workers)?;
    if !options.row_batching || !options.jit || options.guest_workers > 1 {
        runtime.machine.cpu.disable_row_batching();
    } else if let Some(workers) = options.row_workers {
        runtime.machine.cpu.set_row_workers(workers)?;
    }
    if options.codec_kernels != adv_runtime::codec_kernels::Mode::Guest {
        use std::io::Read;
        let mut signatures = String::new();
        if let Some(path) = &options.codec_signatures {
            std::fs::File::open(path)
                .map_err(|e| format!("codec signatures {}: {e}", path.display()))?
                .take(1_048_577)
                .read_to_string(&mut signatures)
                .map_err(|e| format!("codec signatures {}: {e}", path.display()))?;
            if signatures.len() > 1_048_576 {
                return Err("codec signatures exceed 1 MiB".into());
            }
        } else {
            signatures.push_str(adv_runtime::codec_kernels::BUNDLED_SIGNATURES);
        }
        match runtime.install_codec_kernels(options.codec_kernels, &signatures) {
            Ok(installed) => eprintln!("codec kernels: {installed:?}"),
            // The default is an optimization: a binding is recorded only after
            // its detour is complete, so the guest path stays usable.
            Err(error) if !options.codec_kernels_explicit => {
                eprintln!("warning: codec kernels disabled: {error}")
            }
            Err(error) => return Err(error),
        }
    }
    runtime.set_api_mode(options.api_mode);
    runtime.machine.cpu.set_waker(wake.clone())?;
    runtime
        .machine
        .cpu
        .set_background_sender(background.into_local())?;
    if options.time_skip {
        let skip_keys = keys.clone();
        let pending_input = wake.interrupt_flag();
        runtime
            .machine
            .set_time_skip_source(Some(std::sync::Arc::new(move || {
                // Pending input/audio notifications must be drained before jumping.
                !pending_input.load(std::sync::atomic::Ordering::Acquire)
                    && [0x11, 0xa2, 0xa3]
                        .iter()
                        .any(|key| skip_keys.get(*key) & 0x80 != 0)
            })));
    }
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
        .executor_mut()
        .set_tiered_compilation(options.tiered_compilation);
    runtime
        .machine
        .cpu
        .set_region_linking(options.region_linking);
    runtime
        .machine
        .cpu
        .set_async_compilation(options.jit && options.async_compilation);
    // Guest regions resident at the previous exit are compiled in the background
    // as soon as identical code is mapped, before the guest reaches them.
    let warm_path = options.data_dir.join("jit-warm.bin");
    if options.jit && options.async_compilation {
        // Leave cores for the guest, presentation and audio threads.
        let threads = std::thread::available_parallelism().map_or(1, |cores| {
            cores
                .get()
                .saturating_sub(options.guest_workers + 1)
                .clamp(1, 3)
        });
        runtime
            .machine
            .cpu
            .executor_mut()
            .set_compile_threads(threads)?;
        if let Ok(profile) = std::fs::read(&warm_path) {
            runtime
                .machine
                .cpu
                .executor_mut()
                .load_warm_profile(&profile);
        }
    }
    runtime.set_frame_limit(options.frames);
    runtime.set_audio_enabled(options.audio);
    #[cfg(feature = "gpu")]
    runtime.set_d3d9_shader_caps(options.d3d9_sm2, options.d3d9_sm3);
    #[cfg(feature = "gpu")]
    runtime.set_d3d9_shader_cache(options.d3d9_shader_cache.clone());
    runtime.machine.cpu.set_profiling(options.profile);
    runtime.machine.sys.stats.services_enabled =
        options.host_profile.is_some() || options.schedule_profile.is_some();
    if let Some(path) = &options.schedule_profile {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        runtime.machine.sys.schedule_trace =
            adv_machine::schedule_trace::ScheduleTrace::to_writer(Box::new(
                std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?),
            ))
            .map_err(|e| e.to_string())?;
    }
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
        // Every call must reach the host to be traced.
        let machine = &mut runtime.machine;
        machine
            .sys
            .thunks
            .set_guest_code(&mut machine.cpu, false)
            .map_err(|e| format!("{e:?}"))?;
    }
    runtime.set_host_keys(keys.clone());
    if let Some(worker) = window.as_mut() {
        let producer = worker.take_frames();
        runtime.set_frame_mailbox(producer, worker.ui_wake.clone());
    }
    #[cfg(all(windows, feature = "gpu"))]
    {
        let gpu = match window.as_mut().and_then(|worker| worker.gpu.take()) {
            Some(gpu) => gpu,
            None => {
                let gpu = pollster::block_on(adv_device::gpu::Gpu::request(options.gpu_software))
                    .map_err(|e| e.to_string())?;
                gpu.context()
                    .set_execution_wake(wake.clone())
                    .map_err(|_| "GPU wake already installed")?;
                runtime.set_deferred_presentation(true)?;
                gpu
            }
        };
        runtime.install_gpu(gpu)?;
        let gpu_background = runtime.machine.cpu.background_sender()?;
        runtime.set_gpu_background(gpu_background, wake.clone());
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
    let mut scene_gate = control::SceneGate::default();
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
                    window::Event::Menu(event) => {
                        inputs += u64::from(runtime.post_menu_event(event))
                    }
                    window::Event::Dialog(event) => {
                        inputs += u64::from(runtime.post_dialog_event(event));
                    }
                }
            }
        }
        if let Some(controller) = control.as_mut() {
            let commands = if scene_gate.frozen() {
                controller.drain_frozen(true)
            } else {
                controller.drain()
            };
            for command in commands {
                let receipt_before = runtime.latest_input_receipt();
                let result = match command {
                    control::Command::Dialogs => runtime
                        .dialogs()
                        .map(|set| format!("dialogs {}", json(&format!("{set:?}")))),
                    control::Command::Dialog(id) | control::Command::DialogTrack(id, _) => {
                        let set = runtime.dialogs()?;
                        let found: Vec<_> = set
                            .dialogs
                            .iter()
                            .filter(|d| d.visible && d.enabled)
                            .flat_map(|d| d.controls.iter())
                            .filter(|c| c.target.id == id && c.visible && c.enabled)
                            .map(|c| c.target)
                            .collect();
                        if found.len() == 1
                            && runtime.post_dialog_event(match command {
                                control::Command::DialogTrack(_, value) => {
                                    adv_runtime::DialogEvent::Track {
                                        target: found[0],
                                        value,
                                        code: 4,
                                    }
                                }
                                _ => adv_runtime::DialogEvent::Click(found[0]),
                            })
                        {
                            inputs += 1;
                            Ok(format!("dialog {id}"))
                        } else {
                            Err(format!(
                                "dialog control {id} is missing, ambiguous or disabled"
                            ))
                        }
                    }
                    control::Command::DialogClose => {
                        let set = runtime.dialogs()?;
                        let found: Vec<_> = set
                            .dialogs
                            .iter()
                            .filter(|d| d.visible && d.enabled)
                            .map(|d| d.target)
                            .collect();
                        if found.len() == 1
                            && runtime.post_dialog_event(adv_runtime::DialogEvent::Close(found[0]))
                        {
                            inputs += 1;
                            Ok("dialog-close".into())
                        } else {
                            Err("dialog-close requires one visible enabled dialog".into())
                        }
                    }
                    control::Command::Menus => runtime.menu_bar().map(|bar| {
                        fn visit(
                            items: &[adv_runtime::MenuItem],
                            path: &str,
                            rows: &mut Vec<String>,
                        ) {
                            for item in items {
                                let label =
                                    format!("{path}/{}", String::from_utf16_lossy(&item.text));
                                if item.children.is_empty() {
                                    rows.push(format!(
                                        "{}:{}:{}",
                                        item.command.id, item.flags, label
                                    ));
                                } else {
                                    visit(&item.children, &label, rows);
                                }
                            }
                        }
                        let mut rows = Vec::new();
                        visit(&bar.items, "", &mut rows);
                        format!(
                            "menus enabled={} {}",
                            bar.key.enabled,
                            json(&rows.join(" | "))
                        )
                    }),
                    control::Command::Menu(id) => {
                        fn find(
                            items: &[adv_runtime::MenuItem],
                            id: u32,
                            found: &mut Vec<adv_runtime::MenuCommand>,
                        ) {
                            for item in items {
                                if item.children.is_empty() && item.command.id == id {
                                    found.push(item.command);
                                }
                                find(&item.children, id, found);
                            }
                        }
                        let mut found = Vec::new();
                        let bar = runtime.menu_bar()?;
                        find(&bar.items, id, &mut found);
                        if found.len() == 1
                            && runtime.post_menu_event(adv_runtime::MenuEvent::Command(found[0]))
                        {
                            inputs += 1;
                            Ok(format!("menu {id}"))
                        } else {
                            Err(format!(
                                "menu {id} is missing, ambiguous, detached or disabled"
                            ))
                        }
                    }
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
                    control::Command::MouseDown(x, y) | control::Command::MouseUp(x, y) => {
                        let pressed = matches!(command, control::Command::MouseDown(..));
                        let position = u32::from(x as u16) | (u32::from(y as u16) << 16);
                        let moved = runtime.post_input(0x200, u32::from(pressed), position);
                        let posted = runtime.post_input(
                            if pressed { 0x201 } else { 0x202 },
                            u32::from(pressed),
                            position,
                        );
                        inputs += u64::from(moved) + u64::from(posted);
                        if moved && posted {
                            Ok(format!(
                                "mouse-{} {x} {y}",
                                if pressed { "down" } else { "up" }
                            ))
                        } else {
                            Err("mouse button has no guest target window".into())
                        }
                    }
                    control::Command::Move(x, y) => {
                        let position = u32::from(x as u16) | (u32::from(y as u16) << 16);
                        if runtime.post_input(0x200, 0, position) {
                            inputs += 1;
                            Ok(format!("move {x} {y}"))
                        } else {
                            Err("mouse move has no guest target window".into())
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
                    control::Command::Memory(address, len) => {
                        runtime.inspect_memory(address, len).map(|bytes| {
                            let hex = bytes
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<Vec<_>>()
                                .join("");
                            format!("memory {address:#010x} {hex}")
                        })
                    }
                    control::Command::Capture(path) => {
                        capture_diagnostic_frame(&mut runtime, &wake)?;
                        save_frame(&path, runtime.frame())
                            .map(|_| format!("capture {}", path.display()))
                    }
                    control::Command::Report(path) => {
                        runtime.machine.sys.schedule_trace.event(
                            "observe",
                            0,
                            runtime.presented_frames(),
                            0,
                            0,
                        );
                        capture_diagnostic_frame(&mut runtime, &wake)?;
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
                            failure: failure.as_deref(),
                            screenshot_error: None,
                            performance: &performance,
                            audio: &audio,
                            scene_gate: &scene_gate,
                        });
                        write_report(&path, &snapshot).map(|_| format!("report {}", path.display()))
                    }
                    control::Command::ResetMetrics => {
                        runtime.machine.sys.schedule_trace.event("mark", 0, 0, 0, 0);
                        let serial = runtime.presented_frames();
                        performance.reset(serial);
                        Ok(format!("reset-metrics start_frame={serial}"))
                    }
                    control::Command::AwaitFrame(spec) => {
                        if !options.headless || options.guest_workers != 1 {
                            Err(
                                "await-frame requires headless execution with one guest worker"
                                    .into(),
                            )
                        } else {
                            let hold = spec.hold;
                            if hold.is_some() && runtime.guest_window_count() == 0 {
                                Err("await-frame hold has no guest target window".into())
                            } else {
                                match scene_gate.arm(
                                    spec,
                                    runtime.presented_frames(),
                                    Instant::now(),
                                ) {
                                    Err(error) => Err(error),
                                    Ok(()) => {
                                        // Arm before publishing the physical held state. Guest
                                        // execution resumes only after this command returns.
                                        if let Some(key) = hold {
                                            keys.set(key, true);
                                            let posted = runtime.post_input(0x100, key, 1);
                                            inputs += u64::from(posted);
                                            if !posted
                                                && let Some(completion) = scene_gate.abort(
                                                    "hold has no guest target window",
                                                    runtime.presented_frames(),
                                                    Instant::now(),
                                                )
                                            {
                                                acknowledge_scene_gate(
                                                    completion,
                                                    &mut runtime,
                                                    &keys,
                                                    &mut inputs,
                                                    &mut failure,
                                                );
                                            }
                                            controller
                                                .note_input_receipt(runtime.latest_input_receipt());
                                        }
                                        continue; // Deferred ACK belongs to the actual latch/timeout.
                                    }
                                }
                            }
                        }
                    }
                    control::Command::Resume => {
                        scene_gate.resume(Instant::now()).map(|_| "resume".into())
                    }
                    control::Command::Close => {
                        scene_gate.close(Instant::now());
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
        if let Some(completion) = scene_gate.observe(
            runtime.presented_frames(),
            runtime.frame().width,
            &runtime.frame().bgra,
            Instant::now(),
        ) {
            acknowledge_scene_gate(completion, &mut runtime, &keys, &mut inputs, &mut failure);
        }
        if options
            .seconds
            .is_some_and(|limit| started.elapsed().as_secs_f64() >= limit)
        {
            if let Some(completion) = scene_gate.abort(
                "session time limit",
                runtime.presented_frames(),
                Instant::now(),
            ) {
                acknowledge_scene_gate(completion, &mut runtime, &keys, &mut inputs, &mut failure);
            }
            stop_reason = "time_limit";
            break;
        }
        if close_at.is_some_and(|at: Instant| at.elapsed() >= Duration::from_secs(2)) {
            stop_reason = "host_window_closed";
            break;
        }
        if scene_gate.frozen() {
            // Input is queued while frozen. Diagnostics share the same retained
            // wake event and remain responsive without running another guest frame.
            if !control.as_ref().is_some_and(control::Control::has_pending) {
                wake.wait(Some(Duration::from_millis(50)))
                    .map_err(|e| e.to_string())?;
            }
            continue;
        }
        let time_remaining = options
            .seconds
            .map(|seconds| Duration::from_secs_f64(seconds).saturating_sub(started.elapsed()));
        let close_remaining =
            close_at.map(|at| Duration::from_secs(2).saturating_sub(at.elapsed()));
        let slice = [time_remaining, close_remaining]
            .into_iter()
            .flatten()
            .min()
            .map_or(SOFT_SLICE, |remaining| remaining.min(SOFT_SLICE));
        // A host deadline yields to the shell; it is not a guest failure or a
        // process exit. Machine's soft slice already bounds CPU execution and
        // collects leases without publishing a terminal process status.
        let step_started = Instant::now();
        let interrupts_before = runtime.machine.sys.scheduling.external_interrupts;
        let frame_yields_before = runtime.machine.sys.scheduling.frame_host_yields;
        let outcome = runtime.step_for(slice);
        // Publish before an Idle wait: an engine may finish constructing its
        // menu and then wait indefinitely for the very first UI command.
        if let Some(worker) = window.as_mut()
            && worker.needs_menu(runtime.menu_key())
        {
            worker.publish_menu(runtime.menu_bar()?);
        }
        if let Some(worker) = window.as_mut() {
            worker.publish_dialogs(runtime.dialogs()?);
        }
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
        if matches!(
            &outcome,
            SliceResult::Complete(RunResult::Exited(_) | RunResult::Failed(_))
        ) && let Some(completion) =
            scene_gate.abort("guest stopped before target", presented, Instant::now())
        {
            acknowledge_scene_gate(completion, &mut runtime, &keys, &mut inputs, &mut failure);
        }
        if let Some(completion) = scene_gate.observe(
            presented,
            runtime.frame().width,
            &runtime.frame().bgra,
            Instant::now(),
        ) {
            acknowledge_scene_gate(completion, &mut runtime, &keys, &mut inputs, &mut failure);
        }
        if first_frame_seconds.is_none() && presented > 0 {
            first_frame_seconds = runtime
                .first_frame_time()
                .map(|at| at.saturating_duration_since(started).as_secs_f64());
        }
        if scene_gate.frozen() && matches!(&outcome, SliceResult::Complete(RunResult::Idle { .. }))
        {
            continue;
        }
        let outcome = match outcome {
            SliceResult::Yielded => {
                performance.soft_yield();
                continue;
            }
            SliceResult::Complete(outcome) => outcome,
        };
        match outcome {
            RunResult::HostRequest => unreachable!("runtime services process requests internally"),
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
                failure = Some(format!("{error}\n{}", error.recent.join("\n")));
                stop_reason = "guest_failure";
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
                if let Some(remaining) = scene_gate.remaining(Instant::now()) {
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
    #[cfg(feature = "gpu")]
    runtime.cancel_pending_graphics();
    runtime.machine.sys.trace.flush();
    if options.profile {
        let mut entries: Vec<_> = runtime.machine.cpu.profile().into_iter().collect();
        entries.sort_unstable_by_key(|(_, sample)| {
            std::cmp::Reverse(sample.native_ns.saturating_add(sample.interpreter_ns))
        });
        if let Some(report) = &options.report {
            use std::io::Write;
            let path = report.with_extension("cpu.tsv");
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut output =
                std::io::BufWriter::new(std::fs::File::create(&path).map_err(|e| e.to_string())?);
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
    #[cfg(all(windows, feature = "gpu"))]
    let native_frame = runtime.has_native_frame();
    #[cfg(not(all(windows, feature = "gpu")))]
    let native_frame = false;
    if options.jit && options.async_compilation {
        let profile = runtime.machine.cpu.executor().warm_profile();
        let _ = std::fs::create_dir_all(&options.data_dir)
            .and_then(|_| std::fs::write(&warm_path, profile));
    }
    if native_frame {
        #[cfg(all(windows, feature = "gpu"))]
        if options.screenshot.is_some() || options.report.is_some() {
            capture_native_after_stop(&mut runtime, &wake)?;
        }
    } else {
        runtime.capture_latest_frame()?;
    }
    let frame = runtime.frame();
    let screenshot_error = options
        .screenshot
        .as_ref()
        .and_then(|path| save_frame(path, frame).err());
    runtime
        .machine
        .sys
        .schedule_trace
        .flush()
        .map_err(|e| e.to_string())?;
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
        scene_gate: &scene_gate,
    });
    if let Some(path) = &options.report {
        write_report(path, &report)?;
    }
    if let Some(path) = &options.host_profile {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file =
            std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
        runtime
            .machine
            .sys
            .stats
            .write_host_csv(&runtime.machine.sys.thunks, &mut file)
            .map_err(|e| e.to_string())?;
        std::io::Write::flush(&mut file).map_err(|e| e.to_string())?;
    }
    let stats = runtime.machine.cpu.stats();
    let parallel = runtime.machine.cpu.parallel_stats();
    let parallel_json = parallel_report(&parallel);
    println!(
        "{stop_reason}: exit={} frames={} elapsed={:.3}s parallel={} host_calls={} native_entries={} interpreter_entries={} idle_waits={}",
        exit_code.map_or_else(|| "none".into(), |code| code.to_string()),
        runtime.presented_frames(),
        started.elapsed().as_secs_f64(),
        parallel_json,
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

/// Run the desktop/headless command-line shell with the process arguments.
pub fn run_cli() -> std::process::ExitCode {
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

/// Wall-time overlap includes the inline lane's API work; it is not CPU usage.
/// `ADV_JIT_VERIFY` interpreter checks of newly published native code.
fn verification_report(v: &adv_runtime::VerifyStats) -> String {
    format!(
        "{{\"verified\":{},\"inconclusive\":{},\"mismatches\":{},\"mxcsr_flags\":{},\"reports\":[{}]}}",
        v.verified,
        v.inconclusive,
        v.mismatches,
        v.mxcsr_flags,
        v.reports
            .iter()
            .map(|r| json(r))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Slow-path counters, each list cut to its most frequent entries.
fn cliffs_report(c: &adv_runtime::Cliffs) -> String {
    const ROWS: usize = 24;
    let counts = |rows: Vec<(String, u64)>, limit: usize| {
        rows.into_iter()
            .take(limit)
            .map(|(key, n)| format!("{}:{n}", json(&key)))
            .collect::<Vec<_>>()
            .join(",")
    };
    let interpreter = c
        .interpreter_rows()
        .into_iter()
        .map(|(reason, n)| {
            format!(
                "{}:{{\"entries\":{},\"instructions\":{}}}",
                json(&reason),
                n.entries,
                n.instructions
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let detail_rows = c.interpreter_detail_rows();
    let omitted_rows = detail_rows.len().saturating_sub(ROWS);
    let details = detail_rows
        .iter()
        .take(ROWS)
        .map(|(label, n)| {
            format!(
                "{}:{{\"entries\":{},\"instructions\":{}}}",
                json(label),
                n.entries,
                n.instructions
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let omitted = ["Uncovered", "Rejected"]
        .map(|reason| {
            let mut entries = 0;
            let mut instructions = 0;
            for (label, n) in detail_rows.iter().skip(ROWS) {
                if label.starts_with(&format!("{reason}/")) {
                    entries += n.entries;
                    instructions += n.instructions;
                }
            }
            format!(
                "{}:{{\"entries\":{entries},\"instructions\":{instructions}}}",
                json(reason)
            )
        })
        .join(",");
    format!(
        "{{\"x87_helper_calls\":{},\"x87_fast\":{},\"x87_exact\":{{{}}},\"x87_control\":{{{}}},\"x87_control_rejections\":{{{}}},\"x87_pc64\":{{{}}},\"fallbacks\":{{{}}},\"interpreter\":{{{}}},\"interpreter_details\":{{{details}}},\"interpreter_details_omitted\":{{{omitted}}},\"interpreter_detail_rows_omitted\":{omitted_rows}}}",
        c.x87_helper_calls,
        c.x87_fast,
        counts(c.x87_exact_rows(), ROWS),
        counts(c.x87_control_rows(), usize::MAX),
        counts(c.x87_control_rejections(), usize::MAX),
        counts(c.x87_pc64_rows(), usize::MAX),
        counts(c.fallback_rows(), ROWS),
        interpreter
    )
}

fn parallel_report(p: &adv_runtime::ParallelStats) -> String {
    format!(
        "{{\"workers\":{},\"submitted\":{},\"completed\":{},\"worker_run_ns\":{},\"queue_wait_ns\":{},\"queue_wait_max_ns\":{},\"image_builds\":{},\"image_build_ns\":{},\"empty_jobs\":{},\"short_traps\":{},\"overlap_ns\":{},\"max_running\":{},\"traps\":{},\"cold_retries\":{},\"cache_misses\":{},\"quantum_returns\":{},\"quiescence_waits\":{},\"quiescence_wait_ns\":{},\"quiescence_wait_max_ns\":{}}}",
        p.workers,
        p.submitted,
        p.completed,
        p.worker_run_ns,
        p.queue_wait_ns,
        p.queue_wait_max_ns,
        p.image_builds,
        p.image_build_ns,
        p.empty_jobs,
        p.short_traps,
        p.overlap_ns,
        p.max_running,
        p.traps,
        p.cold_retries,
        p.cache_misses,
        p.quantum_returns,
        p.quiescence_waits,
        p.quiescence_wait_ns,
        p.quiescence_wait_max_ns
    )
}
