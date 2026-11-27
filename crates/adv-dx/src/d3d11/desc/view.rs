use super::{Bind, Buffer, Error, Misc, Resource, Texture, TextureKind};
use crate::d3d11::{
    abi::{
        D3D11_DEPTH_STENCIL_VIEW_DESC, D3D11_RENDER_TARGET_VIEW_DESC,
        D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_UNORDERED_ACCESS_VIEW_DESC,
    },
    caps::{FeatureLevel, FormatOperation, Report},
};
use crate::dxgi::formats::{TypeLevel, format_layout};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewKind {
    ShaderResource,
    RenderTarget,
    DepthStencil,
    UnorderedAccess,
}

impl ViewKind {
    fn bind(self) -> u32 {
        match self {
            Self::ShaderResource => Bind::SHADER,
            Self::RenderTarget => Bind::RENDER_TARGET,
            Self::DepthStencil => Bind::DEPTH_STENCIL,
            Self::UnorderedAccess => Bind::UNORDERED,
        }
    }

    fn operation(self) -> FormatOperation {
        match self {
            Self::ShaderResource => FormatOperation::ShaderLoad,
            Self::RenderTarget => FormatOperation::RenderTarget,
            Self::DepthStencil => FormatOperation::DepthStencil,
            Self::UnorderedAccess => FormatOperation::StorageView,
        }
    }
}

/// Buffer views and 3D W slices share their containing subresource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subresources {
    pub mips: Range<u32>,
    pub layers: Range<u32>,
}

impl Subresources {
    pub fn overlaps(&self, other: &Self) -> bool {
        fn overlaps(a: &Range<u32>, b: &Range<u32>) -> bool {
            a.start < b.end && b.start < a.end
        }
        overlaps(&self.mips, &other.mips) && overlaps(&self.layers, &other.layers)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aspects(u8);

impl Aspects {
    pub const COLOR: Self = Self(4);
    pub const DEPTH: Self = Self(1);
    pub const STENCIL: Self = Self(2);

    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    desc: ViewDesc,
    kind: ViewKind,
    range: Subresources,
    elements: Range<u32>,
    aspects: Aspects,
    writable_aspects: Aspects,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewDesc {
    format: u32,
    dimension: i32,
    flags: u32,
    words: [u32; 4],
}

fn words<const N: usize>(bytes: [u8; N]) -> [u32; 4] {
    let mut words = [0; 4];
    for (word, bytes) in words.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*bytes);
    }
    words
}

fn encode<const N: usize>(words: [u32; 4]) -> [u8; N] {
    let mut bytes = [0; N];
    for (bytes, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        *bytes = word.to_le_bytes();
    }
    bytes
}

impl From<D3D11_SHADER_RESOURCE_VIEW_DESC> for ViewDesc {
    fn from(value: D3D11_SHADER_RESOURCE_VIEW_DESC) -> Self {
        Self {
            format: value.Format as u32,
            dimension: value.ViewDimension,
            flags: 0,
            words: words(value.Anonymous),
        }
    }
}

impl From<D3D11_RENDER_TARGET_VIEW_DESC> for ViewDesc {
    fn from(value: D3D11_RENDER_TARGET_VIEW_DESC) -> Self {
        Self {
            format: value.Format as u32,
            dimension: value.ViewDimension,
            flags: 0,
            words: words(value.Anonymous),
        }
    }
}

impl From<D3D11_DEPTH_STENCIL_VIEW_DESC> for ViewDesc {
    fn from(value: D3D11_DEPTH_STENCIL_VIEW_DESC) -> Self {
        Self {
            format: value.Format as u32,
            dimension: value.ViewDimension,
            flags: value.Flags,
            words: words(value.Anonymous),
        }
    }
}

impl From<D3D11_UNORDERED_ACCESS_VIEW_DESC> for ViewDesc {
    fn from(value: D3D11_UNORDERED_ACCESS_VIEW_DESC) -> Self {
        Self {
            format: value.Format as u32,
            dimension: value.ViewDimension,
            flags: 0,
            words: words(value.Anonymous),
        }
    }
}

impl View {
    pub fn new(
        kind: ViewKind,
        resource: Resource,
        desc: Option<ViewDesc>,
        report: &Report,
    ) -> Result<Self, Error> {
        if !resource.bind().contains(kind.bind()) {
            return Err(Error::InvalidArgument);
        }
        match resource {
            Resource::Buffer(buffer) => Self::buffer(kind, resource.misc(), buffer, desc, report),
            Resource::Texture(texture) => Self::texture(kind, resource, texture, desc, report),
        }
    }

