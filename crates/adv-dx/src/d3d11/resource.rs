//! Typed native backing and copy coordinates for validated D3D11 resources.
use super::{
    desc::{Bind, Misc, Resource, Texture, TextureKind, View, ViewKind},
    ops::Error,
};
use adv_device::gpu::{
    BufferRange, BufferWindow, Gpu, ImageAlias, ImageRegion, ModernTransfer, TransferBuffer,
    UniformWindow,
};
use adv_gpu::{
    Buffer, Caps, Extent, Format, FormatCaps, Image, ImageView,
    resource::{
        BufferDesc, BufferUsage, Dimension, ImageDesc, Memory, Usage, ViewComponents, ViewDesc,
        ViewLayout,
    },
};

mod blocks;
use blocks::Blocks;

#[derive(Clone)]
pub enum Backing {
    Buffer(Buffer),
    Constant(UniformWindow),
    Image(Image),
    Blocks(Blocks),
}

impl Backing {
    pub fn buffer_window(&self, range: std::ops::Range<u64>) -> BufferWindow {
        match self {
            Self::Buffer(buffer) => BufferRange {
                buffer: buffer.clone(),
                range,
            }
            .into(),
            Self::Constant(window) => BufferWindow::Uniform(window.slice(range)),
            Self::Image(_) | Self::Blocks(_) => unreachable!(),
        }
    }

    pub fn new(gpu: &Gpu, resource: Resource) -> Result<Self, Error> {
        let bind = resource.bind();
        if let Resource::Buffer(buffer) = resource {
            let mut usage = BufferUsage::COPY_SRC | BufferUsage::COPY_DST;
            if bind.contains(Bind::INDEX) {
                usage |= BufferUsage::INDEX;
            }
            if [
                Bind::VERTEX,
                Bind::INDEX,
                Bind::STREAM_OUTPUT,
                Bind::SHADER,
                Bind::UNORDERED,
            ]
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
        let dxgi = texture.format().format;
        let caps = gpu.device().caps();
        if bc_decoded(caps, dxgi)
            || (texture.kind() == TextureKind::D3
                && is_bc(dxgi)
                && !texture.format().cast_family.iter().all(|&entry| {
                    caps.format(native_format(entry).unwrap())
                        .contains(FormatCaps::BC_VOLUME)
                }))
        {
            return Ok(Self::Blocks(Blocks::new(texture)));
        }
        let format = match (dxgi, bind.contains(Bind::DEPTH_STENCIL)) {
            (39, true) => Format::Depth32Float,
            (53, true) => Format::Depth16Unorm,
            (39, false)
                if cfg!(target_vendor = "apple")
                    && bind.contains(Bind::UNORDERED)
                    && caps
                        .format(Format::R32Uint)
                        .contains(FormatCaps::STORAGE_ATOMIC) =>
            {
                Format::R32Uint
            }
            (format, _) => {
                backing_format(gpu.device().caps(), format).ok_or(Error::InvalidArgument)?
            }
        };
        let dimension = texture_dimension(texture);
        let [width, height, depth] = texture.extent(0);
        let view_formats = alternate_formats(texture.format().format, format);
        let mut usage = Usage::COPY_SRC | Usage::COPY_DST;
        for (bit, flag) in [
            (Bind::SHADER, Usage::SAMPLED),
            (Bind::RENDER_TARGET, Usage::COLOR),
            (Bind::DEPTH_STENCIL, Usage::DEPTH_STENCIL),
        ] {
            if bind.contains(bit) {
                usage |= flag;
            }
        }
        if bind.contains(Bind::UNORDERED)
            && std::iter::once(format)
                .chain(view_formats.iter().copied())
                .any(|format| caps.format(format).contains(FormatCaps::STORAGE))
        {
            usage |= Usage::STORAGE;
        }
        if texture.samples() == 1
            && std::iter::once(format)
                .chain(view_formats.iter().copied())
                .any(|format| image_filter_needed(caps, format))
        {
            usage |= Usage::STORAGE;
        }
        if packed_resolve(caps, dxgi, format).is_some() {
            usage |= Usage::COLOR | Usage::SAMPLED;
        }
        if bind.contains(Bind::RENDER_TARGET) || format.has_stencil() {
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
            view_formats,
            label: "D3D11 texture",
        })?;
        Ok(Self::Image(image))
    }

