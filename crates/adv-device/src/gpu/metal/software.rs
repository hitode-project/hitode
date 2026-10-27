//! No-copy reads of immutable software publications on the shared Metal queue.
use super::*;
use crate::surface::{Damage, PendingFrame, PixelFormat};
use std::ptr::NonNull;

pub(in crate::gpu::renderer) struct Software(Object<dyn MTLComputePipelineState>);
// Pipeline state is immutable and shared by all consumers of this device.
unsafe impl Send for Software {}
unsafe impl Sync for Software {}
impl Software {
    fn new(device: &ProtocolObject<dyn MTLDevice>) -> Result<Self, Error> {
        let library = device
            .newLibraryWithSource_options_error(
                &NSString::from_str(include_str!("software.metal")),
                None,
            )
            .map_err(|e| Error::Device(e.to_string()))?;
        let function = library
            .newFunctionWithName(&NSString::from_str("decode"))
            .ok_or_else(|| invalid("missing software pixel decoder"))?;
        device
            .newComputePipelineStateWithFunction_error(&function)
            .map(Self)
            .map_err(|e| Error::Device(e.to_string()))
    }
}
impl Submission<'_> {
    /// Sample BGRA publications directly from their immutable snapshot allocation.
    /// The texture retains its parent buffer, whose deallocator owns the pixels.
    pub fn software_texture(&self, frame: &PendingFrame) -> Result<Option<wgpu::Texture>, Error> {
        if frame.format != PixelFormat::Bgra32 {
            return Ok(None);
        }
        let device = unsafe { self.context.device.as_hal::<wgpu::hal::api::Metal>() }.unwrap();
        let device = device.raw_device();
        let buffer = software_buffer(device, frame)?;
        let (width, height) = frame.extent();
        let descriptor = MTLTextureDescriptor::new();
        descriptor.setTextureType(MTLTextureType::Type2D);
        descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        unsafe {
            descriptor.setWidth(width as usize);
            descriptor.setHeight(height as usize);
        }
        descriptor.setStorageMode(MTLStorageMode::Shared);
        descriptor.setUsage(MTLTextureUsage::ShaderRead);
        let raw = buffer
            .newTextureWithDescriptor_offset_bytesPerRow(&descriptor, 0, frame.pitch as usize)
            .ok_or_else(|| Error::Device("Metal software texture import failed".into()))?;
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
                        label: Some("adv shared software snapshot"),
                        size: extent(width, height),
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: FORMAT,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    },
                    wgpu::TextureUses::RESOURCE,
                )
        };
        Ok(Some(texture))
    }

    /// Import a page-owned immutable publication. The command retains its bytes
    /// until completion; subsequent producer writes acquire another slot.
    /// The destination is a same-sized BGRA8 texture with COPY_DST, STORAGE,
    /// and RENDER_ATTACHMENT usages. A zero serial initializes its wgpu state.
    pub fn upload_software(
        &self,
        texture: &wgpu::Texture,
        frame: &PendingFrame,
        since: u64,
    ) -> Result<Damage, Error> {
        let (width, height) = frame.extent();
        if frame.native().is_some() || width != texture.width() || height != texture.height() {
            return Err(invalid("software upload extent or frame type mismatch"));
        }
        let damage = frame.damage_since(since);
        if damage.is_empty() {
            return Ok(damage);
        }
        self.flush();
        if since == 0 {
            let view = texture.create_view(&Default::default());
            let mut encoder = self
                .context
                .device
                .create_command_encoder(&Default::default());
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            self.context.queue.submit([encoder.finish()]);
        } else {
            self.context.queue.submit([]);
        }
        let device = unsafe { self.context.device.as_hal::<wgpu::hal::api::Metal>() }.unwrap();
        let device = device.raw_device();
        let buffer = software_buffer(device, frame)?;
        let queue = unsafe { self.context.queue.as_hal::<wgpu::hal::api::Metal>() }.unwrap();
        let command = queue
            .as_raw()
            .commandBuffer()
            .ok_or_else(|| Error::Device("Metal software command allocation failed".into()))?;
        let target = unsafe { texture.as_hal::<wgpu::hal::api::Metal>() }.unwrap();
        if frame.format == PixelFormat::Bgra32 {
            let encoder = command.blitCommandEncoder().unwrap();
            for r in damage.iter() {
                unsafe {
                    encoder.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                    &buffer, (r.y * frame.pitch + r.x * 4) as usize, frame.pitch as usize,
                    0, MTLSize { width: r.w as usize, height: r.h as usize, depth: 1 },
                    target.raw_handle(), 0, 0, MTLOrigin { x: r.x as usize, y: r.y as usize, z: 0 },
                )
                };
            }
            encoder.endEncoding();
        } else {
            let decoder = self
                .context
                .software
                .get_or_init(|| Software::new(device))
                .as_ref()
                .map_err(Clone::clone)?;
            let encoder = command.computeCommandEncoder().unwrap();
            encoder.setComputePipelineState(&decoder.0);
            let format = match frame.format {
                PixelFormat::Bgra32 => unreachable!(),
                PixelFormat::Bgr24 => 1,
                PixelFormat::Rgb565 => 2,
                PixelFormat::Rgb555 => 3,
                PixelFormat::Indexed8 => 4,
                PixelFormat::Indexed4 => 5,
                PixelFormat::Indexed1 => 6,
            };
            unsafe {
                encoder.setBuffer_offset_atIndex(Some(&buffer), 0, 0);
                encoder.setTexture_atIndex(Some(target.raw_handle()), 0);
                encoder.setBytes_length_atIndex(
                    NonNull::from(frame.palette.as_deref().unwrap_or(&[[0; 4]; 256])).cast(),
                    1024,
                    2,
                );
            }
            for r in damage.iter() {
                let p = [frame.pitch, r.x, r.y, r.w, r.h, format];
                unsafe { encoder.setBytes_length_atIndex(NonNull::from(&p).cast(), 24, 1) };
                encoder.dispatchThreadgroups_threadsPerThreadgroup(
                    MTLSize {
                        width: r.w.div_ceil(8) as usize,
                        height: r.h.div_ceil(8) as usize,
                        depth: 1,
                    },
                    MTLSize {
                        width: 8,
                        height: 8,
                        depth: 1,
                    },
                );
            }
            encoder.endEncoding();
        }
        let failure = self.context.failure.clone();
        let complete = block2::RcBlock::new(
            move |command: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                if let Some(error) = unsafe { command.as_ref() }.error() {
                    let _ = failure.set(error.to_string());
                }
            },
        );
        unsafe { command.addCompletedHandler(block2::RcBlock::as_ptr(&complete)) };
        command.commit();
        Ok(damage)
    }
}

fn software_buffer(
    device: &ProtocolObject<dyn MTLDevice>,
    frame: &PendingFrame,
) -> Result<Object<dyn MTLBuffer>, Error> {
    let pixels = frame.pixels.clone();
    let pointer = NonNull::new(pixels.as_ptr().cast_mut().cast()).unwrap();
    let size = pixels.allocation_size();
    let release = block2::RcBlock::new(move |_: NonNull<std::ffi::c_void>, _: usize| {
        let _ = &pixels;
    });
    let buffer = unsafe {
        device.newBufferWithBytesNoCopy_length_options_deallocator(
            pointer,
            size,
            MTLResourceOptions::StorageModeShared,
            Some(&release),
        )
    }
    .ok_or_else(|| Error::Device("Metal software frame import failed".into()))?;
    Ok(buffer)
}
