//! Local-only deterministic sample measurement. No sample code or assets are embedded.
//! sample_bench <EXE> <jit|interpreter|unicorn> [frames=120] [codepage=932] [PNG] [--profile] [--compile fast|speed] [--sync-compilation]
#[path = "../tests/support/cpu.rs"]
mod backend;
use adv_cpu::CompileStrategy;
use adv_machine::{Clock, RunResult};
use adv_runtime::boot::Runtime;
use std::time::{Duration, Instant};

#[cfg(windows)]
fn pin() {
    use std::ffi::c_void;
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn SetProcessAffinityMask(process: *mut c_void, mask: usize) -> i32;
    }
    // SAFETY: the pseudo-handle refers to this process; CPU 0 is a valid affinity bit.
    assert_ne!(unsafe { SetProcessAffinityMask(GetCurrentProcess(), 1) }, 0);
}
#[cfg(not(windows))]
fn pin() {}

fn run() -> Result<(), String> {
    pin();
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let mut profile = false;
    let mut compile_strategy = CompileStrategy::Speed;
    let mut region_linking = true;
    let mut async_compilation = true;
    loop {
        if args.last().is_some_and(|arg| arg == "--sync-compilation") {
            async_compilation = false;
            args.pop();
        } else if args.last().is_some_and(|arg| arg == "--no-region-linking") {
            region_linking = false;
            args.pop();
        } else if args.last().is_some_and(|arg| arg == "--profile") {
            profile = true;
            args.pop();
        } else if args.len() >= 2 && args[args.len() - 2] == "--compile" {
            compile_strategy = match args.pop().unwrap().as_str() {
                "fast" => CompileStrategy::Fast,
                "speed" => CompileStrategy::Speed,
                _ => return Err("compile strategy must be fast or speed".into()),
            };
            args.pop();
        } else {
            break;
        }
    }
    let exe = args
        .first()
        .ok_or("sample_bench <EXE> <jit|interpreter|unicorn> [frames=120] [codepage=932] [PNG]")?;
    let backend = args.get(1).ok_or("missing backend")?.as_str();
    let cpu = backend::Cpu::new(backend)?;
    let jit = backend == "jit";
    let limit: u64 = args
        .get(2)
        .map_or(Ok(120), |s| s.parse())
        .map_err(|_| "invalid frame count")?;
    let codepage = args
        .get(3)
        .map_or(Ok(932), |s| s.parse())
        .map_err(|_| "invalid codepage")?;
    if limit == 0 || !matches!(codepage, 932 | 936) {
        return Err("frames must be positive; codepage must be 932 or 936".into());
    }
    let wall_limit = Duration::from_secs(120);
    let start = Instant::now();
    let overlay = format!(
        "target/sample-bench-data/{}-{}",
        backend,
        std::process::id()
    );
    let mut runtime = Runtime::with_cpu(exe, overlay, cpu, 640, 480)?;
    runtime.set_codepage(codepage)?;
    runtime.set_auto_dialog_ms(Some(50));
    runtime.set_frame_limit(Some(limit));
    runtime.enable_frame_timing();
    runtime.machine.cpu.set_profiling(profile);
    runtime
        .machine
        .cpu
        .set_auto_compile_strategy(compile_strategy);
    runtime.machine.cpu.set_region_linking(region_linking);
    runtime
        .machine
        .cpu
        .set_async_compilation(jit && async_compilation);
    runtime.machine.sys.clock = Clock::virtual_time();
    let clock_start = runtime.machine.sys.clock.now_us();
    let mut first_frame = None;
    let mut steps = 0u64;
    loop {
        let left = wall_limit.saturating_sub(start.elapsed());
        if left.is_zero() {
            return Err("sample benchmark exceeded 120 seconds".into());
        }
        runtime.machine.set_execution_budget(Some(left));
        let outcome = runtime.step();
        if let RunResult::Failed(error) = &outcome {
            return Err(format!("{error}\n{}", runtime.report()));
        }
        steps += 1;
        if runtime.frame().frames > 0 && first_frame.is_none() {
            first_frame = runtime
                .first_frame_time()
                .map(|at| at.saturating_duration_since(start));
        }
        if runtime.frame().frames >= limit {
            break;
        }
        match outcome {
            RunResult::Idle {
                next_deadline: Some(at),
            } => runtime.machine.sys.clock.advance_to(at),
            RunResult::Idle {
                next_deadline: None,
            } => {
                return Err(format!(
                    "sample awaits external input at frame {}",
                    runtime.frame().frames
                ));
            }
            RunResult::Exited(code) => {
                return Err(format!(
                    "sample exited {code} at frame {}",
                    runtime.frame().frames
                ));
            }
            RunResult::HostRequest => return Err("process requests require the runtime family scheduler".into()),
            RunResult::Failed(error) => return Err(format!("{error}\n{}", runtime.report())),
        }
        if runtime.machine.sys.clock.now_us() - clock_start > 120_000_000 {
            return Err(format!(
                "sample did not reach {limit} frames within 120 guest seconds"
            ));
        }
    }
    let elapsed = start.elapsed();
    let frame = runtime.frame();
    let hash = frame.bgra.iter().fold(0xcbf29ce484222325u64, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    if let Some(path) = args.get(4) {
        let mut rgba = frame.bgra.clone();
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
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
            .map_err(|e| e.to_string())?;
    }
    let stats = runtime.machine.cpu.stats();
    let intervals = runtime.frame_intervals();
    let warm = &intervals[intervals.len().min(29)..];
    let mut times: Vec<_> = warm.iter().map(Duration::as_secs_f64).collect();
    times.sort_by(f64::total_cmp);
    let mean = times.iter().sum::<f64>() / times.len().max(1) as f64;
    let p95 = times
        .get((times.len() * 95 / 100).min(times.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0.0);
    println!(
        "backend,compile_strategy,frames,width,height,fnv1a64,wall_seconds,first_frame_seconds,guest_seconds,compile_seconds,native_entries,interpreter_entries,steps,warm_frames,warm_frame_mean_ms,warm_frame_p95_ms,warm_fps,compilations,native_code_bytes,region_linking,linked_entries,interpreter_instructions,async_compilation,pending_compilations,stale_compilations,compile_failures,peak_pending_compilations"
    );
    println!(
        "{},{},{},{},{},{hash:016x},{:.6},{:.6},{:.6},{:.6},{},{},{},{},{:.4},{:.4},{:.2},{},{},{},{},{},{},{},{},{},{}",
        backend,
        if !jit {
            "-"
        } else {
            match compile_strategy {
                CompileStrategy::Speed => "speed",
                CompileStrategy::Fast => "fast",
            }
        },
        frame.frames,
        frame.width,
        frame.height,
        elapsed.as_secs_f64(),
        first_frame.unwrap().as_secs_f64(),
        (runtime.machine.sys.clock.now_us() - clock_start) as f64 / 1e6,
        stats.compile_time.as_secs_f64(),
        stats.native_entries,
        stats.interpreter_entries,
        steps,
        times.len(),
        mean * 1000.0,
        p95 * 1000.0,
        if mean > 0.0 { 1.0 / mean } else { 0.0 },
        stats.compilations,
        stats.native_code_bytes,
        jit && region_linking && !profile,
        stats.linked_entries,
        stats.interpreter_instructions,
        jit && async_compilation,
        stats.pending_compilations,
        stats.stale_compilations,
        stats.compile_failures,
        stats.peak_pending_compilations
    );
    if profile {
        eprintln!(
            "{}",
            runtime.report_with_profile(&runtime.machine.cpu.profile())
        );
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
