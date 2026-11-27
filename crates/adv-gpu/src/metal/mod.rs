//! Metal resources and encoding on one hazard-tracked command queue.
//! Command buffers retain their Metal references; ring reservations additionally
//! remain owned until the shared event reaches their last submission.

use crate::caps::*;
use crate::encode::*;
use crate::pipeline::*;
use crate::present::*;
use crate::queue::*;
use crate::resource::*;
use crate::shader::*;
use crate::{Error, Extent, Format, Rect, Result};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::{NSRange, NSString};
use objc2_metal::*;
use std::collections::{HashMap, VecDeque};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

type Object<T> = Retained<ProtocolObject<T>>;

mod caps;
pub use caps::MetalFamily;
mod clear;
mod encode;
mod pipeline;
mod present;
mod queue;
mod resource;
mod ring;
mod shader;

pub(crate) use encode::{ComputePass, Encoder, RenderPass};
pub(crate) use pipeline::{ComputePipeline, PipelineCache, RenderPipeline};
pub(crate) use present::{Fence, PresentBuffer};
use queue::QueryData;
pub(crate) use queue::{MainNotifier, QuerySet, Queue};
pub(crate) use resource::{Buffer, Image, ImageView, Sampler};
pub(crate) use ring::{Ring, Span};
pub(crate) use shader::Shader;

#[derive(Clone)]
pub(crate) struct Device(Arc<DeviceState>);

#[cfg(feature = "counters")]
#[derive(Default)]
struct ResourceCreations {
    buffers: std::sync::atomic::AtomicU64,
    views: std::sync::atomic::AtomicU64,
}

struct DeviceState {
    #[cfg(feature = "counters")]
    creations: Arc<ResourceCreations>,
    raw: Object<dyn MTLDevice>,
    caps: Caps,
    event: Object<dyn MTLSharedEvent>,
    lost: Arc<OnceLock<String>>,
    samplers: Mutex<HashMap<SamplerDesc, Sampler>>,
    rings: Mutex<Vec<Weak<Mutex<ring::State>>>>,
    clears: Mutex<HashMap<clear::Key, clear::Pipeline>>,
    /// Metal's initial depth-stencil state; encoders cannot be reset to nil.
    default_depth: Object<dyn MTLDepthStencilState>,
    pipelines: Mutex<pipeline::Pipelines>,
}

impl Device {
    #[cfg(feature = "counters")]
    pub(crate) fn resource_creations(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.0.creations.buffers.load(Relaxed),
            self.0.creations.views.load(Relaxed),
        )
    }

    fn new_buffer(
        &self,
        length: usize,
        options: MTLResourceOptions,
    ) -> Option<Object<dyn MTLBuffer>> {
        let buffer = self.0.raw.newBufferWithLength_options(length, options)?;
        #[cfg(feature = "counters")]
        self.0
            .creations
            .buffers
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(buffer)
    }

    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn adapter_identity(&self) -> crate::queue::AdapterIdentity {
        let caps = self.caps();
        let vendor = if self.0.raw.supportsFamily(MTLGPUFamily::Apple1) {
            "apple"
        } else if caps.adapter.contains("AMD") {
            "amd"
        } else if caps.adapter.contains("Intel") {
            "intel"
        } else {
            "unknown"
        };
        crate::queue::AdapterIdentity {
            api: "metal",
            name: caps.adapter.clone(),
            vendor: vendor.into(),
            device: caps.adapter.clone(),
            driver: caps.driver.clone(),
            tier: "metal".into(),
        }
    }

    pub(crate) fn guard_compile<T>(
        &self,
        _probe: &str,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        f()
    }

    pub(crate) fn guard_guest_pipeline<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        f()
    }

    pub(crate) fn open_with<T>(
        options: &DeviceOptions,
        init: &mut dyn FnMut(crate::Device, crate::Queue) -> Result<T>,
    ) -> Result<T> {
        let (device, queue) = Self::open(options)?;
        init(device, queue)
    }

    pub(crate) fn caps(&self) -> &Caps {
        &self.0.caps
    }
    pub(crate) fn lost(&self) -> Option<String> {
        self.0.lost.get().cloned()
    }
    fn check(&self) -> Result<()> {
        match self.lost() {
            Some(reason) => Err(Error::DeviceLost(reason)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
