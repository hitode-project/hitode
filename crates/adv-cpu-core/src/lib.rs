//! Engine-neutral x86-32 execution core: architectural state, guest memory,
//! the interpreter, exact floating point and the guest CPU interface
//! ([`backend`]) the machine layer drives. Engines build on these modules;
//! the hidden ones are engine-facing, not part of the host contract.
#[doc(hidden)]
pub mod architecture;
#[doc(hidden)]
pub mod atomic_memory;
pub mod backend;
#[doc(hidden)]
pub mod cliffs;
mod code_image;
pub mod code_pool;
#[doc(hidden)]
pub mod cpu_profile;
pub mod dual_view;
pub mod engine;
pub mod execution_abi;
#[doc(hidden)]
#[allow(dead_code)]
pub mod fault_recipe;
#[cfg(all(
    any(
        target_os = "linux",
        target_os = "android",
        all(any(target_os = "macos", target_os = "ios"), target_arch = "aarch64")
    ),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub mod fault_recovery;
#[doc(hidden)]
pub mod file_memory;
#[doc(hidden)]
pub mod floating;
pub mod frame_counters;
pub mod host_features;
pub mod instruction_trace;
pub mod interpreter;
#[doc(hidden)]
pub mod machine_memory;
#[doc(hidden)]
pub mod memory;
#[doc(hidden)]
pub mod perf_map;
#[doc(hidden)]
pub mod shared_memory;
#[doc(hidden)]
pub mod state;
#[doc(hidden)]
pub mod strings;
#[doc(hidden)]
pub mod tier_verify;
pub mod timestamp;
#[doc(hidden)]
pub mod trap;
#[doc(hidden)]
pub mod virtual_memory;

pub use architecture::{ArchitecturalState, StateError};
pub use cliffs::{Cliffs, Count as CliffCount, InterpreterReason, X87Cliff};
pub use code_image::AotImage;
pub use cpu_profile::{PROCESSOR_IDENTITY, ProcessorIdentity, SegmentState};
pub use engine::{Engine, EngineCounters, EngineOptions, ProfileSample};
pub use host_features::HostFeatures;
pub use memory::{EXEC, Memory, PAGE_SIZE, READ, WRITE};
pub use state::{IndirectBranch, IndirectBranchKind, X87Register, X87State};
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
    /// Arithmetic flags native code left lazy: operands a and b, result and
    /// recipe kind. Kind 0 means `eflags` is exact. Rust settles before use.
    pub lazy_flags: [u32; 4],
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
            lazy_flags: [0; 4],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
#[doc(hidden)]
pub enum Exit {
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

/// How a bounded execution run ended.
#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    ReachedStop,
    Budget,
    Interrupted,
    ReadFault { pc: u32, address: u32 },
    WriteFault { pc: u32, address: u32 },
    FetchFault { pc: u32, address: u32 },
    InvalidInstruction { pc: u32 },
    UnsupportedInstruction { pc: u32, code: iced_x86::Code },
    Trap { pc: u32, vector: u8 },
    Parked,
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
