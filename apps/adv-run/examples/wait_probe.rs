//! Print measured host waits without timing assertions or changing guest time.
#[cfg(windows)]
use adv_shell::window;

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};
    let mut window = window::Window::new("bounded wait probe", 320, 240, true).unwrap();
    for milliseconds in [1, 5, 16] {
        let mut samples = Vec::new();
        for _ in 0..64 {
            window.pump();
            let started = Instant::now();
            window
                .wait(Some(Duration::from_millis(milliseconds)))
                .unwrap();
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "requested={milliseconds}ms mean={:.3}ms p50={:.3}ms p95={:.3}ms max={:.3}ms samples={}",
            samples.iter().sum::<f64>() / samples.len() as f64,
            samples[samples.len() / 2],
            samples[(samples.len() * 95 / 100).min(samples.len() - 1)],
            samples.last().unwrap(),
            samples.len()
        );
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("native Windows wait probe requires Windows");
}
