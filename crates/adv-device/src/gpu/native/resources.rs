//! Package C1: images, render targets, depth buffers, staging and uploads.
//!
//! Resources are created synchronously on the calling thread. Upload bytes go
//! once into the upload ring through [`super::stream::Producer::bytes`], in the
//! destination's own format, and reach the image with one
//! [`super::stream::Command::Upload`].

use super::super::{AlphaMode, DepthFormat, Error, Rect};
use super::Gpu;

/// A sampled or rendered image. Clones share the allocation.
#[derive(Clone)]
pub struct Image {
    pub(super) storage: Storage,
    pub(super) alpha: AlphaMode,
}

#[derive(Clone)]
pub(super) enum Storage {
    /// One allocation, and the view draws sample and passes render: one mip
    /// level, or all (`level` is `None`).
    Fixed {
        image: adv_gpu::Image,
        view: adv_gpu::ImageView,
        level: Option<u32>,
    },
    /// A swap buffer: the owner thread picks its allocation for each frame.
    Swap(std::sync::Arc<super::swap::Swap>),
}

impl Image {
    pub fn width(&self) -> u32 {
        self.size().0
    }
    pub fn height(&self) -> u32 {
        self.size().1
    }
    fn size(&self) -> (u32, u32) {
        match &self.storage {
            Storage::Fixed { view, .. } => (view.size().width, view.size().height),
            Storage::Swap(swap) => swap.size,
        }
    }
    pub fn alpha_mode(&self) -> AlphaMode {
        self.alpha
    }
    /// Identity of the view: equal for clones, distinct per mip level. The
    /// recording keys of `adv-dx` use it where they compared wgpu views.
    pub fn id(&self) -> ImageId {
        match &self.storage {
            Storage::Fixed { image, level, .. } => ImageId {
                allocation: image.id(),
                level: *level,
            },
            Storage::Swap(swap) => ImageId {
                allocation: swap.id,
                level: None,
            },
        }
    }
    pub fn mip_levels(&self) -> u32 {
        todo!("C1")
    }
    pub fn mip_level(&self, level: u32) -> Result<Self, Error> {
        let _ = level;
        todo!("C1")
    }
    pub fn shares_storage(&self, other: &Self) -> bool {
        self.id().allocation == other.id().allocation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageId {
    allocation: u64,
    level: Option<u32>,
}

#[derive(Clone)]
pub struct Depth {
    pub(super) image: adv_gpu::Image,
    pub(super) format: DepthFormat,
}

impl Depth {
    pub fn width(&self) -> u32 {
        self.image.desc().size.width
    }
    pub fn height(&self) -> u32 {
        self.image.desc().size.height
    }
    pub fn format(&self) -> DepthFormat {
        self.format
    }
}

#[derive(Clone)]
pub struct Target {
    pub image: Image,
    pub(super) depth: Option<Depth>,
}

impl Target {
    pub fn mip_level(&self, level: u32) -> Result<Self, Error> {
        let _ = level;
        todo!("C1")
    }
    pub fn with_alpha_mode(mut self, alpha: AlphaMode) -> Self {
        self.image.alpha = alpha;
        self
    }
    pub fn depth(&self) -> Option<&Depth> {
        self.depth.as_ref()
    }
    pub fn with_depth(self, depth: Option<Depth>) -> Result<Self, Error> {
        let _ = depth;
        todo!("C1")
    }
}

/// Guest rows already in the upload ring, waiting for their destination.
pub struct Staged {
    pub(super) bytes: super::stream::Bytes,
    pub(super) pitch: u32,
}

impl Staged {
    pub fn pitch(&self) -> u32 {
        self.pitch
    }
    pub fn len(&self) -> usize {
        self.bytes.len as usize
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.len == 0
    }
}

impl Gpu {
    pub fn image(&self, width: u32, height: u32) -> Result<Image, Error> {
        let _ = (width, height);
        todo!("C1")
    }
    pub fn image_levels(&self, width: u32, height: u32, levels: u32) -> Result<Image, Error> {
        let _ = (width, height, levels);
        todo!("C1")
    }
    pub fn target(&self, width: u32, height: u32, depth: bool) -> Result<Target, Error> {
        let _ = (width, height, depth);
        todo!("C1")
    }
    /// The largest image side the guest may create (`D3DCAPS9` texture
    /// limits): the device limit, at most 4096.
    pub fn max_image_size(&self) -> u32 {
        todo!("C1")
    }
    /// A swap buffer target ([`super::swap`]): its image is resolved by the
    /// owner thread, which hands it to presentation without a copy.
    pub fn presentation_target(
        &self,
        width: u32,
        height: u32,
        depth: bool,
    ) -> Result<Target, Error> {
        let _ = (width, height, depth);
        todo!("C1")
    }
    pub fn target_levels(&self, width: u32, height: u32, levels: u32) -> Result<Target, Error> {
        let _ = (width, height, levels);
        todo!("C1")
    }
    pub fn depth(&self, width: u32, height: u32, format: DepthFormat) -> Result<Depth, Error> {
        let _ = (width, height, format);
        todo!("C1")
    }
    /// Write `height` rows through `fill` straight into the upload ring.
    pub fn stage<E>(
        &mut self,
        width: u32,
        height: u32,
        fill: impl FnOnce(&mut dyn FnMut(u32, &[u8])) -> Result<(), E>,
    ) -> Result<Result<Staged, E>, Error> {
        let _ = (width, height, fill);
        todo!("C1")
    }
    pub fn upload_staged(
        &mut self,
        image: &Image,
        rectangle: Rect,
        staged: &Staged,
    ) -> Result<(), Error> {
        let _ = (image, rectangle, staged);
        todo!("C1")
    }
    pub fn upload(
        &mut self,
        image: &Image,
        rectangle: Rect,
        pitch: u32,
        bgra: &[u8],
    ) -> Result<(), Error> {
        let _ = (image, rectangle, pitch, bgra);
        todo!("C1")
    }
    pub fn upload_alpha(
        &mut self,
        image: &Image,
        rectangle: Rect,
        pitch: u32,
        alpha: &[u8],
    ) -> Result<(), Error> {
        let _ = (image, rectangle, pitch, alpha);
        todo!("C1")
    }
}
