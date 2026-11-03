//! Android gralloc imports and sync-fd payloads. Vulkan retains an imported
//! AHardwareBuffer until memory retirement; the pool owns its export reference.

use super::resource::{ImageAllocation, ImageCore, ImageState};
use super::*;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::ptr::NonNull;

pub(super) const FORMAT: Format = Format::Rgba8Unorm;
const QUALCOMM: u32 = 0x5143;
const ARM: u32 = 0x13b5;
const CPU_WRITE_OFTEN: u64 = 3 << 4;
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
    fn new(width: u32, height: u32, vendor: u32, yuv: bool) -> Self {
        Self {
            width,
            height,
            layers: 1,
            format: if yuv { 0x11 } else { 1 }, // YCrCb_420_SP (NV21) / R8G8B8A8_UNORM
            usage: GPU_SAMPLED_IMAGE
                | COMPOSER_OVERLAY
                | if yuv {
                    CPU_WRITE_OFTEN
                } else {
                    GPU_FRAMEBUFFER | if vendor == QUALCOMM { VENDOR_0 } else { 0 }
                },
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

#[repr(C)]
#[derive(Clone, Copy)]
struct HardwarePlane {
    data: *mut std::ffi::c_void,
    pixel_stride: u32,
    row_stride: u32,
}
#[repr(C)]
struct HardwarePlanes {
    count: u32,
    planes: [HardwarePlane; 4],
}
#[link(name = "android")]
unsafe extern "C" {
    fn AHardwareBuffer_lockPlanes(
        buffer: *mut vk::AHardwareBuffer,
        usage: u64,
        fence: i32,
        rect: *const std::ffi::c_void,
        planes: *mut HardwarePlanes,
    ) -> i32;
    fn AHardwareBuffer_unlock(buffer: *mut vk::AHardwareBuffer, fence: *mut i32) -> i32;
    fn AHardwareBuffer_describe(buffer: *mut vk::AHardwareBuffer, desc: *mut HardwareBufferDesc);
    fn poll(fds: *mut PollFd, count: usize, timeout: i32) -> i32;
}

struct HardwareBuffer(NonNull<vk::AHardwareBuffer>);
// SAFETY: AHardwareBuffer references are thread-safe; access to their pixels
// is ordered by queue-family transfers and compositor fences.
unsafe impl Send for HardwareBuffer {}
unsafe impl Sync for HardwareBuffer {}
impl HardwareBuffer {
    fn allocate(desc: &HardwareBufferDesc) -> Result<Self> {
        let mut pointer = std::ptr::null_mut();
        match unsafe { AHardwareBuffer_allocate(desc, &mut pointer) } {
            0 => Ok(Self(
                NonNull::new(pointer).expect("allocated hardware buffer"),
            )),
            -12 => Err(Error::OutOfMemory),
            error => Err(Error::Unavailable(format!(
                "AHardwareBuffer allocation: {error}"
            ))),
        }
    }
}
impl Drop for HardwareBuffer {
    fn drop(&mut self) {
        unsafe { AHardwareBuffer_release(self.0.as_ptr()) };
    }
}
pub(crate) struct PresentBuffer {
    buffer: HardwareBuffer,
    video: Option<super::video::VideoSet>,
}
impl PresentBuffer {
    /// The pool has validated format and size.
    pub(crate) fn new(
        device: &Device,
        desc: &PoolDesc,
        video: Option<&Arc<VideoPool>>,
    ) -> Result<(crate::Image, Self)> {
        let context = &device.0;
        let (handle, memory, buffer, format) = allocate(
            &context.device,
            &context.ahb,
            &HardwareBufferDesc::new(
                desc.width,
                desc.height,
                context.vendor,
                desc.format == Format::Nv21,
            ),
        )?;
        if let Some(format) = format
            && format.external != context.video_format.unwrap().external
        {
            unsafe {
                context.device.destroy_image(handle, None);
                context.device.free_memory(memory, None);
            }
            return Err(Error::Unavailable("NV21 external format changed".into()));
        }
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
                usage: if desc.format == Format::Nv21 {
                    Usage::SAMPLED
                } else {
                    Usage::COLOR | Usage::SAMPLED | Usage::COPY_SRC | Usage::COPY_DST
                },
                view_formats: &[],
                label: desc.label,
            },
            usage: if desc.format == Format::Nv21 {
                vk::ImageUsageFlags::SAMPLED
            } else {
                USAGE
            },
            conversion: video.map(|pool| pool.resources.conversion),
            video: video.cloned(),
            allocation: Some(ImageAllocation::Present(memory)),
            states: Mutex::new(vec![ImageState {
                layout: if desc.format == Format::Nv21 {
                    vk::ImageLayout::GENERAL
                } else {
                    vk::ImageLayout::UNDEFINED
                },
                family: vk::QUEUE_FAMILY_FOREIGN_EXT,
                ..ImageState::default()
            }]),
        })?;
        let video = video
            .map(|pool| pool.allocate(image.view().inner.0.handle))
            .transpose()?;
        Ok((image, Self { buffer, video }))
    }
    pub(crate) fn write(&self, f: impl FnOnce(&mut [PlaneMut<'_>])) {
        let buffer = self.buffer.0.as_ptr();
        let mut desc = HardwareBufferDesc::default();
        let mut mapped = HardwarePlanes {
            count: 0,
            planes: [HardwarePlane {
                data: std::ptr::null_mut(),
                pixel_stride: 0,
                row_stride: 0,
            }; 4],
        };
        unsafe {
            AHardwareBuffer_describe(buffer, &mut desc);
            assert_eq!(
                AHardwareBuffer_lockPlanes(
                    buffer,
                    CPU_WRITE_OFTEN,
                    -1,
                    std::ptr::null(),
                    &mut mapped
                ),
                0,
                "lock YUV planes"
            );
        }
        struct Unlock(*mut vk::AHardwareBuffer);
        impl Drop for Unlock {
            fn drop(&mut self) {
                unsafe {
                    assert_eq!(AHardwareBuffer_unlock(self.0, std::ptr::null_mut()), 0);
                }
            }
        }
        let _unlock = Unlock(buffer);
        assert_eq!(mapped.count, 3, "YUV lock plane count");
        let [y, v, u, _] = mapped.planes;
        assert!(
            !y.data.is_null()
                && !v.data.is_null()
                && y.row_stride >= desc.width
                && u.row_stride >= desc.width.div_ceil(2) * 2
                && y.pixel_stride == 1
                && u.pixel_stride == 2
                && v.pixel_stride == 2
                && u.row_stride == v.row_stride
                && u.data as usize == v.data as usize + 1,
            "NV21 hardware buffer layout"
        );
        let mut planes = unsafe {
            [
                PlaneMut {
                    data: std::slice::from_raw_parts_mut(
                        y.data.cast(),
                        (desc.height - 1) as usize * y.row_stride as usize + desc.width as usize,
                    ),
                    stride: y.row_stride as usize,
                },
                PlaneMut {
                    data: std::slice::from_raw_parts_mut(
                        v.data.cast(),
                        (desc.height.div_ceil(2) - 1) as usize * u.row_stride as usize
                            + desc.width.div_ceil(2) as usize * 2,
                    ),
                    stride: u.row_stride as usize,
                },
            ]
        };
        f(&mut planes);
    }
    pub(crate) fn video_set(&self) -> (&Arc<VideoPool>, vk::DescriptorSet) {
        let video = self.video.as_ref().expect("NV21 video buffer");
        (&video.pool, video.handle)
    }
    pub(crate) fn export(&self) -> Export<'_> {
        Export::AndroidHardwareBuffer(self.buffer.0.cast(), std::marker::PhantomData)
    }
}

