//! The single guest time source. timeGetTime, GetTickCount, QueryPerformanceCounter and
//! every wait deadline read this clock, so a run can be made deterministic (virtual time)
//! or recorded and replayed later.

use std::time::Instant;

pub enum ClockMode {
    /// Wall-clock time since machine start.
    Real { started: Instant, paused_at: Option<Instant>, paused_for_us: u64 },
    /// Time moves only when every context is blocked: it jumps to the earliest deadline.
    Virtual(u64),
}

pub struct Clock {
    mode: ClockMode,
    /// Added to every reading so tick counts don't start at zero (some code treats 0 as unset).
    pub epoch_us: u64,
}

/// QueryPerformanceFrequency. 10 MHz, as on current Windows.
pub const QPC_FREQ: u64 = 10_000_000;

impl Clock {
    pub fn real() -> Self {
        Self {
            mode: ClockMode::Real { started: Instant::now(), paused_at: None, paused_for_us: 0 },
            epoch_us: 1_000_000,
        }
    }

    pub fn virtual_time() -> Self {
        Self { mode: ClockMode::Virtual(0), epoch_us: 1_000_000 }
    }

    pub fn is_virtual(&self) -> bool {
        matches!(self.mode, ClockMode::Virtual(_))
    }

    pub fn now_us(&self) -> u64 {
        self.epoch_us
            + match &self.mode {
                ClockMode::Real { started, paused_at, paused_for_us } => {
                    paused_at.unwrap_or_else(Instant::now).duration_since(*started).as_micros() as u64 - paused_for_us
                }
                ClockMode::Virtual(t) => *t,
            }
    }

    pub fn now_ms(&self) -> u32 {
        (self.now_us() / 1000) as u32
    }

    pub fn qpc(&self) -> u64 {
        self.now_us() * (QPC_FREQ / 1_000_000)
    }

    /// A reading the guest can observe (timeGetTime, QPC, ...). In virtual mode every
    /// reading advances time by 1 µs, so busy-waits on the clock terminate.
    pub fn guest_now_us(&mut self) -> u64 {
        if let ClockMode::Virtual(t) = &mut self.mode {
            *t += 1;
        }
        self.now_us()
    }

    /// Virtual mode: move time forward to `us` (never backwards). Real mode: no-op.
    pub fn advance_to(&mut self, us: u64) {
        if let ClockMode::Virtual(t) = &mut self.mode {
            *t = (*t).max(us.saturating_sub(self.epoch_us));
        }
    }

    /// Freeze guest-visible real time while the frontend is backgrounded.
    pub fn pause(&mut self) {
        if let ClockMode::Real { paused_at, .. } = &mut self.mode {
            paused_at.get_or_insert_with(Instant::now);
        }
    }

    /// Continue real time without counting the interval spent in the background.
    pub fn resume(&mut self) {
        if let ClockMode::Real { paused_at, paused_for_us, .. } = &mut self.mode
            && let Some(at) = paused_at.take()
        {
            *paused_for_us += at.elapsed().as_micros() as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Clock;

    #[test]
    fn paused_real_clock_does_not_advance() {
        let mut clock = Clock::real();
        clock.pause();
        let stopped = clock.now_us();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(clock.now_us(), stopped);
        clock.resume();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(clock.now_us() > stopped);
    }
}
