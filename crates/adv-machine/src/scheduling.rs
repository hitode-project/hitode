//! Host-side scheduling allowances, independent of guest time and instruction fuel.
use std::time::Duration;

/// The default *guest context* quantum inside a real-time host slice.
/// This is not a Sleep duration and does not advance the guest clock.
pub const DEFAULT_QUANTUM: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Limit {
    Hard,
    HostSlice,
    Reschedule,
}

/// Ties favor hard failure, then returning to the host, then internal rescheduling.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Allowance {
    pub duration: Duration,
    pub limit: Limit,
}

impl Allowance {
    pub fn choose(
        hard: Option<Duration>,
        host: Option<Duration>,
        reschedule: Option<Duration>,
    ) -> Option<Self> {
        [
            (hard, Limit::Hard),
            (host, Limit::HostSlice),
            (reschedule, Limit::Reschedule),
        ]
        .into_iter()
        .filter_map(|(duration, limit)| duration.map(|duration| Self { duration, limit }))
        .min_by_key(|a| a.duration)
    }
}

/// Owner-lane service per guest context. Includes inline native/interpreted work,
/// synchronous API handlers; excludes background worker time
/// and continuations resumed outside Backend::run. Not CPU utilization.
#[derive(Clone, Debug, Default)]
pub struct ContextRunStats {
    pub runs: u64,
    pub owner_wall_ns: u64,
    pub host_calls: u64,
    pub competing_wall_ns: u64,
}

/// Cheap cumulative diagnostics. Wake lateness is sampled when the scheduler observes
/// a timed wait becoming ready, NOT when that thread subsequently executes.
/// Counters are host observations; no timing is inferred from guest instruction fuel.
#[derive(Clone, Debug, Default)]
pub struct SchedulingStats {
    pub contexts: Vec<ContextRunStats>,
    pub external_interrupts: u64,
    pub internal_preemptions: u64,
    pub polling_rounds: u64,
    pub timed_wakes: u64,
    pub timed_wake_lateness_us_total: u64,
    pub timed_wake_lateness_us_max: u64,
    pub timed_wakes_over_1ms: u64,
    pub bounded_cpu_runs: u64,
    pub cpu_budget_overrun_us_total: u64,
    pub cpu_budget_overrun_us_max: u64,
    pub wake_priority_dispatches: u64,
    pub wake_dispatches: u64,
    pub wake_ready_queue_us_total: u64,
    pub wake_ready_queue_us_max: u64,
    pub timed_dispatches: u64,
    pub timed_dispatch_lateness_us_total: u64,
    pub timed_dispatch_lateness_us_max: u64,
    pub frame_host_yields: u64,
    pub active_time_skips: u64,
    pub active_time_skip_us: u64,
}

/// Only runnable peers (or waits whose signal can change inside guest execution)
/// require a periodic quantum. Timed sleepers alone require just their deadline.
pub(crate) fn reschedule_after(
    quantum: Duration,
    competitive: bool,
    wake: Option<Duration>,
) -> Option<Duration> {
    [competitive.then_some(quantum), wake]
        .into_iter()
        .flatten()
        .min()
}

impl SchedulingStats {
    pub(crate) fn context_run(&mut self, id: usize, wall: Duration, calls: u64, competing: bool) {
        self.contexts
            .resize_with(self.contexts.len().max(id + 1), Default::default);
        let row = &mut self.contexts[id];
        let ns = wall.as_nanos().min(u128::from(u64::MAX)) as u64;
        row.runs += 1;
        row.owner_wall_ns = row.owner_wall_ns.saturating_add(ns);
        row.host_calls += calls;
        row.competing_wall_ns =
            row.competing_wall_ns
                .saturating_add(if competing { ns } else { 0 });
    }

    pub(crate) fn dispatch(
        &mut self,
        stamp: Option<crate::wake_queue::WakeStamp>,
        now: u64,
        promoted: bool,
    ) {
        self.wake_priority_dispatches += u64::from(promoted);
        if let Some(stamp) = stamp {
            let queued = now.saturating_sub(stamp.observed_us);
            self.wake_dispatches += 1;
            self.wake_ready_queue_us_total = self.wake_ready_queue_us_total.saturating_add(queued);
            self.wake_ready_queue_us_max = self.wake_ready_queue_us_max.max(queued);
            if let Some(deadline) = stamp.deadline_us {
                let late = now.saturating_sub(deadline);
                self.timed_dispatches += 1;
                self.timed_dispatch_lateness_us_total =
                    self.timed_dispatch_lateness_us_total.saturating_add(late);
                self.timed_dispatch_lateness_us_max = self.timed_dispatch_lateness_us_max.max(late);
            }
        }
    }

