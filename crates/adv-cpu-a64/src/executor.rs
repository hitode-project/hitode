//! Execution owner of the a64 engine.
//!
//! On AArch64 hosts entries run translated code from their second visit
//! (A64-TR-2); elsewhere, and in interpreter-only executors, the
//! core interpreter runs blocks directly. Exits, traps, fuel and deadlines
//! follow the x64 executor's interpreter path exactly, so both engines
//! pass the same semantic tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use adv_cpu_core::instruction_trace::Trace;
use adv_cpu_core::interpreter::{Interpreter, Step};
use adv_cpu_core::tier_verify::VerifyStats;
#[cfg(native)]
use adv_cpu_core::tier_verify::{self, Check, Verifier};
use adv_cpu_core::trap::{Bridge, ExecutionTrap, call_trap};
use adv_cpu_core::{CpuState, Memory, Region, RunOutcome};

use crate::runtime::Exit;

/// Interpreted instructions per decoded block, as in the x64 executor.
const BLOCK_UNITS: u64 = 256;

/// A saved guest CPU, restored into the same executor.
#[derive(Clone)]
pub struct ExecutionContext {
    cpu: CpuState,
}

impl ExecutionContext {
    pub fn cpu(&self) -> &CpuState {
        &self.cpu
    }

    pub fn cpu_mut(&mut self) -> &mut CpuState {
        &mut self.cpu
    }
}

/// Execution counters. Names match the x64 executor where the meaning is
/// shared; native and translation counters stay zero without translated code.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub interpreter_entries: u64,
    pub interpreter_instructions: u64,
    pub native_entries: u64,
    pub native_blocks: u64,
    pub compilations: u64,
    pub native_regions: u64,
    pub native_code_bytes: u64,
    pub invalidations: u64,
    pub code_revalidations: u64,
    pub trap_calls: u64,
    pub bulk_calls: u64,
    pub bulk_bytes: u64,
    pub cliffs: Box<adv_cpu_core::Cliffs>,
    pub verification: VerifyStats,
}

pub struct Executor {
    pub(crate) images: std::rc::Rc<std::cell::RefCell<Vec<(u32, u32)>>>,
    pub state: CpuState,
    pub stats: Stats,
    pub(crate) memory: Memory,
    /// Whether every instruction runs in the interpreter.
    pub(crate) interpreter_only: bool,
    /// Instructions translated code left to the interpreter, by EIP.
    pub(crate) histogram: rustc_hash::FxHashMap<u32, u64>,
    /// Faults of translated code, by EIP.
    pub(crate) faults: rustc_hash::FxHashMap<u32, u64>,
    interpreter: Interpreter,
    timestamp: adv_cpu_core::timestamp::Counter,
    interrupt: Arc<AtomicU32>,
    host_interrupts: bool,
    diagnostics: u8,
    alarm: Option<adv_sync::pending::Deadline>,
    deadline: Option<Instant>,
    /// `ADV_INTERP_TRACE_*` observations of an interpreter-only executor.
    pub(crate) instruction_trace: Option<Box<Trace>>,
    /// `ADV_JIT_VERIFY` checks, and how often each entry has been checked.
    #[cfg(native)]
    verifier: Option<Box<Verifier>>,
    #[cfg(native)]
    verified: rustc_hash::FxHashMap<u32, u8>,
    /// Translated code; `None` when every instruction is interpreted, so
    /// such an executor maps no code cache and installs no fault handler.
    #[cfg(native)]
    native: Option<crate::native::Native>,
    #[cfg(native)]
    native_warning: Option<String>,
}

impl Executor {
    pub fn new(base: u32, len: usize) -> Result<Self, String> {
        #[cfg(native)]
        let memory = {
            let mut memory = Memory::window(base, len)?;
            memory.map(base, len, 0)?;
            memory
        };
        #[cfg(not(native))]
        let memory = Memory::new(base, len)?;
        Self::create(memory, true)
    }

    pub fn address_space() -> Result<Self, String> {
        Self::create(Self::space()?, true)
    }

    /// A 4 GiB address space whose every instruction is interpreted.
    pub(crate) fn interpreted_address_space() -> Result<Self, String> {
        Self::create(Self::space()?, false)
    }

