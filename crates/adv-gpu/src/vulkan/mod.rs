//! Vulkan resources, shaders, pipelines and ordered submission. Ownership stays
//! behind reference-counted handles. Android presentable images import GPU and
//! composer-only AHardwareBuffers; submission transfers ownership to FOREIGN
//! and exchanges sync fds through pooled binary semaphores.

use ash::vk;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{
    Allocation, AllocationCreateDesc, AllocationScheme, Allocator, AllocatorCreateDesc,
};
use std::collections::{HashMap, VecDeque};
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
#[cfg(target_os = "android")]
mod present;
mod queue;
mod resource;
mod ring;
mod shader;
mod video;
pub(crate) use video::VideoPool;

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
    /// `nonCoherentAtomSize`, the granularity of flushes and invalidations.
    atom: u64,
    allocator: Mutex<Option<Allocator>>,
    lost: AtomicBool,
    /// The token the next submission takes, and the encoders still open:
    /// each may submit a use of an object dropped now.
    next: AtomicU64,
    open: AtomicU64,
    retired: Mutex<VecDeque<(u64, Retired)>>,
    passes: Mutex<HashMap<encode::PassKey, encode::LegacyPass>>,
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
    /// Set when extended dynamic state is in use.
    eds: Option<Eds>,
    cache_prefix: [u8; 28],
    #[cfg(target_os = "android")]
    ahb: ash::android::external_memory_android_hardware_buffer::Device,
    #[cfg(target_os = "android")]
    semaphore_fd: ash::khr::external_semaphore_fd::Device,
    #[cfg(target_os = "android")]
    vendor: u32,
    #[cfg(target_os = "android")]
    video_format: Option<video::VideoFormat>,
}

/// A GPU object whose last handle dropped. Encoders hold no references:
/// destruction waits for the submission that may have used it instead.
pub(super) enum Retired {
    Buffer {
        handle: vk::Buffer,
        allocation: Allocation,
        storage: Option<u32>,
        uniforms: Vec<vk::DescriptorSet>,
    },
    Image {
        handle: vk::Image,
        allocation: resource::ImageAllocation,
        conversion: Option<vk::SamplerYcbcrConversion>,
    },
    View {
        handle: vk::ImageView,
        sampled: Option<u32>,
        storage: Option<u32>,
    },
    Sampler {
        handle: vk::Sampler,
        index: Option<u32>,
    },
    Video(video::Resources),
    #[cfg(target_os = "android")]
    VideoDescriptors(vk::DescriptorPool),
    Query(vk::QueryPool),
    #[cfg(target_os = "android")]
    Semaphore(vk::Semaphore),
}

/// Extended dynamic state entry points: core on 1.3, else the extensions.
pub(super) struct Eds {
    cull: vk::PFN_vkCmdSetCullMode,
    front: vk::PFN_vkCmdSetFrontFace,
    topology: vk::PFN_vkCmdSetPrimitiveTopology,
    depth_test: vk::PFN_vkCmdSetDepthTestEnable,
    depth_write: vk::PFN_vkCmdSetDepthWriteEnable,
    depth_compare: vk::PFN_vkCmdSetDepthCompareOp,
    stencil_test: vk::PFN_vkCmdSetStencilTestEnable,
    stencil_op: vk::PFN_vkCmdSetStencilOp,
    depth_bias: vk::PFN_vkCmdSetDepthBiasEnable,
    primitive_restart: vk::PFN_vkCmdSetPrimitiveRestartEnable,
    rasterizer_discard: vk::PFN_vkCmdSetRasterizerDiscardEnable,
}
impl Eds {
    fn load(instance: &ash::Instance, device: &ash::Device, api: u32) -> Self {
        if api >= vk::API_VERSION_1_3 {
            let f = device.fp_v1_3();
            return Self {
                cull: f.cmd_set_cull_mode,
                front: f.cmd_set_front_face,
                topology: f.cmd_set_primitive_topology,
                depth_test: f.cmd_set_depth_test_enable,
                depth_write: f.cmd_set_depth_write_enable,
                depth_compare: f.cmd_set_depth_compare_op,
                stencil_test: f.cmd_set_stencil_test_enable,
                stencil_op: f.cmd_set_stencil_op,
                depth_bias: f.cmd_set_depth_bias_enable,
                primitive_restart: f.cmd_set_primitive_restart_enable,
                rasterizer_discard: f.cmd_set_rasterizer_discard_enable,
            };
        }
        let one = ash::ext::extended_dynamic_state::Device::new(instance, device);
        let two = ash::ext::extended_dynamic_state2::Device::new(instance, device);
        let (one, two) = (one.fp(), two.fp());
        Self {
            cull: one.cmd_set_cull_mode_ext,
            front: one.cmd_set_front_face_ext,
            topology: one.cmd_set_primitive_topology_ext,
            depth_test: one.cmd_set_depth_test_enable_ext,
            depth_write: one.cmd_set_depth_write_enable_ext,
            depth_compare: one.cmd_set_depth_compare_op_ext,
            stencil_test: one.cmd_set_stencil_test_enable_ext,
            stencil_op: one.cmd_set_stencil_op_ext,
            depth_bias: two.cmd_set_depth_bias_enable_ext,
            primitive_restart: two.cmd_set_primitive_restart_enable_ext,
            rasterizer_discard: two.cmd_set_rasterizer_discard_enable_ext,
        }
    }
}

