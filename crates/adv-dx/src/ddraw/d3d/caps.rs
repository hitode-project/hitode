//! Legacy capabilities derived from fixed-function coverage and resource backing.
#![deny(clippy::todo, clippy::unimplemented, clippy::wildcard_enum_match_arm)]

use adv_abi::{gfx_iids::*, types::*};
use adv_gpu::{Caps, Features, Format, FormatCaps};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Rgb,
    Hal,
    TnlHal,
}

impl Device {
    pub fn name(self) -> (&'static str, &'static str) {
        match self {
            Self::Rgb => ("RGB Emulation", "Microsoft Direct3D RGB Software Emulation"),
            Self::Hal => (
                "Direct3D HAL",
                "Microsoft Direct3D Hardware acceleration through Direct3D HAL",
            ),
            Self::TnlHal => (
                "Direct3D T&L HAL",
                "Microsoft Direct3D Hardware Transform and Lighting acceleration capable device",
            ),
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Rgb => 0,
            Self::Hal => 1,
            Self::TnlHal => 2,
        }
    }

    pub fn guid(self) -> [u8; 16] {
        match self {
            Self::Rgb => IID_IDIRECT3DRGBDEVICE,
            Self::Hal => IID_IDIRECT3DHALDEVICE,
            Self::TnlHal => IID_IDIRECT3DTNLHALDEVICE,
        }
    }
}

/// One distinct fixed-function execution path. Each capability row below
/// identifies the descriptor bits allowed when the path is executing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    TransformedVertices,
    ExecuteBuffer,
    LegacyVertexBuffer,
    TextureStorage,
    RenderAfterFlip,
    DrawPrimitives2,
    Tnl,
    Upload,
    HardwareRaster,
    TexturePerspective,
    TextureAlpha,
    ColorKey,
    PowerOfTwoValidation,
    BorderColor,
    TextureRepeat,
    PaletteAlpha,
    ProjectedTexture,
    CubeTexture,
    Filter,
    AnisotropicFilter,
    Blend,
    CombinedBlend,
    DepthComparison,
    AlphaComparison,
    Dither,
    SoftwareRasterOps,
    SubpixelRaster,
    Fog,
    AntialiasedRaster,
    MipBias,
    DepthBias,
    Shading,
    TextureBlend,
    Address,
    Cull,
    Stencil,
    VertexLayout,
    TextureCombiner,
    ExtendedTextureCombiner,
    BumpMapping,
    TextureGeneration,
    MaterialSources,
    LightTypes,
    LocalViewer,
}

#[derive(Clone, Copy)]
struct Row {
    path: Path,
    masks: [u32; 3],
}

const fn row(path: Path, rgb: u32, hal: u32, tnl: u32) -> Row {
    Row {
        path,
        masks: [rgb, hal, tnl],
    }
}
const fn both(path: Path, rgb: u32, hardware: u32) -> Row {
    row(path, rgb, hardware, hardware)
}
const fn all(path: Path, mask: u32) -> Row {
    row(path, mask, mask, mask)
}

use Path::*;

// The three columns are the RGB, HAL and T&L HAL device classes from
// d3d7_semantics. The fixed-function paths partition each descriptor's bits.
// GFX-CAP-6 / descriptors_follow_the_fixed_function_paths.
const DEVICE: &[Row] = &[
    all(TransformedVertices, 0x401),
    both(ExecuteBuffer, 0x10, 0x30),
    both(LegacyVertexBuffer, 0x40, 0xc0),
    both(TextureStorage, 0x100, 0x1200),
    both(RenderAfterFlip, 0, 0x800),
    both(DrawPrimitives2, 0, 0xa000),
    row(Tnl, 0, 0, 0x10000),
    both(Upload, 0, 0x20000),
    both(HardwareRaster, 0, 0x02180000),
];
const TEXTURE: &[Row] = &[
    all(TexturePerspective, 1),
    all(TextureAlpha, 4),
    all(ColorKey, 8),
    both(PowerOfTwoValidation, 2, 0),
    both(BorderColor, 0x10, 0),
    all(TextureRepeat, 0x40),
    all(PaletteAlpha, 0x80),
    both(ProjectedTexture, 0, 0x400),
    both(CubeTexture, 0, 0x800),
];
const RASTER: &[Row] = &[
    all(Dither, 1),
    both(SoftwareRasterOps, 6, 0),
    all(DepthComparison, 0x10),
    both(SubpixelRaster, 0x60, 0x20),
    both(Fog, 0x00210180, 0x00310180),
    both(AntialiasedRaster, 0, 0x1800),
    all(MipBias, 0x2000),
    both(DepthBias, 0, 0x4000),
    both(AnisotropicFilter, 0, 0x20000),
];
const FILTER: &[Row] = &[all(Filter, 0x0303033f), both(AnisotropicFilter, 0, 0x400)];
const BLEND_SOURCE: &[Row] = &[all(Blend, 0x7ff), all(CombinedBlend, 0x1800)];
const BLEND_DESTINATION: &[Row] = &[all(Blend, 0x7ff), both(CombinedBlend, 0, 0x1800)];
const TEXTURE_OP: &[Row] = &[
    all(TextureCombiner, 0x78ff),
    both(ExtendedTextureCombiner, 0, 0x039e8700),
    both(BumpMapping, 0, 0x00600000),
];
const VERTEX_PROCESSING: &[Row] = &[
    all(TextureGeneration, 1),
    all(MaterialSources, 2),
    all(Fog, 4),
    all(LightTypes, 0x18),
    all(LocalViewer, 0x20),
];

