//! Explicit ARM64 Tier 1 lane: bounded integer/SIMD templates and guarded x87 helper calls.
//! Memory operations retain the existing checked Memory contract, through
//! helper calls from native code and an exit when a helper refuses. No hardware
//! fault recovery, floating approximation or parallel guest execution is enabled.
#![cfg_attr(
    not(all(
        target_arch = "aarch64",
        any(target_os = "linux", target_os = "android", target_os = "macos")
    )),
    allow(dead_code)
)]
use std::time::Duration;

/// Selection is fixed before execution. Bridge preserves the caller's automatic
/// and asynchronous Cranelift policy; Exclusive retains the original opt-in lane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tier1Mode {
    #[default]
    Off,
    Exclusive,
    Bridge,
}

impl std::str::FromStr for Tier1Mode {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" => Ok(Self::Off),
            "exclusive" => Ok(Self::Exclusive),
            "bridge" => Ok(Self::Bridge),
            _ => Err("ADV_TIER1 must be bridge, exclusive or off".into()),
        }
    }
}

pub(crate) mod census;
pub(crate) mod forms;
mod x87;

pub(crate) const DISPATCH: u32 = 0;
pub(crate) const FALLBACK: u32 = 1;
pub(crate) const BUDGET: u32 = 2;
pub(crate) const LOAD: u32 = 3;
pub(crate) const STORE: u32 = 4;

#[derive(Clone, Debug, Default)]
pub struct Tier1Stats {
    /// Retired native units selected as a Cranelift waiting bridge, excluding fallback.
    pub bridge_units: u64,
    pub bridge_entries: u64,
    /// Live RX mappings, including page rounding; code_bytes is cumulative emission.
    pub resident_bytes: usize,
    pub cache_evictions: u64,
    pub cache_rejections: u64,
    /// Cranelift revisions observed, including revisions with an empty cache.
    pub publication_revisions: u64,
    /// Nonempty whole-cache retirements caused by Cranelift publication.
    pub publication_flushes: u64,
    pub compile_misses: u64,
    /// Entries returning through Tier 0, including unsupported native exits.
    pub fallback_entries: u64,
    pub compilations: u64,
    pub compile_time: Duration,
    pub code_bytes: u64,
    pub native_entries: u64,
    pub native_instructions: u64,
    pub direct_transfers: u64,
    pub saves: u64,
    pub restores: u64,
    pub fallback_instructions: u64,
    /// Accesses that left native code for the checked Rust exit.
    pub checked_accesses: u64,
    /// Accesses completed by a helper call without leaving native code.
    pub inline_accesses: u64,
    pub invalidations: u64,
}

#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
mod emit;
#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
mod native;
#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
mod simd;

pub(crate) struct Execution {
    pub execution: crate::interpreter::Execution,
    pub interpreted_units: u32,
    pub interpreted: bool,
    pub last_units: u32,
}

#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
pub(crate) use native::Engine;

#[cfg(not(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
)))]
pub(crate) struct Engine;
#[cfg(not(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
)))]
impl Engine {
    pub fn record_bridge(&mut self, _units: u32) {}
    pub fn set_cache_limits(&mut self, _entries: usize, _bytes: usize) {}
    pub fn new() -> Result<Self, String> {
        Err(
            "ARM64 Tier 1 requires aarch64 Linux, Android or macOS; other platforms are not supported"
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
        _boundaries: Option<(&std::collections::BTreeMap<u32, u32>, u64)>,
    ) -> Result<Execution, String> {
        unreachable!("unsupported Tier 1 cannot be selected")
    }
}
