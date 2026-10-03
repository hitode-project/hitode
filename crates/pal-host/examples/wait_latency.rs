//! Windows host deadline probe. Does not change global timer resolution.
#[cfg(windows)]
fn main() {
    use pal_host::WakeEvent;
    use std::{
        ffi::c_void,
        time::{Duration, Instant},
    };
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn WaitForSingleObject(event: *mut c_void, milliseconds: u32) -> u32;
    }
    fn report(path: &str, millis: u32, mut overruns: Vec<f64>) {
        overruns.sort_by(f64::total_cmp);
        let count = overruns.len();
        println!(
            "{{\"path\":\"{path}\",\"requested_ms\":{millis},\"samples\":{count},\"overshoot_mean_ms\":{:.6},\"overshoot_p50_ms\":{:.6},\"overshoot_p95_ms\":{:.6},\"overshoot_max_ms\":{:.6}}}",
            overruns.iter().sum::<f64>() / count as f64,
            overruns[count / 2],
            overruns[(count * 95 / 100).min(count - 1)],
            overruns[count - 1]
        );
    }
    let wake = WakeEvent::new().unwrap();
    // Warm up the thread-owned timer before recording deadline behavior.
    assert!(!wake.wait(Some(Duration::from_millis(1))).unwrap());
    for millis in [1, 5, 16, 33] {
        let duration = Duration::from_millis(u64::from(millis));
        let mut legacy = Vec::new();
        let mut current = Vec::new();
        for _ in 0..31 {
            let started = Instant::now();
            // The event stays alive and unsignaled throughout this single wait.
            assert_eq!(unsafe { WaitForSingleObject(wake.as_raw_handle(), millis) }, 258);
            legacy.push(started.elapsed().saturating_sub(duration).as_secs_f64() * 1000.0);
            let started = Instant::now();
            assert!(!wake.wait(Some(duration)).unwrap());
            current.push(started.elapsed().saturating_sub(duration).as_secs_f64() * 1000.0);
        }
        report("legacy_event_timeout", millis, legacy);
        report("deadline_timer", millis, current);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This probe compares Windows event timeouts with waitable timer deadlines.");
}
