//! Android gralloc imports and sync-fd payloads. Vulkan retains an imported
//! AHardwareBuffer until memory retirement; the pool owns its export reference.

use super::resource::{ImageAllocation, ImageCore, ImageState};
use super::*;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::ptr::NonNull;

pub(super) const FORMAT: Format = Format::Rgba8Unorm;
const QUALCOMM: u32 = 0x5143;
const ARM: u32 = 0x13b5;
const GPU_SAMPLED_IMAGE: u64 = 1 << 8;
const GPU_FRAMEBUFFER: u64 = 1 << 9;
const COMPOSER_OVERLAY: u64 = 1 << 11;
const VENDOR_0: u64 = 1 << 28;
const USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
    vk::ImageUsageFlags::COLOR_ATTACHMENT.as_raw()
        | vk::ImageUsageFlags::SAMPLED.as_raw()
        | vk::ImageUsageFlags::TRANSFER_SRC.as_raw()
        | vk::ImageUsageFlags::TRANSFER_DST.as_raw(),
);

#[repr(C)]
#[derive(Default)]
struct HardwareBufferDesc {
    width: u32,
    height: u32,
    layers: u32,
    format: u32,
    usage: u64,
    stride: u32,
    rfu0: u32,
    rfu1: u64,
}
impl HardwareBufferDesc {
    fn new(width: u32, height: u32, vendor: u32) -> Self {
        Self {
            width,
            height,
            layers: 1,
            format: 1, // AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM
            usage: GPU_SAMPLED_IMAGE
                | GPU_FRAMEBUFFER
                | COMPOSER_OVERLAY
                | if vendor == QUALCOMM { VENDOR_0 } else { 0 },
            ..Self::default()
        }
    }
}
#[link(name = "android")]
unsafe extern "C" {
    fn AHardwareBuffer_allocate(
        desc: *const HardwareBufferDesc,
        out: *mut *mut vk::AHardwareBuffer,
    ) -> i32;
    fn AHardwareBuffer_release(buffer: *mut vk::AHardwareBuffer);
    fn AHardwareBuffer_isSupported(desc: *const HardwareBufferDesc) -> i32;
}

struct HardwareBuffer(NonNull<vk::AHardwareBuffer>);
// SAFETY: AHardwareBuffer references are thread-safe; access to their pixels
// is ordered by queue-family transfers and compositor fences.
unsafe impl Send for HardwareBuffer {}
unsafe impl Sync for HardwareBuffer {}
impl Drop for HardwareBuffer {
    fn drop(&mut self) {
        unsafe { AHardwareBuffer_release(self.0.as_ptr()) };
    }
}
pub(crate) struct PresentBuffer(HardwareBuffer);
impl PresentBuffer {
    /// The pool has validated format and size.
    pub(crate) fn new(device: &Device, desc: &PoolDesc) -> Result<(crate::Image, Self)> {
        let context = &device.0;
        let (handle, memory, buffer) = allocate(
            &context.device,
            &context.ahb,
            &HardwareBufferDesc::new(desc.width, desc.height, context.vendor),
        )?;
        let image = Image::wrap(ImageCore {
            context: context.clone(),
            handle,
            desc: ImageDesc {
                dimension: Dimension::D2,
                size: Extent {
                    width: desc.width,
                    height: desc.height,
                    depth_or_layers: 1,
                },
                format: desc.format,
                mips: 1,
                samples: 1,
                usage: Usage::COLOR | Usage::SAMPLED | Usage::COPY_SRC | Usage::COPY_DST,
                view_formats: &[],
                label: desc.label,
            },
            usage: USAGE,
            conversion: None,
            allocation: Some(ImageAllocation::Present(memory)),
            states: Mutex::new(vec![ImageState {
                family: vk::QUEUE_FAMILY_FOREIGN_EXT,
                ..ImageState::default()
            }]),
        })?;
        Ok((image, Self(buffer)))
    }
    pub(crate) fn export(&self) -> Export<'_> {
        Export::AndroidHardwareBuffer(self.0.0.cast(), std::marker::PhantomData)
    }
}

