//! The frame contract between producers (the D3D9 executor, GDI composition,
//! video decoders) and the display layer (issue #103). Producers publish
//! frames; the display layer plans and presents them on the GPU owner thread.

use adv_gpu::{Rect, Token};
use arrayvec::ArrayVec;
use std::sync::Arc;
use std::time::Instant;

/// Changed pixels since the producer's previous frame. More than four
/// rectangles collapse into their union.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Damage {
    None,
    Rects(ArrayVec<Rect, 4>),
    Whole,
}

pub struct Frame {
    pub serial: u64,
    pub base: Base,
    /// Decoded pictures and their canvas geometry.
    pub video: Arc<[VideoPlane]>,
    pub published_at: Instant,
}

pub enum Base {
    /// A D3D9 or DirectDraw swap buffer; its last write completes at `ready`.
    Gpu {
        image: SwapImage,
        damage: Damage,
        ready: Token,
    },
}

pub enum SwapImage {
    /// Read by the display layer's stages, which the owner thread encodes
    /// before the next frame writes the image.
    Private(adv_gpu::Image),
    /// Shown by reference; back in its pool once the compositor releases it.
    Presentable(adv_gpu::PresentImage),
}

/// A decoded picture. The lease can be transferred once, even when several
/// queued frames refer to this same picture. An unshown picture keeps its lease.
pub struct DecodedPicture {
    image: adv_gpu::Image,
    planes: Option<[adv_gpu::ImageView; 2]>,
    color: adv_gpu::present::YuvColor,
    lease: std::sync::Mutex<Option<adv_gpu::PresentImage>>,
    #[cfg(feature = "media")]
    source: ffmpeg_next::frame::Video,
}

impl std::fmt::Debug for DecodedPicture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedPicture")
            .field("id", &self.image.id())
            .finish()
    }
}

impl DecodedPicture {
    #[cfg(feature = "media")]
    pub(crate) fn new(image: adv_gpu::Image, color: adv_gpu::present::YuvColor) -> Self {
        let planes = (image.desc().format == adv_gpu::Format::Nv12).then(|| {
            std::array::from_fn(|plane| {
                image
                    .create_view(&adv_gpu::resource::ViewDesc {
                        format: Some([adv_gpu::Format::R8Unorm, adv_gpu::Format::Rg8Unorm][plane]),
                        dimension: None,
                        mips: 0..1,
                        layers: 0..1,
                        plane: plane as u32,
                    })
                    .expect("NV12 plane view")
            })
        });
        Self {
            image,
            planes,
            color,
            lease: std::sync::Mutex::new(None),
            source: ffmpeg_next::frame::Video::empty(),
        }
    }
    #[cfg(feature = "media")]
    pub(crate) fn release_unshown(&mut self) {
        self.lease.get_mut().unwrap().take();
    }
    #[cfg(feature = "media")]
    pub(crate) fn set_image(
        &mut self,
        image: adv_gpu::PresentImage,
        source: &mut ffmpeg_next::frame::Video,
    ) {
        // Retain decoder planes for guest snapshots after the present lease moves.
        unsafe {
            ffmpeg_next::ffi::av_frame_unref(self.source.as_mut_ptr());
            ffmpeg_next::ffi::av_frame_move_ref(self.source.as_mut_ptr(), source.as_mut_ptr());
        }
        *self.lease.get_mut().unwrap() = Some(image);
    }
    pub(crate) fn bgra(&self) -> Vec<u8> {
        #[cfg(feature = "media")]
        {
            crate::media::planes::bgra(&self.source, self.color)
        }
        #[cfg(not(feature = "media"))]
        {
            unreachable!("decoded pictures require media")
        }
    }
    pub fn color(&self) -> adv_gpu::present::YuvColor {
        self.color
    }
    pub fn image(&self) -> &adv_gpu::Image {
        &self.image
    }
    pub fn planes(&self) -> &[adv_gpu::ImageView; 2] {
        self.planes.as_ref().expect("NV12 plane views")
    }
}

/// Geometry after owner clipping, in canvas coordinates. The UV rectangle
/// preserves sampling phase for signed destinations and cropped source pictures.
#[derive(Clone, Copy, Debug)]
pub struct VideoGeometry {
    pub source: (u32, u32),
    pub destination: Rect,
    pub source_uv: [f32; 4],
    pub background: Option<(Rect, [u8; 4])>,
    pub clip: Rect,
    pub hide_cursor: bool,
}

pub struct VideoPlane {
    pub id: u64,
    pub geometry: VideoGeometry,
    pub picture: Arc<DecodedPicture>,
}

/// A changed picture transfers its lease; a retained picture refers to the
/// layer's already attached buffer. There is no separately writable changed bit.
pub enum VideoUpdate {
    Changed(adv_gpu::PresentImage),
    Retained,
}

impl VideoPlane {
    pub fn take_picture(&self) -> VideoUpdate {
        match self.picture.lease.lock().unwrap().take() {
            Some(image) => VideoUpdate::Changed(image),
            None => VideoUpdate::Retained,
        }
    }
}

/// The display layer's entry on the GPU owner thread. It encodes its stages on
/// the same queue, after the frame's own commands.
pub trait Presenter: Send {
    /// Whether swap buffers of this size should be presentable buffers: the
    /// display plan shows them by reference (`SwapRole` in `adv-display`).
    /// Changes with settings or rotation, never per frame.
    fn presentable(&self, width: u32, height: u32) -> bool;
    fn present(&mut self, queue: &mut adv_gpu::Queue, frame: Frame);
    /// Return the images the compositor released to their pools; nonblocking.
    fn drain(&mut self);
}