    pub fn view(&self, gpu: &Gpu, view: &View) -> Result<ImageView, Error> {
        let image = match self {
            Self::Image(image) => image,
            Self::Blocks(blocks) => return blocks.view(gpu, view),
            Self::Buffer(_) | Self::Constant(_) => return Err(Error::InvalidArgument),
        };
        let format = if view.format() == 65 && view.kind() != ViewKind::ShaderResource {
            Format::R8Unorm
        } else if image.desc().format.has_stencil()
            || (matches!(view.format(), 85 | 86 | 115) && image.desc().format == Format::Bgra8Unorm)
            || (image.desc().format.has_depth()
                && view.kind() == ViewKind::ShaderResource
                && matches!(view.format(), 41 | 56))
        {
            image.desc().format
        } else {
            backing_format(gpu.device().caps(), view.format()).ok_or(Error::InvalidArgument)?
        };
        image_view(image, view, format)
    }

    pub fn upload_texture(
        &self,
        gpu: &Gpu,
        texture: Texture,
        subresource: u32,
        origin: [u32; 3],
        size: [u32; 3],
        bytes: &[u8],
    ) -> Result<Option<ModernTransfer>, Error> {
        match self {
            Self::Blocks(blocks) => {
                blocks.write(subresource, origin, size, bytes);
                Ok(None)
            }
            Self::Image(image) => {
                let mut destination = region(image, texture, subresource);
                destination.origin = origin.into();
                destination.extent = Extent {
                    width: size[0],
                    height: size[1],
                    depth_or_layers: size[2],
                };
                Ok(Some(texture_transfer(gpu, texture, destination, bytes)?))
            }
            Self::Buffer(_) | Self::Constant(_) => unreachable!(),
        }
    }
}

fn texture_dimension(texture: Texture) -> Dimension {
    if Resource::Texture(texture).misc().contains(Misc::CUBE) {
        Dimension::Cube
    } else {
        match texture.kind() {
            TextureKind::D1 | TextureKind::D2 => Dimension::D2,
            TextureKind::D3 => Dimension::D3,
        }
    }
}

pub fn image_view(image: &Image, view: &View, format: Format) -> Result<ImageView, Error> {
    let range = view.subresources();
    let plane = match (
        image.desc().format.has_stencil(),
        view.kind(),
        view.format(),
    ) {
        (true, ViewKind::ShaderResource, 22 | 47) => 1,
        (true, ViewKind::ShaderResource, 21 | 46) => 2,
        _ => 0,
    };
    let volume_target =
        image.desc().dimension == Dimension::D3 && view.kind() == ViewKind::RenderTarget;
    let dimension = (volume_target
        || (image.desc().dimension == Dimension::Cube && view.kind() != ViewKind::ShaderResource))
        .then_some(Dimension::D2);
    let layers = if volume_target {
        let start = view.attachment_layers().start;
        start..start + 1
    } else {
        range.layers.clone()
    };
    let desc = ViewDesc {
        format: (plane == 0 && format != image.desc().format).then_some(format),
        dimension,
        mips: range.mips.clone(),
        layers,
        plane,
    };
    let components = if view.kind() != ViewKind::ShaderResource {
        ViewComponents::Identity
    } else if matches!(view.format(), 88 | 93)
        || (matches!(view.format(), 6..=8)
            && matches!(
                format,
                Format::Rgba32Float | Format::Rgba32Uint | Format::Rgba32Sint
            ))
    {
        ViewComponents::Rgb
    } else {
        ViewComponents::Identity
    };
    let layout = ViewLayout {
        array: matches!(
            view.kind(),
            ViewKind::ShaderResource | ViewKind::UnorderedAccess
        ) && view.arrayed(),
        components,
    };
    Ok(image.create_view_with_layout(&desc, layout)?)
}

