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
    /// Decoded video shown as its own layers (package E).
    pub video: Arc<[VideoPlane]>,
    pub published_at: Instant,
}

pub enum Base {
    /// A D3D9 or DirectDraw back buffer; its last write completes at `ready`.
    Gpu {
        image: adv_gpu::Image,
        damage: Damage,
        ready: Token,
    },
}

/// Package E.
pub struct VideoPlane {}

/// The display layer's entry on the GPU owner thread. It encodes its stages on
/// the same queue, after the frame's own commands.
pub trait Presenter: Send {
    fn present(&mut self, queue: &mut adv_gpu::Queue, frame: Frame);
}
