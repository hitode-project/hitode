//! Bounded, address-free aggregates for asynchronous compilation lifetimes.
use super::Executor;
use crate::{CompileStrategy, Region, compiler::Completion};
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Histogram {
    count: u64,
    sum: u64,
    max: u64,
    // Bucket zero is zero; bucket i contains [2^(i-1), 2^i-1].
    bins: [u64; 65],
}
impl Default for Histogram {
    fn default() -> Self {
        Self {
            count: 0,
            sum: 0,
            max: 0,
            bins: [0; 65],
        }
    }
}
impl Histogram {
    fn record(&mut self, value: u64) {
        self.count = self.count.saturating_add(1);
        self.sum = self.sum.saturating_add(value);
        self.max = self.max.max(value);
        let bucket = (64 - value.leading_zeros()) as usize;
        self.bins[bucket] = self.bins[bucket].saturating_add(1);
    }
    fn time(&mut self, value: Duration) {
        self.record(value.as_micros().min(u128::from(u64::MAX)) as u64);
    }
    fn json(&self) -> String {
        format!(
            "{{\"count\":{},\"sum\":{},\"max\":{},\"log2_bins\":{:?}}}",
            self.count, self.sum, self.max, self.bins
        )
    }
}

#[derive(Default, Clone, Debug)]
struct Phase {
    deferred_us: Histogram,
    queue_us: Histogram,
    service_us: Histogram,
    publication_us: Histogram,
    age_us: Histogram,
    instructions: Histogram,
    decoded_bytes: Histogram,
    window_bytes: Histogram,
    native_bytes: Histogram,
    waiting_units: Histogram,
    // Deferred, worker queue, compiling, completed but not collected.
    interpreter_units: [u64; 4],
}
impl Phase {
    fn json(&self) -> String {
        let fields = [
            ("deferred_us", &self.deferred_us),
            ("queue_us", &self.queue_us),
            ("service_us", &self.service_us),
            ("publication_us", &self.publication_us),
            ("age_us", &self.age_us),
            ("instructions", &self.instructions),
            ("decoded_bytes", &self.decoded_bytes),
            ("window_bytes", &self.window_bytes),
            ("native_bytes", &self.native_bytes),
            ("waiting_units", &self.waiting_units),
        ]
        .map(|(name, h)| format!("\"{name}\":{}", h.json()))
        .join(",");
        format!(
            "{{{fields},\"interpreter_units_by_stage\":{:?}}}",
            self.interpreter_units
        )
    }
}

#[derive(Default, Clone, Debug)]
pub struct CompilationSchedule {
    phases: [Phase; 2],
    pub(super) duplicate_requests: u64,
    pub(super) capacity_rejections: u64,
    pub(super) window_deferrals: u64,
    pub(super) worker_full: u64,
    pub(super) compile_threads: usize,
    pub(super) available_cores: usize,
    in_flight: Histogram,
    active_at_start: Histogram,
}

struct Waiting {
    born: Instant,
    units: u64,
}
struct Pending {
    waiting: Waiting,
    start: u32,
    len: usize,
    phase: usize,
    stage: Arc<AtomicUsize>,
}
#[derive(Default)]
pub(super) struct Tracker {
    deferred: BTreeMap<u32, Waiting>,
    pending: BTreeMap<u64, Pending>,
}
fn phase(strategy: CompileStrategy) -> usize {
    usize::from(strategy == CompileStrategy::Speed)
}

