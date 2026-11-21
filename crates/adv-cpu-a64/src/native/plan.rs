//! Per-site policy for tiers, versions, rebuilds and worker offers (A64A-ROLE-6).
//!
//! The owner passes observations to the arbiter and applies its decisions
//! through the compiler and code store.
//!
//! | State | Event | Transition |
//! |---|---|---|
//! | Baseline | Back-edge threshold | Profile and request a build |
//! | Profiling | Compile done | Install a resident form or decline |
//! | Resident | Context miss, cold exit, or probation end | Train, rebuild, or decline |
//! | Resident | Unchecked host faults | Two at one site: check faulting sites on its stream |
//! | Resident | Other cold exits | At least two at a site and more than one per eight iterations: cut, refuse, or demote |
//! | Declined | Code invalidation | Reset discovery |
//! | Offerable | Admission failure or job completion | Cache, back off, or re-offer |
//!
//! The arbiter applies the shared prices to owner and worker observations.

use crate::ExecutionContext;
use crate::policy::worker::COMPUTATION_SLICE;
use adv_cpu_core::backend::{Completion, OfferGate, Pace};
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::Cell;
use std::time::Duration;

use super::compile::{self, ResidentCold};
use super::store::{self, Tier1Chain};
use crate::facts::Key;
use crate::policy::proved::{ROW_PROBE_WORDS, RowPrice};
pub(super) use crate::policy::resident::{
    CHECKED_FAULTS, HOT, MISSES, PROBATION, PROBES, SAMPLE, VERSIONS,
};
use crate::policy::tier1::{CHAIN_LEN, CHECK_AFTER};
use crate::policy::worker::RESPECIALIZE;
pub(super) use crate::policy::worker::{REOFFER, WAKE_NS};

const NO_LIMIT: u32 = i32::MAX as u32;
const SHORT_JOB: Duration = Duration::from_nanos(WAKE_NS);

/// Accumulated AArch64 instruction words. Settled row forms keep their
/// measured credit while successful-work sampling is patched out.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Ledger {
    pub saved: u128,
    pub wasted: u128,
}

impl Ledger {
    pub fn record(&mut self, saved: u32, units: u64, cost: u32, attempts: u64) {
        self.saved += u128::from(saved) * u128::from(units);
        self.wasted += u128::from(cost) * u128::from(attempts);
    }