#[derive(Clone, Copy, Default)]
pub struct Coverage<'a> {
    pub paths: &'a [Path],
    pub textures: &'a [TextureFormat],
    pub depths: &'a [DepthFormat],
    pub max_texture_extent: u32,
    pub max_texture_repeat: u32,
    pub max_anisotropy: u32,
    pub max_stages: u16,
    pub max_textures: u16,
    pub max_lights: u32,
    pub clip_planes: u16,
    pub vertex_blend_matrices: u16,
    pub max_vertices: u32,
    pub converted_packed_render_targets: bool,
    pub render_formats: &'a [Format],
    pub converted_textures: &'a [TextureFormat],
    pub converted_depths: &'a [DepthFormat],
}

impl Coverage<'_> {
    fn enabled(self, path: Path) -> bool {
        self.paths.contains(&path)
    }

    fn bits(self, device: Device, rows: &[Row]) -> u32 {
        rows.iter()
            .filter(|row| self.enabled(row.path))
            .fold(0, |bits, row| bits | row.masks[device.index()])
    }

    fn primitive(self, device: Device) -> D3DPRIMCAPS {
        D3DPRIMCAPS {
            dwSize: size_of::<D3DPRIMCAPS>() as u32,
            dwMiscCaps: self.bits(device, &[all(Cull, 0x70), all(DepthComparison, 2)]),
            dwRasterCaps: self.bits(device, RASTER),
            dwZCmpCaps: self.bits(device, &[all(DepthComparison, 0xff)]),
            dwSrcBlendCaps: self.bits(device, BLEND_SOURCE),
            dwDestBlendCaps: self.bits(device, BLEND_DESTINATION),
            dwAlphaCmpCaps: self.bits(device, &[all(AlphaComparison, 0xff)]),
            dwShadeCaps: self.bits(device, &[both(Shading, 0x000c528a, 0x000c528f)]),
            dwTextureCaps: self.bits(device, TEXTURE),
            dwTextureFilterCaps: self.bits(device, FILTER),
            dwTextureBlendCaps: self.bits(device, &[all(TextureBlend, 0xcf)]),
            dwTextureAddressCaps: self.bits(device, &[all(Address, 0x17), both(BorderColor, 8, 0)]),
            ..Default::default()
        }
    }
}

pub struct Report {
    pub device: D3DDEVICEDESC7,
    pub hardware: D3DDEVICEDESC,
    pub software: D3DDEVICEDESC,
    pub texture_formats: Vec<DDPIXELFORMAT>,
    pub depth_formats: Vec<DDPIXELFORMAT>,
}

