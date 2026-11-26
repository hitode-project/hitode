//! Typed native backing and copy coordinates for validated D3D11 resources.
use super::{
    desc::{Bind, Misc, Resource, Texture, TextureKind, View, ViewKind},
    ops::Error,
};
use adv_device::gpu::{Gpu, ImageRegion, TransferBuffer};
use adv_gpu::{
    Buffer, Extent, Format, Image, ImageView,
    resource::{BufferDesc, BufferUsage, Dimension, ImageDesc, Memory, Usage, ViewDesc},
};

#[derive(Clone)]
pub enum Backing {
    Buffer(Buffer),
    Image(Image),
}

impl Backing {
    pub fn new(gpu: &Gpu, resource: Resource) -> Result<Self, Error> {
        let bind = resource.bind();
        if let Resource::Buffer(buffer) = resource {
            let mut usage = BufferUsage::COPY_SRC | BufferUsage::COPY_DST;
            if bind.contains(Bind::INDEX) {
                usage |= BufferUsage::INDEX;
            }
            if bind.contains(Bind::CONSTANT) {
                usage |= BufferUsage::UNIFORM;
            }
            if [Bind::VERTEX, Bind::SHADER, Bind::UNORDERED]
                .into_iter()
                .any(|bit| bind.contains(bit))
            {
                usage |= BufferUsage::STORAGE;
            }
            if resource.misc().contains(Misc::INDIRECT) {
                usage |= BufferUsage::INDIRECT | BufferUsage::STORAGE;
            }
            return Ok(Self::Buffer(gpu.device().create_buffer(&BufferDesc {
                size: u64::from(buffer.size()).next_multiple_of(4),
                usage,
                memory: Memory::Device,
                label: "D3D11 buffer",
            })?));
        }
        let Resource::Texture(texture) = resource else {
            unreachable!()
        };
        let format = native_format(texture.format().format).ok_or(Error::InvalidArgument)?;
        let dimension = if resource.misc().contains(Misc::CUBE) {
            Dimension::Cube
        } else {
            match texture.kind() {
                TextureKind::D1 => Dimension::D1,
                TextureKind::D2 => Dimension::D2,
                TextureKind::D3 => Dimension::D3,
            }
        };
        let [width, height, depth] = texture.extent(0);
        let mut usage = Usage::COPY_SRC | Usage::COPY_DST;
        for (bit, flag) in [
            (Bind::SHADER, Usage::SAMPLED),
            (Bind::RENDER_TARGET, Usage::COLOR),
            (Bind::DEPTH_STENCIL, Usage::DEPTH_STENCIL),
            (Bind::UNORDERED, Usage::STORAGE),
        ] {
            if bind.contains(bit) {
                usage |= flag;
            }
        }
        if bind.contains(Bind::RENDER_TARGET) {
            usage |= Usage::SAMPLED;
        }
        let image = gpu.device().create_image(&ImageDesc {
            dimension,
            size: Extent {
                width,
                height,
                depth_or_layers: if dimension == Dimension::D3 {
                    depth
                } else {
                    texture.layers()
                },
            },
            format,
            mips: texture.levels(),
            samples: texture.samples(),
            usage,
            view_formats: alternate_formats(texture.format().format),
            label: "D3D11 texture",
        })?;
        Ok(Self::Image(image))
    }

    pub fn view(&self, view: &View) -> Result<ImageView, Error> {
        let Self::Image(image) = self else {
            return Err(Error::InvalidArgument);
        };
        let range = view.subresources();
        let format = native_format(view.format()).ok_or(Error::InvalidArgument)?;
        let plane = match (
            image.desc().format.has_stencil(),
            view.kind(),
            view.format(),
        ) {
            (true, ViewKind::ShaderResource, 22 | 47) => 1,
            (true, ViewKind::ShaderResource, 21 | 46) => 2,
            _ => 0,
        };
        let dimension = (image.desc().dimension == Dimension::Cube
            && view.kind() != ViewKind::ShaderResource)
            .then_some(Dimension::D2);
        Ok(image.create_view(&ViewDesc {
            format: (plane == 0 && format != image.desc().format).then_some(format),
            dimension,
            mips: range.mips.clone(),
            layers: range.layers.clone(),
            plane,
        })?)
    }
}

fn alternate_formats(format: u32) -> &'static [Format] {
    use Format::*;
    match format {
        27 => &[Rgba8Unorm, Rgba8Srgb, Rgba8Uint, Rgba8Snorm, Rgba8Sint],
        90 | 92 => &[Bgra8Unorm, Bgra8Srgb],
        70 => &[Bc1Unorm, Bc1Srgb],
        73 => &[Bc2Unorm, Bc2Srgb],
        76 => &[Bc3Unorm, Bc3Srgb],
        97 => &[Bc7Unorm, Bc7Srgb],
        _ => &[],
    }
}