    fn space() -> Result<Memory, String> {
        // A compact window holds the 2 GiB user space of 32-bit Windows.
        #[cfg(native)]
        return Memory::window(0, adv_cpu_core::virtual_memory::GuestWindow::guest_span());
        #[cfg(not(native))]
        Memory::reserve(0, 1usize << 32)
    }

    fn create(memory: Memory, translate: bool) -> Result<Self, String> {
        #[cfg(native)]
        let (native, native_warning) = match translate {
            true => match crate::native::Native::new(
                memory
                    .window_base()
                    .ok_or("a64 guest memory must live in a guest window")?,
            ) {
                Ok(native) => (Some(native), None),
                Err(error) => {
                    let warning = format!("A64 JIT unavailable; using Tier 0: {error}");
                    log::warn!("{warning}");
                    (None, Some(warning))
                }
            },
            false => (None, None),
        };
        let images = std::rc::Rc::default();
        #[cfg(native)]
        let images = native
            .as_ref()
            .map_or(images, |n| std::rc::Rc::clone(&n.images));
        Ok(Self {
            images,
            state: CpuState::default(),
            stats: Stats::default(),
            memory,
            interpreter_only: !translate,
            histogram: Default::default(),
            faults: Default::default(),
            interpreter: Interpreter::new(),
            timestamp: Default::default(),
            interrupt: Arc::new(AtomicU32::new(0)),
            host_interrupts: true,
            diagnostics: 0,
            alarm: None,
            deadline: None,
            instruction_trace: (!translate).then(Trace::from_env).flatten(),
            #[cfg(native)]
            verifier: translate.then(Verifier::from_env).flatten(),
            #[cfg(native)]
            verified: Default::default(),
            #[cfg(native)]
            native,
            #[cfg(native)]
            native_warning,
        })
    }

    /// Runs every instruction in the interpreter, without a trap handler.
    #[cfg(test)]
    pub(crate) fn run_interpreted(&mut self, stop: u32, budget: u64) -> Result<RunOutcome, String> {
        let saved = std::mem::replace(&mut self.interpreter_only, true);
        let result = self.run_execution(stop, budget, None);
        self.interpreter_only = saved;
        result
    }

    pub(crate) fn count_tier0(&mut self, enabled: bool) {
        self.interpreter.frame_retired = enabled.then_some(0);
        self.interpreter.frame_rep_elements = 0;
        #[cfg(native)]
        if let Some(native) = &mut self.native {
            native.count_forms(enabled);
        }
    }

    pub(crate) fn tier0_instructions(&self) -> Option<u64> {
        self.interpreter.frame_retired
    }

    pub(crate) fn retired_forms(&self) -> Option<[u64; 5]> {
        let retired = self.interpreter.frame_retired?;
        #[allow(unused_mut)]
        let mut forms = [0; 5];
        #[cfg(native)]
        if let Some(native) = &self.native {
            forms = native.retired_forms.unwrap_or_default();
        }
        forms[3] += retired;
        forms[4] += self.interpreter.frame_rep_elements;
        Some(forms)
    }

    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    pub fn memory_mut(&mut self) -> &mut Memory {
        &mut self.memory
    }

