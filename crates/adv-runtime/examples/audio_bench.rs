//! Hash the original guest decoder's first PCM buffer. Contains no sample resources.
//! audio_bench <EXE> <jit|interpreter|unicorn> [codepage=932] [--profile]
//! Diagnostic virtual time advances by at least 16,667 us per observed guest frame.
use adv_runtime::boot::Runtime;
#[path = "../tests/support/cpu.rs"]
mod backend;
use adv_machine::{Clock, RunResult, SliceResult};
use std::time::{Duration, Instant};

fn run() -> Result<(), String> {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let profile = args.iter().any(|arg| arg == "--profile");
    args.retain(|arg| arg != "--profile");
    let exe = args
        .first()
        .ok_or("audio_bench <EXE> <jit|interpreter|unicorn> [codepage=932]")?;
    let cpu = backend::Cpu::new(args.get(1).ok_or("missing backend")?)?;
    let codepage = args
        .get(2)
        .map_or(Ok(932), |s| s.parse())
        .map_err(|_| "invalid codepage")?;
    if !matches!(codepage, 932 | 936) || args.len() > 3 {
        return Err("invalid arguments".into());
    }
    let started = Instant::now();
    let limit = Duration::from_secs(120);
    let overlay = format!("target/audio-bench-data/{}-{}", args[1], std::process::id());
    let mut runtime = Runtime::with_cpu(exe, overlay, cpu, 640, 480)?;
    runtime.set_codepage(codepage)?;
    runtime.set_auto_dialog_ms(Some(50));
    runtime.set_audio_enabled(true);
    runtime.enable_audio_pcm_digests();
    runtime.machine.cpu.set_profiling(profile);
    runtime.machine.sys.clock = Clock::virtual_time();
    let mut frames = 0;
    loop {
        let left = limit.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err("audio decoder validation exceeded 120 seconds".into());
        }
        runtime.machine.set_execution_budget(Some(left));
        let outcome = runtime.step_for(Duration::from_millis(8));
        let presented = runtime.frame().frames;
        if presented > frames {
            let next = runtime.machine.sys.clock.now_us() + (presented - frames) * 16_667;
            runtime.machine.sys.clock.advance_to(next);
            frames = presented;
        }
        if let SliceResult::Complete(RunResult::Failed(error)) = &outcome {
            return Err(format!("{error}\n{}", runtime.report()));
        }
        if let Some(pcm) = runtime.audio_pcm_digests().first() {
            let stats = runtime.machine.cpu.stats();
            println!(
                "backend,wall_seconds,compile_seconds,frames,rate,channels,bits,bytes,pcm_fnv1a64,interpreter_entries,interpreter_instructions"
            );
            println!(
                "{},{:.6},{:.6},{},{},{},{},{},{:016x},{},{}",
                args[1],
                started.elapsed().as_secs_f64(),
                stats.compile_time.as_secs_f64(),
                runtime.frame().frames,
                pcm.sample_rate,
                pcm.channels,
                pcm.bits_per_sample,
                pcm.bytes,
                pcm.fnv1a64,
                stats.interpreter_entries,
                stats.interpreter_instructions
            );
            if profile {
                eprintln!(
                    "{}",
                    runtime.report_with_profile(&runtime.machine.cpu.profile())
                );
            }
            return Ok(());
        }
        match outcome {
            SliceResult::Yielded => {}
            SliceResult::Complete(RunResult::Idle {
                next_deadline: Some(deadline),
            }) => runtime.machine.sys.clock.advance_to(deadline),
            SliceResult::Complete(RunResult::Idle {
                next_deadline: None,
            }) => return Err(format!("blocked before decoded PCM\n{}", runtime.report())),
            SliceResult::Complete(RunResult::Exited(code)) => {
                return Err(format!("guest exited {code} before decoded PCM"));
            }
            SliceResult::Complete(RunResult::Failed(_)) => unreachable!(),
        }
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
