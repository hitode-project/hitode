//! Core Animation compositor (macOS, iOS, HOST-VIEW-11). Presentable
//! IOSurfaces become layer contents in a commit that runs on the main thread
//! the moment the GPU finishes their writes: the queue's main notifier calls
//! it, so no thread polls or waits. A replaced surface returns to its pool once
//! the replacing commit ran and Core Animation no longer uses it.

use super::*;
use crate::Rect;
use crate::plan::Layer;
use adv_gpu::present::Export;
use adv_gpu::queue::MainNotifier;
use dispatch2::{DispatchQueue, MainThreadBound};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{MainThreadMarker, msg_send, sel};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGColor;
use objc2_io_surface::IOSurfaceRef;
use objc2_quartz_core::{
    CALayer, CATransaction, kCAFilterLinear, kCAFilterNearest, kCAGravityResize,
};
use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct LayerCompositor {
    caps: CompositorCaps,
    notifier: MainNotifier,
    /// Moved into the last main-thread closure on drop, so the tree is
    /// never dropped off the main thread (which would sync to it).
    tree: ManuallyDrop<Arc<MainThreadBound<RefCell<Tree>>>>,
    shared: Arc<Mutex<Shared>>,
    /// The image each layer shows, with the serial that brought it.
    showing: Vec<(u64, AcquiredImage)>,
    /// Replaced images: their serial and the serial whose commit replaced them.
    retired: Vec<(u64, u64, AcquiredImage)>,
    /// The newest acquire token: every commit waits for it, so commits run in
    /// serial order even when a present brings no image.
    last: Token,
}

#[derive(Default)]
struct Shared {
    committed: u64,
    presented: Vec<Presented>,
}

/// Layer state, touched only on the main thread.
struct Tree {
    root: Retained<CALayer>,
    scale: f64,
    layers: Vec<(LayerId, Retained<CALayer>)>,
}

struct Placement {
    id: LayerId,
    /// Normalized source rectangle.
    crop: CGRect,
    /// Destination in pixels, top left origin.
    dst: Rect,
    nearest: bool,
}

struct Commit {
    background: [f32; 3],
    placements: Vec<Placement>,
    contents: Vec<(LayerId, Surface)>,
}

struct Surface(CFRetained<IOSurfaceRef>);
// SAFETY: IOSurfaces are thread-safe; the reference only travels to the main
// thread, which hands it to Core Animation.
unsafe impl Send for Surface {}

impl LayerCompositor {
    /// `root` becomes the letterbox: its background is `Layers::background`,
    /// and game layers are its sublayers. `scale` converts pixels to points.
    pub fn new(
        root: Retained<CALayer>,
        scale: f64,
        notifier: MainNotifier,
        mut caps: CompositorCaps,
        mtm: MainThreadMarker,
    ) -> Self {
        root.setOpaque(true);
        caps.yuv_layers = true;
        Self {
            caps,
            notifier,
            tree: ManuallyDrop::new(Arc::new(MainThreadBound::new(
                RefCell::new(Tree {
                    root,
                    scale,
                    layers: vec![],
                }),
                mtm,
            ))),
            shared: Arc::default(),
            showing: vec![],
            retired: vec![],
            last: Token(0),
        }
    }

    /// The backing scale changed (the window moved between displays).
    pub fn set_scale(&self, scale: f64, mtm: MainThreadMarker) {
        self.tree.get(mtm).borrow_mut().scale = scale;
    }
}

impl Compositor for LayerCompositor {
    fn caps(&self) -> CompositorCaps {
        self.caps.clone()
    }

    fn present(
        &mut self,
        layers: &Layers,
        _damage: &Tiles,
        images: Vec<AcquiredImage>,
        serial: u64,
        _at: Instant,
    ) -> Result<(), PresentError> {
        let mut contents = Vec::with_capacity(images.len());
        for image in images {
            let Export::IoSurface(surface, _) = image.image.export() else {
                unreachable!("Apple presentable buffers are IOSurfaces")
            };
            // SAFETY: the export is a live IOSurfaceRef borrowed from the image.
            let surface = unsafe { CFRetained::retain(surface.cast::<IOSurfaceRef>()) };
            contents.push((image.layer, Surface(surface)));
            self.last = self.last.max(image.acquire);
            match self
                .showing
                .iter_mut()
                .find(|(_, i)| i.layer == image.layer)
            {
                Some(old) => {
                    let (shown, old) = std::mem::replace(old, (serial, image));
                    self.retired.push((shown, serial, old));
                }
                None => self.showing.push((serial, image)),
            }
        }
        // A video that left the frame retires with its layer.
        let gone = self.showing.extract_if(.., |(_, image)| {
            matches!(image.layer, LayerId::Video(v) if !layers.video.iter().any(|(id, _)| *id == v))
        });
        self.retired
            .extend(gone.map(|(shown, image)| (shown, serial, image)));
        let size = |id: LayerId| {
            self.showing
                .iter()
                .find(|(_, image)| image.layer == id)
                .map(|(_, image)| image.image.image().desc().size)
        };
        let mut placements = Vec::new();
        let mut place = |id: LayerId, layer: &Layer| {
            if let Some(size) = size(id) {
                let (w, h) = (f64::from(size.width), f64::from(size.height));
                let c = layer.crop;
                placements.push(Placement {
                    id,
                    crop: rect(
                        f64::from(c.x) / w,
                        f64::from(c.y) / h,
                        f64::from(c.w) / w,
                        f64::from(c.h) / h,
                    ),
                    dst: layer.dst,
                    nearest: layer.nearest,
                });
            }
        };
        place(LayerId::Base, &layers.base);
        for (id, layer) in &layers.video {
            place(LayerId::Video(*id), layer);
        }
        let commit = Commit {
            background: layers.background,
            placements,
            contents,
        };
        let (tree, shared) = (Arc::clone(&self.tree), self.shared.clone());
        self.notifier.on_complete(
            self.last,
            Box::new(move || {
                let mtm = MainThreadMarker::new().expect("main notifier off the main thread");
                tree.get(mtm).borrow_mut().apply(commit, mtm);
                let mut shared = shared.lock().unwrap();
                shared.committed = serial;
                // Core Animation reports no latch; the commit time stands in.
                shared.presented.push(Presented {
                    serial,
                    latched: Some(Instant::now()),
                    shown: None,
                });
            }),
        );
        Ok(())
    }

