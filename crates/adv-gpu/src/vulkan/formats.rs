use super::*;

pub(super) const ALL: &[Format] = &[
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
        Format::R8Snorm => vk::Format::R8_SNORM,
        Format::R8Uint => vk::Format::R8_UINT,
        Format::R8Sint => vk::Format::R8_SINT,
        Format::R16Unorm => vk::Format::R16_UNORM,
        Format::R16Snorm => vk::Format::R16_SNORM,
        Format::R16Uint => vk::Format::R16_UINT,
        Format::R16Sint => vk::Format::R16_SINT,
        Format::R32Sint => vk::Format::R32_SINT,
        Format::Rg8Snorm => vk::Format::R8G8_SNORM,
        Format::Rg8Uint => vk::Format::R8G8_UINT,
        Format::Rg8Sint => vk::Format::R8G8_SINT,
        Format::Rg16Unorm => vk::Format::R16G16_UNORM,
        Format::Rg16Snorm => vk::Format::R16G16_SNORM,
        Format::Rg16Uint => vk::Format::R16G16_UINT,
        Format::Rg16Sint => vk::Format::R16G16_SINT,
        Format::Rg32Uint => vk::Format::R32G32_UINT,
        Format::Rg32Sint => vk::Format::R32G32_SINT,
        Format::Rgba8Snorm => vk::Format::R8G8B8A8_SNORM,
        Format::Rgba8Uint => vk::Format::R8G8B8A8_UINT,
        Format::Rgba8Sint => vk::Format::R8G8B8A8_SINT,
        Format::Rgba16Unorm => vk::Format::R16G16B16A16_UNORM,
        Format::Rgba16Snorm => vk::Format::R16G16B16A16_SNORM,
        Format::Rgba16Uint => vk::Format::R16G16B16A16_UINT,
        Format::Rgba16Sint => vk::Format::R16G16B16A16_SINT,
        Format::Rgba32Uint => vk::Format::R32G32B32A32_UINT,
        Format::Rgba32Sint => vk::Format::R32G32B32A32_SINT,
        Format::A8Unorm => vk::Format::R8_UNORM,
        Format::Rgb10a2Uint => vk::Format::A2B10G10R10_UINT_PACK32,
        Format::Rg11b10Float => vk::Format::B10G11R11_UFLOAT_PACK32,
        Format::Rgb9e5Float => vk::Format::E5B9G9R9_UFLOAT_PACK32,
        Format::Bc1Srgb => vk::Format::BC1_RGBA_SRGB_BLOCK,
        Format::Bc2Srgb => vk::Format::BC2_SRGB_BLOCK,
        Format::Bc3Srgb => vk::Format::BC3_SRGB_BLOCK,
        Format::Bc4Unorm => vk::Format::BC4_UNORM_BLOCK,
        Format::Bc4Snorm => vk::Format::BC4_SNORM_BLOCK,
        Format::Bc5Unorm => vk::Format::BC5_UNORM_BLOCK,
        Format::Bc5Snorm => vk::Format::BC5_SNORM_BLOCK,
        Format::Bc6hUfloat => vk::Format::BC6H_UFLOAT_BLOCK,
        Format::Bc6hSfloat => vk::Format::BC6H_SFLOAT_BLOCK,
        Format::Bc7Unorm => vk::Format::BC7_UNORM_BLOCK,
        Format::Bc7Srgb => vk::Format::BC7_SRGB_BLOCK,
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