    pub fn over_budget(self, words: u64) -> bool {
        self.wasted > self.saved + u128::from(words)
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct RowLedger {
    pub total: Ledger,
    pub failures: [u64; 6],
    pub rows: u64,
    pub entries: u64,
}

impl RowLedger {
    pub fn priced(
        price: RowPrice,
        (entries, rows, work): (u64, u64, u64),
        failures: [u64; 6],
    ) -> Self {
        let mut total = Ledger::default();
        let rejected = failures.iter().sum::<u64>();
        total.record(price.saved, work, price.guard, entries + rejected);
        total.record(price.row_savings, rows, 0, 0);
        Self {
            total,
            failures,
            rows,
            entries,
        }
    }

    pub fn add(&mut self, other: Self) {
        self.total.saved += other.total.saved;
        self.total.wasted += other.total.wasted;
        self.rows += other.rows;
        self.entries += other.entries;
        for (failures, added) in self.failures.iter_mut().zip(other.failures) {
            *failures += added;
        }
    }

    pub fn router(&mut self, words: u32, failures: [u64; 6]) {
        self.total.record(0, 0, words, failures.iter().sum());
        for (count, additional) in self.failures.iter_mut().zip(failures) {
            *count += additional;
        }
    }
}

pub(super) enum RowAction {
    Keep,
    Sample(bool),
    Withdraw,
}

pub(super) struct JobPlan {
    /// The serial cost of a split loop in host ticks.
    pub split_ticks: Cell<u64>,
    pub splits: bool,
    pub scoped: bool,
    pub split_minimum: Cell<u64>,
    pub scope_offers: Cell<u64>,
    pub scope_splits: Cell<u64>,
    misses: Cell<u32>,
    /// Optimized forms requested, and whether one is being built.
    forms: Cell<u8>,
    building: Cell<bool>,
    pub reoffer: Cell<u32>,
    /// Eligible offers to skip after a refused or short job.
    backoff: Cell<u32>,
    pub mode: Cell<Mode>,
    /// Instructions and time per outer iteration, once a job showed them.
    pub per_iteration: Cell<Option<(u64, f64)>>,
    /// Serial ns of the last job that showed them.
    cost: Cell<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Whole(u8),
    Trial,
    Iteration,
    WholeOnly,
}

impl JobPlan {
    /// Records the instructions and time per outer iteration of a job that ran
    /// `iterations` of them.
    pub fn learn(
        prepared: &JobPlan,
        done: &Completion<Option<ExecutionContext>>,
        iterations: u32,
        execution: Duration,
        wake: f64,
    ) {
        if iterations == 0 || done.requires_owner {
            return;
        }
        let n = u64::from(iterations);
        let ns = execution.as_nanos() as f64;
        prepared.set_cost(ns as u64);
        if prepared.scoped && ns > 0.0 {
            prepared.split_minimum.set(
                ((1.0 + 2.0 * BLOCKS_PER_THREAD) * wake * n as f64 / ns)
                    .ceil()
                    .max(2.0) as u64,
            );
        }
        prepared
            .per_iteration
            .set(Some(((done.retired / n).max(1), ns / n as f64)));
    }

    /// `iterates` when the code can stop after one outer iteration.
    pub fn new(iterates: bool, splits: bool) -> Self {
        let mode = if iterates {
            Mode::Whole(0)
        } else {
            Mode::WholeOnly
        };
        Self {
            splits,
            scoped: false,
            split_minimum: Cell::new(2),
            scope_offers: Cell::new(0),
            scope_splits: Cell::new(0),
            split_ticks: Cell::new(0),
            misses: Cell::new(0),
            forms: Cell::new(0),
            building: Cell::new(false),
            reoffer: Cell::new(1),
            backoff: Cell::new(0),
            mode: Cell::new(mode),
            per_iteration: Cell::new(None),
            cost: Cell::new(0),
        }
    }

    /// Counts an entry whose state the optimized form did or did not admit.
    pub fn admitted(&self, admitted: bool) {
        let misses = &self.misses;
        misses.set(if admitted {
            0
        } else {
            (misses.get() + 1).min(NO_LIMIT)
        });
    }

    /// Whether to build a form for the state at this entry: the first one,
    /// or up to `RESPECIALIZE` more after `MISSES` entries it did not admit.
    pub fn wants_form(&self) -> bool {
        !self.building.get()
            && match self.forms.get() {
                0 => true,
                forms => forms <= RESPECIALIZE && self.misses.get() >= MISSES,
            }
    }

    pub fn requested(&self) {
        self.forms.set(self.forms.get() + 1);
        self.building.set(true);
        self.misses.set(0);
    }

    pub fn built(&self) {
        self.building.set(false);
    }

    pub fn scope_cost(&self, count: u32) -> u64 {
        self.per_iteration
            .get()
            .map_or(WAKE_NS * (1 + 2 * BLOCKS_PER_THREAD as u64), |(_, ns)| {
                (f64::from(count) * ns) as u64
            })
    }

    pub fn scope_threshold(&self, enabled: bool) -> Option<u64> {
        (self.scoped && enabled).then(|| self.split_minimum.get())
    }

    pub fn gate(&self) -> store::OfferEntry {
        store::OfferEntry {
            counter: self.reoffer.as_ptr() as u64,
            split: self.splits,
        }
    }

    pub fn offer_due(&self, gate: &OfferGate, now: fn() -> u64) -> bool {
        use std::sync::atomic::Ordering::Relaxed;

        let ticks = self.split_ticks.get();
        let due = gate.stale.load(Relaxed) != 0
            || ticks != 0
                && (gate.split_over.load(Relaxed) != 0
                    || now().wrapping_add(ticks) > gate.split_deadline.load(Relaxed));
        if !due {
            return false;
        }
        let skip = self.backoff.get();
        if skip != 0 {
            self.backoff.set(skip - 1);
            return false;
        }
        true
    }

    pub fn iteration(&self) -> bool {
        matches!(self.mode.get(), Mode::Trial | Mode::Iteration)
    }

    /// The serial ns a job of this computation last took, 0 until one ran.
    pub fn cost(&self) -> u64 {
        self.cost.get()
    }

    pub fn defer_offer(&self) {
        self.reoffer.set(REOFFER);
    }

    pub fn back_off(&self, count: u32) {
        self.backoff.set(count);
    }

    pub fn sample_offer(&self) -> bool {
        let remaining = self.reoffer.get() - 1;
        if remaining == 0 {
            self.defer_offer();
            true
        } else {
            self.reoffer.set(remaining);
            false
        }
    }

    pub fn set_cost(&self, ns: u64) {
        self.cost.set(ns);
        let ticks = if self.splits && ns > 0 {
            let hz = u128::from(adv_cpu_core::backend::host_tick_hz());
            (u128::from(ns) * hz)
                .div_ceil(1_000_000_000)
                .min(u128::from(u64::MAX)) as u64
        } else {
            0
        };
        self.split_ticks.set(ticks);
    }

    pub fn observe(
        &self,
        entry: u32,
        iteration: bool,
        deadline_recall: bool,
        completion: &adv_cpu_core::backend::Completion<Option<crate::ExecutionContext>>,
        slice: u32,
    ) {
        if iteration {
            if self.mode.get() == Mode::Trial
                && !completion.requires_owner
                && completion.context.as_ref().unwrap().cpu().eip == entry
            {
                self.mode.set(if completion.elapsed >= SHORT_JOB {
                    Mode::Iteration
                } else {
                    Mode::WholeOnly
                });
            }
            return;
        }
        if let Mode::Whole(recalls) = self.mode.get()
            && deadline_recall
        {
            self.mode.set(if recalls >= 1 {
                self.reoffer.set(1);
                Mode::Trial
            } else {
                Mode::Whole(recalls + 1)
            });
        }
        if completion.requires_owner || matches!(self.mode.get(), Mode::Trial | Mode::Iteration) {
            return;
        }
        // Short jobs do not amortize admission and queueing. Give them
        // another trial after about one ordinary slice.
        if completion.elapsed < SHORT_JOB {
            let skip = (u64::from(slice) / completion.retired.max(1)) as u32;
            self.back_off(skip.max(REOFFER));
        }
        self.defer_offer();
    }
}

/// Blocks per thread, so the last block to finish leaves the others idle
/// for about a quarter of a thread's part. A block is at most one slice.
const BLOCKS_PER_THREAD: f64 = 4.0;
/// The owner's fixed work in a split besides its blocks: admission and merge.
const ROUND_NS: f64 = 10_000.0;
/// Rolling median of three claimed wakes, seeded with the startup estimate.
pub(super) struct WakeEstimate {
    recent: [u64; 2],
    pub ns: u64,
}

impl Default for WakeEstimate {
    fn default() -> Self {
        Self {
            recent: [WAKE_NS; 2],
            ns: WAKE_NS,
        }
    }
}

impl WakeEstimate {
    pub fn record(&mut self, ns: u64) {
        let [older, newer] = self.recent;
        let ns = ns.max(1);
        self.ns = ns.clamp(older.min(newer), older.max(newer));
        self.recent = [newer, ns];
    }
}

pub(super) fn rows_per_slice(retired: u64) -> u32 {
    (u64::from(COMPUTATION_SLICE) / retired).max(1) as u32
}

/// The fewest helpers whose blocks cut `serial` ns by `need` ns, or all
/// that are worth waking, given the `wake` latency and the most a `slice`
/// block takes. One is worth waking only if its part of the loop exceeds
/// four wake latencies.
pub(super) fn helpers(serial: f64, (wake, slice): (f64, f64), need: f64, most: usize) -> usize {
    if need <= 0.0 {
        return 0;
    }
    let worth = ((serial - wake) / (BLOCKS_PER_THREAD * wake) - 1.0).max(0.0) as usize;
    let most = most.min(worth);
    let target = serial - need;
    (1..=most)
        .find(|&h| {
            let part = (serial - wake) / (h + 1) as f64;
            wake + part + (part / BLOCKS_PER_THREAD).min(slice) + ROUND_NS <= target
        })
        .unwrap_or(most)
}

pub(super) fn split_work(
    iterations: u32,
    (retired, ns): (u64, f64),
    wake: u64,
    pace: Pace,
    most: usize,
) -> Option<(usize, u32)> {
    let slice = rows_per_slice(retired);
    let serial = f64::from(iterations) * ns;
    let need = pace.need(serial as u64) as f64;
    let helpers = helpers(serial, (wake as f64, f64::from(slice) * ns), need, most);
    if helpers == 0 {
        return None;
    }
    let blocks = (helpers + 1) as f64 * BLOCKS_PER_THREAD;
    Some((
        helpers,
        slice.min((f64::from(iterations) / blocks).ceil() as u32),
    ))
}

#[derive(Default)]
pub(super) struct Arbiter {
    sites: FxHashMap<u32, SitePlan>,
    checked: usize,
    pub(super) baseline_only: bool,
    pub(super) offer_resume: Option<u32>,
}

#[derive(Default)]
pub(super) struct SitePlan {
    seen: bool,
    samples: u8,
    promoted: bool,
    strikes: u32,
    checked: bool,
    pub(super) force_tier1: bool,
    counter: Option<Box<Cell<u32>>>,
    pub(super) resident: Option<Box<ResidentPlan>>,
    job: Option<Box<JobPlan>>,
}

pub(super) enum Arrival<'a> {
    Baseline,
    Profiling,
    Resident,
    Declined,
    Offerable(&'a JobPlan),
}

impl Arbiter {
    pub(super) fn new(baseline_only: bool) -> Self {
        Self {
            baseline_only,
            ..Self::default()
        }
    }

    pub(super) fn site(&mut self, eip: u32) -> &mut SitePlan {
        self.sites.entry(eip).or_default()
    }

    pub(super) fn arrival(&self, eip: u32) -> Arrival<'_> {
        if self.baseline_only {
            return Arrival::Baseline;
        }
        let Some(site) = self.sites.get(&eip) else {
            return Arrival::Baseline;
        };
        if let Some(job) = &site.job {
            return Arrival::Offerable(job);
        }
        match site.resident.as_ref().map(|head| &head.state) {
            None => Arrival::Baseline,
            Some(ResidentState::Profiling) => Arrival::Profiling,
            Some(ResidentState::Installed) => Arrival::Resident,
            Some(ResidentState::Declined(_)) => Arrival::Declined,
        }
    }

    pub(super) fn checked(&self, eip: u32) -> bool {
        self.sites.get(&eip).is_some_and(|site| site.checked)
    }

    pub(super) fn mark_checked(&mut self, eip: u32) {
        if !std::mem::replace(&mut self.site(eip).checked, true) {
            self.checked += 1;
        }
    }

    pub(super) fn checked_count(&self) -> usize {
        self.checked
    }

    pub(super) fn ran(&self, eip: u32, first_visits: bool) -> bool {
        !first_visits || self.sites.get(&eip).is_some_and(|site| site.seen)
    }

    pub(super) fn first_visit(&mut self, eip: u32, enabled: bool, placed: bool) -> bool {
        enabled && !placed && !std::mem::replace(&mut self.site(eip).seen, true)
    }

    pub(super) fn promoted(&self, eip: u32) -> bool {
        self.sites.get(&eip).is_some_and(|site| site.promoted)
    }

    pub(super) fn sample(&mut self, eip: u32, threshold: u8) -> bool {
        let plan = self.site(eip);
        if plan.promoted {
            return false;
        }
        plan.samples += 1;
        if plan.samples < threshold {
            return false;
        }
        plan.samples = 0;
        plan.promoted = true;
        true
    }

    pub(super) fn strike(&mut self, eip: u32) -> bool {
        let site = self.site(eip);
        site.strikes += 1;
        if site.strikes < CHECK_AFTER {
            return false;
        }
        self.mark_checked(eip);
        true
    }

    pub(super) fn routing(&self, eip: u32) -> (bool, bool) {
        let resident = self.resident(eip);
        let counting =
            resident.is_none() && self.counter_at(eip).is_some_and(|c| c.get() != u32::MAX);
        (counting, resident.is_none_or(ResidentPlan::declined))
    }

    pub(super) fn discover(&mut self, eip: u32, source: u32, code: usize) {
        self.site(eip).resident = Some(Box::new(ResidentPlan::new(code, source)));
    }

    pub(super) fn counter(&mut self, eip: u32) -> (u64, u32) {
        let plan = self.site(eip);
        let value = if plan.force_tier1 { u32::MAX } else { HOT };
        let count = plan.counter.get_or_insert_with(|| Box::new(value.into()));
        (std::ptr::from_ref(&**count) as u64, count.get())
    }

    pub(super) fn counter_at(&self, eip: u32) -> Option<&Cell<u32>> {
        self.sites.get(&eip)?.counter.as_deref()
    }

    pub(super) fn resident(&self, eip: u32) -> Option<&ResidentPlan> {
        self.sites.get(&eip)?.resident.as_deref()
    }

    pub(super) fn residents(&self) -> impl Iterator<Item = (u32, &ResidentPlan)> {
        self.sites
            .iter()
            .filter_map(|(&eip, site)| Some((eip, site.resident.as_deref()?)))
    }

    pub(super) fn resident_mut(&mut self, eip: u32) -> Option<&mut ResidentPlan> {
        self.sites.get_mut(&eip)?.resident.as_deref_mut()
    }

    pub(super) fn add_job(&mut self, eip: u32, iterates: bool, splits: bool) -> &JobPlan {
        self.site(eip)
            .job
            .insert(Box::new(JobPlan::new(iterates, splits)))
    }

    pub(super) fn job(&self, eip: u32) -> Option<&JobPlan> {
        self.sites.get(&eip)?.job.as_deref()
    }

    pub(super) fn invalidate(&mut self, eip: u32) {
        if let Some(site) = self.sites.get_mut(&eip) {
            if let Some(count) = &site.counter {
                count.set(if site.force_tier1 { u32::MAX } else { HOT });
            }
            site.promoted = false;
            site.resident = None;
            site.job = None;
        }
    }

    pub(super) fn clear_overlays(&mut self) {
        for site in self.sites.values_mut() {
            site.counter = None;
            site.resident = None;
            site.job = None;
        }
    }

    pub(super) fn reset_samples(&mut self) {
        for site in self.sites.values_mut() {
            site.samples = 0;
        }
    }
}

pub(super) enum Tier1Miss {
    Match(usize),
    Build {
        key: Key,
        position: usize,
        joining: bool,
    },
}

pub(super) fn tier1_miss(chain: &Tier1Chain, observed: Key) -> Tier1Miss {
    let key = observed.project(chain.mask);
    if let Some(version) = chain.versions.iter().find(|v| key.proves(v.key)) {
        return Tier1Miss::Match(version.entry.unit);
    }
    let position = chain.versions.len().min(CHAIN_LEN - 1);
    let joining = chain.versions.len() == CHAIN_LEN;
    let key = if joining {
        chain.versions[position].key.join(key)
    } else {
        key
    };
    Tier1Miss::Build {
        key,
        position,
        joining,
    }
}

pub(super) enum ColdAction {
    Observe,
    FinishProbation,
    CheckStream,
    Ignore,
}

pub(super) struct Probation {
    pub iteration_retired: u64,
    pub saved_words: u64,
    pub entry_exit_words: u64,
    pub cold_entries: u64,
    pub cold_retired: u64,
}

impl Probation {
    pub(super) fn price(
        tier1: Option<u32>,
        per_iteration: u32,
        iteration_retired: u32,
        entry_exit: u32,
    ) -> Result<Option<Self>, Decline> {
        let Some(tier1) = tier1.filter(|_| iteration_retired != 0) else {
            return Ok(None);
        };
        if tier1 <= per_iteration {
            return Err(Decline::Unprofitable);
        }
        Ok(Some(Self {
            iteration_retired: u64::from(iteration_retired),
            saved_words: u64::from(tier1 - per_iteration),
            entry_exit_words: u64::from(entry_exit),
            cold_entries: 0,
            cold_retired: 0,
        }))
    }

