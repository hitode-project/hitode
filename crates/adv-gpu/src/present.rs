//! Presentable buffers: images the system compositor can show without a copy.
//!
//! Frame-buffer compression is a per-vendor allocation policy, never assumed:
//!
//! - Android allocates AHardwareBuffers with GPU sampling, GPU output and
//!   composer usage and no CPU usage bit; any CPU bit makes gralloc fall back
//!   to linear layout. Qualcomm gralloc compresses (UBWC) only when asked
//!   with its private bit (`AHARDWAREBUFFER_USAGE_VENDOR_0`, i.e.
//!   `GRALLOC_USAGE_PRIVATE_ALLOC_UBWC`). Measured on Adreno 750
//!   (2026-11-02), a full 3120×1440 draw of compressible content costs 372 µs
//!   without it and 97 µs with it, the same as a private image. Mali gralloc
//!   chooses AFBC by itself when producer and consumers support it; no bit is
//!   added. Where `VK_EXT_image_compression_control` exists (Mali G610 and
//!   later, Adreno 8xx), the backend reads each imported buffer's compression
//!   and reports it to the speed report and flight recorder.
//! - Apple backs them with IOSurfaces, which are not losslessly compressed:
//!   measured on A12, a full 2560×1440 draw costs 544 µs into an IOSurface
//!   against 220 µs into a private texture.
//!
//! [`crate::Caps::format`] reports `PRESENTABLE` together with whether such
//! buffers are compressed, and `SwapRole` in `adv-display` follows it: where
//! presentable buffers cost what private images cost (UBWC, AFBC), D3D9 swap
//! buffers live in them; where they do not (IOSurface), only when the plan
//! shows them by reference.
//!
//! A pool tracks each buffer's age, the number of presents since its content
//! was current, so callers redraw only the damage since then (GFX-REC-5).

use crate::queue::Fence;
use crate::{Format, Image, Token};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PoolDesc {
    pub width: u32,
    pub height: u32,
    /// An RGBA format, or `Nv12`/`P010` for video planes.
    pub format: Format,
    pub count: u32,
    pub label: &'static str,
}

pub struct PresentPool {
    pub(crate) inner: crate::imp::PresentPool,
}

impl PresentPool {
    /// A free buffer, or `None` while every buffer is in flight or on screen.
    pub fn acquire(&mut self) -> Option<PresentImage> {
        self.inner.acquire()
    }
    /// Drop free buffers down to `keep`, for memory pressure and background.
    pub fn trim(&mut self, keep: u32) {
        self.inner.trim(keep)
    }
}

/// One pooled buffer. It returns to the pool when the compositor releases it
/// ([`PresentImage::released`]) or when dropped unpresented, with its age
/// unchanged then.
pub struct PresentImage {
    pub(crate) inner: crate::imp::PresentImage,
}

impl PresentImage {
    pub fn image(&self) -> &Image {
        self.inner.image()
    }
    /// Presents since this buffer's content was current; `None` when its
    /// content is undefined (new, or invalidated by a resize).
    pub fn age(&self) -> Option<u32> {
        self.inner.age()
    }
    /// The platform handle the compositor attaches. The image stays owned by
    /// the pool; the handle is valid until the release is reported.
    pub fn export(&self) -> Export<'_> {
        self.inner.export()
    }
    /// Mark the buffer handed to the compositor after `written` completes.
    pub fn presented(&mut self, written: Token) {
        self.inner.presented(written)
    }
    /// The compositor no longer reads the buffer once `fence` signals.
    pub fn released(self, fence: Option<Fence>) {
        self.inner.released(fence)
    }
}

#[non_exhaustive]
pub enum Export<'a> {
    /// `AHardwareBuffer*`, borrowed.
    AndroidHardwareBuffer(
        std::ptr::NonNull<std::ffi::c_void>,
        std::marker::PhantomData<&'a ()>,
    ),
    /// `IOSurfaceRef`, borrowed.
    IoSurface(
        std::ptr::NonNull<std::ffi::c_void>,
        std::marker::PhantomData<&'a ()>,
    ),
    /// Headless and swapchain compositors copy or sample the image instead.
    None,
}