fn alternate_formats(format: u32, backing: Format) -> &'static [Format] {
    use Format::*;
    match format {
        5 if backing == Rgb32Float => &[Rgb32Float, Rgb32Uint, Rgb32Sint],
        1 | 5 => &[Rgba32Float, Rgba32Uint, Rgba32Sint],
        9 => &[
            Rgba16Float,
            Rgba16Unorm,
            Rgba16Uint,
            Rgba16Snorm,
            Rgba16Sint,
        ],
        15 => &[Rg32Float, Rg32Uint, Rg32Sint],
        23 => &[Rgb10a2Unorm, Rgb10a2Uint],
        27 => &[Rgba8Unorm, Rgba8Srgb, Rgba8Uint, Rgba8Snorm, Rgba8Sint],
        33 => &[Rg16Float, Rg16Unorm, Rg16Uint, Rg16Snorm, Rg16Sint],
        39 if !backing.has_depth() => &[R32Float, R32Uint, R32Sint],
        48 => &[Rg8Unorm, Rg8Uint, Rg8Snorm, Rg8Sint],
        53 if !backing.has_depth() => &[R16Float, R16Unorm, R16Uint, R16Snorm, R16Sint],
        60 => &[R8Unorm, R8Uint, R8Snorm, R8Sint],
        90 | 92 => &[Bgra8Unorm, Bgra8Srgb],
        70 => &[Bc1Unorm, Bc1Srgb],
        73 => &[Bc2Unorm, Bc2Srgb],
        76 => &[Bc3Unorm, Bc3Srgb],
        79 => &[Bc4Unorm, Bc4Snorm],
        82 => &[Bc5Unorm, Bc5Snorm],
        94 => &[Bc6hUfloat, Bc6hSfloat],
        97 => &[Bc7Unorm, Bc7Srgb],
        _ => &[],
    }
}

fn is_bc(format: u32) -> bool {
    matches!(format, 70..=84 | 94..=99)
}

pub fn bc_decoded(caps: &Caps, format: u32) -> bool {
    is_bc(format)
        && ((cfg!(target_vendor = "apple") && matches!(format, 70..=72))
            || crate::dxgi::formats::format_layout(format)
                .unwrap()
                .cast_family
                .iter()
                .any(|&entry| caps.format(native_format(entry).unwrap()).is_empty()))
}

pub fn backing_format(caps: &Caps, format: u32) -> Option<Format> {
    use Format::*;
    // All members use the same physical texel width so typeless copies and
    // sample-frequency views keep sharing their original storage.
    let rgb = [Rgb32Float, Rgb32Uint, Rgb32Sint];
    if matches!(format, 5..=8)
        && rgb.into_iter().all(|host| {
            caps.format(host)
                .contains(FormatCaps::SAMPLED | FormatCaps::COLOR)
                && caps.format_sample_counts(host) & 4 != 0
        })
    {
        return Some(match format {
            7 => Rgb32Uint,
            8 => Rgb32Sint,
            _ => Rgb32Float,
        });
    }
    let native = native_format(format)?;
    if bc_decoded(caps, format) {
        let decoded = adv_device::gpu::decoded_bc_format(native).unwrap();
        return caps
            .format(decoded)
            .contains(FormatCaps::SAMPLED)
            .then_some(decoded);
    }
    if native == Format::Depth24UnormStencil8 && caps.format(native).is_empty() {
        return caps
            .format(Format::Depth32FloatStencil8)
            .contains(FormatCaps::SAMPLED | FormatCaps::DEPTH_STENCIL)
            .then_some(Format::Depth32FloatStencil8);
    }
    if format == 115 && caps.format_sample_counts(Abgr4Unorm) & !1 != 0 {
        return Some(Abgr4Unorm);
    }
    if (matches!(format, 85 | 86 | 115) && caps.format(native).is_empty())
        || (matches!(format, 86 | 115)
            && caps.format_sample_counts(native) & !1 == 0
            && packed_resolve(caps, format, Bgra8Unorm).is_some())
    {
        return caps
            .format(Format::Bgra8Unorm)
            .contains(FormatCaps::SAMPLED | FormatCaps::FILTERED)
            .then_some(Format::Bgra8Unorm);
    }
    (!caps.format(native).is_empty()).then_some(native)
}

