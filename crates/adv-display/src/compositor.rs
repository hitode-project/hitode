//! The bridge to the system compositor. One implementation per platform, plus
//! a swapchain compositor (Linux, Windows) and a headless one (tests, replays).

use crate::plan::{CompositorCaps, Layers, Tiles};
use std::time::Instant;

/// What the compositor reported for one present, for pacing and the recorder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Presented {
    pub serial: u64,
    /// When the compositor latched the buffers, if it reports it.
    pub latched: Option<Instant>,
    /// When the frame reached the panel, if it reports it.
    pub shown: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The window or surface is gone; rebuild the compositor, keep the pools.
    SurfaceLost,
    /// The GPU device is lost.
    DeviceLost(String),
}

pub trait Compositor {
    fn caps(&self) -> CompositorCaps;
    /// Attach `layers` in one transaction, hinting `damage`, for display at or
    /// after `at`. Buffers in `layers` are already written or have acquire
    /// fences attached; their release returns them to their pools.
    fn present(
        &mut self,
        layers: &Layers,
        damage: &Tiles,
        serial: u64,
        at: Instant,
    ) -> Result<(), Error>;
    /// Reports for earlier presents, oldest first. Never blocks.
    fn drain(&mut self, out: &mut Vec<Presented>);
}
