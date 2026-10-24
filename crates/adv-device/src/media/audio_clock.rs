//! ASF/WMA packet timestamps describe packet boundaries, not always the exact start of
//! decoded samples. Anchor once, then count samples; retain genuine discontinuities.
#[derive(Default)]
pub(crate) struct AudioClock {
    next: Option<f64>,
}

impl AudioClock {
    #[cfg(feature = "media")]
    pub fn missing(&mut self, frames: usize, rate: u32) -> u64 {
        let start = self.next.unwrap_or(0.0);
        self.next = Some(start + frames as f64 * 1_000_000.0 / f64::from(rate));
        start.round() as u64
    }

    pub fn stamp(&mut self, pts: u64, frames: usize, rate: u32) -> u64 {
        let raw = pts as f64;
        let start = self
            .next
            .filter(|next| (raw - next).abs() <= 100_000.0)
            .unwrap_or(raw);
        self.next = Some(start + frames as f64 * 1_000_000.0 / f64::from(rate));
        start.round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coarse_packets_do_not_insert_silence_or_trim_samples() {
        let mut clock = AudioClock::default();
        // LL1 opening ASF/WMA: 56.8 ms positive jitter followed by negative jitter.
        assert_eq!(clock.stamp(0, 10240, 44100), 0);
        assert_eq!(clock.stamp(289000, 8192, 44100), 232200);
        assert_eq!(clock.stamp(464000, 10240, 44100), 417959);
        assert_eq!(clock.stamp(714000, 8192, 44100), 650159);
    }
    #[test]
    fn real_gaps_and_seek_keep_their_new_anchor() {
        let mut clock = AudioClock::default();
        assert_eq!(clock.stamp(500_000, 4410, 44100), 500_000);
        assert_eq!(clock.stamp(900_000, 4410, 44100), 900_000);
        assert_eq!(clock.stamp(1_001_000, 4410, 44100), 1_000_000);
        clock = AudioClock::default();
        assert_eq!(clock.stamp(50_000, 4410, 44100), 50_000);
    }
    #[test]
    fn fractional_sample_durations_do_not_accumulate_rounding_drift() {
        let mut clock = AudioClock::default();
        for i in 0..10000_u64 {
            let exact = i as f64 * 1024.0 * 1_000_000.0 / 44100.0;
            assert_eq!(
                clock.stamp(exact as u64 / 1000 * 1000, 1024, 44100),
                exact.round() as u64
            );
        }
    }
}
