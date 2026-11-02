//! Ownership at the system compositor boundary. A successful present retains
//! every image (including a direct base) until `drain` returns its release.

use crate::plan::{CompositorCaps, Layers, Tiles};
use adv_gpu::{PresentImage, Token, queue::Fence};
use std::time::Instant;

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
    Host(crate::plan::HostLayer),
}

/// The executor marks the image `presented(acquire)` before transferring it.
/// This crate only holds handles; it does not call the GPU backend.
pub struct AcquiredImage {
    pub layer: LayerId,
    pub image: PresentImage,
    pub acquire: Token,
}

pub struct ReleasedImage {
    pub serial: u64,
    pub layer: LayerId,
    pub image: PresentImage,
    pub fence: Option<Fence>,
}

/// On error image ownership returns to the executor for retirement. An image
/// whose write completed still contains that write even if attachment failed.
pub struct PresentError {
    pub error: Error,
    pub images: Vec<AcquiredImage>,
}

pub trait Compositor {
    fn caps(&self) -> CompositorCaps;
    /// Attach all layers atomically at or after `at`. Implementations order
    /// acquire tokens before latching.
    fn present(
        &mut self,
        layers: &Layers,
        damage: &Tiles,
        images: Vec<AcquiredImage>,
        serial: u64,
        at: Instant,
    ) -> Result<(), PresentError>;
    /// Nonblocking, oldest first. The caller returns each released image to its
    /// pool with `PresentImage::released`, forwarding the optional fence.
    fn drain(&mut self, presented: &mut Vec<Presented>, released: &mut Vec<ReleasedImage>);
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
    held: Vec<(u64, AcquiredImage)>,
    presented: Vec<Presented>,
    released: Vec<ReleasedImage>,
}

impl HeadlessCompositor {
    pub fn new(caps: CompositorCaps) -> Self {
        Self {
            caps,
            presentations: vec![],
            held: vec![],
            presented: vec![],
            released: vec![],
        }
    }
}

impl Compositor for HeadlessCompositor {
    fn caps(&self) -> CompositorCaps {
        self.caps.clone()
    }

    fn present(
        &mut self,
        layers: &Layers,
        damage: &Tiles,
        images: Vec<AcquiredImage>,
        serial: u64,
        at: Instant,
    ) -> Result<(), PresentError> {
        self.released
            .extend(self.held.drain(..).map(|(serial, acquired)| ReleasedImage {
                serial,
                layer: acquired.layer,
                image: acquired.image,
                fence: None,
            }));
        self.presentations.push(Presentation {
            layers: layers.clone(),
            damage: damage.clone(),
            serial,
            at,
            acquires: images.iter().map(|i| (i.layer, i.acquire)).collect(),
        });
        self.held = images.into_iter().map(|i| (serial, i)).collect();
        self.presented.push(Presented {
            serial,
            latched: Some(at),
            shown: Some(at),
        });
        Ok(())
    }

    fn drain(&mut self, presented: &mut Vec<Presented>, released: &mut Vec<ReleasedImage>) {
        presented.append(&mut self.presented);
        released.append(&mut self.released);
    }
}
