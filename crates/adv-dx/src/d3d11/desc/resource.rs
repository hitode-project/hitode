use super::Error;
use crate::d3d11::{
    abi::{
        D3D11_BUFFER_DESC, D3D11_TEXTURE1D_DESC, D3D11_TEXTURE2D_DESC, D3D11_TEXTURE3D_DESC,
        DXGI_SAMPLE_DESC, limit,
    },
    caps::{FeatureLevel, FormatOperation, Report, Requirement},
};
use crate::dxgi::formats::{FormatLayout, format_layout};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Usage {
    Default,
    Immutable,
    Dynamic,
    Staging,
}

impl Usage {
    fn parse(value: i32) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Default),
            1 => Ok(Self::Immutable),
            2 => Ok(Self::Dynamic),
            3 => Ok(Self::Staging),
            _ => Err(Error::InvalidArgument),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bind(u32);

impl Bind {
    pub const VERTEX: u32 = 1;
    pub const INDEX: u32 = 2;
    pub const CONSTANT: u32 = 4;
    pub const SHADER: u32 = 8;
    pub const STREAM_OUTPUT: u32 = 16;
    pub const RENDER_TARGET: u32 = 32;
    pub const DEPTH_STENCIL: u32 = 64;
    pub const UNORDERED: u32 = 128;

    const INPUTS: u32 = Self::VERTEX | Self::INDEX | Self::CONSTANT | Self::SHADER;
    const OUTPUTS: u32 =
        Self::STREAM_OUTPUT | Self::RENDER_TARGET | Self::DEPTH_STENCIL | Self::UNORDERED;

    pub fn contains(self, flags: u32) -> bool {
        self.0 & flags == flags
    }

