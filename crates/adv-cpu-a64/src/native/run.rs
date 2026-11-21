//! Owner entry and recovery from translated code.
//!
//! Each Rust arrival reads one site plan. Decided links stay in translated
//! code, with explicit cold and offload events returning to the arbiter.

use adv_cpu_core::backend::OfferGate;
use adv_cpu_core::trap::Bridge;
use adv_cpu_core::{CpuState, Memory};

use super::compile;
use super::offload::{Code, ScopedJob, WorkerJob};
use super::offload::{Entry, Notifications, Session};
use super::plan::{
    self, ColdAction, Decline, JobPlan, MISSES, PROBATION, Probation, RowAction, SAMPLE, Tier1Miss,
};
use super::store::{self, ACTIVE, Active, Faults, Tier1Chain, Tier1Unit, Version, WorkerCode};
use super::{
    CLEAR_GATE, Checks, Native, Owner, Run, SLICE, STALE_GATE, access, call, locked_access,
    timestamp, value, value_access,
};
use crate::facts::{self, Key};
use crate::policy::proved::{ROW_SAMPLE, RowFailure};
use crate::runtime::record_branch;
use crate::runtime::{Event, ExecutionContext, Exit, PlanEvent, Runtime, ScopedOffer, alias_base};
use crate::translate::front::{Unit, decode::Fetcher};
use crate::translate::lower::resident::ResidentCold;
use crate::translate::opt::computation::Proof;
use crate::translate::opt::recovery;
use rustc_hash::FxHashSet;
use std::sync::Arc;

impl Native {
    /// Runs the unit at `cpu.eip` on positive fuel, allowing that unit's
    /// first prepaid region to finish. A fault returns
    /// `Exit::Event(Event::Replay)` with EIP at the faulting instruction and the guest state
    /// exactly that before it.
    pub fn run(
        &mut self,
        cpu: &mut CpuState,
        memory: &mut Memory,
        stop: u32,
        fuel: u64,
        bridge: *mut std::ffi::c_void,
    ) -> Result<Option<Run>, String> {
        self.bridge = bridge;
        self.invalidate(memory);
        self.observe_stop(stop);
        self.install_optimized(false);
        let (entry, translated) = match self.code.tier1(cpu.eip) {
            Some(entry) => (entry, 0),
            None if !memory.executable(cpu.eip, 1) => {
                return Ok(Some(Run {
                    exit: Exit::Event(Event::Fetch),
                    retired: 0,
                    translated: 0,
                }));
            }
            None if !Fetcher::fetchable(memory, cpu.eip) => {
                self.diagnostics.untracked += 1;
                return Ok(Some(Run {
                    exit: Exit::Untracked,
                    retired: 0,
                    translated: 0,
                }));
            }
            None if self.first_visit(cpu.eip) => {
                self.diagnostics.first_visits += 1;
                return Ok(Some(Run {
                    exit: Exit::FirstVisit,
                    retired: 0,
                    translated: 0,
                }));
            }
            None => {
                let before = self.code.used();
                let eip = cpu.eip;
                let entry = self.translate(memory, eip, Key::read(cpu, facts::ALL));
                (entry?, self.code.used().saturating_sub(before))
            }
        };
        if fuel == 0 {
            return Ok(None);
        }
        let offer = match self.plans.arrival(cpu.eip) {
            plan::Arrival::Offerable(job) if self.plans.offer_resume != Some(cpu.eip) => {
                let gate = self.published_gate();
                let stale = gate.stale.load(std::sync::atomic::Ordering::Relaxed) != 0;
                (stale || job.sample_offer()) && job.offer_due(gate, super::ticks)
            }
            _ => false,
        };
        if offer && self.offer_job(cpu) {
            return Ok(Some(Run {
                exit: Exit::Computation,
                retired: 0,
                translated,
            }));
        }
        if entry.retired == 0 {
            self.diagnostics.empty += 1;
            // The first instruction is not translated: skip the round trip.
            return Ok(Some(Run {
                exit: Exit::Event(Event::Fetch),
                retired: 0,
                translated,
            }));
        }
        debug_assert_eq!(cpu.lazy_flags[3], 0, "lazy flags settle before entry");
        let budget = fuel.min(SLICE).max(u64::from(entry.retired));
        if self.code.uses_permissions
            || self.checks != Checks::Adaptive
            || self.plans.checked_count() != 0
        {
            self.classify(memory);
        }
        if let Some(word) = &self.interrupt
            && word.load(std::sync::atomic::Ordering::Relaxed)
                & (adv_sync::pending::STOP | adv_sync::pending::WORK)
                != 0
        {
            word.fetch_and(
                !(adv_sync::pending::STOP | adv_sync::pending::WORK),
                std::sync::atomic::Ordering::AcqRel,
            );
        }
        self.code.sync();
        self.prepare_inline(cpu, memory, fuel.max(budget), budget);
        self.runtime.row_suppressed = 0;
        let resumed_repeat = cpu.rep.take().map(|rep| {
            let visible_flags = cpu.eflags;
            cpu.eflags = rep.native_input_flags(cpu);
            (rep, visible_flags)
        });
        let runtime: *mut Runtime = &mut *self.runtime;
        let faults: *const Faults = &self.code.faults;
        let enter = self.code.enter;
        let mut owner = Owner {
            native: self,
            memory,
            bridge,
            translated,
            error: None,
            internal_discovered: Vec::new(),
        };
        // SAFETY: until `enter` returns, `self` is reached only through
        // `owner`, by `dispatch`. `unit` is translated code ending at the
        // stubs, and the runtime points at a live CPU for the call.
        let returned = unsafe {
            (*runtime).countdown = budget;
            (*runtime).cpu = cpu;
            (*runtime).helper = dispatch as *const () as u64;
            (*runtime).lookup_miss = lookup_miss as *const () as u64;
            (*runtime).access = access as *const () as u64;
            (*runtime).value = value as *const () as u64;
            (*runtime).value_access = value_access as *const () as u64;
            (*runtime).locked_access = locked_access as *const () as u64;
            (*runtime).call = call as *const () as u64;
            (*runtime).timestamp = timestamp as *const () as u64;
            (*runtime).owner = &raw mut owner as u64;
            (*runtime).body.memory = owner.memory;
            let previous = ACTIVE.replace(Active::Owner(faults));
            let returned = enter.run(runtime, entry.unit);
            ACTIVE.set(previous);
            returned
        };
        self.runtime.cpu = std::ptr::null_mut();
        self.runtime.owner = 0;
        self.runtime.body.memory = std::ptr::null();
        if returned.reason as u32 != Exit::Optimized.raw() {
            self.runtime.shadow = returned.shadow;
        }
        if let Some(error) = owner.error {
            return Err(error);
        }
        let translated = owner.translated;
        let mut exit = Exit::from_raw(returned.reason as u32)
            .ok_or_else(|| format!("unknown a64 exit {}", returned.reason))?;
        if self.code.slow_paths.is_some() {
            *self.diagnostics.exit_counts.entry(exit.word()).or_default() += 1;
            if exit == Exit::Dispatch
                && returned.reason >> 32 == u64::from(crate::runtime::VALUE_MODEL_DISPATCH)
            {
                self.diagnostics.x87_value_dispatches += 1;
            }
        }
        if let Exit::Plan(event) = exit {
            self.count_plan_event(event.name());
        }
        let budget_exit = exit == Exit::Budget;
        if budget_exit {
            exit = Exit::Dispatch;
        }
        match exit {
            Exit::Optimized => {}
            Exit::Event(Event::Fetch) => self.diagnostics.interprets += 1,
            Exit::Event(Event::Replay | Event::CheckedAccess) => self.diagnostics.faults += 1,
            Exit::Event(_) | Exit::Plan(_) => {}
            Exit::Dispatch => self.diagnostics.dispatches += 1,
            Exit::Reschedule | Exit::Computation | Exit::Recall | Exit::Budget => {}
            Exit::FirstVisit | Exit::Untracked => unreachable!("owner dispatch actions"),
            Exit::Yield => self.runtime.shadow = self.code.shadow.top(),
        }
        // Inline forms may refill the countdown, and a cold exit may leave
        // it negative.
        let mut retired = budget.wrapping_sub(returned.remaining);
        let resident_cold = if exit == Exit::Plan(PlanEvent::ResidentRecovery) {
            let recovery = &self.runtime.recovery;
            let identity = (recovery.head, recovery.version as usize);
            let (refund, site, reason, checked) = self.materialize_inline(cpu, memory);
            retired -= u64::from(refund);
            exit = Exit::Dispatch;
            Some((identity, site, reason, checked, refund))
        } else {
            None
        };
        if exit == Exit::Event(Event::Replay) {
            let pc = self.code.faults.capture.pc.get();
            let site = self.code.faults.site(pc).expect("faulting site");
            self.code.faults.capture.recover(cpu, site);
            retired -= u64::from(site.refund);
            if !site.checked {
                self.unchecked_faults += 1;
                self.strike(memory, site.eip, pc, cpu);
            } else {
                self.diagnostics.checked_faults += 1;
            }
        }
        returned.commit_repeat(cpu);
        if let Some((rep, visible_flags)) = resumed_repeat
            && retired == 0
            && cpu.eip == rep.pc
            && !matches!(exit, Exit::Event(_))
        {
            cpu.rep = Some(rep);
            cpu.eflags = visible_flags;
        }
        if let Some((identity, site, reason, checked, refund)) = resident_cold {
            self.resident_cold_exit(
                identity,
                site,
                reason,
                checked,
                refund,
                returned.remaining.wrapping_neg(),
            );
        }
        let mut scoped = false;
        let mut scope_declined = false;
        if let Exit::Plan(event) = exit {
            let payload = (returned.reason >> 32) as u32;
            match event {
                PlanEvent::BodyStack => self.refuse_body_stack(memory, cpu),
                PlanEvent::DiscoverBackEdge => {
                    let head = cpu.eip;
                    self.stop_counting(head);
                    self.discover(memory, head, head, cpu.clone());
                }
                PlanEvent::EnvironmentMiss => {
                    self.environment_miss(cpu.eip, (payload >> 8) as usize, payload as u8 as usize);
                }
                PlanEvent::OfferReady => exit = Exit::Computation,
                PlanEvent::RowTargetMiss => self.observe_row_target(memory, cpu, payload),
                PlanEvent::RowGuardMiss => self.observe_row_failure(cpu.eip),
                PlanEvent::ResidentSplitOffer(version) => {
                    let head = cpu.eip;
                    if let Some(job) = self.scoped_jobs.get(&(head, version)) {
                        if self.code.version_stats.is_some() {
                            job.policy
                                .scope_offers
                                .set(job.policy.scope_offers.get() + 1);
                        }
                        if self.code.offers_split(head, usize::from(version))
                            && (self.offer_computation()
                                || self.offer_split(job.policy.scope_cost(payload)))
                        {
                            exit = Exit::Computation;
                            scoped = true;
                            self.scoped_offer = Some(ScopedOffer {
                                version,
                                row_work: self.runtime.row_work,
                                entry: job.worker.code.placed.entry,
                                generation: self.code.generation,
                            });
                        }
                    }
                    scope_declined = !scoped;
                }
                PlanEvent::Probation(version) => {
                    self.finish_probation(payload, usize::from(version))
                }
                PlanEvent::ResidentRecovery | PlanEvent::WorkerBranch(_) => unreachable!(),
            }
            if exit != Exit::Computation {
                exit = Exit::Dispatch;
            }
        }
        for sample in owner.internal_discovered {
            self.apply_back_edge_sample(memory, sample);
        }
        if budget_exit {
            self.sample(memory, cpu);
        }
        self.install_optimized(false);
        if let Some(forms) = &mut self.retired_forms {
            // Native REP elements stay in translated fuel: telling them
            // apart would cost emitted code in every REP loop.
            forms[0] += retired;
        }
        if retired != 0 {
            self.progressed();
        }
        if scope_declined || (exit == Exit::Computation && !scoped && !self.offer_job(cpu)) {
            self.set_computation_offer(self.offload.enabled, Some(cpu.eip));
            exit = Exit::Dispatch;
        }
        let retired = retired as u32;
        Ok(Some(Run {
            exit,
            retired,
            translated,
        }))
    }

