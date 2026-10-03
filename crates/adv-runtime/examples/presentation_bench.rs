#[path = "support/pipeline.rs"]
mod pipeline;
use adv_cpu::RunOutcome;
use pipeline::*;
use std::time::Instant;

#[cfg(windows)]
fn pin() {
    use std::ffi::c_void;
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn SetProcessAffinityMask(p: *mut c_void, mask: usize) -> i32;
    }
    assert_ne!(unsafe { SetProcessAffinityMask(GetCurrentProcess(), 1) }, 0);
}
#[cfg(not(windows))]
fn pin() {}
fn run(f: &mut Fixture) {
    f.machine.state.eip = f.frame_pc;
    f.machine.state.regs[4] = STACK;
    assert_eq!(
        f.machine
            .run_with_services(STOP, 10_000_000, &mut f.graphics)
            .unwrap(),
        RunOutcome::ReachedStop
    );
}
fn main() {
    let samples: usize = std::env::args().nth(1).map_or(9, |s| s.parse().unwrap());
    assert!(samples > 0);
    pin();
    println!(
        "pipeline,resolution,frame_ms,api_calls_per_frame,native_entries_per_frame,interpreter_entries_per_frame,copies_after_render"
    );
    for (w, h) in [(1280, 720), (1920, 1080)] {
        for ddraw in [false, true] {
            let mut f = fixture(ddraw, w, h, false);
            assert_eq!(
                f.machine
                    .run_with_services(STOP, 10_000_000, &mut f.graphics)
                    .unwrap(),
                RunOutcome::ReachedStop
            );
            assert_eq!(f.graphics.presenter.bgra, f.expected);
            for _ in 0..3 {
                run(&mut f);
            }
            let calls = f.machine.stats.host_calls;
            let entries = f.machine.stats.native_entries;
            let interpreted = f.machine.stats.interpreter_entries;
            let mut times = Vec::new();
            for _ in 0..samples {
                let start = Instant::now();
                run(&mut f);
                times.push(start.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(f.graphics.presenter.bgra, f.expected);
            }
            times.sort_by(f64::total_cmp);
            assert_eq!(f.machine.stats.native_entries - entries, samples as u64);
            assert_eq!(f.machine.stats.interpreter_entries, 0);
            println!(
                "{},{w}x{h},{:.4},{},1,{},2",
                if ddraw { "ddraw" } else { "gdi" },
                times[samples / 2],
                (f.machine.stats.host_calls - calls) / samples as u64,
                (f.machine.stats.interpreter_entries - interpreted) / samples as u64
            );
        }
    }
}
