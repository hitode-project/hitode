//! Texel formats. The set grows with users; support is queried through
//! [`crate::Caps::format`], never assumed.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Format {
    // Color, 8 bits per channel.
    R8Unorm,
    Rg8Unorm,
    Rgba8Unorm,
    Rgba8Srgb,
    Bgra8Unorm,
    Bgra8Srgb,
    // Packed 16-bit color (DirectDraw and D3D9 surfaces). Names follow DXGI:
    // components from the lowest bits up. `B5g6r5Unorm` is D3DFMT_R5G6B5
    // (blue in bits 0–4, red in 11–15), `Bgr5a1Unorm` D3DFMT_A1R5G5B5 and
    // `Bgra4Unorm` D3DFMT_A4R4G4B4; `Rgb10a2Unorm` has red in bits 0–9.
    B5g6r5Unorm,
    Bgr5a1Unorm,
    Bgra4Unorm,
    Rgb10a2Unorm,
    // Float and integer color (upscaler intermediates, guest render targets).
    R16Float,
    Rg16Float,
    Rgba16Float,
    R32Float,
    Rg32Float,
    Rgba32Float,
    R32Uint,
    // Block compressed (guest DXT textures).
    Bc1Unorm,
    Bc2Unorm,
    Bc3Unorm,
    // Depth and stencil. A12 has no 24-bit depth with stencil; such requests
    // are served as `Depth32FloatStencil8` and reported by [`crate::Caps`].
    Depth16Unorm,
    Depth24UnormStencil8,
    Depth32Float,
    Depth32FloatStencil8,
    // Two-plane YUV 4:2:0 for video planes: plane 0 is R8, plane 1 is Rg8
    // (`P010`: R16 and Rg16). Only presentable or sampled; never rendered.
    Nv12,
    /// Y, then interleaved Cr and Cb, 4:2:0. Presentable YUV pools only.
    Nv21,
    P010,
    R8Snorm,
    R8Uint,
    R8Sint,
    R16Unorm,
    R16Snorm,
    R16Uint,
    R16Sint,
    R32Sint,
    Rg8Snorm,
    Rg8Uint,
    Rg8Sint,
    Rg16Unorm,
    Rg16Snorm,
    Rg16Uint,
    Rg16Sint,
    Rg32Uint,
    Rg32Sint,
    Rgba8Snorm,
    Rgba8Uint,
    Rgba8Sint,
    Rgba16Unorm,
    Rgba16Snorm,
    Rgba16Uint,
    Rgba16Sint,
    Rgba32Uint,
    Rgba32Sint,
    A8Unorm,
    Rgb10a2Uint,
    Rg11b10Float,
    Rgb9e5Float,
    Bc1Srgb,
    Bc2Srgb,
    Bc3Srgb,
    Bc4Unorm,
    Bc4Snorm,
    Bc5Unorm,
    Bc5Snorm,
    Bc6hUfloat,
    Bc6hSfloat,
    Bc7Unorm,
    Bc7Srgb,
}

impl Format {
    pub fn has_depth(self) -> bool {
        matches!(
            self,
            Self::Depth16Unorm
                | Self::Depth24UnormStencil8
                | Self::Depth32Float
                | Self::Depth32FloatStencil8
        )
    }
    pub fn has_stencil(self) -> bool {
        matches!(
            self,
            Self::Depth24UnormStencil8 | Self::Depth32FloatStencil8
        )
    }
    #[cfg(feature = "backend")]
    pub(crate) fn is_bc(self) -> bool {
        matches!(
            self,
            Self::Bc1Unorm
                | Self::Bc2Unorm
                | Self::Bc3Unorm
                | Self::Bc1Srgb
                | Self::Bc2Srgb
                | Self::Bc3Srgb
                | Self::Bc4Unorm
                | Self::Bc4Snorm
                | Self::Bc5Unorm
                | Self::Bc5Snorm
                | Self::Bc6hUfloat
                | Self::Bc6hSfloat
                | Self::Bc7Unorm
                | Self::Bc7Srgb
        )
    }

    #[cfg(feature = "backend")]
    pub(crate) fn is_uint(self) -> bool {
        matches!(
            self,
            Self::R32Uint
                | Self::R8Uint
                | Self::R16Uint
                | Self::Rg8Uint
                | Self::Rg16Uint
                | Self::Rg32Uint
                | Self::Rgba8Uint
                | Self::Rgba16Uint
                | Self::Rgba32Uint
                | Self::Rgb10a2Uint
        )
    }

    #[cfg(feature = "backend")]
    pub(crate) fn is_sint(self) -> bool {
        matches!(
            self,
            Self::R8Sint
                | Self::R16Sint
                | Self::R32Sint
                | Self::Rg8Sint
                | Self::Rg16Sint
                | Self::Rg32Sint
                | Self::Rgba8Sint
                | Self::Rgba16Sint
                | Self::Rgba32Sint
        )
    }

    pub fn planes(self) -> u32 {
        match self {
            Self::Nv12 | Self::Nv21 | Self::P010 => 2,
            _ => 1,
        }
    }
}
