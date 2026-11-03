use super::*;

pub(super) const ALL: &[Format] = &[
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
    Format::Nv21,
    Format::P010,
];
pub(super) fn vk(format: Format) -> vk::Format {
    match format {
        Format::R8Unorm => vk::Format::R8_UNORM,
        Format::Rg8Unorm => vk::Format::R8G8_UNORM,
        Format::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        Format::Rgba8Srgb => vk::Format::R8G8B8A8_SRGB,
        Format::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        Format::Bgra8Srgb => vk::Format::B8G8R8A8_SRGB,
        // Vulkan packed names run from most significant to least significant.
        Format::B5g6r5Unorm => vk::Format::R5G6B5_UNORM_PACK16,
        Format::Bgr5a1Unorm => vk::Format::A1R5G5B5_UNORM_PACK16,
        Format::Bgra4Unorm => vk::Format::A4R4G4B4_UNORM_PACK16_EXT,
        Format::Rgb10a2Unorm => vk::Format::A2B10G10R10_UNORM_PACK32,
        Format::R16Float => vk::Format::R16_SFLOAT,
        Format::Rg16Float => vk::Format::R16G16_SFLOAT,
        Format::Rgba16Float => vk::Format::R16G16B16A16_SFLOAT,
        Format::R32Float => vk::Format::R32_SFLOAT,
        Format::Rg32Float => vk::Format::R32G32_SFLOAT,
        Format::Rgba32Float => vk::Format::R32G32B32A32_SFLOAT,
        Format::R32Uint => vk::Format::R32_UINT,
        Format::Bc1Unorm => vk::Format::BC1_RGBA_UNORM_BLOCK,
        Format::Bc2Unorm => vk::Format::BC2_UNORM_BLOCK,
        Format::Bc3Unorm => vk::Format::BC3_UNORM_BLOCK,
        Format::Depth16Unorm => vk::Format::D16_UNORM,
        Format::Depth24UnormStencil8 => vk::Format::D24_UNORM_S8_UINT,
        Format::Depth32Float => vk::Format::D32_SFLOAT,
        Format::Depth32FloatStencil8 => vk::Format::D32_SFLOAT_S8_UINT,
        Format::Nv12 => vk::Format::G8_B8R8_2PLANE_420_UNORM,
        Format::Nv21 => vk::Format::UNDEFINED,
        Format::P010 => vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
    }
}
pub(super) fn aspect(format: Format) -> vk::ImageAspectFlags {
    if format.has_depth() {
        vk::ImageAspectFlags::DEPTH
            | if format.has_stencil() {
                vk::ImageAspectFlags::STENCIL
            } else {
                vk::ImageAspectFlags::empty()
            }
    } else {
        vk::ImageAspectFlags::COLOR
    }
}
pub(super) fn block(format: Format) -> (u32, u32) {
    match format {
        Format::Bc1Unorm => (4, 8),
        Format::Bc2Unorm | Format::Bc3Unorm => (4, 16),
        Format::R8Unorm | Format::Nv12 | Format::Nv21 => (1, 1),
        Format::Rg8Unorm
        | Format::B5g6r5Unorm
        | Format::Bgr5a1Unorm
        | Format::Bgra4Unorm
        | Format::R16Float
        | Format::Depth16Unorm
        | Format::P010 => (1, 2),
        Format::Rgba16Float | Format::Rg32Float | Format::Depth32FloatStencil8 => (1, 8),
        Format::Rgba32Float => (1, 16),
        _ => (1, 4),
    }
}