fn allocate(
    device: &ash::Device,
    ahb: &ash::android::external_memory_android_hardware_buffer::Device,
    requested: &HardwareBufferDesc,
) -> Result<(
    vk::Image,
    vk::DeviceMemory,
    HardwareBuffer,
    Option<super::video::VideoFormat>,
)> {
    let buffer = HardwareBuffer::allocate(requested)?;
    let pointer = buffer.0.as_ptr();
    // SAFETY: the live AHardwareBuffer and Vulkan handles outlive every call.
    unsafe {
        let mut format = vk::AndroidHardwareBufferFormatPropertiesANDROID::default();
        let mut properties =
            vk::AndroidHardwareBufferPropertiesANDROID::default().push_next(&mut format);
        ahb.get_android_hardware_buffer_properties(pointer, &mut properties)
            .map_err(vk_error)?;
        let (allocation_size, memory_type_bits) =
            (properties.allocation_size, properties.memory_type_bits);
        let yuv = requested.format == 0x11;
        if yuv
            && (format.format != vk::Format::UNDEFINED
                || format.external_format == 0
                || !format
                    .format_features
                    .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE))
        {
            return Err(Error::Unavailable(
                "AHardwareBuffer has no sampleable NV21 external format".into(),
            ));
        }
        let mut external = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::ANDROID_HARDWARE_BUFFER_ANDROID);
        let mut external_format = vk::ExternalFormatANDROID::default().external_format(if yuv {
            format.external_format
        } else {
            0
        });
        let handle = device
            .create_image(
                &vk::ImageCreateInfo::default()
                    .push_next(&mut external)
                    .push_next(&mut external_format)
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
                    .usage(if yuv {
                        vk::ImageUsageFlags::SAMPLED
                    } else {
                        USAGE
                    }),
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
        Ok((
            handle,
            memory,
            buffer,
            yuv.then_some(super::video::VideoFormat {
                external: format.external_format,
                features: format.format_features,
                x: format.suggested_x_chroma_offset,
                y: format.suggested_y_chroma_offset,
            }),
        ))
    }
}

