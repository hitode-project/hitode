//! Host-facing control of the platform's execution engine.
//!
//! Shells configure, report and persist the engine only through [`Engine`],
//! so the engine behind the `adv-cpu` facade can differ per target
//! architecture. Options an engine does not have are ignored, and report
//! fields belong to the engine that produced them.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::CompileStrategy;
use crate::backend::Backend;
use crate::frame_counters::FrameCounters;

/// Engine policy chosen by the host shell before the first guest run.
#[derive(Clone, Debug)]
pub struct EngineOptions {
    pub compile_strategy: CompileStrategy,
    pub tiered_compilation: bool,
    pub region_linking: bool,
    pub async_compilation: bool,
    pub row_batching: bool,
    /// Additional row helpers; `None` keeps the engine's default.
    pub row_workers: Option<usize>,
    /// Background compiler workers; `None` applies the engine's host policy.
    pub compile_threads: Option<usize>,
    /// Directory of the engine's startup artifacts. `None` keeps nothing.
    pub artifacts: Option<PathBuf>,
    /// Persistent native machine code in `artifacts`.
    pub code_cache: bool,
    /// Bounded discovery and compilation of mapped images before execution.
    pub precompile: bool,
    /// Bounded per-frame accounting for the frame-budget tools.
    pub frame_timing: bool,
    /// Per-entry execution timings; region links are disabled while enabled.
    pub profiling: bool,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            compile_strategy: CompileStrategy::Fast,
            tiered_compilation: true,
            region_linking: true,
            async_compilation: true,
            row_batching: true,
            row_workers: None,
            compile_threads: None,
            artifacts: None,
            code_cache: true,
            precompile: true,
            frame_timing: false,
            profiling: false,
        }
    }
}

/// Per-entry execution timings collected while profiling.
#[derive(Default, Clone, Debug)]
pub struct ProfileSample {
    pub native_entries: u64,
    pub native_ns: u64,
    pub interpreter_entries: u64,
    pub interpreter_instructions: u64,
    pub interpreter_ns: u64,
}

/// Counters every engine reports in the one-line run summary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineCounters {
    pub native_entries: u64,
    pub interpreter_entries: u64,
}

/// The execution engine as the host shell sees it.
pub trait Engine: Backend + Sized {
    /// A fresh address space; `native` false keeps every run in the interpreter.
    fn create(park: u32, native: bool) -> Result<Self, String>;
    /// Apply the shell's policy. Call once, after the waker and background
    /// sender are installed and before the first run.
    fn configure(&mut self, options: &EngineOptions) -> Result<(), String>;
    /// Install the execution owner's retained event before the first guest run.
    fn set_waker(&mut self, wake: adv_sync::WakeEvent) -> Result<(), String>;
    fn set_background_sender(
        &mut self,
        sender: adv_sync::background::LocalSender,
    ) -> Result<(), String>;
    fn background_sender(&mut self) -> Result<adv_sync::background::LocalSender, String>;
    fn counters(&self) -> EngineCounters;
    /// Per-frame accounting while frame timing is enabled.
    fn frame_counters(&self) -> Option<FrameCounters> {
        Backend::frame_counters(self)
    }
    /// Engine-specific report members as `"key": value` pairs joined by
    /// commas, without surrounding braces; empty when there are none.
    fn report_fields(&self) -> String;
    /// Exact retired Tier 0 instructions while diagnostic counting is enabled.
    /// REP elements remain work units and retire only one completed instruction.
    fn tier0_instructions(&self) -> Option<u64> {
        None
    }
    /// Write files that accompany a JSON report at `report`.
    fn write_report_extras(&self, _report: &Path) -> Result<(), String> {
        Ok(())
    }
    /// Per-entry timings collected while profiling.
    fn profile(&self) -> BTreeMap<u32, ProfileSample> {
        BTreeMap::new()
    }
    /// Save startup artifacts to the configured directory.
    fn persist(&self) -> Result<(), String> {
        Ok(())
    }
}

/// A JSON string literal.
pub fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