pub fn native_format(format: u32) -> Option<Format> {
    use Format::*;
    Some(match format {
        1 | 2 => Rgba32Float,
        3 => Rgba32Uint,
        4 => Rgba32Sint,
        9 | 10 => Rgba16Float,
        11 => Rgba16Unorm,
        12 => Rgba16Uint,
        13 => Rgba16Snorm,
        14 => Rgba16Sint,
        15 | 16 => Rg32Float,
        17 => Rg32Uint,
        18 => Rg32Sint,
        19..=22 => Depth32FloatStencil8,
        23 | 24 => Rgb10a2Unorm,
        25 => Rgb10a2Uint,
        26 => Rg11b10Float,
        27 | 28 => Rgba8Unorm,
        29 => Rgba8Srgb,
        30 => Rgba8Uint,
        31 => Rgba8Snorm,
        32 => Rgba8Sint,
        33 | 34 => Rg16Float,
        35 => Rg16Unorm,
        36 => Rg16Uint,
        37 => Rg16Snorm,
        38 => Rg16Sint,
        39 | 41 => R32Float,
        40 => Depth32Float,
        42 => R32Uint,
        43 => R32Sint,
        44..=47 => Depth24UnormStencil8,
        48 | 49 => Rg8Unorm,
        50 => Rg8Uint,
        51 => Rg8Snorm,
        52 => Rg8Sint,
        53 | 54 => R16Float,
        55 => Depth16Unorm,
        56 => R16Unorm,
        57 => R16Uint,
        58 => R16Snorm,
        59 => R16Sint,
        60 | 61 => R8Unorm,
        62 => R8Uint,
        63 => R8Snorm,
        64 => R8Sint,
        65 => A8Unorm,
        67 => Rgb9e5Float,
        70 | 71 => Bc1Unorm,
        72 => Bc1Srgb,
        73 | 74 => Bc2Unorm,
        75 => Bc2Srgb,
        76 | 77 => Bc3Unorm,
        78 => Bc3Srgb,
        79 | 80 => Bc4Unorm,
        81 => Bc4Snorm,
        82 | 83 => Bc5Unorm,
        84 => Bc5Snorm,
        85 => B5g6r5Unorm,
        86 => Bgr5a1Unorm,
        87 | 88 | 90 | 92 => Bgra8Unorm,
        91 | 93 => Bgra8Srgb,
        94 | 95 => Bc6hUfloat,
        96 => Bc6hSfloat,
        97 | 98 => Bc7Unorm,
        99 => Bc7Srgb,
        115 => Bgra4Unorm,
        _ => return None,
    })
}

pub fn region(image: &Image, texture: Texture, subresource: u32) -> ImageRegion {
    let mip = subresource % texture.levels();
    let [width, height, depth] = texture.extent(mip);
    ImageRegion {
        image: image.clone(),
        mip,
        layer: subresource / texture.levels(),
        origin: (0, 0, 0),
        extent: Extent {
            width,
            height,
            depth_or_layers: depth,
        },
    }
}

pub fn upload(gpu: &Gpu, bytes: &[u8], usage: BufferUsage, size: u64) -> Result<Buffer, Error> {
    let buffer = gpu.device().create_buffer(&BufferDesc {
        size,
        usage,
        memory: Memory::Upload,
        label: "D3D11 captured data",
    })?;
    let mapping = buffer.mapped().unwrap();
    // SAFETY: This is a newly allocated upload buffer. Its mapping is at least
    // size bytes, and no GPU command or other thread has received it yet.
    unsafe {
        mapping.cast::<u8>().as_ptr().write_bytes(0, size as usize);
        mapping
            .cast::<u8>()
            .as_ptr()
            .copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
    }
    Ok(buffer)
}

pub fn texture_transfer(
    gpu: &Gpu,
    texture: Texture,
    region: &ImageRegion,
    bytes: &[u8],
) -> Result<TransferBuffer, Error> {
    let format = texture.format();
    let row = region.extent.width.div_ceil(u32::from(format.alignment[0]))
        * u32::from(format.aligned_unit_bytes);
    let rows = region
        .extent
        .height
        .div_ceil(u32::from(format.alignment[1]));
    let pitch = row.next_multiple_of(256);
    let size = u64::from(pitch) * u64::from(rows) * u64::from(region.extent.depth_or_layers);
    let buffer = gpu.device().create_buffer(&BufferDesc {
        size,
        usage: BufferUsage::COPY_SRC,
        memory: Memory::Upload,
        label: "D3D11 texture upload",
    })?;
    let mapping = buffer.mapped().unwrap();
    // SAFETY: Each row fits the newly allocated mapped buffer. No other
    // reader or writer can access the buffer before submission.
    unsafe {
        for (index, source) in bytes.chunks_exact(row as usize).enumerate() {
            mapping
                .cast::<u8>()
                .as_ptr()
                .add(index * pitch as usize)
                .copy_from_nonoverlapping(source.as_ptr(), row as usize);
        }
    }
    Ok(TransferBuffer {
        buffer,
        offset: 0,
        row_pitch: pitch,
        rows,
    })
}
