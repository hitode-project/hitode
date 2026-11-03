//! Presentation planning, stage encoding, pool bookkeeping and compositor ownership.
//!
//! [`plan::plan`] consumes serializable geometry, damage, capabilities and pool
//! state. Executors run its stages and call [`plan::PoolState::after_present`]
//! only after submitting the complete frame. [`stages`] records GPU commands;
//! the owner thread submits them.
//! [`compositor`] transfers presentable images to the system compositor and
//! returns ownership on release. [`recorder`] stores and replays planner data.

pub mod compositor;
pub mod display;
pub mod plan;
pub mod recorder;
pub mod stages;

/// A rectangle in physical pixels or texels, with its origin at the top left.
/// Display owns this serializable geometry; GPU rectangles remain GPU data.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl From<Rect> for adv_gpu::Rect {
    fn from(r: Rect) -> Self {
        Self {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        }
    }
}

impl From<adv_gpu::Rect> for Rect {
    fn from(r: adv_gpu::Rect) -> Self {
        Self {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        }
    }
}

#[cfg(test)]
mod tests;
// Native presentable buffers are supplied by IOSurface or AHardwareBuffer.
// The Vulkan desktop backend exposes private images only.
#[cfg(all(test, target_vendor = "apple"))]
mod display_tests;
