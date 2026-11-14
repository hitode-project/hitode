//! Frame load (PROC-MACH-1): how much of a loop's work the backend should
//! save by running it in blocks on other threads. Owner work is the time
//! since the last frame outside host idle, with split loops at their serial
//! cost, so splitting does not change the measure it is chosen from. It may
//! fill 60% of the frame period: 1/60 s, unless frames keep sleeping to
//! their own longer interval.
use adv_cpu::backend::Pace;
use std::time::{Duration, Instant};

const DEFAULT_PERIOD_NS: u64 = 1_000_000_000 / 60;
/// Owner work may fill this share of the period: a 40% margin.
const LOAD: (u64, u64) = (3, 5);
/// A frame that slept this long to a timeout was paced by a wait.
const PACED_IDLE_NS: u64 = 2_000_000;
/// Consecutive paced frames, each a tenth longer than the default period,
/// that set the period to their median interval.
const PACED_FRAMES: usize = 3;

#[derive(Debug)]
pub struct FrameLoad {
    last: Option<Instant>,
    idle_ns: u64,
    timed_ns: u64,
    saved_ns: u64,
    /// Work of the last frame.
    work_ns: u64,
    paced: Vec<u64>,
    period_ns: u64,
}

impl Default for FrameLoad {
    fn default() -> Self {
        Self {
            last: None,
            idle_ns: 0,
            timed_ns: 0,
            saved_ns: 0,
            work_ns: 0,
            paced: Vec::with_capacity(PACED_FRAMES),
            period_ns: DEFAULT_PERIOD_NS,
        }
    }
}

fn nanos(time: Duration) -> u64 {
    time.as_nanos().min(u128::from(u64::MAX)) as u64
}

impl FrameLoad {
    /// The host slept, until its timeout or a signal.
    pub(crate) fn idle(&mut self, time: Duration, timed_out: bool) {
        self.idle_ns = self.idle_ns.saturating_add(nanos(time));
        if timed_out {
            self.timed_ns = self.timed_ns.saturating_add(nanos(time));
        }
    }

    /// An owner computation whose helpers spent `helpers` beside it.
    pub(crate) fn computed(&mut self, helpers: Duration) {
        self.saved_ns = self.saved_ns.saturating_add(nanos(helpers));
    }

    fn work(&self, now: Instant) -> u64 {
        self.last.map_or(0, |last| {
            nanos(now - last)
                .saturating_sub(self.idle_ns)
                .saturating_add(self.saved_ns)
        })
    }

    pub(crate) fn pace(&self, now: Instant) -> Pace {
        Pace {
            work_ns: self.work(now),
            last_ns: self.work_ns,
            saved_ns: self.saved_ns,
            limit_ns: self.period_ns * LOAD.0 / LOAD.1,
        }
    }

    pub(crate) fn period_ns(&self) -> u64 {
        self.period_ns
    }

    /// A completed frame: returns its owner work in ns.
    pub(crate) fn frame(&mut self, now: Instant) -> u64 {
        let work = self.work(now);
        if let Some(last) = self.last {
            let interval = nanos(now - last);
            if self.timed_ns >= PACED_IDLE_NS && interval * 10 > DEFAULT_PERIOD_NS * 11 {
                if self.paced.len() == PACED_FRAMES {
                    self.paced.remove(0);
                }
                self.paced.push(interval);
                if self.paced.len() == PACED_FRAMES {
                    let mut sorted = self.paced.clone();
                    sorted.sort_unstable();
                    self.period_ns = sorted[PACED_FRAMES / 2];
                }
            } else {
                self.paced.clear();
                self.period_ns = DEFAULT_PERIOD_NS;
            }
        }
        self.last = Some(now);
        self.idle_ns = 0;
        self.timed_ns = 0;
        self.saved_ns = 0;
        self.work_ns = work;
        work
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    // spec: PROC-MACH-1
    #[test]
    fn splitting_saves_the_expected_work_past_the_margin() {
        let mut load = FrameLoad::default();
        let start = Instant::now();
        load.frame(start);
        // 6 ms of work, then a vsync wait.
        load.idle(10 * MS, true);
        assert_eq!(load.frame(start + 16 * MS), 6_000_000);
        // 9 ms of work against a 10 ms limit: a 4 ms loop must save the 3 ms
        // past it.
        let pace = load.pace(start + 25 * MS);
        let limit = DEFAULT_PERIOD_NS * 3 / 5;
        assert_eq!(pace.limit_ns, limit);
        assert_eq!(pace.need(4_000_000), 13_000_000 - limit);
        assert_eq!(pace.need(0), 0);
        // Split, it took 2 ms and its helpers 2 ms: owner work keeps the serial
        // measure and the next loop need not save what this one did.
        load.computed(2 * MS);
        let pace = load.pace(start + 28 * MS);
        assert_eq!(pace.work_ns, 14_000_000);
        assert_eq!(pace.need(1_000_000), 1_000_000);
        assert_eq!(load.frame(start + 30 * MS), 16_000_000);
        // The last frame's work predicts this one's from its start.
        assert_eq!(
            load.pace(start + 31 * MS).need(8_000_000),
            16_000_000 - limit
        );
    }

    // spec: PROC-MACH-1
    #[test]
    fn frames_set_the_period_only_after_three_timed_sleeps() {
        let mut load = FrameLoad::default();
        let mut now = Instant::now();
        load.frame(now);
        for _ in 0..2 {
            load.idle(20 * MS, true);
            now += 33 * MS;
            load.frame(now);
            assert_eq!(load.period_ns, DEFAULT_PERIOD_NS);
        }
        load.idle(20 * MS, true);
        now += 34 * MS;
        load.frame(now);
        assert_eq!(load.period_ns, 33_000_000);
        // Signalled waits do not pace frames: the period falls back at once.
        load.idle(20 * MS, false);
        now += 33 * MS;
        load.frame(now);
        assert_eq!(load.period_ns, DEFAULT_PERIOD_NS);
    }
}
