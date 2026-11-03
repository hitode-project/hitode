//! Core Animation compositor (macOS, iOS, HOST-VIEW-11). Presentable
//! IOSurfaces become layer contents in a commit that runs on the main thread
//! the moment the GPU finishes their writes: the queue's main notifier calls
//! it, so no thread polls or waits. A replaced surface is released once the
//! replacing commit ran and Core Animation no longer uses it.

use super::*;
use crate::Rect;
use crate::plan::Layer;
use adv_gpu::present::Export;
use adv_gpu::queue::MainNotifier;
use dispatch2::MainThreadBound;
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

pub struct LayerCompositor {
    caps: CompositorCaps,
    notifier: MainNotifier,
    /// Moved into the last main-thread closure on drop, so the tree is
    /// never dropped off the main thread (which would sync to it).
    tree: ManuallyDrop<Arc<MainThreadBound<RefCell<Tree>>>>,
    shared: Arc<Mutex<Shared>>,
    /// Each layer's image: the serial that brought it and its size.
    showing: Vec<(u64, LayerId, (u32, u32))>,
    /// Replaced images: their serial, the serial whose commit replaced them,
    /// and their layer.
    retired: Vec<(u64, u64, LayerId)>,
    /// The newest acquire token: every commit waits for it, so commits run in
    /// serial order even when a present brings no image.
    last: Token,
}

#[derive(Default)]
struct Shared {
    committed: u64,
    presented: Vec<Presented>,
    wake: Option<Wake>,
    scale: f64,
}

/// Layer state, touched only on the main thread.
struct Tree {
    root: Retained<CALayer>,
    placements: Vec<Placement>,
    background: Option<[f32; 3]>,
    scale: f64,
    layers: Vec<(LayerId, Retained<CALayer>)>,
}

#[derive(PartialEq)]
struct Placement {
    id: LayerId,
    /// Normalized source rectangle.
    crop: CGRect,
    /// Destination in pixels, top left origin.
    dst: Rect,
    nearest: bool,
    color: Option<[f32; 4]>,
    z: f64,
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
                    placements: vec![],
                    background: None,
                    scale,
                    layers: vec![],
                }),
                mtm,
            ))),
            shared: Arc::new(Mutex::new(Shared {
                scale,
                ..Default::default()
            })),
            showing: vec![],
            retired: vec![],
            last: Token(0),
        }
    }
}

impl Compositor for LayerCompositor {
    fn set_waker(&mut self, wake: Wake) {
        self.shared.lock().unwrap().wake = Some(wake);
    }
    fn set_scale(&mut self, scale: f64) {
        self.shared.lock().unwrap().scale = scale;
        let (tree, shared) = (Arc::clone(&self.tree), self.shared.clone());
        self.notifier.on_complete(
            self.last,
            Box::new(move || {
                let mtm = MainThreadMarker::new().unwrap();
                let scale = shared.lock().unwrap().scale;
                CATransaction::begin();
                CATransaction::setDisableActions(true);
                let mut tree = tree.get(mtm).borrow_mut();
                if tree.scale != scale {
                    tree.place(scale);
                }
                CATransaction::commit();
            }),
        );
    }
    fn detach(&mut self) -> Option<Fence> {
        let (send, done) = std::sync::mpsc::sync_channel(1);
        let tree = Arc::clone(&self.tree);
        self.notifier.on_complete(
            self.last,
            Box::new(move || {
                let mtm = MainThreadMarker::new().expect("detach runs through the main notifier");
                tree.get(mtm).borrow_mut().clear(Box::new(move || {
                    send.send(()).unwrap();
                }));
            }),
        );
        done.recv().unwrap();
        self.retired.clear();
        self.showing.clear();
        None
    }

    fn caps(&self) -> CompositorCaps {
        self.caps.clone()
    }

