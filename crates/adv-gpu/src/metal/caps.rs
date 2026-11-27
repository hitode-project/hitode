use super::*;
use objc2_foundation::NSProcessInfo;

// Apple Metal Feature Set Tables, GPU implementation limits and texture capabilities:
// https://developer.apple.com/metal/Metal-Feature-Set-Tables.pdf
/// The device facts the Metal format table depends on, as the Feature Set Tables list them per
/// GPU family, so the table can be derived for a family without that device.
#[derive(Clone, Copy, Debug)]
pub struct MetalFamily {
    /// Apple5 or newer.
    pub apple: bool,
    pub apple3: bool,
    pub apple6: bool,
    pub apple7: bool,
    pub apple9: bool,
    pub bc: bool,
    pub float32_filtering: bool,
    pub image_atomics: bool,
    /// Bit i set when 2^i samples are supported.
    pub sample_counts: u32,
}

impl MetalFamily {
    fn query(raw: &ProtocolObject<dyn MTLDevice>) -> Self {
        let apple6 = raw.supportsFamily(MTLGPUFamily::Apple6);
        let mac = raw.supportsFamily(MTLGPUFamily::Mac2);
        Self {
            apple: raw.supportsFamily(MTLGPUFamily::Apple5),
            apple3: raw.supportsFamily(MTLGPUFamily::Apple3),
            apple6,
            apple7: raw.supportsFamily(MTLGPUFamily::Apple7),
            apple9: raw.supportsFamily(MTLGPUFamily::Apple9),
            bc: raw.supportsBCTextureCompression(),
            float32_filtering: raw.supports32BitFloatFiltering(),
            image_atomics: (apple6 || mac) && objc2::available!(macos = 14.0, ios = 17.0),
            sample_counts: [1usize, 2, 4, 8, 16]
                .into_iter()
                .enumerate()
                .filter(|&(_, n)| raw.supportsTextureSampleCount(n))
                .fold(0, |mask, (i, _)| mask | (1 << i)),
        }
    }

    /// Apple7 (A14, M1) as the Feature Set Tables give it.
    pub fn apple7() -> Self {
        Self {
            apple: true,
            apple3: true,
            apple6: true,
            apple7: true,
            apple9: false,
            bc: true,
            float32_filtering: true,
            image_atomics: true,
            sample_counts: 0b111,
        }
    }

    /// Each format's capabilities and sample-count mask for this family.
    pub fn formats(&self) -> Vec<(Format, FormatCaps, u32)> {
        FORMATS
            .iter()
            .map(|&f| {
                let mut caps = format_caps(self, f);
                if self.image_atomics && matches!(f, Format::R32Uint | Format::R32Sint) {
                    caps |= FormatCaps::STORAGE_ATOMIC;
                }
                let samples = if !caps.intersects(FormatCaps::COLOR | FormatCaps::DEPTH_STENCIL) {
                    0
                } else if self.apple
                    && !self.apple7
                    && matches!(
                        f,
                        Format::R32Uint
                            | Format::R32Sint
                            | Format::Rg32Uint
                            | Format::Rg32Sint
                            | Format::Rg32Float
                            | Format::Rgba32Uint
                            | Format::Rgba32Sint
                            | Format::Rgba32Float
                    )
                {
                    1
                } else {
                    self.sample_counts
                };
                (f, caps, samples)
            })
            .collect()
    }
}