    fn buffer(
        kind: ViewKind,
        misc: Misc,
        buffer: Buffer,
        desc: Option<ViewDesc>,
        report: &Report,
    ) -> Result<Self, Error> {
        if kind == ViewKind::DepthStencil {
            return Err(Error::InvalidArgument);
        }
        let structured = misc.contains(Misc::STRUCTURED);
        let mut desc = match desc {
            Some(desc) => desc,
            None if structured && kind != ViewKind::RenderTarget => ViewDesc {
                format: 0,
                dimension: 1,
                flags: 0,
                words: [0, buffer.size() / buffer.stride(), 0, 0],
            },
            None => return Err(Error::InvalidArgument),
        };
        let [first, count, flags, _] = desc.words;
        let stride = match (kind, desc.dimension, flags) {
            (ViewKind::ShaderResource, 11, 1) | (ViewKind::UnorderedAccess, 1, 1)
                if misc.contains(Misc::RAW) && desc.format == 39 =>
            {
                4
            }
            (ViewKind::ShaderResource, 1 | 11, 0) | (ViewKind::UnorderedAccess, 1, 0 | 2 | 4)
                if structured && desc.format == 0 =>
            {
                buffer.stride()
            }
            (
                ViewKind::ShaderResource | ViewKind::RenderTarget | ViewKind::UnorderedAccess,
                1,
                0,
            ) if !structured => {
                if kind == ViewKind::UnorderedAccess && report.level < FeatureLevel::L11_0 {
                    return Err(Error::UnsupportedView);
                }
                let layout = format_layout(desc.format).ok_or(Error::InvalidArgument)?;
                if layout.type_level != TypeLevel::FullType
                    || !report.supports(desc.format, FormatOperation::Buffer)
                    || !report.supports(desc.format, kind.operation())
                {
                    return Err(Error::InvalidArgument);
                }
                u32::from(layout.aligned_unit_bytes)
            }
            _ => return Err(Error::InvalidArgument),
        };
        let elements = range(first, count, buffer.size() / stride, false)?;
        desc.words = [first, count, flags, 0];
        Ok(Self::make(desc, kind, 0..1, 0..1, elements, Aspects::COLOR))
    }

