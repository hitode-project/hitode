//! `LayerCompositor` against real Core Animation layers and Metal presentable
//! buffers. Its callbacks run on the main dispatch queue, so this test owns the
//! main thread (no harness) and pumps the run loop.

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
    println!("core_animation: ok");
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod macos {
    use adv_display::Rect;
    use adv_display::compositor::{AcquiredImage, Compositor, LayerCompositor, LayerId};
    use adv_display::plan::{CompositorCaps, Damage, Layer, Layers, Rotation, Source, Tiles};
    use adv_gpu::encode::{ColorAttachment, Load, Store};
    use adv_gpu::present::{Export, PoolDesc, PresentPool};
    use adv_gpu::queue::DeviceOptions;
    use adv_gpu::{Device, Error, Format, Queue, RenderPassDesc};
    use arrayvec::ArrayVec;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_core_foundation::{CFRunLoop, CGPoint, CGRect, CGSize, kCFRunLoopDefaultMode};
    use objc2_quartz_core::{CALayer, kCAFilterLinear, kCAFilterNearest};
    use smallvec::smallvec;
    use std::time::{Duration, Instant};

    const W: u32 = 400;
    const H: u32 = 200;

    pub fn run() {
        let mtm = MainThreadMarker::new().expect("main thread");
        let (device, mut queue) = match Device::open(&DeviceOptions {
            ring_size: 1 << 20,
            label: "core animation",
            ..DeviceOptions::default()
        }) {
            Ok(opened) => opened,
            Err(Error::Unavailable(reason))
                if std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_none() =>
            {
                println!("skipped: {reason}");
                return;
            }
            Err(error) => panic!("open device: {error}"),
        };
        let mut pool = device
            .create_present_pool(&PoolDesc {
                width: W,
                height: H,
                format: Format::Bgra8Unorm,
                count: 3,
                label: "core animation",
            })
            .expect("pool");
        // 200×100 points at 2× backing scale.
        let root = CALayer::new();
        root.setBounds(rect(0.0, 0.0, 200.0, 100.0));
        let mut compositor =
            LayerCompositor::new(root.clone(), 2.0, queue.main_notifier(), caps(), mtm);
        let mut presented = vec![];
        let mut released = vec![];
        // Drains until `serial` committed and `releases` images came back;
        // Core Animation may let go of a surface a run loop turn later.
        let mut drain = |compositor: &mut LayerCompositor, serial: u64, releases: usize| {
            let start = Instant::now();
            while presented
                .last()
                .is_none_or(|p: &adv_display::compositor::Presented| p.serial < serial)
                || released.len() < releases
            {
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "serial {serial} not committed"
                );
                CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.005, true);
                compositor.drain(&mut presented, &mut released);
            }
            std::mem::take(&mut released)
        };

        // The first image attaches with nearest filtering at the full view.
        let (a, a_surface) = draw(&mut pool, &mut queue, LayerId::Base);
        let whole = Rect {
            x: 0,
            y: 0,
            w: W,
            h: H,
        };
        present(
            &mut compositor,
            layers(whole, whole, true, None),
            vec![a],
            1,
        );
        assert!(drain(&mut compositor, 1, 0).is_empty());
        let base = sublayers(&root);
        assert_eq!(base.len(), 1);
        let base = &base[0];
        assert_eq!(contents(base), a_surface);
        assert_eq!(base.frame(), rect(0.0, 0.0, 200.0, 100.0));
        assert_eq!(&*base.magnificationFilter(), unsafe { kCAFilterNearest });

        // A replacing image releases the first once its commit ran, reported
        // with the serial that presented it. macOS puts the origin bottom left.
        let (b, b_surface) = draw(&mut pool, &mut queue, LayerId::Base);
        let inset = Rect {
            x: 40,
            y: 20,
            w: 320,
            h: 160,
        };
        present(
            &mut compositor,
            layers(whole, inset, false, None),
            vec![b],
            2,
        );
        let out = drain(&mut compositor, 2, 1);
        assert_eq!(
            out.iter().map(|r| (r.serial, r.layer)).collect::<Vec<_>>(),
            [(1, LayerId::Base)]
        );
        out.into_iter().for_each(|r| r.image.released(r.fence));
        assert_eq!(contents(base), b_surface);
        assert_eq!(base.frame(), rect(20.0, 10.0, 160.0, 80.0));
        assert_eq!(&*base.magnificationFilter(), unsafe { kCAFilterLinear });

        // Geometry alone: a half crop, no release.
        let half = Rect {
            x: 0,
            y: 0,
            w: W / 2,
            h: H,
        };
        present(&mut compositor, layers(half, whole, false, None), vec![], 3);
        assert!(drain(&mut compositor, 3, 0).is_empty());
        assert_eq!(base.contentsRect(), rect(0.0, 0.0, 0.5, 1.0));
        assert_eq!(contents(base), b_surface);

        // A video layer comes above the base and goes with its image.
        let (v, v_surface) = draw(&mut pool, &mut queue, LayerId::Video(7));
        present(
            &mut compositor,
            layers(whole, whole, false, Some(inset)),
            vec![v],
            4,
        );
        assert!(drain(&mut compositor, 4, 0).is_empty());
        let both = sublayers(&root);
        assert_eq!(both.len(), 2);
        let video = both
            .iter()
            .find(|l| contents(l) == v_surface)
            .expect("video layer");
        assert!(video.zPosition() > base.zPosition());
        present(
            &mut compositor,
            layers(whole, whole, false, None),
            vec![],
            5,
        );
        let out = drain(&mut compositor, 5, 1);
        assert_eq!(
            out.iter().map(|r| (r.serial, r.layer)).collect::<Vec<_>>(),
            [(4, LayerId::Video(7))]
        );
        out.into_iter().for_each(|r| r.image.released(r.fence));
        assert_eq!(sublayers(&root).len(), 1);

        // Drop empties the tree on main and returns every image to its pool.
        drop(compositor);
        let start = Instant::now();
        while !sublayers(&root).is_empty() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "layers not removed"
            );
            CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.005, true);
        }
        let mut held = vec![];
        let start = Instant::now();
        while held.len() < 3 {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "images not returned"
            );
            CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.005, true);
            held.extend(pool.acquire(&mut queue).expect("acquire"));
        }
    }

    fn caps() -> CompositorCaps {
        CompositorCaps {
            scales: true,
            nearest: true,
            rotates: false,
            yuv_layers: false,
            solid_color_layers: true,
            damage_hints: false,
            presentable_compressed: true,
            tile: (32, 32),
        }
    }

    fn layer(source: Source, crop: Rect, dst: Rect, nearest: bool) -> Layer {
        Layer {
            source,
            crop,
            dst,
            rotation: Rotation::None,
            nearest,
            damage: Damage::Whole,
        }
    }

    fn layers(crop: Rect, dst: Rect, nearest: bool, video: Option<Rect>) -> Layers {
        let whole = Rect {
            x: 0,
            y: 0,
            w: W,
            h: H,
        };
        Layers {
            base: layer(Source::Base, crop, dst, nearest),
            video: video
                .map(|dst| smallvec![(7, layer(Source::Video(7), whole, dst, false))])
                .unwrap_or_default(),
            host: smallvec![],
            background: [0.0, 0.0, 0.0],
        }
    }

    fn present(
        compositor: &mut LayerCompositor,
        layers: Layers,
        images: Vec<AcquiredImage>,
        serial: u64,
    ) {
        assert!(
            compositor
                .present(&layers, &Tiles::default(), images, serial, Instant::now())
                .is_ok()
        );
    }

    /// Clears a pool buffer and submits it as a present.
    fn draw(pool: &mut PresentPool, queue: &mut Queue, layer: LayerId) -> (AcquiredImage, usize) {
        let mut image = pool.acquire(queue).expect("acquire").expect("free buffer");
        let mut encoder = queue.encoder("clear");
        let mut colors = ArrayVec::new();
        colors.push(ColorAttachment {
            view: image.image().view(),
            load: Load::Clear([0.2, 0.4, 0.6, 1.0]),
            store: Store::Store,
            resolve: None,
        });
        drop(encoder.render_pass(&RenderPassDesc {
            colors,
            depth_stencil: None,
            area: adv_gpu::Rect {
                x: 0,
                y: 0,
                w: W,
                h: H,
            },
            timestamps: None,
            label: "clear",
        }));
        let acquire = queue
            .submit_present(encoder, &mut [&mut image])
            .expect("submit");
        let Export::IoSurface(surface, _) = image.export() else {
            unreachable!()
        };
        (
            AcquiredImage {
                layer,
                image,
                acquire,
            },
            surface.as_ptr() as usize,
        )
    }

    fn sublayers(root: &CALayer) -> Vec<Retained<CALayer>> {
        unsafe { root.sublayers() }
            .map(|layers| layers.to_vec())
            .unwrap_or_default()
    }

    fn contents(layer: &CALayer) -> usize {
        unsafe { layer.contents() }.map_or(0, |c| Retained::as_ptr(&c) as usize)
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }
}
