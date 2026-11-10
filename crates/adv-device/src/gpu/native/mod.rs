//! The D3D9 and DirectDraw executor over `adv-gpu`.
//!
//! # Threads
//!
//! - The execution thread calls the facade and creates resources. Pipelines
//!   share one job table with the low-priority compiler (`adv_gpu::Device` is
//!   `Send + Sync`); creation-time predictions compile before demand. Data is
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
//! | `frame.rs`, software and video slots | E | frames, video planes and their presentation layers |
//!

mod backing;
mod owner;
pub(crate) mod pass;
mod pipelines;
mod record;
mod resources;
pub(crate) mod stream;
#[cfg(feature = "test-support")]
#[path = "../test_support/native.rs"]
pub mod test_support;

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
    pub fn request(fallback: bool) -> Result<Self, Error> {
        Self::execution(fallback)
    }
    /// Start the execution thread's owner with its lifetime presenter, before
    /// any surface store or facade starts a headless one. The presenter stays
    /// until the last facade drops; a running execution refuses another.
    pub fn request_with_presenter(
        device: adv_gpu::Device,
        queue: adv_gpu::Queue,
        presenter: Box<dyn crate::frame::Presenter>,
    ) -> Result<Self, Error> {
        Self::start(false, Some((device, queue, presenter)))
    }
    /// Share the thread's execution, starting a headless one if none runs.
    pub(crate) fn execution(fallback: bool) -> Result<Self, Error> {
        Self::start(fallback, None)
    }
    fn start(
        fallback: bool,
        presenter: Option<(
            adv_gpu::Device,
            adv_gpu::Queue,
            Box<dyn crate::frame::Presenter>,
        )>,
    ) -> Result<Self, Error> {
        let execution = EXECUTION.with(|cached| {
            if let Some(execution) = cached.borrow().upgrade() {
                if presenter.is_some() {
                    return Err(Error::Invalid(
                        "the execution already runs with its presenter".into(),
                    ));
                }
                return Ok(execution);
            }
            let (device, queue, presenter) = if let Some((device, queue, presenter)) = presenter {
                (device, queue, presenter)
            } else {
                let (device, queue) = adv_gpu::Device::open(&adv_gpu::queue::DeviceOptions {
                    software: fallback,
                    validation: cfg!(all(
                        any(test, feature = "test-support"),
                        not(target_os = "android")
                    )),
                    #[cfg(feature = "test-support")]
                    disable: adv_gpu::Modern::from_test_env(),
                    label: "execution GPU",
                    ..Default::default()
                })
                .map_err(record::gpu_error)?;
                (
                    device,
                    queue,
                    Box::new(owner::Headless) as Box<dyn crate::frame::Presenter>,
                )
            };
            let (producer, commands) =
                stream::channel(device.ring(64 << 20).map_err(record::gpu_error)?);
            let execution = std::rc::Rc::new(Execution {
                device,
                stream: stream::ExecutionStream(std::rc::Rc::new(std::cell::RefCell::new(
                    producer,
                ))),
                #[cfg(feature = "test-support")]
                capture: Default::default(),
                owner: owner::Owner::start(queue, commands, presenter)?,
            });
            *cached.borrow_mut() = std::rc::Rc::downgrade(&execution);
            Ok(execution)
        })?;
        Self::facade(execution)
    }
    /// Another facade on this execution, for a child process that the
    /// session drives on the same thread. Its commands share the device,
    /// stream and owner; pipelines, recording state and staging start fresh.
    pub fn fork(&self) -> Result<Self, Error> {
        self.check()?;
        Self::facade(self.execution.clone())
    }
    fn facade(execution: std::rc::Rc<Execution>) -> Result<Self, Error> {
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
    pub fn waker(&self) -> crate::frame::Waker {
        self.execution.owner.wake.as_ref().unwrap().clone()
    }
    /// Device shared with the presenting queue, including decoder pools.
    pub fn device(&self) -> &adv_gpu::Device {
        &self.device
    }

    pub(crate) fn software_slots(
        &self,
        surface: &crate::surface::Surface,
    ) -> Result<crate::surface::slots::Slots, String> {
        crate::surface::slots::Slots::new(&self.device, surface)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn publish_software(
        &mut self,
        slots: &mut crate::surface::slots::Slots,
        surface: &crate::surface::Surface,
        memory: &dyn crate::surface::SurfaceMemory,
        damage: crate::frame::Region,
        whole: bool,
        serial: u64,
        video: Arc<[crate::frame::VideoPlane]>,
        windows: Arc<[crate::frame::WindowLayer]>,
    ) -> Result<u64, String> {
        self.check().map_err(|e| e.to_string())?;
        slots.publish(
            &mut self.stream.0.borrow_mut(),
            surface,
            memory,
            damage,
            whole,
            serial,
            video,
            windows,
        )
    }
    pub fn set_clip(&mut self, clip: Option<Rect>) {
        self.clip = clip;
    }
    pub fn stats(&self) -> Stats {
        let mut stats = self.stats;
        let cache = self.pipelines.stats();
        stats.pipeline_cache_hits += cache.pipeline_cache_hits;
        stats.anticipated_pipeline_draws += cache.anticipated_pipeline_draws;
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
    owner: owner::Owner,
    #[cfg(feature = "test-support")]
    capture: std::cell::RefCell<Option<std::rc::Rc<test_support::Frames>>>,
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
        let first =
            Gpu::execution(!cfg!(any(target_vendor = "apple", target_os = "android"))).unwrap();
        let second =
            Gpu::execution(!cfg!(any(target_vendor = "apple", target_os = "android"))).unwrap();
        assert!(std::rc::Rc::ptr_eq(&first.stream.0, &second.stream.0));
        assert!(std::rc::Rc::ptr_eq(&first.execution, &second.execution));
        let child = first.fork().unwrap();
        assert!(std::rc::Rc::ptr_eq(&first.stream.0, &child.stream.0));
        assert!(std::rc::Rc::ptr_eq(&first.execution, &child.execution));
    }
    #[test]
    fn the_execution_presenter_is_fixed_until_its_last_facade_drops() {
        struct Capture(std::sync::mpsc::Sender<u64>);
        impl crate::frame::Presenter for Capture {
            fn presentable(&self, _: u32, _: u32) -> bool {
                false
            }
            fn present(&mut self, _: &mut adv_gpu::Queue, frame: crate::frame::Frame) {
                self.0.send(frame.serial).unwrap();
            }
            fn service(&mut self, _: &mut adv_gpu::Queue, _: Option<&crate::frame::Waker>) {}
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (device, queue) = adv_gpu::Device::open(&adv_gpu::queue::DeviceOptions {
            software: !cfg!(any(target_vendor = "apple", target_os = "android")),
            ..Default::default()
        })
        .unwrap();
        let mut gpu =
            Gpu::request_with_presenter(device.clone(), queue, Box::new(Capture(tx))).unwrap();
        let (other_tx, _) = std::sync::mpsc::channel();
        let (other_device, other_queue) = adv_gpu::Device::open(&adv_gpu::queue::DeviceOptions {
            software: !cfg!(any(target_vendor = "apple", target_os = "android")),
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(
            Gpu::request_with_presenter(other_device, other_queue, Box::new(Capture(other_tx))),
            Err(Error::Invalid(_))
        ));
        let shared = Gpu::request(false).unwrap();
        assert!(std::rc::Rc::ptr_eq(&gpu.execution, &shared.execution));
        let slot =
            crate::frame::Slot::new(&gpu.device, 1, 1, 4, crate::surface::PixelFormat::Bgra32)
                .unwrap();
        gpu.stream.push(stream::Command::Present {
            base: crate::frame::Base::Software {
                slot,
                format: crate::surface::PixelFormat::Bgra32,
                palette: None,
                damage: crate::frame::Region::whole((1, 1)),
            },
            video: Arc::from([]),
            windows: Arc::from([]),
            serial: 7,
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap(),
            7
        );
        drop(shared);
        drop(gpu);
        let _next =
            Gpu::request(!cfg!(any(target_vendor = "apple", target_os = "android"))).unwrap();
    }
}
