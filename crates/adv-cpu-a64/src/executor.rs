//! Execution owner of the a64 engine.
//!
//! On AArch64 hosts entries run translated code from their second visit
//! (execution ABI §A7); elsewhere, and in interpreter-only executors, the
//! core interpreter runs blocks directly. Exits, traps, fuel and deadlines follow the x64
//! executor's interpreter path exactly, so both engines pass the same
//! semantic tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use adv_cpu_core::interpreter::{Interpreter, Step};
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
}

pub struct Executor {
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
    interrupt: Arc<AtomicBool>,
    host_interrupts: bool,
    deadline: Option<Instant>,
    /// Translated code; `None` when every instruction is interpreted, so
    /// such an executor maps no code cache and installs no fault handler.
    #[cfg(native)]
    native: Option<crate::native::Native>,
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
        #[cfg(native)]
        return Memory::window(0, 1usize << 32);
        #[cfg(not(native))]
        Memory::reserve(0, 1usize << 32)
    }

    fn create(memory: Memory, translate: bool) -> Result<Self, String> {
        #[cfg(native)]
        let native = match translate {
            true => Some(crate::native::Native::new(
                memory
                    .window_base()
                    .ok_or("a64 guest memory must live in a guest window")?,
            )?),
            false => None,
        };
        Ok(Self {
            state: CpuState::default(),
            stats: Stats::default(),
            memory,
            interpreter_only: !translate,
            histogram: Default::default(),
            faults: Default::default(),
            interpreter: Interpreter::new(),
            timestamp: Default::default(),
            interrupt: Arc::new(AtomicBool::new(false)),
            host_interrupts: true,
            deadline: None,
            #[cfg(native)]
            native,
        })
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
    }

    pub fn set_host_interrupt_flag(&mut self, interrupt: Arc<AtomicBool>) {
        self.interrupt = interrupt;
    }

    pub fn host_interrupt_pending(&self) -> bool {
        self.host_interrupts && self.interrupt.load(Ordering::Acquire)
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
        String::new()
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
        forms.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        forms
    }

    fn run_execution(
        &mut self,
        stop: u32,
        budget: u64,
        handler: Option<&mut dyn ExecutionTrap>,
    ) -> Result<RunOutcome, String> {
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
        let result = self.run_inner(stop, budget, &mut bridge);
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
        // Host calls run in place only with a handler to take them.
        let traps = if bridge.handler.is_some() {
            bridge.memory = &mut self.memory;
            (bridge as *mut Bridge<'_>).cast()
        } else {
            std::ptr::null_mut()
        };
        let run = native.run(&mut self.state, &mut self.memory, stop, fuel, traps);
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
        self.state.fuel -= u64::from(run.retired);
        Ok(Some(run.exit))
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
    ) -> Result<RunOutcome, String> {
        self.state.refresh_mmx();
        self.state.fuel = budget;
        // Reading the clock costs as much as a short native run, so the
        // deadline is polled every few iterations; one call is capped by
        // its fuel anyway.
        let mut polls = 0u32;
        loop {
            if bridge.parked {
                return Ok(RunOutcome::Parked);
            }
            if self.state.eip == stop {
                return Ok(RunOutcome::ReachedStop);
            }
            polls = polls.wrapping_add(1);
            if polls % 16 == 0 && self.deadline.is_some_and(|at| Instant::now() >= at) {
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
            let single_step = self.state.eflags & (1 << 8) != 0;
            // A restored TF or RF needs precise instruction boundaries.
            let precise_flags = self.state.eflags & ((1 << 8) | (1 << 16)) != 0;
            let units = self.state.fuel.min(BLOCK_UNITS) as u32;
            let (execution, last_units) = if precise_flags {
                let execution =
                    self.interpreter
                        .run_chunk(&mut self.state, &mut self.memory, units);
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

#[cfg(test)]
mod differential;
