//! Execution owner of the a64 engine.
//!
//! On AArch64 hosts every boundary goes through translated code, which in M0
//! exits to interpret one instruction; elsewhere the core interpreter runs
//! blocks directly. Exits, traps, fuel and deadlines follow the x64
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
/// shared; native and translation counters stay zero until M1 translates.
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
    memory: Memory,
    interpreter: Interpreter,
    timestamp: adv_cpu_core::timestamp::Counter,
    interrupt: Arc<AtomicBool>,
    host_interrupts: bool,
    deadline: Option<Instant>,
    #[cfg(native)]
    native: crate::native::Native,
}

impl Executor {
    pub fn new(base: u32, len: usize) -> Result<Self, String> {
        Self::create(Memory::new(base, len)?)
    }

    pub fn address_space() -> Result<Self, String> {
        Self::create(Memory::reserve(0, 1usize << 32)?)
    }

    fn create(memory: Memory) -> Result<Self, String> {
        Ok(Self {
            state: CpuState::default(),
            stats: Stats::default(),
            memory,
            interpreter: Interpreter::new(),
            timestamp: Default::default(),
            interrupt: Arc::new(AtomicBool::new(false)),
            host_interrupts: true,
            deadline: None,
            #[cfg(native)]
            native: crate::native::Native::new()?,
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

    /// Execute with a caller-owned raw trap handler. `_interpreter_only` is
    /// accepted for interface parity; M0 interprets everything.
    pub fn run_with_traps(
        &mut self,
        stop: u32,
        budget: u64,
        handler: &mut dyn ExecutionTrap,
        _interpreter_only: bool,
    ) -> Result<RunOutcome, String> {
        self.run_execution(stop, budget, Some(handler))
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
    fn run_native(&mut self) -> Result<Option<Exit>, String> {
        let (exit, translated) = self.native.run(&mut self.state)?;
        self.stats.native_entries += 1;
        if translated != 0 {
            self.stats.compilations += 1;
            self.stats.native_code_bytes += translated as u64;
        }
        Ok(Some(exit))
    }

    #[cfg(not(native))]
    fn run_native(&mut self) -> Result<Option<Exit>, String> {
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
        loop {
            if bridge.parked {
                return Ok(RunOutcome::Parked);
            }
            if self.state.eip == stop {
                return Ok(RunOutcome::ReachedStop);
            }
            if self.deadline.is_some_and(|at| Instant::now() >= at) {
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
            } else if let Some(exit) = self.run_native()? {
                match exit {
                    Exit::Dispatch => continue,
                    Exit::Interpret | Exit::Fault => {
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
