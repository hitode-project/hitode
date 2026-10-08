//! Execution layer: shared-state x86-32 interpreter, hot-region JIT, and the guest CPU
//! interface ([`backend`]) the machine layer drives. See the README for scope.
mod architecture;
mod atomic_memory;
pub mod backend;
mod cliffs;
mod compile;
mod compile_budget;
mod compiler;
mod cpu_profile;
#[cfg(all(
    any(
        target_os = "linux",
        target_os = "android",
        all(target_os = "macos", target_arch = "aarch64")
    ),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod direct_memory;
mod discovery;
pub mod execution_abi;
mod executor;
#[path = "host_fault/recipe.rs"]
#[allow(dead_code)]
mod fault_recipe;
#[cfg(all(
    any(
        target_os = "linux",
        target_os = "android",
        all(target_os = "macos", target_arch = "aarch64")
    ),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub mod fault_recovery;
mod file_memory;
mod floating;
// Independent semantics and diagnostics; no GIR code in default library builds.
#[cfg(any(test, feature = "gir-lift"))]
mod gir;
#[cfg(feature = "gir-capture")]
#[path = "gir/capture.rs"]
mod gir_capture;
// The opt-in fault lane accepts only verified scalar recovery sites. Other
// accesses and hosts retain the current checked execution lane.
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
#[allow(dead_code)]
mod host_fault;
pub mod interpreter;
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
mod jit_memory;
#[cfg(all(target_os = "android", target_arch = "aarch64"))]
mod jit_memory_android;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod jit_memory_macos;
mod linking;
mod memory;
mod parallel;
mod parallel_memory;
mod perf_map;
mod rows;
mod shared_memory;
mod state;
mod strings;
mod tier1;
mod timestamp;
mod trap;
mod virtual_memory;

pub use architecture::{ArchitecturalState, StateError};
pub use backend::jit::JitCpu;
pub use cliffs::{Cliffs, Count as CliffCount, InterpreterReason, X87Cliff};
pub use compile::coverage::report as loop_coverage;
pub use compile_budget::RESPONSIVE_WORK as RESPONSIVE_COMPILE_WORK;
pub use cpu_profile::SegmentState;
pub use executor::{
    DirectMemoryStats, ExecutionContext, Executor, ProfileSample, RunOutcome, Stats, VerifyStats,
};
pub use memory::{EXEC, Memory, PAGE_SIZE, READ, WRITE};
pub use parallel::ParallelStats;
pub use rows::RowStats;
pub use state::{IndirectBranch, IndirectBranchKind, X87Register, X87State};
pub use tier1::census::{Coverage as Tier1Coverage, Tier1Census};
pub use tier1::{Tier1Mode, Tier1Stats};
pub use timestamp::TSC_FREQ;
pub use trap::ExecutionTrap;
#[cfg(unix)]
pub use virtual_memory::{
    DirectFaultView, FaultClassification, GuestAccess, GuestPageProtection,
    Reservation as DirectMemoryReservation,
};

/// Register order follows the x86 encoding: eax, ecx, edx, ebx, esp, ebp, esi, edi.
#[repr(C)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuState {
    pub regs: [u32; 8],
    pub eip: u32,
    pub eflags: u32,
    pub fs_base: u32,
    pub fault_addr: u32,
    /// Work budget: native basic block, interpreted instruction, or REP element.
    pub fuel: u64,
    /// Raw XMM bits shared by native and interpreted execution.
    pub xmm: [[u8; 16]; 8],
    /// Native MMX cache of physical x87 significands. Pending writes are described
    /// by mmx_mode and materialized into x87 at executor/context boundaries.
    pub mmx: [u64; 8],
    /// Deferred MMX entry (bit 0), EMMS (bit 1), tracked writes (bit 2),
    /// and written physical slots (bits 8–15).
    pub mmx_mode: u32,
    /// Canonical x87 state. Registers are physical R0–R7, independent of TOP.
    pub x87: X87State,
    /// Guest SSE control/status, including rounding, DAZ and FTZ.
    pub mxcsr: u32,
    pub segments: SegmentState,
    /// Shared REP continuation; not a guest-visible architectural register.
    #[doc(hidden)]
    pub rep: Option<interpreter::RepContinuation>,
    /// Last committed indirect call, jump or return, preserved across CPU contexts.
    pub last_indirect_branch: IndirectBranch,
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            regs: [0; 8],
            eip: 0,
            eflags: 2,
            fs_base: 0,
            fault_addr: 0,
            fuel: 0,
            xmm: [[0; 16]; 8],
            mmx: [0; 8],
            mmx_mode: 0,
            x87: X87State::default(),
            mxcsr: 0x1f80,
            segments: SegmentState::default(),
            rep: None,
            last_indirect_branch: IndirectBranch::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Exit {
    Dispatch = 0,
    Fallback = 1,
    Budget = 2,
    ReadFault = 3,
    WriteFault = 4,
    Trap = 5,
    Interrupted = 6,
}

/// Optimization policy for a native guest region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompileStrategy {
    /// Optimize generated code for sustained execution speed.
    #[default]
    Speed,
    /// Skip expensive optimization passes to reduce cold compilation time.
    Fast,
}

/// A bounded region of x86 instructions. Extra entries can come from a profile or
/// a discovered jump table; they must name instruction boundaries.
#[derive(Clone, Debug)]
pub struct Region {
    pub start: u32,
    pub len: usize,
    pub entries: Vec<u32>,
    /// Transfers to these guest PCs leave this compilation unit. Automatic
    /// discovery uses existing entries and bounded CFG edges as link frontiers.
    pub frontier: Vec<u32>,
    /// Decode control flow from entries instead of scanning embedded data and padding.
    pub reachable: bool,
    /// Complete a final instruction whose start is inside this region. Automatic
    /// regions enable this; explicit regions retain their strict byte boundary.
    pub complete_tail: bool,
    pub compile_strategy: CompileStrategy,
    /// Observed indirect call targets at or below this region. Their
    /// straight-line code is decoded into this unit so a call and its return
    /// dispatch internally. They are not entries of this unit.
    pub callees: Vec<u32>,
}

impl Region {
    pub fn new(start: u32, len: usize) -> Self {
        Self {
            start,
            len,
            entries: Vec::new(),
            frontier: Vec::new(),
            reachable: false,
            complete_tail: false,
            compile_strategy: CompileStrategy::Speed,
            callees: Vec::new(),
        }
    }
}
