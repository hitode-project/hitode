//! Presentable buffers: images the system compositor can show without a copy.
//!
//! Frame-buffer compression is a per-vendor allocation policy, never assumed:
//!
//! - Android allocates AHardwareBuffers with GPU sampling, GPU output and
//!   composer usage and no CPU usage bit for RGB; any CPU bit makes gralloc fall back
//!   to linear layout. Qualcomm gralloc compresses (UBWC) only when asked
//!   with its private bit (`AHARDWAREBUFFER_USAGE_VENDOR_0`, i.e.
//!   `GRALLOC_USAGE_PRIVATE_ALLOC_UBWC`). Measured on Adreno 750
//!   (2026-11-02), a full 3120×1440 draw of compressible content costs 372 µs
//!   without it and 97 µs with it, the same as a private image. Mali gralloc
//!   chooses AFBC by itself when producer and consumers support it; no bit is
//!   added. Where `VK_EXT_image_compression_control` exists (Mali G610 and
//!   later, Adreno 8xx), the backend reads each imported buffer's compression
//!   and reports it to the speed report and flight recorder.
//!   NV21 video buffers instead request CPU writing and GPU sampling with
//!   composer overlay usage; their YUV layout is linear.
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
    /// Fixed matrix and range: required for NV12 or NV21, `None` for RGB.
    pub color: Option<YuvColor>,
    pub width: u32,
    pub height: u32,
    /// A format with [`FormatCaps::PRESENTABLE`].
    pub format: Format,
    /// Limit for ordinary acquisition; indexed acquisition grows to the planner's slots.
    pub count: u32,
    /// Compute writes the buffers as storage images (the upscaler's final
    /// layer). RGB only; on Mali it may cost the buffers their compression.
    pub storage: bool,
    pub label: &'static str,
}

/// The picture's YCbCr matrix and code-value range, fixed for a pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum YuvColor {
    Bt601Limited,
    Bt601Full,
    Bt709Limited,
    Bt709Full,
}

/// One CPU-writable plane. Rows include padding. Both formats have Y first;
/// NV12 chroma interleaves Cb then Cr (UV), NV21 Cr then Cb (VU).
pub struct PlaneMut<'a> {
    pub data: &'a mut [u8],
    pub stride: usize,
}

/// Buffers allocated on demand, by newest free content or stable planner index.
/// They carry their age and release fence; the pool orders reuse after the compositor.
#[derive(Clone)]
pub struct PresentPool {
    #[cfg(target_os = "android")]
    pub(crate) video: Option<Arc<imp::VideoPool>>,
    pub(crate) device: Device,
    desc: PoolDesc,
    shared: Arc<Mutex<Shared>>,
}

struct Shared {
    slots: Vec<Slot>,
    /// Presents of this pool so far; a buffer presented at `n` has age
    /// `presents - n + 1`.
    presents: u64,
}