    fn texture(
        kind: ViewKind,
        resource: Resource,
        texture: Texture,
        desc: Option<ViewDesc>,
        report: &Report,
    ) -> Result<Self, Error> {
        let shader = kind == ViewKind::ShaderResource;
        let depth = kind == ViewKind::DepthStencil;
        let ms = texture.samples() > 1;
        let cube = shader && resource.misc().contains(Misc::CUBE) && texture.layers() >= 6;
        let source = texture.format();
        let typeless = source.parent == source.format && source.type_level != TypeLevel::FullType;
        let mut desc = match desc {
            Some(desc) => desc,
            None if !typeless => {
                let array = texture.layers() > 1;
                let layers = texture.layers();
                let (dim, words) = match (texture.kind(), ms, array, cube) {
                    (TextureKind::D1, _, false, _) => (2, [0, texture.levels(), 0, 0]),
                    (TextureKind::D1, _, true, _) => (3, array_words(shader, texture, layers)),
                    (TextureKind::D2, true, false, _) => (6, [0; 4]),
                    (TextureKind::D2, true, true, _) => (7, [0, layers, 0, 0]),
                    (TextureKind::D2, false, _, true) if layers >= 12 => {
                        (10, [0, texture.levels(), 0, layers / 6])
                    }
                    (TextureKind::D2, false, _, true) => (9, [0, texture.levels(), 0, 0]),
                    (TextureKind::D2, false, false, false) => (4, [0, texture.levels(), 0, 0]),
                    (TextureKind::D2, false, true, false) => {
                        (5, array_words(shader, texture, layers))
                    }
                    (TextureKind::D3, _, _, _) => (
                        8,
                        if shader {
                            [0, texture.levels(), 0, 0]
                        } else {
                            [0, 0, texture.extent(0)[2], 0]
                        },
                    ),
                };
                ViewDesc {
                    format: source.format,
                    dimension: dim - i32::from(depth),
                    flags: 0,
                    words,
                }
            }
            None => return Err(Error::InvalidArgument),
        };
        if desc.format == 0 {
            desc.format = source.format;
        }
        let target = format_layout(desc.format).ok_or(Error::InvalidArgument)?;
        if (source.format != target.format
            && !(typeless && source.cast_family.contains(&target.format)))
            || (target.type_level != TypeLevel::FullType && target.parent == target.format)
            || !(report.supports(desc.format, kind.operation())
                || (shader && report.supports(desc.format, FormatOperation::ShaderSample)))
        {
            return Err(Error::InvalidArgument);
        }

        let dimension = if depth {
            match desc.dimension {
                1..=6 => desc.dimension + 1,
                _ => return Err(Error::InvalidArgument),
            }
        } else {
            desc.dimension
        };
        let [a, b, c, d] = desc.words;
        let (ms, single, array, volume) = match (texture.kind(), ms, dimension) {
            (TextureKind::D1, false, 2) | (TextureKind::D2, false, 4) => {
                (false, true, false, false)
            }
            (TextureKind::D1, false, 3) | (TextureKind::D2, false, 5) => {
                (false, false, true, false)
            }
            (TextureKind::D2, true, 6) if kind != ViewKind::UnorderedAccess => {
                (true, true, false, false)
            }
            (TextureKind::D2, true, 7) if kind != ViewKind::UnorderedAccess => {
                (true, false, true, false)
            }
            (TextureKind::D3, false, 8) if !depth => (false, false, false, true),
            (TextureKind::D2, false, 9 | 10) if cube => (false, false, false, false),
            _ => return Err(Error::InvalidArgument),
        };
        let mips = if ms {
            0..1
        } else {
            range(a, if shader { b } else { 1 }, texture.levels(), true)?
        };
        let layers = match (ms, single, array, volume, dimension) {
            (true, _, true, _, _) => range(a, b, texture.layers(), true)?,
            (false, _, true, _, _) if shader => range(c, d, texture.layers(), true)?,
            (false, _, true, _, _) => range(b, c, texture.layers(), true)?,
            (_, _, _, _, 9) => 0..6,
            (_, _, _, _, 10)
                if report
                    .level
                    .limit(crate::d3d11::caps::Requirement::CubeArrays)
                    != 0 =>
            {
                let faces = if d == u32::MAX {
                    d
                } else {
                    d.checked_mul(6).ok_or(Error::InvalidArgument)?
                };
                let layers = range(c, faces, texture.layers(), true)?;
                if layers.len() % 6 != 0 {
                    return Err(Error::InvalidArgument);
                }
                layers
            }
            (_, true, _, _, _) | (_, _, _, true, _) => 0..1,
            _ => return Err(Error::InvalidArgument),
        };
        let elements = if volume && !shader {
            range(b, c, texture.extent(mips.start)[2], true)?
        } else {
            0..1
        };
        let aspects = match desc.format {
            20 | 45 => Aspects(3),
            21 | 46 => Aspects::DEPTH,
            22 | 47 => Aspects::STENCIL,
            40 | 55 => Aspects::DEPTH,
            41 | 56 if resource.bind().contains(Bind::DEPTH_STENCIL) => Aspects::DEPTH,
            _ => Aspects::COLOR,
        };
        if depth && desc.flags & !u32::from(aspects.0) != 0 {
            return Err(Error::InvalidArgument);
        }

        let nmips = mips.end - mips.start;
        let nlayers = layers.end - layers.start;
        desc.words = match (ms, shader, array, volume, dimension) {
            (true, _, false, _, _) => [0; 4],
            (true, _, true, _, _) => [layers.start, nlayers, 0, 0],
            (_, true, _, _, 10) => [mips.start, nmips, layers.start, nlayers / 6],
            (_, true, true, _, _) => [mips.start, nmips, layers.start, nlayers],
            (_, true, false, _, _) => [mips.start, nmips, 0, 0],
            (_, false, true, _, _) => [mips.start, layers.start, nlayers, 0],
            (_, false, false, true, _) => {
                [mips.start, elements.start, elements.end - elements.start, 0]
            }
            (_, false, false, false, _) => [mips.start, 0, 0, 0],
        };
        Ok(Self::make(desc, kind, mips, layers, elements, aspects))
    }

