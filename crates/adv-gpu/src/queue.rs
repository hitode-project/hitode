//! The device, its single queue, and completion.

use crate::imp;
use crate::pipeline::{
    ComputePipeline, ComputePipelineDesc, PipelineCache, RenderPipeline, RenderPipelineDesc,
};
use crate::present::{PoolDesc, PresentPool};
use crate::resource::{BufferDesc, ImageDesc, SamplerDesc};
use crate::shader::ShaderDesc;
use crate::{Buffer, Caps, Encoder, Image, Result, Ring, Sampler, Shader};
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct DeviceOptions {
    /// A software adapter (lavapipe) for tests and CI.
    pub software: bool,
    /// Validation layers; errors fail the owning test.
    pub validation: bool,
    /// Modern mechanisms to treat as unsupported, forcing their stand-ins.
    /// CI runs lavapipe with all of them masked; reports use it to tell a
    /// driver bug from ours.
    pub disable: crate::caps::Modern,
    pub ring_size: u64,
    pub label: &'static str,
}

/// Shared handle to one GPU device. Creation calls are allowed from any
/// thread; they never encode or submit.
#[derive(Clone)]
pub struct Device {
    pub(crate) inner: crate::imp::Device,
}

/// Adapter identifiers contain model and driver data, never a registry ID or serial.
#[derive(Clone, Debug)]
pub struct AdapterIdentity {
    pub api: &'static str,
    pub name: String,
    pub vendor: String,
    pub device: String,
    pub driver: String,
    pub tier: &'static str,
}

impl Device {
    /// Open the platform's device and its queue.
    pub fn open(options: &DeviceOptions) -> Result<(Device, Queue)> {
        imp::Device::open(options)
    }
    pub fn adapter_identity(&self) -> AdapterIdentity {
        self.inner.adapter_identity()
    }
    pub fn caps(&self) -> &Caps {
        self.inner.caps()
    }
    pub fn create_image(&self, desc: &ImageDesc) -> Result<Image> {
        self.inner.create_image(desc)
    }
    pub fn create_buffer(&self, desc: &BufferDesc) -> Result<Buffer> {
        self.inner.create_buffer(desc)
    }
    pub fn sampler(&self, desc: &SamplerDesc) -> Result<Sampler> {
        self.inner.sampler(desc)
    }
    pub fn create_shader(&self, desc: ShaderDesc<'_>) -> Result<Shader> {
        self.inner.create_shader(desc)
    }
    /// Blocking compile; run it on the compile pool, not the owner thread.
    pub fn create_render_pipeline(
        &self,
        desc: &RenderPipelineDesc,
        cache: Option<&PipelineCache>,
    ) -> Result<RenderPipeline> {
        self.inner.create_render_pipeline(desc, cache)
    }
    /// Compile once for a pool's immutable video sampler, off the frame path.
    pub fn create_video_pipeline(
        &self,
        desc: &crate::pipeline::VideoPipelineDesc<'_>,
        cache: Option<&PipelineCache>,
    ) -> Result<RenderPipeline> {
        #[cfg(target_os = "android")]
        {
            self.inner.create_video_pipeline(desc, cache)
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (desc, cache);
            Err(crate::Error::Unsupported("converted video pools".into()))
        }
    }
    /// Identity of the driver object `desc` needs on this backend (see
    /// [`crate::pipeline`]). Equal identities share one compilation.
    pub fn pipeline_identity(&self, desc: &RenderPipelineDesc) -> u64 {
        self.inner.pipeline_identity(desc)
    }
    pub fn create_compute_pipeline(
        &self,
        desc: &ComputePipelineDesc,
        cache: Option<&PipelineCache>,
    ) -> Result<ComputePipeline> {
        self.inner.create_compute_pipeline(desc, cache)
    }
    /// A new upload ring for one producer thread.
    pub fn ring(&self, size: u64) -> Result<Ring> {
        self.inner.ring(size)
    }
    pub fn create_present_pool(&self, desc: &PoolDesc) -> Result<PresentPool> {
        PresentPool::new(self, desc)
    }
    pub fn create_query_set(&self, kind: QueryKind, count: u32) -> Result<QuerySet> {
        self.inner.create_query_set(kind, count)
    }
    /// Set once the device is lost; see [`crate::Error::DeviceLost`].
    pub fn lost(&self) -> Option<String> {
        self.inner.lost()
    }
}

/// Completion of one submission. Tokens are ordered: a completed token implies
/// every earlier one completed. Backed by a timeline semaphore on Vulkan
/// (a fence pool on older drivers) and an `MTLSharedEvent` on Metal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Token(pub u64);

/// The device's only queue, owned by the GPU owner thread.
pub struct Queue {
    pub(crate) inner: crate::imp::Queue,
}

impl Queue {
    pub fn device(&self) -> &Device {
        self.inner.device()
    }
    /// Begin the next encoder. Only one is open at a time.
    pub fn encoder(&mut self, label: &'static str) -> Encoder {
        self.inner.encoder(label)
    }
    /// Submit, returning the token that completes when the GPU finishes it.
    /// Never blocks on the GPU.
    pub fn submit(&mut self, encoder: Encoder) -> Result<Token> {
        self.inner.submit(encoder)
    }
    /// The latest completed token. Retires ring spans and deferred
    /// destructions up to it. Never blocks.
    pub fn poll(&mut self) -> Result<Token> {
        self.inner.poll()
    }
    /// Block until `token` completes or `timeout` passes. For readbacks the
    /// guest must wait for; never on a frame path.
    pub fn wait(&mut self, token: Token, timeout: Duration) -> Result<bool> {
        self.inner.wait(token, timeout)
    }
    /// Run `callback` once `token` completes. An already retired token may
    /// run it inline; device loss drops callbacks for unfinished work.
    pub fn on_complete(&mut self, token: Token, callback: Box<dyn FnOnce() + Send>) {
        self.inner.on_complete(token, callback)
    }
    /// Completion callbacks on the main thread, for compositors whose commits
    /// belong there (Core Animation).
    #[cfg(target_vendor = "apple")]
    pub fn main_notifier(&self) -> MainNotifier {
        MainNotifier {
            inner: self.inner.main_notifier(),
        }
    }
}

/// Runs completion callbacks on the main thread; any thread may register.
#[cfg(target_vendor = "apple")]
pub struct MainNotifier {
    inner: imp::MainNotifier,
}
#[cfg(target_vendor = "apple")]
impl MainNotifier {
    /// Run `callback` on the main thread once `token` completes.
    pub fn on_complete(&self, token: Token, callback: Box<dyn FnOnce() + Send>) {
        self.inner.on_complete(token, callback)
    }
}

/// A system compositor fence: a sync file descriptor on Android. Other
/// platforms have no compositor fences, and no value of this type exists
/// there.
pub struct Fence {
    pub(crate) inner: crate::imp::Fence,
}

impl Fence {
    #[cfg(target_os = "android")]
    pub fn into_fd(self) -> std::os::fd::OwnedFd {
        self.inner.0
    }
    #[cfg(target_os = "android")]
    pub fn from_fd(fd: std::os::fd::OwnedFd) -> Self {
        Self {
            inner: imp::Fence(fd),
        }
    }
    pub fn duplicate(&self) -> Result<Self> {
        Ok(Self {
            inner: self.inner.duplicate()?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum QueryKind {
    Occlusion,
    Timestamp,
}

pub struct QuerySet {
    pub(crate) inner: crate::imp::QuerySet,
}

impl QuerySet {
    /// Nanoseconds per timestamp tick.
    pub fn period(&self) -> f64 {
        self.inner.period()
    }
}