    pub(super) fn published_gate(&self) -> &OfferGate {
        if !self.offload.enabled || self.plans.baseline_only {
            &CLEAR_GATE
        } else if self.bridge.is_null() {
            &STALE_GATE
        } else {
            // SAFETY: the run's bridge outlives it.
            let bridge = unsafe { &*self.bridge.cast::<Bridge<'_>>() };
            bridge
                .handler
                .as_ref()
                .and_then(|handler| handler.offer_gate())
                .unwrap_or(&STALE_GATE)
        }
    }

    /// Points the runtime at what head entry blocks and inline forms read
    /// during a translated run of `budget` within `fuel` (A64-OPT-8).
    pub(super) fn prepare_inline(
        &mut self,
        cpu: &CpuState,
        memory: &Memory,
        fuel: u64,
        budget: u64,
    ) {
        static NEVER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let gate = std::ptr::from_ref(self.published_gate()) as u64;
        let (pending, mask) = self.interrupt.as_ref().map_or((NEVER.as_ptr(), 0), |word| {
            (word.as_ptr(), self.pending_mask)
        });
        self.code.faults.capture.pc.set(0);
        let runtime = &mut *self.runtime;
        if self.count_resident_entries {
            runtime.resident_entry_counter = (&raw mut self.diagnostics.resident_entries) as u64;
            runtime.resident_crossing_counter =
                (&raw mut self.diagnostics.resident_page_crossings) as u64;
        }
        runtime.offer_gate = gate;
        runtime.alias = alias_base(memory);
        runtime.plain_pages = memory.plain_pages() as u64;
        runtime.window_spans = memory.window_spans().map(|ptr| ptr as u64);
        runtime.memory_base = memory.base;
        runtime.memory_pages = (memory.len / adv_cpu_core::memory::PAGE_SIZE) as u32;
        runtime.rep_pending = u32::from(cpu.rep.is_some());
        runtime.recall = NEVER.as_ptr() as u64;
        runtime.recall_mask = 0;
        runtime.computation_pending = pending as u64;
        runtime.computation_pending_mask = mask;
        runtime.head_exit = 0;
        runtime.deferred = 0;
        runtime.completed = 0;
        runtime.completed_limit = fuel.min(u64::from(u32::MAX)) - budget;
        runtime.recovery.saved = self.inline_saved.as_mut_ptr() as u64;
    }

    /// Rebuilds the guest state at a cold inline exit from the entry
    /// baseline and the exit's record. Returns the record's refund.
    pub(super) fn materialize_inline(
        &mut self,
        cpu: &mut CpuState,
        memory: &Memory,
    ) -> (u32, u32, ResidentCold, bool) {
        let recovery = &self.runtime.recovery;
        let eip = recovery.head;
        let index = recovery.version as usize;
        let head = self.code.resident(eip).unwrap();
        let version = &head.versions[index];
        let record = &version.recovery.records[recovery.record as usize];
        recovery::restore_inline_entry(&recovery.entry, cpu);
        let (alias, entry_esp) = (alias_base(memory), cpu.regs[4]);
        self.runtime.shadow = recovery.dump.x[28];
        let (refund, _) = recovery::materialize(
            record,
            &recovery.dump,
            &self.inline_saved,
            &version.recovery.flags,
            cpu,
            // SAFETY: the resident entry admitted these frame bytes.
            |offset, bytes| unsafe {
                let at = alias.wrapping_add(u64::from(entry_esp.wrapping_add_signed(offset)));
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), at as *mut u8, bytes.len());
            },
        );
        if version.recovery.mmx || cpu.mmx_mode != 0 {
            cpu.refresh_mmx();
        }
        let fault = self.code.faults.site(self.code.faults.capture.pc.get());
        if let Some((site, reason)) = ResidentCold::decode(recovery.detail) {
            (refund, site, reason, false)
        } else if let Some(site) = fault {
            (refund, site.eip, ResidentCold::HostFault, site.checked)
        } else {
            (refund, record.eip, ResidentCold::Other, false)
        }
    }
}