    fn intersects(self, flags: u32) -> bool {
        self.0 & flags != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuAccess(u32);

impl CpuAccess {
    const READ: u32 = 0x20000;
    const WRITE: u32 = 0x10000;

    pub fn read(self) -> bool {
        self.0 & Self::READ != 0
    }

    pub fn write(self) -> bool {
        self.0 & Self::WRITE != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Misc(u32);

impl Misc {
    pub const GENERATE_MIPS: u32 = 1;
    pub const SHARED: u32 = 2;
    pub const CUBE: u32 = 4;
    pub const INDIRECT: u32 = 16;
    pub const RAW: u32 = 32;
    pub const STRUCTURED: u32 = 64;
    pub const RESOURCE_CLAMP: u32 = 128;
    pub const GDI_COMPATIBLE: u32 = 512;

    pub fn contains(self, flags: u32) -> bool {
        self.0 & flags == flags
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Access {
    usage: Usage,
    bind: Bind,
    cpu: CpuAccess,
    misc: Misc,
}

impl Access {
    fn parse(usage: i32, bind: u32, cpu: u32, misc: u32) -> Result<Self, Error> {
        let usage = Usage::parse(usage)?;
        if bind & !(Bind::INPUTS | Bind::OUTPUTS) != 0
            || cpu & !(CpuAccess::READ | CpuAccess::WRITE) != 0
        {
            return Err(Error::InvalidArgument);
        }
        Ok(Self {
            usage,
            bind: Bind(bind),
            cpu: CpuAccess(cpu),
            misc: Misc(misc),
        })
    }

    fn validate(self, initial: bool, buffer: bool) -> Result<(), Error> {
        let valid = match self.usage {
            Usage::Default => self.cpu.0 == 0 || (!buffer && self.misc.0 == 0),
            Usage::Immutable => {
                initial
                    && self.cpu.0 == 0
                    && self.bind.0 != 0
                    && !self.bind.intersects(Bind::OUTPUTS)
            }
            Usage::Dynamic => {
                self.cpu.0 == CpuAccess::WRITE
                    && self.bind.0 != 0
                    && !self.bind.intersects(Bind::OUTPUTS)
            }
            Usage::Staging => self.cpu.0 != 0 && self.bind.0 == 0,
        };
        if !valid || (self.usage == Usage::Staging && self.misc.0 != 0) {
            return Err(Error::InvalidArgument);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Buffer {
    size: u32,
    stride: u32,
    access: Access,
}

impl Buffer {
    pub fn new(
        desc: D3D11_BUFFER_DESC,
        initial: bool,
        report: &Report,
        max_size: u64,
    ) -> Result<Self, Error> {
        let access = Access::parse(
            desc.Usage,
            desc.BindFlags,
            desc.CPUAccessFlags,
            desc.MiscFlags,
        )?;
        access.validate(initial, true)?;
        let (bind, misc, size) = (access.bind, access.misc, desc.ByteWidth);
        if size == 0
            || u64::from(size) > max_size
            || bind.contains(Bind::DEPTH_STENCIL)
            || misc.0 & !(Misc::INDIRECT | Misc::RAW | Misc::STRUCTURED) != 0
        {
            return Err(Error::InvalidArgument);
        }
        if bind.contains(Bind::CONSTANT)
            && (bind.0 != Bind::CONSTANT || !size.is_multiple_of(16) || misc.0 != 0)
        {
            return Err(Error::InvalidArgument);
        }
        if (bind.contains(Bind::UNORDERED)
            && report.level < FeatureLevel::L11_0
            && (!report.compute_4x || bind.contains(Bind::RENDER_TARGET)))
            || (report.level < FeatureLevel::L10_0 && bind.contains(Bind::SHADER))
        {
            return Err(Error::InvalidArgument);
        }
        if misc.contains(Misc::INDIRECT)
            && (size < 12 || access.usage != Usage::Default || report.level < FeatureLevel::L11_0)
        {
            return Err(Error::InvalidArgument);
        }
        let stride = if misc.contains(Misc::STRUCTURED) {
            let stride = desc.StructureByteStride;
            if stride == 0
                || stride > limit("D3D11_REQ_MULTI_ELEMENT_STRUCTURE_SIZE_IN_BYTES")
                || !stride.is_multiple_of(4)
                || !size.is_multiple_of(stride)
                || bind.intersects(
                    Bind::VERTEX | Bind::INDEX | Bind::STREAM_OUTPUT | Bind::RENDER_TARGET,
                )
                || misc.contains(Misc::RAW)
            {
                return Err(Error::InvalidArgument);
            }
            stride
        } else {
            if misc.contains(Misc::RAW) && !bind.intersects(Bind::SHADER | Bind::UNORDERED) {
                return Err(Error::InvalidArgument);
            }
            0
        };
        Ok(Self {
            size,
            stride,
            access,
        })
    }

    pub fn size(self) -> u32 {
        self.size
    }

    pub fn stride(self) -> u32 {
        self.stride
    }

    pub fn desc(self) -> D3D11_BUFFER_DESC {
        D3D11_BUFFER_DESC {
            ByteWidth: self.size,
            Usage: self.access.usage as i32,
            BindFlags: self.access.bind.0,
            CPUAccessFlags: self.access.cpu.0,
            MiscFlags: self.access.misc.0,
            StructureByteStride: self.stride,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureKind {
    D1,
    D2,
    D3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Texture {
    kind: TextureKind,
    size: [u32; 3],
    layers: u32,
    mips: u32,
    format: &'static FormatLayout,
    samples: u32,
    quality: u32,
    access: Access,
}

struct TextureDesc {
    kind: TextureKind,
    size: [u32; 3],
    layers: u32,
    mips: u32,
    format: u32,
    samples: u32,
    quality: u32,
    access: Access,
}

impl Texture {
    pub fn new_1d(
        desc: D3D11_TEXTURE1D_DESC,
        initial: bool,
        report: &Report,
    ) -> Result<Self, Error> {
        Self::new(
            TextureDesc {
                kind: TextureKind::D1,
                size: [desc.Width, 1, 1],
                layers: desc.ArraySize,
                mips: desc.MipLevels,
                format: desc.Format as u32,
                samples: 1,
                quality: 0,
                access: Access::parse(
                    desc.Usage,
                    desc.BindFlags,
                    desc.CPUAccessFlags,
                    desc.MiscFlags,
                )?,
            },
            initial,
            report,
        )
    }

    pub fn new_2d(
        desc: D3D11_TEXTURE2D_DESC,
        initial: bool,
        report: &Report,
    ) -> Result<Self, Error> {
        Self::new(
            TextureDesc {
                kind: TextureKind::D2,
                size: [desc.Width, desc.Height, 1],
                layers: desc.ArraySize,
                mips: desc.MipLevels,
                format: desc.Format as u32,
                samples: desc.SampleDesc.Count,
                quality: desc.SampleDesc.Quality,
                access: Access::parse(
                    desc.Usage,
                    desc.BindFlags,
                    desc.CPUAccessFlags,
                    desc.MiscFlags,
                )?,
            },
            initial,
            report,
        )
    }

    pub fn new_3d(
        desc: D3D11_TEXTURE3D_DESC,
        initial: bool,
        report: &Report,
    ) -> Result<Self, Error> {
        Self::new(
            TextureDesc {
                kind: TextureKind::D3,
                size: [desc.Width, desc.Height, desc.Depth],
                layers: 1,
                mips: desc.MipLevels,
                format: desc.Format as u32,
                samples: 1,
                quality: 0,
                access: Access::parse(
                    desc.Usage,
                    desc.BindFlags,
                    desc.CPUAccessFlags,
                    desc.MiscFlags,
                )?,
            },
            initial,
            report,
        )
    }

    fn new(desc: TextureDesc, initial: bool, report: &Report) -> Result<Self, Error> {
        let TextureDesc {
            kind,
            size,
            layers,
            mips,
            format,
            samples,
            quality,
            access,
        } = desc;
        access.validate(initial, false)?;
        let (bind, misc) = (access.bind, access.misc);
        let cube = misc.contains(Misc::CUBE);
        let (operation, limit) = match (kind, cube) {
            (TextureKind::D1, false) => (FormatOperation::Texture1d, Requirement::Texture1d),
            (TextureKind::D2, false) => (FormatOperation::Texture2d, Requirement::Texture2d),
            (TextureKind::D2, true) => (FormatOperation::TextureCube, Requirement::TextureCube),
            (TextureKind::D3, false) => (FormatOperation::Texture3d, Requirement::Texture3d),
            (TextureKind::D1 | TextureKind::D3, true) => return Err(Error::InvalidArgument),
        };
        let layout = format_layout(format).ok_or(Error::InvalidArgument)?;
        let size_limit = report.level.limit(limit);
        if !report.supports(format, operation)
            || size
                .iter()
                .zip(layout.alignment)
                .any(|(&dimension, alignment)| {
                    dimension == 0
                        || dimension > size_limit
                        || !dimension.is_multiple_of(u32::from(alignment))
                })
            || layers == 0
            || (cube && (size[0] != size[1] || layers < 6))
        {
            return Err(Error::InvalidArgument);
        }
        let array_limit = report.level.limit(Requirement::TextureArrayLayers);
        let limit = if cube {
            array_limit.max(6)
        } else {
            array_limit.max(1)
        };
        if layers > limit
            || (cube && layers > 6 && report.level.limit(Requirement::CubeArrays) == 0)
        {
            return Err(Error::InvalidArgument);
        }
        let max_mips = u32::BITS - size.into_iter().max().unwrap().leading_zeros();
        let resolved_mips = if mips == 0 { max_mips } else { mips };
        if resolved_mips > max_mips
            || (resolved_mips > 1 && !report.supports(format, FormatOperation::Mip))
            || (access.usage == Usage::Dynamic && (resolved_mips != 1 || layers != 1))
            || (samples != 1
                && (mips != 1
                    || initial
                    || bind.0 == 0
                    || access.usage != Usage::Default
                    || cube
                    || (report.level < FeatureLevel::L10_0 && bind.contains(Bind::SHADER))))
            || !report.supports_multisample_quality(format, samples, quality)
        {
            return Err(Error::InvalidArgument);
        }
        if bind.intersects(Bind::VERTEX | Bind::INDEX | Bind::CONSTANT | Bind::STREAM_OUTPUT)
            || bind.contains(Bind::DEPTH_STENCIL | Bind::RENDER_TARGET)
            || (bind.contains(Bind::DEPTH_STENCIL) && kind == TextureKind::D3)
            || (bind.contains(Bind::UNORDERED)
                && (report.level < FeatureLevel::L11_0 || samples != 1))
            || misc.0
                & !(Misc::GENERATE_MIPS
                    | Misc::SHARED
                    | Misc::CUBE
                    | Misc::RESOURCE_CLAMP
                    | Misc::GDI_COMPATIBLE)
                != 0
            || (misc.contains(Misc::SHARED)
                && (kind != TextureKind::D2
                    || layers != 1
                    || resolved_mips != 1
                    || samples != 1
                    || access.usage != Usage::Default
                    || !report.supports(format, FormatOperation::Share)))
            || (misc.contains(Misc::GENERATE_MIPS)
                && !bind.contains(Bind::SHADER | Bind::RENDER_TARGET))
            || (misc.contains(Misc::RESOURCE_CLAMP) && !bind.contains(Bind::SHADER))
            || (misc.contains(Misc::GDI_COMPATIBLE)
                && (kind != TextureKind::D2
                    || !matches!(format, 87 | 90 | 91)
                    || layers != 1
                    || resolved_mips != 1
                    || samples != 1
                    || !bind.contains(Bind::RENDER_TARGET)
                    || access.usage != Usage::Default))
        {
            return Err(Error::InvalidArgument);
        }
        for (flag, operation) in [
            (Bind::SHADER, FormatOperation::ShaderLoad),
            (Bind::RENDER_TARGET, FormatOperation::RenderTarget),
            (Bind::DEPTH_STENCIL, FormatOperation::DepthStencil),
            (Bind::UNORDERED, FormatOperation::StorageView),
        ] {
            if bind.contains(flag)
                && !report.supports(format, operation)
                && !(layout.parent == format
                    && layout.type_level != crate::dxgi::formats::TypeLevel::FullType
                    && layout
                        .cast_family
                        .iter()
                        .any(|&format| report.supports(format, operation)))
                && !(flag == Bind::SHADER && report.supports(format, FormatOperation::ShaderSample))
            {
                return Err(Error::InvalidArgument);
            }
        }
        Ok(Self {
            kind,
            size,
            layers,
            mips: resolved_mips,
            format: layout,
            samples,
            quality,
            access,
        })
    }

    pub fn kind(self) -> TextureKind {
        self.kind
    }

    pub fn levels(self) -> u32 {
        self.mips
    }

    pub fn layers(self) -> u32 {
        self.layers
    }

    pub fn format(self) -> &'static FormatLayout {
        self.format
    }

    pub fn samples(self) -> u32 {
        self.samples
    }

    pub fn extent(self, mip: u32) -> [u32; 3] {
        self.size.map(|dimension| (dimension >> mip).max(1))
    }

    pub fn desc_1d(self) -> D3D11_TEXTURE1D_DESC {
        D3D11_TEXTURE1D_DESC {
            Width: self.size[0],
            MipLevels: self.mips,
            ArraySize: self.layers,
            Format: self.format.format as i32,
            Usage: self.access.usage as i32,
            BindFlags: self.access.bind.0,
            CPUAccessFlags: self.access.cpu.0,
            MiscFlags: self.access.misc.0,
        }
    }

    pub fn desc_2d(self) -> D3D11_TEXTURE2D_DESC {
        D3D11_TEXTURE2D_DESC {
            Width: self.size[0],
            Height: self.size[1],
            MipLevels: self.mips,
            ArraySize: self.layers,
            Format: self.format.format as i32,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: self.samples,
                Quality: self.quality,
            },
            Usage: self.access.usage as i32,
            BindFlags: self.access.bind.0,
            CPUAccessFlags: self.access.cpu.0,
            MiscFlags: self.access.misc.0,
        }
    }

    pub fn desc_3d(self) -> D3D11_TEXTURE3D_DESC {
        D3D11_TEXTURE3D_DESC {
            Width: self.size[0],
            Height: self.size[1],
            Depth: self.size[2],
            MipLevels: self.mips,
            Format: self.format.format as i32,
            Usage: self.access.usage as i32,
            BindFlags: self.access.bind.0,
            CPUAccessFlags: self.access.cpu.0,
            MiscFlags: self.access.misc.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Buffer(Buffer),
    Texture(Texture),
}

impl Resource {
    fn access(self) -> Access {
        match self {
            Self::Buffer(buffer) => buffer.access,
            Self::Texture(texture) => texture.access,
        }
    }

    pub fn usage(self) -> Usage {
        self.access().usage
    }

    pub fn bind(self) -> Bind {
        self.access().bind
    }

    pub fn cpu_access(self) -> CpuAccess {
        self.access().cpu
    }

    pub fn misc(self) -> Misc {
        self.access().misc
    }

    pub fn subresources(self) -> u32 {
        match self {
            Self::Buffer(_) => 1,
            Self::Texture(texture) => texture.mips * texture.layers,
        }
    }

    pub fn dimension(self) -> u32 {
        match self {
            Self::Buffer(_) => 1,
            Self::Texture(texture) => match texture.kind {
                TextureKind::D1 => 2,
                TextureKind::D2 => 3,
                TextureKind::D3 => 4,
            },
        }
    }
}