impl Report {
    pub fn derive(caps: &Caps, coverage: Coverage<'_>, device: Device) -> Self {
        let limits = &caps.limits;
        let rgb = device == Device::Rgb;
        let textures = u32::from(coverage.max_textures)
            .min(8)
            .min(limits.max_sampled_images) as u16;
        let stages = coverage.max_stages.min(8);
        let extent = coverage
            .max_texture_extent
            .min(limits.max_image_2d)
            .min(if rgb { 1024 } else { 16384 });
        let depth_formats = DepthFormat::ALL
            .into_iter()
            .filter(|format| {
                (!rgb || matches!(format, DepthFormat::Depth16 | DepthFormat::LowStencil))
                    && (coverage.converted_depths.contains(format)
                        || (coverage.depths.contains(format)
                            && caps
                                .format(format.backing())
                                .contains(FormatCaps::DEPTH_STENCIL)))
            })
            .map(DepthFormat::descriptor)
            .collect::<Vec<_>>();
        let texture_formats = TextureFormat::ALL
            .into_iter()
            .filter(|format| {
                coverage.converted_textures.contains(format)
                    || (coverage.textures.contains(format)
                        && format.backing().is_some_and(|host| {
                            caps.format(host).contains(FormatCaps::SAMPLED)
                                && (!format.is_block_compressed()
                                    || caps.features.contains(Features::BC_TEXTURES))
                        }))
            })
            .map(TextureFormat::descriptor)
            .collect();

        let render_depths = coverage
            .render_formats
            .iter()
            .filter(|&&format| caps.format(format).contains(FormatCaps::COLOR))
            .fold(0, |bits, format| {
                let rgba = matches!(format, Format::Rgba8Unorm | Format::Bgra8Unorm);
                let packed = matches!(
                    format,
                    Format::B5g6r5Unorm | Format::Bgr5a1Unorm | Format::Bgra4Unorm
                );
                bits | (u32::from(rgba) * 0x100) | (u32::from(packed) * 0x400)
            })
            | if coverage.converted_packed_render_targets
                && caps
                    .format(Format::Bgra8Unorm)
                    .contains(FormatCaps::COLOR | FormatCaps::BLEND)
            {
                0x400
            } else {
                0
            };
        let z_depths = depth_formats.iter().fold(0, |bits, format| {
            bits | match format.dwRGBBitCount {
                16 => 0x400,
                24 => 0x200,
                32 => 0x100,
                _ => 0,
            }
        });
        let primitives = coverage.primitive(device);
        let mut legacy = D3DDEVICEDESC {
            dwSize: size_of::<D3DDEVICEDESC>() as u32,
            dwFlags: if rgb { 0x7ff } else { 0x1e3 },
            dcmColorModel: 2, // D3DCOLOR_RGB.
            dwDevCaps: coverage.bits(device, DEVICE),
            dtcTransformCaps: D3DTRANSFORMCAPS {
                dwSize: size_of::<D3DTRANSFORMCAPS>() as u32,
                dwCaps: u32::from(coverage.clip_planes != 0),
            },
            bClipping: i32::from(coverage.enabled(Cull)),
            dlcLightingCaps: D3DLIGHTINGCAPS {
                dwSize: size_of::<D3DLIGHTINGCAPS>() as u32,
                dwCaps: if coverage.enabled(LightTypes) { 7 } else { 0 },
                dwLightingModel: 2,
                dwNumLights: coverage.max_lights,
            },
            dpcLineCaps: primitives,
            dpcTriCaps: primitives,
            dwDeviceRenderBitDepth: render_depths,
            dwDeviceZBufferBitDepth: z_depths,
            dwMaxVertexCount: coverage.max_vertices,
            dwMinTextureWidth: u32::from(extent != 0),
            dwMinTextureHeight: u32::from(extent != 0),
            dwMaxTextureWidth: extent,
            dwMaxTextureHeight: extent,
            dwMaxTextureRepeat: coverage.max_texture_repeat,
            dwMaxTextureAspectRatio: extent,
            dwMaxAnisotropy: coverage.max_anisotropy
                * u32::from(coverage.enabled(AnisotropicFilter) && !rgb),
            dwStencilCaps: coverage.bits(device, &[all(Stencil, 0xff)]),
            dwFVFCaps: coverage.bits(device, &[both(VertexLayout, 8, 0x00180008)]),
            dwTextureOpCaps: coverage.bits(device, TEXTURE_OP),
            wMaxTextureBlendStages: stages,
            wMaxSimultaneousTextures: textures,
            ..Default::default()
        };
        let device_desc = D3DDEVICEDESC7 {
            dwDevCaps: legacy.dwDevCaps,
            dpcLineCaps: legacy.dpcLineCaps,
            dpcTriCaps: legacy.dpcTriCaps,
            dwDeviceRenderBitDepth: render_depths,
            dwDeviceZBufferBitDepth: z_depths,
            dwMinTextureWidth: legacy.dwMinTextureWidth,
            dwMinTextureHeight: legacy.dwMinTextureHeight,
            dwMaxTextureWidth: extent,
            dwMaxTextureHeight: extent,
            dwMaxTextureRepeat: legacy.dwMaxTextureRepeat,
            dwMaxTextureAspectRatio: legacy.dwMaxTextureAspectRatio,
            dwMaxAnisotropy: legacy.dwMaxAnisotropy,
            dwStencilCaps: legacy.dwStencilCaps,
            dwFVFCaps: legacy.dwFVFCaps,
            dwTextureOpCaps: legacy.dwTextureOpCaps,
            wMaxTextureBlendStages: stages,
            wMaxSimultaneousTextures: textures,
            dwMaxActiveLights: if device == Device::TnlHal {
                coverage.max_lights.min(8)
            } else {
                coverage.max_lights
            },
            deviceGUID: bytemuck::cast(device.guid()),
            wMaxUserClipPlanes: coverage.clip_planes,
            wMaxVertexBlendMatrices: coverage.vertex_blend_matrices,
            dwVertexProcessingCaps: coverage.bits(device, VERTEX_PROCESSING),
            ..Default::default()
        };

        let (hardware, software) = if rgb {
            (
                D3DDEVICEDESC {
                    dwSize: legacy.dwSize,
                    ..Default::default()
                },
                legacy,
            )
        } else {
            // The legacy HAL callback includes hardware T&L when its path is
            // available. The HEL descriptor carries the core transform fields.
            legacy.dwDevCaps = coverage.bits(Device::TnlHal, DEVICE);
            let hel = D3DDEVICEDESC {
                dwSize: legacy.dwSize,
                dwFlags: 0x1f,
                dcmColorModel: legacy.dcmColorModel,
                dwDevCaps: coverage.bits(Device::Rgb, &[all(TransformedVertices, 1)]),
                dtcTransformCaps: legacy.dtcTransformCaps,
                bClipping: legacy.bClipping,
                dlcLightingCaps: legacy.dlcLightingCaps,
                ..Default::default()
            };
            (legacy, hel)
        };

        Self {
            device: device_desc,
            hardware,
            software,
            texture_formats,
            depth_formats,
        }
    }
}

