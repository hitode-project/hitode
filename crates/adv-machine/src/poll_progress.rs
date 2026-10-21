//! Productive polling (PROC-CLOCK-5): a real core runs a Sleep(0) loop at full
//! speed. A loop that repeats an earlier yield interval, the same host calls,
//! guest instructions and yield site, observes nothing new, so its polling
//! round may wait. A loop doing new work between yields, such as skipping text,
//! must run on at once, even when only its instruction count tells it apart.

/// Recent yield intervals a repeat may match; covers loops with several yields.
const HISTORY: usize = 4;
/// Repeats before a loop counts as polling. Iteration-paced work breaks its
/// repeats within a few dozen yields; a loop waiting for time or input does not.
const STREAK: u32 = 64;

/// The yield intervals of a context.
#[derive(Clone, Copy, Debug, Default)]
pub struct PollProgress {
    /// Order-sensitive hash of the host calls since the last yield.
    current: u64,
    /// Guest instructions completed since the last yield.
    retired: u64,
    history: [u64; HISTORY],
    next: usize,
    /// Consecutive intervals that matched an earlier one.
    repeats: u32,
}

impl PollProgress {
    pub(crate) fn call(&mut self, thunk: u32) {
        self.mix(u64::from(thunk) ^ 0x100);
    }

    /// Guest instructions completed by one run of the context.
    pub(crate) fn ran(&mut self, retired: u64) {
        self.retired = self.retired.wrapping_add(retired);
    }

    /// The context yielded at `site`, closing its interval.
    pub(crate) fn yielded(&mut self, site: u32) {
        let retired = std::mem::take(&mut self.retired);
        self.mix(retired);
        self.mix(u64::from(site));
        let interval = std::mem::take(&mut self.current);
        self.repeats = if self.history.contains(&interval) {
            self.repeats.saturating_add(1)
        } else {
            0
        };
        self.history[self.next] = interval;
        self.next = (self.next + 1) % HISTORY;
    }

    /// Its last `STREAK` intervals were repeats: it only polls.
    pub(crate) fn settled(&self) -> bool {
        self.repeats >= STREAK
    }

    fn mix(&mut self, value: u64) {
        self.current = (self.current.rotate_left(7) ^ value).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interval(progress: &mut PollProgress, calls: &[u32], retired: u64) {
        for &call in calls {
            progress.call(call);
        }
        progress.ran(retired);
        progress.yielded(0x40);
    }

    #[test]
    fn two_yield_loops_settle_and_new_work_unsettles_at_once() {
        let mut progress = PollProgress::default();
        for _ in 0..STREAK {
            interval(&mut progress, &[1, 2, 3], 10);
            interval(&mut progress, &[4], 10);
        }
        assert!(progress.settled());
        interval(&mut progress, &[1, 5, 2, 3], 10);
        assert!(!progress.settled());
        interval(&mut progress, &[4], 10);
        interval(&mut progress, &[3, 2, 1], 10);
        assert!(!progress.settled(), "order is part of the interval");
    }

    #[test]
    fn one_more_instruction_is_new_work() {
        let mut progress = PollProgress::default();
        for _ in 0..=STREAK {
            interval(&mut progress, &[1], 10);
        }
        assert!(progress.settled());
        interval(&mut progress, &[1], 11);
        assert!(!progress.settled());
    }
}
