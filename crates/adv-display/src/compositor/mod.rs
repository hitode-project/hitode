//! The system compositor boundary. The executor owns every image; a
//! compositor keeps only native references and reports, per present serial and
//! layer, when it stopped reading an image.

#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(target_vendor = "apple")]
pub use apple::LayerCompositor;

use crate::plan::{CompositorCaps, Layers, Tiles};
use adv_gpu::{PresentImage, Token, queue::Fence};
use std::time::Instant;

#[cfg(any(target_os = "android", test))]
pub mod android;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Presented {
    pub serial: u64,
    /// When the compositor latched the buffers, if reported.
    pub latched: Option<Instant>,
    /// When the frame reached the panel, if reported.
    pub shown: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Rebuild the compositor; keep the pools.
    SurfaceLost,
    DeviceLost(String),
}

/// Identifies the layer whose image is transferred, including host layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerId {
    Base,
    Video(u64),
    VideoBackground(u64),
    Host(crate::plan::HostLayer),
}

/// GPU writers submit the image's last write with `Queue::submit_present`,
/// whose token is `acquire`, before transferring it. Android compositors take
/// the latch fence from `PresentImage::acquire_fence`; Apple compositors
/// attach once `acquire` completes. This crate only holds handles; it does not
/// call the GPU backend. CPU writers use `PresentPool::acquire_cpu` and `write`,
/// then transfer with `Token(0)` and no acquire fence.
pub struct AcquiredImage {
    pub layer: LayerId,
    pub image: PresentImage,
    pub acquire: Token,
}

/// The image `layer` showed since present `serial` is no longer read once
/// `fence` signals.
pub struct Released {
    pub serial: u64,
    pub layer: LayerId,
    pub fence: Option<Fence>,
}

pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;
pub trait Compositor: Send {
    fn set_waker(&mut self, _wake: Wake) {}
    fn set_scale(&mut self, _scale: f64) {}
    /// Empty every layer and wait for that to take effect, before the
    /// compositor drops. Every attached image is free once the returned fence
    /// signals; no releases are reported for them.
    fn detach(&mut self) -> Option<Fence>;

    fn caps(&self) -> CompositorCaps;
    /// Attach all layers atomically at or after `at`. Implementations order
    /// acquire tokens before latching. The caller keeps `images` until their
    /// release; on error none of them was attached.
    fn present(
        &mut self,
        layers: &Layers,
        damage: &Tiles,
        images: &mut [AcquiredImage],
        serial: u64,
        at: Instant,
    ) -> Result<(), Error>;
    /// Nonblocking, oldest first.
    fn drain(&mut self, presented: &mut Vec<Presented>, released: &mut Vec<Released>);
}

#[derive(Clone, Debug, PartialEq)]
pub struct Presentation {
    pub layers: Layers,
    pub damage: Tiles,
    pub serial: u64,
    pub at: Instant,
    pub acquires: Vec<(LayerId, Token)>,
}

/// A deterministic compositor without a device or a window. Requested times
/// are also latch and shown times. Images release on the next `present`.
pub struct HeadlessCompositor {
    caps: CompositorCaps,
    pub presentations: Vec<Presentation>,
    /// The serial and layer of each attached image.
    showing: Vec<(u64, LayerId)>,
    presented: Vec<Presented>,
    released: Vec<Released>,
}

impl HeadlessCompositor {
    pub fn new(caps: CompositorCaps) -> Self {
        Self {
            caps,
            presentations: vec![],
            showing: vec![],
            presented: vec![],
            released: vec![],
        }
    }
}

impl Compositor for HeadlessCompositor {
    fn detach(&mut self) -> Option<Fence> {
        self.showing.clear();
        None
    }
    fn caps(&self) -> CompositorCaps {
        self.caps.clone()
    }

    fn present(
        &mut self,
        layers: &Layers,
        damage: &Tiles,
        images: &mut [AcquiredImage],
        serial: u64,
        at: Instant,
    ) -> Result<(), Error> {
        let replaced = self.showing.extract_if(.., |(_, layer)| {
            images.iter().any(|i| i.layer == *layer)
                || matches!(layer, LayerId::Video(id) if !layers.video.iter().any(|(v, _)| v == id))
        });
        self.released
            .extend(replaced.map(|(serial, layer)| Released {
                serial,
                layer,
                fence: None,
            }));
        self.presentations.push(Presentation {
            layers: layers.clone(),
            damage: damage.clone(),
            serial,
            at,
            acquires: images.iter().map(|i| (i.layer, i.acquire)).collect(),
        });
        self.showing
            .extend(images.iter().map(|i| (serial, i.layer)));
        self.presented.push(Presented {
            serial,
            latched: Some(at),
            shown: Some(at),
        });
        Ok(())
    }

    fn drain(&mut self, presented: &mut Vec<Presented>, released: &mut Vec<Released>) {
        presented.append(&mut self.presented);
        released.append(&mut self.released);
    }
}
