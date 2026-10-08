//! Opt-in, bounded wait distributions. Suspension segments are measured from
//! parking to readiness, then to dispatch; host wall time and guest time stay
//! separate, including explicitly skipped time. No guest identities are emitted.
use crate::{Object, Objects, ThunkTable, Wait, WakeReason};
use std::collections::VecDeque;
use std::time::Instant;

const CAPACITY: usize = 4096;
const NAMES: [&str; 9] = [
    "sleep",
    "sleep_ex",
    "object",
    "message",
    "multimedia_timer",
    "waitable_timer",
    "present",
    "yield",
    "other",
];

#[derive(Clone, Copy)]
struct Pending {
    class: usize,
    at: Instant,
    guest_us: u64,
    deadline: Option<u64>,
    skipped_us: u64,
}
#[derive(Clone, Copy)]
struct Ready {
    class: usize,
    at: Instant,
}
#[derive(Clone, Copy, Default)]
struct Sample {
    requested_us: Option<u64>,
    wall_us: u64,
    guest_us: u64,
    late_us: u64,
}
#[derive(Default)]
struct Distribution {
    count: u64,
    timeouts: u64,
    signaled: u64,
    skipped: u64,
    wall_total_us: u64,
    host_waits: u64,
    host_requested_ns: u64,
    host_elapsed_ns: u64,
    host_overshoot_ns: u64,
    samples: VecDeque<Sample>,
    host_samples: VecDeque<Sample>,
    dispatch: VecDeque<u64>,
}
impl Distribution {
    fn record(&mut self, sample: Sample, reason: WakeReason, skipped: bool) {
        self.count += 1;
        self.timeouts += u64::from(matches!(reason, WakeReason::Timeout));
        self.signaled += u64::from(!matches!(reason, WakeReason::Timeout));
        self.skipped += u64::from(skipped);
        self.wall_total_us = self.wall_total_us.saturating_add(sample.wall_us);
        if self.samples.len() == CAPACITY {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }
    fn json(&self) -> String {
        format!(
            concat!(
                "{{\"segments\":{},\"timeouts\":{},\"signaled\":{},\"skipped_segments\":{},",
                "\"wall_total_us\":{},\"retained\":{},\"requested_us\":{},\"wall_us\":{},",
                "\"guest_us\":{},\"lateness_us\":{},\"dispatch_queue_us\":{},",
                "\"host_waits\":{},\"host_requested_ns\":{},\"host_elapsed_ns\":{},\"host_overshoot_ns\":{},",
                "\"host_requested_us\":{},\"host_elapsed_us\":{},\"host_overshoot_us\":{}}}"
            ),
            self.count,
            self.timeouts,
            self.signaled,
            self.skipped,
            self.wall_total_us,
            self.samples.len(),
            percentiles(self.samples.iter().filter_map(|s| s.requested_us)),
            percentiles(self.samples.iter().map(|s| s.wall_us)),
            percentiles(self.samples.iter().map(|s| s.guest_us)),
            percentiles(self.samples.iter().map(|s| s.late_us)),
            percentiles(self.dispatch.iter().copied()),
            self.host_waits,
            self.host_requested_ns,
            self.host_elapsed_ns,
            self.host_overshoot_ns,
            percentiles(self.host_samples.iter().filter_map(|s| s.requested_us)),
            percentiles(self.host_samples.iter().map(|s| s.wall_us)),
            percentiles(self.host_samples.iter().map(|s| s.late_us)),
        )
    }
}
fn percentiles(values: impl Iterator<Item = u64>) -> String {
    let mut values: Vec<_> = values.collect();
    if values.is_empty() {
        return "null".into();
    }
    values.sort_unstable();
    let p = |percent: usize| values[(values.len() * percent).div_ceil(100) - 1];
    format!(
        "{{\"count\":{},\"p50\":{},\"p95\":{},\"p99\":{},\"max\":{}}}",
        values.len(),
        p(50),
        p(95),
        p(99),
        values.last().unwrap(),
    )
}

#[derive(Default)]
pub struct WaitDiagnostics {
    classes: Vec<usize>,
    pending: Vec<Option<Pending>>,
    ready: Vec<Option<Ready>>,
    rows: [Distribution; 9],
    skipped_us: u64,
}
impl WaitDiagnostics {
    pub(crate) fn retire(&mut self, id: u32) {
        if let Some(pending) = self.pending.get_mut(id as usize) {
            *pending = None;
        }
        if let Some(ready) = self.ready.get_mut(id as usize) {
            *ready = None;
        }
    }
    pub fn reset(&mut self) {
        // Drop partial suspensions crossing the observation boundary.
        *self = Self::default();
    }
    pub(crate) fn begin(
        &mut self,
        id: u32,
        thunk: u32,
        thunks: &ThunkTable,
        objects: &Objects,
        wait: &Wait,
        now: u64,
    ) {
        while self.classes.len() <= thunk as usize {
            let f = thunks.get(self.classes.len() as u32);
            self.classes.push(classify(&f.name));
        }
        let timer = |h| matches!(objects.get(h), Some(Object::Timer { .. }));
        let class = if match wait {
            Wait::Object { handle, .. } => timer(*handle),
            Wait::Objects { handles, .. } => handles.iter().any(|h| timer(*h)),
            _ => false,
        } {
            5
        } else {
            self.classes[thunk as usize]
        };
        let mut deadline = match wait {
            Wait::Until(at) => Some(*at),
            Wait::Object { deadline, .. } | Wait::Objects { deadline, .. } => *deadline,
            Wait::Yield => Some(now),
        };
        if class == 5 {
            let due = |h| match objects.get(h) {
                Some(Object::Timer { deadline, .. }) => *deadline,
                _ => None,
            };
            let timer_due = match wait {
                Wait::Object { handle, .. } => due(*handle),
                Wait::Objects { handles, .. } => handles.iter().filter_map(|h| due(*h)).min(),
                _ => None,
            };
            deadline = deadline.into_iter().chain(timer_due).min();
        }
        self.pending
            .resize(self.pending.len().max(id as usize + 1), None);
        self.ready
            .resize(self.ready.len().max(id as usize + 1), None);
        self.pending[id as usize] = Some(Pending {
            class,
            at: Instant::now(),
            guest_us: now,
            deadline,
            skipped_us: self.skipped_us,
        });
        self.ready[id as usize] = None;
    }
    pub(crate) fn wake(&mut self, id: u32, reason: WakeReason, now: u64) {
        let Some(pending) = self.pending.get_mut(id as usize).and_then(Option::take) else {
            return;
        };
        let at = Instant::now();
        self.rows[pending.class].record(
            Sample {
                requested_us: pending.deadline.map(|d| d.saturating_sub(pending.guest_us)),
                wall_us: at.duration_since(pending.at).as_micros() as u64,
                guest_us: now.saturating_sub(pending.guest_us),
                late_us: if matches!(reason, WakeReason::Timeout) || pending.class == 5 {
                    pending.deadline.map_or(0, |d| now.saturating_sub(d))
                } else {
                    0
                },
            },
            reason,
            self.skipped_us != pending.skipped_us,
        );
        self.ready[id as usize] = Some(Ready {
            class: pending.class,
            at,
        });
    }
    pub(crate) fn dispatch(&mut self, id: u32) {
        if let Some(ready) = self.ready.get_mut(id as usize).and_then(Option::take) {
            let rows = &mut self.rows[ready.class].dispatch;
            if rows.len() == CAPACITY {
                rows.pop_front();
            }
            rows.push_back(ready.at.elapsed().as_micros() as u64);
        }
    }
    pub(crate) fn skip(&mut self, us: u64) {
        self.skipped_us = self.skipped_us.saturating_add(us);
    }
    /// Attribute a host wait to every parked source. Multiple sources can share
    /// an interval, so their totals are inclusive and must never be summed.
    pub fn host_wait(
        &mut self,
        requested: Option<std::time::Duration>,
        elapsed: std::time::Duration,
        timed_out: bool,
        polling: bool,
    ) {
        let mut seen = [false; 9];
        seen[7] = polling;
        for pending in self.pending.iter().flatten() {
            seen[pending.class] = true;
        }
        if !seen.iter().any(|present| *present) {
            seen[8] = true;
        }
        for (class, present) in seen.into_iter().enumerate() {
            if !present {
                continue;
            }
            let row = &mut self.rows[class];
            if row.host_samples.len() == CAPACITY {
                row.host_samples.pop_front();
            }
            row.host_samples.push_back(Sample {
                requested_us: requested.map(|d| d.as_micros() as u64),
                wall_us: elapsed.as_micros() as u64,
                late_us: if timed_out {
                    requested.map_or(0, |d| elapsed.saturating_sub(d).as_micros() as u64)
                } else {
                    0
                },
                ..Sample::default()
            });
            row.host_waits += 1;
            row.host_elapsed_ns += elapsed.as_nanos() as u64;
            if let Some(requested) = requested {
                row.host_requested_ns += requested.as_nanos() as u64;
                if timed_out {
                    row.host_overshoot_ns += elapsed.saturating_sub(requested).as_nanos() as u64;
                }
            }
        }
    }
    pub fn json(&self) -> String {
        format!(
            "{{\"schema\":\"adv-wait-diagnostics-v1\",\"capacity_per_source\":{CAPACITY},\"sources\":{{{}}}}}",
            NAMES
                .iter()
                .zip(&self.rows)
                .map(|(name, row)| format!("\"{name}\":{}", row.json()))
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}
fn classify(name: &str) -> usize {
    let name = name.to_ascii_lowercase();
    if name == "sleep" {
        0
    } else if name == "sleepex" {
        1
    } else if name.contains("msgwait") || name.contains("getmessage") {
        3
    } else if name.contains("multimedia_timer") || name == "timesetevent" {
        4
    } else if name.contains("present") || name.contains("verticalblank") {
        6
    } else if name.contains("waitfor") || name.contains("ntwait") {
        2
    } else if name.contains("peekmessage") {
        7
    } else {
        8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_samples_keep_totals_and_separate_early_wakes_from_timeouts() {
        let mut row = Distribution::default();
        for i in 0..CAPACITY + 3 {
            row.record(
                Sample {
                    wall_us: i as u64,
                    requested_us: Some(1000),
                    ..Sample::default()
                },
                WakeReason::Timeout,
                true,
            );
        }
        row.record(Sample::default(), WakeReason::Notified, false);
        assert_eq!(row.samples.len(), CAPACITY);
        assert_eq!(row.count, (CAPACITY + 4) as u64);
        assert_eq!(row.timeouts, (CAPACITY + 3) as u64);
        assert_eq!(row.signaled, 1);
        assert_eq!(row.skipped, row.timeouts);
        assert!(row.json().contains("\"requested_us\":{\"count\":4095"));
    }
    #[test]
    fn percentile_empty_singleton_and_nearest_rank() {
        assert_eq!(percentiles(std::iter::empty()), "null");
        assert_eq!(
            percentiles([5].into_iter()),
            "{\"count\":1,\"p50\":5,\"p95\":5,\"p99\":5,\"max\":5}"
        );
        assert!(percentiles((1..=100).rev()).contains("\"p95\":95,\"p99\":99"));
    }
}
