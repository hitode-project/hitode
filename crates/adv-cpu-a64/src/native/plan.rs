//! The arbiter owns per-site plans and publication of code links (A64-OPT-12).
//!
//! Translators and the compile pool produce artifacts. This module selects
//! their consumers and publishes the result through the code store. The owner
//! run loop follows these choices. A new mechanism supplies an artifact and
//! plan events without introducing another trial in the owner run loop.
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

use super::*;

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

#[derive(Default)]
pub(super) struct AdmissionCache {
    pub generation: Option<(u64, u64)>,
    pub regs: [u32; 8],
    pub words: Vec<(u32, Option<u32>)>,
    pub evaluation: translate::computation::Evaluation,
    pub admitted: bool,
    pub split: Option<Option<translate::computation::Plan>>,
    #[cfg(test)]
    pub evaluations: u32,
}

pub(super) struct JobPlan {
    /// The serial cost of a split loop in host ticks.
    pub split_ticks: Cell<u64>,
    pub splits: bool,
    pub gate_entry: usize,
    pub gate_fallback: usize,
    pub admission: AdmissionCache,
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
            split_ticks: Cell::new(0),
            gate_entry: 0,
            gate_fallback: 0,
            admission: AdmissionCache::default(),
            misses: Cell::new(0),
            forms: Cell::new(0),
            building: Cell::new(false),
            reoffer: Cell::new(0),
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

    pub fn gate(&self) -> head::OfferEntry {
        head::OfferEntry {
            reoffer: self.reoffer.as_ptr() as u64,
            split_ticks: self.splits.then_some(self.split_ticks.as_ptr() as u64),
        }
    }