pub fn native_format(format: u32) -> Option<Format> {
    use Format::*;
    Some(match format {
        1 | 2 => Rgba32Float,
        3 => Rgba32Uint,
        4 => Rgba32Sint,
        5 | 6 => Rgba32Float,
        7 => Rgba32Uint,
        8 => Rgba32Sint,
        9 | 10 => Rgba16Float,
        11 => Rgba16Unorm,
        12 => Rgba16Uint,
        13 => Rgba16Snorm,
        14 => Rgba16Sint,
        15 | 16 => Rg32Float,
        17 => Rg32Uint,
        18 => Rg32Sint,
        19..=22 => Depth32FloatStencil8,
        23 | 24 | 89 => Rgb10a2Unorm,
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
        68 | 69 => Rgba8Unorm,
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

pub fn buffer_format(format: u32) -> Result<(naga::StorageFormat, bool), Error> {
    use naga::StorageFormat::*;
    Ok(match format {
        6 => (Rgba32Float, true),
        7 => (Rgba32Uint, true),
        8 => (Rgba32Sint, true),
        88 => (Bgra8Unorm, true),
        _ => (storage_format(format)?, false),
    })
}

pub fn storage_format(format: u32) -> Result<naga::StorageFormat, Error> {
    use naga::StorageFormat::*;
    Ok(match format {
        2 => Rgba32Float,
        3 => Rgba32Uint,
        4 => Rgba32Sint,
        10 => Rgba16Float,
        11 => Rgba16Unorm,
        12 => Rgba16Uint,
        13 => Rgba16Snorm,
        14 => Rgba16Sint,
        16 => Rg32Float,
        17 => Rg32Uint,
        18 => Rg32Sint,
        24 => Rgb10a2Unorm,
        25 => Rgb10a2Uint,
        26 => Rg11b10Ufloat,
        28 => Rgba8Unorm,
        30 => Rgba8Uint,
        31 => Rgba8Snorm,
        32 => Rgba8Sint,
        34 => Rg16Float,
        35 => Rg16Unorm,
        36 => Rg16Uint,
        37 => Rg16Snorm,
        38 => Rg16Sint,
        41 => R32Float,
        42 => R32Uint,
        43 => R32Sint,
        49 => Rg8Unorm,
        50 => Rg8Uint,
        51 => Rg8Snorm,
        52 => Rg8Sint,
        54 => R16Float,
        56 => R16Unorm,
        57 => R16Uint,
        58 => R16Snorm,
        59 => R16Sint,
        61 => R8Unorm,
        62 => R8Uint,
        63 => R8Snorm,
        64 => R8Sint,
        65 => R8Unorm,
        87 => Bgra8Unorm,
        _ => return Err(Error::InvalidArgument),
    })
}

pub fn storage_uses_buffer(caps: &Caps, format: u32, atomic: bool) -> bool {
    let caps = backing_format(caps, format).map_or(FormatCaps::empty(), |f| caps.format(f));
    !caps.contains(if atomic {
        FormatCaps::STORAGE_ATOMIC
    } else {
        FormatCaps::STORAGE
    })
}

pub fn unfilterable_view(caps: &Caps, format: u32, depth: bool) -> bool {
    let backing = match (format, depth) {
        (41, true) => Some(Format::Depth32Float),
        (56, true) => Some(Format::Depth16Unorm),
        (2 | 11 | 13 | 16 | 21 | 35 | 37 | 41 | 46 | 56, _) => backing_format(caps, format),
        _ => None,
    };
    backing.is_some_and(|format| {
        let features = caps.format(format);
        features.contains(FormatCaps::SAMPLED) && !features.contains(FormatCaps::FILTERED)
    })
}

pub fn image_filter_needed(caps: &Caps, format: Format) -> bool {
    matches!(
        format,
        Format::R32Float
            | Format::Rg32Float
            | Format::Rgba32Float
            | Format::Rg16Unorm
            | Format::Rg16Snorm
            | Format::Rgba16Unorm
            | Format::Rgba16Snorm
    ) && caps
        .format(format)
        .contains(FormatCaps::SAMPLED | FormatCaps::STORAGE)
        && !caps.format(format).contains(FormatCaps::FILTERED)
}

pub fn packed_resolve(caps: &Caps, format: u32, backing: Format) -> Option<[u32; 4]> {
    if backing != Format::Bgra8Unorm
        || !matches!(format, 86 | 115)
        || !caps
            .format(backing)
            .contains(FormatCaps::SAMPLED | FormatCaps::COLOR)
        || caps.format_sample_counts(backing) & !1 == 0
    {
        return None;
    }
    Some(match format {
        86 => [31, 31, 31, 1],
        _ => [15; 4],
    })
}

pub struct LinearStorage {
    pub extent: [u32; 4],
    unit: u32,
    rows: u32,
    slices: u32,
}

impl LinearStorage {
    pub fn new(texture: Texture, view: &View) -> Self {
        let [width, rows, _] = texture.extent(view.subresources().mips.start);
        let slices = if texture.kind() == TextureKind::D3 {
            view.elements().len() as u32
        } else {
            view.subresources().layers.len() as u32
        };
        let unit = u32::from(texture.format().aligned_unit_bytes);
        let pitch = (width * unit).next_multiple_of(256);
        let (height, depth) = if texture.kind() == TextureKind::D1 {
            (slices, 1)
        } else {
            (rows, slices)
        };
        Self {
            extent: [width, height, depth, pitch / unit],
            unit,
            rows,
            slices,
        }
    }

    pub fn alias(
        &self,
        gpu: &Gpu,
        texture: Texture,
        view: &View,
        image: &Image,
    ) -> Result<ImageAlias, Error> {
        let range = view.subresources();
        let mut region = region(
            image,
            texture,
            range.layers.start * texture.levels() + range.mips.start,
        );
        if texture.kind() == TextureKind::D3 {
            region.origin.2 = view.elements().start;
        }
        region.extent.depth_or_layers = self.slices;
        let row_pitch = self.extent[3] * self.unit;
        let buffer = gpu.device().create_buffer(&BufferDesc {
            size: u64::from(row_pitch) * u64::from(self.rows) * u64::from(self.slices),
            usage: BufferUsage::STORAGE | BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
            memory: Memory::Device,
            label: "D3D11 linear UAV",
        })?;
        Ok(ImageAlias {
            image: region,
            linear: TransferBuffer {
                buffer,
                offset: 0,
                row_pitch,
                rows: self.rows,
            },
        })
    }
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
    #[cfg(feature = "d3d9-counters")]
    gpu.count_upload(size + bytes.len() as u64);
    Ok(buffer)
}

pub fn texture_transfer(
    gpu: &Gpu,
    texture: Texture,
    region: ImageRegion,
    bytes: &[u8],
) -> Result<adv_device::gpu::ModernTransfer, Error> {
    if region.image.desc().format.has_stencil() {
        let d24 = texture.format().parent == 44;
        let native_d24 = !cfg!(target_vendor = "apple")
            && region.image.desc().format == Format::Depth24UnormStencil8;
        let mut depth = Vec::with_capacity(bytes.len());
        let mut stencil = Vec::with_capacity(bytes.len() / 4);
        if d24 {
            for &texel in bytes.as_chunks::<4>().0 {
                let unorm = u32::from_le_bytes(texel) & 0x00ff_ffff;
                let depth_bits = if native_d24 {
                    unorm
                } else {
                    (unorm as f32 / 16_777_215.0).to_bits()
                };
                depth.extend_from_slice(&depth_bits.to_le_bytes());
                stencil.push(texel[3]);
            }
        } else {
            for texel in bytes.as_chunks::<8>().0 {
                depth.extend_from_slice(&texel[..4]);
                stencil.push(texel[4]);
            }
        }
        let rows = region.extent.height;
        return Ok(ModernTransfer::UploadDepthStencil {
            depth: transfer_buffer(
                gpu,
                region.extent.width * 4,
                rows,
                region.extent.depth_or_layers,
                &depth,
            )?,
            stencil: transfer_buffer(
                gpu,
                region.extent.width,
                rows,
                region.extent.depth_or_layers,
                &stencil,
            )?,
            destination: region,
        });
    }
    let format = texture.format();
    let unit = transfer_unit(texture, region.image.desc().format);
    let row = region.extent.width.div_ceil(u32::from(format.alignment[0])) * unit;
    let rows = region
        .extent
        .height
        .div_ceil(u32::from(format.alignment[1]));
    let expanded;
    let bytes = match format.format {
        5..=8 if unit == 16 => {
            let padding = if matches!(format.format, 5 | 6) {
                1.0f32.to_le_bytes()
            } else {
                1u32.to_le_bytes()
            };
            expanded = bytes
                .as_chunks::<12>()
                .0
                .iter()
                .flat_map(|texel| texel.iter().copied().chain(padding))
                .collect::<Vec<_>>();
            expanded.as_slice()
        }
        68 | 69 => {
            expanded = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|&[a, b, c, d]| {
                    let (r, g0, bl, g1) = if format.format == 68 {
                        (a, b, c, d)
                    } else {
                        (b, a, d, c)
                    };
                    [r, g0, bl, 255, r, g1, bl, 255]
                })
                .collect();
            expanded.as_slice()
        }
        115 if region.image.desc().format == Format::Abgr4Unorm => {
            expanded = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|&texel| u16::from_le_bytes(texel).rotate_left(4).to_le_bytes())
                .collect();
            expanded.as_slice()
        }
        85 | 86 | 115 if unit == 4 => {
            let widths = packed_widths(format.format);
            expanded = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|&texel| {
                    let mut raw = u16::from_le_bytes(texel);
                    widths.map(|bits| {
                        if bits == 0 {
                            return 255;
                        }
                        let max = (1 << bits) - 1;
                        let value = raw & max;
                        raw >>= bits;
                        ((value * 255 + max / 2) / max) as u8
                    })
                })
                .collect();
            expanded.as_slice()
        }
        _ => bytes,
    };
    let packing = CopyPitch::new(region.image.desc().format, row);
    let source =
        transfer_buffer_with_pitch(gpu, packing, rows, region.extent.depth_or_layers, bytes)?;
    Ok(ModernTransfer::Upload {
        source,
        destination: region,
    })
}

