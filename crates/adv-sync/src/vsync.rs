//! The display's vsync grid, published by the host frame clock and read by
//! guest refresh pacing. Readers extrapolate the grid from the latest vsync;
//! with nothing published, pacing stays on the machine clock.
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Vsync(Mutex<Option<(Instant, Duration)>>);

impl Vsync {
    /// Record a vsync at `at`; a zero period withdraws the grid.
    pub fn publish(&self, at: Instant, period: Duration) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) =
            (!period.is_zero()).then_some((at, period));
    }

    pub fn clear(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Time since the latest grid vsync at or before `now`, and the period.
    pub fn phase(&self, now: Instant) -> Option<(Duration, Duration)> {
        let (at, period) = (*self.0.lock().unwrap_or_else(|e| e.into_inner()))?;
        let period_ns = period.as_nanos();
        let since = if now >= at {
            (now - at).as_nanos() % period_ns
        } else {
            (period_ns - (at - now).as_nanos() % period_ns) % period_ns
        };
        Some((Duration::from_nanos(since as u64), period))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_extrapolates_the_grid_in_both_directions() {
        let vsync = Vsync::default();
        let at = Instant::now() + Duration::from_secs(1);
        let period = Duration::from_micros(16_667);
        assert_eq!(vsync.phase(at), None);
        vsync.publish(at, period);
        assert_eq!(vsync.phase(at), Some((Duration::ZERO, period)));
        let later = at + period * 3 + Duration::from_micros(5);
        assert_eq!(vsync.phase(later), Some((Duration::from_micros(5), period)));
        let earlier = at - period * 2 - Duration::from_micros(5);
        assert_eq!(
            vsync.phase(earlier),
            Some((period - Duration::from_micros(5), period))
        );
        vsync.publish(at, Duration::ZERO);
        assert_eq!(vsync.phase(at), None);
    }
}