    pub(crate) fn timed_wake(&mut self, now_us: u64, deadline_us: u64) {
        let late = now_us.saturating_sub(deadline_us);
        self.timed_wakes += 1;
        self.timed_wake_lateness_us_total = self.timed_wake_lateness_us_total.saturating_add(late);
        self.timed_wake_lateness_us_max = self.timed_wake_lateness_us_max.max(late);
        self.timed_wakes_over_1ms += u64::from(late > 1_000);
    }

    pub(crate) fn cpu_run(&mut self, requested: Duration, actual: Duration) {
        let excess = actual
            .saturating_sub(requested)
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        self.bounded_cpu_runs += 1;
        self.cpu_budget_overrun_us_total = self.cpu_budget_overrun_us_total.saturating_add(excess);
        self.cpu_budget_overrun_us_max = self.cpu_budget_overrun_us_max.max(excess);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn us(n: u64) -> Option<Duration> {
        Some(Duration::from_micros(n))
    }
    #[test]
    fn no_quantum_poll_when_only_timed_sleepers_remain() {
        let quantum = Duration::from_micros(1000);
        assert_eq!(reschedule_after(quantum, false, us(16_667)), us(16_667));
        assert_eq!(reschedule_after(quantum, false, None), None);
        assert_eq!(reschedule_after(quantum, true, us(16_667)), us(1000));
        assert_eq!(reschedule_after(quantum, true, us(125)), us(125));
        assert_eq!(reschedule_after(quantum, false, us(0)), us(0));
    }
    #[test]
    fn dispatch_latency_includes_ready_queue_time() {
        let mut stats = SchedulingStats::default();
        stats.dispatch(
            Some(crate::wake_queue::WakeStamp {
                observed_us: 1500,
                deadline_us: Some(1000),
                priority: Default::default(),
            }),
            2500,
            true,
        );
        assert_eq!(stats.wake_ready_queue_us_total, 1000);
        assert_eq!(stats.timed_dispatch_lateness_us_total, 1500);
        stats.dispatch(
            Some(crate::wake_queue::WakeStamp {
                observed_us: 3000,
                deadline_us: None,
                priority: Default::default(),
            }),
            3000,
            false,
        );
        assert_eq!(stats.wake_dispatches, 2);
        assert_eq!(stats.timed_dispatches, 1);
        assert_eq!(stats.wake_priority_dispatches, 1);
    }
    #[test]
    fn earliest_deadline_limits_cpu_not_the_host_slice() {
        let a = Allowance::choose(us(30_000_000), us(8_000), us(125)).unwrap();
        assert_eq!(a.limit, Limit::Reschedule);
        assert_eq!(a.duration, Duration::from_micros(125));
    }
    #[test]
    fn hard_deadline_wins_ties() {
        assert_eq!(
            Allowance::choose(us(100), us(100), us(100)).unwrap().limit,
            Limit::Hard
        );
    }
    #[test]
    fn host_slice_wins_a_quantum_tie() {
        assert_eq!(
            Allowance::choose(None, us(100), us(100)).unwrap().limit,
            Limit::HostSlice
        );
    }
    #[test]
    fn absent_quantum_preserves_existing_budget() {
        assert_eq!(
            Allowance::choose(us(500), us(800), None).unwrap().limit,
            Limit::Hard
        );
        assert!(Allowance::choose(None, None, None).is_none());
    }
    #[test]
    fn zero_is_observable_and_must_not_enter_cpu() {
        assert_eq!(
            Allowance::choose(us(500), us(800), us(0)).unwrap().duration,
            Duration::ZERO
        );
    }
    #[test]
    fn telemetry_distinguishes_lateness_and_backend_overrun() {
        let mut stats = SchedulingStats::default();
        stats.timed_wake(5_000, 1_000);
        stats.timed_wake(10_000, 10_000);
        stats.cpu_run(Duration::from_micros(100), Duration::from_micros(350));
        stats.cpu_run(Duration::from_micros(100), Duration::from_micros(10));
        assert_eq!(stats.timed_wakes, 2);
        assert_eq!(stats.timed_wakes_over_1ms, 1);
        assert_eq!(stats.timed_wake_lateness_us_max, 4_000);
        assert_eq!(stats.cpu_budget_overrun_us_total, 250);
        assert_eq!(stats.bounded_cpu_runs, 2);
    }
}