    pub fn profitable(&self, entries: u64, retired: u64) -> bool {
        let entries = entries + self.cold_entries;
        let retired = retired + self.cold_retired;
        retired / entries / self.iteration_retired * self.saved_words >= self.entry_exit_words
    }
}

pub(super) struct ResidentPlan {
    /// The Tier-1 address at discovery identifies this trace's lifetime.
    pub code: usize,
    /// The Tier-1 unit containing the back edge that reached the threshold.
    pub source: u32,
    pub probes: u8,
    pub(super) link_samples: FxHashMap<usize, (usize, u8)>,
    pub probation: Vec<Option<Probation>>,
    pub checked: FxHashSet<u32>,
    pub refused: FxHashSet<u32>,
    pub cuts: FxHashSet<u32>,
    pub cold_counts: FxHashMap<(u32, ResidentCold), u64>,
    pub(super) cold_window: Option<ColdWindow>,
    /// Versions to rebuild once the current request settles.
    pub rebuilds: u8,
    pub pending: Option<compile::Task>,
    state: ResidentState,
    pub(super) has_proof: bool,
    recorded_gap: bool,
    pub(super) row_targets: Vec<u32>,
    pub row: RowLedger,
    // Prices change on rebuild. Keep credits already measured at their old price.
    row_history: Vec<RowLedger>,
}

#[derive(Default)]
pub(super) struct ColdWindow {
    start_entries: u64,
    start_iterations: u64,
    cold_iterations: u64,
    pub(super) cold_entries: u64,
    exits: FxHashMap<(u32, ResidentCold), u64>,
}

impl ResidentPlan {
    pub fn new(code: usize, source: u32) -> Self {
        Self {
            code,
            source,
            probes: PROBES,
            link_samples: FxHashMap::default(),
            probation: Vec::new(),
            checked: FxHashSet::default(),
            refused: FxHashSet::default(),
            cuts: FxHashSet::default(),
            cold_counts: FxHashMap::default(),
            cold_window: None,
            rebuilds: 0,
            pending: None,
            state: ResidentState::Profiling,
            has_proof: false,
            recorded_gap: false,
            row_targets: Vec::new(),
            row: RowLedger::default(),
            row_history: Vec::new(),
        }
    }
    pub(super) fn declined(&self) -> bool {
        self.decline().is_some()
    }