/// Texture layout rows in the order returned by Windows' EnumTextureFormats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFormat {
    Xrgb1555,
    Argb1555,
    Argb4444,
    Rgb565,
    Xrgb8888,
    Argb8888,
    V8U8,
    X8L8V8U8,
    Dxt1,
    Dxt2,
    Dxt3,
    Dxt4,
    Dxt5,
    Nvcs,
    Nvhu,
    Nvhs,
    L8,
    A8,
    A8L8,
    Null,
    Ati1,
    Ati2,
    Intz,
    Nvidia31x,
    Nvidia61x,
}

impl TextureFormat {
    const ALL: [Self; 25] = [
        Self::Xrgb1555,
        Self::Argb1555,
        Self::Argb4444,
        Self::Rgb565,
        Self::Xrgb8888,
        Self::Argb8888,
        Self::V8U8,
        Self::X8L8V8U8,
        Self::Dxt1,
        Self::Dxt2,
        Self::Dxt3,
        Self::Dxt4,
        Self::Dxt5,
        Self::Nvcs,
        Self::Nvhu,
        Self::Nvhs,
        Self::L8,
        Self::A8,
        Self::A8L8,
        Self::Null,
        Self::Ati1,
        Self::Ati2,
        Self::Intz,
        Self::Nvidia31x,
        Self::Nvidia61x,
    ];

    pub fn backing(self) -> Option<Format> {
        Some(match self {
            Self::Xrgb1555 | Self::Argb1555 => Format::Bgr5a1Unorm,
            Self::Argb4444 => Format::Bgra4Unorm,
            Self::Rgb565 => Format::B5g6r5Unorm,
            Self::Xrgb8888 | Self::Argb8888 => Format::Bgra8Unorm,
            Self::V8U8 => Format::Rg8Snorm,
            Self::X8L8V8U8 => return None,
            Self::Dxt1 => Format::Bc1Unorm,
            Self::Dxt2 | Self::Dxt3 => Format::Bc2Unorm,
            Self::Dxt4 | Self::Dxt5 => Format::Bc3Unorm,
            Self::L8 => Format::R8Unorm,
            Self::A8 => Format::A8Unorm,
            Self::A8L8 => Format::Rg8Unorm,
            Self::Intz => Format::Depth24UnormStencil8,
            Self::Ati1 => Format::Bc4Unorm,
            Self::Ati2 => Format::Bc5Unorm,
            Self::Null
            | Self::Nvcs
            | Self::Nvhu
            | Self::Nvhs
            | Self::Nvidia31x
            | Self::Nvidia61x => return None,
        })
    }

