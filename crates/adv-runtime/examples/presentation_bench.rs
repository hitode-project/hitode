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
    assert_eq!(f.run_frame().unwrap(), RunOutcome::ReachedStop);
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
            assert_eq!(f.run().unwrap(), RunOutcome::ReachedStop);
            assert_eq!(f.graphics().presenter.bgra, f.expected);
            for _ in 0..3 {
                run(&mut f);
            }
            let calls = f.machine.sys.host_calls;
            let entries = f.machine.cpu.stats().native_entries;
            let interpreted = f.machine.cpu.stats().interpreter_entries;
            let mut times = Vec::new();
            for _ in 0..samples {
                let start = Instant::now();
                run(&mut f);
                times.push(start.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(f.graphics().presenter.bgra, f.expected);
            }
            times.sort_by(f64::total_cmp);
            assert!(f.machine.cpu.stats().native_entries > entries);
            println!(
                "{},{w}x{h},{:.4},{},{},{},2",
                if ddraw { "ddraw" } else { "gdi" },
                times[samples / 2],
                (f.machine.sys.host_calls - calls) / samples as u64,
                (f.machine.cpu.stats().native_entries - entries) / samples as u64,
                (f.machine.cpu.stats().interpreter_entries - interpreted) / samples as u64
            );
        }
    }
}
