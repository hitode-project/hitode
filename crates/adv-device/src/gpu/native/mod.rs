//! The D3D9 and DirectDraw executor over `adv-gpu`.
//!
//! # Threads
//!
//! - The execution thread calls the facade, validates arguments, snapshots
//!   guest memory and creates resources (`adv_gpu::Device` is `Send + Sync`).
//! - The CSMT worker thread (`"adv-gfx-worker"`, [`record`]) owns `Pipelines`,
//!   sampler caches, `record::Recording` and the upload ring (`stream::Producer`).
//!   Pipeline compilation waits, uniform/vertex packing, recipe expansion and
//!   multi-scissor deduplication happen here without stalling the execution
//!   thread or the GPU owner.
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
mod compressed;
mod expand;
mod modern;
mod owner;
pub(crate) mod pass;
mod pipelines;
mod record;
mod resources;
pub(crate) mod stream;
#[cfg(feature = "test-support")]
#[path = "../test_support/native.rs"]
pub mod test_support;
mod uniform;

pub use compressed::{BlockTexture, decode_bc, decoded_bc_format};
pub use expand::{
    Expand, ExpansionBinding, ExpansionDispatch, ExpansionDraw, ExpansionPrelude, ExpansionRegion,
    ExpansionWindow,
};
pub use modern::{
    BufferRange, Dispatch, DispatchArguments, DrawArguments, ImageAlias, ImageRegion, ModernColor,
    ModernDepth, ModernDraw, ModernShader, ModernTransfer, Predicate, QuerySlot, SlotBindings,
    StorageBinding, TimestampDisjoint, TransferBuffer,
};
pub use record::Readback;
pub use resources::{Depth, Image, ImageId, Staged, Target};
pub use uniform::{BufferWindow, UniformWindow};

use super::{Error, Rect, Stats};

/// Execution-thread GPU state. Facades and software surface stores created on
/// one thread share its device, `"adv-gfx-worker"` CSMT thread and `"GPU owner"`;
/// the facade stays on that thread. The last facade closes `"adv-gfx-worker"`
/// (which closes `stream::Producer`) before joining `"GPU owner"`.
pub struct Gpu {
    device: adv_gpu::Device,
    stream: stream::ExecutionStream,
    pipeline_jobs:
        Arc<std::sync::Mutex<std::collections::HashMap<[u8; 32], pipelines::PipelineJob>>>,
    pipeline_counters: super::compile::CompileCounters,
    facade_id: u64,
    failure: Arc<std::sync::Mutex<Option<Error>>>,
    wake: std::cell::Cell<bool>,
    draws: u64,
    /// The damage rectangle the next operations are limited to.
    clip: Option<Rect>,
    readbacks: Arc<std::sync::atomic::AtomicUsize>,
    readback_limit: usize,
    staging: resources::StagingPool,
    execution: std::rc::Rc<Execution>,
}