    fn is_block_compressed(self) -> bool {
        matches!(
            self,
            Self::Dxt1
                | Self::Dxt2
                | Self::Dxt3
                | Self::Dxt4
                | Self::Dxt5
                | Self::Ati1
                | Self::Ati2
        )
    }

    fn descriptor(self) -> DDPIXELFORMAT {
        match self {
            Self::Xrgb1555 => pixel(0x40, 16, [0x7c00, 0x3e0, 0x1f, 0]),
            Self::Argb1555 => pixel(0x41, 16, [0x7c00, 0x3e0, 0x1f, 0x8000]),
            Self::Argb4444 => pixel(0x41, 16, [0xf00, 0xf0, 0xf, 0xf000]),
            Self::Rgb565 => pixel(0x40, 16, [0xf800, 0x7e0, 0x1f, 0]),
            Self::Xrgb8888 => pixel(0x40, 32, [0xff0000, 0xff00, 0xff, 0]),
            Self::Argb8888 => pixel(0x41, 32, [0xff0000, 0xff00, 0xff, 0xff000000]),
            Self::V8U8 => pixel(0x80000, 16, [0xff, 0xff00, 0, 0]),
            Self::X8L8V8U8 => pixel(0xc0000, 32, [0xff, 0xff00, 0xff0000, 0]),
            Self::L8 => pixel(0x20000, 8, [0xff, 0, 0, 0]),
            Self::A8 => pixel(2, 8, [0; 4]),
            Self::A8L8 => pixel(0x20001, 16, [0xff, 0, 0, 0xff00]),
            Self::Dxt1 => fourcc(*b"DXT1"),
            Self::Dxt2 => fourcc(*b"DXT2"),
            Self::Dxt3 => fourcc(*b"DXT3"),
            Self::Dxt4 => fourcc(*b"DXT4"),
            Self::Dxt5 => fourcc(*b"DXT5"),
            Self::Nvcs => fourcc(*b"NVCS"),
            Self::Nvhu => fourcc(*b"NVHU"),
            Self::Nvhs => fourcc(*b"NVHS"),
            Self::Null => fourcc(*b"NULL"),
            Self::Ati1 => fourcc(*b"ATI1"),
            Self::Ati2 => fourcc(*b"ATI2"),
            Self::Intz => fourcc(*b"INTZ"),
            Self::Nvidia31x => fourcc(*b"3x11"),
            Self::Nvidia61x => fourcc(*b"3x16"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepthFormat {
    Depth16,
    Depth32High,
    LowStencil,
    HighStencil,
    Depth32Low,
    Depth24,
}

impl DepthFormat {
    const ALL: [Self; 6] = [
        Self::Depth16,
        Self::Depth32High,
        Self::LowStencil,
        Self::HighStencil,
        Self::Depth32Low,
        Self::Depth24,
    ];

    fn backing(self) -> Format {
        match self {
            Self::Depth16 => Format::Depth16Unorm,
            Self::LowStencil | Self::HighStencil => Format::Depth24UnormStencil8,
            Self::Depth32High | Self::Depth32Low | Self::Depth24 => Format::Depth32Float,
        }
    }

    fn descriptor(self) -> DDPIXELFORMAT {
        match self {
            Self::Depth16 => pixel(0x400, 16, [0, 0xffff, 0, 0]),
            Self::Depth32High => pixel(0x400, 32, [0, 0xffffff00, 0, 0]),
            Self::LowStencil => pixel(0x4400, 32, [8, 0xffffff00, 0xff, 0]),
            Self::HighStencil => pixel(0x4400, 32, [8, 0xffffff, 0xff000000, 0]),
            Self::Depth32Low => pixel(0x400, 32, [0, 0xffffff, 0, 0]),
            Self::Depth24 => pixel(0x400, 24, [0, 0xffffff, 0, 0]),
        }
    }
}

fn pixel(flags: u32, bits: u32, masks: [u32; 4]) -> DDPIXELFORMAT {
    DDPIXELFORMAT {
        dwSize: size_of::<DDPIXELFORMAT>() as u32,
        dwFlags: flags,
        dwFourCC: 0,
        dwRGBBitCount: bits,
        dwRBitMask: masks[0],
        Anonymous3: masks[1].to_le_bytes(),
        dwBBitMask: masks[2],
        dwRGBAlphaBitMask: masks[3],
    }
}

fn fourcc(name: [u8; 4]) -> DDPIXELFORMAT {
    DDPIXELFORMAT {
        dwFourCC: u32::from_le_bytes(name),
        ..pixel(4, 0, [0; 4])
    }
}

#[cfg(test)]
mod tests;