fn allocate(
    device: &ash::Device,
    ahb: &ash::android::external_memory_android_hardware_buffer::Device,
    requested: &HardwareBufferDesc,
) -> Result<(vk::Image, vk::DeviceMemory, HardwareBuffer)> {
    let mut pointer = std::ptr::null_mut();
    // SAFETY: NDK descriptions have the C layout; all handles and pNext chains
    // live through their calls. Partial imports release every acquired object.
    unsafe {
        match AHardwareBuffer_allocate(requested, &mut pointer) {
            0 => {}
            -12 => return Err(Error::OutOfMemory), // -ENOMEM
            error => {
                return Err(Error::Unavailable(format!(
                    "AHardwareBuffer allocation: {error}"
                )));
            }
        }
        let buffer = HardwareBuffer(NonNull::new_unchecked(pointer));
        let mut format = vk::AndroidHardwareBufferFormatPropertiesANDROID::default();
        let mut properties =
            vk::AndroidHardwareBufferPropertiesANDROID::default().push_next(&mut format);
        ahb.get_android_hardware_buffer_properties(pointer, &mut properties)
            .map_err(vk_error)?;
        let (allocation_size, memory_type_bits) =
            (properties.allocation_size, properties.memory_type_bits);
        let mut external = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::ANDROID_HARDWARE_BUFFER_ANDROID);
        let handle = device
            .create_image(
                &vk::ImageCreateInfo::default()
                    .push_next(&mut external)
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format.format)
                    .extent(vk::Extent3D {
                        width: requested.width,
                        height: requested.height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(USAGE),
                None,
            )
            .map_err(vk_error)?;
        let mut import = vk::ImportAndroidHardwareBufferInfoANDROID::default().buffer(pointer);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(handle);
        let memory = match device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .push_next(&mut import)
                .push_next(&mut dedicated)
                .allocation_size(allocation_size)
                .memory_type_index(memory_type_bits.trailing_zeros()),
            None,
        ) {
            Ok(memory) => memory,
            Err(error) => {
                device.destroy_image(handle, None);
                return Err(vk_error(error));
            }
        };
        if let Err(error) = device.bind_image_memory(handle, memory, 0) {
            device.destroy_image(handle, None);
            device.free_memory(memory, None);
            return Err(vk_error(error));
        }
        Ok((handle, memory, buffer))
    }
}

/// Presentable caps of [`FORMAT`]. Compression follows the vendor:
/// `VK_EXT_image_compression_control` reports lossless and uncompressed
/// images alike as `DEFAULT`, so no query proves it; `present_cost` does.
pub(super) fn capabilities(vendor: u32) -> FormatCaps {
    if unsafe { AHardwareBuffer_isSupported(&HardwareBufferDesc::new(64, 64, vendor)) } == 0 {
        FormatCaps::empty()
    } else if matches!(vendor, QUALCOMM | ARM) {
        FormatCaps::PRESENTABLE | FormatCaps::PRESENTABLE_COMPRESSED
    } else {
        FormatCaps::PRESENTABLE
    }
}

/// Permanent payloads are unsignaled: waits consume temporary imports and
/// SYNC_FD exports consume signals. Only completed submissions recycle them.
#[derive(Default)]
pub(super) struct Semaphores {
    pub(super) free: Vec<vk::Semaphore>,
    pub(super) waits: Vec<vk::Semaphore>,
    pub(super) stages: Vec<vk::PipelineStageFlags>,
    retired: VecDeque<(u64, vk::Semaphore)>,
}
impl Semaphores {
    pub(super) fn semaphore(
        &mut self,
        context: &Context,
    ) -> std::result::Result<vk::Semaphore, vk::Result> {
        if let Some(semaphore) = self.free.pop() {
            return Ok(semaphore);
        }
        let mut export = vk::ExportSemaphoreCreateInfo::default()
            .handle_types(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD);
        unsafe {
            context.device.create_semaphore(
                &vk::SemaphoreCreateInfo::default().push_next(&mut export),
                None,
            )
        }
    }
    pub(super) fn wait_next(&mut self, context: &Context, fence: Fence) -> Result<()> {
        let semaphore = self.semaphore(context).map_err(vk_error)?;
        let result = unsafe {
            context.semaphore_fd.import_semaphore_fd(
                &vk::ImportSemaphoreFdInfoKHR::default()
                    .semaphore(semaphore)
                    .flags(vk::SemaphoreImportFlags::TEMPORARY)
                    .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD)
                    .fd(fence.0.as_raw_fd()),
            )
        };
        if let Err(error) = result {
            self.free.push(semaphore);
            return Err(vk_error(error));
        }
        let _ = fence.0.into_raw_fd(); // Vulkan owns it after a successful import.
        self.waits.push(semaphore);
        self.stages.push(
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT | vk::PipelineStageFlags::TRANSFER,
        );
        Ok(())
    }
    pub(super) fn submitted(&mut self, token: u64) {
        self.retired
            .extend(self.waits.drain(..).map(|s| (token, s)));
        self.stages.clear();
    }
    pub(super) fn export(
        &mut self,
        context: &Context,
        semaphore: vk::Semaphore,
        token: u64,
    ) -> Result<Option<Fence>> {
        match unsafe {
            context.semaphore_fd.get_semaphore_fd(
                &vk::SemaphoreGetFdInfoKHR::default()
                    .semaphore(semaphore)
                    .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD),
            )
        } {
            Ok(fd) => {
                self.retired.push_back((token, semaphore));
                // -1 stands for a payload that has already signaled.
                Ok((fd != -1).then(|| Fence(unsafe { OwnedFd::from_raw_fd(fd) })))
            }
            Err(error) => {
                context.retire(Retired::Semaphore(semaphore));
                Err(vk_error(error))
            }
        }
    }
    pub(super) fn retire(&mut self, completed: u64) {
        while self
            .retired
            .front()
            .is_some_and(|(token, _)| *token <= completed)
        {
            self.free.push(self.retired.pop_front().unwrap().1);
        }
    }
    pub(super) fn destroy(&mut self, context: &Context) {
        for semaphore in self
            .free
            .drain(..)
            .chain(self.waits.drain(..))
            .chain(self.retired.drain(..).map(|(_, s)| s))
        {
            unsafe { context.device.destroy_semaphore(semaphore, None) };
        }
    }
}
