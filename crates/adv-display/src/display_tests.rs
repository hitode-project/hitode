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
/// The platform decoders' presentable YUV format; fixtures fill neutral
/// chroma, so plane order does not matter.
const DECODED: Format = if cfg!(target_os = "android") {
    Format::Nv21
} else {
    Format::Nv12
};
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
        view: (64, 48),
        rotation: Rotation::None,
        upscaler: Upscaler::Off,
        background: [0.; 3],
        damage_overlay: false,
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
/// Damage of the test canvas.
fn cells(rects: &[Rect]) -> Region {
    let mut region = Region::empty((64, 48));
    for &r in rects {
        region.add(r.into());
    }
    region
}
fn all() -> Region {
    Region::whole((64, 48))
}
fn none() -> Region {
    Region::empty((64, 48))
}
fn info(serial: u64, base: BaseKind, damage: Region) -> FrameInfo {
    FrameInfo {
        serial,
        canvas: (64, 48),
        base,
        damage,
        video: Default::default(),
        cursor_regions: Default::default(),
        windows: Default::default(),
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
fn frame(serial: u64, image: &Image, damage: Region) -> Frame {
    Frame {
        info: info(serial, BaseKind::Gpu { presentable: false }, damage),
        base: Base::Private(image.clone()),
        video: vec![],
        windows: vec![],
        published_at: std::time::Instant::now(),
        on_read: None,
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
                info: info(1, BaseKind::Gpu { presentable: true }, all()),
                base: Base::Presentable { image, ready },
                video: vec![],
                windows: vec![],
                published_at: std::time::Instant::now(),
                on_read: None,
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
            all()
        } else {
            cells(&[Rect {
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
    // Age two: this frame's change and the previous one, as one span.
    assert_eq!(
        tiles.rects,
        [Rect {
            x: 16,
            y: 0,
            w: 32,
            h: 16
        }]
    );
    assert_eq!(
        records[2].plan.targets[0].buffer,
        records[0].plan.targets[0].buffer
    );
    assert_eq!(display.ledger().pools[0].buffers.len(), 2);
    for serial in 5..=7 {
        display
            .present(&mut q, frame(serial, &image, none()))
            .unwrap();
    }
    let quiet = display.recorder().latest().unwrap();
    assert_eq!(quiet.statistics.passes, 0);
    assert_eq!(quiet.statistics.submissions, 0);
    // Trimming keeps one free buffer and forgets content: the next present redraws.
    display.trim();
    assert!(
        display.ledger().pools[0].buffers[1..]
            .iter()
            .all(|b| b.in_use)
    );
    display.present(&mut q, frame(8, &image, none())).unwrap();
    assert!(display.recorder().latest().unwrap().statistics.passes > 0);
    crate::recorder::replay(&crate::recorder::parse(&display.recorder().dump()).unwrap()).unwrap();
}
// spec: HOST-VIEW-1, HOST-VIEW-9
#[test]
fn rotation_and_settings_invalidate_content_and_rebuild_only_reattaches() {
    let (d, mut q, mut display, history) = setup();
    let image = private(&d, &mut q);
    display.present(&mut q, frame(1, &image, all())).unwrap();
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
    assert_eq!(record.frame.damage, all());
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
            format: DECODED,
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
    let mut info = info(1, BaseKind::Software { indexed: false }, all());
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
                windows: vec![],
                published_at: std::time::Instant::now(),
                on_read: None,
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
            info: info(1, BaseKind::Software { indexed: false }, all()),
            base: Base::Software {
                buffer,
                pitch: 256,
                extent: (64, 48),
                format: crate::stages::ingest::PixelFormat::Bgra32,
                palette: None,
            },
            video: vec![],
            windows: vec![],
            published_at: std::time::Instant::now(),
            on_read: None,
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
// spec: HOST-VIEW-11, HOST-SES-10
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
                        cells(&[Rect {
                            x,
                            y: 0,
                            w: 1,
                            h: 1,
                        }]),
                    ),
                    base: Base::Presentable { image, ready },
                    video: vec![],
                    windows: vec![],
                    published_at: std::time::Instant::now(),
                    on_read: None,
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
        cells(&[
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
    let mut frame = frame(serial, &private(d, q), all());
    let mut pool = d
        .create_present_pool(&PoolDesc {
            color: Some(adv_gpu::present::YuvColor::Bt601Full),
            width: 8,
            height: 8,
            format: DECODED,
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
        // Android decoders' external formats are sampled converted.
        planes: (!cfg!(target_os = "android")).then(|| {
            std::array::from_fn(|i| {
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
            })
        }),
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
    // A second video from its own decoder.
    let (mut other, _second_decoder) = video_frame(&d, &mut q, 1);
    let mut second = other.video.remove(0);
    second.id = 10;
    second.hide_cursor = false;
    second.geometry.destination.x = 32;
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

// spec: HOST-VIEW-3, HOST-VIEW-11
#[test]
fn coalesced_software_waits_unread_and_superseded_video_returns_to_the_decoder() {
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
    let reads = Arc::new(Mutex::new(vec![]));
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
        let reads = reads.clone();
        frame.on_read = Some(Box::new(move |token| {
            reads.lock().unwrap().push((serial, token))
        }));
        display.present(&mut q, frame).unwrap();
        decoders.push(pool);
    }
    // The first frame attached; the later ones wait unencoded, and the second,
    // replaced before the compositor latched the first, is never read.
    let serials = |reads: &[(u64, Token)]| reads.iter().map(|r| r.0).collect::<Vec<_>>();
    assert_eq!(serials(&reads.lock().unwrap()), [1]);
    assert!(decoders[0].acquire_cpu().is_none());
    assert!(decoders[1].acquire_cpu().is_some());
    assert!(decoders[2].acquire_cpu().is_none());
    latch(&events, 1);
    display.service(&mut q).unwrap();
    let read = reads.lock().unwrap().clone();
    assert_eq!(serials(&read), [1, 3]);
    assert!(
        q.wait(read[1].1, std::time::Duration::from_secs(20))
            .unwrap()
    );
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
    // A frame clock encodes the second frame on the next refresh, held until
    // the first latches.
    display.refreshed(1);
    display.present(&mut q, frame(1, &image, all())).unwrap();
    display.refreshed(2);
    display.present(&mut q, frame(2, &image, none())).unwrap();
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
    display.refreshed(3);
    latch(&events, 2);
    display.service(&mut q).unwrap();
    let record = display.recorder().latest().unwrap();
    assert_eq!(record.frame.damage, all());
    assert_eq!(record.frame.serial, 2);
    assert_eq!(record.plan.layers.base.dst.w, 128);
    assert_eq!(record.plan.layers.base.dst.h, 96);

    // A resize that latches on a later refresh still leaves only a
    // resampled copy on screen. The next real frame must replace it whole.
    display.refreshed(4);
    latch(&events, 3);
    let damage = cells(&[Rect {
        x: 0,
        y: 0,
        w: 8,
        h: 8,
    }]);
    display
        .present(&mut q, frame(3, &image, damage.clone()))
        .unwrap();
    assert_eq!(display.recorder().latest().unwrap().frame.damage, all());

    display.refreshed(5);
    latch(&events, 4);
    display
        .present(&mut q, frame(4, &image, damage.clone()))
        .unwrap();
    assert_eq!(display.recorder().latest().unwrap().frame.damage, damage);
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
    // A held frame reuses the intermediate's content without writing it.
    let (mut held, _pool) = video_frame(&d, &mut q, 6);
    held.info.damage = none();
    held.info.video[0].changed = false;
    display.present(&mut q, held).unwrap();
    latch(&events, 2);
    display.service(&mut q).unwrap();
    let record = display.recorder().latest().unwrap();
    assert_eq!(record.frame.serial, 6);
    let Stage::Video { tiles, .. } = &record.plan.stages[0] else {
        panic!("composed video")
    };
    assert!(tiles.rects.is_empty());
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
fn exact_software_upscaling_uses_producer_cells() {
    use crate::stages::upscale;

    let (d, mut q, mut display, _) = setup();
    display
        .settings(
            &mut q,
            Settings {
                view: (128, 96),
                upscaler: Upscaler::Anime4k,
                ..settings()
            },
        )
        .unwrap();
    let software = |serial, pixels: &[u8], damage| {
        let buffer = d
            .create_buffer(&BufferDesc {
                size: pixels.len() as u64,
                usage: BufferUsage::STORAGE,
                memory: Memory::Upload,
                label: "exact software source",
            })
            .unwrap();
        // SAFETY: no GPU work references the new buffer.
        unsafe { buffer.mapped().unwrap().as_mut() }.copy_from_slice(pixels);
        Frame {
            info: info(serial, BaseKind::Software { indexed: false }, damage),
            base: Base::Software {
                buffer,
                pitch: 256,
                extent: (64, 48),
                format: crate::stages::ingest::PixelFormat::Bgra32,
                palette: None,
            },
            video: vec![],
            windows: vec![],
            published_at: std::time::Instant::now(),
            on_read: None,
        }
    };
    let mut pixels: Vec<u8> = (0..64 * 48 * 4).map(|i| (i * 7 % 251) as u8).collect();
    display
        .present(&mut q, software(100, &pixels, all()))
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !display.upscaler_ready(&mut q) {
        assert!(
            std::time::Instant::now() < deadline,
            "upscaler build timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    display
        .present(&mut q, software(1, &pixels, all()))
        .unwrap();
    let changed = Rect {
        x: 24,
        y: 24,
        w: 8,
        h: 8,
    };
    for y in changed.y..changed.y + changed.h {
        for x in changed.x..changed.x + changed.w {
            let at = ((y * 64 + x) * 4) as usize;
            pixels[at..at + 3].copy_from_slice(&[255, 0, 128]);
        }
    }
    display
        .present(&mut q, software(2, &pixels, cells(&[changed])))
        .unwrap();
    let latest = display.recorder().latest().unwrap();
    let Stage::Upscale { tiles, .. } = &latest.plan.stages[0] else {
        panic!("upscaled software frame")
    };
    let model = upscale::model(&d, Upscaler::Anime4k, false);
    let upscaling = upscale::Upscaling::with_precision(&d, model, (64, 48), false).unwrap();
    let expected = upscaling.reached(&[changed]);
    assert!(expected < upscaling.reached(&tiles.rects));
    assert_eq!(latest.statistics.stages[0].pixels, expected);
    let partial = display.upscaled_copy(&mut q);
    let partial = read(&d, &mut q, &partial);
    display
        .present(&mut q, software(3, &pixels, all()))
        .unwrap();
    let whole = display.upscaled_copy(&mut q);
    assert_eq!(partial, read(&d, &mut q, &whole));
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
    let history: Arc<Mutex<Vec<Presentation>>> = Arc::default();
    // Tiles as the shell sizes them, so partial passes keep this GPU's render
    // area granularity.
    let tiled = Box::new(Capture {
        headless: HeadlessCompositor::new(CompositorCaps {
            tile: d.caps().limits.tile,
            ..caps()
        }),
        history: history.clone(),
    });
    let mut display = Display::new(d.clone(), tiled, upscaled.clone()).unwrap();
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
        display.present(q, frame(100, &image, all())).unwrap();
        assert_eq!(
            display.recorder().latest().unwrap().settings.upscaler,
            Upscaler::Off
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !display.upscaler_ready(q) {
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
            None => all(),
            Some(r) => {
                for y in r.y..r.y + r.h {
                    for x in r.x..r.x + r.w {
                        let at = ((y * 64 + x) * 4) as usize;
                        pixels[at..at + 3].copy_from_slice(&[255, serial as u8 * 60, 0]);
                    }
                }
                cells(&[r])
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
    // One tile covers the last change in the corner.
    assert!(
        matches!(&tiles.rects[..], [r] if r.x <= 60 && r.y <= 44 && r.x + r.w == 64 && r.y + r.h == 48),
        "{:?}",
        tiles.rects
    );
    assert_eq!(latest.statistics.kind, crate::recorder::PlanKind::Upscaled);
    assert!(display.totals().upscaled_runs > 0 && display.totals().upscaled_pixels > 0);
    let partial = read(&d, &mut q, &display.shown_base());
    let partial_model = display.upscaled_copy(&mut q);
    let partial_model = read(&d, &mut q, &partial_model);
    // Each frame's stage tiles, for a device failure's report.
    let plans: Vec<String> = display
        .recorder()
        .events()
        .filter_map(|e| match e {
            crate::recorder::Event::Present(r) => Some(format!(
                "{}: {:?}",
                r.frame.serial,
                r.plan
                    .stages
                    .iter()
                    .map(|s| match s {
                        Stage::Upscale { tiles, .. } => ("upscale", tiles.rects.clone()),
                        Stage::Output { tiles, .. } => ("output", tiles.rects.clone()),
                        _ => ("other", vec![]),
                    })
                    .collect::<Vec<_>>()
            )),
            _ => None,
        })
        .collect();
    let mut whole = Display::new(d.clone(), compositor(&history), upscaled).unwrap();
    ready(&mut whole, &mut q, &pixels);
    whole.present(&mut q, frame(1, &image, all())).unwrap();
    assert_eq!(whole.shown_base().desc().size.width, 128);
    let expected = read(&d, &mut q, &whole.shown_base());
    let whole_model = whole.upscaled_copy(&mut q);
    let whole_model = read(&d, &mut q, &whole_model);
    let model_differing = (0..partial_model.len() / 4)
        .filter(|&i| partial_model[i * 4..i * 4 + 4] != whole_model[i * 4..i * 4 + 4])
        .count();
    // Report where the frames differ: a device failure leaves only this line.
    let differing: Vec<_> = (0..partial.len() / 4)
        .filter(|&i| partial[i * 4..i * 4 + 4] != expected[i * 4..i * 4 + 4])
        .map(|i| (i % 128, i / 128))
        .collect();
    assert!(
        differing.is_empty(),
        "{} pixels differ, x {:?}..={:?}, y {:?}..={:?}, first {:?}: {:?} != {:?}; \
         model output differs in {} pixels, first there {:?}; tile {:?}, plans {:#?}",
        differing.len(),
        differing.iter().map(|p| p.0).min(),
        differing.iter().map(|p| p.0).max(),
        differing.iter().map(|p| p.1).min(),
        differing.iter().map(|p| p.1).max(),
        differing.first(),
        differing
            .first()
            .map(|&(x, y)| &partial[(y * 128 + x) * 4..][..4]),
        differing
            .first()
            .map(|&(x, y)| &expected[(y * 128 + x) * 4..][..4]),
        model_differing,
        differing
            .first()
            .map(|&(x, y)| &partial_model[(y * 128 + x) * 4..][..4]),
        d.caps().limits.tile,
        plans,
    );
}

// spec: HOST-VIEW-11
#[test]
fn a_frame_clock_encodes_and_attaches_at_most_once_per_refresh() {
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
    let reads = Arc::new(Mutex::new(vec![]));
    let software = |serial: u64| {
        let buffer = d
            .create_buffer(&BufferDesc {
                size: 64 * 48 * 4,
                usage: BufferUsage::STORAGE,
                memory: Memory::Upload,
                label: "paced software",
            })
            .unwrap();
        let reads = reads.clone();
        Frame {
            info: info(serial, BaseKind::Software { indexed: false }, all()),
            base: Base::Software {
                buffer,
                pitch: 256,
                extent: (64, 48),
                format: crate::stages::ingest::PixelFormat::Bgra32,
                palette: None,
            },
            video: vec![],
            windows: vec![],
            published_at: std::time::Instant::now(),
            on_read: Some(Box::new(move |_| reads.lock().unwrap().push(serial))),
        }
    };
    display.refreshed(1);
    display.present(&mut q, software(1)).unwrap();
    assert_eq!(display.totals().presents, 1);
    // Within the same refresh a frame waits unencoded, and a newer one
    // replaces it.
    display.present(&mut q, software(2)).unwrap();
    display.present(&mut q, software(3)).unwrap();
    display.service(&mut q).unwrap();
    assert_eq!(*reads.lock().unwrap(), [1]);
    // The next refresh encodes it before the first present latches; the latch
    // then attaches it.
    display.refreshed(2);
    display.service(&mut q).unwrap();
    assert_eq!(*reads.lock().unwrap(), [1, 3]);
    assert_eq!(display.totals().presents, 1);
    // While it waits to attach, a late latch makes later refreshes encode
    // nothing: a frame encoded only to be replaced is GPU work nobody sees.
    display.present(&mut q, software(4)).unwrap();
    display.refreshed(3);
    display.service(&mut q).unwrap();
    assert_eq!(*reads.lock().unwrap(), [1, 3]);
    latch(&events, 1);
    display.service(&mut q).unwrap();
    assert_eq!(display.totals().presents, 2);
    assert_eq!(display.recorder().latest().unwrap().frame.serial, 3);
    // Once it attaches, the waiting frame is encoded for the next latch.
    assert_eq!(*reads.lock().unwrap(), [1, 3, 4]);
}

// spec: HOST-VIEW-10, HOST-SES-10
#[test]
fn paced_upscaling_compares_the_newest_composition_to_the_last_encoded_input() {
    let (d, mut q) = Device::open(&queue::DeviceOptions {
        validation: !cfg!(target_os = "android"),
        disable: adv_gpu::Modern::from_test_env(),
        ..Default::default()
    })
    .unwrap();
    let settings = Settings {
        view: (128, 96),
        upscaler: Upscaler::Anime4k,
        ..settings()
    };
    let events = Arc::new(Mutex::new(vec![]));
    let mut display = Display::new(
        d.clone(),
        Box::new(Controlled {
            headless: HeadlessCompositor::new(caps()),
            history: Arc::default(),
            latch: events.clone(),
        }),
        settings.clone(),
    )
    .unwrap();
    let mut seed = 0x2545_f491u32;
    let original: Vec<u8> = (0..64 * 48 * 4)
        .map(|_| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 16) as u8
        })
        .collect();
    let mut intermediate = original.clone();
    intermediate[4..7].copy_from_slice(&[0, 255, 0]);
    let mut newest = original.clone();
    newest[(40 * 64 + 50) * 4..][..3].copy_from_slice(&[255, 0, 128]);
    let images: Vec<_> = [&original, &intermediate, &newest]
        .into_iter()
        .map(|pixels| {
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
                    label: "paced upscale source",
                })
                .unwrap();
            let upload = d
                .create_buffer(&BufferDesc {
                    size: pixels.len() as u64,
                    usage: BufferUsage::COPY_SRC,
                    memory: Memory::Upload,
                    label: "paced upscale upload",
                })
                .unwrap();
            // SAFETY: the upload is new.
            unsafe { upload.mapped().unwrap().as_mut() }.copy_from_slice(pixels);
            let mut encoder = q.encoder("paced upscale upload");
            encoder.copy_buffer_to_image(
                encode::BufferCopy {
                    buffer: &upload,
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
            image
        })
        .collect();
    let ready = |display: &mut Display, q: &mut Queue, image: &Image| {
        display.present(q, frame(100, image, all())).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !display.upscaler_ready(q) {
            assert!(
                std::time::Instant::now() < deadline,
                "upscaler build timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    ready(&mut display, &mut q, &images[0]);
    latch(&events, 1);
    display.service(&mut q).unwrap();
    display
        .present(&mut q, frame(1, &images[0], all()))
        .unwrap();
    assert_eq!(display.recorder().latest().unwrap().frame.serial, 1);
    display
        .present(
            &mut q,
            frame(
                2,
                &images[1],
                cells(&[Rect {
                    x: 1,
                    y: 0,
                    w: 1,
                    h: 1,
                }]),
            ),
        )
        .unwrap();
    // The newest publication cancels the first pixel change and changes a
    // distant pixel; both publications' cells survive in merged S.
    display
        .present(
            &mut q,
            frame(
                3,
                &images[2],
                cells(&[
                    Rect {
                        x: 1,
                        y: 0,
                        w: 1,
                        h: 1,
                    },
                    Rect {
                        x: 50,
                        y: 40,
                        w: 1,
                        h: 1,
                    },
                ]),
            ),
        )
        .unwrap();
    assert_eq!(display.recorder().latest().unwrap().frame.serial, 1);
    latch(&events, 2);
    display.service(&mut q).unwrap();
    assert_eq!(display.recorder().latest().unwrap().frame.serial, 3);
    let partial = display.upscaled_copy(&mut q);
    let partial = read(&d, &mut q, &partial);
    let mut whole = Display::new(d.clone(), compositor(&Arc::default()), settings).unwrap();
    ready(&mut whole, &mut q, &images[2]);
    whole.present(&mut q, frame(1, &images[2], all())).unwrap();
    let expected = whole.upscaled_copy(&mut q);
    assert_eq!(partial, read(&d, &mut q, &expected));
}

/// A compositor that releases each image before its specified buffer age.
struct AgeCompositor {
    age: u64,
    serial: u64,
    images: Vec<(u64, LayerId)>,
    presented: Vec<Presented>,
}
impl Compositor for AgeCompositor {
    fn caps(&self) -> CompositorCaps {
        caps()
    }
    fn detach(&mut self) -> Option<adv_gpu::queue::Fence> {
        self.images.clear();
        None
    }
    fn present(
        &mut self,
        _: &Layers,
        _: &Tiles,
        images: &mut [AcquiredImage],
        serial: u64,
        at: std::time::Instant,
    ) -> std::result::Result<(), crate::compositor::Error> {
        self.serial = serial;
        self.images.extend(images.iter().map(|a| (serial, a.layer)));
        self.presented.push(Presented {
            serial,
            latched: Some(at),
            shown: Some(at),
        });
        Ok(())
    }
    fn drain(&mut self, presented: &mut Vec<Presented>, released: &mut Vec<Released>) {
        presented.append(&mut self.presented);
        released.extend(
            self.images
                .extract_if(.., |(serial, _)| *serial + self.age <= self.serial + 1)
                .map(|(serial, layer)| Released {
                    serial,
                    layer,
                    fence: None,
                }),
        );
    }
}

// spec: HOST-VIEW-12
#[test]
fn damage_off_matches_legacy_settings_and_toggle_preserves_clean_frames_at_ages_one_to_three() {
    for age in 1..=3 {
        let (device, mut queue) = Device::open(&queue::DeviceOptions {
            validation: !cfg!(target_os = "android"),
            ..Default::default()
        })
        .unwrap();
        let mut legacy = serde_json::to_value(settings()).unwrap();
        legacy.as_object_mut().unwrap().remove("damage_overlay");
        let legacy: Settings = serde_json::from_value(legacy).unwrap();
        let compositor = || {
            Box::new(AgeCompositor {
                age,
                serial: 0,
                images: vec![],
                presented: vec![],
            }) as Box<dyn Compositor>
        };
        let mut baseline = Display::new(device.clone(), compositor(), legacy.clone()).unwrap();
        let mut explicit_off = Display::new(device.clone(), compositor(), settings()).unwrap();
        let mut toggled = Display::new(device.clone(), compositor(), settings()).unwrap();
        let source = private(&device, &mut queue);
        let mut observed_age = false;
        for serial in 1..=14 {
            let damage = if serial == 1 {
                all()
            } else {
                cells(&[Rect {
                    x: ((serial % 8) * 8) as u32,
                    y: 8,
                    w: 8,
                    h: 8,
                }])
            };
            let mut toggle = settings();
            toggle.damage_overlay = (3..=7).contains(&serial);
            toggled.settings(&mut queue, toggle).unwrap();
            baseline
                .present(&mut queue, frame(serial, &source, damage.clone()))
                .unwrap();
            explicit_off
                .present(&mut queue, frame(serial, &source, damage.clone()))
                .unwrap();
            toggled
                .present(&mut queue, frame(serial, &source, damage))
                .unwrap();
            let expected = baseline.recorder().latest().unwrap();
            let actual = explicit_off.recorder().latest().unwrap();
            assert_eq!(actual.plan, expected.plan);
            assert_eq!(actual.statistics.stages, expected.statistics.stages);
            if let Some(target) = expected.plan.targets.first() {
                observed_age |= expected
                    .pools
                    .pools
                    .iter()
                    .find(|p| p.id == target.pool)
                    .and_then(|p| p.buffers.get(target.buffer))
                    .and_then(|b| b.age)
                    == Some(age as u32);
            }
            let pixels = read(&device, &mut queue, &baseline.shown_base());
            assert_eq!(
                read(&device, &mut queue, &explicit_off.shown_base()),
                pixels
            );
            assert_eq!(read(&device, &mut queue, &toggled.shown_base()), pixels);
            assert_eq!(
                toggled.recorder().latest().unwrap().statistics.stages,
                expected.statistics.stages
            );
            if serial >= 10 {
                assert_eq!(toggled.recorder().latest().unwrap().plan, expected.plan);
            }
        }
        assert!(observed_age, "buffer age {age} was not exercised");
    }
}

// spec: HOST-VIEW-12
#[test]
fn damage_toggle_keeps_separate_video_and_rebuilt_captures_clean() {
    let (device, mut queue, mut diagnostic, history) = setup();
    let baseline_history = Arc::default();
    let mut baseline =
        Display::new(device.clone(), compositor(&baseline_history), settings()).unwrap();
    let source = private(&device, &mut queue);
    let mut pool = device
        .create_present_pool(&PoolDesc {
            color: Some(adv_gpu::present::YuvColor::Bt601Full),
            width: 8,
            height: 8,
            format: DECODED,
            count: 2,
            storage: false,
            label: "damage video fixture",
        })
        .unwrap();
    let mut pictures = [pool.acquire_cpu().unwrap(), pool.acquire_cpu().unwrap()];
    for picture in &mut pictures {
        picture.write(|planes| {
            planes[0].data.fill(80);
            planes[1].data.fill(128);
        });
    }
    let raw = [pictures[0].image().clone(), pictures[1].image().clone()];
    let mut initial = pictures.map(Some);
    let rect = Rect {
        x: 16,
        y: 16,
        w: 16,
        h: 16,
    };
    for serial in 1..=12 {
        let mut current = settings();
        current.damage_overlay = matches!(serial, 2..=4 | 6..=7);
        diagnostic.settings(&mut queue, current).unwrap();
        for (index, display) in [&mut baseline, &mut diagnostic].into_iter().enumerate() {
            let mut frame = frame(serial, &source, if serial == 1 { all() } else { none() });
            if serial <= 10 {
                frame.info.video.push(VideoGeometry {
                    source_uv: None,
                    background: None,
                    id: 9,
                    source: (8, 8),
                    rect,
                    changed: serial == 1,
                });
                frame.video.push(Video {
                    id: 9,
                    image: raw[index].clone(),
                    planes: (DECODED == Format::Nv12).then(|| {
                        std::array::from_fn(|plane| {
                            raw[index]
                                .create_view(&ViewDesc {
                                    format: Some([Format::R8Unorm, Format::Rg8Unorm][plane]),
                                    dimension: None,
                                    mips: 0..1,
                                    layers: 0..1,
                                    plane: plane as u32,
                                })
                                .unwrap()
                        })
                    }),
                    hide_cursor: false,
                    geometry: crate::stages::video::Geometry {
                        destination: rect,
                        source_uv: [0., 0., 1., 1.],
                        background: None,
                        color: adv_gpu::present::YuvColor::Bt601Full,
                    },
                    update: initial[index].take(),
                });
            }
            display.present(&mut queue, frame).unwrap();
        }
        if serial == 3 {
            baseline
                .rebuild(compositor(&baseline_history), &mut queue)
                .unwrap();
            diagnostic
                .rebuild(compositor(&history), &mut queue)
                .unwrap();
        }
        let capture = |display: &mut Display, queue: &mut Queue| {
            let (send, done) = std::sync::mpsc::channel();
            display.capture(queue, Box::new(move |result| send.send(result).unwrap()));
            done.recv_timeout(std::time::Duration::from_secs(20))
                .unwrap()
                .unwrap()
                .bgra
        };
        assert_eq!(
            capture(&mut diagnostic, &mut queue),
            capture(&mut baseline, &mut queue)
        );
        assert_eq!(
            diagnostic.recorder().latest().unwrap().statistics.stages,
            baseline.recorder().latest().unwrap().statistics.stages
        );
        let presentation = history.lock().unwrap().last().unwrap().clone();
        let expected = baseline_history.lock().unwrap().last().unwrap().clone();
        assert_eq!(presentation.layers.base, expected.layers.base);
        assert_eq!(presentation.layers.video, expected.layers.video);
        assert_eq!(presentation.layers.solids, expected.layers.solids);
        if matches!(serial, 2..=4 | 6..=7) {
            assert_eq!(presentation.layers.video.len(), 1);
            assert!(presentation.layers.host.contains(&HostLayer::Damage));
        }
    }
}

// spec: WINE-GFX-10
#[test]
fn window_layers_upload_once_capture_over_the_frame_and_leave() {
    let (d, mut q, mut display, history) = setup();
    let image = private(&d, &mut q);
    let red = [0u8, 0, 255, 0].repeat(8);
    let windowed = |serial, version| {
        let mut f = frame(serial, &image, if serial == 1 { all() } else { none() });
        f.info.windows.push(WindowGeometry {
            id: 3,
            at: (10, 20),
            size: (4, 2),
            changed: false,
        });
        f.windows.push(crate::windows::Window {
            id: 3,
            size: (4, 2),
            pixels: red.clone().into(),
            layered: false,
            version,
        });
        f
    };
    let shown = |display: &mut Display, q: &mut Queue, f| {
        let token = display.present(q, f).unwrap();
        assert!(q.wait(token, std::time::Duration::from_secs(20)).unwrap());
        display.service(q).unwrap();
        history.lock().unwrap().last().cloned().unwrap()
    };
    let first = shown(&mut display, &mut q, windowed(1, 7));
    let [(3, layer)] = &first.layers.windows[..] else {
        panic!("one window layer")
    };
    assert_eq!(
        layer.dst,
        Rect {
            x: 10,
            y: 20,
            w: 4,
            h: 2
        }
    );
    assert!(first.acquires.iter().any(|(l, _)| *l == LayerId::Window(3)));
    // The same pixels again keep the attached image.
    let second = shown(&mut display, &mut q, windowed(2, 7));
    assert!(
        !second
            .acquires
            .iter()
            .any(|(l, _)| *l == LayerId::Window(3))
    );
    assert!(second.layers.windows[0].1.damage.is_empty());
    let publication = display.take_publication().unwrap();
    assert_eq!(publication.windows, vec![(3, layer.dst, (4, 2))]);
    // A capture shows the window where the guest has it, opaque.
    let (send, done) = std::sync::mpsc::channel();
    display.capture(&mut q, Box::new(move |r| send.send(r).unwrap()));
    let captured = done
        .recv_timeout(std::time::Duration::from_secs(20))
        .unwrap()
        .unwrap();
    let at = |x: usize, y: usize| &captured.bgra[(y * 64 + x) * 4..][..4];
    assert_eq!(at(10, 20), &[0, 0, 255, 255]);
    assert_eq!(at(13, 21), &[0, 0, 255, 255]);
    assert_eq!(at(14, 21), &[191, 128, 64, 255]);
    // Closed, its layer goes.
    let third = shown(&mut display, &mut q, frame(3, &image, none()));
    assert!(third.layers.windows.is_empty());
}

// spec: HOST-VIEW-7, HOST-VIEW-10
#[test]
fn hd_video_bypasses_upscaling_and_resumes_clean_software_damage() {
    let (d, mut q, mut display, history) = setup();
    display
        .settings(
            &mut q,
            Settings {
                view: (160, 120),
                upscaler: Upscaler::Anime4k,
                ..settings()
            },
        )
        .unwrap();
    let software = |serial, pixels: &[u8], damage, video: Vec<(VideoGeometry, Video)>| {
        let buffer = d
            .create_buffer(&BufferDesc {
                size: pixels.len() as u64,
                usage: BufferUsage::STORAGE,
                memory: Memory::Upload,
                label: "software canvas",
            })
            .unwrap();
        unsafe { buffer.mapped().unwrap().as_mut() }.copy_from_slice(pixels);
        let mut info = info(serial, BaseKind::Software { indexed: false }, damage);
        let mut videos = Vec::new();
        for (geom, layer) in video {
            info.video.push(geom);
            videos.push(layer);
        }
        Frame {
            info,
            base: Base::Software {
                buffer,
                pitch: 256,
                extent: (64, 48),
                format: crate::stages::ingest::PixelFormat::Bgra32,
                palette: None,
            },
            video: videos,
            windows: vec![],
            published_at: std::time::Instant::now(),
            on_read: None,
        }
    };
    let mut pixels: Vec<u8> = (0..64 * 48 * 4).map(|i| (i * 11 % 251) as u8).collect();
    display
        .present(&mut q, software(100, &pixels, all(), vec![]))
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !display.upscaler_ready(&mut q) {
        assert!(
            std::time::Instant::now() < deadline,
            "upscaler build timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    display
        .present(&mut q, software(1, &pixels, all(), vec![]))
        .unwrap();
    let runs_before = display.totals().upscaled_runs;

    let mut pool = d
        .create_present_pool(&PoolDesc {
            color: Some(adv_gpu::present::YuvColor::Bt709Limited),
            width: 1280,
            height: 720,
            format: DECODED,
            count: 2,
            storage: false,
            label: "hd video",
        })
        .unwrap();
    let mut vimg = pool.acquire_cpu().unwrap();
    vimg.write(|p| {
        p[0].data.fill(96);
        p[1].data.fill(128);
    });
    let raw = vimg.image().clone();
    // Mutate part of the software base while a 720p video bypasses upscaling.
    let mid_change = Rect {
        x: 8,
        y: 8,
        w: 8,
        h: 8,
    };
    for y in mid_change.y..mid_change.y + mid_change.h {
        for x in mid_change.x..mid_change.x + mid_change.w {
            let at = ((y * 64 + x) * 4) as usize;
            pixels[at..at + 3].copy_from_slice(&[20, 220, 40]);
        }
    }
    let hd_geom = VideoGeometry {
        source_uv: None,
        background: None,
        id: 11,
        source: (1280, 720),
        rect: Rect {
            x: 0,
            y: 0,
            w: 64,
            h: 48,
        },
        changed: true,
    };
    let hd_video = Video {
        hide_cursor: false,
        id: 11,
        image: raw,
        planes: None,
        geometry: crate::stages::video::Geometry {
            destination: hd_geom.rect,
            source_uv: [0., 0., 1., 1.],
            background: None,
            color: adv_gpu::present::YuvColor::Bt709Limited,
        },
        update: Some(vimg),
    };
    display
        .present(
            &mut q,
            software(2, &pixels, cells(&[mid_change]), vec![(hd_geom, hd_video)]),
        )
        .unwrap();
    assert_eq!(display.totals().upscaled_runs, runs_before);
    assert_eq!(
        history.lock().unwrap().last().unwrap().layers.video.len(),
        1
    );

    // Video ends with a second small change elsewhere; both changes must appear.
    let end_change = Rect {
        x: 40,
        y: 24,
        w: 8,
        h: 8,
    };
    for y in end_change.y..end_change.y + end_change.h {
        for x in end_change.x..end_change.x + end_change.w {
            let at = ((y * 64 + x) * 4) as usize;
            pixels[at..at + 3].copy_from_slice(&[240, 30, 180]);
        }
    }
    display
        .present(&mut q, software(3, &pixels, cells(&[end_change]), vec![]))
        .unwrap();
    assert_eq!(display.totals().upscaled_runs, runs_before + 1);
    let resumed_image = display.upscaled_copy(&mut q);
    let resumed = read(&d, &mut q, &resumed_image);
    display
        .present(&mut q, software(4, &pixels, all(), vec![]))
        .unwrap();
    let whole_image = display.upscaled_copy(&mut q);
    let whole = read(&d, &mut q, &whole_image);
    assert_eq!(resumed, whole);
}