/// Bytes per aligned unit in an image transfer, including any physical padding.
pub fn transfer_unit(texture: Texture, backing: Format) -> u32 {
    match texture.format().format {
        5..=8
            if !matches!(
                backing,
                Format::Rgb32Float | Format::Rgb32Uint | Format::Rgb32Sint
            ) =>
        {
            16
        }
        68 | 69 => 8,
        85 | 86 | 115 if backing == Format::Bgra8Unorm => 4,
        _ => u32::from(texture.format().aligned_unit_bytes),
    }
}

/// Pack a physical row back into the exact byte layout exposed by Map.
pub fn readback_row(texture: Texture, backing: Format, bytes: &[u8], packed: &mut Vec<u8>) {
    packed.clear();
    match texture.format().format {
        5..=8 => packed.extend(
            bytes
                .as_chunks::<16>()
                .0
                .iter()
                .flat_map(|texel| &texel[..12]),
        ),
        68 | 69 => packed.extend(bytes.as_chunks::<8>().0.iter().flat_map(|pair| {
            let [r, g0, b, _, _, g1, _, _] = *pair;
            if texture.format().format == 68 {
                [r, g0, b, g1]
            } else {
                [g0, r, g1, b]
            }
        })),
        115 if backing == Format::Abgr4Unorm => packed.extend(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|&texel| u16::from_le_bytes(texel).rotate_right(4).to_le_bytes()),
        ),
        85 | 86 | 115 => {
            let widths = packed_widths(texture.format().format);
            packed.extend(bytes.as_chunks::<4>().0.iter().flat_map(|texel| {
                let mut raw = 0;
                let mut shift = 0;
                for (&component, bits) in
                    texel.iter().zip(widths).take_while(|(_, bits)| *bits != 0)
                {
                    let max = (1u16 << bits) - 1;
                    let value = (u16::from(component) * max + 127) / 255;
                    raw |= value << shift;
                    shift += bits;
                }
                raw.to_le_bytes()
            }));
        }
        _ => unreachable!(),
    }
}