    pub(super) fn decline(&self) -> Option<&Decline> {
        match &self.state {
            ResidentState::Declined(reason) => Some(reason),
            _ => None,
        }
    }

    pub(super) fn retry(&self) -> bool {
        self.rebuilds == 0 && self.pending.is_none() && self.retry_period().is_some()
    }

    pub(super) fn sampling(&self) -> bool {
        self.rebuilds == 0 && !matches!(self.pending, Some(compile::Task::Replace(_)))
    }

    pub(super) fn sample_link(
        &mut self,
        point: usize,
        version: Option<usize>,
    ) -> (Option<usize>, bool) {
        let link = if let Some(version) = version {
            let count = self
                .link_samples
                .get(&point)
                .filter(|&&(last, _)| last == version)
                .map_or(1, |&(_, count)| count + 1);
            self.link_samples.insert(point, (version, count));
            (count == 2).then_some(version)
        } else {
            self.link_samples.remove(&point);
            None
        };
        self.probes -= 1;
        (link, self.probes == 0)
    }

    pub(super) fn row_version(&self, index: usize, mut measured: RowLedger) -> RowLedger {
        if let Some(history) = self.row_history.get(index) {
            measured.add(*history);
        }
        measured
    }

    pub(super) fn archive_row(&mut self, index: usize, measured: RowLedger) {
        if self.row_history.len() <= index {
            self.row_history.resize(index + 1, RowLedger::default());
        }
        self.row_history[index].add(measured);
    }

