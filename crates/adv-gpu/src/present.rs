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

use crate::queue::{Device, Fence, Queue};
use crate::{Encoder, Format, FormatCaps, Image, Result, Token, imp};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PoolDesc {
    pub width: u32,
    pub height: u32,
    /// A format with [`FormatCaps::PRESENTABLE`].
    pub format: Format,
    pub count: u32,
    pub label: &'static str,
}

/// Up to `count` buffers, allocated on demand. Buffers carry their age and
/// any release fence; the pool, not its users, orders reuse after the
/// compositor.
pub struct PresentPool {
    device: Device,
    desc: PoolDesc,
    shared: Arc<Mutex<Shared>>,
}

struct Shared {
    free: Vec<Buffer>,
    allocated: u32,
    /// Presents of this pool so far; a buffer presented at `n` has age
    /// `presents - n + 1`.
    presents: u64,
}

struct Buffer {
    image: Image,
    raw: imp::PresentBuffer,
    stamp: Option<u64>,
    release: Option<Fence>,
}

impl PresentPool {
    pub(crate) fn new(device: &Device, desc: &PoolDesc) -> Result<Self> {
        let caps = device.caps();
        if !caps.format(desc.format).contains(FormatCaps::PRESENTABLE)
            || desc.width == 0
            || desc.height == 0
            || desc.width.max(desc.height) > caps.limits.max_image_2d
            || desc.count == 0
        {
            return Err(crate::Error::Invalid(format!("present pool {desc:?}")));
        }
        Ok(Self {
            device: device.clone(),
            desc: *desc,
            shared: Arc::new(Mutex::new(Shared {
                free: Vec::new(),
                allocated: 0,
                presents: 0,
            })),
        })
    }
    /// The free buffer with the newest content, allocating one while fewer
    /// than `count` exist; `None` while every buffer is in flight or on
    /// screen. A buffer's release fence becomes a wait of `queue`'s next
    /// submission, so callers never order against the compositor.
    pub fn acquire(&mut self, queue: &mut Queue) -> Result<Option<PresentImage>> {
        let mut shared = self.shared.lock().unwrap();
        let newest = (0..shared.free.len()).max_by_key(|&i| shared.free[i].stamp);
        let mut buffer = match newest {
            Some(i) => shared.free.swap_remove(i),
            None if shared.allocated < self.desc.count => {
                let (image, raw) = imp::PresentBuffer::new(&self.device.inner, &self.desc)?;
                shared.allocated += 1;
                Buffer {
                    image,
                    raw,
                    stamp: None,
                    release: None,
                }
            }
            None => return Ok(None),
        };
        let age = buffer.stamp.map(|n| (shared.presents - n + 1) as u32);
        if let Some(fence) = buffer.release.take() {
            queue.inner.wait_next(fence.inner);
        }
        Ok(Some(PresentImage {
            buffer: Some(buffer),
            age,
            fence: None,
            pool: self.shared.clone(),
        }))
    }
    /// Drop free buffers, oldest content first, until at most `keep` remain
    /// allocated; for memory pressure and background.
    pub fn trim(&mut self, keep: u32) {
        let mut shared = self.shared.lock().unwrap();
        shared
            .free
            .sort_unstable_by_key(|b| std::cmp::Reverse(b.stamp));
        while shared.allocated > keep && shared.free.pop().is_some() {
            shared.allocated -= 1;
        }
    }
}

/// One pooled buffer. It returns to the pool when the compositor releases it
/// ([`PresentImage::released`]) or when dropped unpresented, with its age
/// unchanged then.
pub struct PresentImage {
    buffer: Option<Buffer>,
    age: Option<u32>,
    fence: Option<Fence>,
    pool: Arc<Mutex<Shared>>,
}

impl PresentImage {
    pub fn image(&self) -> &Image {
        &self.buffer().image
    }
    /// Presents since this buffer's content was current; `None` when its
    /// content is undefined (new to the pool).
    pub fn age(&self) -> Option<u32> {
        self.age
    }
    /// The platform handle the compositor attaches. The image stays owned by
    /// the pool; the handle is valid until the release is reported.
    pub fn export(&self) -> Export<'_> {
        self.buffer().raw.export()
    }
    /// After [`Queue::submit_present`], the fence the compositor waits on
    /// before latching this buffer: a sync fd on Android, `None` on Apple,
    /// where the compositor attaches once the token completes.
    pub fn acquire_fence(&mut self) -> Option<Fence> {
        self.fence.take()
    }
    /// The compositor no longer reads the buffer once `fence` signals.
    pub fn released(mut self, fence: Option<Fence>) {
        self.buffer.as_mut().unwrap().release = fence;
    }
    fn buffer(&self) -> &Buffer {
        self.buffer.as_ref().unwrap()
    }
}

impl Drop for PresentImage {
    fn drop(&mut self) {
        let buffer = self.buffer.take().unwrap();
        self.pool.lock().unwrap().free.push(buffer);
    }
}

impl Queue {
    /// Submit `encoder` as the last write of `images`, which go to the system
    /// compositor: the backend ends the submission with each buffer's hand-off
    /// (a queue-family release on Vulkan) and signals their acquire fence in
    /// the same batch. Each image's age restarts here.
    pub fn submit_present(
        &mut self,
        encoder: Encoder,
        images: &mut [&mut PresentImage],
    ) -> Result<Token> {
        let (token, fence) = {
            let buffers: arrayvec::ArrayVec<_, 8> = images
                .iter()
                .map(|i| (&i.buffer().image.inner, &i.buffer().raw))
                .collect();
            self.inner.submit_present(encoder, &buffers)?
        };
        let mut fence = fence.map(|inner| Fence { inner });
        let last = images.len().saturating_sub(1);
        for (n, image) in images.iter_mut().enumerate() {
            let mut pool = image.pool.lock().unwrap();
            pool.presents += 1;
            image.buffer.as_mut().unwrap().stamp = Some(pool.presents);
            image.fence = if n == last {
                fence.take()
            } else {
                fence.as_ref().map(Fence::duplicate).transpose()?
            };
        }
        Ok(token)
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