fn packed_widths(format: u32) -> [u8; 4] {
    match format {
        85 => [5, 6, 5, 0],
        86 => [5, 5, 5, 1],
        115 => [4; 4],
        _ => unreachable!(),
    }
}

pub struct CopyPitch {
    row: u32,
    pub pitch: u32,
}

impl CopyPitch {
    pub fn new(format: Format, row: u32) -> Self {
        // A Vulkan row length must be an integral number of native texels.
        let alignment = match format {
            Format::Rgb32Float | Format::Rgb32Uint | Format::Rgb32Sint => 768,
            _ => 256,
        };
        Self {
            row,
            pitch: row.next_multiple_of(alignment),
        }
    }
}

fn transfer_buffer(
    gpu: &Gpu,
    row: u32,
    rows: u32,
    layers: u32,
    bytes: &[u8],
) -> Result<TransferBuffer, Error> {
    transfer_buffer_with_pitch(
        gpu,
        CopyPitch::new(Format::Rgba8Unorm, row),
        rows,
        layers,
        bytes,
    )
}

fn transfer_buffer_with_pitch(
    gpu: &Gpu,
    packing: CopyPitch,
    rows: u32,
    layers: u32,
    bytes: &[u8],
) -> Result<TransferBuffer, Error> {
    let CopyPitch { row, pitch } = packing;
    let size = u64::from(pitch) * u64::from(rows) * u64::from(layers);
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
    #[cfg(feature = "d3d9-counters")]
    gpu.count_upload(bytes.len() as u64);
    Ok(TransferBuffer {
        buffer,
        offset: 0,
        row_pitch: pitch,
        rows,
    })
}
