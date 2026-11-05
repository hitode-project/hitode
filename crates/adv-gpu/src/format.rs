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
    pub fn planes(self) -> u32 {
        match self {
            Self::Nv12 | Self::Nv21 | Self::P010 => 2,
            _ => 1,
        }
    }
}