pub(super) fn query(raw: &ProtocolObject<dyn MTLDevice>) -> Caps {
    let family = MetalFamily::query(raw);
    let MetalFamily {
        apple,
        apple6,
        apple7,
        bc,
        sample_counts,
        ..
    } = family;
    let mac = raw.supportsFamily(MTLGPUFamily::Mac2);
    let mut features = Features::SHADER_F16
        | Features::OCCLUSION_QUERIES
        | Features::PIPELINE_CACHE
        | Features::INDIRECT
        | Features::MULTISAMPLE
        | Features::PRESENTABLE_BUFFERS
        | Features::STANDARD_SAMPLE_LOCATIONS
        | Features::DUAL_SOURCE_BLEND
        | Features::DEPTH_CLAMP
        | Features::CLIP_DISTANCES
        | Features::WIREFRAME;
    if apple || mac {
        features |= Features::MULTIPLE_VIEWPORTS;
    }
    if family.image_atomics {
        features |= Features::IMAGE_ATOMICS;
    }
    if apple {
        features |= Features::TRANSIENT_ATTACHMENTS | Features::TILE_SHADERS;
    }
    if apple6 || mac {
        features |= Features::SUBGROUP;
    }
    if apple7 {
        features |= Features::MATRIX_F16_8X8;
    }
    if raw.supportsCounterSampling(MTLCounterSamplingPoint::AtStageBoundary)
        && timestamp_counter(raw).is_some()
    {
        features |= Features::TIMESTAMPS;
    }
    if bc {
        features |= Features::BC_TEXTURES;
    }
    let formats = family.formats();
    Caps {
        features,
        ieee_f32: true,
        modern: Modern::empty(),
        limits: Limits {
            max_image_2d: if raw.supportsFamily(MTLGPUFamily::Apple10) {
                32768
            } else {
                16384
            },
            max_image_3d: 2048,
            max_image_layers: 2048,
            max_color_attachments: 8,
            max_color_bytes_per_sample: if apple7 || mac { 128 } else { 64 },
            max_fragment_input_components: if apple { 124 } else { 128 },
            max_framebuffer_layers: 2048,
            // The backend binds real MTLBuffers, not inlined constant data.
            max_uniform_buffer_bytes: raw.maxBufferLength().min(u32::MAX as usize) as u32,
            max_viewports: if features.contains(Features::MULTIPLE_VIEWPORTS) {
                16
            } else {
                1
            },
            max_buffer_size: raw.maxBufferLength() as u64,
            max_uniform_buffers: 31,
            max_sampled_images: if apple && !apple6 { 96 } else { 128 },
            max_samplers: 32,
            max_storage_bindings: 31,
            max_workgroup_storage: raw.maxThreadgroupMemoryLength() as u32,
            max_workgroup_invocations: 1024,
            // Vector-typed bindings need their 16-byte alignment in the offset.
            uniform_offset_alignment: if apple { 16 } else { 256 },
            storage_offset_alignment: if apple { 16 } else { 256 },
            sample_counts,
            tile: if apple { (32, 32) } else { (0, 0) },
            push_constant_bytes: 0,
        },
        adapter: raw.name().to_string(),
        driver: NSProcessInfo::processInfo()
            .operatingSystemVersionString()
            .to_string(),
        formats,
    }
}

pub(super) fn timestamp_counter(
    raw: &ProtocolObject<dyn MTLDevice>,
) -> Option<Object<dyn MTLCounterSet>> {
    raw.counterSets()?.iter().find(|s| {
        s.name()
            .isEqualToString(unsafe { MTLCommonCounterSetTimestamp })
    })
}

const FORMATS: &[Format] = &[
    Format::R8Unorm,
    Format::R8Snorm,
    Format::R8Uint,
    Format::R8Sint,
    Format::R16Unorm,
    Format::R16Snorm,
    Format::R16Uint,
    Format::R16Sint,
    Format::R32Sint,
    Format::Rg8Snorm,
    Format::Rg8Uint,
    Format::Rg8Sint,
    Format::Rg16Unorm,
    Format::Rg16Snorm,
    Format::Rg16Uint,
    Format::Rg16Sint,
    Format::Rg32Uint,
    Format::Rg32Sint,
    Format::Rgba8Snorm,
    Format::Rgba8Uint,
    Format::Rgba8Sint,
    Format::Rgba16Unorm,
    Format::Rgba16Snorm,
    Format::Rgba16Uint,
    Format::Rgba16Sint,
    Format::Rgba32Uint,
    Format::Rgba32Sint,
    Format::A8Unorm,
    Format::Rgb10a2Uint,
    Format::Rg11b10Float,
    Format::Rgb9e5Float,
    Format::Bc1Srgb,
    Format::Bc2Srgb,
    Format::Bc3Srgb,
    Format::Bc4Unorm,
    Format::Bc4Snorm,
    Format::Bc5Unorm,
    Format::Bc5Snorm,
    Format::Bc6hUfloat,
    Format::Bc6hSfloat,
    Format::Bc7Unorm,
    Format::Bc7Srgb,
    Format::Rg8Unorm,
    Format::Rgba8Unorm,
    Format::Rgba8Srgb,
    Format::Bgra8Unorm,
    Format::Bgra8Srgb,
    Format::B5g6r5Unorm,
    Format::Bgr5a1Unorm,
    Format::Bgra4Unorm,
    Format::Abgr4Unorm,
    Format::Rgb10a2Unorm,
    Format::R16Float,
    Format::Rg16Float,
    Format::Rgba16Float,
    Format::R32Float,
    Format::Rg32Float,
    Format::Rgba32Float,
    Format::R32Uint,
    Format::Bc1Unorm,
    Format::Bc2Unorm,
    Format::Bc3Unorm,
    Format::Depth16Unorm,
    Format::Depth24UnormStencil8,
    Format::Depth32Float,
    Format::Depth32FloatStencil8,
    Format::Nv12,
    Format::P010,
];

