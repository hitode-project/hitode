//! Package C2: attachments whose allocation the GPU owner thread picks.
//!
//! The execution thread holds one handle per swap buffer and per depth
//! buffer and records every frame against it; the owner thread decides,
//! when it encodes, what memory is behind it.
//!
//! # Swap buffers
//!
//! - Private, while [`crate::frame::Presenter::presentable`] says no: one
//!   private (compressed) image, presented as `SwapImage::Private`. The
//!   display stages read it in queue order, before the next frame writes it.
//! - Presentable: a pool from `Device::create_present_pool`. After each
//!   `Present` the owner acquires the next buffer and, before the next pass
//!   on it, copies from the buffer just presented what is not current in it:
//!   the damage of the presents since its age, everything when its content is
//!   undefined (a new buffer, or a change of role). The frame then goes to
//!   the presenter as `SwapImage::Presentable`, with no copy.
//!
//! The guest's damage therefore always describes one frame ago (the
//! recording sees age one), and no frame waits for a buffer on the execution
//! thread. While a pool has no free buffer the owner calls
//! `Presenter::drain` and retries at most every millisecond, leaving the
//! execution thread's chunks queued meanwhile.
//!
//! # Depth buffers
//!
//! A depth buffer starts without memory. A pass whose plan stores depth
//! ([`super::pass::Pass::depth`] or `stencil` with `Store::Store`) gives it a
//! real allocation first; until then, and again after a frame in which no
//! pass stored it, it is transient (`Usage::TRANSIENT`: memoryless on Apple,
//! lazily allocated on Vulkan) and the real allocation is freed. A pass that
//! loads depth always follows one that stored it, so transient memory is
//! never loaded.

/// Shared by the images of one swap buffer.
pub(crate) struct Swap {
    /// Image identity, in a range adv-gpu image ids never reach.
    pub id: u64,
    pub size: (u32, u32),
    /// The owner thread's half; the execution thread never locks it.
    pub(super) owner: std::sync::Mutex<SwapBacking>,
}

/// The allocation of the current frame, the presented buffers' damage
/// history, and the pool while presentable.
#[derive(Default)]
pub(super) struct SwapBacking {
    current: Option<crate::frame::SwapImage>,
    previous: Option<crate::frame::SwapImage>,
    pool: Option<adv_gpu::PresentPool>,
    role: Option<bool>,
    history: std::collections::VecDeque<crate::frame::Damage>,
}

/// Shared by the clones of one depth buffer.
pub(crate) struct DepthBuffer {
    pub size: (u32, u32),
    pub format: super::super::DepthFormat,
    /// The owner thread's half; the execution thread never locks it.
    pub(super) owner: std::sync::Mutex<DepthBacking>,
}

/// Transient or real, and whether the current frame stored it.
#[derive(Default)]
pub(super) struct DepthBacking {
    image: Option<adv_gpu::Image>,
    stored: bool,
}

use super::resources::Storage;
use super::{Depth, Image};
use crate::frame::{Damage, Presenter, SwapImage};
use adv_gpu::encode::{ImageCopy, Load, Store};
use adv_gpu::resource::{Dimension, ImageDesc, Usage};
use adv_gpu::{Device, Encoder, Extent, Format, Queue, Rect};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

impl Swap {
    pub fn new(size: (u32, u32)) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1 << 63);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            size,
            owner: Mutex::new(SwapBacking::default()),
        }
    }
}

impl DepthBuffer {
    pub fn new(size: (u32, u32), format: super::super::DepthFormat) -> Self {
        Self {
            size,
            format,
            owner: Mutex::new(DepthBacking::default()),
        }
    }
}

pub(super) fn depth_format(format: super::super::DepthFormat) -> Format {
    match format {
        super::super::DepthFormat::Unorm16 => Format::Depth16Unorm,
        super::super::DepthFormat::Float32 => Format::Depth32Float,
        super::super::DepthFormat::Depth24Stencil8 => Format::Depth24UnormStencil8,
    }
}

