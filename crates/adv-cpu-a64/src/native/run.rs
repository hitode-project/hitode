//! Owner entry and recovery from translated code.
//!
//! Each Rust arrival reads one site plan. Decided links stay in translated
//! code, with explicit cold and offload events returning to the arbiter.

use adv_cpu_core::backend::OfferGate;
use adv_cpu_core::trap::Bridge;
use adv_cpu_core::{CpuState, Memory};

use super::plan::{self, dispatch};
use super::{
    ACTIVE, Active, CLEAR_GATE, Checks, Faults, Native, Owner, Run, SLICE, STALE_GATE, access,
    alias_base, call, interpret, timestamp, value, value_access,
};
use crate::facts::{self, Key};
use crate::runtime::{Event, Exit, PlanEvent, ResidentCold, Runtime};
use crate::translate;

impl Native {
    /// Runs the unit at `cpu.eip` if it completes at most `fuel` guest
    /// instructions; `None` leaves them to the interpreter. A fault returns
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
            None if !crate::translate::Fetcher::fetchable(memory, cpu.eip) => return Ok(None),
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
        let offer = match self.plans.arrival(cpu.eip) {
            plan::Arrival::Offerable(job) if self.plans.offer_resume != Some(cpu.eip) => {
                job.offer_due(self.published_gate())
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
        if u64::from(entry.retired) > fuel {
            self.diagnostics.over_fuel += 1;
            return Ok(None);
        }
        if entry.retired == 0 {
            self.diagnostics.empty += 1;
            // The first instruction is not translated: skip the round trip.
            return Ok(Some(Run {
                exit: Exit::Interpret,
                retired: 0,
                translated,
            }));
        }
        debug_assert_eq!(cpu.lazy_flags[3], 0, "lazy flags settle before entry");
        self.helper_units = 0;
        let budget = fuel.min(SLICE);
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
        self.prepare_inline(cpu, memory, fuel, budget);
        let runtime: *mut Runtime = &mut *self.runtime;
        let faults: *const Faults = &self.code.faults;
        let enter = self.code.enter;
        let mut owner = Owner {
            native: self,
            memory,
            bridge,
            translated,
            error: None,
            discovered: None,
            internal_discovered: Vec::new(),
        };
        // SAFETY: until `enter` returns, `self` is reached only through
        // `owner`, by `dispatch`. `unit` is translated code ending at the
        // stubs, and the runtime points at a live CPU for the call.
        let returned = unsafe {
            (*runtime).countdown = budget;
            (*runtime).cpu = cpu;
            (*runtime).helper = dispatch as *const () as u64;
            (*runtime).access = access as *const () as u64;
            (*runtime).fault_simd = (*faults).capture.simd.as_ptr() as u64;
            (*runtime).interpret = interpret as *const () as u64;
            (*runtime).value = value as *const () as u64;
            (*runtime).value_access = value_access as *const () as u64;
            (*runtime).call = call as *const () as u64;
            (*runtime).timestamp = timestamp as *const () as u64;
            (*runtime).owner = &raw mut owner as u64;
            let previous = ACTIVE.replace(Active::Owner(faults));
            let returned = enter(runtime, entry.unit);
            ACTIVE.set(previous);
            returned
        };
        self.runtime.cpu = std::ptr::null_mut();
        self.runtime.owner = 0;
        if let Some(error) = owner.error {
            return Err(error);
        }
        let translated = owner.translated;
        self.diagnostics.resident_entries += self.runtime.resident_entries;
        self.diagnostics.resident_page_crossings += self.runtime.resident_page_crossings;
        let mut exit = Exit::from_raw(returned.reason as u32)
            .ok_or_else(|| format!("unknown a64 exit {}", returned.reason))?;
        let budget_exit = exit == Exit::Budget;
        if budget_exit {
            exit = Exit::Dispatch;
        }
        match exit {
            Exit::Optimized => {}
            Exit::Interpret => self.diagnostics.interprets += 1,
            Exit::Event(Event::Replay | Event::CheckedAccess) => self.diagnostics.faults += 1,
            Exit::Event(_) | Exit::Plan(_) => {}
            Exit::Dispatch => self.diagnostics.dispatches += 1,
            Exit::Stepped | Exit::Reschedule | Exit::Computation | Exit::Recall | Exit::Budget => {}
            Exit::FirstVisit => unreachable!("first visits begin in the owner"),
            Exit::Yield => self.runtime.shadow = self.code.shadow.top(),
        }
        // Inline forms may refill the countdown, and a cold exit may leave
        // it negative.
        let mut retired = budget.wrapping_sub(returned.remaining);
        let resident_cold = if exit == Exit::Optimized {
            let (refund, site, reason, checked) = self.materialize_inline(cpu, memory);
            retired -= u64::from(refund);
            exit = Exit::Dispatch;
            Some((site, reason, checked, refund))
        } else {
            None
        };
        if exit == Exit::Event(Event::Replay) {
            let pc = self.code.faults.capture.pc.get();
            let site = self.code.faults.site(pc).expect("faulting site");
            self.code
                .faults
                .capture
                .recover(cpu, site, self.runtime.temps);
            retired -= u64::from(site.refund);
            if !site.checked {
                self.unchecked_faults += 1;
                self.strike(memory, site.eip, pc, cpu);
            } else {
                self.diagnostics.checked_faults += 1;
            }
        }
        if let Some(head) = owner.discovered {
            self.discover(memory, head, head, cpu.clone());
        }
        for (head, source, at) in owner.internal_discovered {
            if self.code.units.contains_key(&source) && !self.plans.resident(head).is_some() {
                self.discover(memory, head, source, at);
            }
        }
        self.resident_arrival(cpu);
        self.environment_miss();
        self.finish_probation();
        if let Some((site, reason, checked, refund)) = resident_cold {
            // A rebuild may replace versions named by this run's entry events.
            self.resident_cold_exit(
                self.runtime.opt_head,
                site,
                reason,
                checked,
                refund,
                returned.remaining.wrapping_neg(),
            );
        }
        if exit == Exit::Plan(PlanEvent::BodyStack) {
            self.refuse_body_stack(memory, cpu);
            exit = Exit::Dispatch;
        }
        if budget_exit {
            self.sample(memory, cpu);
        }
        self.install_optimized(false);
        if let Some(forms) = &mut self.retired_forms {
            // Native REP elements stay in translated fuel: telling them
            // apart would cost emitted code in every REP loop.
            forms[0] += retired - self.helper_units;
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

    fn published_gate(&self) -> &OfferGate {
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
        runtime.resident_point = 0;
        runtime.resident_env = 0;
        runtime.resident_probation = 0;
        runtime.resident_site = u32::MAX;
        runtime.resident_cold = ResidentCold::Other;
        runtime.resident_entries = 0;
        runtime.resident_page_crossings = 0;
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
        runtime.opt_recalled = 0;
        runtime.opt_save = self.inline_saved.as_mut_ptr() as u64;
    }

    /// Rebuilds the guest state at a cold inline exit from the entry
    /// baseline and the exit's record. Returns the record's refund.
    pub(super) fn materialize_inline(
        &mut self,
        cpu: &mut CpuState,
        memory: &Memory,
    ) -> (u32, u32, ResidentCold, bool) {
        let eip = self.runtime.opt_head;
        let index = self.runtime.resident_version as usize;
        let head = self.plans.resident(eip).unwrap();
        let version = &head.versions[index];
        let record = &version.records[self.runtime.opt_record as usize];
        translate::opt::recovery::restore_inline_entry(&self.runtime.opt_entry, cpu);
        let (alias, entry_esp) = (alias_base(memory), cpu.regs[4]);
        self.runtime.shadow = self.runtime.opt_dump.x[28];
        let refund = translate::opt::recovery::materialize(
            record,
            &self.runtime.opt_dump,
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
        if self.runtime.resident_cold != ResidentCold::Other {
            (
                refund,
                self.runtime.resident_site,
                self.runtime.resident_cold,
                false,
            )
        } else if let Some(site) = fault {
            (refund, site.eip, ResidentCold::HostFault, site.checked)
        } else {
            (refund, record.eip, ResidentCold::Other, false)
        }
    }
}
