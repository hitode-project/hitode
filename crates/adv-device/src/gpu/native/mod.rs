//! The D3D9 and DirectDraw executor over `adv-gpu` (issue #103, packages C1
//! and C2), behind the `native-display` feature. It keeps the facade the wgpu
//! renderer offers `adv-dx`, method for method, so the front end does not
//! change; package G removes the wgpu renderer and the feature.
//!
//! # Threads
//!
//! - The execution thread calls the facade. Resources and pipelines are
//!   created synchronously on it (`adv_gpu::Device` is `Send + Sync`); data is
//!   written once, into the upload ring, in the resource's own format; every
//!   GPU operation becomes a [`stream::Command`]. It never encodes, submits or
//!   waits for the GPU, except a readback the guest waits for.
//! - The GPU owner thread ([`owner`]) owns the `adv_gpu::Queue`. It turns
//!   command chunks into passes ([`pass::plan`]), encodes and submits them,
//!   and hands each presented frame to the display layer on the same thread,
//!   so one thread makes every submission.
//!
//! # Mechanisms
//!
//! - Render passes cover only the pixels their operations change, and load
//!   or store an attachment only when its contents are read before or after.
//! - Uploads carry guest bytes unconverted; formats the GPU stores natively
//!   need no CPU pass, and X8 formats are sampled with alpha forced to one.
//! - Depth buffers that every pass clears first and none reads afterwards are
//!   transient attachments (memoryless on Apple, lazily allocated on Vulkan).
//! - Recording allocates nothing after warm-up: command chunks and their ring
//!   spans are reused.
//!
//! # Ownership of the files
//!
//! | File | Package | Contents |
//! |---|---|---|
//! | `stream.rs`, `pass.rs` | skeleton | the command stream; pass formation, loads and stores |
//! | `resources.rs` | C1 | images, targets, depth, staging and uploads |
//! | `pipelines.rs` | C1 | fixed-function, programmable and blit pipelines; the disk cache |
//! | `record.rs` | C2 | clears, draws, blits, copies, presents and readbacks as commands |
//! | `owner.rs` | C2 | the owner thread: passes, encoding, submission, frames |
//! | `surface.rs` native surfaces | C2 | present through [`Gpu::present`]; the rotation pool and `NativeFrame` publication of the wgpu renderer are compiled out under this feature |
//! | `frame.rs`, software and video slots | E | frames, video planes and their presentation layers |
//!
//! Under this feature `gpu/clear.rs` and the wgpu `Depth` are compiled out too;
//! package G deletes them with the renderer.

mod owner;
pub(crate) mod pass;
mod pipelines;
mod record;
mod resources;
pub(crate) mod stream;

pub use record::Readback;
pub use resources::{Depth, Image, Staged, Target};

use super::{Error, Rect, Stats};

/// One executor: a device, its execution-thread state and the channel to its
/// owner thread.
pub struct Gpu {
    device: adv_gpu::Device,
    stream: stream::Producer,
    pipelines: pipelines::Pipelines,
    /// The damage rectangle the next operations are limited to.
    clip: Option<Rect>,
    stats: Stats,
}

impl Gpu {
    /// Open the device and start its owner thread. `fallback` selects a
    /// software adapter (lavapipe) for tests and diagnostics.
    pub async fn request(fallback: bool) -> Result<Self, Error> {
        let _ = fallback;
        todo!("C2")
    }
    /// A second executor on the same device and owner thread, for another
    /// guest device.
    pub fn fork(&self) -> Result<Self, Error> {
        todo!("C2")
    }
    pub fn set_clip(&mut self, clip: Option<Rect>) {
        self.clip = clip;
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
    /// Device loss, as reported by the owner thread.
    pub fn check(&self) -> Result<(), Error> {
        todo!("C2")
    }
}
