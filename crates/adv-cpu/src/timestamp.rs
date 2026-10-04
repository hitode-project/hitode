//! A 1 GHz guest reference counter, independent of host ISA and instruction
//! retirement rate. The machine supplies its sole guest clock; standalone callers can
//! inject the same microsecond source through the raw trap boundary.
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

/// Explicit virtual TSC frequency. This is not a host CPU speed claim.
pub const TSC_FREQ: u64 = 1_000_000_000;

#[derive(Default)]
pub(crate) struct Counter {
    origin: Option<Instant>,
    last: Option<u64>,
    offset: u64,
    shared: Option<Arc<AtomicU64>>,
}
impl Counter {
    pub fn share(&mut self, shared: Arc<AtomicU64>) {
        if let Some(last) = self.last {
            shared.fetch_max(last, Ordering::AcqRel);
        }
        self.shared = Some(shared);
    }
    pub fn anchored((us, at): (u64, Instant), shared: Arc<AtomicU64>) -> Self {
        Self {
            origin: Some(at),
            last: None,
            offset: us.wrapping_mul(TSC_FREQ / 1_000_000),
            shared: Some(shared),
        }
    }
    pub fn read(&mut self, guest_time_us: Option<u64>) -> u64 {
        let sample = match guest_time_us {
            Some(us) => us.wrapping_mul(TSC_FREQ / 1_000_000),
            None => self.offset.wrapping_add(
                self.origin
                    .get_or_insert_with(Instant::now)
                    .elapsed()
                    .as_nanos() as u64,
            ),
        };
        // Preserve progress within the owner's clock resolution, across context
        // restoration and executor changes. Forward 64-bit wrap remains valid.
        let value = match self.last {
            Some(last) if sample == last || sample.wrapping_sub(last) > i64::MAX as u64 => {
                last.wrapping_add(1)
            }
            _ => sample,
        };
        let value = if let Some(shared) = &self.shared {
            let mut previous = shared.load(Ordering::Acquire);
            loop {
                let next = if value == previous || value.wrapping_sub(previous) > i64::MAX as u64 {
                    previous.wrapping_add(1)
                } else {
                    value
                };
                match shared.compare_exchange_weak(
                    previous,
                    next,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break next,
                    Err(actual) => previous = actual,
                }
            }
        } else {
            value
        };
        self.last = Some(value);
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_frequency_submicrosecond_progress_and_low_word_carry() {
        let mut counter = Counter::default();
        assert_eq!(counter.read(Some(1_000_000)), TSC_FREQ);
        assert_eq!(counter.read(Some(1_000_000)), TSC_FREQ + 1);
        assert_eq!(counter.read(Some(1_000_001)), TSC_FREQ + 1000);
        assert_eq!(counter.read(Some(999_999)), TSC_FREQ + 1001);
        let ticks = counter.read(Some(4_294_968));
        assert_eq!((ticks as u32, ticks >> 32), (704, 1));
    }
}
