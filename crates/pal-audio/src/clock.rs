//! Playback time published by the device consumer. No guest memory is shared.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};
use std::time::Instant;

pub struct PlaybackClock {
    started: Instant,
    device_allowed: bool,
    device_time: AtomicBool,
    sequence: AtomicU64,
    anchor_us: AtomicU64,
    host_us: AtomicU64,
    observed_us: AtomicU64,
    paused: AtomicBool,
    paused_us: AtomicU64,
}

impl Default for PlaybackClock {
    fn default() -> Self {
        Self::new(false)
    }
}

impl PlaybackClock {
    /// Virtual/headless consumers leave device time disabled. A real output
    /// opts in and publishes its consumed-frame timeline before each callback.
    pub fn new(device_allowed: bool) -> Self {
        Self {
            started: Instant::now(),
            device_allowed,
            device_time: AtomicBool::new(false),
            sequence: AtomicU64::new(0),
            anchor_us: AtomicU64::new(0),
            host_us: AtomicU64::new(0),
            observed_us: AtomicU64::new(0),
            paused: AtomicBool::new(false),
            paused_us: AtomicU64::new(0),
        }
    }
    pub fn uses_device_time(&self) -> bool {
        self.device_time.load(Ordering::Acquire)
    }
    pub fn now_us(&self, virtual_us: u64) -> u64 {
        self.now_at(virtual_us, self.host_now_us())
    }
    fn host_now_us(&self) -> u64 {
        self.started.elapsed().as_micros() as u64
    }
    fn now_at(&self, virtual_us: u64, host_us: u64) -> u64 {
        if !self.uses_device_time() {
            return virtual_us;
        }
        if self.paused.load(Ordering::Acquire) {
            return self.paused_us.load(Ordering::Relaxed);
        }
        loop {
            let sequence = self.sequence.load(Ordering::Acquire);
            if sequence & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let position = self.anchor_us.load(Ordering::Relaxed);
            let timestamp = self.host_us.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == sequence {
                let current = if host_us >= timestamp {
                    position.saturating_add(host_us - timestamp)
                } else {
                    position.saturating_sub(timestamp - host_us)
                };
                // Device reopen and buffered callback timestamp jitter must not
                // rewind a cursor that guest code has already observed.
                return self.observed_us.fetch_max(current, Ordering::Relaxed).max(current);
            }
        }
    }
    /// Called while starting a stream, before its callback runs. After a stream
    /// stops, the last anchor continues by host time until another device opens.
    pub fn start_device(&self, virtual_us: u64) -> bool {
        if !self.device_allowed {
            return false;
        }
        if !self.uses_device_time() {
            self.publish_at(virtual_us, self.host_now_us());
            self.observed_us.store(virtual_us, Ordering::Relaxed);
            self.device_time.store(true, Ordering::Release);
        }
        true
    }
    /// One device callback is the writer. Stop/reopen joins the old callback
    /// before changing consumer state. Delay is the backend's playback timestamp
    /// relative to its callback timestamp, rather than guest scheduling time.
    pub fn publish_device(&self, position_us: u64, delay_us: u64) {
        if self.uses_device_time() && !self.paused.load(Ordering::Acquire) {
            self.publish_at(position_us, self.host_now_us().saturating_add(delay_us));
        }
    }
    pub fn suspend_device(&self, suspended: bool) {
        self.suspend_at(suspended, self.host_now_us());
    }
    fn suspend_at(&self, suspended: bool, host_us: u64) {
        if !self.uses_device_time() {
            return;
        }
        if suspended && !self.paused.load(Ordering::Acquire) {
            let position = self.now_at(0, host_us);
            self.paused_us.store(position, Ordering::Relaxed);
            self.paused.store(true, Ordering::Release);
        } else if !suspended && self.paused.load(Ordering::Acquire) {
            self.publish_at(self.paused_us.load(Ordering::Relaxed), host_us);
            self.paused.store(false, Ordering::Release);
        }
    }
    fn publish_at(&self, position_us: u64, host_us: u64) {
        let sequence = self.sequence.load(Ordering::Relaxed);
        if sequence & 1 != 0
            || self
                .sequence
                .compare_exchange(sequence, sequence.wrapping_add(1), Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        self.anchor_us.store(position_us, Ordering::Relaxed);
        self.host_us.store(host_us, Ordering::Relaxed);
        self.sequence.store(sequence.wrapping_add(2), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_session_pause_freezes_device_time_without_an_idle_gap_jump() {
        let clock = PlaybackClock::new(true);
        clock.start_device(1_000_000);
        clock.publish_at(1_000_000, 100_000_000);
        clock.suspend_at(true, 100_010_000);
        assert_eq!(clock.now_at(9_000_000, 200_000_000), 1_010_000);
        clock.suspend_at(false, 200_000_000);
        assert_eq!(clock.now_at(0, 200_005_000), 1_015_000);
    }
    #[test]
    fn virtual_device_is_deterministic_and_does_not_follow_host_time() {
        let c = PlaybackClock::new(false);
        assert!(!c.start_device(100));
        assert_eq!(c.now_at(123, 999999), 123);
        assert_eq!(c.now_at(456, 0), 456);
    }
    #[test]
    fn real_cursor_uses_callback_frames_and_timestamp_across_guest_jumps() {
        let c = PlaybackClock::new(true);
        assert!(c.start_device(1_000_000));
        c.publish_at(1_020_000, 500_000);
        assert_eq!(c.now_at(9_000_000, 505_000), 1_025_000);
        assert_eq!(c.now_at(1, 507_000), 1_027_000);
        c.publish_at(1_040_000, 520_000);
        assert_eq!(c.now_at(99_000_000, 521_000), 1_041_000);
        assert_eq!(c.now_at(0, 620_000), 1_140_000, "device absence extrapolates without guest jumps");
    }
    #[test]
    fn future_playback_timestamp_and_device_reopen_do_not_rewind() {
        let c = PlaybackClock::new(true);
        c.start_device(1_000_000);
        c.publish_at(1_020_000, 500_000);
        assert_eq!(c.now_at(0, 490_000), 1_010_000);
        assert_eq!(c.now_at(0, 510_000), 1_030_000);
        c.publish_at(1_010_000, 510_000);
        assert_eq!(c.now_at(0, 511_000), 1_030_000);
    }
}
