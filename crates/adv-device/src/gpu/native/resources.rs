//! Package C1: images, render targets, depth buffers, staging and uploads.
//!
//! Resources are created synchronously on the calling thread. Upload bytes go
//! once into the upload ring through [`super::stream::Producer::bytes`], in the
//! destination's own format, and reach the image with one
//! [`super::stream::Command::Upload`].

use super::super::{AlphaMode, DepthFormat, Error, Rect, TexelFormat};
use super::Gpu;
use adv_gpu::resource::{Dimension, ImageDesc, Usage, ViewDesc};

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

/// Guest rows already in the upload ring, waiting for their destination.
#[derive(Clone)]
pub struct Staged {
    pub(super) bytes: super::stream::Bytes,
    pub(super) pitch: u32,
}

impl Staged {
    pub fn pitch(&self) -> u32 {
        self.pitch
    }
    pub fn len(&self) -> usize {
        self.bytes.len as usize
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.len == 0
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
    /// Write `rows` rows through `fill` straight into the upload ring.
    pub fn stage<E>(
        &mut self,
        row_bytes: u32,
        rows: u32,
        fill: impl FnOnce(&mut dyn FnMut(u32, &[u8])) -> Result<(), E>,
    ) -> Result<Result<Staged, E>, Error> {
        let size = row_bytes
            .checked_mul(rows)
            .filter(|_| row_bytes != 0 && rows != 0)
            .ok_or_else(|| Error::Invalid("staged upload size".into()))?;
        let mut result = None;
        let bytes = self.stream.bytes(size, 256, |destination| {
            result = Some(fill(&mut |y, row| {
                let at = y as usize * row_bytes as usize;
                destination[at..at + row_bytes as usize].copy_from_slice(row);
            }));
        });
        Ok(result.unwrap().map(|()| Staged {
            bytes,
            pitch: row_bytes,
        }))
    }
    pub fn upload_staged(
        &mut self,
        image: &Image,
        rectangle: Rect,
        staged: &Staged,
    ) -> Result<(), Error> {
        upload_bounds(image, rectangle, staged.pitch, staged.len())?;
        self.record_upload(image, rectangle, staged);
        Ok(())
    }
    fn record_upload(&mut self, image: &Image, rectangle: Rect, staged: &Staged) {
        let mip = match &image.storage {
            Storage::Fixed { level, .. } => level.unwrap_or(0),
            Storage::Swap(_) => 0,
        };
        self.stream.push(super::stream::Command::Upload {
            bytes: staged.bytes,
            pitch: staged.pitch,
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
        let row = upload_bounds(image, rectangle, pitch, bgra.len())?;
        let staged = self
            .stage(row, rectangle.h, |write| {
                for y in 0..rectangle.h {
                    let start = y as usize * pitch as usize;
                    write(y, &bgra[start..start + row as usize]);
                }
                Ok::<(), std::convert::Infallible>(())
            })?
            .unwrap();
        self.record_upload(image, rectangle, &staged);
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
        let (producer, consumer) = stream::channel(device.ring(4 << 20).unwrap());
        Some((
            Gpu {
                device,
                stream: producer,
                pipelines: pipelines::Pipelines::default(),
                clip: None,
                stats: Default::default(),
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
                bytes,
                pitch,
                image,
                mip,
                rect,
            } = command
            {
                let Storage::Fixed { image, .. } = &image.storage else {
                    unreachable!()
                };
                let span = &chunk.spans[bytes.span as usize];
                encoder.copy_buffer_to_image(
                    BufferCopy {
                        buffer: span.buffer(),
                        offset: span.offset() + u64::from(bytes.offset),
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
            )
            .unwrap();
        let params: [u32; 8] = [
            1,
            1 | pipelines::fixed_sampling_flags(Some(image)),
            7,
            0,
            0,
            0,
            0,
            0,
        ];
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
                pipelines::binding_slot(pipelines::slot::STREAMS),
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
    // Same integer expansion as d3d9::format::upload, including bit replication.
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
                        [b << 3 | b >> 2, g << 2 | g >> 4, r << 3 | r >> 2, 255]
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
            if let Some(at) = actual.iter().zip(&expected).position(|(a, b)| a != b) {
                mismatches.push(format!(
                    "{format:?} differs at byte {at}: {} != {}",
                    actual[at], expected[at]
                ));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("; "));
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