fn format_caps(family: &MetalFamily, f: Format) -> FormatCaps {
    use FormatCaps as C;
    let MetalFamily { apple, bc, .. } = *family;
    let color = C::SAMPLED | C::FILTERED | C::COLOR | C::BLEND;
    match f {
        Format::Nv12 => C::SAMPLED | C::FILTERED | C::PRESENTABLE,
        Format::P010 | Format::Bgra4Unorm => C::empty(),
        Format::B5g6r5Unorm | Format::Bgr5a1Unorm | Format::Abgr4Unorm => {
            if apple {
                color
            } else {
                C::empty()
            }
        }
        f if f.is_bc() => {
            if bc {
                let mut caps = C::SAMPLED | C::FILTERED;
                if family.apple3 {
                    caps |= C::BC_VOLUME;
                }
                caps
            } else {
                C::empty()
            }
        }
        Format::Depth16Unorm => C::SAMPLED | C::FILTERED | C::DEPTH_STENCIL,
        Format::Depth24UnormStencil8 | Format::Depth32FloatStencil8 => {
            let mut caps = C::SAMPLED | C::DEPTH_STENCIL;
            if !apple || family.apple9 {
                caps |= C::FILTERED;
            }
            caps
        }
        Format::Depth32Float => {
            let mut caps = C::SAMPLED | C::DEPTH_STENCIL;
            if family.float32_filtering {
                caps |= C::FILTERED;
            }
            caps
        }
        present::FORMAT => color | C::STORAGE | C::PRESENTABLE,

        Format::Rgb9e5Float if !apple => C::SAMPLED | C::FILTERED,
        Format::R32Uint | Format::R32Sint => C::SAMPLED | C::COLOR | C::STORAGE | C::STORAGE_READ,
        f if f.is_uint() || f.is_sint() => C::SAMPLED | C::COLOR | C::STORAGE,
        Format::R32Float | Format::Rg32Float | Format::Rgba32Float => {
            let mut caps = C::SAMPLED | C::COLOR | C::STORAGE | C::BLEND;
            if family.float32_filtering {
                caps |= C::FILTERED;
            }
            if f == Format::R32Float {
                caps |= C::STORAGE_READ;
            }
            caps
        }
        Format::Rgba8Srgb | Format::Bgra8Srgb => {
            if apple {
                color | C::STORAGE
            } else {
                color
            }
        }
        _ => color | C::STORAGE,
    }
}

pub(super) fn pixel_format(f: Format) -> Option<MTLPixelFormat> {
    use MTLPixelFormat as M;
    Some(match f {
        Format::R8Unorm => M::R8Unorm,
        Format::R8Snorm => M::R8Snorm,
        Format::R8Uint => M::R8Uint,
        Format::R8Sint => M::R8Sint,
        Format::R16Unorm => M::R16Unorm,
        Format::R16Snorm => M::R16Snorm,
        Format::R16Uint => M::R16Uint,
        Format::R16Sint => M::R16Sint,
        Format::R32Sint => M::R32Sint,
        Format::Rg8Snorm => M::RG8Snorm,
        Format::Rg8Uint => M::RG8Uint,
        Format::Rg8Sint => M::RG8Sint,
        Format::Rg16Unorm => M::RG16Unorm,
        Format::Rg16Snorm => M::RG16Snorm,
        Format::Rg16Uint => M::RG16Uint,
        Format::Rg16Sint => M::RG16Sint,
        Format::Rg32Uint => M::RG32Uint,
        Format::Rg32Sint => M::RG32Sint,
        Format::Rgba8Snorm => M::RGBA8Snorm,
        Format::Rgba8Uint => M::RGBA8Uint,
        Format::Rgba8Sint => M::RGBA8Sint,
        Format::Rgba16Unorm => M::RGBA16Unorm,
        Format::Rgba16Snorm => M::RGBA16Snorm,
        Format::Rgba16Uint => M::RGBA16Uint,
        Format::Rgba16Sint => M::RGBA16Sint,
        Format::Rgba32Uint => M::RGBA32Uint,
        Format::Rgba32Sint => M::RGBA32Sint,
        Format::A8Unorm => M::R8Unorm,
        Format::Rgb10a2Uint => M::RGB10A2Uint,
        Format::Rg11b10Float => M::RG11B10Float,
        Format::Rgb9e5Float => M::RGB9E5Float,
        Format::Bc1Srgb => M::BC1_RGBA_sRGB,
        Format::Bc2Srgb => M::BC2_RGBA_sRGB,
        Format::Bc3Srgb => M::BC3_RGBA_sRGB,
        Format::Bc4Unorm => M::BC4_RUnorm,
        Format::Bc4Snorm => M::BC4_RSnorm,
        Format::Bc5Unorm => M::BC5_RGUnorm,
        Format::Bc5Snorm => M::BC5_RGSnorm,
        Format::Bc6hUfloat => M::BC6H_RGBUfloat,
        Format::Bc6hSfloat => M::BC6H_RGBFloat,
        Format::Bc7Unorm => M::BC7_RGBAUnorm,
        Format::Bc7Srgb => M::BC7_RGBAUnorm_sRGB,
        Format::Rg8Unorm => M::RG8Unorm,
        Format::Rgba8Unorm => M::RGBA8Unorm,
        Format::Rgba8Srgb => M::RGBA8Unorm_sRGB,
        Format::Bgra8Unorm => M::BGRA8Unorm,
        Format::Bgra8Srgb => M::BGRA8Unorm_sRGB,
        // Metal's packed names list components from the least significant bit.
        Format::B5g6r5Unorm => M::B5G6R5Unorm,
        Format::Bgr5a1Unorm => M::BGR5A1Unorm,
        Format::Abgr4Unorm => M::ABGR4Unorm,
        // ABGR4 has alpha in the low nibble, so it cannot represent BGRA4.
        Format::Bgra4Unorm
        | Format::Nv12
        | Format::Nv21
        | Format::P010
        | Format::Rgb32Float
        | Format::Rgb32Uint
        | Format::Rgb32Sint => return None,
        Format::Rgb10a2Unorm => M::RGB10A2Unorm,
        Format::R16Float => M::R16Float,
        Format::Rg16Float => M::RG16Float,
        Format::Rgba16Float => M::RGBA16Float,
        Format::R32Float => M::R32Float,
        Format::Rg32Float => M::RG32Float,
        Format::Rgba32Float => M::RGBA32Float,
        Format::R32Uint => M::R32Uint,
        Format::Bc1Unorm => M::BC1_RGBA,
        Format::Bc2Unorm => M::BC2_RGBA,
        Format::Bc3Unorm => M::BC3_RGBA,
        Format::Depth16Unorm => M::Depth16Unorm,
        Format::Depth24UnormStencil8 | Format::Depth32FloatStencil8 => M::Depth32Float_Stencil8,
        Format::Depth32Float => M::Depth32Float,
    })
}

