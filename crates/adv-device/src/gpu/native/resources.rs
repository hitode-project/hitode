//! Package C1: images, render targets, depth buffers, staging and uploads.
//!
//! Resources are created synchronously on the calling thread. Immediate
//! uploads write once into the ring; replayable snapshots use pooled mapped
//! buffers, independent of command chunks and ring capacity. Both carry the
//! destination's own format and reach it with one upload command.

use super::super::{AlphaMode, DepthFormat, Error, Rect, TexelFormat};
use super::Gpu;
use adv_gpu::resource::{BufferDesc, BufferUsage, Dimension, ImageDesc, Memory, Usage, ViewDesc};
use std::sync::Arc;

pub(super) fn gpu_error(error: adv_gpu::Error) -> Error {
    match error {
        adv_gpu::Error::Unavailable(message) => Error::Unavailable(message),
        adv_gpu::Error::Invalid(message) => Error::Invalid(message),
        adv_gpu::Error::Busy => Error::Busy,
        error => Error::Device(error.to_string()),
    }
}

fn format(format: TexelFormat) -> adv_gpu::Format {
    match format {
        TexelFormat::Bgra8 | TexelFormat::Bgrx8 => adv_gpu::Format::Bgra8Unorm,
        TexelFormat::B5g6r5 => adv_gpu::Format::B5g6r5Unorm,
        TexelFormat::Bgra4 => adv_gpu::Format::Bgra4Unorm,
        TexelFormat::A8 => adv_gpu::Format::R8Unorm,
    }
}

fn stores(device: &adv_gpu::Device, texel: TexelFormat) -> bool {
    // The public caps API has no transfer bits; COPY_DST is requested at
    // resource creation alongside the reported sampled capability.
    device
        .caps()
        .format(format(texel))
        .contains(adv_gpu::FormatCaps::SAMPLED)
}

fn fixed(
    device: &adv_gpu::Device,
    width: u32,
    height: u32,
    levels: u32,
    texel: TexelFormat,
    usage: Usage,
) -> Result<Image, Error> {
    let limit = device.caps().limits.max_image_2d;
    if width == 0
        || height == 0
        || width > limit
        || height > limit
        || levels == 0
        || levels > 32 - width.max(height).leading_zeros()
    {
        return Err(Error::Invalid(
            "image dimensions or mip count exceed device limits".into(),
        ));
    }
    let image = device
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: adv_gpu::Extent {
                width,
                height,
                depth_or_layers: 1,
            },
            format: format(texel),
            mips: levels,
            samples: 1,
            usage,
            view_formats: &[],
            label: "guest image",
        })
        .map_err(gpu_error)?;
    Ok(Image {
        storage: Storage::Fixed {
            view: image.view().clone(),
            image,
            level: None,
        },
        alpha: if texel == TexelFormat::Bgrx8 {
            AlphaMode::Opaque
        } else {
            AlphaMode::Stored
        },
    })
}

/// A sampled or rendered image. Clones share the allocation.
#[derive(Clone)]
pub struct Image {
    pub(super) storage: Storage,
    pub(super) alpha: AlphaMode,
}

#[derive(Clone)]
pub(super) enum Storage {
    /// One allocation, and the view draws sample and passes render: one mip
    /// level, or all (`level` is `None`).
    Fixed {
        image: adv_gpu::Image,
        view: adv_gpu::ImageView,
        level: Option<u32>,
    },
    /// A swap buffer: the owner thread picks its allocation for each frame.
    Swap(std::sync::Arc<super::backing::Swap>),
}

