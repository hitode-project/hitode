//! Productive polling (PROC-CLOCK-5): a real core runs a Sleep(0) loop at full
//! speed. A loop that repeats an earlier yield interval, the same host calls,
//! guest instructions and yield site, observes nothing new, so its polling
//! round may wait. A loop doing new work between yields, such as skipping text,
//! must run on at once, even when only its instruction count tells it apart.
//! Explicit observations also admit an earlier quiet round (PROC-CLOCK-7),
//! provided the machine sees no intervening delivery or productive host call.
//!
//! Between observations a guest is deterministic. An interval that read no
//! clock and saw no event changes only through its own computation or another
//! context's memory. One longer than a memory spin's lap, the yield call and a
//! test, is computing, such as a script interpreter yielding after a fixed
//! number of steps, and is never a repeat. What an interval observed also
//! bounds its round: a quiet interval that read no clock sees nothing new
//! before the next delivery or deadline (PROC-CLOCK-6). An unclassified host
//! call may read anything, so it keeps the clock's ticks.

/// Recent yield intervals a repeat may match; covers loops with several yields.
const HISTORY: usize = 4;
/// Repeats before a loop counts as polling. Iteration-paced work breaks its
/// repeats within a few dozen yields; a loop waiting for time or input does not.
const STREAK: u32 = 64;
/// Guest instructions above which an interval that observed nothing computes.
/// A memory spin's lap, Sleep(0) through its system call and a flag test,
/// takes tens; a stepped interpreter's slice takes thousands or more.
const SPIN_LAP: u64 = 1024;

/// What a host call that is not work tells the guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observation {
    /// It gave up the processor; it read nothing.
    Yield,
    /// It read the message queue or an object's state.
    Event,
    /// It read a clock.
    Clock,
}

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
    /// This interval contains unclassified or productive work.
    work: bool,
    /// This interval read a clock or saw an event.
    sensed: bool,
    /// This interval read a clock.
    clock: bool,
    quiet: bool,
    /// The last closed interval read a clock.
    read_clock: bool,
}

impl PollProgress {
    pub(crate) fn call(&mut self, thunk: u32, observation: Option<Observation>) {
        self.work |= observation.is_none();
        self.sensed |= matches!(observation, Some(Observation::Event | Observation::Clock));
        if observation == Some(Observation::Clock) {
            self.read_clock();
        }
        self.mix(u64::from(thunk) ^ 0x100);
    }

    /// Guest code read a clock without a host call: RDTSC, a direct body or a
    /// shared clock field.
    pub(crate) fn read_clock(&mut self) {
        self.sensed = true;
        self.clock = true;
    }

    /// A direct body read the message queue without a host call.
    pub(crate) fn saw_event(&mut self) {
        self.sensed = true;
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
        let computing = !std::mem::take(&mut self.sensed) && retired > SPIN_LAP;
        let repeat = !computing && self.history.contains(&interval);
        self.quiet = !std::mem::take(&mut self.work) && repeat;
        self.repeats = if repeat {
            self.repeats.saturating_add(1)
        } else {
            0
        };
        self.read_clock = std::mem::take(&mut self.clock);
        self.history[self.next] = interval;
        self.next = (self.next + 1) % HISTORY;
    }

    /// Its last `STREAK` intervals were repeats: it only polls.
    pub(crate) fn settled(&self) -> bool {
        self.repeats >= STREAK
    }

    pub(crate) fn quiet(&self) -> bool {
        self.quiet
    }

    /// Its last interval read a clock, so the next tick may change it.
    #[cfg(test)]
    fn reads_clock(&self) -> bool {
        self.read_clock
    }

    /// Its last interval was quiet and read no clock: only a delivery or a
    /// deadline can change what it observes.
    pub(crate) fn timeless(&self) -> bool {
        self.quiet && !self.read_clock
    }

    pub(crate) fn new_work(&mut self) {
        self.work = true;
        self.quiet = false;
    }

    fn mix(&mut self, value: u64) {
        self.current = (self.current.rotate_left(7) ^ value).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const YIELD: u32 = 9;

    /// One interval: `calls`, `retired` instructions, then a Sleep(0).
    fn interval(progress: &mut PollProgress, calls: &[(u32, Option<Observation>)], retired: u64) {
        for &(call, observation) in calls {
            progress.call(call, observation);
        }
        progress.ran(retired);
        progress.call(YIELD, Some(Observation::Yield));
        progress.yielded(0x40);
    }

    #[test]
    fn two_yield_loops_settle_and_new_work_unsettles_at_once() {
        let mut progress = PollProgress::default();
        for _ in 0..STREAK {
            interval(&mut progress, &[(1, None), (2, None), (3, None)], 10);
            interval(&mut progress, &[(4, None)], 10);
        }
        assert!(progress.settled());
        interval(
            &mut progress,
            &[(1, None), (5, None), (2, None), (3, None)],
            10,
        );
        assert!(!progress.settled());
        interval(&mut progress, &[(4, None)], 10);
        interval(&mut progress, &[(3, None), (2, None), (1, None)], 10);
        assert!(!progress.settled(), "order is part of the interval");
    }

    #[test]
    fn direct_body_peeks_are_observations_not_computation() {
        // A hundred empty peeks completed in guest code, then the handoff.
        let mut progress = PollProgress::default();
        for _ in 0..=STREAK {
            progress.saw_event();
            interval(&mut progress, &[], 3_000);
        }
        assert!(progress.settled() && progress.timeless());
    }

    #[test]
    fn one_more_instruction_is_new_work() {
        let mut progress = PollProgress::default();
        for _ in 0..=STREAK {
            interval(&mut progress, &[(1, None)], 10);
        }
        assert!(progress.settled());
        interval(&mut progress, &[(1, None)], 11);
        assert!(!progress.settled());
    }

    #[test]
    fn a_computing_interval_never_repeats() {
        // A stepped interpreter: the same slice, then Sleep(0), observing nothing.
        let mut progress = PollProgress::default();
        for _ in 0..=STREAK {
            interval(&mut progress, &[], 30_000);
            assert!(!progress.quiet() && !progress.settled());
        }
    }

    #[test]
    fn a_heavy_clock_poll_repeats_and_reads_the_clock() {
        // GetTickCount between heavy slices, read from the shared clock page.
        let mut progress = PollProgress::default();
        for _ in 0..=STREAK {
            progress.read_clock();
            interval(&mut progress, &[], 30_000);
        }
        assert!(progress.quiet() && progress.settled() && progress.reads_clock());
        assert!(!progress.timeless());
        // timeGetTime as a host call counts the same.
        interval(&mut progress, &[(2, Some(Observation::Clock))], 30_000);
        interval(&mut progress, &[(2, Some(Observation::Clock))], 30_000);
        assert!(progress.quiet() && progress.reads_clock());
    }

    #[test]
    fn event_polls_and_memory_spins_repeat_without_the_clock() {
        let mut progress = PollProgress::default();
        for _ in 0..2 {
            interval(&mut progress, &[(3, Some(Observation::Event))], 30_000);
        }
        assert!(progress.timeless());
        let mut spin = PollProgress::default();
        for _ in 0..2 {
            interval(&mut spin, &[], 40);
        }
        assert!(spin.timeless());
        // An unclassified call may read something that changes with time.
        let mut unknown = PollProgress::default();
        for _ in 0..=STREAK {
            interval(&mut unknown, &[(4, None)], 40);
        }
        assert!(unknown.settled() && !unknown.timeless());
    }
}
