//! The display layer: everything between a published game frame and the panel.
//!
//! **Skeleton for review (issue #103, wave 0).** Contract types only.
//!
//! - [`plan`] decides, per present, the cheapest way to show a frame. It is a
//!   pure function of plain data and never touches the GPU, so tests and the
//!   flight recorder replay it exactly.
//! - Stages execute a plan on the GPU owner thread over `adv-gpu`.
//! - [`compositor::Compositor`] hands layers to the system compositor:
//!   SurfaceControl on Android, Core Animation on Apple, a swapchain elsewhere,
//!   and a headless one for tests.

pub mod compositor;
pub mod plan;
pub mod recorder;

pub use adv_gpu::Rect;