impl Image {
    pub fn width(&self) -> u32 {
        self.size().0
    }
    pub fn height(&self) -> u32 {
        self.size().1
    }
    fn size(&self) -> (u32, u32) {
        match &self.storage {
            Storage::Fixed { view, .. } => (view.size().width, view.size().height),
            Storage::Swap(swap) => swap.size,
        }
    }
    pub fn texel_format(&self) -> TexelFormat {
        match &self.storage {
            Storage::Fixed { image, .. } => match image.desc().format {
                adv_gpu::Format::R8Unorm => TexelFormat::A8,
                adv_gpu::Format::B5g6r5Unorm => TexelFormat::B5g6r5,
                adv_gpu::Format::Bgra4Unorm => TexelFormat::Bgra4,
                adv_gpu::Format::Bgra8Unorm => {
                    if self.alpha == AlphaMode::Opaque {
                        TexelFormat::Bgrx8
                    } else {
                        TexelFormat::Bgra8
                    }
                }
                _ => unreachable!("executor image format"),
            },
            Storage::Swap(_) => TexelFormat::Bgra8,
        }
    }
    pub fn alpha_mode(&self) -> AlphaMode {
        self.alpha
    }
    /// Identity of the view: equal for clones, distinct per mip level. The
    /// recording keys of `adv-dx` use it where they compared wgpu views.
    pub fn id(&self) -> ImageId {
        match &self.storage {
            Storage::Fixed { image, level, .. } => ImageId {
                allocation: image.id(),
                level: *level,
            },
            Storage::Swap(swap) => ImageId {
                allocation: swap.id,
                level: None,
            },
        }
    }
    pub fn mip_levels(&self) -> u32 {
        match &self.storage {
            Storage::Fixed { image, level, .. } => {
                if level.is_some() {
                    1
                } else {
                    image.desc().mips
                }
            }
            Storage::Swap(_) => 1,
        }
    }
    pub fn mip_level(&self, level: u32) -> Result<Self, Error> {
        if level >= self.mip_levels() {
            return Err(Error::Invalid("mip level is outside the image view".into()));
        }
        match &self.storage {
            Storage::Fixed {
                image, level: base, ..
            } => {
                let mip = base.unwrap_or(0) + level;
                let view = image
                    .create_view(&ViewDesc {
                        format: None,
                        dimension: None,
                        mips: mip..mip + 1,
                        layers: 0..1,
                        plane: 0,
                    })
                    .map_err(gpu_error)?;
                Ok(Self {
                    storage: Storage::Fixed {
                        image: image.clone(),
                        view,
                        level: Some(mip),
                    },
                    alpha: self.alpha,
                })
            }
            Storage::Swap(_) => Ok(self.clone()),
        }
    }
    pub fn shares_storage(&self, other: &Self) -> bool {
        self.id().allocation == other.id().allocation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageId {
    allocation: u64,
    level: Option<u32>,
}

/// A depth-stencil buffer; the owner thread backs it ([`super::backing`]).
#[derive(Clone)]
pub struct Depth(pub(super) std::sync::Arc<super::backing::DepthBuffer>);

impl Depth {
    pub fn width(&self) -> u32 {
        self.0.size.0
    }
    pub fn height(&self) -> u32 {
        self.0.size.1
    }
    pub fn format(&self) -> DepthFormat {
        self.0.format
    }
}

#[derive(Clone)]
pub struct Target {
    pub image: Image,
    pub(super) depth: Option<Depth>,
}

impl Target {
    pub fn mip_level(&self, level: u32) -> Result<Self, Error> {
        Self {
            image: self.image.mip_level(level)?,
            depth: None,
        }
        .with_depth(self.depth.clone())
    }
    pub fn with_alpha_mode(mut self, alpha: AlphaMode) -> Self {
        self.image.alpha = alpha;
        self
    }
    pub fn depth(&self) -> Option<&Depth> {
        self.depth.as_ref()
    }
    pub fn with_depth(mut self, depth: Option<Depth>) -> Result<Self, Error> {
        if depth
            .as_ref()
            .is_some_and(|d| (d.width(), d.height()) != self.image.size())
        {
            return Err(Error::Invalid(
                "depth size differs from color attachment".into(),
            ));
        }
        self.depth = depth;
        Ok(self)
    }
}

/// Idle staging bytes kept for reuse.
const FREE_STAGING_BYTES: u64 = 64 << 20;

/// Every staging buffer, shared with the [`Staged`] handles that use it. A
/// buffer is free once the pool holds its only reference: the owner thread
/// drops its handle after the submission that read it completed.
#[derive(Default)]
pub(super) struct StagingPool(Vec<Arc<adv_gpu::Buffer>>);

impl StagingPool {
    /// The smallest free buffer of at least `size` bytes, or a new one after
    /// trimming idle buffers to the budget.
    fn take(&mut self, device: &adv_gpu::Device, size: u64) -> Result<Arc<adv_gpu::Buffer>, Error> {
        let mut best: Option<(usize, u64)> = None;
        for (n, buffer) in self.0.iter_mut().enumerate() {
            let len = buffer.desc().size;
            if len >= size && best.is_none_or(|(_, b)| len < b) && Arc::get_mut(buffer).is_some() {
                best = Some((n, len));
            }
        }
        if let Some((n, _)) = best {
            return Ok(self.0[n].clone());
        }
        let mut idle: u64 = self
            .0
            .iter_mut()
            .filter_map(|b| Arc::get_mut(b).map(|b| b.desc().size))
            .sum();
        self.0.retain_mut(|b| {
            let Some(b) = Arc::get_mut(b) else {
                return true;
            };
            if idle + size <= FREE_STAGING_BYTES {
                return true;
            }
            idle -= b.desc().size;
            false
        });
        let buffer = Arc::new(
            device
                .create_buffer(&BufferDesc {
                    size,
                    usage: BufferUsage::COPY_SRC,
                    memory: Memory::Upload,
                    label: "staged guest rows",
                })
                .map_err(gpu_error)?,
        );
        self.0.push(buffer.clone());
        Ok(buffer)
    }
}

/// Immutable guest rows in a pooled, CPU-written upload buffer.
#[derive(Clone)]
pub struct Staged {
    buffer: Arc<adv_gpu::Buffer>,
    pitch: u32,
    len: u32,
}
impl Staged {
    pub fn pitch(&self) -> u32 {
        self.pitch
    }
    pub fn len(&self) -> usize {
        self.len as usize
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub(super) fn buffer(&self) -> &adv_gpu::Buffer {
        &self.buffer
    }
}

impl Gpu {
    pub fn image(&self, width: u32, height: u32) -> Result<Image, Error> {
        self.image_levels(width, height, 1)
    }
    pub fn image_levels(&self, width: u32, height: u32, levels: u32) -> Result<Image, Error> {
        self.texture(width, height, levels, TexelFormat::Bgra8)
    }
    pub fn target(&self, width: u32, height: u32, depth: bool) -> Result<Target, Error> {
        let target = self.target_levels(width, height, 1)?;
        target.with_depth(
            depth
                .then(|| self.depth(width, height, DepthFormat::Float32))
                .transpose()?,
        )
    }
    /// The largest image side the device supports.
    pub fn max_image_size(&self) -> u32 {
        self.device.caps().limits.max_image_2d
    }
    /// A swap buffer target ([`super::backing`]): its image is resolved by the
    /// owner thread, which hands it to presentation without a copy.
    pub fn presentation_target(
        &self,
        width: u32,
        height: u32,
        depth: bool,
    ) -> Result<Target, Error> {
        self.attachment_dimensions(width, height)?;
        Ok(Target {
            image: Image {
                storage: Storage::Swap(std::sync::Arc::new(super::backing::Swap::new((
                    width, height,
                )))),
                alpha: AlphaMode::Stored,
            },
            depth: depth.then(|| {
                Depth(std::sync::Arc::new(super::backing::DepthBuffer::new(
                    (width, height),
                    DepthFormat::Float32,
                )))
            }),
        })
    }
    pub fn target_levels(&self, width: u32, height: u32, levels: u32) -> Result<Target, Error> {
        Ok(Target {
            image: fixed(
                &self.device,
                width,
                height,
                levels,
                TexelFormat::Bgra8,
                Usage::SAMPLED | Usage::COPY_DST | Usage::COLOR | Usage::COPY_SRC,
            )?,
            depth: None,
        })
    }
    pub fn depth(&self, width: u32, height: u32, format: DepthFormat) -> Result<Depth, Error> {
        self.attachment_dimensions(width, height)?;
        Ok(Depth(std::sync::Arc::new(
            super::backing::DepthBuffer::new((width, height), format),
        )))
    }
    pub fn texture(
        &self,
        width: u32,
        height: u32,
        levels: u32,
        texel: TexelFormat,
    ) -> Result<Image, Error> {
        fixed(
            &self.device,
            width,
            height,
            levels,
            texel,
            Usage::SAMPLED | Usage::COPY_DST,
        )
    }
    pub fn stores(&self, texel: TexelFormat) -> bool {
        stores(&self.device, texel)
    }
    /// Write rows directly into a mapped staging buffer, without clearing it.
    pub fn stage<E>(
        &mut self,
        row_bytes: u32,
        rows: u32,
        fill: impl FnOnce(&mut dyn FnMut(u32, &[u8])) -> Result<(), E>,
    ) -> Result<Result<Staged, E>, Error> {
        self.check()?;
        let size = row_bytes
            .checked_mul(rows)
            .filter(|_| row_bytes != 0 && rows != 0)
            .ok_or_else(|| Error::Invalid("staged upload size".into()))?;
        let buffer = self.staging.take(&self.device, u64::from(size))?;
        // SAFETY: `take` returns a buffer no other handle shares, so neither the
        // GPU nor another writer touches it until the returned `Staged` is used.
        let bytes = unsafe { &mut buffer.mapped().unwrap().as_mut()[..size as usize] };
        let result = fill(&mut |y, row| {
            let at = y as usize * row_bytes as usize;
            bytes[at..at + row_bytes as usize].copy_from_slice(row);
        });
        Ok(result.map(|()| Staged {
            buffer,
            pitch: row_bytes,
            len: size,
        }))
    }
    pub fn upload_staged(
        &mut self,
        image: &Image,
        rectangle: Rect,
        staged: &Staged,
    ) -> Result<(), Error> {
        #[cfg(feature = "test-support")]
        self.stream.0.borrow().admit()?;
        upload_bounds(image, rectangle, staged.pitch, staged.len())?;
        self.record_upload(
            image,
            rectangle,
            staged.pitch,
            super::stream::UploadSource::Staged(staged.clone()),
        );
        Ok(())
    }
    fn record_upload(
        &mut self,
        image: &Image,
        rectangle: Rect,
        pitch: u32,
        source: super::stream::UploadSource,
    ) {
        let mip = match &image.storage {
            Storage::Fixed { level, .. } => level.unwrap_or(0),
            Storage::Swap(_) => 0,
        };
        self.stream.push(super::stream::Command::Upload {
            source,
            pitch,
            image: image.clone(),
            mip,
            rect: adv_gpu::Rect {
                x: rectangle.x,
                y: rectangle.y,
                w: rectangle.w,
                h: rectangle.h,
            },
        });
    }
    pub fn upload(
        &mut self,
        image: &Image,
        rectangle: Rect,
        pitch: u32,
        bgra: &[u8],
    ) -> Result<(), Error> {
        #[cfg(feature = "test-support")]
        self.stream.0.borrow().admit()?;
        let row = upload_bounds(image, rectangle, pitch, bgra.len())?;
        let bytes = self.stream.bytes(row * rectangle.h, 256, |bytes| {
            for y in 0..rectangle.h {
                let source = y as usize * pitch as usize;
                let destination = y as usize * row as usize;
                bytes[destination..destination + row as usize]
                    .copy_from_slice(&bgra[source..source + row as usize]);
            }
        });
        self.record_upload(
            image,
            rectangle,
            row,
            super::stream::UploadSource::Ring(bytes),
        );
        Ok(())
    }
    pub fn upload_alpha(
        &mut self,
        image: &Image,
        rectangle: Rect,
        pitch: u32,
        alpha: &[u8],
    ) -> Result<(), Error> {
        self.upload(image, rectangle, pitch, alpha)
    }
}

fn upload_bounds(image: &Image, r: Rect, pitch: u32, length: usize) -> Result<u32, Error> {
    let bpp = match &image.storage {
        Storage::Fixed { image, .. } => match image.desc().format {
            adv_gpu::Format::R8Unorm => 1,
            adv_gpu::Format::B5g6r5Unorm | adv_gpu::Format::Bgra4Unorm => 2,
            adv_gpu::Format::Bgra8Unorm => 4,
            _ => unreachable!("executor image format"),
        },
        Storage::Swap(_) => 4,
    };
    let row = u64::from(r.w) * bpp;
    if r.w == 0
        || r.h == 0
        || u64::from(r.x) + u64::from(r.w) > u64::from(image.width())
        || u64::from(r.y) + u64::from(r.h) > u64::from(image.height())
        || u64::from(pitch) < row
        || !u64::from(pitch).is_multiple_of(bpp)
        || u64::from(pitch) * u64::from(r.h - 1) + row > length as u64
    {
        return Err(Error::Invalid(
            "upload rectangle, pitch or storage is outside the image".into(),
        ));
    }
    Ok(row as u32)
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::{pipelines, stream};
    use super::*;
    use crate::gpu;
    use adv_gpu::encode::{Bind, BufferCopy, ColorAttachment, ImageCopy, Load, Store};
    use adv_gpu::resource::{
        Address, BorderColor, BufferDesc, BufferUsage, Filter, Memory, SamplerDesc,
    };

    pub(in crate::gpu::native) fn open() -> Option<(Gpu, adv_gpu::Queue, stream::Consumer)> {
        let options = adv_gpu::queue::DeviceOptions {
            software: !cfg!(target_vendor = "apple"),
            validation: true,
            ring_size: 4 << 20,
            ..Default::default()
        };
        let (device, queue) = match adv_gpu::Device::open(&options) {
            Ok(opened) => opened,
            Err(adv_gpu::Error::Unavailable(reason))
                if std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_none() =>
            {
                eprintln!("skipped: {reason}");
                return None;
            }
            Err(error) => panic!("open native device: {error}"),
        };
        let (mut producer, consumer) = stream::channel(device.ring(4 << 20).unwrap());
        let recording = super::super::record::Recording::new(&device, &mut producer).unwrap();
        let stream = stream::ExecutionStream(std::rc::Rc::new(std::cell::RefCell::new(producer)));
        let execution = std::rc::Rc::new(super::super::Execution {
            device: device.clone(),
            stream: stream.clone(),
            video: Arc::from([]),
            owner: super::super::owner::Owner::unstarted(),
            #[cfg(feature = "test-support")]
            capture: Default::default(),
        });
        Some((
            Gpu {
                device,
                stream,
                pipelines: pipelines::Pipelines::default(),
                clip: None,
                stats: Default::default(),
                execution,
                recording,
                staging: Default::default(),
            },
            queue,
            consumer,
        ))
    }
    pub(in crate::gpu::native) fn upload_chunk(
        gpu: &mut Gpu,
        queue: &mut adv_gpu::Queue,
        consumer: &stream::Consumer,
    ) {
        gpu.stream.push(stream::Command::Submit);
        let mut chunk = consumer.next().unwrap();
        let mut encoder = queue.encoder("native upload test");
        for command in &chunk.commands {
            if let stream::Command::Upload {
                source,
                pitch,
                image,
                mip,
                rect,
            } = command
            {
                let Storage::Fixed { image, .. } = &image.storage else {
                    unreachable!()
                };
                let (buffer, offset) = match source {
                    stream::UploadSource::Ring(bytes) => {
                        let span = &chunk.spans[bytes.span as usize];
                        (span.buffer(), span.offset() + u64::from(bytes.offset))
                    }
                    stream::UploadSource::Staged(staged) => (staged.buffer(), 0),
                };
                encoder.copy_buffer_to_image(
                    BufferCopy {
                        buffer,
                        offset,
                        row_pitch: *pitch,
                        rows: rect.h,
                    },
                    ImageCopy {
                        image,
                        mip: *mip,
                        layer: 0,
                        origin: (rect.x, rect.y, 0),
                    },
                    adv_gpu::Extent {
                        width: rect.w,
                        height: rect.h,
                        depth_or_layers: 1,
                    },
                );
            }
        }
        for span in chunk.spans.drain(..) {
            encoder.retain(span);
        }
        let token = queue.submit(encoder).unwrap();
        assert!(
            queue
                .wait(token, std::time::Duration::from_secs(20))
                .unwrap()
        );
        consumer.recycle(chunk);
    }
    pub(in crate::gpu::native) fn sample(
        gpu: &mut Gpu,
        queue: &mut adv_gpu::Queue,
        image: &Image,
    ) -> Vec<u8> {
        let target = gpu.target_levels(image.width(), image.height(), 1).unwrap();
        let pipeline = gpu
            .pipelines
            .fixed(
                &gpu.device,
                &gpu::DrawState::new(image.width(), image.height()),
                &[adv_gpu::Format::Bgra8Unorm],
                None,
                pipelines::fixed_sampling_flags(Some(image)),
            )
            .unwrap();
        let params: [u32; 8] = [1, 1, 7, 0, 0, 0, 0, 0];
        render(
            gpu,
            queue,
            &target.image,
            &pipeline,
            image,
            bytemuck::cast_slice(&params),
        )
    }
    pub(in crate::gpu::native) fn render(
        gpu: &mut Gpu,
        queue: &mut adv_gpu::Queue,
        target: &Image,
        pipeline: &adv_gpu::RenderPipeline,
        image: &Image,
        parameters: &[u8],
    ) -> Vec<u8> {
        let vertices = [
            gpu::Vertex {
                position: [-1., -1., 0., 1.],
                color: [1.; 4],
                uv: [0., 1.],
            },
            gpu::Vertex {
                position: [3., -1., 0., 1.],
                color: [1.; 4],
                uv: [2., 1.],
            },
            gpu::Vertex {
                position: [-1., 3., 0., 1.],
                color: [1.; 4],
                uv: [0., -1.],
            },
        ];
        render_stream(
            gpu,
            queue,
            target,
            pipeline,
            &[image],
            [&[], parameters],
            bytemuck::cast_slice(&vertices),
        )
    }
    pub(in crate::gpu::native) fn render_stream(
        gpu: &mut Gpu,
        queue: &mut adv_gpu::Queue,
        target: &Image,
        pipeline: &adv_gpu::RenderPipeline,
        images: &[&Image],
        uniforms: [&[u8]; 2],
        vertices: &[u8],
    ) -> Vec<u8> {
        let mut ring = gpu.device.ring(1 << 20).unwrap();
        let offsets = [
            vertices.len().next_multiple_of(256),
            (vertices.len().next_multiple_of(256) + uniforms[0].len()).next_multiple_of(256),
        ];
        let size = offsets[1] + uniforms[1].len();
        let mut span = ring.reserve(size as u64, 256).unwrap();
        let bytes = span.bytes();
        bytes[..vertices.len()].copy_from_slice(vertices);
        for (uniform, offset) in uniforms.iter().zip(offsets) {
            bytes[offset..offset + uniform.len()].copy_from_slice(uniform);
        }
        let sampler = gpu
            .device
            .sampler(&SamplerDesc {
                min: Filter::Nearest,
                mag: Filter::Nearest,
                mip: Filter::Nearest,
                address: [Address::Clamp; 3],
                border: BorderColor::TransparentBlack,
                lod_bias: 0,
                lod_min: 0,
                lod_max: 0,
                anisotropy: 1,
                compare: None,
            })
            .unwrap();
        let Storage::Fixed {
            image: destination,
            view,
            ..
        } = &target.storage
        else {
            unreachable!()
        };
        let width = target.width();
        let height = target.height();
        let readback = gpu
            .device
            .create_buffer(&BufferDesc {
                size: u64::from(width * height * 4),
                usage: BufferUsage::COPY_DST,
                memory: Memory::Readback,
                label: "native resource readback",
            })
            .unwrap();
        let mut encoder = queue.encoder("native sample test");
        {
            let mut pass = encoder.render_pass(&adv_gpu::RenderPassDesc {
                colors: [ColorAttachment {
                    view,
                    load: Load::DontCare,
                    store: Store::Store,
                    resolve: None,
                }]
                .into_iter()
                .collect(),
                depth_stencil: None,
                area: adv_gpu::Rect {
                    x: 0,
                    y: 0,
                    w: width,
                    h: height,
                },
                timestamps: None,
                label: "native sample",
            });
            pass.set_pipeline(pipeline);
            pass.bind(
                pipelines::slot::STREAMS,
                Bind::StorageBuffer(
                    span.buffer()
                        .slice(span.offset()..span.offset() + vertices.len() as u64),
                ),
            );
            for (slot, (uniform, offset)) in uniforms.iter().zip(offsets).enumerate() {
                if !uniform.is_empty() {
                    pass.bind(
                        slot as u32,
                        Bind::Uniform(span.buffer().slice(
                            span.offset() + offset as u64
                                ..span.offset() + (offset + uniform.len()) as u64,
                        )),
                    );
                }
            }
            for (slot, image) in images.iter().enumerate() {
                let Storage::Fixed { view: source, .. } = &image.storage else {
                    unreachable!()
                };
                pass.bind(slot as u32, Bind::Sampled(source));
                pass.bind(slot as u32, Bind::Sampler(&sampler));
            }
            pass.set_viewport(0., 0., width as f32, height as f32, 0., 1.);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_image_to_buffer(
            ImageCopy {
                image: destination,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            BufferCopy {
                buffer: &readback,
                offset: 0,
                row_pitch: width * 4,
                rows: height,
            },
            adv_gpu::Extent {
                width,
                height,
                depth_or_layers: 1,
            },
        );
        encoder.retain(span);
        let token = queue.submit(encoder).unwrap();
        assert!(
            queue
                .wait(token, std::time::Duration::from_secs(20))
                .unwrap()
        );
        // SAFETY: the submission completed; this buffer is mapped for readback.
        unsafe { readback.mapped().unwrap().as_ref().to_vec() }
    }
    // Stored formats sample with exact unorm conversion; CPU-only expansion is independent.
    fn expanded(format: TexelFormat, input: &[u8]) -> Vec<u8> {
        match format {
            TexelFormat::Bgra8 => input.to_vec(),
            TexelFormat::Bgrx8 => input
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|c| [c[0], c[1], c[2], 255])
                .collect(),
            TexelFormat::A8 => input.iter().flat_map(|&a| [0, 0, 0, a]).collect(),
            TexelFormat::Bgra4 | TexelFormat::B5g6r5 => input
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|c| {
                    let v = u16::from_le_bytes(*c);
                    if format == TexelFormat::Bgra4 {
                        std::array::from_fn::<_, 4, _>(|n| ((v >> (n * 4)) & 15) as u8 * 17)
                    } else {
                        let b = (v & 31) as u8;
                        let g = ((v >> 5) & 63) as u8;
                        let r = (v >> 11) as u8;
                        let unorm = |value: u8, maximum: u16| {
                            ((u16::from(value) * 255 + maximum / 2) / maximum) as u8
                        };
                        [unorm(b, 31), unorm(g, 63), unorm(r, 31), 255]
                    }
                })
                .collect(),
        }
    }
    // spec: GFX-SAMPLER-4, GFX-COPY-1
    #[test]
    fn native_formats_round_trip_through_ring_sampling_and_readback() {
        let Some((mut gpu, mut queue, consumer)) = open() else {
            return;
        };
        let mut mismatches = Vec::new();
        for format in [
            TexelFormat::Bgra8,
            TexelFormat::Bgrx8,
            TexelFormat::B5g6r5,
            TexelFormat::Bgra4,
            TexelFormat::A8,
        ] {
            if !gpu.stores(format) {
                continue;
            }
            let (input, bpp): (Vec<u8>, u32) = match format {
                TexelFormat::B5g6r5 | TexelFormat::Bgra4 => {
                    ((0..=u16::MAX).flat_map(u16::to_le_bytes).collect(), 2)
                }
                TexelFormat::A8 => ((0..=u16::MAX).map(|v| v as u8).collect(), 1),
                _ => (
                    (0..=u16::MAX)
                        .flat_map(|v| {
                            [
                                v as u8,
                                (v >> 8) as u8,
                                !v as u8,
                                (v.wrapping_mul(13)) as u8,
                            ]
                        })
                        .collect(),
                    4,
                ),
            };
            let image = gpu.texture(256, 256, 1, format).unwrap();
            let staged = gpu
                .stage(256 * bpp, 256, |write| {
                    for (y, row) in input.chunks_exact((256 * bpp) as usize).enumerate() {
                        write(y as u32, row);
                    }
                    Ok::<(), ()>(())
                })
                .unwrap()
                .unwrap();
            gpu.upload_staged(
                &image,
                Rect {
                    x: 0,
                    y: 0,
                    w: 256,
                    h: 256,
                },
                &staged,
            )
            .unwrap();
            upload_chunk(&mut gpu, &mut queue, &consumer);
            let actual = sample(&mut gpu, &mut queue, &image);
            let expected = expanded(format, &input);
            assert_eq!(actual.len(), expected.len());
            // Float to unorm may yield either neighboring integer (Vulkan "Fixed-Point
            // Data Conversions"); only 5- and 6-bit channels convert to non-integers.
            let slack = u8::from(format == TexelFormat::B5g6r5);
            if let Some(at) = actual
                .iter()
                .zip(&expected)
                .position(|(a, b)| a.abs_diff(*b) > slack)
            {
                mismatches.push(format!(
                    "{format:?} differs at byte {at}: {} != {}",
                    actual[at], expected[at]
                ));
            }
            gpu.upload(
                &image,
                Rect {
                    x: 0,
                    y: 0,
                    w: 256,
                    h: 256,
                },
                256 * bpp,
                &input,
            )
            .unwrap();
            upload_chunk(&mut gpu, &mut queue, &consumer);
            assert_eq!(sample(&mut gpu, &mut queue, &image), actual);
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("; "));
    }

    pub(in crate::gpu::native) fn executor() -> Option<Gpu> {
        match pollster::block_on(Gpu::request(!cfg!(target_vendor = "apple"))) {
            Ok(gpu) => Some(gpu),
            Err(Error::Unavailable(reason))
                if std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_none() =>
            {
                eprintln!("skipped: {reason}");
                None
            }
            Err(error) => panic!("device: {error}"),
        }
    }
    pub(in crate::gpu::native) fn read(gpu: &mut Gpu, image: &Image) -> Vec<u8> {
        let mut transfer = gpu.readback(image, None).unwrap();
        transfer.take_completion_job().unwrap()();
        transfer.poll().unwrap().unwrap()
    }

    // spec: GFX-SAMPLER-4
    #[test]
    fn staged_rows_survive_chunk_handoffs_submissions_and_readbacks() {
        let Some(mut gpu) = executor() else { return };
        let target = gpu.target(16, 16, false).unwrap();
        let rectangle = Rect {
            x: 0,
            y: 0,
            w: 16,
            h: 16,
        };
        let pixels: Vec<u8> = (0..1024).map(|i| (i * 37) as u8).collect();
        let staged = gpu
            .stage(64, 16, |write| {
                for (y, row) in pixels.as_chunks::<64>().0.iter().enumerate() {
                    write(y as u32, row);
                }
                Ok::<(), ()>(())
            })
            .unwrap()
            .unwrap();
        // Each 128 uploads hands off a full chunk. Reusing the ring for these
        // rows must never change a snapshot admitted before those chunks.
        for _ in 0..400 {
            gpu.upload(&target.image, rectangle, 64, &[0; 1024])
                .unwrap();
        }
        gpu.flush().unwrap();
        assert_eq!(read(&mut gpu, &target.image), vec![0; 1024]);
        gpu.upload_staged(&target.image, rectangle, &staged)
            .unwrap();
        drop(staged);
        assert_eq!(read(&mut gpu, &target.image), pixels);
    }

    // spec: GFX-SAMPLER-4
    #[test]
    fn staged_snapshot_can_exceed_the_upload_ring() {
        let Some(mut gpu) = executor() else { return };
        let (width, height) = (4096, 4097);
        let target = gpu.target(width, height, false).unwrap();
        let rectangle = Rect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        };
        let row = [19, 71, 133, 229].repeat(width as usize);
        let staged = gpu
            .stage(width * 4, height, |write| {
                for y in 0..height {
                    write(y, &row);
                }
                Ok::<(), ()>(())
            })
            .unwrap()
            .unwrap();
        assert!(staged.len() > FREE_STAGING_BYTES as usize);
        gpu.flush().unwrap();
        gpu.upload_staged(&target.image, rectangle, &staged)
            .unwrap();
        drop(staged);
        let pixels = read(&mut gpu, &target.image);
        assert_eq!(pixels.len(), row.len() * height as usize);
        assert!(pixels.chunks_exact(row.len()).all(|p| p == row));
    }

    // spec: GFX-SAMPLER-4
    #[test]
    fn staging_pool_reuses_the_smallest_fit_after_the_last_reference() {
        let Some((mut gpu, _, _)) = open() else {
            return;
        };
        let stage =
            |gpu: &mut Gpu, size| gpu.stage(size, 1, |_| Ok::<(), ()>(())).unwrap().unwrap();
        let small = stage(&mut gpu, 4);
        let medium = stage(&mut gpu, 8);
        let large = stage(&mut gpu, 12);
        let medium_id = Arc::as_ptr(&medium.buffer);
        let held = medium.clone();
        drop(medium);
        // A clone still shares it, so a fitting request allocates.
        assert_ne!(Arc::as_ptr(&stage(&mut gpu, 8).buffer), medium_id);
        drop(large);
        std::thread::spawn(move || drop(held)).join().unwrap();
        assert_eq!(Arc::as_ptr(&stage(&mut gpu, 5).buffer), medium_id);
        drop(small);
        assert_eq!(gpu.staging.0.len(), 4);
        // A request past the budget trims every idle buffer first.
        let above_budget = stage(&mut gpu, FREE_STAGING_BYTES as u32);
        assert_eq!(gpu.staging.0.len(), 1);
        drop(above_budget);
    }
    // spec: GFX-SAMPLER-4
    #[test]
    fn uploads_validate_bounds_and_preserve_mip_views() {
        let Some((mut gpu, mut queue, consumer)) = open() else {
            return;
        };
        let image = gpu.texture(4, 4, 3, TexelFormat::Bgra8).unwrap();
        let mip = image.mip_level(1).unwrap();
        assert_eq!((mip.width(), mip.height(), mip.mip_levels()), (2, 2, 1));
        assert!(mip.shares_storage(&image));
        assert_ne!(mip.id(), image.id());
        assert!(image.mip_level(3).is_err());
        assert!(gpu.texture(0, 1, 1, TexelFormat::Bgra8).is_err());
        assert!(gpu.texture(1, 1, 2, TexelFormat::Bgra8).is_err());
        let rectangle = Rect {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
        };
        assert!(gpu.upload(&mip, rectangle, 7, &[0; 16]).is_err());
        assert!(gpu.upload(&mip, rectangle, 8, &[0; 15]).is_err());
        assert!(
            gpu.upload(&mip, Rect { x: 1, ..rectangle }, 8, &[0; 16])
                .is_err()
        );
        let pixels = [11, 22, 33, 44].repeat(4);
        gpu.upload(&mip, rectangle, 8, &pixels).unwrap();
        upload_chunk(&mut gpu, &mut queue, &consumer);
        assert_eq!(sample(&mut gpu, &mut queue, &mip), pixels);
    }
}