    pub fn offer_due(&self, gate: &OfferGate) -> bool {
        use std::sync::atomic::Ordering::Relaxed;

        let ticks = self.split_ticks.get();
        let due = gate.stale.load(Relaxed) != 0
            || ticks != 0
                && (gate.split_over.load(Relaxed) != 0
                    || super::ticks().wrapping_add(ticks) > gate.split_deadline.load(Relaxed));
        due && self.worth_offering()
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

    pub fn worth_offering(&self) -> bool {
        let skip = self.reoffer.get();
        if skip == 0 {
            true
        } else {
            self.reoffer.set(skip - 1);
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
                self.reoffer.set(0);
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
            (u64::from(runtime::COMPUTATION_SLICE) / completion.retired.max(1)).max(1) as u32
        } else {
            0
        };
        self.reoffer.set(skip);
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
struct SitePlan {
    seen: bool,
    samples: u8,
    promoted: bool,
    strikes: u32,
    checked: bool,
    force_tier1: bool,
    counter: Option<Box<Cell<u32>>>,
    resident: Option<Box<ResidentPlan>>,
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

    fn site(&mut self, eip: u32) -> &mut SitePlan {
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

    fn invalidate(&mut self, eip: u32) {
        if let Some(site) = self.sites.get_mut(&eip) {
            if let Some(count) = &site.counter {
                count.set(if site.force_tier1 { u32::MAX } else { HOT });
            }
            site.resident = None;
            site.job = None;
        }
    }

    fn clear_overlays(&mut self) {
        for site in self.sites.values_mut() {
            site.counter = None;
            site.resident = None;
            site.job = None;
        }
    }

    fn reset_samples(&mut self) {
        for site in self.sites.values_mut() {
            site.samples = 0;
        }
    }
}

pub(super) struct ResidentPlan {
    /// The Tier-1 address at discovery identifies this trace's lifetime.
    pub code: usize,
    /// The Tier-1 unit containing the back edge that reached the threshold.
    pub source: u32,
    pub unit: Arc<translate::Unit>,
    pub trampoline: usize,
    pub tier1: usize,
    pub sample: Box<Cell<u32>>,
    pub probes: u8,
    link_samples: FxHashMap<usize, (usize, u8)>,
    pub versions: Vec<head::Version>,
    pub checked: FxHashSet<u32>,
    pub refused: FxHashSet<u32>,
    pub cuts: FxHashSet<u32>,
    pub cold_counts: FxHashMap<(u32, ResidentCold), u64>,
    cold_window: Option<ColdWindow>,
    /// Versions to rebuild once the current request settles.
    pub rebuilds: u8,
    pub pending: Option<compile::Task>,
    state: ResidentState,
    has_proof: bool,
    recorded_gap: bool,
}

#[derive(Default)]
struct ColdWindow {
    start_entries: u64,
    start_iterations: u64,
    cold_iterations: u64,
    exits: FxHashMap<(u32, ResidentCold), u64>,
}

impl ResidentPlan {
    pub fn new(code: usize, source: u32, unit: Arc<translate::Unit>) -> Self {
        Self {
            code,
            source,
            unit,
            trampoline: 0,
            tier1: 0,
            sample: Box::new(SAMPLE.into()),
            probes: PROBES,
            link_samples: FxHashMap::default(),
            versions: Vec::new(),
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

    fn decline(&self) -> Option<&Decline> {
        match &self.state {
            ResidentState::Declined(reason) => Some(reason),
            _ => None,
        }
    }

    fn accounted(&self) -> (u64, u64) {
        self.versions
            .iter()
            .fold((0, 0), |(entries, iterations), version| {
                (
                    entries + version.slot.entries.get(),
                    iterations + version.iterations(version.slot.countdown[1].get()),
                )
            })
    }

    fn check_stream(&mut self, site: u32) {
        let trace = &self.unit.trace;
        let sites = trace.insts.iter().zip(&trace.x86);
        if let Some(key) = sites
            .clone()
            .find(|(inst, _)| inst.pc == site)
            .and_then(|(_, summary)| summary.as_ref()?.stream)
        {
            self.checked.extend(sites.filter_map(|(inst, summary)| {
                (self
                    .cold_counts
                    .contains_key(&(inst.pc, ResidentCold::HostFault))
                    && summary.as_ref()?.stream == Some(key))
                .then_some(inst.pc)
            }));
        }
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
    Compile(translate::opt::front::Reject),
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

impl Native {
    pub(super) fn resident_cold_exit(
        &mut self,
        eip: u32,
        site: u32,
        reason: ResidentCold,
        checked: bool,
        refund: u32,
    ) {
        self.diagnostics.resident_cold_exits += 1;
        *self
            .diagnostics
            .resident_cold_exits_by_reason
            .entry(reason)
            .or_default() += 1;
        let Some(head) = self.plans.resident_mut(eip) else {
            return;
        };
        let count = head.cold_counts.entry((site, reason)).or_default();
        *count += 1;
        let count = *count;
        if count.is_power_of_two() {
            crate::jit_map::optimized(
                eip,
                format_args!("resident\tcold\t{site}\t{reason:?}\t{count}"),
            );
        }
        if head.declined() {
            return;
        }
        if reason == ResidentCold::HostFault && !checked {
            if count >= CHECKED_FAULTS && !head.checked.contains(&site) {
                head.check_stream(site);
                crate::jit_map::optimized(
                    eip,
                    format_args!("resident\tchecked\t{site}\t{reason:?}"),
                );
                self.rebuild_resident_versions(eip);
            }
            return;
        }
        if head.cold_window.is_none() {
            for version in &head.versions {
                if version.probation.is_none() {
                    version.slot.entries.set(0);
                    version.slot.countdown[1].set(0);
                    version.account(&mut self.code.cache, true);
                }
            }
            self.code.cache.sync();
            let (start_entries, start_iterations) = head.accounted();
            head.cold_window = Some(ColdWindow {
                start_entries,
                start_iterations,
                ..ColdWindow::default()
            });
        }
        let (entries, iterations) = head.accounted();
        let version = &head.versions[self.runtime.resident_version as usize];
        let window = head.cold_window.as_mut().unwrap();
        if entries > window.start_entries {
            let retired = self
                .runtime
                .completed
                .wrapping_sub(self.runtime.countdown)
                .wrapping_sub(version.slot.countdown[0].get())
                .wrapping_sub(u64::from(refund));
            window.cold_iterations += version.iterations(retired);
        }
        let visits = 1
            + (entries - window.start_entries)
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
        if *exits >= COLD_MIN_EXITS && *exits * COLD_ITERATIONS_PER_EXIT > visits {
            self.stop_cold_window(eip);
            if reason == ResidentCold::FrameDisjoint {
                self.refuse_resident_fact(eip, u32::MAX);
                *self
                    .diagnostics
                    .resident_refusals_by_reason
                    .entry(reason)
                    .or_default() += 1;
                crate::jit_map::optimized(
                    eip,
                    format_args!("resident\trefused\t{site}\t{reason:?}"),
                );
            } else {
                self.cut_resident_site(eip, site, reason);
            }
        }
    }

    fn stop_cold_window(&mut self, eip: u32) {
        let head = self.plans.resident_mut(eip).unwrap();
        if head.cold_window.take().is_some() {
            for version in &head.versions {
                if version.probation.is_none() {
                    version.account(&mut self.code.cache, false);
                }
            }
            self.code.cache.sync();
        }
    }

    /// Keeps the loop at `head` in ordinary units, for tests of their
    /// mechanisms.
    #[cfg(test)]
    pub(crate) fn keep_ordinary(&mut self, head: u32) {
        self.plans.site(head).force_tier1 = true;
        if let Some(count) = self.plans.counter_at(head) {
            count.set(u32::MAX);
        }
    }

    /// Counts a fault at an unchecked site of `eip`, at host address `pc`.
    /// From `CHECK_AFTER` on, `eip` is checked and a new unit supersedes
    /// the one holding `pc`.
    pub(super) fn strike(&mut self, memory: &mut Memory, eip: u32, pc: usize) {
        let site = self.plans.site(eip);
        site.strikes += 1;
        if site.strikes < CHECK_AFTER {
            return;
        }
        self.plans.mark_checked(eip);
        let index = self.code.placed.partition_point(|&(start, _)| start <= pc);
        let Some(&(start, entry)) = index.checked_sub(1).map(|index| &self.code.placed[index])
        else {
            return;
        };
        // A unit dropped before stays in `placed`.
        if self
            .code
            .units
            .get(&entry)
            .is_some_and(|unit| unit.unit == start)
        {
            self.replace_unit(memory, entry);
        }
    }

    /// Points the runtime at the page classes of the current permissions.
    /// The unit at `eip`, translated if needed; a nonzero `point` is
    /// patched into a branch to it. The caller keeps stops unlinked: the
    /// owner must see them.
    pub(super) fn link(
        &mut self,
        memory: &mut Memory,
        point: usize,
        eip: u32,
        entry: Option<Tier1Unit>,
    ) -> Result<usize, String> {
        let generation = self.code.generation;
        let entry = match entry {
            Some(entry) => entry,
            None => self.translate(memory, eip)?,
        };
        // Translating may have cleared the cache and the link point with it.
        if point != 0 && self.code.generation == generation {
            self.patch(point, eip, entry.unit);
            if self.code.back_points.contains(&point) {
                // Reload the back edge's identity after the dispatch stub.
                return Ok(point - 4);
            }
            if self.plans.job(eip).is_some_and(|job| job.gate_entry != 0)
                || self.code.direct_points.contains_key(&point)
                    && self
                        .plans
                        .resident(eip)
                        .is_some_and(|head| head.trampoline != 0 && !head.declined())
            {
                return Ok(point);
            }
        }
        Ok(entry.unit)
    }

    /// Makes the link point at `point` branch to the unit at `eip`.
    pub(super) fn patch(&mut self, point: usize, eip: u32, unit: usize) {
        let link = self.code.link(point, eip);
        if link.back {
            let fallback = point + runtime::BACK_UNIT;
            self.code.repoint(fallback, unit);
        }
        Self::route_link(
            &mut self.code.cache,
            &link,
            self.plans.resident(eip),
            self.plans.job(eip),
            unit,
        );
    }

    pub(super) fn enters_resident(link: &Linked, head: &plan::ResidentPlan) -> bool {
        head.trampoline != 0 && !head.declined() && (link.back || link.direct.is_some())
    }

    pub(super) fn route_link(
        cache: &mut CodeCache,
        link: &Linked,
        head: Option<&plan::ResidentPlan>,
        job: Option<&JobPlan>,
        unit: usize,
    ) {
        let point = link.point;
        let target = if let Some(head) = head.filter(|head| Self::enters_resident(link, head)) {
            if link.back {
                head.trampoline
            } else if let Some(transition) = link.direct {
                cache.patch(
                    transition + 4,
                    head::branch(transition + 4, head.trampoline),
                );
                transition
            } else {
                unit
            }
        } else if let Some(job) =
            job.filter(|job| job.gate_entry != 0 && head.is_none_or(ResidentPlan::declined))
        {
            job.gate_entry
        } else if link.back && head.is_none() {
            point + runtime::BACK_COUNT
        } else {
            unit
        };
        cache.patch(point, link.branch(target));
    }

    /// Tier-1 replacement keeps old continuations and all their watches.
    pub(super) fn replace_unit(&mut self, memory: &mut Memory, eip: u32) {
        let Some(new) = self.emit(memory, eip) else {
            return;
        };
        let slot = &mut self.code.table[runtime::table_slot(eip)];
        if slot[0] == u64::from(eip) {
            slot[1] = new.unit as u64;
        }
        self.resolve_tables(eip, new.unit);
        let resident = self.plans.resident(eip);
        let mut next = self.code.links.get(&eip).copied().unwrap_or(u32::MAX);
        while let Some(link) = self.code.linked.get(next as usize).copied() {
            next = link.next;
            if link.back {
                let fallback = link.point + runtime::BACK_UNIT;
                self.code.repoint(fallback, new.unit);
            }
            if resident.is_some_and(|head| Self::enters_resident(&link, head)) {
                continue;
            }
            Self::route_link(
                &mut self.code.cache,
                &link,
                resident,
                self.plans.job(eip),
                new.unit,
            );
        }
        self.code.cache.sync();
    }

    pub(super) fn resolve_tables(&mut self, eip: u32, unit: usize) {
        if !self.code.rows.contains_key(&eip)
            || self.is_stop(eip)
            || !self.front_units.get(&eip).is_some_and(|site| {
                site.hot
                    .as_ref()
                    .or(site.cold.as_ref())
                    .is_some_and(|unit| unit.table_target)
            })
        {
            return;
        }
        for &(_, point, _) in &self.code.rows[&eip] {
            self.code.cache.patch(point, unit as u32);
            self.code
                .cache
                .patch(point + 4, ((unit as u64) >> 32) as u32);
        }
    }

    /// Drops units whose guest code changed, and restores the link points
    /// that branch to them.
    pub(super) fn invalidate(&mut self, memory: &mut Memory) {
        if memory.dirty_pages.is_empty() {
            return;
        }
        let mut dropped = false;
        for page in std::mem::take(&mut memory.dirty_pages).into_keys() {
            self.decodes.invalidate(page);
            for eip in self.code.pages.remove(&page).unwrap_or_default() {
                self.plans.site(eip).promoted = false;
                let unit = self.drop_unit(eip);
                self.diagnostics.dropped_units += u64::from(unit);
                dropped |= unit;
            }
        }
        if dropped {
            self.plans.reset_samples();
            self.forget_dynamic();
        }
    }

    /// Drops the unit at `eip`, if any, and restores the link points that
    /// branch to it; the caller forgets dynamic targets.
    pub(super) fn drop_unit(&mut self, eip: u32) -> bool {
        self.front_units.remove(&eip);
        let dropped = self.code.invalidate_unit(eip);
        self.repoint_internal_edges(eip, None);
        self.plans.invalidate(eip);
        self.offload.invalidate(eip);
        dropped
    }

    /// Empties the indirect table and the shadow stack, which may lead to
    /// dropped units.
    pub(super) fn forget_dynamic(&mut self) {
        self.diagnostics.global_invalidations += 1;
        self.code.table.fill(EMPTY);
        self.runtime.shadow = self.code.shadow.top();
    }

    /// Units must not run past any stop; a new one cuts those that could.
    pub(super) fn observe_stop(&mut self, stop: u32) {
        if self.stops.contains(&stop) {
            return;
        }
        self.stops.push(stop);
        if let Some(word) = &self.interrupt {
            word.fetch_or(
                adv_sync::pending::STOP,
                std::sync::atomic::Ordering::Release,
            );
        }
        self.cut(stop, stop.saturating_add(1));
    }

    /// Drops the units decoded from the pages of `start..end`, which hold
    /// every unit whose trace runs through a new boundary there, and the
    /// unit starting at it, whose dropping restores the links to it.
    pub(super) fn cut(&mut self, start: u32, end: u32) {
        let end = u64::from(end).max(u64::from(start) + 1);
        let mut dropped = false;
        for page in (u64::from(start & !0xfff)..end).step_by(4096) {
            for eip in self.code.pages.remove(&(page as u32)).unwrap_or_default() {
                dropped |= self.drop_unit(eip);
            }
        }
        if dropped {
            self.forget_dynamic();
        }
    }

    /// Clears the cache as a whole (A64-CTL-1).
    pub(crate) fn clear(&mut self) {
        self.diagnostics.clears += 1;
        self.code.cache.truncate(self.code.stub_len);
        self.code.units.clear();
        self.offload.clear();
        self.code.rows.clear();
        self.code.table_sources.clear();
        self.code.uses_permissions = false;
        self.code.links.clear();
        self.code.linked.clear();
        self.code.back_points.clear();
        self.code.direct_points.clear();
        self.code.internal_edges.clear();
        self.plans.clear_overlays();
        self.runtime.resident_point = 0;
        self.runtime.resident_env = 0;
        self.runtime.resident_probation = 0;
        self.code.tier1_words.clear();
        self.code.placed.clear();
        self.code.helper_eips = self.helper_histogram();
        self.code.helpers.clear();
        self.code.helper_counts.clear();
        self.code.generation += 1;
        self.code.faults.sites.clear();
        self.forget_dynamic();
    }

    /// Helper runs by EIP.
    pub(crate) fn helper_histogram(&self) -> FxHashMap<u32, u64> {
        let mut eips = self.code.helper_eips.clone();
        for (helper, &count) in self.code.helpers.iter().zip(&self.code.helper_counts) {
            *eips.entry(helper.pc).or_default() += count;
        }
        eips
    }

    /// Whether code at `eip` is known to run, so translating it pays.
    pub(super) fn ran(&self, eip: u32) -> bool {
        !self.first_visits || self.plans.sites.get(&eip).is_some_and(|plan| plan.seen)
    }

    /// Whether `eip`, which has no unit, runs for the first time; it will
    /// not be again.
    pub(super) fn first_visit(&mut self, eip: u32) -> bool {
        self.first_visits
            && !self.code.units.contains_key(&eip)
            && !std::mem::replace(&mut self.plans.site(eip).seen, true)
    }

    /// Translates the unit at `eip`, and ahead of time the units its main
    /// path leaves to that have run before, so they share one cache sync
    /// and link directly. Every link point of these units to a unit that
    /// exists is patched.
    pub(super) fn translate(&mut self, memory: &mut Memory, eip: u32) -> Result<Tier1Unit, String> {
        self.code.points.clear();
        let entry = match self.emit(memory, eip) {
            Some(entry) => entry,
            None => {
                self.clear();
                self.code.points.clear();
                self.emit(memory, eip)
                    .ok_or("a64 unit does not fit an empty cache")?
            }
        };
        // Only the first unit's main path is translated ahead: deeper
        // successors are less certain to run.
        let first = self.code.points.len();
        let mut index = 0;
        while let Some(&(point, target, main)) = self.code.points.get(index) {
            index += 1;
            // The owner must see stops.
            if self.is_stop(target) {
                continue;
            }
            let unit = match self.code.units.get(&target) {
                Some(entry) => entry.unit,
                // A full cache waits for the next miss to clear it.
                None if main
                    && index <= first
                    && self.ran(target)
                    && crate::translate::Fetcher::fetchable(memory, target) =>
                {
                    match self.emit(memory, target) {
                        Some(entry) => entry.unit,
                        None => break,
                    }
                }
                None => continue,
            };
            self.patch(point, target, unit);
        }
        Ok(entry)
    }

    pub(super) fn emit(&mut self, memory: &mut Memory, eip: u32) -> Option<Tier1Unit> {
        let loops =
            self.promotion == 0 || self.plans.sites.get(&eip).is_some_and(|plan| plan.promoted);
        let unit = self.front_unit(memory, eip, loops, false);
        let at = self.code.cache.next();
        let mut work = std::mem::take(&mut self.code.work);
        let all = self.checks != Checks::Adaptive;
        let (control, pe) = if self.runtime.cpu.is_null() {
            (self.control, self.pe)
        } else {
            // SAFETY: during a run the runtime points at its live CPU, which
            // nothing writes while the owner translates.
            let x87 = unsafe { &(*self.runtime.cpu).x87 };
            (x87.control, x87.status & 0x20 != 0)
        };
        translate::lower_unit(
            &mut work.code,
            &unit,
            eip,
            &|pc| all || self.plans.checked(pc),
            self.code.helpers.len() as u32,
            self.features,
            at,
            self.code.stubs,
            control,
            pe,
            !self.plans.baseline_only,
        );
        if work.retired() == 0 {
            self.diagnostics.empty_units[usize::from(unit.trace.failure & 3)] += 1;
        }
        let entry = self.code.install_unit(
            eip,
            work.words(),
            work.sites(),
            work.retired(),
            work.marks(),
        );
        if let Some(entry) = entry {
            debug_assert_eq!(entry.unit, at);
            if !unit.tables.is_empty() {
                self.code.table_sources.insert(eip);
            }
            for (table, &(offset, stub)) in unit.tables.iter().zip(&work.code.table_positions) {
                for (index, &target) in table.targets.iter().enumerate() {
                    let slot = (eip, at + offset + index * 8, at + stub);
                    self.code.rows.entry(target).or_default().push(slot);
                }
            }
            let mut targets: Vec<_> = unit
                .tables
                .iter()
                .flat_map(|t| t.targets.iter().copied())
                .collect();
            targets.sort_unstable();
            targets.dedup();
            for target in targets {
                if let Some(entry) = self.code.units.get(&target) {
                    self.resolve_tables(target, entry.unit);
                }
            }
            for link in work.links() {
                let point = at + link.offset;
                self.code.points.push((point, link.eip, link.main));
                if let Some(transition) = link.direct {
                    self.code.direct_points.insert(point, at + transition);
                }
                if link.back {
                    self.code.back_points.insert(point);
                    let (address, _) = self.counter(link.eip);
                    self.bind_counter(point + runtime::BACK_COUNTER, address);
                }
            }
            for link in work.internal_links() {
                let edge = InternalEdge {
                    point: at + link.point,
                    target: at + link.target,
                    transition: at + link.transition,
                    link: at + link.link,
                    source: eip,
                    source_pc: link.source_pc,
                    back: link.counter.is_some(),
                };
                let value = if let Some(counter) = link.counter {
                    let (address, value) = self.counter(link.eip);
                    self.bind_counter(at + counter, address);
                    value
                } else {
                    u32::MAX
                };
                let resident = self.plans.resident(link.eip);
                if let Some(head) = resident.filter(|head| {
                    head.trampoline != 0
                        && !head.declined()
                        && (edge.back
                            || !head
                                .unit
                                .trace
                                .insts
                                .iter()
                                .any(|inst| inst.pc == edge.source_pc))
                }) {
                    self.code
                        .cache
                        .patch(edge.link, head::branch(edge.link, head.trampoline));
                    self.code
                        .cache
                        .patch(edge.point, head::branch(edge.point, edge.transition));
                } else if resident.is_some() || value == u32::MAX {
                    self.code
                        .cache
                        .patch(edge.point, head::branch(edge.point, edge.target));
                }
                self.code
                    .internal_edges
                    .entry(link.eip)
                    .or_default()
                    .push(edge);
            }
            if self.plans.counter_at(eip).is_some() || !work.internal_links().is_empty() {
                self.code
                    .tier1_words
                    .insert(eip, store::tier1_words(&work, &unit.trace));
            }
            if let Some(head) = self.plans.resident(eip).filter(|head| head.tier1 != 0) {
                self.code
                    .cache
                    .patch(head.tier1, head::branch(head.tier1, entry.unit));
            }
            if let Some(gate) = self
                .plans
                .job(eip)
                .map(|job| job.gate_fallback)
                .filter(|&gate| gate != 0)
            {
                self.code.repoint(gate, entry.unit);
            }
            self.code.helpers.extend(unit.helpers().iter().cloned());
            self.code.helper_counts.resize(self.code.helpers.len(), 0);
        }
        self.code.uses_permissions |= entry.is_some() && work.code.uses_permissions;
        // Code discovery can protect a host page shared with ordinary data.
        // Refresh permission snapshots before linked code resumes.
        if self.code.uses_permissions {
            self.classify(memory);
        }
        self.code.work = work;
        entry
    }

    pub(super) fn counter(&mut self, eip: u32) -> (u64, u32) {
        let plan = self.plans.site(eip);
        let value = if plan.force_tier1 { u32::MAX } else { HOT };
        let count = plan.counter.get_or_insert_with(|| Box::new(value.into()));
        (std::ptr::from_ref(&**count) as u64, count.get())
    }

    pub(super) fn bind_counter(&mut self, at: usize, address: u64) {
        self.code.cache.patch(at, address as u32);
        self.code.cache.patch(at + 4, (address >> 32) as u32);
    }

    pub(super) fn stop_counting(&mut self, eip: u32) {
        self.plans.counter_at(eip).unwrap().set(u32::MAX);
        self.repoint_resident_edges(eip, None);
    }

    pub(super) fn discover(&mut self, memory: &mut Memory, eip: u32, source: u32, at: CpuState) {
        if self.plans.baseline_only {
            return;
        }
        let unit = self.front_unit(memory, eip, true, true);
        let code = self.code.units[&source].unit;
        self.plans.site(eip).resident = Some(Box::new(ResidentPlan::new(code, source, unit)));
        self.request_resident(eip, compile::Task::Discover, at);
    }

    pub(super) fn request_resident(&mut self, eip: u32, task: compile::Task, mut at: CpuState) {
        at.refresh_mmx();
        let checked = self
            .plans
            .resident(eip)
            .unwrap()
            .unit
            .trace
            .insts
            .iter()
            .filter_map(|inst| self.plans.checked(inst.pc).then_some(inst.pc))
            .collect::<FxHashSet<_>>();
        let head = self.plans.resident_mut(eip).unwrap();
        head.pending = Some(task);
        if let Some(last) = head.versions.last() {
            last.slot.limit.set(0);
        }
        let mut checked = checked;
        checked.extend(&head.checked);
        self.compiler.request(compile::Request {
            eip,
            generation: self.code.generation,
            code: head.code,
            task,
            unit: Arc::clone(&head.unit),
            proof: self.offload.proof(eip),
            at,
            checked,
            all_checked: self.checks != Checks::Adaptive,
            refused: head.refused.clone(),
            cuts: head.cuts.clone(),
            count_entries: self.count_resident_entries,
        });
    }

    pub(super) fn resident_arrival(&mut self, cpu: &CpuState) {
        let point = self.runtime.resident_point;
        if point == 0 {
            return;
        }
        let eip = cpu.eip;
        let Some(head) = self.plans.resident_mut(eip) else {
            return;
        };
        if head.rebuilds != 0 || matches!(head.pending, Some(compile::Task::Replace(_))) {
            return;
        }
        if point == 1 {
            if head.pending.is_none() && head.versions.len() < plan::VERSIONS {
                self.request_resident(eip, compile::Task::Append, cpu.clone());
            }
            return;
        }
        if head.versions.is_empty() {
            return;
        }
        self.diagnostics.rust_decisions_on_decided_sites += u64::from(head.probes == 0);
        let patch = if let Some(edge) = self
            .code
            .internal_edges
            .get_mut(&eip)
            .and_then(|edges| edges.iter_mut().find(|edge| edge.point == point))
        {
            edge.link
        } else {
            let mut next = self.code.links[&eip];
            while self.code.linked[next as usize].point != point {
                next = self.code.linked[next as usize].next;
            }
            let edge = &mut self.code.linked[next as usize];
            edge.direct.map_or(point, |transition| transition + 4)
        };
        let version = head.versions.iter().position(|version| {
            cpu.rep.is_none()
                && cpu.mmx_mode == 0
                && translate::opt::admit_into(
                    &version.sources,
                    &version.assumed,
                    cpu,
                    &mut self.resident_params,
                    |_, _| 0,
                )
        });
        let mut patched = false;
        if let Some(version) = version {
            let count = head
                .link_samples
                .get(&point)
                .filter(|&&(last, _)| last == version)
                .map_or(1, |&(_, count)| count + 1);
            head.link_samples.insert(point, (version, count));
            if count == 2 {
                self.code
                    .cache
                    .patch(patch, head::branch(patch, head.versions[version].entry));
                patched = true;
            }
        } else {
            head.link_samples.remove(&point);
        }
        head.probes -= 1;
        if head.probes == 0 {
            self.code.cache.patch(
                head.trampoline,
                head::branch(head.trampoline, head.versions[0].entry),
            );
            patched = true;
        }
        if patched {
            self.code.cache.sync();
        }
    }

    pub(super) fn environment_miss(&mut self) {
        let event = self.runtime.resident_env;
        if event == 0 {
            return;
        }
        let eip = (event >> 32) as u32;
        let index = ((event >> 8) & 0xff) as usize - 1;
        let Some(head) = self.plans.resident_mut(eip) else {
            return;
        };
        let Some(version) = head.versions.get(index) else {
            return;
        };
        let fact = event as u8 as usize;
        let Some(env) = version.slot.env.get(fact) else {
            return;
        };
        let Some(&site) = version.plain_sites.get(fact) else {
            return;
        };
        env.set(0);
        self.refuse_resident_fact(eip, site);
    }

    pub(super) fn refuse_resident_fact(&mut self, eip: u32, site: u32) {
        let head = self.plans.resident_mut(eip).unwrap();
        if head.refused.insert(site) {
            for (i, version) in head.versions.iter().enumerate() {
                if version.plain_sites.contains(&site) {
                    head.rebuilds |= 1 << i;
                }
            }
            self.rebuild_resident(eip);
        }
    }

    pub(super) fn cut_resident_site(&mut self, eip: u32, site: u32, reason: ResidentCold) {
        let head = self.plans.resident_mut(eip).unwrap();
        if head.cuts.insert(site) {
            if head
                .unit
                .trace
                .reaches_latch_without(&|pc| head.cuts.contains(&pc))
            {
                *self
                    .diagnostics
                    .resident_cuts_by_reason
                    .entry(reason)
                    .or_default() += 1;
                crate::jit_map::optimized(eip, format_args!("resident\tcut\t{site}\t{reason:?}"));
                self.rebuild_resident_versions(eip);
            } else {
                *self
                    .diagnostics
                    .resident_demotions_by_reason
                    .entry(reason)
                    .or_default() += 1;
                crate::jit_map::declined(eip, reason);
                self.decline_resident(eip, Decline::Cold(reason));
            }
        }
    }

    pub(super) fn rebuild_resident_versions(&mut self, eip: u32) {
        let head = self.plans.resident_mut(eip).unwrap();
        head.rebuilds = (1 << head.versions.len()) - 1;
        self.rebuild_resident(eip);
    }

    pub(super) fn rebuild_resident(&mut self, eip: u32) {
        let head = self.plans.resident_mut(eip).unwrap();
        if head.declined() || head.rebuilds == 0 {
            return;
        }
        let (trampoline, tier1) = (head.trampoline, head.tier1);
        self.stop_cold_window(eip);
        // A sampled link may bypass the trampoline. Send those links
        // through it too until all stale versions have been replaced.
        self.code.repoint(trampoline, tier1);
        self.repoint_resident_edges(eip, Some(trampoline));
        let head = self.plans.resident_mut(eip).unwrap();
        if head.pending.is_none() {
            let index = head.rebuilds.trailing_zeros() as usize;
            head.rebuilds &= !(1 << index);
            let at = head.versions[index].at.clone();
            self.request_resident(eip, compile::Task::Replace(index), at);
        }
    }

    pub(super) fn install_optimized(&mut self, wait: bool) {
        while let Some(done) = self.compiler.next(wait) {
            let eip = done.eip;
            if done.generation != self.code.generation {
                continue;
            }
            if done.task == compile::Task::Job {
                if !self.finish_job(eip, done.code, done.job) {
                    continue;
                }
                if let Some(head) = self.plans.resident_mut(eip) {
                    head.pending = None;
                    if head.versions.len() < plan::VERSIONS
                        && let Some(last) = head.versions.last()
                    {
                        last.slot
                            .limit
                            .set(last.slot.misses.get().wrapping_add(plan::MISSES));
                    }
                    self.rebuild_resident(eip);
                }
                continue;
            }
            let Some(head) = self
                .plans
                .resident_mut(eip)
                .filter(|head| head.code == done.code)
            else {
                continue;
            };
            head.pending = None;
            if let Some(job) = done.job {
                head.has_proof = true;
                let unit = Arc::clone(&head.unit);
                self.place_job(eip, &done.at, unit, job);
                self.install_offer_link(eip);
            }
            let head = self.plans.resident(eip).unwrap();
            if !head.declined()
                && (done.task != compile::Task::Append || head.rebuilds == 0)
                && let Some(result) = done.resident
            {
                let decline = match result {
                    Ok(form) => (!self.install_resident(eip, done.task, done.at, form))
                        .then_some(Decline::CacheFull),
                    Err(reject) => Some(Decline::Compile(reject)),
                };
                if matches!(
                    done.task,
                    compile::Task::Discover | compile::Task::Replace(_)
                ) && let Some(reason) = decline
                {
                    self.decline_resident(eip, reason);
                }
            }
            self.rebuild_resident(eip);
        }
    }

    pub(super) fn install_resident(
        &mut self,
        eip: u32,
        task: compile::Task,
        at: CpuState,
        form: compile::ResidentForm,
    ) -> bool {
        let compile::ResidentForm { built, mut lowered } = form;
        let source = self.plans.resident(eip).unwrap().source;
        let mut probation = None;
        if lowered.iteration_retired != 0
            && let Some(words) = self.code.tier1_words.get(&source)
            && let Some(tier1) = lowered
                .iteration_pcs
                .iter()
                .try_fold(0u32, |sum, pc| words.get(pc).map(|&words| sum + words))
        {
            if tier1 <= lowered.iteration_words {
                self.decline_resident(eip, Decline::Unprofitable);
                return true;
            }
            probation = Some(head::Probation {
                iteration_retired: u64::from(lowered.iteration_retired),
                saved_words: u64::from(tier1 - lowered.iteration_words),
                entry_exit_words: u64::from(lowered.entry_words + lowered.exit_words),
            });
        }
        let gate = self.plans.job(eip).map(JobPlan::gate);
        self.stop_cold_window(eip);
        let head = self.plans.resident_mut(eip).unwrap();
        let (trampoline, tier1) = if head.trampoline == 0 {
            let trampoline = self.code.cache.next();
            let tier1 = trampoline + 4;
            let mut words = vec![b(4)];
            if let Some(unit) = self.code.units.get(&eip) {
                words.push(head::branch(tier1, unit.unit));
            } else {
                words.extend(head::fallback(eip, tier1, self.code.stubs.exit));
            }
            if self.code.cache.append(&words).is_none() {
                return false;
            }
            (trampoline, tier1)
        } else {
            (head.trampoline, head.tier1)
        };
        let index = match task {
            compile::Task::Replace(i) => i,
            _ => head.versions.len(),
        };
        let slot = Box::<head::Slot>::default();
        let mut entry = head::emit(
            (eip, index),
            &head.sample,
            &slot,
            gate,
            &mut lowered,
            self.code.cache.next(),
            head::Targets {
                tier1,
                exit: self.code.stubs.exit,
                dispatch: self.code.stubs.dispatch,
                opt_exit: self.code.stubs.opt_exit,
            },
        );
        let accounting = lowered
            .probation
            .iter()
            .map(|word| {
                let point = entry.form + word * 4;
                let placed = (entry.form - self.code.cache.next()) / 4 + word;
                let call = entry.words[placed];
                if probation.is_none() {
                    entry.words[placed] = crate::encode::NOP;
                }
                (point, call)
            })
            .collect();
        if self.code.cache.append(&entry.words).is_none() {
            return false;
        }
        for &(point, transition) in &lowered.transitions {
            self.code
                .direct_points
                .insert(entry.form + point, entry.form + transition);
        }
        let from = self.code.faults.sites.len().saturating_sub(1);
        self.code.faults.sites.extend(
            lowered
                .sites
                .iter()
                .map(|&(offset, site)| (entry.form + offset, site)),
        );
        if !self.code.faults.sites[from..].is_sorted_by_key(|&(at, _)| at) {
            self.code.faults.sites.sort_unstable_by_key(|&(at, _)| at);
        }
        crate::jit_map::optimized(eip, format_args!("installed\t-\t-"));
        crate::jit_map::record(
            entry.form,
            &lowered.words,
            eip,
            head.unit.trace.prepaid(0),
            &lowered.marks,
            "a64-resident",
        );
        if self.inline_saved.len() < lowered.spill_cells {
            self.inline_saved.resize(lowered.spill_cells, 0);
        }
        let version = head::Version {
            entry: entry.entry,
            probe: entry.probe,
            next: entry.next,
            at,
            assumed: built.assumed,
            sources: built.func.entry,
            plain_sites: built.plain_sites,
            records: lowered.records,
            flags: lowered.flags,
            slot,
            probation,
            accounting,
            iteration_retired: u64::from(lowered.iteration_retired),
        };
        if index == head.versions.len() {
            head.versions.push(version);
        } else {
            head.versions[index] = version;
        }
        head.state = ResidentState::Installed;
        head.trampoline = trampoline;
        head.tier1 = tier1;
        head.probes = plan::PROBES;
        head.sample.set(plan::SAMPLE);
        head.versions
            .iter()
            .for_each(|version| version.slot.limit.set(0));
        if head.versions.len() < plan::VERSIONS {
            let last = &head.versions.last().unwrap().slot;
            last.limit.set(last.misses.get().wrapping_add(plan::MISSES));
        }
        if head.rebuilds == 0 {
            self.code
                .cache
                .patch(trampoline, head::branch(trampoline, head.versions[0].probe));
        }
        for pair in head.versions.windows(2) {
            self.code
                .cache
                .patch(pair[0].next, head::branch(pair[0].next, pair[1].entry));
        }
        self.code.uses_permissions = true;
        self.code.faults.end.set(self.code.cache.next());
        self.repoint_resident_edges(eip, Some(trampoline));
        true
    }

    fn install_offer_link(&mut self, eip: u32) {
        let Some(job) = self.plans.job_mut(eip) else {
            return;
        };
        let at = self.code.cache.next();
        let unit = self.code.units.get(&eip).map(|entry| entry.unit);
        let (words, fallback) = head::offer_link(eip, at, self.code.stubs.exit, unit, job.gate());
        if self.code.cache.append(&words).is_some() {
            job.gate_entry = at;
            job.gate_fallback = fallback;
        }
    }

    pub(super) fn repoint_resident_edges(&mut self, eip: u32, resident: Option<usize>) {
        if let Some(head) = self.plans.resident_mut(eip) {
            head.link_samples.clear();
        }
        self.repoint_internal_edges(eip, resident);
        let mut link = self.code.links.get(&eip).copied().unwrap_or(u32::MAX);
        let head = self.plans.resident(eip);
        let job = self.plans.job(eip);
        let unit = self
            .code
            .units
            .get(&eip)
            .map(|entry| entry.unit)
            .or_else(|| head.map(|head| head.tier1).filter(|&tier1| tier1 != 0))
            .or_else(|| {
                job.map(|job| job.gate_fallback)
                    .filter(|&fallback| fallback != 0)
            });
        while let Some(edge) = self.code.linked.get_mut(link as usize) {
            link = edge.next;
            if edge.back || edge.direct.is_some() || job.is_some() {
                Self::route_link(&mut self.code.cache, edge, head, job, unit.unwrap());
            }
        }
        self.code.cache.sync();
    }

    pub(super) fn repoint_internal_edges(&mut self, eip: u32, resident: Option<usize>) {
        let head = self.plans.resident(eip);
        for edge in self.code.internal_edges.get_mut(&eip).into_iter().flatten() {
            let target = if let Some(resident) = resident.filter(|_| {
                edge.back
                    || !head
                        .unwrap()
                        .unit
                        .trace
                        .insts
                        .iter()
                        .any(|inst| inst.pc == edge.source_pc)
            }) {
                self.code
                    .cache
                    .patch(edge.link, head::branch(edge.link, resident));
                edge.transition
            } else {
                edge.target
            };
            self.code
                .cache
                .patch(edge.point, head::branch(edge.point, target));
        }
    }

    pub(super) fn finish_probation(&mut self) {
        let event = self.runtime.resident_probation;
        if event == 0 {
            return;
        }
        let eip = (event >> 32) as u32;
        let index = event as u32 as usize - 1;
        let Some(head) = self.plans.resident_mut(eip) else {
            return;
        };
        if index >= head.versions.len() {
            return;
        }
        self.stop_cold_window(eip);
        let version = &mut self.plans.resident_mut(eip).unwrap().versions[index];
        let Some(probation) = version.probation.take() else {
            return;
        };
        if probation.profitable(&version.slot) {
            version.account(&mut self.code.cache, false);
            self.code.cache.sync();
        } else {
            crate::jit_map::demoted(
                eip,
                index,
                version.slot.entries.get(),
                version.slot.countdown[1].get() / probation.iteration_retired,
            );
            self.decline_resident(eip, Decline::Unprofitable);
        }
    }

    pub(super) fn decline_resident(&mut self, eip: u32, reason: Decline) {
        self.stop_cold_window(eip);
        let head = self.plans.resident_mut(eip).unwrap();
        if head.has_proof && !head.recorded_gap {
            crate::jit_map::optimized(eip, format_args!("coverage-gap\t{reason}"));
            self.diagnostics.proof_without_resident += 1;
            head.recorded_gap = true;
        }
        head.state = ResidentState::Declined(reason);
        head.versions.clear();
        self.repoint_resident_edges(eip, None);
    }

    /// Budget samples select Tier-1 loop traces.
    pub(super) fn sample(&mut self, memory: &mut Memory, eip: u32) {
        if self.promotion == 0 || self.is_stop(eip) {
            return;
        }
        let plan = self.plans.site(eip);
        if plan.promoted {
            return;
        }
        plan.samples += 1;
        if plan.samples >= self.promotion {
            plan.samples = 0;
            plan.promoted = true;
            self.diagnostics.promotions += 1;
            crate::jit_map::optimized(eip, format_args!("promoted\t-\t-"));
            self.replace_unit(memory, eip);
        }
    }

    #[cfg(test)]
    pub(super) fn prepare_job(&mut self, memory: &mut Memory, stop: u32, at: &CpuState) {
        self.observe_stop(stop);
        self.control = at.x87.control;
        self.pe = at.x87.status & 0x20 != 0;
        if !self.code.units.contains_key(&at.eip) {
            self.translate(memory, at.eip).unwrap();
        }
        self.discover(memory, at.eip, at.eip, at.clone());
        self.install_optimized(true);
        if !self.plans.resident(at.eip).unwrap().declined() {
            self.decline_resident(at.eip, Decline::Unprofitable);
        }
    }

    fn front_unit(
        &mut self,
        memory: &mut Memory,
        eip: u32,
        loops: bool,
        summarize: bool,
    ) -> Arc<translate::Unit> {
        let site = self.front_units.entry(eip).or_default();
        let shape = if loops { &mut site.hot } else { &mut site.cold };
        let unit = shape.get_or_insert_with(|| {
            let handler = if self.bridge.is_null() {
                None
            } else {
                // SAFETY: preparation stays within this owner's bridge call.
                unsafe { &*self.bridge.cast::<Bridge<'_>>() }
                    .handler
                    .as_ref()
            };
            let mut unit = translate::Unit::default();
            unit.images.clone_from(&self.images.borrow());
            unit.build_cached(
                memory,
                eip,
                &self.stops,
                &|pc, word| handler.and_then(|handler| handler.host_thunk(pc, word)),
                &|pc| {
                    let handler = handler?;
                    let (target, original) = handler.redirect(pc)?;
                    Some(translate::Redirect {
                        target,
                        original: original.to_vec(),
                        checks: handler.redirect_checks(pc).to_vec(),
                        body: handler.direct_body(pc),
                    })
                },
                loops,
                &mut self.decodes,
            );
            unit.prepare_regions(memory, &|pc, code| {
                handler.map_or_else(Vec::new, |handler| handler.native_region(pc, code))
            });
            self.code.watch_pages(eip, unit.code());
            Arc::new(unit)
        });
        if summarize && unit.trace.x86.is_empty() {
            Arc::make_mut(unit).summarize(&mut self.decodes);
        }
        Arc::clone(unit)
    }
}

/// Called by the `dispatch` stub with the guest EIP and the link point to
/// patch, or 0. Returns the unit to continue at, or 0 to leave.
pub(super) unsafe extern "C" fn dispatch(runtime: *mut Runtime, eip: u32, point: usize) -> usize {
    // SAFETY: `run` points `owner` at a live `Owner` whose pointees it does
    // not touch while translated code runs.
    let owner = unsafe { &mut *((*runtime).owner as *mut Owner) };
    let (native, memory) = unsafe { (&mut *owner.native, &mut *owner.memory) };
    // An indirect transfer missed the table: record it as the interpreter
    // would, so a jump to nowhere names its source.
    let branch = std::mem::take(unsafe { &mut (*runtime).branch });
    if branch != 0 {
        // SAFETY: the runtime's CPU is live while units run.
        let cpu = unsafe { &mut *(*runtime).cpu };
        record_branch(cpu, branch, eip);
    }
    if owner.error.is_some() {
        return 0;
    }
    if native.is_stop(eip) {
        return 0;
    }
    let back = native.code.back_points.contains(&point);
    if back && native.plans.counter_at(eip).unwrap().get() == 0 {
        native.stop_counting(eip);
        owner.discovered = Some(eip);
        return 0;
    }
    // A first visit runs in the owner; the point links on the next one.
    let entry = native.code.units.get(&eip).copied();
    if entry.is_none()
        && native.code.direct_points.contains_key(&point)
        && let Some(head) = native.plans.resident(eip)
        && head.trampoline != 0
        && !head.declined()
    {
        native.patch(point, eip, head.tier1);
        native.code.cache.sync();
        return point;
    }
    if entry.is_none() && (!crate::translate::Fetcher::fetchable(memory, eip) || !native.ran(eip)) {
        return 0;
    }
    let before = native.code.cache.used();
    let unit = match native.link(memory, point, eip, entry) {
        Ok(unit) => unit,
        Err(error) => {
            owner.error = Some(error);
            return 0;
        }
    };
    native.resolve_tables(eip, native.code.units[&eip].unit);
    if point == 0 {
        native.code.table[runtime::table_slot(eip)] = [u64::from(eip), unit as u64];
    }
    owner.translated += native.code.cache.used().saturating_sub(before);
    native.code.cache.sync();
    native.code.faults.end.set(native.code.cache.next());
    unit
}