impl Native {
    pub(crate) fn with_computation(
        &mut self,
        memory: &mut Memory,
        retired: &mut u64,
        pending: (Arc<std::sync::atomic::AtomicU32>, u32),
        wake: Option<adv_sync::WakeEvent>,
        visit: &mut dyn FnMut(
            &mut dyn adv_cpu_core::backend::Computation<Option<ExecutionContext>>,
        ),
    ) {
        if self.plans.baseline_only
            || (self.worker_jobs.is_empty() && self.scoped_jobs.is_empty())
            || memory.attach().is_err()
        {
            return;
        }
        self.invalidate(memory);
        self.code.sync();
        let entry = Entry {
            enter: self.code.enter,
            opt_enter: self.code.opt_enter,
            window: self.runtime.window,
            alias: alias_base(memory),
            fault: self.code.stubs.fault,
        };
        let mut updates = Vec::new();
        if let Some(mut session) = Session::new(
            (entry, &self.code),
            (&self.worker_jobs, &self.scoped_jobs, &self.plans),
            memory,
            &mut self.offload,
            (retired, &mut self.retired_forms),
            Notifications { pending, wake },
            (self.pace.get(), &mut self.diagnostics.split),
        ) {
            visit(&mut session);
            updates = session.take_scope_updates();
        }
        for eip in updates {
            self.publish_scope(eip);
        }
    }
}

impl Native {
    pub(super) fn stop_counting(&mut self, eip: u32) {
        self.count_sensor_decision("back_edge_discovery");
        self.plans.counter_at(eip).unwrap().set(u32::MAX);
        self.code.stop_counting(eip);
        self.repoint_resident_edges(eip, None);
    }

    fn repoint_resident_edges(&mut self, eip: u32, resident: Option<usize>) {
        if let Some(head) = self.plans.resident_mut(eip) {
            head.link_samples.clear();
        }
        let (counting, offer) = self.plans.routing(eip);
        self.code
            .repoint_resident_edges(eip, resident, counting, offer);
        self.code.sync();
    }

    fn repoint_tier1(&mut self, eip: u32, checked: usize) {
        self.resolve_tables(eip, checked);
        let (counting, offer) = self.plans.routing(eip);
        self.code.repoint_tier1(eip, checked, counting, offer);
    }

    pub(super) fn patch(&mut self, point: usize, eip: u32, unit: usize) {
        let (counting, offer) = self.plans.routing(eip);
        self.code.patch_link(eip, point, unit, counting, offer);
    }

    pub(crate) fn clear(&mut self) {
        self.diagnostics.clears += 1;
        self.code.clear();
        self.worker_jobs.clear();
        self.scoped_jobs.clear();
        self.offload.admission.clear();
        self.plans.clear_overlays();
        self.forget_dynamic();
    }

    pub(super) fn count_plan_event(&mut self, event: &'static str) {
        if self.count_resident_entries {
            *self.diagnostics.plan_events.entry(event).or_default() += 1;
        }
    }

    fn count_sensor_decision(&mut self, decision: &'static str) {
        if self.count_resident_entries {
            *self
                .diagnostics
                .sensor_decisions
                .entry(decision)
                .or_default() += 1;
        }
    }

    pub(super) fn apply_back_edge_sample(
        &mut self,
        memory: &mut Memory,
        sample: super::BackEdgeSample,
    ) {
        if self.code.generation == sample.generation
            && self.code.contains_unit(sample.source, sample.unit)
            && self.plans.resident(sample.at.eip).is_none()
        {
            self.discover(memory, sample.at.eip, sample.source, sample.at);
        }
    }