    fn drain(&mut self, presented: &mut Vec<Presented>, released: &mut Vec<ReleasedImage>) {
        let committed = {
            let mut shared = self.shared.lock().unwrap();
            presented.append(&mut shared.presented);
            shared.committed
        };
        let done = self.retired.extract_if(.., |(_, by, image)| {
            *by <= committed && !surface(image).is_in_use()
        });
        released.extend(done.map(|(serial, _, image)| ReleasedImage {
            serial,
            layer: image.layer,
            image: image.image,
            fence: None,
        }));
    }
}

impl Drop for LayerCompositor {
    /// Empties the layers on the main thread after the last commit, then
    /// returns every image to its pool once Core Animation lets go of it.
    /// Nothing here waits, so a main thread that tears the window down while
    /// this thread drops cannot deadlock.
    fn drop(&mut self) {
        // SAFETY: `tree` is not used after this.
        let tree = unsafe { ManuallyDrop::take(&mut self.tree) };
        let mut images: Vec<_> = self.showing.drain(..).map(|(_, image)| image).collect();
        images.extend(self.retired.drain(..).map(|(_, _, image)| image));
        self.notifier.on_complete(
            self.last,
            Box::new(move || {
                let mtm = MainThreadMarker::new().expect("main notifier off the main thread");
                tree.get(mtm).borrow_mut().clear();
                retire(images, 60);
                drop(tree);
            }),
        );
    }
}

/// Drops images that Core Animation no longer uses; polls the rest once per
/// display frame for about a second, then lets them go.
fn retire(mut images: Vec<AcquiredImage>, tries: u32) {
    images.retain(|image| tries > 0 && surface(image).is_in_use());
    if !images.is_empty() {
        let when = Duration::from_millis(16).try_into().unwrap();
        let _ = DispatchQueue::main().after(when, move || retire(images, tries - 1));
    }
}

fn surface(image: &AcquiredImage) -> &IOSurfaceRef {
    let Export::IoSurface(surface, _) = image.image.export() else {
        unreachable!("Apple presentable buffers are IOSurfaces")
    };
    // SAFETY: the image keeps its IOSurface alive.
    unsafe { surface.cast::<IOSurfaceRef>().as_ref() }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

impl Tree {
    fn apply(&mut self, commit: Commit, mtm: MainThreadMarker) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let [r, g, b] = commit.background.map(f64::from);
        self.root
            .setBackgroundColor(Some(&CGColor::new_srgb(r, g, b, 1.0)));
        let height = self.root.bounds().size.height;
        let scale = self.scale;
        // Videos that left the frame go; an absent base hides.
        self.layers.retain(|(id, layer)| {
            let placed = commit.placements.iter().any(|p| p.id == *id);
            layer.setHidden(!placed);
            if !placed && matches!(id, LayerId::Video(_)) {
                layer.removeFromSuperlayer();
                return false;
            }
            true
        });
        for placement in &commit.placements {
            let layer = self.layer(placement.id, mtm);
            let d = placement.dst;
            let (w, h) = (f64::from(d.w) / scale, f64::from(d.h) / scale);
            let x = f64::from(d.x) / scale;
            let y = f64::from(d.y) / scale;
            // macOS layers put the origin at the bottom left.
            let y = if cfg!(target_os = "macos") {
                height - y - h
            } else {
                y
            };
            layer.setFrame(rect(x, y, w, h));
            layer.setContentsRect(placement.crop);
            unsafe {
                layer.setMagnificationFilter(if placement.nearest {
                    kCAFilterNearest
                } else {
                    kCAFilterLinear
                });
            }
        }
        for (id, surface) in commit.contents {
            let layer = self.layer(id, mtm);
            // SAFETY: IOSurfaceRef is a supported layer contents object.
            unsafe {
                layer.setContents(Some(
                    &*(CFRetained::as_ptr(&surface.0).as_ptr() as *const AnyObject),
                ))
            };
        }
        CATransaction::commit();
    }

    /// The layer for `id`, created on first use above lower layer kinds.
    fn layer(&mut self, id: LayerId, _mtm: MainThreadMarker) -> &Retained<CALayer> {
        let index = match self.layers.iter().position(|(i, _)| *i == id) {
            Some(index) => index,
            None => {
                let layer = CALayer::new();
                unsafe { layer.setContentsGravity(kCAGravityResize) };
                layer.setOpaque(true);
                // Core Animation's contentsOpaque (HOST-VIEW-11) ignores the
                // IOSurface's alpha channel.
                if layer.respondsToSelector(sel!(setContentsOpaque:)) {
                    let _: () = unsafe { msg_send![&layer, setContentsOpaque: true] };
                }
                layer.setZPosition(match id {
                    LayerId::Base => 0.0,
                    LayerId::Video(_) => 1.0,
                    LayerId::Host(_) => 2.0,
                });
                self.root.addSublayer(&layer);
                self.layers.push((id, layer));
                self.layers.len() - 1
            }
        };
        &self.layers[index].1
    }

    fn clear(&mut self) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for (_, layer) in self.layers.drain(..) {
            layer.removeFromSuperlayer();
        }
        CATransaction::commit();
    }
}