impl Executor {
    pub(super) fn schedule_deferred(&mut self, start: u32) {
        self.schedule
            .deferred
            .retain(|pc, _| self.deferred_compilations.contains_key(pc));
        self.schedule.deferred.entry(start).or_insert(Waiting {
            born: Instant::now(),
            units: 0,
        });
    }
    pub(super) fn schedule_submitted(&mut self, region: &Region, ticket: u64) {
        let waiting = self
            .schedule
            .deferred
            .remove(&region.start)
            .unwrap_or(Waiting {
                born: Instant::now(),
                units: 0,
            });
        let stats = &mut self.stats.compilation_schedule;
        let index = phase(region.compile_strategy);
        stats.phases[index].deferred_us.time(waiting.born.elapsed());
        stats.phases[index].window_bytes.record(region.len as u64);
        stats
            .in_flight
            .record(self.compiler.as_ref().unwrap().outstanding() as u64);
        self.schedule.pending.insert(
            ticket,
            Pending {
                waiting,
                start: region.start,
                len: region.len,
                phase: index,
                stage: self.compiler.as_ref().unwrap().stage(ticket),
            },
        );
    }
    pub(super) fn schedule_completed(&mut self, completion: &Completion) {
        let stats = &mut self.stats.compilation_schedule;
        stats
            .active_at_start
            .record(completion.active_at_start as u64);
        let p = &mut stats.phases[phase(completion.region.compile_strategy)];
        p.queue_us.time(completion.queue_wait);
        p.service_us.time(completion.duration);
        p.publication_us.time(completion.completed_at.elapsed());
        if let Some(pending) = self.schedule.pending.remove(&completion.ticket) {
            p.age_us.time(pending.waiting.born.elapsed());
            p.waiting_units.record(pending.waiting.units);
        }
        if let Ok(compiled) = &completion.result {
            p.instructions.record(compiled.instruction_count as u64);
            p.decoded_bytes.record(compiled.decoded_bytes as u64);
            p.native_bytes.record(compiled.code_size as u64);
        }
    }
    pub(super) fn schedule_interpreted(&mut self, pc: u32, units: u32, elapsed_ns: u64) {
        // Attribute each chunk once, to the oldest matching request window.
        // A window can include CFG gaps; these are blocked discovery units,
        // not a claim that the eventual native unit will cover every instruction.
        if let Some(pending) = self
            .schedule
            .pending
            .iter_mut()
            .filter(|(ticket, p)| self.pending_regions.get(&p.start) == Some(ticket))
            .find(|(_, p)| pc.wrapping_sub(p.start) < p.len as u32)
            .map(|(_, p)| p)
        {
            pending.waiting.units = pending.waiting.units.saturating_add(u64::from(units));
            let stage = pending.stage.load(Ordering::Relaxed).min(2) + 1;
            // Sample the stage at the chunk boundary, as for the unit counts.
            self.stats.compile_wait_stage_ns[stage] += elapsed_ns;
            self.stats.compilation_schedule.phases[pending.phase].interpreter_units[stage] +=
                u64::from(units);
        } else if let Some((&start, input)) = self
            .deferred_compilations
            .iter()
            .filter(|(_, s)| pc.wrapping_sub(s.region.start) < s.region.len as u32)
            .min_by_key(|(start, _)| self.schedule.deferred.get(start).map(|w| w.born))
        {
            if let Some(waiting) = self.schedule.deferred.get_mut(&start) {
                self.stats.compile_wait_stage_ns[0] += elapsed_ns;
                waiting.units = waiting.units.saturating_add(u64::from(units));
                self.stats.compilation_schedule.phases[phase(input.region.compile_strategy)]
                    .interpreter_units[0] += u64::from(units);
            }
        }
    }
    /// Fixed-size aggregates only. Live ages include unfinished requests rather
    /// than silently excluding them from the observed latency distribution.
    pub fn compilation_schedule_json(&self) -> String {
        let stats = &self.stats.compilation_schedule;
        let mut live_age = Histogram::default();
        let mut live_units = Histogram::default();
        for waiting in self.schedule.pending.values().map(|p| &p.waiting).chain(
            self.schedule
                .deferred
                .iter()
                .filter(|(s, _)| self.deferred_compilations.contains_key(s))
                .map(|(_, w)| w),
        ) {
            live_age.time(waiting.born.elapsed());
            live_units.record(waiting.units);
        }
        format!(
            concat!(
                "{{\"version\":1,\"compile_threads\":{},\"available_cores\":{},",
                "\"duplicate_requests\":{},\"capacity_rejections\":{},\"window_deferrals\":{},\"worker_full\":{},",
                "\"in_flight_at_submit\":{},\"active_at_start\":{},\"unfinished_age_us\":{},\"unfinished_waiting_units\":{},",
                "\"fast\":{},\"speed\":{}}}"
            ),
            stats.compile_threads,
            stats.available_cores,
            stats.duplicate_requests,
            stats.capacity_rejections,
            stats.window_deferrals,
            stats.worker_full,
            stats.in_flight.json(),
            stats.active_at_start.json(),
            live_age.json(),
            live_units.json(),
            stats.phases[0].json(),
            stats.phases[1].json()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn histogram_keeps_zero_edges_and_overflow_in_fixed_storage() {
        let mut h = Histogram::default();
        for n in [0, 1, 2, 3, 4, u64::MAX] {
            h.record(n);
        }
        assert_eq!(h.count, 6);
        assert_eq!(h.bins[0..4], [1, 1, 2, 1]);
        assert_eq!(h.bins[64], 1);
        assert_eq!(h.sum, u64::MAX);
        assert_eq!(h.bins.iter().sum::<u64>(), h.count);
    }

    #[test]
    fn waiting_units_count_once_and_ignore_retired_request_identity() {
        let mut m = Executor::new(0x1000, 0x4000).unwrap();
        for ticket in 1..=3 {
            m.schedule.pending.insert(
                ticket,
                Pending {
                    waiting: Waiting {
                        born: Instant::now(),
                        units: 0,
                    },
                    start: if ticket < 3 { 0x1000 } else { 0x1001 },
                    len: 16,
                    phase: 0,
                    stage: Arc::new(AtomicUsize::new(0)),
                },
            );
        }
        m.pending_regions.insert(0x1000, 2);
        m.pending_regions.insert(0x1001, 3);
        for stage in 0..3 {
            m.schedule.pending[&2].stage.store(stage, Ordering::Relaxed);
            m.schedule_interpreted(0x1002, 7, 11);
        }
        assert_eq!(m.schedule.pending[&1].waiting.units, 0);
        assert_eq!(m.schedule.pending[&2].waiting.units, 21);
        assert_eq!(m.schedule.pending[&3].waiting.units, 0);
        assert_eq!(m.stats.compile_wait_stage_ns, [0, 11, 11, 11]);
        assert_eq!(
            m.stats.compilation_schedule.phases[0].interpreter_units,
            [0, 7, 7, 7]
        );
        m.set_async_compilation(false);
        assert!(m.schedule.pending.is_empty());
    }

    #[test]
    fn deferred_metadata_is_pruned_and_sync_has_no_async_samples() {
        let mut m = Executor::new(0x1000, 0x4000).unwrap();
        for start in 0..5000 {
            m.schedule_deferred(start);
        }
        assert_eq!(m.schedule.deferred.len(), 1);
        m.protect(0x1000, 4096, crate::READ | crate::WRITE | crate::EXEC)
            .unwrap();
        m.write(0x1000, &[0x40, 0xc3]).unwrap();
        m.add_region(Region::new(0x1000, 2)).unwrap();
        assert_eq!(m.stats.compilation_schedule.phases[0].service_us.count, 0);
        assert_eq!(m.stats.compilation_schedule.in_flight.count, 0);
    }

    #[test]
    fn indexed_pending_entries_contribute_to_stage_timing() {
        let mut m = Executor::new(0x1000, 0x4000).unwrap();
        m.protect(0x1000, 4096, crate::READ | crate::WRITE | crate::EXEC)
            .unwrap();
        m.write(0x1000, &[0x40, 0xeb, 0xfd]).unwrap();
        m.refresh().unwrap();
        m.index_region(Region::new(0x1000, 3));
        m.code.entry_owner.insert(0x1000, 0x1000);
        m.enable_auto_jit(true);
        m.pending_regions.insert(0x1000, 1);
        m.schedule.pending.insert(
            1,
            Pending {
                waiting: Waiting {
                    born: Instant::now(),
                    units: 0,
                },
                start: 0x1000,
                len: 3,
                phase: 0,
                stage: Arc::new(AtomicUsize::new(1)),
            },
        );
        m.enable_frame_timing(true);
        m.state.eip = 0x1000;
        m.run(0x4000, 100).unwrap();
        assert!(m.stats.compile_wait_execution_ns > 0);
        assert_eq!(
            m.stats.compile_wait_stage_ns,
            [0, 0, m.stats.compile_wait_execution_ns, 0]
        );
        assert_eq!(m.schedule.pending[&1].waiting.units, 100);
    }
}
