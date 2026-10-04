//! Independently owned depth/stencil images. Color targets borrow these handles;
//! switching a target never copies or reinitializes the attachment's contents.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DepthFormat {
    Float32,
    Unorm16,
    Depth24Stencil8,
}
impl DepthFormat {
    pub(super) fn format(self) -> wgpu::TextureFormat {
        match self {
            Self::Float32 => wgpu::TextureFormat::Depth32Float,
            Self::Unorm16 => wgpu::TextureFormat::Depth16Unorm,
            Self::Depth24Stencil8 => wgpu::TextureFormat::Depth24PlusStencil8,
        }
    }
    pub fn has_stencil(self) -> bool {
        self == Self::Depth24Stencil8
    }
}

#[derive(Clone)]
pub struct Depth {
    pub(super) _texture: wgpu::Texture,
    pub(super) view: wgpu::TextureView,
    pub(super) context: u64,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) format: DepthFormat,
}
impl Depth {
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn format(&self) -> DepthFormat {
        self.format
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StencilOp {
    Keep,
    Zero,
    Replace,
    IncrementClamp,
    DecrementClamp,
    Invert,
    IncrementWrap,
    DecrementWrap,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StencilFace {
    pub compare: Compare,
    pub fail: StencilOp,
    pub depth_fail: StencilOp,
    pub pass: StencilOp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Stencil {
    pub front: StencilFace,
    pub back: StencilFace,
    pub read_mask: u32,
    pub write_mask: u32,
}
