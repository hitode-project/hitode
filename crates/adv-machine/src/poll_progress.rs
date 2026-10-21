//! Productive polling (PROC-CLOCK-5): a real core runs a Sleep(0) loop at full
//! speed. A loop that repeats the host calls of an earlier yield interval
//! observes nothing new, so its polling round may wait. A loop doing new work
//! between yields, such as skipping text, must run on at once.

/// Recent yield intervals a repeat may match; covers loops with several yields.
const HISTORY: usize = 4;

/// The host calls of a context's yield intervals.
#[derive(Clone, Copy, Debug, Default)]
pub struct PollProgress {
    /// Order-sensitive hash of the host calls since the last yield.
    current: u64,
    history: [u64; HISTORY],
    next: usize,
    /// Consecutive intervals that matched an earlier one.
    repeats: u32,
}

impl PollProgress {
    pub(crate) fn call(&mut self, thunk: u32) {
        self.current = (self.current.rotate_left(7) ^ u64::from(thunk) ^ 0x100)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }

    /// The context yielded, closing its interval.
    pub(crate) fn yielded(&mut self) {
        let interval = std::mem::take(&mut self.current);
        self.repeats = if self.history.contains(&interval) {
            self.repeats.saturating_add(1)
        } else {
            0
        };
        self.history[self.next] = interval;
        self.next = (self.next + 1) % HISTORY;
    }

    /// Its last full history of intervals were repeats: it only polls.
    pub(crate) fn settled(&self) -> bool {
        self.repeats >= HISTORY as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interval(progress: &mut PollProgress, calls: &[u32]) {
        for &call in calls {
            progress.call(call);
        }
        progress.yielded();
    }

    #[test]
    fn two_yield_loops_settle_and_new_work_unsettles_at_once() {
        let mut progress = PollProgress::default();
        for _ in 0..HISTORY {
            interval(&mut progress, &[1, 2, 3]);
            interval(&mut progress, &[4]);
        }
        assert!(progress.settled());
        interval(&mut progress, &[1, 5, 2, 3]);
        assert!(!progress.settled());
        interval(&mut progress, &[4]);
        interval(&mut progress, &[3, 2, 1]);
        assert!(!progress.settled(), "order is part of the interval");
    }
}