impl Drop for Gpu {
    fn drop(&mut self) {
        if self.wake.get() {
            self.execution.worker.set_wake(self.facade_id, None);
        }
        self.execution.worker.drop_facade(self.facade_id);
    }
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
    /// Clone the active execution's GPU device on this thread, if one is running.
    pub(crate) fn current_device() -> Option<adv_gpu::Device> {
        EXECUTION.with(|cached| cached.borrow().upgrade().map(|e| e.device.clone()))
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
            const TRANSIENT_BYTES: u64 = 64 << 20;
            let (ring, resident) = device
                .ring_with_resident(TRANSIENT_BYTES, uniform::PAGE_BYTES)
                .map_err(record::gpu_error)?;
            let uniforms = uniform::Pool::new(resident, TRANSIENT_BYTES);
            let (producer, commands) = stream::channel(ring);
            let gate = producer.gate();
            let worker_shared = Arc::new(record::WorkerShared::new());
            let owner = owner::Owner::start(queue, commands, presenter, worker_shared.clone())?;
            let worker = record::WorkerHandle::start(
                device.clone(),
                producer,
                worker_shared,
                owner.wake.clone(),
            )?;
            let execution = std::rc::Rc::new(Execution {
                device,
                stream: stream::ExecutionStream(std::rc::Rc::new(std::cell::RefCell::new(gate))),
                worker,
                owner,
                uniforms,
                #[cfg(feature = "test-support")]
                capture: Default::default(),
            });
            *cached.borrow_mut() = std::rc::Rc::downgrade(&execution);
            Ok(execution)
        })?;
        Self::facade(execution, None)
    }
    /// Another facade on this execution, for a child process that the
    /// session drives on the same thread. Its commands share the device,
    /// worker, stream and owner; pipelines, recording state and staging start fresh.
    pub fn fork(&self) -> Result<Self, Error> {
        self.check()?;
        Self::facade(self.execution.clone(), None)
    }

    /// Another API front on the same worker, sharing its immutable fallback texture.
    pub fn fork_front(&self) -> Result<Self, Error> {
        self.check()?;
        Self::facade(self.execution.clone(), Some(self.facade_id))
    }

    fn facade(execution: std::rc::Rc<Execution>, shared_white: Option<u64>) -> Result<Self, Error> {
        let device = execution.device.clone();
        let stream = execution.stream.clone();
        let failure = Arc::new(std::sync::Mutex::new(None));
        let (facade_id, pipeline_jobs, pipeline_counters) = execution
            .worker
            .create_facade(failure.clone(), shared_white)?;
        Ok(Self {
            device,
            stream,
            pipeline_jobs,
            pipeline_counters,
            facade_id,
            failure,
            wake: std::cell::Cell::new(false),
            draws: 0,
            clip: None,
            readbacks: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            readback_limit: 8,
            staging: Default::default(),
            execution,
        })
    }
    pub fn waker(&self) -> crate::frame::Waker {
        let owner_wake = self.execution.owner.wake.as_ref().unwrap();
        let worker_shared = self.execution.worker.shared();
        owner_wake.with_capture_hook(Arc::new(move |tx| {
            worker_shared.push_capture(tx);
        }))
    }
    /// Register an execution-thread wake event so backpressure on this facade
    /// returns `Error::Busy` instead of blocking on a condvar.
    pub fn set_wake(&self, wake: Option<Arc<adv_sync::WakeEvent>>) {
        self.wake.set(wake.is_some());
        self.execution.worker.set_wake(self.facade_id, wake);
    }
    /// Check device health, test submission gate, and in-flight frame budget
    /// before starting a presentation.
    pub fn admit_present(&self) -> Result<(), Error> {
        #[cfg(feature = "test-support")]
        self.stream.0.borrow().admit()?;
        self.check()?;
        self.execution.worker.admit_present(self.wake.get())
    }
    /// Synchronously drain all commands currently queued in `"adv-gfx-worker"`.
    pub fn drain_worker(&self) {
        self.execution.worker.drain();
    }
    /// Drain queued commands and clear a recoverable per-facade worker error
    /// (such as a single failed pipeline build) before device reset. Permanent
    /// GPU owner or hardware device loss remains reported by [`Self::check`].
    pub fn clear_worker_failure(&self) {
        self.drain_worker();
        *self.failure.lock().unwrap() = None;
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
        let (copied, base) = slots.publish(
            &self.stream.0.borrow(),
            || {
                self.execution
                    .worker
                    .push_boundary(record::WorkerItem::Flush, 0);
            },
            surface,
            memory,
            damage,
            whole,
        )?;
        self.execution
            .worker
            .push_present(record::WorkerItem::PresentSoftware {
                base,
                video,
                windows,
                serial,
            });
        Ok(copied)
    }
    pub fn set_clip(&mut self, clip: Option<Rect>) {
        self.clip = clip;
    }
    pub fn draw_count(&self) -> u64 {
        self.draws
    }

    pub fn stats(&self) -> Stats {
        self.execution.worker.stats(self.facade_id)
    }

    #[cfg(feature = "graphics-counters")]
    pub fn traffic(&self) -> super::Traffic {
        self.stream.0.borrow().traffic()
    }

    #[cfg(feature = "graphics-counters")]
    pub fn count_upload(&self, bytes: u64) {
        self.stream.0.borrow().count_upload(bytes);
    }

    #[cfg(feature = "graphics-counters")]
    pub fn count_readback(&self, bytes: u64) {
        self.stream.0.borrow().count_readback(bytes);
    }
    /// Device loss or per-facade worker error, as reported by `"adv-gfx-worker"` or `"GPU owner"`.
    pub fn check(&self) -> Result<(), Error> {
        if let Some(failure) = self.execution.owner.failure.get() {
            return Err(Error::Device(failure.clone()));
        }
        if let Some(failure) = self.device.lost() {
            return Err(Error::Device(failure));
        }
        if let Some(failure) = self.failure.lock().unwrap().clone() {
            return Err(failure);
        }
        Ok(())
    }
}

use std::sync::Arc;
struct Execution {
    device: adv_gpu::Device,
    stream: stream::ExecutionStream,
    // Drop order: `worker` closes and joins `"adv-gfx-worker"` (dropping
    // `stream::Producer`) BEFORE `owner` joins `"GPU owner"`.
    worker: record::WorkerHandle,
    owner: owner::Owner,
    uniforms: uniform::Pool,
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
        let gpu =
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
        gpu.execution
            .worker
            .push_present(record::WorkerItem::PresentSoftware {
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