pub(super) fn image_desc(size: (u32, u32), format: Format, usage: Usage) -> ImageDesc {
    ImageDesc {
        dimension: Dimension::D2,
        size: Extent {
            width: size.0,
            height: size.1,
            depth_or_layers: 1,
        },
        format,
        mips: 1,
        samples: 1,
        usage,
        view_formats: &[],
        label: "executor attachment",
    }
}

impl SwapBacking {
    fn acquire(
        &mut self,
        swap: &Swap,
        queue: &mut Queue,
        presenter: &mut dyn Presenter,
        encoder: &mut Encoder,
    ) -> adv_gpu::Result<adv_gpu::Image> {
        let presentable = presenter.presentable(swap.size.0, swap.size.1);
        if self
            .current
            .as_ref()
            .is_some_and(|image| matches!(image, SwapImage::Presentable(_)) == presentable)
        {
            return Ok(self.current_image().clone());
        }
        let changed = self.role != Some(presentable);
        if changed {
            self.previous = self.current.take().or_else(|| self.previous.take());
            self.pool = None;
            self.history.clear();
            self.role = Some(presentable);
        }
        let current = if presentable {
            if self.pool.is_none() {
                self.pool = Some(queue.device().create_present_pool(
                    &adv_gpu::present::PoolDesc {
                        width: swap.size.0,
                        height: swap.size.1,
                        format: swap_format(),
                        color: None,
                        count: 3,
                        storage: false,
                        label: "executor swap",
                    },
                )?);
            }
            let pool = self.pool.as_mut().unwrap();
            let buffer = loop {
                if let Some(buffer) = pool.acquire(queue)? {
                    break buffer;
                }
                presenter.drain();
                std::thread::sleep(Duration::from_millis(1));
            };
            SwapImage::Presentable(buffer)
        } else {
            SwapImage::Private(queue.device().create_image(&image_desc(
                swap.size,
                swap_format(),
                Usage::COLOR | Usage::SAMPLED | Usage::COPY_SRC | Usage::COPY_DST,
            ))?)
        };
        let image = raw(&current);
        if let Some(previous) = &self.previous {
            let age = match &current {
                SwapImage::Presentable(image) => image.age(),
                SwapImage::Private(_) => None,
            };
            let whole = Rect {
                x: 0,
                y: 0,
                w: swap.size.0,
                h: swap.size.1,
            };
            let damage = if changed || age.is_none_or(|age| age as usize > self.history.len() + 1) {
                Damage::Whole
            } else {
                let mut damage = Damage::None;
                for old in self
                    .history
                    .iter()
                    .rev()
                    .take(age.unwrap().saturating_sub(1) as usize)
                {
                    merge_damage(&mut damage, old);
                }
                damage
            };
            match damage {
                Damage::None => {}
                Damage::Whole => repair(encoder, raw(previous), image, whole),
                Damage::Rects(rects) => {
                    for rect in rects {
                        repair(encoder, raw(previous), image, rect);
                    }
                }
            }
        }
        self.current = Some(current);
        Ok(self.current_image().clone())
    }

    pub(super) fn current_image(&self) -> &adv_gpu::Image {
        raw(self.current.as_ref().unwrap())
    }

    pub fn take_present(&mut self) -> SwapImage {
        self.current.take().unwrap()
    }

    pub fn presented(&mut self, image: &SwapImage, damage: &Damage) {
        // Keep the latest allocation alive as the source of age repair. A
        // presentable lease itself goes to the compositor, never to this clone.
        self.previous = Some(SwapImage::Private(raw(image).clone()));
        if matches!(image, SwapImage::Private(_)) {
            self.current = Some(SwapImage::Private(raw(image).clone()));
        }
        self.history.push_back(damage.clone());
        if self.history.len() > 8 {
            self.history.pop_front();
        }
    }
}

fn raw(image: &SwapImage) -> &adv_gpu::Image {
    match image {
        SwapImage::Private(image) => image,
        SwapImage::Presentable(image) => image.image(),
    }
}

