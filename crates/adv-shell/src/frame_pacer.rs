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

/// The wait from `since` into a display period to the next guest frame tick.
/// Faster displays still get one frame per 1/60 s. A display our own rate
/// request slowed below 60 Hz (HOST-SES-11) is subdivided to the nearest
/// 1/60 s, so a lowered refresh can never slow the guest that requested it.
fn until_tick(since: Duration, period: Duration) -> Duration {
    let ratio = PERIOD.as_nanos() as f64 / period.as_nanos().max(1) as f64;
    if ratio >= 1.0 {
        (period * ratio.round() as u32).saturating_sub(since)
    } else {
        let tick = period / ratio.recip().round() as u32;
        tick - Duration::from_nanos((since.as_nanos() % tick.as_nanos()) as u64)
    }
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
        Some(until_tick(since, period))
    }

    fn input_held(&self) -> bool {
        self.keys.any_pressed()
    }
}

/// Publication cadence, independent of render latency and mailbox frame drops.
/// It requests the fastest 1/60 s multiple the guest reaches at least twice a
/// second, not the average: a guest that misses frames under load still
/// reaches single ticks and keeps 60 Hz, so the request steps through 60/n
/// only when the guest's own pacing changes.
#[cfg(any(host_view, test))]
#[derive(Default)]
pub(crate) struct Cadence {
    last: Option<(u64, Instant)>,
    /// 1/60 s ticks per publication over the last second, oldest first.
    ticks: std::collections::VecDeque<(Instant, u32)>,
}
#[cfg(any(host_view, test))]
impl Cadence {
    pub(crate) fn published(&mut self, serial: u64, at: Instant) {
        if let Some((previous, time)) = self.last {
            if serial <= previous {
                return;
            }
            let elapsed = at.saturating_duration_since(time);
            if elapsed > Duration::from_millis(250) {
                self.ticks.clear();
            } else {
                let per = elapsed.as_nanos() as f64 / (serial - previous) as f64;
                let ticks = (per / PERIOD.as_nanos() as f64).round().max(1.0) as u32;
                self.ticks.push_back((at, ticks));
                while self
                    .ticks
                    .front()
                    .is_some_and(|(t, _)| at.duration_since(*t) > Duration::from_secs(1))
                {
                    self.ticks.pop_front();
                }
            }
        }
        self.last = Some((serial, at));
    }

    pub(crate) fn rate(&self) -> u32 {
        // One early publication is jitter; a cadence counts once it repeats.
        (1..=15)
            .find(|&n| self.ticks.iter().filter(|(_, t)| *t == n).count() >= 2)
            .map_or(60, |n| 60 / n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: HOST-SES-8
    #[test]
    fn requested_cadence_caps_fast_publications_at_sixty() {
        let origin = Instant::now();
        for rate in [120, 180] {
            let mut cadence = Cadence::default();
            let period = Duration::from_secs_f64(1.0 / f64::from(rate));
            for serial in 1..=5 {
                cadence.published(u64::from(serial), origin + period * serial);
            }
            assert_eq!(cadence.rate(), 60);
        }
    }

    // spec: HOST-SES-11
    #[test]
    fn requested_rate_keeps_sixty_under_load_and_ignores_one_early_frame() {
        let origin = Instant::now();
        let tick = Duration::from_secs_f64(1.0 / 60.0);
        let mut cadence = Cadence::default();
        let mut at = origin;
        // A guest at about 44 fps under load, with intervals of 1 to 2 ticks.
        for (serial, ticks) in (1..=60u64).zip([2.0, 1.0, 1.5, 1.0, 2.0, 2.0, 1.0].iter().cycle()) {
            at += tick.mul_f64(*ticks);
            cadence.published(serial, at);
            if serial > 4 {
                assert_eq!(cadence.rate(), 60, "serial {serial}");
            }
        }
        // A steady 30 fps guest keeps 30 through one early frame.
        let mut cadence = Cadence::default();
        let mut at = origin;
        for serial in 1..=40u64 {
            at += if serial == 20 { tick } else { tick * 2 };
            cadence.published(serial, at);
        }
        assert_eq!(cadence.rate(), 30);
    }

    // spec: HOST-SES-11
    #[test]
    fn a_lowered_refresh_keeps_sixty_hertz_guest_ticks() {
        let ms = |v: f64| Duration::from_secs_f64(v / 1000.0);
        let near = |a: Duration, b: Duration| a.abs_diff(b) < Duration::from_micros(50);
        // 120 Hz: the next tick is one 1/60 s period away from the vsync.
        assert!(near(until_tick(ms(1.0), ms(8.333)), ms(15.667)));
        // 30 Hz from our own request: two ticks per vsync.
        assert!(near(until_tick(ms(1.0), ms(33.333)), ms(15.667)));
        assert!(near(until_tick(ms(20.0), ms(33.333)), ms(13.333)));
        // A real 50 Hz display caps the guest at its own rate (HOST-SES-8).
        assert!(near(until_tick(ms(1.0), ms(20.0)), ms(19.0)));
    }

    // spec: HOST-SES-11
    #[test]
    fn requested_rate_follows_publication_cadence_including_dropped_frames() {
        let origin = Instant::now();
        for rate in [30, 60] {
            let mut cadence = Cadence::default();
            assert_eq!(cadence.rate(), 60);
            let period = Duration::from_secs_f64(1.0 / f64::from(rate));
            for serial in [1, 2, 4, 7, 8, 9] {
                cadence.published(u64::from(serial), origin + period * serial);
            }
            assert_eq!(cadence.rate(), rate);
            cadence.published(9, origin + period * 9); // Repaint is not a new publication.
            assert_eq!(cadence.rate(), rate);
            cadence.published(10, origin + Duration::from_secs(2));
            assert_eq!(
                cadence.rate(),
                60,
                "a still screen does not request a low rate"
            );
        }
    }
}