    fn present(
        &mut self,
        layers: &Layers,
        _damage: &Tiles,
        images: &mut [AcquiredImage],
        serial: u64,
        _at: Instant,
    ) -> Result<(), Error> {
        let mut contents = Vec::with_capacity(images.len());
        for image in images.iter() {
            let Export::IoSurface(surface, _) = image.image.export() else {
                unreachable!("Apple presentable buffers are IOSurfaces")
            };
            // SAFETY: the export is a live IOSurfaceRef borrowed from the image.
            let surface = unsafe { CFRetained::retain(surface.cast::<IOSurfaceRef>()) };
            contents.push((image.layer, Surface(surface)));
            self.last = self.last.max(image.acquire);
            let size = image.image.image().desc().size;
            let shown = (serial, image.layer, (size.width, size.height));
            match self.showing.iter_mut().find(|(_, l, _)| *l == image.layer) {
                Some(old) => {
                    let (since, layer, _) = std::mem::replace(old, shown);
                    self.retired.push((since, serial, layer));
                }
                None => self.showing.push(shown),
            }
        }
        // A video that left the frame retires with its layer.
        let gone = self.showing.extract_if(.., |(_, layer, _)| {
            matches!(layer, LayerId::Video(v) if !layers.video.iter().any(|(id, _)| id == v))
        });
        self.retired
            .extend(gone.map(|(since, layer, _)| (since, serial, layer)));
        let size = |id: LayerId| {
            self.showing
                .iter()
                .find(|(_, layer, _)| *layer == id)
                .map(|(_, _, size)| *size)
        };
        let mut placements = Vec::new();
        let mut place = |id: LayerId, layer: &Layer, z: f64| {
            if let Some((w, h)) = size(id) {
                let (w, h) = (f64::from(w), f64::from(h));
                let c = layer.crop;
                placements.push(Placement {
                    id,
                    crop: if let Some([x, y, w, h]) = layer.source_uv {
                        rect(x as f64, y as f64, w as f64, h as f64)
                    } else {
                        rect(
                            c.x as f64 / w,
                            c.y as f64 / h,
                            c.w as f64 / w,
                            c.h as f64 / h,
                        )
                    },
                    dst: layer.dst,
                    nearest: layer.nearest,
                    color: None,
                    z,
                });
            }
        };
        place(LayerId::Base, &layers.base, 0.);
        for (i, (id, layer)) in layers.video.iter().enumerate() {
            place(LayerId::Video(*id), layer, (i * 2 + 2) as f64);
        }
        for (id, dst, color) in &layers.solids {
            let z = layers.video.iter().position(|(v, _)| v == id).unwrap() * 2 + 1;
            placements.push(Placement {
                id: LayerId::VideoBackground(*id),
                crop: rect(0., 0., 1., 1.),
                dst: *dst,
                nearest: false,
                color: Some(*color),
                z: z as f64,
            });
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
                let scale = shared.lock().unwrap().scale;
                tree.get(mtm).borrow_mut().apply(
                    commit,
                    scale,
                    mtm,
                    Box::new(move || {
                        let mut shared = shared.lock().unwrap();
                        shared.committed = serial;
                        shared.presented.push(Presented {
                            serial,
                            latched: Some(Instant::now()),
                            shown: None,
                        });
                        let wake = shared.wake.clone();
                        drop(shared);
                        if let Some(wake) = wake {
                            wake();
                        }
                    }),
                );
            }),
        );
        Ok(())
    }

    fn drain(&mut self, presented: &mut Vec<Presented>, released: &mut Vec<Released>) {
        let committed = {
            let mut shared = self.shared.lock().unwrap();
            presented.append(&mut shared.presented);
            shared.committed
        };
        let done = self.retired.extract_if(.., |(_, by, _)| *by <= committed);
        released.extend(done.map(|(serial, _, layer)| Released {
            serial,
            layer,
            fence: None,
        }));
    }
}

impl Drop for LayerCompositor {
    /// Empties the layers on the main thread after the last commit. Nothing
    /// here waits, so a main thread that tears the window down while this
    /// thread drops cannot deadlock. Core Animation retains the surfaces it
    /// still shows.
    fn drop(&mut self) {
        // SAFETY: `tree` is not used after this.
        let tree = unsafe { ManuallyDrop::take(&mut self.tree) };
        self.notifier.on_complete(
            self.last,
            Box::new(move || {
                let mtm = MainThreadMarker::new().expect("main notifier off the main thread");
                tree.get(mtm).borrow_mut().clear(Box::new(|| {}));
            }),
        );
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

impl Tree {
    fn apply(
        &mut self,
        commit: Commit,
        scale: f64,
        mtm: MainThreadMarker,
        done: Box<dyn FnOnce() + Send>,
    ) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if self.background != Some(commit.background) {
            let [r, g, b] = commit.background.map(f64::from);
            self.root
                .setBackgroundColor(Some(&CGColor::new_srgb(r, g, b, 1.)));
            self.background = Some(commit.background);
        }
        if self.placements != commit.placements {
            // Videos that left the frame go; an absent base hides.
            self.layers.retain(|(id, layer)| {
                let placed = commit.placements.iter().any(|p| p.id == *id);
                layer.setHidden(!placed);
                if !placed && matches!(id, LayerId::Video(_) | LayerId::VideoBackground(_)) {
                    layer.removeFromSuperlayer();
                    return false;
                }
                true
            });
            for placement in &commit.placements {
                let layer = self.layer(placement.id, mtm);
                layer.setContentsRect(placement.crop);
                layer.setZPosition(placement.z);
                if let Some([r, g, b, a]) = placement.color {
                    layer.setBackgroundColor(Some(&CGColor::new_srgb(
                        r as f64, g as f64, b as f64, a as f64,
                    )));
                }

                unsafe {
                    layer.setMagnificationFilter(if placement.nearest {
                        kCAFilterNearest
                    } else {
                        kCAFilterLinear
                    });
                }
            }
            self.placements = commit.placements;
            self.place(scale);
        } else if self.scale != scale {
            self.place(scale);
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
        completion(done);
        CATransaction::commit();
    }
    fn place(&mut self, scale: f64) {
        self.scale = scale;
        let height = self.root.bounds().size.height;
        for p in &self.placements {
            let layer = &self.layers.iter().find(|(id, _)| *id == p.id).unwrap().1;
            let d = p.dst;
            let (w, h) = (f64::from(d.w) / scale, f64::from(d.h) / scale);
            let (x, y) = (f64::from(d.x) / scale, f64::from(d.y) / scale);
            let y = if cfg!(target_os = "macos") {
                height - y - h
            } else {
                y
            };
            layer.setFrame(rect(x, y, w, h));
        }
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
                    LayerId::Video(_) | LayerId::VideoBackground(_) => 1.0,
                    LayerId::Host(_) => 2.0,
                });
                self.root.addSublayer(&layer);
                self.layers.push((id, layer));
                self.layers.len() - 1
            }
        };
        &self.layers[index].1
    }

    fn clear(&mut self, done: Box<dyn FnOnce() + Send>) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for (_, layer) in self.layers.drain(..) {
            layer.removeFromSuperlayer();
        }
        self.placements.clear();
        completion(done);
        CATransaction::commit();
    }
}

// CATransaction copies the block; the images remain leased until that commit completes.
fn completion(done: Box<dyn FnOnce() + Send>) {
    let done = Mutex::new(Some(done));
    let block = block2::RcBlock::new(move || {
        if let Some(done) = done.lock().unwrap().take() {
            done();
        }
    });
    unsafe {
        CATransaction::setCompletionBlock(Some(&block));
    }
}
