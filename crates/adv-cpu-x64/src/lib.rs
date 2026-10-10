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
pub use executor::{AotStats, ExecutionContext, Executor, Stats, VerifyStats};
pub use rows::RowStats;
