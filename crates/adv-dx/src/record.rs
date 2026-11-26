//! Shared frame recording and damage planning.
use rustc_hash::FxHashMap as HashMap;
use std::sync::Arc;

mod contents;
pub(crate) mod damage;
pub(crate) mod key;
pub(crate) mod passes;
pub(crate) mod reuse;
pub(crate) mod trace;
pub(crate) use contents::{Contents, ResourceKey};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Draw,
    Clear,
    Copy,
    Upload,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub fixed_draws: u64,
    pub fixed_shaders_built: u64,
    pub fixed_shader_cache_hits: u64,
    pub draw_key_hashes: u64,
    /// Draws that reused a previous frame's preparation, and draws that
    /// skipped the attempt while their target rested.
    pub reused_draws: u64,
    pub rested_draws: u64,
    pub readback_bytes: u64,
    pub presents: u64,
    pub recorded_draws: u64,
    pub recorded_draw_bytes: u64,
    pub recording_lookups: u64,
    pub stateblock_applies: u64,
    pub stateblock_items: u64,
    pub eliminated_passes: u64,
    pub recorded_commands: u64,
    pub gpu_draws: u64,
    pub uncoalesced_gpu_draws: u64,
    pub fast_path_draws: u64,
    pub general_path_draws: u64,
    /// Present, readback, lock, DC, Reset, Release, limit, existing GPU wait.
    pub flushes: [u64; 8],
    pub eliminated_frames: u64,
    pub verification_mismatches: u64,
    pub full_executions: u64,
    pub clipped_executions: u64,
    pub clipped_area_sum: f64,
    /// Presents that changed the whole surface, by cause: a flush that
    /// changed contents outside the analysis, a new presentation, a whole
    /// COPY publication, and the analysis itself (a whole change).
    pub whole: [u64; 4],
    /// Final chains planned whole because a late demand reached writes
    /// already planned.
    pub promotions: u64,
    pub intermediate_promotions: u64,
    /// Recorded writes submitted, once per scissor, and the pixels those
    /// submissions covered (a whole write counts its image).
    pub submissions: u64,
    pub submitted_pixels: u64,
    /// Input vertices submitted, including instances and repeated scissors.
    pub submitted_vertices: u64,
    pub repeated_vertices: u64,
    /// Backbuffer cell areas accumulated once per completed present.
    pub changed_pixels: u64,
    pub executed_pixels: u64,
    pub extra_pixels: u64,
}
impl Counters {
    pub fn work_report(&self) -> String {
        if !cfg!(feature = "d3d9-counters") {
            return String::new();
        }
        format!(
            "\nD3D9 work: draws {}; recorded bytes {} ({:.2}/draw); recording lookups {} ({:.2}/draw); StateBlock applies {}; applied items {} ({:.2}/apply); dead frames {}; dead passes {}",
            self.recorded_draws,
            self.recorded_draw_bytes,
            self.recorded_draw_bytes as f64 / self.recorded_draws.max(1) as f64,
            self.recording_lookups,
            self.recording_lookups as f64 / self.recorded_draws.max(1) as f64,
            self.stateblock_applies,
            self.stateblock_items,
            self.stateblock_items as f64 / self.stateblock_applies.max(1) as f64,
            self.eliminated_frames,
            self.eliminated_passes
        )
    }
}
#[cfg(feature = "d3d9-counters")]
thread_local! { static LOOKUPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
#[cfg(feature = "d3d9-counters")]
pub(crate) fn lookup(count: u64) {
    LOOKUPS.set(LOOKUPS.get() + count);
}
#[cfg(feature = "d3d9-counters")]
pub(crate) fn lookups() -> u64 {
    LOOKUPS.get()
}
