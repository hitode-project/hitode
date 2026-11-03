//! Package C2: attachments whose allocation the GPU owner thread picks.
//!
//! The execution thread holds one handle per swap buffer and per depth
//! buffer and records every frame against it; the owner thread decides,
//! when it encodes, what memory is behind it.
//!
//! # Swap buffers
//!
//! - Private, while [`crate::frame::Presenter::presentable`] says no: one
//!   private (compressed) image, presented as `SwapImage::Private`. The
//!   display stages read it in queue order, before the next frame writes it.
//! - Presentable: a pool from `Device::create_present_pool`. After each
//!   `Present` the owner acquires the next buffer and, before the next pass
//!   on it, copies from the buffer just presented what is not current in it:
//!   the damage of the presents since its age, everything when its content is
//!   undefined (a new buffer, or a change of role). The frame then goes to
//!   the presenter as `SwapImage::Presentable`, with no copy.
//!
//! The guest's damage therefore always describes one frame ago (the
//! recording sees age one), and no frame waits for a buffer on the execution
//! thread. While a pool has no free buffer the owner calls
//! `Presenter::drain` and retries at most every millisecond, leaving the
//! execution thread's chunks queued meanwhile.
//!
//! # Depth buffers
//!
//! A depth buffer starts without memory. A pass whose plan stores depth
//! ([`super::pass::Pass::depth`] or `stencil` with `Store::Store`) gives it a
//! real allocation first; until then, and again after a frame in which no
//! pass stored it, it is transient (`Usage::TRANSIENT`: memoryless on Apple,
//! lazily allocated on Vulkan) and the real allocation is freed. A pass that
//! loads depth always follows one that stored it, so transient memory is
//! never loaded.

/// Shared by the images of one swap buffer.
pub(crate) struct Swap {
    /// Image identity, in a range adv-gpu image ids never reach.
    pub id: u64,
    pub size: (u32, u32),
    /// The owner thread's half; the execution thread never locks it.
    pub(super) owner: std::sync::Mutex<SwapBacking>,
}

/// The allocation of the current frame, the presented buffers' damage
/// history, and the pool while presentable.
pub(super) struct SwapBacking {}

/// Shared by the clones of one depth buffer.
pub(crate) struct DepthBuffer {
    pub size: (u32, u32),
    pub format: super::super::DepthFormat,
    /// The owner thread's half; the execution thread never locks it.
    pub(super) owner: std::sync::Mutex<DepthBacking>,
}

/// Transient or real, and whether the current frame stored it.
pub(super) struct DepthBacking {}
