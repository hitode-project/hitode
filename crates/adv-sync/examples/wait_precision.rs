//! Usage: cargo run -p adv-sync --release --example wait_precision.
//! Inputs: none. Outputs: aggregate host deadline samples
//! in microseconds, without guest code, assets, addresses or timing assertions.
use adv_sync::WakeEvent;
use std::time::{Duration, Instant};

fn main() {
    let wake = WakeEvent::new().unwrap();
    for requested_us in [250, 1000, 5000, 16667, 50000] {
        let mut samples = Vec::new();
        for _ in 0..64 {
            let start = Instant::now();
            let delay = Some(Duration::from_micros(requested_us));
            let signaled = wake.wait(delay).unwrap();
            assert!(!signaled);
            samples.push(start.elapsed().as_micros() as u64);
        }
        samples.sort_unstable();
        println!(
            "{{\"requested_us\":{requested_us},\"count\":{},\"mean_us\":{:.3},\"p50_us\":{},\"p95_us\":{},\"p99_us\":{},\"max_us\":{}}}",
            samples.len(),
            samples.iter().sum::<u64>() as f64 / samples.len() as f64,
            samples[31],
            samples[60],
            samples[63],
            samples[63],
        );
    }
}