const LOST: &str = "Vulkan device lost";

impl Context {
    /// Device loss is reported by submit, wait and poll only.
    fn check(&self) -> Result<()> {
        if self.lost.load(Ordering::Relaxed) {
            return Err(Error::DeviceLost(LOST.into()));
        }
        Ok(())
    }
    fn result<T>(&self, result: std::result::Result<T, vk::Result>) -> Result<T> {
        result.map_err(|error| {
            if error == vk::Result::ERROR_DEVICE_LOST {
                self.lost.store(true, Ordering::Relaxed);
            }
            vk_error(error)
        })
    }
    pub(super) fn retire(&self, item: Retired) {
        // Submit advances `next` before closing its encoder, so reading
        // `open` first never misses that encoder's token.
        let open = self.open.load(Ordering::Acquire);
        let token = self.next.load(Ordering::Acquire) - 1 + open;
        let mut retired = self.retired.lock().unwrap();
        let token = retired.back().map_or(token, |(last, _)| token.max(*last));
        retired.push_back((token, item));
    }
    /// Destroys objects whose last possible use has completed. Holding the
    /// lock throughout means a poll that returns has destroyed them, whichever
    /// thread reaped; destruction drops no handles, so it never retires.
    pub(super) fn reap(&self, completed: u64) {
        let mut retired = self.retired.lock().unwrap();
        while retired
            .front()
            .is_some_and(|(token, _)| *token <= completed)
        {
            let (_, item) = retired.pop_front().unwrap();
            self.destroy(item);
        }
    }
    fn destroy(&self, item: Retired) {
        let heap = self.heap.as_ref();
        unsafe {
            match item {
                Retired::Buffer {
                    handle,
                    allocation,
                    storage,
                    uniforms,
                } => {
                    if let Some(heap) = heap {
                        for set in uniforms {
                            heap.free_uniform(self, set);
                        }
                        if let Some(index) = storage {
                            heap.free(3, index);
                        }
                    }
                    self.device.destroy_buffer(handle, None);
                    self.free(allocation);
                }
                Retired::Image {
                    handle,
                    allocation,
                    conversion,
                } => {
                    self.device.destroy_image(handle, None);
                    if let Some(conversion) = conversion {
                        self.device
                            .destroy_sampler_ycbcr_conversion(conversion, None);
                    }
                    match allocation {
                        resource::ImageAllocation::Private(allocation) => self.free(allocation),
                        #[cfg(target_os = "android")]
                        resource::ImageAllocation::Present(memory) => {
                            self.device.free_memory(memory, None);
                        }
                    }
                }
                Retired::View {
                    handle,
                    sampled,
                    storage,
                } => {
                    if let Some(heap) = heap {
                        for (kind, index) in [(0, sampled), (2, storage)] {
                            if let Some(index) = index {
                                heap.free(kind, index);
                            }
                        }
                    }
                    self.passes.lock().unwrap().retain(|key, pass| {
                        let used = key.uses(handle);
                        if used {
                            self.device.destroy_framebuffer(pass.framebuffer, None);
                            self.device.destroy_render_pass(pass.pass, None);
                        }
                        !used
                    });
                    self.device.destroy_image_view(handle, None);
                }
                Retired::Sampler { handle, index } => {
                    if let (Some(heap), Some(index)) = (heap, index) {
                        heap.free(1, index);
                    }
                    self.device.destroy_sampler(handle, None);
                }
                Retired::Video(resources) => resources.destroy(&self.device),
                #[cfg(target_os = "android")]
                Retired::VideoDescriptors(pool) => self.device.destroy_descriptor_pool(pool, None),
                Retired::Query(handle) => self.device.destroy_query_pool(handle, None),
                #[cfg(target_os = "android")]
                Retired::Semaphore(handle) => self.device.destroy_semaphore(handle, None),
            }
        }
    }
    fn allocate(
        &self,
        mut requirements: vk::MemoryRequirements,
        memory: Memory,
        linear: bool,
        lazy: bool,
        name: &str,
    ) -> Result<Allocation> {
        // Upload memory is never flushed, so it must be coherent. Memory the
        // CPU reads wants a cached type, flushed or invalidated where it is
        // not coherent; the first preference outranks the second. The type
        // bits decide alone: the allocator's host locations demand coherence.
        let visible = vk::MemoryPropertyFlags::HOST_VISIBLE;
        let coherent = vk::MemoryPropertyFlags::HOST_COHERENT;
        let cached = vk::MemoryPropertyFlags::HOST_CACHED;
        let (required, preferred, location) = match memory {
            Memory::Device => (
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
                [vk::MemoryPropertyFlags::DEVICE_LOCAL; 2],
                MemoryLocation::GpuOnly,
            ),
            Memory::Upload => (visible | coherent, [cached; 2], MemoryLocation::Unknown),
            Memory::Shared | Memory::Readback => {
                (visible, [cached, coherent], MemoryLocation::Unknown)
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
        requirements.memory_type_bits &= mask(required);
        // Flushed and invalidated ranges round out to atoms, so host blocks,
        // including one sized to a single large allocation, are atom multiples.
        if memory != Memory::Device {
            requirements.size = requirements.size.next_multiple_of(self.atom);
        }
        let mut prefer = |flags| {
            let types = requirements.memory_type_bits & mask(flags);
            if types != 0 {
                requirements.memory_type_bits = types;
            }
        };
        for flags in preferred {
            prefer(flags);
        }
        if lazy {
            prefer(vk::MemoryPropertyFlags::LAZILY_ALLOCATED);
        }
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
        vk::Result::ERROR_DEVICE_LOST => Error::DeviceLost(LOST.into()),
        _ => Error::Unavailable(format!("Vulkan: {error:?}")),
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // Every handle is gone, so nothing records; wait for the GPU.
        let _ = unsafe { self.device.device_wait_idle() };
        self.reap(u64::MAX);
        for (_, pass) in self.passes.get_mut().unwrap().drain() {
            unsafe {
                self.device.destroy_framebuffer(pass.framebuffer, None);
                self.device.destroy_render_pass(pass.pass, None);
            }
        }
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
        self.0.lost.load(Ordering::Relaxed).then(|| LOST.into())
    }
}

pub(crate) use pipeline::{ComputePipeline, PipelineCache, RenderPipeline};
pub(crate) use shader::Shader;

pub(crate) use encode::{ComputePass, Encoder, RenderPass};
pub(crate) use queue::Queue;
pub(crate) use resource::{Buffer, Image, ImageView, QuerySet, Sampler, SamplerCore};
pub(crate) use ring::{Ring, Span};

/// A sync file descriptor from or for the system compositor.
#[cfg(target_os = "android")]
pub(crate) struct Fence(pub(crate) std::os::fd::OwnedFd);
#[cfg(target_os = "android")]
impl Fence {
    pub(crate) fn duplicate(&self) -> Result<Self> {
        self.0
            .try_clone()
            .map(Self)
            .map_err(|e| Error::Unavailable(format!("sync fd: {e}")))
    }
}
/// No compositor fences exist without presentable buffers.
#[cfg(not(target_os = "android"))]
pub(crate) enum Fence {}
#[cfg(not(target_os = "android"))]
impl Fence {
    pub(crate) fn wait_cpu(self) {
        match self {}
    }
    pub(crate) fn duplicate(&self) -> Result<Self> {
        match *self {}
    }
}

#[cfg(target_os = "android")]
pub(crate) use present::PresentBuffer;
/// Other Vulkan targets have no presentable formats.
#[cfg(not(target_os = "android"))]
pub(crate) struct PresentBuffer;
#[cfg(not(target_os = "android"))]
impl PresentBuffer {
    pub(crate) fn new(_device: &Device, _desc: &PoolDesc) -> Result<(crate::Image, Self)> {
        unreachable!("no presentable Vulkan formats on this target")
    }
    pub(crate) fn write(&self, _f: impl FnOnce(&mut [PlaneMut<'_>])) {
        unreachable!("no presentable Vulkan formats on this target")
    }
    pub(crate) fn export(&self) -> Export<'_> {
        unreachable!("no presentable Vulkan formats on this target")
    }
}

#[cfg(test)]
mod tests;
