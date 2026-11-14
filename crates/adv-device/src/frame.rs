//! The frame contract between producers (the D3D9 executor, GDI composition,
//! video decoders) and the display layer (issue #103). Producers publish
//! frames; the display layer plans and presents them on the GPU owner thread.

#[cfg(feature = "test-support")]
pub mod test_support;

use adv_gpu::{Rect, Token};
use std::sync::Arc;
use std::time::Instant;

/// Changed pixels since the producer's previous frame, in the base's size.
pub use adv_gpu::Region;

pub struct Frame {
    pub serial: u64,
    pub base: Base,
    /// Decoded pictures and their canvas geometry.
    pub video: Arc<[VideoPlane]>,
    /// Guest windows over everything else, bottom first (WINE-GFX-10).
    pub windows: Arc<[WindowLayer]>,
    pub published_at: Instant,
}

/// A guest window shown over the frame as a layer of its own, as a native
/// window shows over the game: the host draws it at its own scale, never
/// through the base's scaling (WINE-GFX-10).
#[derive(Clone, Debug, PartialEq)]
pub struct WindowLayer {
    /// The guest window; host input on the layer goes to it.
    pub id: u32,
    /// Top-left corner in canvas pixels, possibly outside the canvas.
    pub at: (i32, i32),
    pub width: u32,
    pub height: u32,
    /// Top-down `Bgra32` rows of `width * 4` bytes.
    pub pixels: Arc<[u8]>,
    /// Premultiplied alpha in `pixels` instead of opaque fill.
    pub layered: bool,
    /// Changes whenever `pixels` do.
    pub version: u64,
}

pub use crate::surface::PixelFormat;
pub type Palette = Arc<[[u8; 4]; 256]>;

pub enum Base {
    /// A D3D9 or DirectDraw swap buffer; its last write completes at `ready`.
    Gpu {
        image: SwapImage,
        damage: Region,
        ready: Token,
    },
    /// Guest-format pixels in GPU-visible memory, normalized to top-down rows.
    Software {
        slot: Slot,
        format: PixelFormat,
        palette: Option<Palette>,
        damage: Region,
    },
}

/// One software publication. The owner reports its last read with [`Slot::ingested`].
/// Dropping an unencoded publication returns it immediately.
pub struct Slot {
    pub(crate) buffer: adv_gpu::Buffer,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pitch: u32,
    pub(crate) state: Arc<std::sync::atomic::AtomicU64>,
}
impl Slot {
    /// Allocate one guest-format upload slot. Rows may have guest padding.
    pub fn new(
        device: &adv_gpu::Device,
        width: u32,
        height: u32,
        pitch: u32,
        format: PixelFormat,
    ) -> adv_gpu::Result<Self> {
        crate::surface::size(width, height).map_err(adv_gpu::Error::Invalid)?;
        let bytes = pitch
            .checked_mul(height)
            .filter(|_| pitch >= format.row_bytes(width))
            .ok_or_else(|| adv_gpu::Error::Invalid("software slot pitch".into()))?;
        let buffer = device.create_buffer(&adv_gpu::resource::BufferDesc {
            // Byte extraction uses u32 loads, including the final pixel.
            size: u64::from(bytes).next_multiple_of(4),
            usage: adv_gpu::resource::BufferUsage::STORAGE,
            memory: adv_gpu::resource::Memory::Shared,
            label: "software slot",
        })?;
        Ok(Self {
            buffer,
            width,
            height,
            pitch,
            state: Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX)),
        })
    }

    pub fn buffer(&self) -> &adv_gpu::Buffer {
        &self.buffer
    }
    pub fn extent(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn pitch(&self) -> u32 {
        self.pitch
    }
    /// Call after submitting every stage that reads this slot, on its owner queue.
    pub fn ingested(self, ready: Token) {
        debug_assert_ne!(ready.0, 0);
        self.state
            .store(ready.0, std::sync::atomic::Ordering::Release);
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        let _ = self.state.compare_exchange(
            u64::MAX,
            0,
            std::sync::atomic::Ordering::Release,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
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
#[derive(Clone, Copy, Debug, PartialEq)]
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
    /// Service notifications on the owner. `Some(wake)` also services UI work:
    /// the guest encoder is closed, so the hook may encode settings and captures.
    fn service(&mut self, queue: &mut adv_gpu::Queue, wake: Option<&Waker>);
}

pub use crate::gpu::native::stream::Waker;
pub struct Captured {
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}
pub type CaptureReply = std::sync::mpsc::Sender<Result<Captured, String>>;
