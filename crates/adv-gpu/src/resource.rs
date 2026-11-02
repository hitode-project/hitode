//! Images, buffers and samplers, allocated by role.
//!
//! Roles decide memory and compression; usage flags decide what the driver may
//! do with the image. Two rules keep framebuffer compression:
//!
//! - Presentable images come only from [`crate::PresentPool`] and never carry
//!   [`Usage::STORAGE`]. Every writer of a presentable image is a draw.
//! - Render targets do not request `STORAGE` unless a compute pass really
//!   writes them; on some drivers it disables lossless compression.

use crate::{Extent, Format, Result};

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Usage: u32 {
        const SAMPLED = 1 << 0;
        const COLOR = 1 << 1;
        const DEPTH_STENCIL = 1 << 2;
        const STORAGE = 1 << 3;
        const COPY_SRC = 1 << 4;
        const COPY_DST = 1 << 5;
        /// Contents never leave tile memory: load and store must be
        /// `DontCare`. Falls back to ordinary memory without
        /// [`crate::Features::TRANSIENT_ATTACHMENTS`].
        const TRANSIENT = 1 << 6;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Dimension {
    D1,
    D2,
    D3,
    Cube,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageDesc {
    pub dimension: Dimension,
    pub size: Extent,
    pub format: Format,
    pub mips: u32,
    pub samples: u32,
    pub usage: Usage,
    /// Other formats views of this image may use, within its compatibility
    /// class (sRGB siblings, D3D10/11 typeless families). Empty for most
    /// images: declaring formats can cost framebuffer compression (Vulkan
    /// `MUTABLE_FORMAT` with a format list, Metal `PixelFormatView`), so only
    /// images that really reinterpret pay for it.
    pub view_formats: &'static [Format],
    pub label: &'static str,
}

/// A GPU-private image, compressed where the hardware allows. Clone shares the
/// allocation. Content is undefined until written; the first pass that covers
/// it may use `Load::DontCare`.
#[derive(Clone)]
pub struct Image {
    pub(crate) inner: crate::imp::Image,
}

impl Image {
    pub fn desc(&self) -> &ImageDesc {
        self.inner.desc()
    }
    /// The default view: every mip and layer in the image's own format.
    pub fn view(&self) -> &ImageView {
        self.inner.view()
    }
    /// A view of a mip or layer range, optionally in one of the formats the
    /// image declared in [`ImageDesc::view_formats`].
    pub fn create_view(&self, desc: &ViewDesc) -> Result<ImageView> {
        self.inner.create_view(desc)
    }
    /// Identity of the allocation, equal for every clone and view.
    pub fn id(&self) -> u64 {
        self.inner.id()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ViewDesc {
    pub format: Option<Format>,
    pub dimension: Option<Dimension>,
    pub mips: std::ops::Range<u32>,
    pub layers: std::ops::Range<u32>,
    /// For two-plane formats, the plane this view samples.
    pub plane: u32,
}

#[derive(Clone)]
pub struct ImageView {
    pub(crate) inner: crate::imp::ImageView,
}

impl ImageView {
    pub fn image(&self) -> &Image {
        self.inner.image()
    }
    pub fn size(&self) -> Extent {
        self.inner.size()
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct BufferUsage: u32 {
        const VERTEX = 1 << 0;
        const INDEX = 1 << 1;
        const UNIFORM = 1 << 2;
        const STORAGE = 1 << 3;
        const INDIRECT = 1 << 4;
        const COPY_SRC = 1 << 5;
        const COPY_DST = 1 << 6;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Memory {
    /// GPU-only.
    Device,
    /// Mapped for CPU writes and read by the GPU in place: software surface
    /// slots, dynamic guest buffers. Cached where the platform keeps it
    /// coherent; write-combined otherwise.
    Upload,
    /// Mapped for CPU reads after GPU writes: readbacks.
    Readback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BufferDesc {
    pub size: u64,
    pub usage: BufferUsage,
    pub memory: Memory,
    pub label: &'static str,
}

#[derive(Clone)]
pub struct Buffer {
    pub(crate) inner: crate::imp::Buffer,
}

impl Buffer {
    pub fn desc(&self) -> &BufferDesc {
        self.inner.desc()
    }
    /// The mapping of an `Upload` or `Readback` buffer, for the life of the
    /// buffer. The caller orders CPU access against GPU use through tokens:
    /// write only ranges no unfinished submission reads, read only after the
    /// writing submission completes.
    pub fn mapped(&self) -> Option<std::ptr::NonNull<[u8]>> {
        self.inner.mapped()
    }
    pub fn slice(&self, range: std::ops::Range<u64>) -> BufferSlice<'_> {
        self.inner.slice(range)
    }
}

#[derive(Clone, Copy)]
pub struct BufferSlice<'a> {
    pub buffer: &'a Buffer,
    pub offset: u64,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Filter {
    Nearest,
    Linear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Address {
    Repeat,
    Mirror,
    Clamp,
    Border,
    MirrorOnce,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BorderColor {
    TransparentBlack,
    OpaqueBlack,
    OpaqueWhite,
}

#[derive(Clone, Copy, Debug, PartialEq, Hash, Eq)]
pub struct SamplerDesc {
    pub min: Filter,
    pub mag: Filter,
    pub mip: Filter,
    pub address: [Address; 3],
    pub border: BorderColor,
    /// Level-of-detail bias and clamp, in 1/256 levels so the key stays hashable.
    pub lod_bias: i16,
    pub lod_min: u16,
    pub lod_max: u16,
    pub anisotropy: u8,
    pub compare: Option<crate::pipeline::Compare>,
}

/// Samplers are interned by description; equal descriptions share one object.
#[derive(Clone)]
pub struct Sampler {
    #[allow(dead_code)] // read by the backends
    pub(crate) inner: crate::imp::Sampler,
}
