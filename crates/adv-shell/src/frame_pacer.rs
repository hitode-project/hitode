//! The host frame clock behind guest frame spins (PROC-CLOCK-4).
use adv_sync::{input::KeyState, vsync::Vsync};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Frames reach the display at most once per 1/60 s (HOST-SES-8).
const PERIOD: Duration = Duration::from_nanos(16_666_667);

pub(crate) struct HostPacer {
    /// The display's grid where the host publishes one; else a 60 Hz grid.
    pub vsync: Option<Arc<Vsync>>,
    pub keys: Arc<KeyState>,
    pub origin: Instant,
}

impl adv_machine::frame_spin::FramePacer for HostPacer {
    fn until_vsync(&self) -> Option<Duration> {
        let now = Instant::now();
        let (since, period) = self
            .vsync
            .as_ref()
            .and_then(|vsync| vsync.phase(now))
            .unwrap_or_else(|| {
                let since = now.duration_since(self.origin).as_nanos() % PERIOD.as_nanos();
                (Duration::from_nanos(since as u64), PERIOD)
            });
        // Faster displays still get one frame per 1/60 s.
        let period = period
            * (PERIOD.as_nanos() as f64 / period.as_nanos().max(1) as f64)
                .round()
                .max(1.0) as u32;
        Some(period.saturating_sub(since))
    }

    fn input_held(&self) -> bool {
        self.keys.any_pressed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_machine::frame_spin::FramePacer;

    // spec: HOST-SES-8
    #[test]
    fn faster_displays_still_tick_once_per_sixtieth() {
        let vsync = Arc::new(Vsync::default());
        let pacer = HostPacer {
            vsync: Some(vsync.clone()),
            keys: Arc::default(),
            origin: Instant::now(),
        };
        for hz in [120.0, 144.0] {
            vsync.publish(Instant::now(), Duration::from_secs_f64(1.0 / hz));
            let wait = pacer.until_vsync().unwrap();
            assert!(
                wait > Duration::from_secs_f64(1.0 / hz),
                "{hz} Hz: {wait:?}"
            );
            assert!(wait <= PERIOD.mul_f64(1.1), "{hz} Hz: {wait:?}");
        }
    }
}
