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
pub struct Depth(std::sync::Arc<DepthImage>);
struct DepthImage {
    _texture: wgpu::Texture,
    #[cfg(target_vendor = "apple")]
    metal_initialized: std::sync::atomic::AtomicBool,
    view: wgpu::TextureView,
    context: u64,
    width: u32,
    height: u32,
    format: DepthFormat,
}
impl Depth {
    pub(super) fn new(
        texture: wgpu::Texture,
        view: wgpu::TextureView,
        context: u64,
        width: u32,
        height: u32,
        format: DepthFormat,
    ) -> Self {
        Self(std::sync::Arc::new(DepthImage {
            _texture: texture,
            #[cfg(target_vendor = "apple")]
            metal_initialized: std::sync::atomic::AtomicBool::new(false),
            view,
            context,
            width,
            height,
            format,
        }))
    }
    #[cfg(target_vendor = "apple")]
    pub(super) fn metal_initialized(&self) -> &std::sync::atomic::AtomicBool {
        &self.0.metal_initialized
    }
    #[cfg(target_vendor = "apple")]
    pub(super) fn texture(&self) -> &wgpu::Texture {
        &self.0._texture
    }
    pub(super) fn context(&self) -> u64 {
        self.0.context
    }
    pub(super) fn view(&self) -> &wgpu::TextureView {
        &self.0.view
    }
    pub fn width(&self) -> u32 {
        self.0.width
    }
    pub fn height(&self) -> u32 {
        self.0.height
    }
    pub fn format(&self) -> DepthFormat {
        self.0.format
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
