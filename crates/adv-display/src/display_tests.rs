use super::display::*;
use super::{Rect, compositor::*, plan::*};
use adv_gpu::{
    encode::{ColorAttachment, Load, RenderPassDesc, Store},
    present::PoolDesc,
    resource::*,
    *,
};
use std::sync::{Arc, Mutex};

struct Capture {
    headless: HeadlessCompositor,
    history: Arc<Mutex<Vec<Presentation>>>,
}
impl Compositor for Capture {
    fn detach(&mut self) -> Option<adv_gpu::queue::Fence> {
        self.headless.detach()
    }
    fn caps(&self) -> CompositorCaps {
        self.headless.caps()
    }
    fn present(
        &mut self,
        l: &Layers,
        d: &Tiles,
        i: &mut [AcquiredImage],
        s: u64,
        a: std::time::Instant,
    ) -> std::result::Result<(), crate::compositor::Error> {
        self.history.lock().unwrap().push(Presentation {
            layers: l.clone(),
            damage: d.clone(),
            serial: s,
            at: a,
            acquires: i.iter().map(|i| (i.layer, i.acquire)).collect(),
        });
        self.headless.present(l, d, i, s, a)
    }
    fn drain(&mut self, p: &mut Vec<Presented>, r: &mut Vec<Released>) {
        self.headless.drain(p, r);
    }
}
fn caps() -> CompositorCaps {
    CompositorCaps {
        fractional_crops: !cfg!(target_os = "android"),
        scales: true,
        nearest: true,
        rotates: false,
        yuv_layers: true,
        solid_color_layers: true,
        damage_hints: true,
        presentable_compressed: false,
        tile: (16, 16),
    }
}
fn settings() -> Settings {
    Settings {
        placement: crate::plan::Placement::Fit,
        scale: 1.,
        rate: 60,
        view: (64, 48),
        rotation: Rotation::None,
        upscaler: Upscaler::Off,
        background: [0.; 3],
    }
}
fn compositor(history: &Arc<Mutex<Vec<Presentation>>>) -> Box<dyn Compositor + Send> {
    Box::new(Capture {
        headless: HeadlessCompositor::new(caps()),
        history: history.clone(),
    })
}
fn setup() -> (Device, Queue, Display, Arc<Mutex<Vec<Presentation>>>) {
    let (d, q) = Device::open(&queue::DeviceOptions {
        validation: !cfg!(target_os = "android"),
        ..Default::default()
    })
    .unwrap();
    let history = Arc::default();
    let display = Display::new(d.clone(), compositor(&history), settings()).unwrap();
    (d, q, display, history)
}
fn info(serial: u64, base: BaseKind, damage: Damage) -> FrameInfo {
    FrameInfo {
        serial,
        canvas: (64, 48),
        base,
        damage,
        video: Default::default(),
        cursor_regions: Default::default(),
    }
}
fn private(d: &Device, q: &mut Queue) -> Image {
    let image = d
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: 64,
                height: 48,
                depth_or_layers: 1,
            },
            format: Format::Bgra8Unorm,
            mips: 1,
            samples: 1,
            usage: Usage::COLOR | Usage::SAMPLED,
            view_formats: &[],
            label: "executor source",
        })
        .unwrap();
    let mut encoder = q.encoder("source");
    {
        let _pass = encoder.render_pass(&RenderPassDesc {
            colors: [ColorAttachment {
                view: image.view(),
                load: Load::Clear([0.25, 0.5, 0.75, 1.]),
                store: Store::Store,
                resolve: None,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            area: adv_gpu::Rect {
                x: 0,
                y: 0,
                w: 64,
                h: 48,
            },
            timestamps: None,
            label: "source",
        });
    }
    q.submit(encoder).unwrap();
    image
}
fn frame(serial: u64, image: &Image, damage: Damage) -> Frame {
    Frame {
        info: info(serial, BaseKind::Gpu { presentable: false }, damage),
        base: Base::Private(image.clone()),
        video: vec![],
        published_at: std::time::Instant::now(),
    }
}
// spec: HOST-VIEW-11
#[test]
fn presentable_swap_is_shown_by_reference_without_a_stage() {
    let (d, mut q, mut display, history) = setup();
    let mut pool = d
        .create_present_pool(&PoolDesc {
            color: None,
            width: 64,
            height: 48,
            format: adv_gpu::present::COLOR_FORMAT,
            count: 2,
            storage: false,
            label: "direct swap",
        })
        .unwrap();
    let mut image = pool.acquire(&mut q).unwrap().unwrap();
    let encoder = q.encoder("swap");
    let ready = q.submit_present(encoder, &mut [&mut image]).unwrap();
    display
        .present(
            &mut q,
            Frame {
                info: info(1, BaseKind::Gpu { presentable: true }, Damage::Whole),
                base: Base::Presentable { image, ready },
                video: vec![],
                published_at: std::time::Instant::now(),
            },
        )
        .unwrap();
    assert_eq!(display.recorder().latest().unwrap().statistics.passes, 0);
    assert_eq!(
        display.recorder().latest().unwrap().statistics.submissions,
        0
    );
    assert_eq!(
        history.lock().unwrap()[0].acquires,
        [(LayerId::Base, ready)]
    );
    assert_eq!(history.lock().unwrap()[0].layers.background, [0.; 3]);
}
// spec: HOST-VIEW-1, HOST-VIEW-3, HOST-VIEW-9
#[test]
fn private_swap_limits_output_to_damage_and_reconstructs_buffer_age() {
    let (d, mut q, mut display, _) = setup();
    let image = private(&d, &mut q);
    for (serial, x) in [(1, 0), (2, 16), (3, 32), (4, 48)] {
        let damage = if serial == 1 {
            Damage::Whole
        } else {
            Damage::rects([Rect {
                x,
                y: 0,
                w: 1,
                h: 1,
            }])
        };
        display
            .present(&mut q, frame(serial, &image, damage))
            .unwrap();
    }
    let records = display
        .recorder()
        .events()
        .filter_map(|e| {
            if let crate::recorder::Event::Present(r) = e {
                Some(r)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let Stage::Output { tiles, .. } = &records[2].plan.stages[0] else {
        panic!("private output")
    };
    assert_eq!(
        tiles.rects,
        [
            Rect {
                x: 16,
                y: 0,
                w: 16,
                h: 16
            },
            Rect {
                x: 32,
                y: 0,
                w: 16,
                h: 16
            }
        ]
    );
    assert_eq!(
        records[2].plan.targets[0].buffer,
        records[0].plan.targets[0].buffer
    );
    assert_eq!(display.ledger().pools[0].buffers.len(), 2);
    for serial in 5..=7 {
        display
            .present(&mut q, frame(serial, &image, Damage::None))
            .unwrap();
    }
    let quiet = display.recorder().latest().unwrap();
    assert_eq!(quiet.statistics.passes, 0);
    assert_eq!(quiet.statistics.submissions, 0);
    crate::recorder::replay(&crate::recorder::parse(&display.recorder().dump()).unwrap()).unwrap();
}
// spec: HOST-VIEW-1, HOST-VIEW-9
#[test]
fn rotation_and_settings_invalidate_content_and_rebuild_only_reattaches() {
    let (d, mut q, mut display, history) = setup();
    let image = private(&d, &mut q);
    display
        .present(&mut q, frame(1, &image, Damage::Whole))
        .unwrap();
    let mut changed = settings();
    changed.rotation = Rotation::Quarter;
    changed.background = [0.1, 0.2, 0.3];
    display.settings(&mut q, changed).unwrap();

    let record = display
        .recorder()
        .events()
        .filter_map(|e| {
            if let crate::recorder::Event::Present(r) = e {
                Some(r)
            } else {
                None
            }
        })
        .last()
        .unwrap();
    assert_eq!(record.frame.damage, Damage::Whole);
    let Stage::Output {
        transform, tiles, ..
    } = &record.plan.stages[0]
    else {
        panic!("output")
    };
    assert_eq!(transform.rotation, Rotation::Quarter);
    assert_eq!(
        tiles.rects,
        [Rect {
            x: 0,
            y: 0,
            w: 48,
            h: 64
        }]
    );
    let stages = display.recorder().latest().unwrap().statistics.passes;
    display.rebuild(compositor(&history), &mut q).unwrap();
    assert_eq!(history.lock().unwrap().len(), 3);
    assert_eq!(
        display
            .recorder()
            .events()
            .filter(|e| matches!(e, crate::recorder::Event::Present(_)))
            .count(),
        2
    );
    assert_eq!(
        display.recorder().latest().unwrap().statistics.passes,
        stages
    );
    let history = history.lock().unwrap();
    assert_eq!(history[1].layers, history[2].layers);
    assert_eq!(history[2].acquires, history[1].acquires);
}
// spec: HOST-VIEW-3
#[test]
fn software_ingest_and_video_layers_share_one_plan() {
    let (d, mut q, mut display, history) = setup();
    let buffer = d
        .create_buffer(&BufferDesc {
            size: 64 * 48 * 4,
            usage: BufferUsage::STORAGE,
            memory: Memory::Upload,
            label: "software source",
        })
        .unwrap();
    unsafe { buffer.mapped().unwrap().as_mut() }.fill(255);
    let mut pool = d
        .create_present_pool(&PoolDesc {
            color: Some(adv_gpu::present::YuvColor::Bt601Full),
            width: 8,
            height: 8,
            format: Format::Nv12,
            count: 2,
            storage: false,
            label: "video",
        })
        .unwrap();
    let mut image = pool.acquire_cpu().unwrap();
    image.write(|p| {
        p[0].data.fill(80);
        p[1].data.fill(128);
    });
    let mut info = info(1, BaseKind::Software { indexed: false }, Damage::Whole);
    info.video.push(VideoGeometry {
        source_uv: None,
        background: None,
        id: 9,
        source: (8, 8),
        rect: Rect {
            x: 16,
            y: 16,
            w: 16,
            h: 16,
        },
        changed: true,
    });
    let raw = image.image().clone();
    let video = Video {
        hide_cursor: false,
        id: 9,
        image: raw,
        planes: None,
        geometry: crate::stages::video::Geometry {
            destination: Rect {
                x: 16,
                y: 16,
                w: 16,
                h: 16,
            },
            source_uv: [0., 0., 1., 1.],
            background: None,
            color: adv_gpu::present::YuvColor::Bt601Full,
        },
        update: Some(image),
    };
    display
        .present(
            &mut q,
            Frame {
                info,
                base: Base::Software {
                    buffer,
                    pitch: 256,
                    extent: (64, 48),
                    format: crate::stages::ingest::PixelFormat::Bgra32,
                    palette: None,
                },
                video: vec![video],
                published_at: std::time::Instant::now(),
            },
        )
        .unwrap();
    assert_eq!(display.recorder().latest().unwrap().statistics.passes, 1);
    let history = history.lock().unwrap();
    assert_eq!(history[0].layers.video.len(), 1);
    assert_eq!(history[0].acquires.len(), 2);
}

// spec: HOST-VIEW-3, HOST-VIEW-11
#[test]
fn failed_attachment_keeps_the_ledger_uncommitted_and_reports_the_submitted_read() {
    struct Reject;
    impl Compositor for Reject {
        fn detach(&mut self) -> Option<adv_gpu::queue::Fence> {
            None
        }
        fn caps(&self) -> CompositorCaps {
            caps()
        }
        fn present(
            &mut self,
            _: &Layers,
            _: &Tiles,
            _: &mut [AcquiredImage],
            _: u64,
            _: std::time::Instant,
        ) -> std::result::Result<(), crate::compositor::Error> {
            Err(crate::compositor::Error::SurfaceLost)
        }
        fn drain(&mut self, _: &mut Vec<Presented>, _: &mut Vec<Released>) {}
    }
    let (d, mut q) = Device::open(&queue::DeviceOptions {
        validation: !cfg!(target_os = "android"),
        ..Default::default()
    })
    .unwrap();
    let mut display = Display::new(d.clone(), Box::new(Reject), settings()).unwrap();
    let buffer = d
        .create_buffer(&BufferDesc {
            size: 64 * 48 * 4,
            usage: BufferUsage::STORAGE,
            memory: Memory::Upload,
            label: "failed ingest",
        })
        .unwrap();
    unsafe { buffer.mapped().unwrap().as_mut() }.fill(255);
    let result = display.present(
        &mut q,
        Frame {
            info: info(1, BaseKind::Software { indexed: false }, Damage::Whole),
            base: Base::Software {
                buffer,
                pitch: 256,
                extent: (64, 48),
                format: crate::stages::ingest::PixelFormat::Bgra32,
                palette: None,
            },
            video: vec![],
            published_at: std::time::Instant::now(),
        },
    );
    let failure = result.unwrap_err();
    assert!(matches!(
        failure.error,
        super::display::Error::Compositor(crate::compositor::Error::SurfaceLost)
    ));
    assert!(display.ledger().history.is_empty());
    assert!(display.recorder().latest().is_none());
    assert_ne!(failure.token, Token(0));
    assert!(
        q.wait(failure.token, std::time::Duration::from_secs(20))
            .unwrap()
    );
}

type ImageHistory = Arc<Mutex<Vec<Vec<(LayerId, u64)>>>>;
struct Controlled {
    headless: HeadlessCompositor,
    history: ImageHistory,
    latch: Arc<Mutex<Vec<Presented>>>,
}
impl Compositor for Controlled {
    fn caps(&self) -> CompositorCaps {
        self.headless.caps()
    }
    fn detach(&mut self) -> Option<adv_gpu::queue::Fence> {
        self.headless.detach()
    }
    fn present(
        &mut self,
        l: &Layers,
        d: &Tiles,
        i: &mut [AcquiredImage],
        s: u64,
        a: std::time::Instant,
    ) -> std::result::Result<(), crate::compositor::Error> {
        self.history
            .lock()
            .unwrap()
            .push(i.iter().map(|a| (a.layer, a.image.image().id())).collect());
        self.headless.present(l, d, i, s, a)
    }
    fn drain(&mut self, p: &mut Vec<Presented>, r: &mut Vec<Released>) {
        self.headless.drain(&mut vec![], r);
        p.append(&mut self.latch.lock().unwrap());
    }
}
fn latch(events: &Arc<Mutex<Vec<Presented>>>, serial: u64) {
    events.lock().unwrap().push(Presented {
        serial,
        latched: Some(std::time::Instant::now()),
        shown: None,
    });
}
// spec: HOST-VIEW-11, HOST-SES-11
#[test]
fn coalescing_replaces_the_held_frame_merges_damage_and_releases_its_swap() {
    let (d, mut q) = Device::open(&queue::DeviceOptions::default()).unwrap();
    let history = Arc::new(Mutex::new(vec![]));
    let events = Arc::new(Mutex::new(vec![]));
    let mut display = Display::new(
        d.clone(),
        Box::new(Controlled {
            headless: HeadlessCompositor::new(caps()),
            history: history.clone(),
            latch: events.clone(),
        }),
        settings(),
    )
    .unwrap();
    let mut pool = d
        .create_present_pool(&PoolDesc {
            color: None,
            width: 64,
            height: 48,
            format: adv_gpu::present::COLOR_FORMAT,
            count: 3,
            storage: false,
            label: "coalesced swaps",
        })
        .unwrap();
    let mut superseded = 0;
    for (serial, x) in [(1, 0), (2, 16), (3, 32)] {
        let mut image = pool.acquire(&mut q).unwrap().unwrap();
        if serial == 2 {
            superseded = image.index();
        }
        let encoder = q.encoder("swap ready");
        let ready = q.submit_present(encoder, &mut [&mut image]).unwrap();
        display
            .present(
                &mut q,
                Frame {
                    info: info(
                        serial,
                        BaseKind::Gpu { presentable: true },
                        Damage::rects([Rect {
                            x,
                            y: 0,
                            w: 1,
                            h: 1,
                        }]),
                    ),
                    base: Base::Presentable { image, ready },
                    video: vec![],
                    published_at: std::time::Instant::now(),
                },
            )
            .unwrap();
    }
    assert_eq!(history.lock().unwrap().len(), 1);
    assert_eq!(display.recorder().latest().unwrap().frame.serial, 1);
    assert!(
        pool.acquire_index(&mut q, superseded, true)
            .unwrap()
            .is_some()
    );
    latch(&events, 1);
    display.service(&mut q).unwrap();
    let record = display.recorder().latest().unwrap();
    assert_eq!(record.frame.serial, 3);
    assert_eq!(
        record.frame.damage,
        Damage::rects([
            Rect {
                x: 32,
                y: 0,
                w: 1,
                h: 1
            },
            Rect {
                x: 16,
                y: 0,
                w: 1,
                h: 1
            }
        ])
    );
    assert_eq!(history.lock().unwrap().len(), 2);
    assert_eq!(record.statistics.passes, 0);
}
fn video_frame(d: &Device, q: &mut Queue, serial: u64) -> (Frame, PresentPool) {
    let mut frame = frame(serial, &private(d, q), Damage::Whole);
    let mut pool = d
        .create_present_pool(&PoolDesc {
            color: Some(adv_gpu::present::YuvColor::Bt601Full),
            width: 8,
            height: 8,
            format: Format::Nv12,
            count: 1,
            storage: false,
            label: "decoder",
        })
        .unwrap();
    let mut image = pool.acquire_cpu().unwrap();
    image.write(|p| {
        p[0].data.fill(80);
        p[1].data.fill(128);
    });
    let dst = Rect {
        x: 16,
        y: 16,
        w: 16,
        h: 16,
    };
    let bg = Some((
        Rect {
            x: 8,
            y: 8,
            w: 32,
            h: 32,
        },
        [1, 2, 3, 0],
    ));
    frame.info.video.push(VideoGeometry {
        id: 9,
        source: (8, 8),
        rect: dst,
        source_uv: Some([0.125, 0.25, 0.5, 0.75]),
        background: bg,
        changed: true,
    });
    frame.video.push(Video {
        id: 9,
        image: image.image().clone(),
        planes: Some(std::array::from_fn(|i| {
            image
                .image()
                .create_view(&ViewDesc {
                    format: Some([Format::R8Unorm, Format::Rg8Unorm][i]),
                    dimension: None,
                    mips: 0..1,
                    layers: 0..1,
                    plane: i as u32,
                })
                .unwrap()
        })),
        update: Some(image),
        hide_cursor: true,
        geometry: crate::stages::video::Geometry {
            destination: dst,
            source_uv: [0.125, 0.25, 0.5, 0.75],
            background: bg,
            color: adv_gpu::present::YuvColor::Bt601Full,
        },
    });
    (frame, pool)
}
// spec: HOST-VIEW-3, HOST-VIEW-11, AND-APP-10
#[test]
fn detach_rebuild_keeps_direct_executor_and_video_leases_without_encoding() {
    for direct in [true, false] {
        let (d, mut q) = Device::open(&queue::DeviceOptions::default()).unwrap();
        let history = Arc::new(Mutex::new(vec![]));
        let events = Arc::new(Mutex::new(vec![]));
        let make = || {
            Box::new(Controlled {
                headless: HeadlessCompositor::new(caps()),
                history: history.clone(),
                latch: events.clone(),
            }) as Box<dyn Compositor>
        };
        let mut display = Display::new(d.clone(), make(), settings()).unwrap();
        let (mut frame, mut decoder) = video_frame(&d, &mut q, 1);
        let mut pool = d
            .create_present_pool(&PoolDesc {
                color: None,
                width: 64,
                height: 48,
                format: adv_gpu::present::COLOR_FORMAT,
                count: 1,
                storage: false,
                label: "direct",
            })
            .unwrap();
        if direct {
            let mut image = pool.acquire(&mut q).unwrap().unwrap();
            let encoder = q.encoder("ready");
            let ready = q.submit_present(encoder, &mut [&mut image]).unwrap();
            frame.info.base = BaseKind::Gpu { presentable: true };
            frame.base = Base::Presentable { image, ready };
        }
        display.present(&mut q, frame).unwrap();
        assert!(display.take_publication().is_none());
        latch(&events, 1);
        display.service(&mut q).unwrap();
        let publication = display.take_publication().unwrap();
        assert_eq!(
            publication.cursor_regions,
            [(
                Rect {
                    x: 8,
                    y: 8,
                    w: 32,
                    h: 32
                },
                true
            )]
        );
        let before = display.recorder().dump();
        display.suspend();
        assert!(decoder.acquire_cpu().is_none());
        if direct {
            assert!(pool.acquire(&mut q).unwrap().is_none());
        }
        display.rebuild(make(), &mut q).unwrap();
        assert!(decoder.acquire_cpu().is_none());
        let h = history.lock().unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0], h[1]);
        assert_eq!(h[0].len(), 2);
        // Rebuild adds lifecycle events, never another present/stage record.
        let records = crate::recorder::parse(&before).unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|e| matches!(e, crate::recorder::Event::Present(_)))
                .count(),
            display
                .recorder()
                .events()
                .filter(|e| matches!(e, crate::recorder::Event::Present(_)))
                .count()
        );
        let r = display.recorder().latest().unwrap();
        assert_eq!(r.statistics.passes, u32::from(!direct));
        assert_eq!(
            r.plan.layers.video[0].1.source_uv,
            Some([0.125, 0.25, 0.5, 0.75])
        );
        assert_eq!(
            r.plan.layers.solids[0].2,
            [3. / 255., 2. / 255., 1. / 255., 1.]
        );
    }
}
// spec: HOST-VIEW-3, HOST-VIEW-9
#[test]
fn composition_uses_one_pass_for_base_borders_and_all_videos_and_records_gpu_time() {
    let (d, mut q) = Device::open(&queue::DeviceOptions::default()).unwrap();
    let mut c = caps();
    c.yuv_layers = false;
    c.solid_color_layers = false;
    let mut display =
        Display::new(d.clone(), Box::new(HeadlessCompositor::new(c)), settings()).unwrap();
    let (mut frame, _decoder) = video_frame(&d, &mut q, 1);
    let first = &frame.video[0];
    let planes = std::array::from_fn(|i| {
        first
            .image
            .create_view(&ViewDesc {
                format: Some(if i == 0 {
                    Format::R8Unorm
                } else {
                    Format::Rg8Unorm
                }),
                plane: i as u32,
                dimension: None,
                mips: 0..1,
                layers: 0..1,
            })
            .unwrap()
    });
    let mut second = Video {
        id: 10,
        image: first.image.clone(),
        planes: Some(planes),
        geometry: first.geometry,
        hide_cursor: false,
        update: None,
    };
    second.geometry.destination.x = 32;
    // Both draws can sample one decoder lease, with distinct video identities.
    let geometry = frame.info.video[0].clone();
    frame.info.video.push(VideoGeometry {
        id: 10,
        rect: Rect {
            x: 32,
            y: 16,
            w: 16,
            h: 16,
        },
        ..geometry
    });
    frame.video.push(second);
    let token = display.present(&mut q, frame).unwrap();
    assert!(q.wait(token, std::time::Duration::from_secs(20)).unwrap());
    display.service(&mut q).unwrap();
    let r = display.recorder().latest().unwrap();
    assert_eq!(r.statistics.passes, 1);
    assert_eq!(r.statistics.submissions, 1);
    assert_eq!(r.plan.stages.len(), 1);
    if d.caps().features.contains(Features::TIMESTAMPS) {
        assert!(r.stage_times[0] > std::time::Duration::ZERO);
    }
    let (send, done) = std::sync::mpsc::channel();
    display.capture(&mut q, Box::new(move |r| send.send(r).unwrap()));
    let captured = done
        .recv_timeout(std::time::Duration::from_secs(20))
        .unwrap()
        .unwrap();
    assert_eq!(
        (captured.serial, captured.width, captured.height),
        (1, 64, 48)
    );
    assert!(captured.bgra.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    assert_eq!(&captured.bgra[0..4], &[191, 128, 64, 255]);
}

// spec: HOST-VIEW-3, HOST-VIEW-11, HOST-SES-11
#[test]
fn coalesced_software_reports_its_read_and_superseded_video_returns_to_the_decoder() {
    let (d, mut q) = Device::open(&queue::DeviceOptions::default()).unwrap();
    let events = Arc::new(Mutex::new(vec![]));
    let mut display = Display::new(
        d.clone(),
        Box::new(Controlled {
            headless: HeadlessCompositor::new(caps()),
            history: Arc::default(),
            latch: events.clone(),
        }),
        Settings {
            view: (160, 120),
            ..settings()
        },
    )
    .unwrap();
    let mut decoders = vec![];
    let mut tokens = vec![];
    for serial in 1..=3 {
        let (mut frame, pool) = video_frame(&d, &mut q, serial);
        let buffer = d
            .create_buffer(&BufferDesc {
                size: 64 * 48 * 4,
                usage: BufferUsage::STORAGE,
                memory: Memory::Upload,
                label: "coalesced software",
            })
            .unwrap();
        unsafe { buffer.mapped().unwrap().as_mut() }.fill(255);
        frame.info.base = BaseKind::Software { indexed: false };
        frame.base = Base::Software {
            buffer,
            pitch: 256,
            extent: (64, 48),
            format: crate::stages::ingest::PixelFormat::Bgra32,
            palette: None,
        };
        let token = display.present(&mut q, frame).unwrap();
        assert_ne!(token, Token(0));
        tokens.push(token);
        decoders.push(pool);
    }
    let (send, done) = std::sync::mpsc::channel();
    q.on_complete(tokens[2], Box::new(move || send.send(()).unwrap()));
    done.recv_timeout(std::time::Duration::from_secs(20))
        .unwrap();
    assert!(
        q.wait(tokens[1], std::time::Duration::from_secs(20))
            .unwrap()
    );
    assert!(decoders[0].acquire_cpu().is_none());
    assert!(decoders[1].acquire_cpu().is_some());
    assert!(decoders[2].acquire_cpu().is_none());
    latch(&events, 1);
    display.service(&mut q).unwrap();
    display.service(&mut q).unwrap();
    assert_eq!(display.recorder().latest().unwrap().frame.serial, 3);
    assert!(decoders[0].acquire_cpu().is_some());
    crate::recorder::replay(&crate::recorder::parse(&display.recorder().dump()).unwrap()).unwrap();
}
// spec: HOST-VIEW-1, HOST-VIEW-9, HOST-VIEW-11
#[test]
fn changing_settings_while_a_frame_is_held_never_aliases_its_read_source() {
    let (d, mut q) = Device::open(&queue::DeviceOptions::default()).unwrap();
    let events = Arc::new(Mutex::new(vec![]));
    let mut display = Display::new(
        d.clone(),
        Box::new(Controlled {
            headless: HeadlessCompositor::new(caps()),
            history: Arc::default(),
            latch: events.clone(),
        }),
        settings(),
    )
    .unwrap();
    let image = private(&d, &mut q);
    display
        .present(&mut q, frame(1, &image, Damage::Whole))
        .unwrap();
    display
        .present(&mut q, frame(2, &image, Damage::None))
        .unwrap();
    let mut changed = settings();
    changed.rotation = Rotation::Half;
    display.settings(&mut q, changed).unwrap();
    // The redraw planned around the shown image and its read source.
    let buffers = &display.ledger().pools[0].buffers;
    assert_eq!(buffers.len(), 2);
    assert!(buffers.iter().all(|b| b.in_use));
    latch(&events, 1);
    display.service(&mut q).unwrap();
    assert_eq!(display.recorder().latest().unwrap().frame.serial, 2);
    crate::recorder::replay(&crate::recorder::parse(&display.recorder().dump()).unwrap()).unwrap();
    let mut changed = settings();
    changed.view = (128, 96);
    display.settings(&mut q, changed).unwrap();
    latch(&events, 2);
    display.service(&mut q).unwrap();
    let record = display.recorder().latest().unwrap();
    assert_eq!(record.frame.damage, Damage::Whole);
    crate::recorder::replay(&crate::recorder::parse(&display.recorder().dump()).unwrap()).unwrap();
}

// spec: HOST-VIEW-3, HOST-VIEW-11
#[test]
fn held_frames_reuse_intermediate_targets_without_reserving_their_slots() {
    let (d, mut q) = Device::open(&queue::DeviceOptions::default()).unwrap();
    let events = Arc::new(Mutex::new(vec![]));
    let mut c = caps();
    c.yuv_layers = false;
    let mut display = Display::new(
        d.clone(),
        Box::new(Controlled {
            headless: HeadlessCompositor::new(c),
            history: Arc::default(),
            latch: events.clone(),
        }),
        Settings {
            view: (160, 120),
            ..settings()
        },
    )
    .unwrap();
    for serial in 1..=5 {
        let (frame, _pool) = video_frame(&d, &mut q, serial);
        display.present(&mut q, frame).unwrap();
    }
    let intermediate = display
        .ledger()
        .pools
        .iter()
        .find(|p| p.id == PoolId(1))
        .unwrap();
    assert_eq!(intermediate.buffers.len(), 1);
    assert!(!intermediate.buffers[0].in_use);
    latch(&events, 1);
    display.service(&mut q).unwrap();
    let record = display.recorder().latest().unwrap();
    assert_eq!(record.frame.serial, 5);
    assert_eq!(record.statistics.passes, 2);
    assert_eq!(record.plan.targets.len(), 2);
    crate::recorder::replay(&crate::recorder::parse(&display.recorder().dump()).unwrap()).unwrap();
}

fn read(d: &Device, q: &mut Queue, image: &Image) -> Vec<u8> {
    let size = image.desc().size;
    let buffer = d
        .create_buffer(&BufferDesc {
            size: u64::from(size.width * size.height * 4),
            usage: BufferUsage::COPY_DST,
            memory: Memory::Readback,
            label: "shown readback",
        })
        .unwrap();
    let mut encoder = q.encoder("shown readback");
    encoder.copy_image_to_buffer(
        encode::ImageCopy {
            image,
            mip: 0,
            layer: 0,
            origin: (0, 0, 0),
        },
        encode::BufferCopy {
            buffer: &buffer,
            offset: 0,
            row_pitch: 0,
            rows: 0,
        },
        size,
    );
    let token = q.submit(encoder).unwrap();
    assert!(q.wait(token, std::time::Duration::from_secs(10)).unwrap());
    // SAFETY: the copy completed.
    unsafe { buffer.mapped().unwrap().as_ref() }.to_vec()
}

// spec: HOST-VIEW-10
#[test]
fn upscaled_partial_frames_match_a_whole_run_across_buffer_ages() {
    let (d, mut q) = Device::open(&queue::DeviceOptions {
        validation: !cfg!(target_os = "android"),
        disable: adv_gpu::Modern::from_test_env(),
        ..Default::default()
    })
    .unwrap();
    let upscaled = Settings {
        view: (128, 96),
        upscaler: Upscaler::Anime4k,
        ..settings()
    };
    let history = Arc::default();
    let mut display = Display::new(d.clone(), compositor(&history), upscaled.clone()).unwrap();
    let image = d
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: 64,
                height: 48,
                depth_or_layers: 1,
            },
            format: Format::Bgra8Unorm,
            mips: 1,
            samples: 1,
            usage: Usage::SAMPLED | Usage::COPY_DST,
            view_formats: &[],
            label: "changing source",
        })
        .unwrap();
    let upload = |q: &mut Queue, pixels: &[u8]| {
        let buffer = d
            .create_buffer(&BufferDesc {
                size: pixels.len() as u64,
                usage: BufferUsage::COPY_SRC,
                memory: Memory::Upload,
                label: "source upload",
            })
            .unwrap();
        // SAFETY: the buffer is new.
        unsafe { buffer.mapped().unwrap().as_mut() }.copy_from_slice(pixels);
        let mut encoder = q.encoder("source upload");
        encoder.copy_buffer_to_image(
            encode::BufferCopy {
                buffer: &buffer,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            encode::ImageCopy {
                image: &image,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            image.desc().size,
        );
        q.submit(encoder).unwrap();
    };
    let mut pixels: Vec<u8> = (0..64 * 48 * 4).map(|i| (i * 7 % 251) as u8).collect();
    // The model builds off the owner thread; frames show unscaled meanwhile.
    let ready = |display: &mut Display, q: &mut Queue, pixels: &[u8]| {
        upload(q, pixels);
        display
            .present(q, frame(100, &image, Damage::Whole))
            .unwrap();
        assert_eq!(
            display.recorder().latest().unwrap().settings.upscaler,
            Upscaler::Off
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !display.upscaler_ready() {
            assert!(
                std::time::Instant::now() < deadline,
                "upscaler build timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    ready(&mut display, &mut q, &pixels);
    let rect = |x, y, w, h| Rect { x, y, w, h };
    // Changes at edges and apart, each frame into the older of two buffers.
    let changes = [
        None,
        Some(rect(5, 7, 3, 2)),
        Some(rect(40, 30, 6, 6)),
        Some(rect(0, 0, 1, 1)),
        Some(rect(60, 44, 4, 4)),
    ];
    for (serial, change) in changes.into_iter().enumerate() {
        let damage = match change {
            None => Damage::Whole,
            Some(r) => {
                for y in r.y..r.y + r.h {
                    for x in r.x..r.x + r.w {
                        let at = ((y * 64 + x) * 4) as usize;
                        pixels[at..at + 3].copy_from_slice(&[255, serial as u8 * 60, 0]);
                    }
                }
                Damage::rects([r])
            }
        };
        upload(&mut q, &pixels);
        display
            .present(&mut q, frame(serial as u64 + 1, &image, damage))
            .unwrap();
    }
    let latest = display.recorder().latest().unwrap();
    let Stage::Upscale { tiles, .. } = &latest.plan.stages[0] else {
        panic!("upscaled frame")
    };
    assert_eq!(tiles.rects, [rect(48, 32, 16, 16)]);
    let partial = read(&d, &mut q, &display.shown_base());
    let mut whole = Display::new(d.clone(), compositor(&history), upscaled).unwrap();
    ready(&mut whole, &mut q, &pixels);
    whole
        .present(&mut q, frame(1, &image, Damage::Whole))
        .unwrap();
    assert_eq!(whole.shown_base().desc().size.width, 128);
    assert!(partial == read(&d, &mut q, &whole.shown_base()));
}
