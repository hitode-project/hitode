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

impl HostPacer {
    /// Wait time from `now` until the next 60 Hz presentation vsync (HOST-SES-8).
    fn until_vsync_at(&self, now: Instant) -> Duration {
        let (since, period) = self
            .vsync
            .as_ref()
            .and_then(|vsync| vsync.phase(now))
            .unwrap_or_else(|| {
                let since =
                    now.saturating_duration_since(self.origin).as_nanos() % PERIOD.as_nanos();
                (Duration::from_nanos(since as u64), PERIOD)
            });
        let period_ns = period.as_nanos().max(1);
        let rate = 1e9 / period_ns as f64;
        // Slower or ~60 Hz panels pace on every host vsync.
        if rate <= 60.0 * 1.01 {
            return period.saturating_sub(since);
        }
        // Whole multiples within 1% stay locked to exact k-vsync steps.
        let whole = (rate / 60.0).round();
        if whole >= 2.0 && (rate - whole * 60.0).abs() <= 0.01 * rate {
            return (period * whole as u32).saturating_sub(since);
        }
        // Non-integer faster panels snap the rational 60 Hz cadence anchored at
        // `self.origin` to the nearest host vsync strictly after `now`, without
        // repaying missed frames (consistent with GFX-PACE-1 and RefreshPacer).
        let elapsed = now.saturating_duration_since(self.origin).as_nanos() as i128;
        let p = period_ns as i128;
        let v0 = elapsed - since.as_nanos() as i128;
        let base_slot = elapsed * 60 / 1_000_000_000;
        for slot in base_slot..=base_slot + 2 {
            let ideal = (slot * 1_000_000_000 + 30) / 60;
            // Shift-invariant round-half-up onto the host vsync grid.
            let step = (ideal - v0 + p / 2).div_euclid(p);
            let snapped = v0 + step * p;
            if snapped > elapsed {
                return Duration::from_nanos((snapped - elapsed) as u64);
            }
        }
        period.saturating_sub(since)
    }
}

impl adv_machine::frame_spin::FramePacer for HostPacer {
    fn until_vsync(&self) -> Option<Duration> {
        Some(self.until_vsync_at(Instant::now()))
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

    // spec: HOST-SES-8
    #[test]
    fn host_pacer_matches_sixty_hertz_across_integer_and_noninteger_panels() {
        let origin = Instant::now();
        for hz in [60u64, 90, 120, 144, 165] {
            let period = Duration::from_nanos(1_000_000_000 / hz);
            for phase_quarter in 0..4u32 {
                let offset = period * phase_quarter / 4;
                let vsync = Arc::new(Vsync::default());
                vsync.publish(origin + offset, period);
                let pacer = HostPacer {
                    vsync: Some(vsync),
                    keys: Arc::default(),
                    origin,
                };
                for render in [
                    Duration::ZERO,
                    Duration::from_micros(500),
                    Duration::from_millis(2),
                ] {
                    let mut now = origin + offset;
                    let mut last = now;
                    for frame in 1..=600u64 {
                        let wait = pacer.until_vsync_at(now);
                        assert!(wait > Duration::ZERO, "{hz} Hz frame={frame}");
                        let wake = now + wait;
                        // Every wake lands on a host vsync.
                        let since_anchor = wake.duration_since(origin + offset).as_nanos();
                        let rem = since_anchor % period.as_nanos();
                        assert!(
                            rem.min(period.as_nanos() - rem) <= 1,
                            "{hz} Hz phase={phase_quarter} frame={frame}: off-grid rem={rem}"
                        );
                        if frame > 1 && hz % 60 == 0 {
                            let step = wake.duration_since(last);
                            assert_eq!(
                                step,
                                period * (hz / 60) as u32,
                                "{hz} Hz whole multiple step={step:?}"
                            );
                        }
                        last = wake;
                        now = wake + render;
                    }
                    let total = last.duration_since(origin + offset);
                    assert!(
                        total.abs_diff(Duration::from_secs(10))
                            <= period + Duration::from_millis(1),
                        "{hz} Hz phase={phase_quarter} render={render:?}: 600 frames took {total:?}"
                    );
                }
            }
        }
    }

    // spec: HOST-SES-8
    #[test]
    fn host_pacer_does_not_repay_missed_frames_after_stall() {
        let origin = Instant::now();
        for hz in [90u64, 144, 165] {
            let period = Duration::from_nanos(1_000_000_000 / hz);
            let vsync = Arc::new(Vsync::default());
            vsync.publish(origin, period);
            let pacer = HostPacer {
                vsync: Some(vsync),
                keys: Arc::default(),
                origin,
            };
            let first_wake = origin + pacer.until_vsync_at(origin);
            // Stall for 50 ms (3 missed frames), then spin.
            let stalled = first_wake + Duration::from_millis(50);
            let wait = pacer.until_vsync_at(stalled);
            assert!(
                wait > Duration::ZERO && wait <= period * 3,
                "{hz} Hz: {wait:?}"
            );
            let resumed = stalled + wait;
            let next_wait = pacer.until_vsync_at(resumed + Duration::from_millis(1));
            assert!(next_wait <= period * 3, "{hz} Hz next_wait={next_wait:?}");
        }
    }
}