    pub(super) fn observe_row(
        &self,
        ledger: RowLedger,
        measuring: bool,
        probation: bool,
    ) -> RowAction {
        if ledger.total.over_budget(ROW_PROBE_WORDS) {
            if measuring {
                RowAction::Withdraw
            } else {
                RowAction::Sample(true)
            }
        } else if measuring && !probation && self.cold_window.is_none() {
            RowAction::Sample(false)
        } else {
            RowAction::Keep
        }
    }

    pub(super) fn record_cold(&mut self, site: u32, reason: ResidentCold) -> u64 {
        let count = self.cold_counts.entry((site, reason)).or_default();
        *count += 1;
        *count
    }

    pub(super) fn cold_action(
        &mut self,
        version: usize,
        site: u32,
        reason: ResidentCold,
        checked: bool,
        count: u64,
        retired: u64,
    ) -> ColdAction {
        if let Some(probation) = &mut self.probation[version] {
            probation.cold_entries += 1;
            probation.cold_retired += retired;
            if probation.cold_entries == u64::from(PROBATION) {
                return ColdAction::FinishProbation;
            }
        }
        if reason == ResidentCold::HostFault && !checked {
            if count >= CHECKED_FAULTS && !self.checked.contains(&site) {
                ColdAction::CheckStream
            } else {
                ColdAction::Ignore
            }
        } else {
            ColdAction::Observe
        }
    }

