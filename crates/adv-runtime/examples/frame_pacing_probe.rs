//! Headless guest/scheduler diagnostics, not a native-window pacing benchmark.
//! Usage: frame_pacing_probe EXE OVERLAY [QUANTUM_US=1000|0] [SECONDS=30]
//!                          [CODEPAGE=932] [--audio]
//! OVERLAY must be a dedicated local diagnostic directory. No report is uploaded.
use adv_machine::{RunResult, SliceResult};
use adv_runtime::boot::Runtime;
use std::time::{Duration, Instant};

fn run() -> Result<(), String> {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let audio = args.iter().any(|a| a == "--audio");
    args.retain(|a| a != "--audio");
    if !(2..=5).contains(&args.len()) {
        return Err("usage: frame_pacing_probe EXE OVERLAY [QUANTUM_US=1000|0] [SECONDS=30] [CODEPAGE=932] [--audio]".into());
    }
    let number = |index: usize, default: u64| -> Result<u64, String> {
        args.get(index).map_or(Ok(default), |s| {
            s.parse().map_err(|_| format!("invalid integer: {s}"))
        })
    };
    let quantum_us = number(2, 1000)?;
    let seconds = number(3, 30)?;
    let codepage = number(4, 932)?;
    if seconds == 0 || seconds > 300 || quantum_us > 100_000 || ![932, 936].contains(&codepage) {
        return Err("SECONDS must be 1..=300; QUANTUM_US 0..=100000; CODEPAGE 932 or 936".into());
    }
    let startup = Instant::now();
    let mut runtime = Runtime::new(&args[0], &args[1], true, 640, 480)?;
    let startup_ms = startup.elapsed().as_secs_f64() * 1000.0;
    runtime.set_codepage(codepage as u32);
    runtime.set_audio_enabled(audio);
    runtime.enable_frame_timing();
    runtime
        .machine
        .set_scheduling_quantum((quantum_us != 0).then_some(Duration::from_micros(quantum_us)))
        .map_err(str::to_owned)?;
    runtime
        .machine
        .set_execution_budget(Some(Duration::from_secs(seconds + 1)));
    let limit = Duration::from_secs(seconds);
    let started = Instant::now();
    let mut active = Duration::ZERO;
    let mut waited = Duration::ZERO;
    let mut max_step = Duration::ZERO;
    let mut failure = None;
    let stop;
    loop {
        let remaining = limit.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            stop = "wall-time limit";
            break;
        }
        let step = Instant::now();
        let outcome = runtime.step_for(remaining.min(Duration::from_millis(8)));
        let elapsed = step.elapsed();
        active += elapsed;
        max_step = max_step.max(elapsed);
        match outcome {
            SliceResult::Yielded => {}
            SliceResult::Complete(RunResult::Idle { next_deadline }) => {
                let requested = next_deadline
                    .map_or(Duration::from_millis(10), |deadline| {
                        Duration::from_micros(
                            deadline.saturating_sub(runtime.machine.sys.clock.now_us()),
                        )
                    })
                    .min(limit.saturating_sub(started.elapsed()));
                let before = Instant::now();
                std::thread::sleep(requested);
                waited += before.elapsed();
            }
            SliceResult::Complete(RunResult::Exited(_)) => {
                stop = "guest exit";
                break;
            }
            SliceResult::Complete(RunResult::Failed(error)) => {
                failure = Some(error.to_string());
                stop = "guest failure";
                break;
            }
        }
    }
    let mut intervals: Vec<_> = runtime
        .frame_intervals()
        .iter()
        .map(|d| d.as_secs_f64() * 1000.0)
        .collect();
    intervals.sort_by(f64::total_cmp);
    let percentile = |p: usize| -> Option<f64> {
        let rank = (intervals.len() * p).div_ceil(100).saturating_sub(1);
        intervals.get(rank).copied()
    };
    println!(
        "mode=headless_guest_diagnostic stop={stop} quantum_us={quantum_us} audio_simulated={audio}"
    );
    println!(
        "startup_ms={startup_ms:.3} elapsed_ms={:.3} frames={} interval_samples={}",
        started.elapsed().as_secs_f64() * 1000.0,
        runtime.frame().frames,
        intervals.len()
    );
    println!(
        "guest_frame_interval_ms p50={:?} p95={:?} p99={:?} max={:?}",
        percentile(50),
        percentile(95),
        percentile(99),
        intervals.last()
    );
    println!(
        "runtime_active_ms={:.3} host_wait_ms={:.3} max_step_ms={:.3}",
        active.as_secs_f64() * 1000.0,
        waited.as_secs_f64() * 1000.0,
        max_step.as_secs_f64() * 1000.0
    );
    println!("scheduler={:?}", runtime.machine.sys.scheduling);
    println!("execution={:?}", runtime.machine.cpu.stats());
    println!(
        "NOTE: headless sleeps use std::thread::sleep, not the native Windows waitable timer. Validate actual window pacing with adv-run --hidden --report. No input was injected; idle menus are not a throughput test."
    );
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
