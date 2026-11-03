//! Package C2: swap buffers, resolved on the GPU owner thread.
//!
//! The execution thread sees one image per swap buffer and records every
//! frame into it. The owner thread picks its allocation per frame:
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
//! The guest's damage therefore always describes one frame ago, and no
//! frame waits for a buffer on the execution thread. While a pool has no
//! free buffer the owner calls `Presenter::drain` and retries at most every
//! millisecond, and keeps the execution thread's chunks queued meanwhile.

/// Shared by the images of one swap buffer.
pub(crate) struct Swap {
    /// Image identity, in a range adv-gpu image ids never reach.
    pub id: u64,
    pub size: (u32, u32),
    /// The owner thread's half; the execution thread never locks it.
    pub(super) owner: std::sync::Mutex<Backing>,
}

/// The allocation of the current frame, the presented buffers' damage
/// history, and the pool while presentable.
pub(super) struct Backing {}