// (block width and height, bytes per block), for packed blit pitches.
pub(super) fn block(f: Format) -> (u32, u32) {
    match f {
        Format::Bc1Unorm => (4, 8),
        Format::Bc2Srgb
        | Format::Bc3Srgb
        | Format::Bc5Unorm
        | Format::Bc5Snorm
        | Format::Bc6hUfloat
        | Format::Bc6hSfloat
        | Format::Bc7Unorm
        | Format::Bc7Srgb => (4, 16),
        Format::Bc1Srgb | Format::Bc4Unorm | Format::Bc4Snorm => (4, 8),
        Format::Rgba32Uint | Format::Rgba32Sint => (1, 16),
        Format::Rg32Uint
        | Format::Rg32Sint
        | Format::Rgba16Unorm
        | Format::Rgba16Snorm
        | Format::Rgba16Uint
        | Format::Rgba16Sint => (1, 8),
        Format::R32Sint
        | Format::Rg16Unorm
        | Format::Rg16Snorm
        | Format::Rg16Uint
        | Format::Rg16Sint
        | Format::Rgba8Snorm
        | Format::Rgba8Uint
        | Format::Rgba8Sint
        | Format::Rgb10a2Uint
        | Format::Rg11b10Float
        | Format::Rgb9e5Float => (1, 4),
        Format::R16Unorm
        | Format::R16Snorm
        | Format::R16Uint
        | Format::R16Sint
        | Format::Rg8Snorm
        | Format::Rg8Uint
        | Format::Rg8Sint => (1, 2),
        Format::R8Snorm | Format::R8Uint | Format::R8Sint | Format::A8Unorm => (1, 1),
        Format::Bc2Unorm | Format::Bc3Unorm => (4, 16),
        Format::R8Unorm => (1, 1),
        Format::Rg8Unorm
        | Format::B5g6r5Unorm
        | Format::Bgr5a1Unorm
        | Format::Bgra4Unorm
        | Format::Abgr4Unorm
        | Format::R16Float
        | Format::Depth16Unorm => (1, 2),
        Format::Rgba16Float
        | Format::Rg32Float
        | Format::Depth24UnormStencil8
        | Format::Depth32FloatStencil8 => (1, 8),
        Format::Rgb32Float | Format::Rgb32Uint | Format::Rgb32Sint => (1, 12),
        Format::Rgba32Float => (1, 16),
        _ => (1, 4),
    }
}
