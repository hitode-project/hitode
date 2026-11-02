use super::*;
use objc2_foundation::NSProcessInfo;

// Apple Metal Feature Set Tables, GPU implementation limits and texture capabilities:
// https://developer.apple.com/metal/Metal-Feature-Set-Tables.pdf
pub(super) fn query(raw: &ProtocolObject<dyn MTLDevice>) -> Caps {
    let apple = raw.supportsFamily(MTLGPUFamily::Apple5);
    let apple6 = raw.supportsFamily(MTLGPUFamily::Apple6);
    let apple7 = raw.supportsFamily(MTLGPUFamily::Apple7);
    let mac = raw.supportsFamily(MTLGPUFamily::Mac2);
    let bc = raw.supportsBCTextureCompression();
    let mut features = Features::SHADER_F16
        | Features::OCCLUSION_QUERIES
        | Features::PIPELINE_CACHE
        | Features::INDIRECT
        | Features::MULTISAMPLE
        | Features::PRESENTABLE_BUFFERS;
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
    let formats = FORMATS
        .iter()
        .map(|&f| (f, format_caps(raw, f, apple, bc)))
        .collect();
    let sample_counts = [1usize, 2, 4, 8, 16]
        .into_iter()
        .enumerate()
        .filter(|&(_, n)| raw.supportsTextureSampleCount(n))
        .fold(0, |mask, (i, _)| mask | (1 << i));
    Caps {
        features,
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
            max_uniform_buffers: 31,
            max_sampled_images: if apple && !apple6 { 96 } else { 128 },
            max_samplers: 16,
            max_storage_bindings: 31,
            max_workgroup_storage: raw.maxThreadgroupMemoryLength() as u32,
            max_workgroup_invocations: 1024,
            uniform_offset_alignment: if apple { 4 } else { 256 },
            storage_offset_alignment: if apple { 4 } else { 256 },
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
    Format::Rg8Unorm,
    Format::Rgba8Unorm,
    Format::Rgba8Srgb,
    Format::Bgra8Unorm,
    Format::Bgra8Srgb,
    Format::B5g6r5Unorm,
    Format::Bgr5a1Unorm,
    Format::Bgra4Unorm,
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

fn format_caps(
    raw: &ProtocolObject<dyn MTLDevice>,
    f: Format,
    apple: bool,
    bc: bool,
) -> FormatCaps {
    use FormatCaps as C;
    let color = C::SAMPLED | C::FILTERED | C::COLOR | C::BLEND;
    match f {
        Format::Nv12 | Format::P010 | Format::Bgra4Unorm => C::empty(),
        Format::B5g6r5Unorm | Format::Bgr5a1Unorm => {
            if apple {
                color
            } else {
                C::empty()
            }
        }
        Format::Bc1Unorm | Format::Bc2Unorm | Format::Bc3Unorm => {
            if bc {
                C::SAMPLED | C::FILTERED
            } else {
                C::empty()
            }
        }
        Format::Depth16Unorm => C::SAMPLED | C::FILTERED | C::DEPTH_STENCIL,
        Format::Depth24UnormStencil8 | Format::Depth32FloatStencil8 => {
            let mut caps = C::SAMPLED | C::DEPTH_STENCIL;
            if !apple || raw.supportsFamily(MTLGPUFamily::Apple8) {
                caps |= C::FILTERED;
            }
            caps
        }
        Format::Depth32Float => {
            let mut caps = C::SAMPLED | C::DEPTH_STENCIL;
            if raw.supports32BitFloatFiltering() {
                caps |= C::FILTERED;
            }
            caps
        }
        present::FORMAT => color | C::STORAGE | C::PRESENTABLE,
        Format::R32Uint => C::SAMPLED | C::COLOR | C::STORAGE,
        Format::R32Float | Format::Rg32Float | Format::Rgba32Float => {
            let mut caps = C::SAMPLED | C::COLOR | C::STORAGE | C::BLEND;
            if raw.supports32BitFloatFiltering() {
                caps |= C::FILTERED;
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
        Format::Rg8Unorm => M::RG8Unorm,
        Format::Rgba8Unorm => M::RGBA8Unorm,
        Format::Rgba8Srgb => M::RGBA8Unorm_sRGB,
        Format::Bgra8Unorm => M::BGRA8Unorm,
        Format::Bgra8Srgb => M::BGRA8Unorm_sRGB,
        // Metal's packed names list components from the least significant bit.
        Format::B5g6r5Unorm => M::B5G6R5Unorm,
        Format::Bgr5a1Unorm => M::BGR5A1Unorm,
        // ABGR4 has alpha in the low nibble, so it cannot represent BGRA4.
        Format::Bgra4Unorm | Format::Nv12 | Format::P010 => return None,
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
        Format::Bc2Unorm | Format::Bc3Unorm => (4, 16),
        Format::R8Unorm => (1, 1),
        Format::Rg8Unorm
        | Format::B5g6r5Unorm
        | Format::Bgr5a1Unorm
        | Format::Bgra4Unorm
        | Format::R16Float
        | Format::Depth16Unorm => (1, 2),
        Format::Rgba16Float
        | Format::Rg32Float
        | Format::Depth24UnormStencil8
        | Format::Depth32FloatStencil8 => (1, 8),
        Format::Rgba32Float => (1, 16),
        _ => (1, 4),
    }
}
