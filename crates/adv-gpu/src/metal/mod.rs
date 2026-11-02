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
mod clear;
mod encode;
mod pipeline;
mod queue;
mod resource;
mod ring;
mod shader;

pub(crate) use encode::{ComputePass, Encoder, RenderPass};
pub(crate) use pipeline::{ComputePipeline, PipelineCache, RenderPipeline};
use queue::QueryData;
pub(crate) use queue::{QuerySet, Queue};
pub(crate) use resource::{Buffer, Image, ImageView, Sampler};
pub(crate) use ring::{Ring, Span};
pub(crate) use shader::Shader;

#[derive(Clone)]
pub(crate) struct Device(Arc<DeviceState>);

struct DeviceState {
    raw: Object<dyn MTLDevice>,
    caps: Caps,
    event: Object<dyn MTLSharedEvent>,
    lost: Arc<OnceLock<String>>,
    samplers: Mutex<HashMap<SamplerDesc, Sampler>>,
    rings: Mutex<Vec<Weak<Mutex<ring::State>>>>,
    clears: Mutex<HashMap<clear::Key, clear::Pipeline>>,
    pipelines: Mutex<pipeline::Pipelines>,
}

impl Device {
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
    pub(crate) fn create_present_pool(&self, _desc: &PoolDesc) -> Result<crate::PresentPool> {
        todo!("B3")
    }
}

#[derive(Clone)]
pub(crate) struct Fence;

impl Fence {
    #[cfg(unix)]
    pub(crate) fn into_fd(self) -> Option<std::os::fd::OwnedFd> {
        todo!("B3")
    }

    #[cfg(unix)]
    pub(crate) fn from_fd(_fd: std::os::fd::OwnedFd) -> crate::queue::Fence {
        todo!("B3")
    }
}

#[derive(Clone)]
pub(crate) struct PresentPool;

impl PresentPool {
    pub(crate) fn acquire(&mut self) -> Option<crate::PresentImage> {
        todo!("B3")
    }

    pub(crate) fn trim(&mut self, _keep: u32) {
        todo!("B3")
    }
}

pub(crate) struct PresentImage;

impl Drop for PresentImage {
    fn drop(&mut self) {
        todo!("B3")
    }
}

impl PresentImage {
    pub(crate) fn image(&self) -> &crate::Image {
        todo!("B3")
    }

    pub(crate) fn age(&self) -> Option<u32> {
        todo!("B3")
    }

    pub(crate) fn export(&self) -> Export<'_> {
        todo!("B3")
    }

    pub(crate) fn presented(&mut self, _written: Token) {
        todo!("B3")
    }

    pub(crate) fn released(self, _fence: Option<crate::queue::Fence>) {
        todo!("B3")
    }
}

#[cfg(test)]
mod tests;