fn repair(
    encoder: &mut Encoder,
    source: &adv_gpu::Image,
    destination: &adv_gpu::Image,
    rect: Rect,
) {
    if source.id() == destination.id() {
        return;
    }
    encoder.copy_image(
        ImageCopy {
            image: source,
            mip: 0,
            layer: 0,
            origin: (rect.x, rect.y, 0),
        },
        ImageCopy {
            image: destination,
            mip: 0,
            layer: 0,
            origin: (rect.x, rect.y, 0),
        },
        Extent {
            width: rect.w,
            height: rect.h,
            depth_or_layers: 1,
        },
    );
}

pub(super) fn resolve(
    image: &Image,
    queue: &mut Queue,
    presenter: &mut dyn Presenter,
    encoder: &mut Encoder,
) -> adv_gpu::Result<(adv_gpu::Image, adv_gpu::ImageView, u32)> {
    match &image.storage {
        Storage::Fixed { image, view, level } => {
            Ok((image.clone(), view.clone(), level.unwrap_or(0)))
        }
        Storage::Swap(swap) => {
            let image = swap
                .owner
                .lock()
                .unwrap()
                .acquire(swap, queue, presenter, encoder)?;
            let view = image.view().clone();
            Ok((image, view, 0))
        }
    }
}

impl DepthBacking {
    pub fn attachment(
        &mut self,
        depth: &Depth,
        device: &Device,
        plans: &[super::pass::Pass],
    ) -> adv_gpu::Result<adv_gpu::Image> {
        let stores = plans.iter().any(|p| {
            p.depth.1 == Store::Store || depth.format().has_stencil() && p.stencil.1 == Store::Store
        });
        let loads = plans.iter().any(|p| {
            p.depth.0 == Load::Load || depth.format().has_stencil() && p.stencil.0 == Load::Load
        });
        // A pass over part of a real image keeps the contents outside it.
        let (width, height) = depth.0.size;
        let keeps = self
            .image
            .as_ref()
            .is_some_and(|image| !image.desc().usage.contains(Usage::TRANSIENT))
            && plans
                .iter()
                .any(|p| p.area.x != 0 || p.area.y != 0 || p.area.w < width || p.area.h < height);
        let stores = stores || keeps;
        let real = self.stored || stores || loads;
        if self
            .image
            .as_ref()
            .is_none_or(|image| image.desc().usage.contains(Usage::TRANSIENT) == real)
        {
            let usage = Usage::DEPTH_STENCIL
                | if real {
                    Usage::empty()
                } else {
                    Usage::TRANSIENT
                };
            self.image = Some(device.create_image(&image_desc(
                depth.0.size,
                depth_format(depth.format()),
                usage,
            ))?);
        }
        self.stored |= stores;
        Ok(self.image.as_ref().unwrap().clone())
    }

    pub fn end_frame(&mut self) {
        if !self.stored
            && self
                .image
                .as_ref()
                .is_some_and(|image| !image.desc().usage.contains(Usage::TRANSIENT))
        {
            self.image = None;
        }
        self.stored = false;
    }

    pub fn discard(&mut self) {
        self.image = None;
        self.stored = false;
    }
}

pub(super) fn swap_format() -> Format {
    if cfg!(target_os = "android") {
        Format::Rgba8Unorm
    } else {
        Format::Bgra8Unorm
    }
}

