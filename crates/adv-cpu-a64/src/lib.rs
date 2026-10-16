//! AArch64 thin translator for the adv-cpu core.
//!
//! The facade selects this engine on every AArch64 host; the contracts live
//! in `docs/spec/a64.md`. Encoders are checked against
//! NDK clang vectors (`tools/cpu/a64_vectors.py`), and translated code runs
//! under Unicorn arm64 in host tests, so most work needs no device.

pub mod asm;
#[cfg(native)]
mod cache;
#[cfg(native)]
pub use cache::prepare_code_pool;
#[cfg(target_os = "ios")]
pub use cache::{prepare_code_pool_after_attach, prepare_code_pool_if_debugged};
mod cpu;
pub mod encode;
mod executor;
#[cfg(native)]
mod native;
pub mod runtime;
pub mod translate;

pub use adv_cpu_core::*;
pub use cpu::Cpu;
pub use executor::{ExecutionContext, Executor, Stats};

#[cfg(all(test, emulator))]
mod emulator;