    pub(super) fn resident_cold_exit(
        &mut self,
        (eip, version_index): (u32, usize),
        site: u32,
        reason: ResidentCold,
        checked: bool,
        refund: u32,
        elapsed: u64,
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
        let count = head.record_cold(site, reason);
        if count.is_power_of_two() {
            crate::jit_map::optimized(
                eip,
                format_args!("resident\tcold\t{site}\t{reason:?}\t{count}"),
            );
        }
        if head.declined() {
            return;
        }
        let site_versions = self.code.chains[&eip].resident.as_ref().unwrap();
        let version = &site_versions.versions[version_index];
        let measured = head.probation[version_index].is_some() || head.cold_window.is_some();
        let retired = elapsed
            .wrapping_sub(version.slot.countdown[0].get())
            .wrapping_sub(u64::from(refund));
        match head.cold_action(version_index, site, reason, checked, count, retired) {
            ColdAction::FinishProbation => {
                self.finish_probation(eip, version_index);
                return;
            }
            ColdAction::CheckStream => {
                head.check_stream(compile::stream_sites(&site_versions.unit, site), site);
                crate::jit_map::optimized(
                    eip,
                    format_args!("resident\tchecked\t{site}\t{reason:?}"),
                );
                self.rebuild_resident_versions(eip);
                return;
            }
            ColdAction::Ignore => return,
            ColdAction::Observe => {}
        }
        let cold_iterations = if measured {
            version.iterations(retired)
        } else {
            0
        };
        if head.cold_window.is_none() {
            self.code.measure_cold(eip, true);
        }
        if head.observe_cold(site, reason, self.code.accounted(eip), cold_iterations) {
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
            self.code.measure_cold(eip, false);
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
    pub(super) fn strike(&mut self, memory: &mut Memory, eip: u32, pc: usize, cpu: &CpuState) {
        if !self.plans.strike(eip) {
            return;
        }
        let index = self.code.placed.partition_point(|&(start, _)| start <= pc);
        let Some(&(start, entry)) = index.checked_sub(1).map(|index| &self.code.placed[index])
        else {
            return;
        };
        // A unit dropped before stays in `placed`.
        if self.code.contains_unit(entry, start) {
            self.replace_unit(memory, entry, Key::read(cpu, facts::ALL));
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
        observed: Key,
    ) -> Result<usize, String> {
        let generation = self.code.generation;
        let entry = match entry {
            Some(entry) => entry,
            None => self.translate(memory, eip, observed)?,
        };
        // Translating may have cleared the cache and the link point with it.
        if point != 0 && self.code.generation == generation {
            self.patch(point, eip, entry.unit);
            if self.code.back_points.contains(&point) {
                // Reload the back edge's identity after the dispatch stub.
                return Ok(point - 4);
            }
            if self.code.offer(eip).is_some()
                || self.code.direct_points.contains_key(&point)
                    && self
                        .code
                        .resident(eip)
                        .and_then(store::ResidentSite::entry)
                        .is_some()
            {
                return Ok(point);
            }
        }
        Ok(entry.unit)
    }

    /// Rebuild with checked argument stores at each site after the entry's
    /// hoisted stack guard refused the span.
    pub(super) fn refuse_body_stack(&mut self, memory: &mut Memory, cpu: &CpuState) {
        let eip = cpu.eip;
        self.plans.mark_checked(eip);
        if !self.replace_unit(memory, eip, Key::read(cpu, facts::ALL)) {
            self.clear();
        }
    }

    /// Tier-1 replacement keeps old continuations and all their watches.
    pub(super) fn replace_unit(&mut self, memory: &mut Memory, eip: u32, observed: Key) -> bool {
        let Some(new) = self.emit(memory, eip, observed) else {
            return false;
        };
        self.repoint_tier1(eip, new.unit);
        self.code.sync();
        true
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
        self.code.publish_switch(eip, unit);
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
        self.remove_scoped_jobs(eip);
        let dropped = self.code.invalidate_unit(eip);
        self.plans.invalidate(eip);
        self.worker_jobs.remove(&eip);
        self.offload.admission.invalidate((eip, None));
        dropped
    }

    /// Empties the indirect table and the shadow stack, which may lead to
    /// dropped units.
    pub(super) fn forget_dynamic(&mut self) {
        self.diagnostics.global_invalidations += 1;
        self.runtime.shadow = self.code.forget_dynamic();
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

    /// Whether code at `eip` is known to run, so translating it pays. A
    /// direct-body entry has no guest code for a first visit: the host call
    /// runs before any unit would, so it counts as run.
    pub(super) fn ran(&self, eip: u32) -> bool {
        self.plans.ran(eip, self.first_visits) || self.direct_entry(eip)
    }

    fn direct_entry(&self, eip: u32) -> bool {
        if self.bridge.is_null() {
            return false;
        }
        // SAFETY: `run` sets the bridge for the owner call that reaches here.
        unsafe { &*self.bridge.cast::<adv_cpu_core::trap::Bridge<'_>>() }
            .handler
            .as_ref()
            .is_some_and(|handler| handler.direct_body(eip).is_some())
    }

    /// Whether `eip`, which has no unit, runs for the first time; it will
    /// not be again.
    pub(super) fn first_visit(&mut self, eip: u32) -> bool {
        self.plans
            .first_visit(eip, self.first_visits, self.code.tier1(eip).is_some())
    }

    /// Translates the unit at `eip`, and ahead of time the units its main
    /// path leaves to that have run before, so they share one cache sync
    /// and link directly. Every link point of these units to a unit that
    /// exists is patched.
    pub(super) fn translate(
        &mut self,
        memory: &mut Memory,
        eip: u32,
        observed: Key,
    ) -> Result<Tier1Unit, String> {
        self.code.points.clear();
        let entry = match self.emit(memory, eip, observed) {
            Some(entry) => entry,
            None => {
                self.clear();
                self.code.points.clear();
                self.emit(memory, eip, observed)
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
            let unit = match self.code.tier1(target) {
                Some(entry) => entry.unit,
                // A full cache waits for the next miss to clear it.
                None if main
                    && index <= first
                    && self.ran(target)
                    && Fetcher::fetchable(memory, target) =>
                {
                    match self.emit(memory, target, observed) {
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

    fn emit(&mut self, memory: &mut Memory, eip: u32, observed: Key) -> Option<Tier1Unit> {
        let loops = self.promotion == 0 || self.plans.promoted(eip);
        let unit = self.front_unit(memory, eip, loops, false, None);
        let at = self.code.next();
        let mut work = std::mem::take(&mut self.compiler.tier1);
        let all = self.checks != Checks::Adaptive;
        let mask = unit.fact_mask();
        let key = observed.project(mask);
        let helpers = self.code.helpers.len() as u32;
        let arrivals = if mask == 0 {
            0
        } else {
            self.code.counter(eip, 0)
        };
        work.lower_tier1(
            &unit,
            eip,
            &|pc| all || self.plans.checked(pc),
            helpers,
            self.features,
            at,
            self.code.stubs,
            !self.plans.baseline_only,
            key,
            arrivals,
        );
        if work.retired() == 0 {
            self.diagnostics.empty_units[usize::from(unit.trace.failure & 3)] += 1;
        }
        let entry = self.place_tier1(memory, eip, &work, &unit, key, true);
        if let Some(entry) = entry {
            self.code.helpers.extend(unit.helpers().iter().cloned());
            self.code.set_tier1(
                eip,
                Tier1Chain {
                    unit,
                    mask,
                    helpers,
                    versions: vec![Version { key, entry }],
                },
            );
            self.code.repoint_resident_fallback(eip, entry.unit);
            if mask != 0
                && let Some(stats) = &mut self.code.version_stats
            {
                stats.get_mut(&eip).unwrap().built += 1;
            }
        }
        self.compiler.tier1 = work;
        entry
    }

    fn place_tier1(
        &mut self,
        memory: &mut Memory,
        eip: u32,
        work: &compile::Workspace,
        unit: &Unit,
        key: Key,
        head: bool,
    ) -> Option<Tier1Unit> {
        let cost =
            head && (self.plans.counter_at(eip).is_some() || !work.internal_links().is_empty());
        let entry = self
            .code
            .place_tier1(eip, &work.placement(), unit, key, cost, |target| {
                self.plans.counter(target)
            })?;
        let mut targets: Vec<_> = unit
            .tables
            .iter()
            .flat_map(|table| table.targets.iter().copied())
            .collect();
        targets.sort_unstable();
        targets.dedup();
        for target in targets {
            if let Some(entry) = self.code.tier1(target) {
                self.resolve_tables(target, entry.unit);
            }
        }
        if self.code.uses_permissions {
            self.classify(memory);
        }
        Some(entry)
    }

    pub(super) fn miss_version(
        &mut self,
        memory: &mut Memory,
        eip: u32,
        observed: Key,
    ) -> Result<usize, String> {
        let Some(chain) = self.code.tier1_chain(eip) else {
            return self
                .translate(memory, eip, observed)
                .map(|entry| entry.unit);
        };
        // A fact transfer can reach an entry whose standalone unit has no facts.
        if chain.mask == 0 {
            return Ok(chain.versions[0].entry.unit);
        }
        let action = plan::tier1_miss(chain, observed);
        if let Some(stats) = &mut self.code.version_stats {
            stats.get_mut(&eip).unwrap().misses += 1;
        }
        let (key, position, joining) = match action {
            Tier1Miss::Match(entry) => return Ok(entry),
            Tier1Miss::Build {
                key,
                position,
                joining,
            } => (key, position, joining),
        };
        let chain = self.code.take_tier1(eip).unwrap();
        let arrivals = self.code.counter(eip, position);
        let mut work = std::mem::take(&mut self.compiler.tier1);
        let all = self.checks != Checks::Adaptive;
        work.lower_tier1(
            &chain.unit,
            eip,
            &|pc| all || self.plans.checked(pc),
            chain.helpers,
            self.features,
            self.code.next(),
            self.code.stubs,
            !self.plans.baseline_only,
            key,
            arrivals,
        );
        let entry = self.place_tier1(memory, eip, &work, &chain.unit, key, false);
        self.compiler.tier1 = work;
        let Some(entry) = entry else {
            self.clear();
            return self
                .translate(memory, eip, observed)
                .map(|entry| entry.unit);
        };
        self.code
            .install_tier1_version(eip, chain, position, Version { key, entry });
        if joining {
            self.repoint_tier1(eip, self.code.tier1(eip).unwrap().unit);
        }
        if let Some(stats) = &mut self.code.version_stats {
            let stats = stats.get_mut(&eip).unwrap();
            stats.built += 1;
            stats.joins += u64::from(joining);
        }
        Ok(entry.unit)
    }

    pub(super) fn discover(&mut self, memory: &mut Memory, eip: u32, source: u32, at: CpuState) {
        if self.plans.baseline_only {
            return;
        }
        let unit = self.front_unit(memory, eip, true, true, None);
        let guarded = unit.trace.insts[..unit.trace.primary].iter().any(|inst| {
            matches!(inst.op, crate::translate::front::decode::Op::CallIndirect(
                crate::translate::front::decode::Src::Mem(slot)
            ) if slot.base.is_some() && slot.index.is_none() && !slot.fs)
        });
        let extension = guarded.then(|| self.front_unit(memory, eip, true, true, Some(&at.regs)));
        let code = self.code.tier1(source).unwrap().unit;
        self.plans.discover(eip, source, code);
        self.code.prepare_resident(eip, unit, SAMPLE);
        self.request_resident_unit(eip, compile::Task::Discover, at, extension);
    }

    fn observe_row_target(&mut self, memory: &mut Memory, cpu: &CpuState, target: u32) {
        let eip = cpu.eip;
        let site = self.code.resident(eip).unwrap();
        site.row_feedback
            .as_deref()
            .unwrap()
            .target_next
            .set(ROW_SAMPLE);
        let plan = self.plans.resident_mut(eip).unwrap();
        if plan.pending.is_none()
            && plan.row_targets.len() < plan::VERSIONS
            && !plan.row_targets.contains(&target)
        {
            plan.row_targets.push(target);
            let unit = self.front_unit(memory, eip, true, true, Some(&cpu.regs));
            self.request_resident_unit(eip, compile::Task::Append, cpu.clone(), Some(unit));
        }
    }

    fn observe_row_failure(&mut self, eip: u32) {
        let site = self.code.resident(eip).unwrap();
        let feedback = site.row_feedback.as_ref().unwrap();
        feedback.next.set(ROW_SAMPLE);
        let head = self.plans.resident(eip).unwrap();
        if head.pending.is_some() || head.declined() {
            return;
        }
        // Price recurring failures after same-run retries have been coalesced.
        if let Some(row) = self.row_ledger(eip)
            && [
                RowFailure::Parameter,
                RowFailure::Alias,
                RowFailure::Count,
                RowFailure::Span,
            ]
            .iter()
            .any(|&reason| row.failures[reason as usize] != 0)
            && self.code.install_row_retry(eip)
        {
            if self.code.resident(eip).unwrap().row_entry.is_none() {
                self.code.settle_sample(eip);
            }
            self.repoint_resident_edges(eip, self.code.resident(eip).unwrap().entry());
            return;
        }
        let site = self.code.resident(eip).unwrap();
        let mut actions = Vec::new();
        for (index, version) in site.versions.iter().enumerate() {
            if version.enabled
                && let Some(price) = version.row_price
            {
                let measured =
                    plan::RowLedger::priced(price, version.row_counts(), version.row_failures());
                let ledger = head.row_version(index, measured);
                actions.push((
                    index,
                    head.observe_row(ledger, version.row_measuring, version.probation),
                ));
            }
        }
        for (index, action) in actions {
            match action {
                RowAction::Keep => {}
                RowAction::Sample(active) => self.code.measure_rows(eip, index, active),
                RowAction::Withdraw => self.withdraw_row(eip, index),
            }
            if self.plans.resident(eip).unwrap().declined() {
                break;
            }
        }
        self.code.sync();
    }

    fn withdraw_row(&mut self, eip: u32, index: usize) {
        self.count_sensor_decision("row_withdrawn");
        self.stop_cold_window(eip);
        let site = self.code.resident_mut(eip).unwrap();
        site.versions[index].enabled = false;
        if !site.versions.iter().any(|v| v.enabled) {
            self.decline_resident(eip, Decline::Unprofitable);
            return;
        }
        self.remove_scoped_job(eip, index as u8);
        let head = self.plans.resident(eip).unwrap();
        if let Some(slot) = self
            .code
            .resident(eip)
            .unwrap()
            .row_entry
            .as_ref()
            .map(|row| row.slot)
            && !self.code.publish_row_targets(eip, slot, &head.row_targets)
        {
            self.decline_resident(eip, Decline::CacheFull);
            return;
        }
        self.code.publish_resident(eip, head.retry_period());
        self.repoint_resident_edges(eip, self.code.resident(eip).unwrap().entry());
    }

    pub(super) fn request_resident(&mut self, eip: u32, task: compile::Task, at: CpuState) {
        self.request_resident_unit(eip, task, at, None);
    }

    fn request_resident_unit(
        &mut self,
        eip: u32,
        task: compile::Task,
        mut at: CpuState,
        unit: Option<Arc<Unit>>,
    ) {
        at.refresh_mmx();
        let site = self.code.resident(eip).unwrap();
        let unit = unit.unwrap_or_else(|| match task {
            compile::Task::Replace(index) => Arc::clone(&site.versions[index].unit),
            _ => Arc::clone(&site.unit),
        });
        let checked = unit
            .trace
            .insts
            .iter()
            .filter_map(|inst| self.plans.checked(inst.pc).then_some(inst.pc))
            .collect::<FxHashSet<_>>();
        let head = self.plans.resident_mut(eip).unwrap();
        head.pending = Some(task);
        if self.code.resident(eip).unwrap().entry().is_some() {
            self.code.publish_resident(eip, None);
        }
        let mut checked = checked;
        checked.extend(&head.checked);
        self.compiler.request(compile::Request {
            eip,
            generation: self.code.generation,
            code: head.code,
            task,
            unit,
            proof: self.worker_jobs.get(&eip).map(|job| job.proof.clone()),
            at,
            checked,
            all_checked: self.checks != Checks::Adaptive,
            refused: head.refused.clone(),
            cuts: head.cuts.clone(),
            count_entries: self.count_resident_entries,
        });
    }

    pub(super) fn retry_resident(&mut self, cpu: &CpuState, version: usize) {
        let eip = cpu.eip;
        let head = self.plans.resident(eip).unwrap();
        if head.retry(version) {
            self.count_sensor_decision("context_retry");
            self.request_resident(eip, compile::Task::Append, cpu.clone());
            self.code.sync();
        }
    }

    pub(super) fn sample_resident_link(&mut self, cpu: &CpuState, eip: u32, point: usize) {
        let head = self.plans.resident_mut(eip).unwrap();
        let site = self.code.resident(eip).unwrap();
        site.sample.set(SAMPLE);
        if !head.sampling() {
            return;
        }
        self.diagnostics.rust_decisions_on_decided_sites += u64::from(head.probes == 0);
        let version = site
            .versions
            .iter()
            .position(|v| v.enabled && cpu.rep.is_none() && v.key.holds(cpu));
        let (link, settled) = head.sample_link(point, version);
        if let Some(version) = link {
            self.code.specialize_link(eip, point, version);
        }
        if settled {
            self.code.settle_sample(eip);
            self.count_sensor_decision("resident_links_settled");
        }
        if settled || link.is_some() {
            self.code.sync();
        }
    }

    pub(super) fn environment_miss(&mut self, eip: u32, index: usize, fact: usize) {
        let site = self.code.environment_failed(eip, index, fact);
        self.count_sensor_decision("environment_rebuild");
        self.refuse_resident_fact(eip, site);
    }

    pub(super) fn refuse_resident_fact(&mut self, eip: u32, site: u32) {
        let versions = &self.code.resident(eip).unwrap().versions;
        let affected = versions.iter().enumerate().filter_map(|(i, version)| {
            (version.enabled && version.plain_sites.contains(&site)).then_some(i)
        });
        if self.plans.resident_mut(eip).unwrap().refuse(site, affected) {
            self.rebuild_resident(eip);
        }
    }

    pub(super) fn cut_resident_site(&mut self, eip: u32, site: u32, reason: ResidentCold) {
        let head = self.plans.resident_mut(eip).unwrap();
        if head.cuts.insert(site) {
            if self
                .code
                .resident(eip)
                .unwrap()
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
        let versions = &self.code.resident(eip).unwrap().versions;
        let rebuilds = versions
            .iter()
            .enumerate()
            .fold(0, |mask, (index, version)| {
                mask | (u8::from(version.enabled) << index)
            });
        self.plans.resident_mut(eip).unwrap().rebuilds = rebuilds;
        self.rebuild_resident(eip);
    }

    pub(super) fn rebuild_resident(&mut self, eip: u32) {
        let head = self.plans.resident_mut(eip).unwrap();
        if !head.needs_rebuild() {
            return;
        }
        self.stop_cold_window(eip);
        self.code.resident_mut(eip).unwrap().suspended = true;
        self.repoint_resident_edges(eip, None);
        let head = self.plans.resident_mut(eip).unwrap();
        if let Some(index) = head.next_rebuild() {
            let at = self.code.resident(eip).unwrap().versions[index].at.clone();
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
                    if let Some(period) = head.retry_period() {
                        self.code.publish_resident(eip, Some(period));
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
            if head.accepts(done.task) {
                let mut scoped = done.job;
                head.has_proof |= scoped.is_some();
                if scoped.as_ref().is_some_and(|job| job.scoped.is_none()) {
                    let job = scoped.take().unwrap();
                    self.place_job(eip, &done.at, Arc::clone(&done.unit), job, None);
                    self.install_offer_link(eip);
                }
                match (done.resident, done.pass_through) {
                    (Some(Ok(form)), _) => {
                        let site = self.code.resident(eip).unwrap();
                        if site.row_entry.as_ref().is_some_and(|row| {
                            form.lowered
                                .row_target
                                .is_none_or(|target| row.slot != (target.base, target.offset))
                        }) {
                            if done.task != compile::Task::Append {
                                self.decline_resident(eip, Decline::CacheFull);
                            }
                            self.rebuild_resident(eip);
                            continue;
                        }
                        let at = done.at;
                        let index = match done.task {
                            compile::Task::Replace(index) => index,
                            _ => self.code.resident(eip).unwrap().versions.len(),
                        };
                        match self.install_resident(
                            eip,
                            index,
                            at.clone(),
                            form,
                            Arc::clone(&done.unit),
                        ) {
                            Ok(()) => {
                                if let Some(job) = scoped {
                                    self.place_job(eip, &at, done.unit, job, Some(index as u8));
                                }
                                self.publish_scope(eip);
                            }
                            Err(reason) => self.decline_resident(eip, reason),
                        }
                    }
                    (Some(Err(reject)), target) => {
                        let head = self.plans.resident_mut(eip).unwrap();
                        let slot = match (done.task, target) {
                            (compile::Task::Discover, Some(target)) => {
                                head.row_targets.push(target.value);
                                Some((target.base, target.offset))
                            }
                            (compile::Task::Append, _) => self
                                .code
                                .resident(eip)
                                .unwrap()
                                .row_entry
                                .as_ref()
                                .map(|row| row.slot),
                            _ => None,
                        };
                        if let Some(slot) = slot {
                            if self.code.publish_row_targets(eip, slot, &head.row_targets) {
                                self.repoint_resident_edges(
                                    eip,
                                    self.code.resident(eip).unwrap().entry(),
                                );
                            } else {
                                self.decline_resident(eip, Decline::CacheFull);
                            }
                        } else if done.task != compile::Task::Append {
                            self.decline_resident(eip, Decline::Compile(reject));
                        }
                    }
                    _ => {}
                }
            }
            self.rebuild_resident(eip);
        }
    }

    fn install_resident(
        &mut self,
        eip: u32,
        index: usize,
        at: CpuState,
        mut form: compile::ResidentForm,
        unit: Arc<Unit>,
    ) -> Result<(), Decline> {
        let lowered = &mut form.lowered;
        let source = self.plans.resident(eip).unwrap().source;
        let tier1 = self.code.tier1_words.get(&source).and_then(|words| {
            lowered
                .iteration_pcs
                .iter()
                .try_fold(0u32, |sum, pc| words.get(pc).map(|&cost| sum + cost))
        });
        let probation = Probation::price(
            tier1,
            lowered.iteration_words,
            lowered.iteration_retired,
            lowered.entry_words + lowered.exit_words,
        )?;
        let gate = self.plans.job(eip).map(JobPlan::gate);
        self.stop_cold_window(eip);
        let recovery = compile::ResidentRecovery {
            records: std::mem::take(&mut form.lowered.records),
            flags: std::mem::take(&mut form.lowered.flags),
            mmx: form.built.assumed.mmx,
        };
        let spill_cells = form.lowered.spill_cells;
        let row_target = form.lowered.row_target;
        let previous = self
            .code
            .resident(eip)
            .unwrap()
            .versions
            .get(index)
            .and_then(|version| {
                Some(plan::RowLedger::priced(
                    version.row_price?,
                    version.row_counts(),
                    version.row_failures(),
                ))
            });
        if !self.code.place_resident(
            eip,
            index,
            at,
            form.lowered.key,
            std::mem::take(&mut form.built.plain_sites),
            recovery,
            form.lowered.iteration_retired,
            gate,
            (probation.is_some() || form.lowered.has_rows()).then_some(PROBATION),
            MISSES,
            form.placement(),
            unit,
            row_target,
        ) {
            return Err(Decline::CacheFull);
        }
        self.remove_scoped_job(eip, index as u8);
        if self.inline_saved.len() < spill_cells {
            self.inline_saved.resize(spill_cells, 0);
        }
        let head = self.plans.resident_mut(eip).unwrap();
        if let Some(previous) = previous {
            head.archive_row(index, previous);
        }
        let period = head.installed(index, probation);
        self.code.resident_mut(eip).unwrap().suspended = head.rebuilds != 0;
        if let Some(target) = row_target {
            if !head.row_targets.contains(&target.value) {
                head.row_targets.push(target.value);
            }
            if !self
                .code
                .publish_row_targets(eip, (target.base, target.offset), &head.row_targets)
            {
                return Err(Decline::CacheFull);
            }
        } else {
            self.code.start_link_sampling(eip, SAMPLE);
        }
        self.code.publish_resident(eip, period);
        self.repoint_resident_edges(eip, self.code.resident(eip).unwrap().entry());
        Ok(())
    }

    fn install_offer_link(&mut self, eip: u32) {
        if let Some(job) = self.plans.job(eip) {
            self.code.install_offer(eip, job.gate());
        }
    }

    pub(super) fn finish_probation(&mut self, eip: u32, index: usize) {
        self.stop_cold_window(eip);
        let probation = self.plans.resident_mut(eip).unwrap().probation[index].take();
        let (entries, retired) = self.code.finish_probation(eip, index);
        let Some(probation) = probation else {
            self.code.sync();
            return;
        };
        if probation.profitable(entries, retired) {
            self.code.sync();
            self.count_sensor_decision("probation_kept");
        } else {
            crate::jit_map::demoted(eip, index, entries, retired / probation.iteration_retired);
            self.count_sensor_decision("probation_declined");
            self.decline_resident(eip, Decline::Unprofitable);
        }
    }

    pub(super) fn decline_resident(&mut self, eip: u32, reason: Decline) {
        if let Some(summary) = self.row_ledger(eip) {
            self.plans.resident_mut(eip).unwrap().row = summary;
        }
        self.stop_cold_window(eip);
        let head = self.plans.resident_mut(eip).unwrap();
        if head.decline_with(reason) {
            let reason = head.decline().unwrap();
            crate::jit_map::optimized(eip, format_args!("coverage-gap\t{reason}"));
            self.diagnostics.proof_without_resident += 1;
        }
        self.repoint_resident_edges(eip, None);
        self.remove_scoped_jobs(eip);
        let site = self.code.resident_mut(eip).unwrap();
        site.versions.clear();
        site.row_entry = None;
    }

    /// Budget samples select Tier-1 loop traces.
    pub(super) fn sample(&mut self, memory: &mut Memory, cpu: &CpuState) {
        let eip = cpu.eip;
        if self.promotion != 0 && !self.is_stop(eip) && self.plans.sample(eip, self.promotion) {
            self.diagnostics.promotions += 1;
            crate::jit_map::optimized(eip, format_args!("promoted\t-\t-"));
            self.replace_unit(memory, eip, Key::read(cpu, facts::ALL));
        }
    }

    #[cfg(test)]
    pub(super) fn prepare_job(&mut self, memory: &mut Memory, stop: u32, at: &CpuState) {
        self.observe_stop(stop);
        if self.code.tier1(at.eip).is_none() {
            self.translate(memory, at.eip, Key::read(at, facts::ALL))
                .unwrap();
        }
        self.discover(memory, at.eip, at.eip, at.clone());
        self.install_optimized(true);
        if !self.plans.resident(at.eip).unwrap().declined() {
            self.decline_resident(at.eip, Decline::Unprofitable);
        }
    }
}

/// Called by the `dispatch` stub with the guest EIP and the link point to
/// patch, or 0. Returns the unit to continue at, or 0 to leave.
pub(super) unsafe extern "C" fn lookup_miss(runtime: *mut Runtime, eip: u32, branch: u64) -> usize {
    // SAFETY: the current run owns the CPU and the dispatch context.
    unsafe {
        record_branch(&mut *(*runtime).cpu, branch, eip);
        let owner = &*((*runtime).owner as *const Owner);
        (*owner.native).count_plan_event("lookup_miss");
    }
    unsafe { dispatch(runtime, eip, 0) }
}

pub(super) unsafe extern "C" fn dispatch(runtime: *mut Runtime, eip: u32, point: usize) -> usize {
    // SAFETY: `run` points `owner` at a live `Owner` whose pointees it does
    // not touch while translated code runs.
    let owner = unsafe { &mut *((*runtime).owner as *mut Owner) };
    let (native, memory) = unsafe { (&mut *owner.native, &mut *owner.memory) };
    if owner.error.is_some() {
        return 0;
    }
    if native.is_stop(eip) {
        return 0;
    }
    if point == usize::MAX {
        let before = native.code.used();
        let observed = Key::read(unsafe { &*(*runtime).cpu }, facts::ALL);
        let entry = match native.miss_version(memory, eip, observed) {
            Ok(entry) => entry,
            Err(error) => {
                owner.error = Some(error);
                return 0;
            }
        };
        owner.translated += native.code.used().saturating_sub(before);
        native.code.sync();
        return entry;
    }
    // A first visit runs in the owner; the point links on the next one.
    let entry = native.code.tier1(eip);
    if entry.is_none()
        && native.code.direct_points.contains_key(&point)
        && native
            .code
            .resident(eip)
            .and_then(store::ResidentSite::entry)
            .is_some()
    {
        native.patch(point, eip, native.code.resident_fallback(eip).unwrap());
        native.code.sync();
        return point;
    }
    if entry.is_none() && (!Fetcher::fetchable(memory, eip) || !native.ran(eip)) {
        return 0;
    }
    let before = native.code.used();
    let observed = if entry.is_none() {
        Key::read(unsafe { &*(*runtime).cpu }, facts::ALL)
    } else {
        Key::default()
    };
    let unit = match native.link(memory, point, eip, entry, observed) {
        Ok(unit) => unit,
        Err(error) => {
            owner.error = Some(error);
            return 0;
        }
    };
    native.resolve_tables(eip, native.code.tier1(eip).unwrap().unit);
    if point == 0 {
        native.code.publish_indirect(eip, unit);
    }
    owner.translated += native.code.used().saturating_sub(before);
    native.code.sync();
    unit
}

impl Native {
    pub(crate) fn set_computation_offer(&mut self, enabled: bool, skip: Option<u32>) {
        let changed = self.offload.enabled != enabled;
        self.offload.enabled = enabled;
        let before = std::mem::replace(&mut self.plans.offer_resume, skip);
        if changed {
            self.publish_scopes();
        } else if before != skip {
            for head in [before, skip].into_iter().flatten() {
                self.publish_scope(head);
            }
        }
    }

    fn publish_scope(&mut self, eip: u32) {
        if self.scoped_jobs.is_empty() {
            return;
        }
        let enabled = self.offload.enabled && self.plans.offer_resume != Some(eip);
        for index in 0..self
            .code
            .resident(eip)
            .map_or(0, |site| site.versions.len()) as u8
        {
            if let Some(job) = self.scoped_jobs.get(&(eip, index)) {
                self.code
                    .publish_split(eip, index, job.policy.scope_threshold(enabled));
            }
        }
        self.code.sync();
    }

    fn publish_scopes(&mut self) {
        for (&(eip, index), job) in &self.scoped_jobs {
            let enabled = self.offload.enabled && self.plans.offer_resume != Some(eip);
            self.code
                .publish_split(eip, index, job.policy.scope_threshold(enabled));
        }
        self.code.sync();
    }

    fn remove_scoped_job(&mut self, eip: u32, index: u8) {
        if self.scoped_jobs.remove(&(eip, index)).is_some() {
            self.offload.admission.invalidate((eip, Some(index)));
        }
    }

    fn remove_scoped_jobs(&mut self, eip: u32) {
        for index in 0..self
            .code
            .resident(eip)
            .map_or(0, |site| site.versions.len()) as u8
        {
            self.remove_scoped_job(eip, index);
        }
    }

    pub(crate) fn set_async_compilation(&mut self, enabled: bool) {
        self.compiler.set_inline(!enabled);
    }

    pub(crate) fn progressed(&mut self) {
        if let Some(head) = self.plans.offer_resume.take() {
            self.publish_scope(head);
        }
    }

    pub(crate) fn take_offer_skip(&mut self) -> Option<u32> {
        let skip = self.plans.offer_resume.take();
        if let Some(head) = skip {
            self.publish_scope(head);
        }
        skip
    }

    /// Whether the computation reached at EIP goes to the job layer
    /// because a peer runs beside it.
    pub(super) fn offer_computation(&self) -> bool {
        self.offload.enabled
            && (self.bridge.is_null()
                || self.published_gate().stale.load(std::sync::atomic::Ordering::Relaxed) != 0
                    // SAFETY: this query occurs only within the owning run.
                    && unsafe { &mut *self.bridge.cast::<Bridge<'_>>() }
                        .handler.as_mut().is_some_and(|handler| handler.allow_computation()))
    }

    /// A single context may offer a loop whose blocks split when the
    /// frame's pace needs it to save time on a loop of serial `cost` ns.
    pub(super) fn offer_split(&self, cost: u64) -> bool {
        if !self.bridge.is_null() {
            // SAFETY: this query occurs only within the owning run.
            let bridge = unsafe { &*self.bridge.cast::<Bridge<'_>>() };
            self.pace
                .set(bridge.handler.as_ref().and_then(|handler| handler.pace()));
        }
        self.offload.enabled && self.pace.get().is_some_and(|pace| pace.need(cost) > 0)
    }

    /// An offer event is separate from ordinary owner dispatch.
    pub(super) fn offer_job(&mut self, cpu: &CpuState) -> bool {
        let job = self.plans.job(cpu.eip).unwrap();
        let cost = job.cost();
        let split = job.splits && cost > 0;
        let peer = self.offer_computation();
        let paced = split && self.offer_split(cost);
        if !peer && !paced {
            job.defer_offer();
            return false;
        }
        if job.wants_form()
            && self
                .plans
                .resident(cpu.eip)
                .is_none_or(|head| head.pending.is_none())
        {
            self.request_optimized(cpu.eip, cpu.clone());
        }
        self.scoped_offer = None;
        true
    }

    fn place_job(
        &mut self,
        eip: u32,
        at: &CpuState,
        unit: Arc<Unit>,
        job: compile::Job,
        version: Option<u8>,
    ) {
        let unit = job.scoped.unwrap_or(unit);
        if let Some(code) = self.emit_computation(
            &unit,
            eip,
            (at.x87.control, at.x87.status & 0x20 != 0),
            &job.proof,
        ) {
            let splits = job.proof.splits();
            self.diagnostics.split_proofs += u64::from(splits);
            let iterates = unit.trace.returns_at_head();
            let entry_registers = job.proof.entry_registers();
            let mut worker = WorkerJob {
                code: Code {
                    placed: code,
                    optimized: None,
                    scoped_mmx: job.proof.scoped_mmx(),
                    scoped_gpr: job.proof.scoped_gpr(),
                    frame: job
                        .proof
                        .scope_frame()
                        .map_or_else(|| Arc::from([]), |frame| Arc::from(frame.cells.as_slice())),
                },
                proof: job.proof,
                retired: unit.trace.prepaid(0),
                unit,
                entry_registers,
            };
            if let Some(form) = job.form {
                worker.install(&mut self.code, eip, form.built, form.lowered);
            }
            if let Some(version) = version {
                let mut policy = JobPlan::new(iterates, splits);
                policy.scoped = true;
                self.scoped_jobs
                    .insert((eip, version), ScopedJob { worker, policy });
            } else {
                self.worker_jobs.insert(eip, worker);
                let policy = self.plans.add_job(eip, iterates, splits);
                policy.requested();
                policy.built();
            }
        }
    }

    fn finish_job(&mut self, eip: u32, code: usize, job: Option<compile::Job>) -> bool {
        let Some(_) = self
            .worker_jobs
            .get(&eip)
            .filter(|prepared| prepared.code.placed.entry == code)
        else {
            return false;
        };
        self.plans.job(eip).unwrap().built();
        if let Some(form) = job.and_then(|job| job.form) {
            self.install_job(eip, form);
        }
        true
    }

    fn emit_computation(
        &mut self,
        unit: &Unit,
        eip: u32,
        (control, pe): (u16, bool),
        proof: &Proof,
    ) -> Option<WorkerCode> {
        let retired = unit.trace.prepaid(0);
        let code = compile::lower_computation(
            unit,
            &mut self.compiler.computation,
            eip,
            self.features,
            self.code.next(),
            self.code.stubs,
            (control, pe, Some(proof)),
        )?;
        let placed = self.code.place_worker(code.words, code.sites)?;
        crate::jit_map::record(placed.entry, code.words, eip, retired, code.marks, "a64");
        Some(placed)
    }

    fn request_optimized(&mut self, eip: u32, mut at: CpuState) {
        at.refresh_mmx();
        let prepared = &self.worker_jobs[&eip];
        self.plans.job(eip).unwrap().requested();
        if let Some(head) = self.plans.resident_mut(eip) {
            head.pending = Some(compile::Task::Job);
            if !head.probation.is_empty() {
                self.code.publish_resident(eip, None);
            }
        }
        self.compiler.request(compile::Request {
            eip,
            generation: self.code.generation,
            code: prepared.code.placed.entry,
            task: compile::Task::Job,
            unit: Arc::clone(&prepared.unit),
            proof: Some(prepared.proof.clone()),
            at,
            checked: FxHashSet::default(),
            all_checked: false,
            refused: FxHashSet::default(),
            cuts: FxHashSet::default(),
            count_entries: false,
        });
        self.install_optimized(false);
    }

    fn install_job(&mut self, eip: u32, form: compile::JobForm) {
        self.worker_jobs.get_mut(&eip).unwrap().install(
            &mut self.code,
            eip,
            form.built,
            form.lowered,
        );
    }
}