    pub fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), String> {
        self.memory.write(addr, bytes)
    }

    pub fn protect(&mut self, addr: u32, len: usize, perms: u8) -> Result<(), String> {
        self.memory.protect(addr, len, perms)
    }

    pub fn map(&mut self, addr: u32, len: usize, perms: u8) -> Result<(), String> {
        self.memory.map(addr, len, perms)
    }

    pub fn unmap(&mut self, addr: u32, len: usize) -> Result<(), String> {
        self.memory.unmap(addr, len)
    }

    /// A translation hint. Units are formed on demand, so nothing is required
    /// ahead of execution.
    pub fn add_region(&mut self, _region: Region) -> Result<(), String> {
        Ok(())
    }

    /// Replaces all translation hints with one region; see `add_region`.
    pub fn set_region(&mut self, region: Region) -> Result<(), String> {
        self.add_region(region)
    }

    pub fn save_context(&self) -> Result<ExecutionContext, String> {
        let mut cpu = self.state.clone();
        cpu.refresh_mmx();
        Ok(ExecutionContext { cpu })
    }

    pub fn restore_context(&mut self, context: &ExecutionContext) -> Result<(), String> {
        self.state = context.cpu.clone();
        self.state.refresh_mmx();
        Ok(())
    }

    pub fn set_deadline(&mut self, deadline: Option<Instant>) {
        self.deadline = deadline;
        if deadline.is_some() && self.alarm.is_none() {
            self.alarm = Some(adv_sync::pending::Deadline::new(self.interrupt.clone()));
        }
        if let Some(alarm) = &self.alarm {
            alarm.set(deadline, &self.interrupt);
        }
    }

    pub fn set_host_interrupt_flag(&mut self, interrupt: Arc<AtomicU32>) {
        self.alarm = None;
        self.interrupt = interrupt;
        self.set_deadline(self.deadline);
    }

    pub fn host_interrupt_pending(&self) -> bool {
        self.host_interrupts
            && self.interrupt.load(Ordering::Acquire) & adv_sync::pending::INTERRUPT != 0
    }

    pub fn set_host_interrupts(&mut self, enabled: bool) {
        self.host_interrupts = enabled;
    }

    pub fn run(&mut self, stop: u32, budget: u64) -> Result<RunOutcome, String> {
        self.run_execution(stop, budget, None)
    }

    /// Execute with a caller-owned raw trap handler; `interpreter_only`
    /// keeps this run out of translated code.
    pub fn run_with_traps(
        &mut self,
        stop: u32,
        budget: u64,
        handler: &mut dyn ExecutionTrap,
        interpreter_only: bool,
    ) -> Result<RunOutcome, String> {
        let saved = self.interpreter_only;
        self.interpreter_only |= interpreter_only;
        let result = self.run_execution(stop, budget, Some(handler));
        self.interpreter_only = saved;
        result
    }

    /// Diagnostic counts of the translated runs, as `name: value` pairs.
    pub fn diagnostics(&self) -> String {
        #[cfg(native)]
        if let Some(native) = &self.native {
            return format!(
                "{:?} unchecked faults {} checked {}",
                native.diagnostics,
                native.unchecked_faults,
                native.checked_count()
            );
        }
        #[cfg(native)]
        if let Some(warning) = &self.native_warning {
            return warning.clone();
        }
        String::new()
    }

    /// Slow-path runs by EIP: interpreted after an exit, after a fault, and
    /// run by helpers. Counts only grow, so callers diff two readings.
    pub fn slow_path_sites(&self) -> Vec<(u32, [u64; 3])> {
        let mut sites = rustc_hash::FxHashMap::<u32, [u64; 3]>::default();
        #[cfg(native)]
        if let Some(native) = &self.native {
            for (eip, count) in native.helper_histogram() {
                sites.entry(eip).or_default()[2] += count;
            }
        }
        for (which, histogram) in [&self.histogram, &self.faults].into_iter().enumerate() {
            for (&eip, &count) in histogram {
                sites.entry(eip).or_default()[which] += count;
            }
        }
        sites.into_iter().collect()
    }

    /// The form of the instruction at `eip` as the code there decodes now.
    pub fn instruction_form(&self, eip: u32) -> Option<iced_x86::Code> {
        let mut bytes = [0; 15];
        let page = (0x1000 - (eip & 0xfff) as usize).min(bytes.len());
        if self.memory.read_into(eip, &mut bytes).is_err() {
            self.memory.read_into(eip, &mut bytes[..page]).ok()?;
        }
        let mut decoder =
            iced_x86::Decoder::with_ip(32, &bytes, u64::from(eip), iced_x86::DecoderOptions::NONE);
        Some(decoder.decode().code())
    }

    /// Instructions by form, most frequent first, as the code at their
    /// EIPs decodes now: those interpreted after an exit, after a fault, or
    /// (`which` 2) run by helpers.
    pub fn interpreted_forms(&self, which: u8) -> Vec<(iced_x86::Code, u64)> {
        let mut forms = rustc_hash::FxHashMap::<_, u64>::default();
        #[cfg(native)]
        let helpers = self
            .native
            .as_ref()
            .map(crate::native::Native::helper_histogram)
            .unwrap_or_default();
        #[cfg(not(native))]
        let helpers = rustc_hash::FxHashMap::default();
        let histogram = match which {
            0 => &self.histogram,
            1 => &self.faults,
            _ => &helpers,
        };
        for (&eip, &count) in histogram {
            let mut bytes = [0; 15];
            let page = (0x1000 - (eip & 0xfff) as usize).min(bytes.len());
            if self.memory.read_into(eip, &mut bytes).is_err()
                && self.memory.read_into(eip, &mut bytes[..page]).is_err()
            {
                continue;
            }
            let mut decoder = iced_x86::Decoder::with_ip(
                32,
                &bytes,
                u64::from(eip),
                iced_x86::DecoderOptions::NONE,
            );
            *forms.entry(decoder.decode().code()).or_default() += count;
        }
        let mut forms: Vec<_> = forms.into_iter().collect();
        forms.sort_unstable_by_key(|a| std::cmp::Reverse(a.1));
        forms
    }

    fn run_execution(
        &mut self,
        stop: u32,
        budget: u64,
        handler: Option<&mut dyn ExecutionTrap>,
    ) -> Result<RunOutcome, String> {
        let diagnostics = handler.as_ref().map_or(0, |handler| handler.diagnostics());
        if self.diagnostics != diagnostics {
            self.diagnostics = diagnostics;
            self.memory.count_host_access(diagnostics != 0);
            #[cfg(native)]
            if let Some(native) = &mut self.native {
                native.clear();
            }
        }
        // Another process of the family may have used the shared window.
        self.memory.attach()?;
        let mut bridge = Bridge {
            timestamp: std::mem::take(&mut self.timestamp),
            memory: &mut self.memory,
            error: None,
            trap: None,
            calls: 0,
            handler,
            parked: false,
            deadline: self.deadline,
            clock_due: true,
            unclocked_bytes: 0,
            bulk_calls: 0,
            bulk_bytes: 0,
            cliffs: std::mem::take(&mut self.stats.cliffs),
            scratch: Default::default(),
        };
        #[cfg(native)]
        if let Some(native) = &mut self.native {
            native.set_body_services(
                bridge
                    .handler
                    .as_ref()
                    .map_or_else(Default::default, |h| h.body_services()),
            );
            native.deadline = self.deadline.map(crate::native::deadline);
            native.interrupt = Some(self.interrupt.clone());
            native.pending_mask = if self.host_interrupts {
                u32::MAX
            } else {
                !adv_sync::pending::INTERRUPT
            };
            self.interrupt.fetch_and(
                !(adv_sync::pending::STOP | adv_sync::pending::WORK),
                Ordering::AcqRel,
            );
        }
        // Observations follow only interpreter-only runs, as in the x64 engine.
        let mut trace = self
            .interpreter_only
            .then(|| self.instruction_trace.take())
            .flatten();
        let result = self.run_inner(stop, budget, &mut bridge, trace.as_deref_mut());
        if let Some(trace) = &mut trace {
            trace.boundary(&result, &self.state, &self.memory);
        }
        if trace.is_some() {
            self.instruction_trace = trace;
        }
        // Export canonical floating state at every public CPU boundary.
        self.state.refresh_mmx();
        self.stats.trap_calls += bridge.calls;
        self.stats.bulk_calls += bridge.bulk_calls;
        self.stats.bulk_bytes += bridge.bulk_bytes;
        self.stats.cliffs = bridge.cliffs;
        self.timestamp = bridge.timestamp;
        result
    }

    /// Publishes the hardware fault address of an access starting at `start`.
    fn fault_address(&mut self, start: u32, permission: u8) -> u32 {
        let address = adv_cpu_core::interpreter::fault_address(&self.memory, start, permission);
        self.state.fault_addr = address;
        address
    }

    /// Runs translated code from EIP; `None` where the host has none.
    #[cfg(native)]
    fn run_native(&mut self, stop: u32, bridge: &mut Bridge<'_>) -> Result<Option<Exit>, String> {
        let Some(native) = self.native.as_mut().filter(|_| !self.interpreter_only) else {
            return Ok(None);
        };
        let fuel = self.state.fuel;
        native.clock = (bridge as *mut Bridge<'_>).cast();
        // Host calls run in place only with a handler to take them.
        let traps = if bridge.handler.is_some() {
            bridge.memory = &mut self.memory;
            (bridge as *mut Bridge<'_>).cast()
        } else {
            std::ptr::null_mut()
        };
        let entry = self.state.eip;
        let due = self.verifier.is_some()
            && self
                .verified
                .get(&entry)
                .is_none_or(|&n| n < tier_verify::ENTRIES);
        let run = if due {
            // The interpreter goes first and restores what it changed; the
            // unit then runs alone, with host calls left to the owner.
            let n = self.verified.entry(entry).or_default();
            *n += 1;
            let verifier = self.verifier.as_mut().unwrap();
            let counted = self.interpreter.frame_retired.take();
            let trace = verifier.record(
                entry,
                stop,
                &mut self.interpreter,
                &mut self.state,
                &mut self.memory,
            );
            self.interpreter.frame_retired = counted;
            let trace = trace?;
            let fuel = fuel.min(tier_verify::MAX_STEPS as u64);
            let run = native.run(
                &mut self.state,
                &mut self.memory,
                stop,
                fuel,
                std::ptr::null_mut(),
            )?;
            if let Some(run) = &run
                && run.exit != Exit::Stepped
            {
                let check = trace.check_retired(run.retired as usize, &self.state, &self.memory);
                let strict = verifier.strict;
                record_verification(&mut self.stats.verification, strict, check)?;
            }
            Ok(run)
        } else {
            native.run(&mut self.state, &mut self.memory, stop, fuel, traps)
        };
        if let Some(error) = bridge.error.take() {
            return Err(error);
        }
        let Some(run) = run? else {
            return Ok(None);
        };
        self.stats.native_entries += 1;
        if run.translated != 0 {
            self.stats.compilations += 1;
            self.stats.native_code_bytes += run.translated as u64;
        }
        self.stats.invalidations = native.diagnostics.dropped_units;
        self.state.fuel -= u64::from(run.retired);
        Ok(Some(run.exit))
    }

    /// Whether the wall deadline has passed. Translated runs compare the
    /// virtual counter every time; otherwise reading the clock costs as
    /// much as a short run, so it is polled every few iterations, and one
    /// call is capped by its fuel anyway.
    fn past_deadline(&self, polls: u32) -> bool {
        #[cfg(native)]
        if let Some(native) = &self.native
            && native.deadline.is_some()
        {
            return native.past();
        }
        polls.is_multiple_of(16) && self.deadline.is_some_and(|at| Instant::now() >= at)
    }

    #[cfg(not(native))]
    fn run_native(&mut self, _stop: u32, _bridge: &mut Bridge<'_>) -> Result<Option<Exit>, String> {
        Ok(None)
    }

    fn run_inner(
        &mut self,
        stop: u32,
        budget: u64,
        bridge: &mut Bridge<'_>,
        mut trace: Option<&mut Trace>,
    ) -> Result<RunOutcome, String> {
        self.interpreter.observe_transfers = trace.is_some();
        self.state.refresh_mmx();
        self.state.fuel = budget;
        let mut polls = 0u32;
        loop {
            if bridge.parked {
                return Ok(RunOutcome::Parked);
            }
            if self.state.eip == stop {
                return Ok(RunOutcome::ReachedStop);
            }
            polls = polls.wrapping_add(1);
            if self.past_deadline(polls) {
                return Ok(RunOutcome::Budget);
            }
            if self.state.fuel == 0 {
                return Ok(RunOutcome::Budget);
            }
            if self.host_interrupt_pending() {
                return Ok(RunOutcome::Interrupted);
            }
            if self
                .state
                .rep
                .as_ref()
                .is_some_and(|rep| rep.pc != self.state.eip)
            {
                self.state.rep = None;
            }
            // Interpreted runs reach the owner's target too (A64-CTL-14).
            if let Some(handler) = bridge.handler.as_ref()
                && let Some((target, original)) = handler
                    .required_redirect(self.state.eip)
                    .or_else(|| handler.redirect(self.state.eip))
                && self
                    .memory
                    .read_data(self.state.eip, original.len())
                    .is_ok_and(|bytes| bytes.as_ref() == original)
            {
                self.state.eip = target;
                continue;
            }
            let single_step = self.state.eflags & (1 << 8) != 0;
            // A restored TF or RF needs precise instruction boundaries.
            let precise_flags = self.state.eflags & ((1 << 8) | (1 << 16)) != 0;
            let units = self.state.fuel.min(BLOCK_UNITS) as u32;
            let (units, observed) = trace.as_deref_mut().map_or((units, None), |trace| {
                trace.prepare(units, &self.state, &self.memory)
            });
            let (execution, last_units) = if precise_flags || observed.is_some() {
                let execution = self
                    .interpreter
                    .run_chunk(&mut self.state, &mut self.memory, 1);
                (execution, execution.units)
            } else if let Some(exit) = self.run_native(stop, bridge)? {
                match exit {
                    Exit::Dispatch => continue,
                    // The helper ran the instruction; the owner completes it.
                    #[cfg(native)]
                    Exit::Stepped => {
                        let execution = self.native.as_mut().and_then(|n| n.pending.take());
                        let execution = execution.expect("a pending step");
                        (execution, execution.units)
                    }
                    #[cfg(not(native))]
                    Exit::Stepped => unreachable!("no translated code"),
                    // The unit spent the budget; the instruction waits.
                    Exit::Interpret | Exit::Fault if self.state.fuel == 0 => continue,
                    Exit::Interpret | Exit::Fault => {
                        let histogram = if exit == Exit::Interpret {
                            &mut self.histogram
                        } else {
                            &mut self.faults
                        };
                        *histogram.entry(self.state.eip).or_default() += 1;
                        let execution =
                            self.interpreter
                                .run_chunk(&mut self.state, &mut self.memory, 1);
                        (execution, execution.units)
                    }
                }
            } else {
                self.interpreter
                    .run_block(&mut self.state, &mut self.memory, units, stop, None)
            };
            self.stats.interpreter_entries += 1;
            self.stats.interpreter_instructions += u64::from(execution.units);
            bridge
                .cliffs
                .interpreted(adv_cpu_core::InterpreterReason::Forced, execution.units);
            self.state.fuel -= u64::from(execution.units);
            let step = match execution.step {
                Step::ReadTimestamp { pc, next } => {
                    let ticks = bridge.read_timestamp();
                    self.state.regs[0] = ticks as u32;
                    self.state.regs[2] = (ticks >> 32) as u32;
                    self.state.eip = next;
                    if single_step {
                        Step::Trap { pc, vector: 1 }
                    } else {
                        Step::Executed
                    }
                }
                Step::InvalidInstruction { pc } if bridge.handler.is_some() => {
                    Step::Trap { pc, vector: 6 }
                }
                other => other,
            };
            if let Some(observed) = observed {
                trace.as_deref_mut().unwrap().complete(
                    observed,
                    step,
                    execution.units,
                    &self.state,
                    &self.memory,
                );
            }
            match step {
                Step::Executed => {}
                Step::ReadTimestamp { .. } => unreachable!("completed by the execution owner"),
                Step::ReadFault { pc, address } => {
                    let address = self.fault_address(address, adv_cpu_core::READ);
                    return Ok(RunOutcome::ReadFault { pc, address });
                }
                Step::WriteFault { pc, address } => {
                    let address = self.fault_address(address, adv_cpu_core::WRITE);
                    return Ok(RunOutcome::WriteFault { pc, address });
                }
                Step::FetchFault { pc, address } => {
                    self.state.fault_addr = address;
                    return Ok(RunOutcome::FetchFault { pc, address });
                }
                Step::InvalidInstruction { pc } => {
                    return Ok(RunOutcome::InvalidInstruction { pc });
                }
                Step::Unsupported { pc, code } => {
                    return Ok(RunOutcome::UnsupportedInstruction { pc, code });
                }
                Step::Trap { pc, vector } => {
                    if bridge.handler.is_none() {
                        return Ok(RunOutcome::Trap { pc, vector });
                    }
                    if last_units == 0 {
                        // A handler repeatedly returning to an unchanged fault
                        // must still yield to the machine scheduler.
                        self.state.fuel = self.state.fuel.saturating_sub(1);
                    }
                    bridge.memory = &mut self.memory;
                    call_trap(bridge, &mut self.state, u32::from(vector));
                    if let Some(error) = bridge.error.take() {
                        return Err(error);
                    }
                }
            }
        }
    }
}

/// Accounts one verified exit; strict mode stops at a mismatch.
#[cfg(native)]
fn record_verification(
    stats: &mut VerifyStats,
    strict: bool,
    check: Option<Result<Check, String>>,
) -> Result<(), String> {
    match check {
        None => stats.inconclusive += 1,
        Some(Ok(Check::Equal)) => stats.verified += 1,
        Some(Ok(Check::MxcsrFlags)) => stats.mxcsr_flags += 1,
        Some(Err(report)) => {
            stats.mismatches += 1;
            if strict {
                return Err(format!("tier verification: {report}"));
            }
            if stats.reports.len() < tier_verify::REPORTS {
                stats.reports.push(report);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod differential;
#[cfg(test)]
mod instruction_trace_tests {
    // These vectors contain address ranges, not the elements of each range.
    #![allow(clippy::single_range_in_vec_init)]
    use super::Executor;

    fn executor(base: u32, len: usize) -> Executor {
        Executor::new(base, len).unwrap()
    }

    /// Reserved memory is no guest window, so the executor only interprets.
    fn reserved(memory: adv_cpu_core::Memory) -> Executor {
        Executor::create(memory, false).unwrap()
    }

    fn run_interpreted(cpu: &mut Executor, stop: u32, budget: u64) -> adv_cpu_core::RunOutcome {
        cpu.run_interpreted(stop, budget).unwrap()
    }

    include!("../../adv-cpu-core/src/instruction_trace/executor_tests.rs");
}
#[cfg(all(test, native))]
mod host_call_tests;

#[cfg(all(test, native))]
mod speed_tests {
    use super::*;
    use iced_x86::code_asm::*;

    // spec: WINE-UI-8, A64-TR-2, A64-TR-4
    #[test]
    fn speed_retired_forms_join_native_helpers_first_visits_and_rep() {
        const CODE: u32 = 0x10000;
        const FIRST: u32 = 0x11000;
        const DATA: u32 = 0x20000;
        let mut a = CodeAssembler::new(32).unwrap();
        let mut again = a.create_label();
        a.mov(ebp, 3).unwrap();
        a.set_label(&mut again).unwrap();
        a.shrd(eax, edx, cl).unwrap(); // native form
        a.cpuid().unwrap(); // interpret helper
        a.mov(ecx, 5).unwrap();
        a.rep().movsb().unwrap();
        a.dec(ebp).unwrap();
        a.jnz(again).unwrap();
        let code = a.assemble(u64::from(CODE)).unwrap();
        let stop = CODE + code.len() as u32;
        let mut a = CodeAssembler::new(32).unwrap();
        a.nop().unwrap();
        a.jmp(u64::from(CODE)).unwrap();
        let first = a.assemble(u64::from(FIRST)).unwrap();
        for interpreted in [false, true] {
            let mut cpu = Executor::address_space().unwrap();
            cpu.interpreter_only = interpreted;
            cpu.map(CODE, 0x4000, 7).unwrap();
            cpu.map(DATA, 0x4000, 3).unwrap();
            cpu.write(CODE, &code).unwrap();
            cpu.write(FIRST, &first).unwrap();
            let start = |cpu: &mut Executor, pc| {
                cpu.state = CpuState::default();
                cpu.state.eip = pc;
                cpu.state.regs[6] = DATA + 0x100;
                cpu.state.regs[7] = DATA + 0x200;
            };
            assert_eq!(cpu.retired_forms(), None);
            // Warm the loop's entries without visiting FIRST.
            for _ in 0..3 {
                start(&mut cpu, CODE);
                assert_eq!(cpu.run(stop, 1000).unwrap(), RunOutcome::ReachedStop);
            }
            cpu.count_tier0(true);
            start(&mut cpu, FIRST);
            assert_eq!(cpu.run(stop, 1000).unwrap(), RunOutcome::ReachedStop);
            let expected = if interpreted {
                [0, 0, 0, 21, 15]
            } else {
                // Native REP elements count as translated work.
                [28, 0, 3, 2, 0]
            };
            assert_eq!(
                cpu.retired_forms(),
                Some(expected),
                "interpreted={interpreted}"
            );
            // A zero-length REP still retires an instruction, with no elements.
            cpu.count_tier0(true);
            start(&mut cpu, FIRST);
            cpu.write(FIRST, &[0xf3, 0xa4]).unwrap();
            cpu.state.eip = FIRST;
            assert_eq!(cpu.run(FIRST + 2, 10).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(cpu.retired_forms().unwrap().iter().take(4).sum::<u64>(), 1);
            assert_eq!(cpu.retired_forms().unwrap()[4], 0);
        }
    }

    // spec: WINE-UI-8
    #[test]
    fn speed_retired_forms_count_partial_rep_and_single_steps() {
        let mut cpu = Executor::address_space().unwrap();
        cpu.map(0x10000, 0x4000, 7).unwrap();
        cpu.map(0x20000, 0x4000, 3).unwrap();
        cpu.write(0x10000, &[0xf3, 0xa4]).unwrap();
        cpu.native.as_mut().unwrap().first_visits = false;
        cpu.count_tier0(true);
        cpu.state.eip = 0x10000;
        cpu.state.regs[1] = 9;
        cpu.state.regs[6] = 0x20000;
        cpu.state.regs[7] = 0x20100;
        // Native elements count as translated; the cut element is interpreted.
        assert_eq!(cpu.run(0x10002, 4).unwrap(), RunOutcome::Budget);
        assert_eq!(cpu.retired_forms(), Some([3, 0, 0, 0, 1]));
        assert_eq!(cpu.run(0x10002, 20).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(cpu.retired_forms(), Some([8, 0, 0, 0, 1]));
        cpu.count_tier0(true);
        cpu.write(0x11000, &[0x90]).unwrap();
        cpu.state.eip = 0x11000;
        cpu.state.eflags |= 1 << 8;
        assert!(matches!(
            cpu.run(0x11001, 10).unwrap(),
            RunOutcome::Trap { vector: 1, .. }
        ));
        assert_eq!(cpu.retired_forms(), Some([0, 0, 0, 1, 0]));
        cpu.count_tier0(true);
        cpu.write(0x12000, &[0xcc]).unwrap(); // an exit interpreted by the owner
        cpu.state.eip = 0x12000;
        cpu.state.eflags &= !(1 << 8);
        assert!(matches!(
            cpu.run(0x12001, 10).unwrap(),
            RunOutcome::Trap { vector: 3, .. }
        ));
        assert_eq!(cpu.retired_forms(), Some([0, 0, 0, 1, 0]));
    }

    // spec: WINE-UI-8, A64-MEM-5
    #[test]
    fn speed_retired_forms_refund_faulting_instructions_and_rep() {
        let mut cpu = Executor::address_space().unwrap();
        cpu.map(0x10000, 0x4000, 7).unwrap();
        cpu.map(0x20000, 0x4000, 3).unwrap();
        cpu.native.as_mut().unwrap().first_visits = false;
        cpu.count_tier0(true);
        // Native NEG reads successfully, but its store faults.
        cpu.write(0x10000, &[0xf7, 0x1b]).unwrap();
        cpu.protect(0x20000, 0x4000, 1).unwrap();
        cpu.state.eip = 0x10000;
        cpu.state.regs[3] = 0x20000;
        assert!(matches!(
            cpu.run(0x10002, 100).unwrap(),
            RunOutcome::WriteFault { .. }
        ));
        assert_eq!(cpu.retired_forms(), Some([0; 5]));
        // REP retains completed elements when its next source faults.
        cpu.count_tier0(true);
        cpu.protect(0x20000, 0x4000, 3).unwrap();
        cpu.write(0x11000, &[0xf3, 0xa4]).unwrap();
        cpu.state.eip = 0x11000;
        cpu.state.regs[1] = 5;
        cpu.state.regs[6] = 0x23ffe;
        cpu.state.regs[7] = 0x20000;
        assert!(matches!(
            cpu.run(0x11002, 100).unwrap(),
            RunOutcome::ReadFault { .. }
        ));
        assert_eq!(cpu.retired_forms(), Some([2, 0, 0, 0, 0]));
    }
}
