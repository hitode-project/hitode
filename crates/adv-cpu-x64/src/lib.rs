//! Cranelift hot-region engine: GIR, compilation, the executor, row kernels,
//! region linking and native code caches over the `adv-cpu-core` modules.
pub use adv_cpu_core::*;

pub mod backend;
mod code_cache;
mod compile;
mod compile_budget;
mod compiler;
mod discovery;
mod executor;
// Tier 2 front-end semantics; optional features add research diagnostics.
mod gir;
#[cfg(feature = "gir-capture")]
#[path = "gir/capture.rs"]
mod gir_capture;
// The opt-in fault lane accepts only verified scalar recovery sites. Other
// accesses and hosts retain the current checked execution lane.
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
#[allow(dead_code)]
mod host_fault;
#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
mod jit_memory;
#[cfg(all(target_os = "android", target_arch = "aarch64"))]
mod jit_memory_android;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod jit_memory_macos;
mod linking;
mod rows;

pub use backend::jit::JitCpu;
pub use compile::coverage::report as loop_coverage;
pub use compile_budget::RESPONSIVE_WORK as RESPONSIVE_COMPILE_WORK;
pub use executor::{AotStats, ExecutionContext, Executor, RunOutcome, Stats, VerifyStats};
pub use rows::RowStats;

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
