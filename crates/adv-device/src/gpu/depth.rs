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
    /// Depth/stencil contents are observable only through attachment, so a
    /// clear waits for the next pass that attaches them, which may never come.
    deferred: std::sync::Mutex<Option<DeferredClear>>,
}

#[derive(Clone)]
pub(super) struct DeferredClear {
    pub(super) region: Rect,
    pub(super) depth: Option<f32>,
    pub(super) stencil: Option<u32>,
    /// A color image covering `region`, for a bounded clear draw.
    pub(super) image: Image,
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
            deferred: Default::default(),
        }))
    }
    fn whole(&self, region: Rect) -> bool {
        region.x == 0 && region.y == 0 && region.w == self.0.width && region.h == self.0.height
    }
    /// Record a clear. An older clear the new one cannot absorb is returned
    /// instead, to execute first; the caller then records the new one again.
    pub(super) fn defer_clear(&self, new: DeferredClear) -> Result<(), DeferredClear> {
        let mut slot = self.0.deferred.lock().unwrap();
        let Some(old) = slot.take() else {
            *slot = Some(new);
            return Ok(());
        };
        if old.region == new.region {
            *slot = Some(DeferredClear {
                depth: new.depth.or(old.depth),
                stencil: new.stencil.or(old.stencil),
                ..new
            });
        } else if new.region.clipped(old.region) == old.region
            && (old.depth.is_none() || new.depth.is_some())
            && (old.stencil.is_none() || new.stencil.is_some())
        {
            *slot = Some(new);
        } else {
            return Err(old);
        }
        Ok(())
    }
    /// A whole-image clear, for the load actions of a pass attaching this image.
    pub(super) fn take_whole_clear(&self) -> Option<(Option<f32>, Option<u32>)> {
        let mut slot = self.0.deferred.lock().unwrap();
        if !slot.as_ref().is_some_and(|clear| self.whole(clear.region)) {
            return None;
        }
        slot.take().map(|clear| (clear.depth, clear.stencil))
    }
    /// A partial clear, executed as a bounded draw before attaching this image.
    pub(super) fn take_partial_clear(&self) -> Option<DeferredClear> {
        let mut slot = self.0.deferred.lock().unwrap();
        if slot.as_ref().is_some_and(|clear| self.whole(clear.region)) {
            return None;
        }
        slot.take()
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