    fn make(
        desc: ViewDesc,
        kind: ViewKind,
        mips: Range<u32>,
        layers: Range<u32>,
        elements: Range<u32>,
        aspects: Aspects,
    ) -> Self {
        let writable_aspects = match kind {
            ViewKind::ShaderResource => Aspects(0),
            ViewKind::DepthStencil => Aspects(aspects.0 & !(desc.flags as u8)),
            ViewKind::RenderTarget | ViewKind::UnorderedAccess => aspects,
        };
        Self {
            desc,
            kind,
            range: Subresources { mips, layers },
            elements,
            aspects,
            writable_aspects,
        }
    }

    #[cfg(feature = "gpu")]
    pub fn has_counter(&self) -> bool {
        self.kind == ViewKind::UnorderedAccess
            && self.desc.dimension == 1
            && self.desc.words[2] & 6 != 0
    }

    pub fn kind(&self) -> ViewKind {
        self.kind
    }

    pub fn format(&self) -> u32 {
        self.desc.format
    }

    pub fn subresources(&self) -> &Subresources {
        &self.range
    }

    /// Buffer elements or W slices for a 3D output.
    pub fn elements(&self) -> Range<u32> {
        self.elements.clone()
    }

    #[cfg(feature = "gpu")]
    pub fn attachment_layers(&self) -> Range<u32> {
        if self.kind == ViewKind::RenderTarget && self.desc.dimension == 8 {
            self.elements()
        } else {
            self.range.layers.clone()
        }
    }

    pub fn aspects(&self) -> Aspects {
        self.aspects
    }

    pub fn writable_aspects(&self) -> Aspects {
        self.writable_aspects
    }

    pub fn srv_desc(&self) -> D3D11_SHADER_RESOURCE_VIEW_DESC {
        D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: self.desc.format as i32,
            ViewDimension: self.desc.dimension,
            Anonymous: encode(self.desc.words),
        }
    }

    pub fn rtv_desc(&self) -> D3D11_RENDER_TARGET_VIEW_DESC {
        D3D11_RENDER_TARGET_VIEW_DESC {
            Format: self.desc.format as i32,
            ViewDimension: self.desc.dimension,
            Anonymous: encode(self.desc.words),
        }
    }

    pub fn dsv_desc(&self) -> D3D11_DEPTH_STENCIL_VIEW_DESC {
        D3D11_DEPTH_STENCIL_VIEW_DESC {
            Format: self.desc.format as i32,
            ViewDimension: self.desc.dimension,
            Flags: self.desc.flags,
            Anonymous: encode(self.desc.words),
        }
    }

    pub fn uav_desc(&self) -> D3D11_UNORDERED_ACCESS_VIEW_DESC {
        D3D11_UNORDERED_ACCESS_VIEW_DESC {
            Format: self.desc.format as i32,
            ViewDimension: self.desc.dimension,
            Anonymous: encode(self.desc.words),
        }
    }
}

fn array_words(shader: bool, texture: Texture, layers: u32) -> [u32; 4] {
    if shader {
        [0, texture.levels(), 0, layers]
    } else {
        [0, 0, layers, 0]
    }
}

fn range(start: u32, count: u32, max: u32, remaining: bool) -> Result<Range<u32>, Error> {
    let end = if remaining && count == u32::MAX {
        max
    } else {
        start.checked_add(count).ok_or(Error::InvalidArgument)?
    };
    if start >= end || end > max {
        return Err(Error::InvalidArgument);
    }
    Ok(start..end)
}
