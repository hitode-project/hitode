//! The D3D9 and DirectDraw executor over `adv-gpu` (issue #103, packages C1
//! and C2), behind the `native-display` feature. It keeps the facade the wgpu
//! renderer offers `adv-dx`, method for method, so the front end does not
//! change; package G removes the wgpu renderer and the feature.
//!
//! # Threads
//!
//! - The execution thread calls the facade. Resources and pipelines are
//!   created synchronously on it (`adv_gpu::Device` is `Send + Sync`); data is
//!   written once, into the upload ring or pooled staging buffers, in the
//!   resource's own format. Every GPU operation becomes a [`stream::Command`].
//!   It never encodes, submits or
//!   waits for the GPU, except a readback the guest waits for.
//! - The GPU owner thread ([`owner`]) owns the `adv_gpu::Queue`. It turns
//!   command chunks into passes ([`pass`]), encodes and submits them,
//!   and hands each presented frame to the display layer on the same thread,
//!   so one thread makes every submission.
//!
//! # Mechanisms
//!
//! - Render passes cover only the pixels their operations change, and load
//!   or store an attachment only when its contents are read before or after.
//! - Uploads carry guest bytes unconverted; formats the GPU stores natively
//!   need no CPU pass, and X8 formats are sampled with alpha forced to one.
//! - Depth buffers that no pass stores are transient attachments (memoryless
//!   on Apple, lazily allocated on Vulkan); the owner thread decides per
//!   frame ([`backing`]).
//! - Recording allocates nothing after warm-up: command chunks and their ring
//!   spans are reused.
//! - Swap buffers are presentable buffers whenever the display plan shows
//!   them by reference, so a frame reaches the system compositor without a
//!   copy; the owner thread picks each frame's buffer ([`backing`]).
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
//! | `backing.rs` | C2 | swap and depth buffers: private, presentable or transient memory |
//! | `surface.rs` native surfaces | C2 | present through [`Gpu::present`]; the rotation pool and `NativeFrame` publication of the wgpu renderer are compiled out under this feature |
//! | `frame.rs`, software and video slots | E | frames, video planes and their presentation layers |
//!
//! Under this feature `gpu/clear.rs` and the wgpu `Depth` are compiled out too;
//! package G deletes them with the renderer.

mod backing;
mod owner;
pub(crate) mod pass;
mod pipelines;
mod record;
mod resources;
pub(crate) mod stream;

pub use record::Readback;
pub use resources::{Depth, Image, ImageId, Staged, Target};

use super::{Error, Rect, Stats};

/// Execution-thread GPU state. Facades and software surface stores created on
/// one thread share its device, producer and owner; the facade stays on that
/// thread. The last facade closes the producer before joining the owner.
pub struct Gpu {
    device: adv_gpu::Device,
    stream: stream::ExecutionStream,
    pipelines: pipelines::Pipelines,
    /// The damage rectangle the next operations are limited to.
    clip: Option<Rect>,
    stats: Stats,
    recording: record::Recording,
    staging: resources::StagingPool,
    execution: std::rc::Rc<Execution>,
}

impl Gpu {
    /// Open the device and start its owner thread. `fallback` selects a
    /// software adapter (lavapipe) for tests and diagnostics.
    pub async fn request(fallback: bool) -> Result<Self, Error> {
        Self::execution(fallback)
    }
    pub(crate) fn execution(fallback: bool) -> Result<Self, Error> {
        let execution = EXECUTION.with(|cached| {
            if let Some(execution) = cached.borrow().upgrade() {
                return Ok(execution);
            }
            let (device, queue) = adv_gpu::Device::open(&adv_gpu::queue::DeviceOptions {
                software: fallback,
                validation: cfg!(test),
                label: "execution GPU",
                ..Default::default()
            })
            .map_err(record::gpu_error)?;
            let (producer, commands) =
                stream::channel(device.ring(64 << 20).map_err(record::gpu_error)?);
            let execution = std::rc::Rc::new(Execution {
                device,
                stream: stream::ExecutionStream(std::rc::Rc::new(std::cell::RefCell::new(
                    producer,
                ))),
                video: Arc::from([]),
                owner: owner::Owner::start(queue, commands, Box::new(owner::Headless))?,
            });
            *cached.borrow_mut() = std::rc::Rc::downgrade(&execution);
            Ok(execution)
        })?;
        let device = execution.device.clone();
        let stream = execution.stream.clone();
        let recording = record::Recording::new(&device, &mut stream.0.borrow_mut())?;
        Ok(Self {
            device,
            stream,
            pipelines: Default::default(),
            clip: None,
            stats: Stats::default(),
            recording,
            staging: Default::default(),
            execution,
        })
    }
    pub(crate) fn software_slots(
        &self,
        surface: &crate::surface::Surface,
    ) -> Result<crate::surface::slots::Slots, String> {
        crate::surface::slots::Slots::new(&self.device, surface)
    }
    pub(crate) fn publish_software(
        &mut self,
        slots: &mut crate::surface::slots::Slots,
        surface: &crate::surface::Surface,
        memory: &dyn crate::surface::SurfaceMemory,
        damage: crate::surface::Damage,
        whole: bool,
        serial: u64,
    ) -> Result<u64, String> {
        self.check().map_err(|e| e.to_string())?;
        slots.publish(
            &mut self.stream.0.borrow_mut(),
            surface,
            memory,
            damage,
            whole,
            serial,
            self.execution.video.clone(),
        )
    }
    pub fn set_clip(&mut self, clip: Option<Rect>) {
        self.clip = clip;
    }
    pub fn stats(&self) -> Stats {
        let mut stats = self.stats;
        let cache = self.pipelines.stats();
        stats.pipeline_cache_hits += cache.pipeline_cache_hits;
        stats.compile_waits += cache.compile_waits;
        stats.compile_wait_ns += cache.compile_wait_ns;
        stats.prebuild_hits += cache.prebuild_hits;
        stats.prebuild_misses += cache.prebuild_misses;
        stats.persisted_pipelines = cache.persisted_pipelines;
        stats.discarded_pipelines = cache.discarded_pipelines;
        stats
    }
    /// Device loss, as reported by the owner thread.
    pub fn check(&self) -> Result<(), Error> {
        if let Some(failure) = self.execution.owner.failure.get() {
            return Err(Error::Device(failure.clone()));
        }
        if let Some(failure) = self.device.lost() {
            return Err(Error::Device(failure));
        }
        Ok(())
    }
}

use std::sync::Arc;
struct Execution {
    device: adv_gpu::Device,
    // The producer must close before Owner joins its thread.
    stream: stream::ExecutionStream,
    video: Arc<[crate::frame::VideoPlane]>,
    owner: owner::Owner,
}
thread_local! {
    static EXECUTION: std::cell::RefCell<std::rc::Weak<Execution>> = const {
        std::cell::RefCell::new(std::rc::Weak::new())
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_gpu_on_the_execution_thread_uses_one_producer() {
        let first = Gpu::execution(!cfg!(target_vendor = "apple")).unwrap();
        let second = Gpu::execution(!cfg!(target_vendor = "apple")).unwrap();
        assert!(std::rc::Rc::ptr_eq(&first.stream.0, &second.stream.0));
        assert!(std::rc::Rc::ptr_eq(&first.execution, &second.execution));
    }
}
