//! AArch64 thin translator for the adv-cpu core.
//!
//! The facade selects this engine only under its `engine-a64` migration
//! comparison feature until M3. The design and milestones live in
//! `docs/a64-workbook.md`. Encoders are checked against
//! NDK clang vectors (`tools/cpu/a64_vectors.py`), and translated code runs
//! under Unicorn arm64 in host tests, so most work needs no device.

pub mod asm;
#[cfg(native)]
mod cache;
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
