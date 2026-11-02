//! Vulkan resources, shaders, pipelines and ordered submission. Ownership stays
//! behind reference-counted handles.

use ash::vk;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{
    Allocation, AllocationCreateDesc, AllocationScheme, Allocator, AllocatorCreateDesc,
};
use std::collections::{HashMap, VecDeque};
use std::ffi::CStr;
use std::sync::{Arc, Mutex, Weak};

use crate::caps::*;
use crate::encode::*;
use crate::pipeline::*;
use crate::present::*;
use crate::queue::*;
use crate::resource::*;
use crate::shader::*;
use crate::{Error, Extent, Format, Rect, Result};

mod binding;
mod encode;
mod formats;
mod open;
mod pipeline;
mod queue;
mod resource;
mod ring;
mod shader;

#[derive(Clone)]
pub(crate) struct Device(pub(crate) Arc<Context>);

pub(crate) struct Context {
    _entry: ash::Entry,
    instance: ash::Instance,
    debug: Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    physical: vk::PhysicalDevice,
    device: ash::Device,
    family: u32,
    caps: Caps,
    memory: vk::PhysicalDeviceMemoryProperties,
    allocator: Mutex<Option<Allocator>>,
    lost: Mutex<Option<String>>,
    samplers: Mutex<HashMap<SamplerDesc, Weak<SamplerCore>>>,
    rings: Mutex<Vec<Weak<ring::RingCore>>>,
    dynamic: ash::khr::dynamic_rendering::Device,
    sync2: ash::khr::synchronization2::Device,
    timeline: ash::khr::timeline_semaphore::Device,
    api: u32,
    anisotropy: bool,
    max_anisotropy: f32,
    mirror_clamp: bool,
    pipelines: Mutex<pipeline::Pipelines>,
    heap: Option<binding::Heap>,
    push: ash::khr::push_descriptor::Device,
    eds1: ash::ext::extended_dynamic_state::Device,
    eds2: ash::ext::extended_dynamic_state2::Device,
    color_write: ash::ext::color_write_enable::Device,
    cache_prefix: [u8; 28],
}

impl Context {
    fn check(&self) -> Result<()> {
        match self.lost.lock().unwrap().as_ref() {
            Some(reason) => Err(Error::DeviceLost(reason.clone())),
            None => Ok(()),
        }
    }
    fn result<T>(&self, result: std::result::Result<T, vk::Result>) -> Result<T> {
        result.map_err(|error| {
            if error == vk::Result::ERROR_DEVICE_LOST {
                let reason = "Vulkan device lost".to_owned();
                *self.lost.lock().unwrap() = Some(reason.clone());
                Error::DeviceLost(reason)
            } else {
                vk_error(error)
            }
        })
    }
    fn allocate(
        &self,
        mut requirements: vk::MemoryRequirements,
        memory: Memory,
        linear: bool,
        lazy: bool,
        name: &str,
    ) -> Result<Allocation> {
        self.check()?;
        let flags = match memory {
            Memory::Device => vk::MemoryPropertyFlags::DEVICE_LOCAL,
            Memory::Upload => {
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT
            }
            Memory::Readback => {
                vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT
                    | vk::MemoryPropertyFlags::HOST_CACHED
            }
        };
        let mask = |flags| {
            self.memory
                .memory_types_as_slice()
                .iter()
                .enumerate()
                .fold(0, |bits, (i, ty)| {
                    bits | if ty.property_flags.contains(flags) {
                        1 << i
                    } else {
                        0
                    }
                })
        };
        requirements.memory_type_bits &= mask(flags);
        if memory == Memory::Upload {
            let cached =
                requirements.memory_type_bits & mask(flags | vk::MemoryPropertyFlags::HOST_CACHED);
            if cached != 0 {
                requirements.memory_type_bits = cached;
            }
        }
        if lazy {
            let lazy_types =
                requirements.memory_type_bits & mask(vk::MemoryPropertyFlags::LAZILY_ALLOCATED);
            if lazy_types != 0 {
                requirements.memory_type_bits = lazy_types;
            }
        }
        let location = if memory == Memory::Device {
            MemoryLocation::GpuOnly
        } else {
            MemoryLocation::CpuToGpu
        };
        self.allocator
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .allocate(&AllocationCreateDesc {
                name,
                requirements,
                location,
                linear,
                allocation_scheme: AllocationScheme::GpuAllocatorManaged,
            })
            .map_err(|error| match error {
                gpu_allocator::AllocationError::OutOfMemory => Error::OutOfMemory,
                _ => Error::Unavailable(format!("memory allocation: {error}")),
            })
    }
    fn free(&self, allocation: Allocation) {
        let _ = self
            .allocator
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .free(allocation);
    }
}

fn vk_error(error: vk::Result) -> Error {
    match error {
        vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => {
            Error::OutOfMemory
        }
        vk::Result::ERROR_DEVICE_LOST => Error::DeviceLost("Vulkan device lost".into()),
        _ => Error::Unavailable(format!("Vulkan: {error:?}")),
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        self.pipelines.get_mut().unwrap().destroy(&self.device);
        self.heap.take();
        self.allocator.get_mut().unwrap().take();
        unsafe {
            self.device.destroy_device(None);
            if let Some((debug, messenger)) = &self.debug {
                debug.destroy_debug_utils_messenger(*messenger, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}

impl Device {
    pub(crate) fn caps(&self) -> &Caps {
        &self.0.caps
    }
    pub(crate) fn lost(&self) -> Option<String> {
        self.0.lost.lock().unwrap().clone()
    }
    pub(crate) fn create_present_pool(&self, _desc: &PoolDesc) -> Result<crate::PresentPool> {
        todo!("A3")
    }
}

pub(crate) use pipeline::{ComputePipeline, PipelineCache, RenderPipeline};
pub(crate) use shader::Shader;

pub(crate) struct Fence;
impl Fence {
    #[cfg(unix)]
    pub(crate) fn into_fd(self) -> Option<std::os::fd::OwnedFd> {
        todo!("A3")
    }
    #[cfg(unix)]
    pub(crate) fn from_fd(_fd: std::os::fd::OwnedFd) -> crate::queue::Fence {
        todo!("A3")
    }
}
pub(crate) struct PresentPool;
impl PresentPool {
    pub(crate) fn acquire(&mut self) -> Option<crate::PresentImage> {
        todo!("A3")
    }
    pub(crate) fn trim(&mut self, _keep: u32) {
        todo!("A3")
    }
}
pub(crate) struct PresentImage;
impl PresentImage {
    pub(crate) fn image(&self) -> &crate::Image {
        todo!("A3")
    }
    pub(crate) fn age(&self) -> Option<u32> {
        todo!("A3")
    }
    pub(crate) fn export(&self) -> Export<'_> {
        todo!("A3")
    }
    pub(crate) fn presented(&mut self, _written: Token) {
        todo!("A3")
    }
    pub(crate) fn released(self, _fence: Option<crate::queue::Fence>) {
        todo!("A3")
    }
}

pub(crate) use encode::{ComputePass, Encoder, RenderPass};
pub(crate) use queue::Queue;
pub(crate) use resource::{Buffer, Image, ImageView, QuerySet, Sampler, SamplerCore};
pub(crate) use ring::{Ring, Span};

#[cfg(test)]
mod tests;