    pub(super) fn observe_cold(
        &mut self,
        site: u32,
        reason: ResidentCold,
        (entries, iterations): (u64, u64),
        cold_iterations: u64,
    ) -> bool {
        let window = self.cold_window.get_or_insert_with(|| ColdWindow {
            start_entries: entries,
            start_iterations: iterations,
            ..ColdWindow::default()
        });
        window.cold_iterations += cold_iterations;
        window.cold_entries += 1;
        let visits = (entries - window.start_entries + window.cold_entries)
            .max(iterations - window.start_iterations + window.cold_iterations);
        if visits > u64::from(PROBATION) {
            *window = ColdWindow {
                start_entries: entries,
                start_iterations: iterations,
                ..ColdWindow::default()
            };
        }
        let exits = window.exits.entry((site, reason)).or_default();
        *exits += 1;
        crate::policy::resident::cut_pays(*exits, visits)
    }

    pub(super) fn refuse(&mut self, site: u32, affected: impl Iterator<Item = usize>) -> bool {
        if !self.refused.insert(site) {
            return false;
        }
        for index in affected {
            self.rebuilds |= 1 << index;
        }
        true
    }

    pub(super) fn needs_rebuild(&self) -> bool {
        !self.declined() && self.rebuilds != 0
    }

    pub(super) fn next_rebuild(&mut self) -> Option<usize> {
        if self.pending.is_some() {
            return None;
        }
        let index = self.rebuilds.trailing_zeros() as usize;
        self.rebuilds &= !(1 << index);
        Some(index)
    }

