//! x86-32 hot-region execution experiment. See the crate README for its scope.
mod compile;
mod host;
mod machine;
mod memory;
#[cfg(feature = "pal")]
pub mod pal;
mod strings;
mod virtual_memory;

pub use host::{ApiCall, ApiMemory, ApiOutcome, ApiSpec, Convention, HostServices, Resume, Wait};
pub use machine::{ExecutionContext, Machine, ProfileSample, RunOutcome, Stats};
pub use memory::{EXEC, Memory, PAGE_SIZE, READ, WRITE};

/// Register order follows the x86 encoding: eax, ecx, edx, ebx, esp, ebp, esi, edi.
#[repr(C)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuState {
    pub regs: [u32; 8],
    pub eip: u32,
    pub eflags: u32,
    pub fs_base: u32,
    pub fault_addr: u32,
    /// Execution budget: one unit per native basic block or fallback instruction.
    pub fuel: u64,
    /// Raw XMM bits, shared with Unicorn at execution boundaries.
    pub xmm: [[u8; 16]; 8],
    /// Physical x87 mantissa bits exposed by MMX, synchronized at execution boundaries.
    pub mmx: [u64; 8],
    /// Pending native MMX entry (bit 0) and EMMS (bit 1) for Unicorn's x87 state.
    pub mmx_mode: u32,
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
    Host = 5,
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
    /// Decode control flow from entries instead of scanning embedded data and padding.
    pub reachable: bool,
    /// Complete a final instruction whose start is inside this region. Automatic
    /// regions enable this; explicit regions retain their strict byte boundary.
    pub complete_tail: bool,
    pub compile_strategy: CompileStrategy,
}

impl Region {
    pub fn new(start: u32, len: usize) -> Self {
        Self {
            start,
            len,
            entries: Vec::new(),
            reachable: false,
            complete_tail: false,
            compile_strategy: CompileStrategy::Speed,
        }
    }
}
