//! Explicit ARM64 Tier 1 lane: bounded integer templates with exact Tier 0 exits.
//! Memory operations retain the existing checked Memory contract. No hardware
//! fault recovery, floating approximation or parallel guest execution is enabled.
#![cfg_attr(
    not(all(
        target_arch = "aarch64",
        any(target_os = "linux", target_os = "android")
    )),
    allow(dead_code)
)]
use std::time::Duration;

pub(crate) const DISPATCH: u32 = 0;
pub(crate) const FALLBACK: u32 = 1;
pub(crate) const BUDGET: u32 = 2;
pub(crate) const LOAD: u32 = 3;
pub(crate) const STORE: u32 = 4;

#[derive(Clone, Debug, Default)]
pub struct Tier1Stats {
    pub compilations: u64,
    pub compile_time: Duration,
    pub code_bytes: u64,
    pub native_entries: u64,
    pub native_instructions: u64,
    pub direct_transfers: u64,
    pub saves: u64,
    pub restores: u64,
    pub fallback_instructions: u64,
    pub checked_accesses: u64,
    pub invalidations: u64,
}

#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android")
))]
mod emit;
#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android")
))]
mod native;

pub(crate) struct Execution {
    pub execution: crate::interpreter::Execution,
    pub interpreted_units: u32,
    pub interpreted: bool,
    pub last_units: u32,
}

#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android")
))]
pub(crate) use native::Engine;

#[cfg(not(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android")
)))]
pub(crate) struct Engine;
#[cfg(not(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android")
)))]
impl Engine {
    pub fn new() -> Result<Self, String> {
        Err(
            "ARM64 Tier 1 requires aarch64 Linux or Android; other platforms are not supported"
                .into(),
        )
    }
    pub fn stats(&self) -> Tier1Stats {
        Tier1Stats::default()
    }
    pub fn run_block(
        &mut self,
        _state: &mut crate::CpuState,
        _memory: &mut crate::Memory,
        _interpreter: &mut crate::interpreter::Interpreter,
        _units: u32,
        _stop: u32,
    ) -> Result<Execution, String> {
        unreachable!("unsupported Tier 1 cannot be selected")
    }
}
