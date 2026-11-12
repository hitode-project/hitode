//! Frame load (PROC-MACH-1): how much of a frame's splittable loop work the
//! backend should save by running it in blocks on other threads. Owner work
//! counts every run except settled polling, and a split loop at its serial
//! cost, so splitting does not change the measure it is chosen from. Owner
//! work may fill 60% of the frame period; frames that left the owner idle
//! set that period, whoever paced them.
use std::time::{Duration, Instant};

/// The period until frames show their own: 60 frames per second.
const DEFAULT_PERIOD_NS: u64 = 1_000_000_000 / 60;
/// Owner work may fill this share of the period: a 40% margin.
const LOAD: (u64, u64) = (3, 5);
/// A frame that left the owner idle for a quarter of its interval was paced
/// by a wait, not by its work.
const PACED: (u64, u64) = (3, 4);

#[derive(Debug)]
pub struct FrameLoad {
    work_ns: u64,
    split_ns: u64,
    /// Serial time of the last frame's offered loops.
    splittable_ns: u64,
    period_ns: u64,
    last: Option<Instant>,
    share: f32,
}

impl Default for FrameLoad {
    fn default() -> Self {
        Self {
            work_ns: 0,
            split_ns: 0,
            splittable_ns: 0,
            period_ns: DEFAULT_PERIOD_NS,
            last: None,
            share: 0.0,
        }
    }
}

fn nanos(time: Duration) -> u64 {
    time.as_nanos().min(u128::from(u64::MAX)) as u64
}

impl FrameLoad {
    /// An ordinary owner run that was not settled polling.
    pub(crate) fn ran(&mut self, time: Duration) {
        self.work_ns = self.work_ns.saturating_add(nanos(time));
    }

    /// An owner computation, with the time its helpers spent.
    pub(crate) fn computed(&mut self, time: Duration, helpers: Duration) {
        let serial = nanos(time).saturating_add(nanos(helpers));
        self.work_ns = self.work_ns.saturating_add(serial);
        self.split_ns = self.split_ns.saturating_add(serial);
    }

    /// A completed frame: returns its owner work in ns.
    pub(crate) fn frame(&mut self, now: Instant) -> u64 {
        let work = std::mem::take(&mut self.work_ns);
        if let Some(interval) = self.last.map(|last| nanos(now - last))
            && work * PACED.1 <= interval * PACED.0
        {
            self.period_ns = interval;
        }
        self.last = Some(now);
        let split = std::mem::take(&mut self.split_ns);
        if split != 0 {
            self.splittable_ns = split;
        }
        let need = work.saturating_sub(self.period_ns * LOAD.0 / LOAD.1);
        // Until the frame offers its loops, any idle helper may serve.
        self.share = if need == 0 {
            0.0
        } else if self.splittable_ns == 0 {
            1.0
        } else {
            (need as f64 / self.splittable_ns as f64).min(1.0) as f32
        };
        work
    }

    pub fn share(&self) -> f32 {
        self.share
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    // spec: PROC-MACH-1
    #[test]
    fn splitting_saves_only_the_work_past_the_margin() {
        let mut load = FrameLoad::default();
        let start = Instant::now();
        load.frame(start);
        load.ran(6 * MS);
        load.frame(start + 16 * MS);
        assert_eq!(load.share(), 0.0);
        assert_eq!(load.period_ns, 16_000_000);
        // 12 ms of work against a 9.6 ms limit, before any loop was offered.
        load.ran(12 * MS);
        load.frame(start + 32 * MS);
        assert_eq!(load.share(), 1.0);
        // Splitting keeps the serial measure: 4 ms of split loops must save 2.4 ms.
        load.ran(8 * MS);
        load.computed(2 * MS, 2 * MS);
        load.frame(start + 44 * MS);
        assert!((load.share() - 0.6).abs() < 1e-6);
        assert_eq!(load.period_ns, 16_000_000);
    }
}