enum Slot {
    Vacant,
    Leased,
    Free(Buffer),
}
impl Shared {
    fn available(&self, limit: u32) -> Option<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                Slot::Free(b) => Some((i, b.stamp)),
                _ => None,
            })
            .max_by_key(|(_, stamp)| *stamp)
            .map(|(i, _)| i)
            .or_else(|| {
                (self
                    .slots
                    .iter()
                    .filter(|s| !matches!(s, Slot::Vacant))
                    .count()
                    < limit as usize)
                    .then(|| {
                        self.slots
                            .iter()
                            .position(|s| matches!(s, Slot::Vacant))
                            .unwrap_or(self.slots.len())
                    })
            })
    }
}
struct Buffer {
    index: usize,
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
            || matches!(desc.format, Format::Nv12 | Format::Nv21) != desc.color.is_some()
            || desc.storage && desc.color.is_some()
        {
            return Err(crate::Error::Invalid(format!("present pool {desc:?}")));
        }
        Ok(Self {
            device: device.clone(),
            desc: *desc,
            #[cfg(target_os = "android")]
            video: imp::VideoPool::for_pool(&device.inner, desc)?,
            shared: Arc::new(Mutex::new(Shared {
                slots: Vec::with_capacity(desc.count as usize),
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
        let Some(index) = shared.available(self.desc.count) else {
            return Ok(None);
        };
        self.acquire_gpu(&mut shared, queue, index)
    }
    /// Acquire a CPU-writable YUV lease, ordering the previous release on this
    /// thread. Allocation and driver failures fail fast; `None` means busy.
    pub fn acquire_cpu(&mut self) -> Option<PresentImage> {
        debug_assert!(matches!(self.desc.format, Format::Nv12 | Format::Nv21));
        let mut shared = self.shared.lock().unwrap();
        let index = shared.available(self.desc.count)?;
        let mut buffer = self
            .take_buffer(&mut shared, index)
            .expect("allocate CPU presentable buffer")
            .unwrap();
        if let Some(fence) = buffer.release.take() {
            fence.inner.wait_cpu();
        }
        Some(self.leased(buffer, shared.presents, true))
    }
    fn take_buffer(&self, shared: &mut Shared, index: usize) -> Result<Option<Buffer>> {
        shared
            .slots
            .resize_with(shared.slots.len().max(index + 1), || Slot::Vacant);
        match std::mem::replace(&mut shared.slots[index], Slot::Leased) {
            Slot::Leased => Ok(None),
            Slot::Free(buffer) => Ok(Some(buffer)),
            Slot::Vacant => {
                let result = imp::PresentBuffer::new(
                    &self.device.inner,
                    &self.desc,
                    #[cfg(target_os = "android")]
                    self.video.as_ref(),
                );
                match result {
                    Ok((image, raw)) => Ok(Some(Buffer {
                        index,
                        image,
                        raw,
                        stamp: None,
                        release: None,
                    })),
                    Err(error) => {
                        shared.slots[index] = Slot::Vacant;
                        Err(error)
                    }
                }
            }
        }
    }
    fn leased(&self, buffer: Buffer, presents: u64, cpu: bool) -> PresentImage {
        PresentImage {
            age: buffer.stamp.map(|n| (presents - n + 1) as u32),
            buffer: Some(buffer),
            cpu,
            fence: None,
            color: self.desc.color,
            pool: self.clone(),
        }
    }
    fn acquire_gpu(
        &self,
        shared: &mut Shared,
        queue: &mut Queue,
        index: usize,
    ) -> Result<Option<PresentImage>> {
        let Some(mut buffer) = self.take_buffer(shared, index)? else {
            return Ok(None);
        };
        if let Some(fence) = buffer.release.take()
            && let Err(error) = queue.inner.wait_next(fence.inner)
        {
            shared.slots[index] = Slot::Vacant;
            return Err(error);
        }
        Ok(Some(self.leased(buffer, shared.presents, false)))
    }
    pub fn description(&self) -> &PoolDesc {
        &self.desc
    }
    pub fn id(&self) -> usize {
        Arc::as_ptr(&self.shared) as usize
    }
    pub fn weak(&self) -> PoolWeak {
        PoolWeak(Arc::downgrade(&self.shared))
    }
    /// Acquire the planner's stable slot. New slots extend this pool lazily.
    /// With no write, reuse initialized content and forward its release fence
    /// directly to presentation, avoiding a GPU submission just to wait.
    pub fn acquire_index(
        &mut self,
        queue: &mut Queue,
        index: usize,
        writing: bool,
    ) -> Result<Option<PresentImage>> {
        let mut shared = self.shared.lock().unwrap();
        if writing {
            return self.acquire_gpu(&mut shared, queue, index);
        }
        let Some(buffer) = self.take_buffer(&mut shared, index)? else {
            return Ok(None);
        };
        debug_assert!(buffer.stamp.is_some(), "reuse requires initialized content");
        let mut image = self.leased(buffer, shared.presents, false);
        drop(shared);
        image.fence = image
            .buffer()
            .release
            .as_ref()
            .map(Fence::duplicate)
            .transpose()?;
        Ok(Some(image))
    }
    /// Drop the oldest free content until at most `keep` allocations remain.
    pub fn trim(&mut self, keep: u32) {
        let mut shared = self.shared.lock().unwrap();
        let count = shared
            .slots
            .iter()
            .filter(|s| !matches!(s, Slot::Vacant))
            .count();
        for _ in keep as usize..count {
            let oldest = shared
                .slots
                .iter()
                .enumerate()
                .filter_map(|(i, s)| match s {
                    Slot::Free(b) => Some((i, b.stamp)),
                    _ => None,
                })
                .min_by_key(|(_, stamp)| *stamp)
                .map(|(i, _)| i);
            let Some(i) = oldest else {
                break;
            };
            shared.slots[i] = Slot::Vacant;
        }
    }
}

/// The color attachment format of native presentable buffers.
#[cfg(target_os = "android")]
pub const COLOR_FORMAT: Format = Format::Rgba8Unorm;
#[cfg(not(target_os = "android"))]
pub const COLOR_FORMAT: Format = Format::Bgra8Unorm;

pub struct PoolWeak(std::sync::Weak<Mutex<Shared>>);
impl PoolWeak {
    pub fn alive(&self) -> bool {
        self.0.strong_count() != 0
    }
}

/// One pooled buffer. It returns to the pool when the compositor releases it
/// ([`PresentImage::released`]) or when dropped unpresented, with its age
/// unchanged then.
pub struct PresentImage {
    buffer: Option<Buffer>,
    age: Option<u32>,
    fence: Option<Fence>,
    cpu: bool,
    color: Option<YuvColor>,
    pool: PresentPool,
}

impl PresentImage {
    pub fn pool(&self) -> &PresentPool {
        &self.pool
    }
    pub fn index(&self) -> usize {
        self.buffer().index
    }
    /// Map a CPU-acquired YUV buffer, call `f` once, then unmap even on unwind.
    /// `acquire_cpu` has already ordered the write after compositor release.
    pub fn write(&mut self, f: impl FnOnce(&mut [PlaneMut<'_>])) {
        debug_assert!(self.cpu, "write requires acquire_cpu");
        self.buffer().raw.write(f);
        #[cfg(target_os = "android")]
        self.buffer().image.inner.cpu_written();
        let mut pool = self.pool.shared.lock().unwrap();
        pool.presents += 1;
        self.buffer.as_mut().unwrap().stamp = Some(pool.presents);
    }

    pub fn image(&self) -> &Image {
        &self.buffer().image
    }
    /// The pool's matrix and range: `Some` exactly for NV12 or NV21.
    pub fn color(&self) -> Option<YuvColor> {
        self.color
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
    /// Preserve the detach fence for both a subsequent attachment and a writer.
    pub fn detached(&mut self, fence: Option<Fence>) -> Result<()> {
        self.buffer.as_mut().unwrap().release = fence.as_ref().map(Fence::duplicate).transpose()?;
        self.fence = fence;
        Ok(())
    }
    /// The compositor no longer reads the buffer once `fence` signals.
    pub fn released(mut self, fence: Option<Fence>) {
        if fence.is_some() {
            self.buffer.as_mut().unwrap().release = fence;
        }
    }
    #[cfg(target_os = "android")]
    pub(crate) fn video_set(&self) -> (&Arc<imp::VideoPool>, ash::vk::DescriptorSet) {
        self.buffer().raw.video_set()
    }
    fn buffer(&self) -> &Buffer {
        self.buffer.as_ref().unwrap()
    }
}

impl Drop for PresentImage {
    fn drop(&mut self) {
        let buffer = self.buffer.take().unwrap();
        let index = buffer.index;
        self.pool.shared.lock().unwrap().slots[index] = Slot::Free(buffer);
    }
}

impl Queue {
    /// Submit `encoder` as the last write of `images`, which go to the system
    /// compositor: the backend ends the submission with each buffer's hand-off
    /// (a queue-family release on Vulkan) and signals their acquire fence in
    /// the same batch. Each image's age restarts here. Every submission also
    /// returns the presentable images it only sampled to the compositor.
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
            let mut pool = image.pool.shared.lock().unwrap();
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