/// Presentable caps of [`FORMAT`]. Compression follows the vendor:
/// `VK_EXT_image_compression_control` reports lossless and uncompressed
/// images alike as `DEFAULT`, so no query proves it; `present_cost` does.
pub(super) fn capabilities(vendor: u32) -> FormatCaps {
    if unsafe { AHardwareBuffer_isSupported(&HardwareBufferDesc::new(64, 64, vendor, false)) } == 0
    {
        FormatCaps::empty()
    } else if matches!(vendor, QUALCOMM | ARM) {
        FormatCaps::PRESENTABLE | FormatCaps::PRESENTABLE_COMPRESSED
    } else {
        FormatCaps::PRESENTABLE
    }
}

pub(super) fn yuv_capabilities(
    device: &ash::Device,
    ahb: &ash::android::external_memory_android_hardware_buffer::Device,
) -> Result<Option<super::video::VideoFormat>> {
    let requested = HardwareBufferDesc::new(64, 64, 0, true);
    if unsafe { AHardwareBuffer_isSupported(&requested) } == 0 {
        return Ok(None);
    }
    let (image, memory, _buffer, format) = match allocate(device, ahb, &requested) {
        Ok(imported) => imported,
        Err(Error::Unavailable(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let format = format.unwrap();
    let conversion = format.conversion(device, YuvColor::Bt601Limited);
    unsafe {
        device.destroy_image(image, None);
        device.free_memory(memory, None);
    }
    match conversion {
        Ok(conversion) => {
            unsafe { device.destroy_sampler_ycbcr_conversion(conversion, None) };
            Ok(Some(format))
        }
        Err(Error::Unavailable(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}
impl Fence {
    pub(crate) fn wait_cpu(self) {
        let mut fd = PollFd {
            fd: self.0.as_raw_fd(),
            events: 1,
            revents: 0,
        };
        assert_eq!(
            unsafe { poll(&mut fd, 1, -1) },
            1,
            "compositor release wait"
        );
        assert_eq!(fd.revents, 1, "compositor release fence");
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
