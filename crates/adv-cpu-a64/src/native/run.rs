//! Owner entry and recovery from translated code.
//!
//! Each Rust arrival reads one site plan. Decided links stay in translated
//! code, with explicit cold and offload events returning to the arbiter.

use adv_cpu_core::backend::OfferGate;
use adv_cpu_core::trap::Bridge;
use adv_cpu_core::{CpuState, Memory};

use super::plan::{self, dispatch, lookup_miss};
use super::{
    ACTIVE, Active, CLEAR_GATE, Checks, Faults, Native, Owner, Run, SLICE, STALE_GATE, access,
    alias_base, call, locked_access, timestamp, value, value_access,
};
use crate::facts::{self, Key};
use crate::runtime::{Event, Exit, PlanEvent, ResidentCold, Runtime};
use crate::translate;

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
        let (entry, translated) = match self.code.units.get(&cpu.eip).copied() {
            Some(entry) => (entry, 0),
            None if !memory.executable(cpu.eip, 1) => {
                return Ok(Some(Run {
                    exit: Exit::Event(Event::Fetch),
                    retired: 0,
                    translated: 0,
                }));
            }
            None if !crate::translate::front::decode::Fetcher::fetchable(memory, cpu.eip) => {
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
                let before = self.code.cache.used();
                let eip = cpu.eip;
                let entry = self.translate(memory, eip, Key::read(cpu, facts::ALL));
                (entry?, self.code.cache.used().saturating_sub(before))
            }
        };
        if fuel == 0 {
            return Ok(None);
        }
        let offer = match self.plans.arrival(cpu.eip) {
            plan::Arrival::Offerable(job) if self.plans.offer_resume != Some(cpu.eip) => {
                job.sample_offer() && job.offer_due(self.published_gate())
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
        self.code.cache.sync();
        self.code.faults.end.set(self.code.cache.next());
        self.prepare_inline(cpu, memory, fuel.max(budget), budget);
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
            let previous = ACTIVE.replace(Active::Owner(faults));
            let returned = enter.run(runtime, entry.unit);
            ACTIVE.set(previous);
            returned
        };
        self.runtime.cpu = std::ptr::null_mut();
        self.runtime.owner = 0;
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
        if let Exit::Plan(event) = exit {
            let payload = (returned.reason >> 32) as u32;
            match event {
                PlanEvent::BodyStack => self.refuse_body_stack(memory, cpu),
                PlanEvent::DiscoverBackEdge => {
                    let head = cpu.eip;
                    self.stop_counting(head);
                    self.discover(memory, head, head, cpu.clone());
                }
                PlanEvent::ContextRetry => self.retry_resident(cpu, payload as usize),
                PlanEvent::EnvironmentMiss => {
                    self.environment_miss(cpu.eip, (payload >> 8) as usize, payload as u8 as usize);
                }
                PlanEvent::OfferReady => exit = Exit::Computation,
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
        if exit == Exit::Computation && !self.offer_job(cpu) {
            self.plans.offer_resume = Some(cpu.eip);
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
        let head = self.plans.resident(eip).unwrap();
        let version = &head.versions[index];
        let record = &version.records[recovery.record as usize];
        translate::opt::recovery::restore_inline_entry(&recovery.entry, cpu);
        let (alias, entry_esp) = (alias_base(memory), cpu.regs[4]);
        self.runtime.shadow = recovery.dump.x[28];
        let refund = translate::opt::recovery::materialize(
            record,
            &recovery.dump,
            &self.inline_saved,
            &version.flags,
            cpu,
            // SAFETY: the resident entry admitted these frame bytes.
            |offset, bytes| unsafe {
                let at = alias.wrapping_add(u64::from(entry_esp.wrapping_add_signed(offset)));
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), at as *mut u8, bytes.len());
            },
        );
        if version.assumed.mmx || cpu.mmx_mode != 0 {
            cpu.refresh_mmx();
        }
        let fault = self.code.faults.site(self.code.faults.capture.pc.get());
        if let Some((site, reason)) = recovery.cold() {
            (refund, site, reason, false)
        } else if let Some(site) = fault {
            (refund, site.eip, ResidentCold::HostFault, site.checked)
        } else {
            (refund, record.eip, ResidentCold::Other, false)
        }
    }
}