/// Keep repair regions disjoint, coalescing intersecting bounds and reducing
/// overflow to one bounding rectangle as GFX-REC-4 does.
fn merge_damage(damage: &mut Damage, new: &Damage) {
    match new {
        Damage::None => {}
        Damage::Whole => *damage = Damage::Whole,
        Damage::Rects(rects) => {
            for &rect in rects {
                if matches!(damage, Damage::Whole) {
                    return;
                }
                if matches!(damage, Damage::None) {
                    *damage = Damage::Rects(Default::default());
                }
                let Damage::Rects(out) = damage else {
                    unreachable!()
                };
                let mut rect = rect;
                let mut i = 0;
                while i < out.len() {
                    let other = out[i];
                    if rect.x <= other.x + other.w
                        && other.x <= rect.x + rect.w
                        && rect.y <= other.y + other.h
                        && other.y <= rect.y + rect.h
                    {
                        rect = bounds(rect, out.swap_remove(i));
                        i = 0;
                    } else {
                        i += 1;
                    }
                }
                if out.is_full() {
                    rect = out.iter().copied().fold(rect, bounds);
                    out.clear();
                }
                out.push(rect);
            }
        }
    }
}
fn bounds(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect {
        x,
        y,
        w: (a.x + a.w).max(b.x + b.w) - x,
        h: (a.y + a.h).max(b.y + b.h) - y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: GFX-PASS-3
    #[test]
    fn a_real_depth_image_outlives_a_frame_of_partial_passes() {
        let device = match Device::open(&adv_gpu::queue::DeviceOptions {
            software: !cfg!(target_vendor = "apple"),
            validation: true,
            label: "backing test",
            ..Default::default()
        }) {
            Ok((device, _)) => device,
            Err(adv_gpu::Error::Unavailable(_))
                if std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_none() =>
            {
                return;
            }
            Err(error) => panic!("device: {error}"),
        };
        let depth = Depth(std::sync::Arc::new(DepthBuffer::new(
            (8, 8),
            super::super::super::DepthFormat::Depth24Stencil8,
        )));
        let pass = |area, store| super::super::pass::Pass {
            ops: 0..1,
            area,
            color: (Load::Load, Store::Store),
            depth: (Load::Clear(1.), store),
            stencil: (Load::Clear(0), store),
        };
        let mut backing = DepthBacking::default();
        backing
            .attachment(&depth, &device, &[pass(rect(0, 0, 8, 8), Store::Store)])
            .unwrap();
        backing.end_frame();
        // The area is cleared again before any read, but the rest of the
        // image keeps the first frame's values.
        backing
            .attachment(&depth, &device, &[pass(rect(0, 0, 4, 4), Store::DontCare)])
            .unwrap();
        backing.end_frame();
        assert!(
            backing
                .image
                .as_ref()
                .is_some_and(|image| !image.desc().usage.contains(Usage::TRANSIENT))
        );
    }

    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }
    fn damage(rects: &[Rect]) -> Damage {
        Damage::Rects(rects.iter().copied().collect())
    }
    fn check_once(damage: &Damage, required: &[Rect]) {
        let Damage::Rects(rects) = damage else {
            panic!("rectangle repair");
        };
        for y in 0..16 {
            for x in 0..16 {
                let inside = |r: &Rect| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;
                let copies = rects.iter().filter(|r| inside(r)).count();
                assert!(copies <= 1, "repair copied ({x}, {y}) more than once");
                if required.iter().any(inside) {
                    assert_eq!(copies, 1, "missing repair at ({x}, {y})");
                }
            }
        }
    }
    // spec: GFX-REC-4, GFX-REC-5
    #[test]
    fn repair_merges_overlap_across_frames_and_preserves_disjoint_regions() {
        let required = [
            rect(0, 0, 3, 3),
            rect(2, 2, 3, 3),
            rect(10, 10, 1, 1),
            rect(4, 0, 2, 3),
        ];
        let mut merged = Damage::None;
        for r in required {
            merge_damage(&mut merged, &damage(&[r]));
        }
        check_once(&merged, &required);
        let Damage::Rects(rects) = &merged else {
            unreachable!()
        };
        assert_eq!(rects.len(), 2);
        assert!(rects.contains(&rect(0, 0, 6, 5)));
        assert!(rects.contains(&rect(10, 10, 1, 1)));
    }
    // spec: GFX-REC-4, GFX-REC-5
    #[test]
    fn repair_overflow_bounds_every_region_and_whole_supersedes_rectangles() {
        let required = [
            rect(0, 0, 1, 1),
            rect(2, 2, 1, 1),
            rect(4, 4, 1, 1),
            rect(6, 6, 1, 1),
            rect(8, 8, 1, 1),
        ];
        let mut merged = Damage::None;
        for r in required {
            merge_damage(&mut merged, &damage(&[r]));
        }
        assert_eq!(merged, damage(&[rect(0, 0, 9, 9)]));
        check_once(&merged, &required);
        merge_damage(&mut merged, &Damage::Whole);
        merge_damage(&mut merged, &damage(&[rect(10, 10, 1, 1)]));
        assert_eq!(merged, Damage::Whole);
    }
}
