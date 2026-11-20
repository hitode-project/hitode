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
//! The policy constants below govern discovery, probation, faults, version
//! limits and re-offers. A settled decline adds no work to the baseline loop.
//! A cut rejoins Tier-1 and loses resident work on each affected iteration.
//! Budget about 32 host instructions for that transfer and lost work, versus
//! about 256 for a cold exit's state dump, Rust recovery and re-entry. The
//! break-even rate is 32/256. Each cold window uses admitted entries or the
//! retirement-based iteration estimate, whichever is larger. It ends after
//! 256 entries or probation end. A cold exit beyond 256 estimated iterations
//! starts a fresh sample, as does a change to the version chain. Windows reuse
//! the probation calls, which are patched out on settled forms between windows.

use adv_cpu_core::backend::OfferGate;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::Cell;

use super::compile::{self, ResidentCold};
use super::store::{self, CHAIN_LEN, Tier1Chain};
use crate::facts::Key;

pub(super) const HOT: u32 = 64;
pub(super) const VERSIONS: usize = 3;
pub(super) const SAMPLE: u32 = 8;
pub(super) const PROBES: u8 = 8;
pub(super) const MISSES: u32 = 4;
pub(super) const CHECKED_FAULTS: u64 = 2;
const COLD_MIN_EXITS: u64 = 2;
const COLD_ITERATIONS_PER_EXIT: u64 = 8;
pub(crate) const CHECK_AFTER: u32 = 8;
pub(crate) const PROMOTION: u8 = 4;
pub(super) const RESPECIALIZE: u8 = 2;
pub(super) const REOFFER: u32 = 64;
pub(super) const PROBATION: u32 = 256;

const NO_LIMIT: u32 = i32::MAX as u32;
const SHORT_JOB: std::time::Duration = std::time::Duration::from_micros(50);

pub(super) struct JobPlan {
    /// The serial cost of a split loop in host ticks.
    pub split_ticks: Cell<u64>,
    pub splits: bool,
    pub scoped: bool,
    pub split_minimum: Cell<u64>,
    misses: Cell<u32>,
    /// Optimized forms requested, and whether one is being built.
    forms: Cell<u8>,
    building: Cell<bool>,
    pub reoffer: Cell<u32>,
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
            split_ticks: Cell::new(0),
            misses: Cell::new(0),
            forms: Cell::new(0),
            building: Cell::new(false),
            reoffer: Cell::new(1),
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
            .map_or(450_000, |(_, ns)| (f64::from(count) * ns) as u64)
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
        gate.stale.load(Relaxed) != 0
            || ticks != 0
                && (gate.split_over.load(Relaxed) != 0
                    || now().wrapping_add(ticks) > gate.split_deadline.load(Relaxed))
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
        let skip = if completion.elapsed < SHORT_JOB {
            (u64::from(slice) / completion.retired.max(1)).max(1) as u32
        } else {
            REOFFER
        };
        self.reoffer.set(skip.max(REOFFER));
    }
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
        if let Some(job) = &site.job
            && !job.scoped
        {
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

    pub(super) fn job_mut(&mut self, eip: u32) -> Option<&mut JobPlan> {
        self.sites.get_mut(&eip)?.job.as_deref_mut()
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

    pub(super) fn retry(&self, version: usize) -> bool {
        self.rebuilds == 0
            && self.pending.is_none()
            && self.probation.len() == version + 1
            && self.probation.len() < VERSIONS
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
        *exits >= COLD_MIN_EXITS && *exits * COLD_ITERATIONS_PER_EXIT > visits
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
