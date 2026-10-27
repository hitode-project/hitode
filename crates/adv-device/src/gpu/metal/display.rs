//! IOSurface render targets and completion fences for external composition.
use super::*;
use objc2_core_foundation::{CFDictionary, CFNumber};
use objc2_io_surface::{
    IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceColorSpace, kIOSurfaceHeight,
    kIOSurfacePixelFormat, kIOSurfaceWidth,
};

impl Gpu {
    pub(in crate::gpu::renderer) fn presentation_image(
        &self,
        width: u32,
        height: u32,
    ) -> Result<Image, Error> {
        self.dimensions(width, height)?;
        let Some(device) = (unsafe { self.context.device.as_hal::<wgpu::hal::api::Metal>() })
        else {
            return self.image(width, height);
        };
        let values = [width, height, 4, u32::from_be_bytes(*b"BGRA")]
            .map(|n| CFNumber::new_i64(i64::from(n)));
        let properties = CFDictionary::from_slices(
            &unsafe {
                [
                    kIOSurfaceWidth,
                    kIOSurfaceHeight,
                    kIOSurfaceBytesPerElement,
                    kIOSurfacePixelFormat,
                ]
            },
            &values.each_ref().map(|v| &**v),
        );
        let surface = unsafe { IOSurfaceRef::new(properties.as_opaque()) }
            .ok_or_else(|| Error::Device("IOSurface allocation failed".into()))?;
        let srgb = objc2_core_graphics::CGColorSpace::with_name(Some(unsafe {
            objc2_core_graphics::kCGColorSpaceSRGB
        }))
        .unwrap();
        let color_space = srgb.property_list().unwrap();
        unsafe { surface.set_value(kIOSurfaceColorSpace, &color_space) };
        let descriptor = MTLTextureDescriptor::new();
        descriptor.setTextureType(MTLTextureType::Type2D);
        descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        unsafe {
            descriptor.setWidth(width as usize);
            descriptor.setHeight(height as usize);
        }
        descriptor.setStorageMode(MTLStorageMode::Shared);
        descriptor.setUsage(
            MTLTextureUsage::ShaderRead
                | MTLTextureUsage::RenderTarget
                | MTLTextureUsage::PixelFormatView,
        );
        let raw = device
            .raw_device()
            .newTextureWithDescriptor_iosurface_plane(&descriptor, &surface, 0)
            .ok_or_else(|| Error::Device("IOSurface Metal texture allocation failed".into()))?;
        let texture = unsafe {
            let raw = wgpu::hal::metal::Device::texture_from_raw(
                raw,
                FORMAT,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width,
                    height,
                    depth: 1,
                },
                None,
            );
            self.context
                .device
                .create_texture_from_hal::<wgpu::hal::api::Metal>(
                    raw,
                    &wgpu::TextureDescriptor {
                        label: Some("adv shared display target"),
                        size: extent(width, height),
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: FORMAT,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_SRC
                            | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    },
                    wgpu::TextureUses::UNINITIALIZED,
                )
        };
        let mut image = Self::wrap_image(&self.context, texture, width, height, 1);
        image.metal.surface = Some(surface);
        self.check()?;
        Ok(image)
    }
}
impl NativeFrame {
    /// The frame pin must outlive external use of this surface.
    pub fn io_surface(&self) -> Option<&IOSurfaceRef> {
        self.image.metal.surface.as_deref()
    }
    pub fn display_completion(
        &self,
        wake: adv_sync::WakeEvent,
    ) -> Result<DisplayCompletion, Error> {
        if let Some(completion) = &self.completion {
            completion.0.watch(wake);
            return Ok(completion.clone());
        }
        self.context.submission()?.display_completion(wake)
    }
}

#[derive(Default)]
pub(super) struct Completion {
    ready: AtomicBool,
    wakes: Mutex<Vec<adv_sync::WakeEvent>>,
}
impl Completion {
    pub(super) fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
    fn watch(&self, wake: adv_sync::WakeEvent) {
        let mut wakes = self.wakes.lock().unwrap();
        if !self.ready() {
            wakes.push(wake);
        }
    }
    pub(super) fn complete(&self) {
        let mut wakes = self.wakes.lock().unwrap();
        self.ready.store(true, Ordering::Release);
        for wake in wakes.drain(..) {
            wake.signal();
        }
    }
}
#[derive(Clone)]
pub struct DisplayCompletion(Arc<Completion>, NativeContext);
impl DisplayCompletion {
    pub fn ready(&self) -> Result<bool, Error> {
        let ready = self.0.ready();
        self.1.check()?;
        Ok(ready)
    }
}
impl Submission<'_> {
    pub(in crate::gpu::renderer) fn native_completion(&self) -> Option<DisplayCompletion> {
        self.context
            .pending
            .lock()
            .unwrap()
            .metal
            .as_ref()?
            .completion
            .as_ref()
            .map(|completion| DisplayCompletion(completion.clone(), self.context.clone()))
    }
    /// Enqueue a completion notification after all producer writes, without
    /// waiting on either the execution, presentation-worker, or UI thread.
    pub fn display_completion(
        &self,
        wake: adv_sync::WakeEvent,
    ) -> Result<DisplayCompletion, Error> {
        self.flush();
        self.context.queue.submit([]);
        let queue = unsafe { self.context.queue.as_hal::<wgpu::hal::api::Metal>() }.unwrap();
        let command = queue
            .as_raw()
            .commandBuffer()
            .ok_or_else(|| Error::Device("Metal display fence allocation failed".into()))?;
        let ready = Arc::new(Completion::default());
        ready.watch(wake);
        let done = ready.clone();
        let failure = self.context.failure.clone();
        let callback = block2::RcBlock::new(
            move |command: std::ptr::NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                if let Some(error) = unsafe { command.as_ref() }.error() {
                    let _ = failure.set(error.to_string());
                }
                done.complete();
            },
        );
        unsafe { command.addCompletedHandler(block2::RcBlock::as_ptr(&callback)) };
        command.commit();
        Ok(DisplayCompletion(ready, self.context.clone()))
    }
}