    pub(super) fn accepts(&self, task: compile::Task) -> bool {
        !self.declined() && (task != compile::Task::Append || self.rebuilds == 0)
    }

    pub(super) fn installed(&mut self, index: usize, probation: Option<Probation>) -> Option<u32> {
        if index == self.probation.len() {
            self.probation.push(probation);
        } else {
            self.probation[index] = probation;
        }
        self.state = ResidentState::Installed;
        self.probes = PROBES;
        self.retry_period()
    }

    pub(super) fn retry_period(&self) -> Option<u32> {
        (1..VERSIONS)
            .contains(&self.probation.len())
            .then_some(MISSES)
    }

    pub(super) fn decline_with(&mut self, reason: Decline) -> bool {
        let gap = self.has_proof && !self.recorded_gap;
        self.recorded_gap |= gap;
        self.state = ResidentState::Declined(reason);
        self.probation.clear();
        gap
    }

    pub(super) fn check_stream(&mut self, stream: impl Iterator<Item = u32>, site: u32) {
        self.checked.extend(stream.filter(|&pc| {
            self.cold_counts
                .contains_key(&(pc, ResidentCold::HostFault))
        }));
        self.checked.insert(site);
    }
}

enum ResidentState {
    Profiling,
    Installed,
    Declined(Decline),
}

#[derive(Debug)]
pub(super) enum Decline {
    Compile(compile::Reject),
    Unprofitable,
    Cold(ResidentCold),
    CacheFull,
}

impl std::fmt::Display for Decline {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Decline::Compile(reason) => write!(out, "{reason:?}"),
            Decline::Unprofitable => out.write_str("Unprofitable"),
            Decline::Cold(reason) => write!(out, "{reason:?}"),
            Decline::CacheFull => out.write_str("CacheFull"),
        }
    }
}
