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
    fn caps(&self) -> CompositorCaps {
        self.headless.caps()
    }
    fn present(
        &mut self,
        l: &Layers,
        d: &Tiles,
        i: Vec<AcquiredImage>,
        s: u64,
        a: std::time::Instant,
    ) -> std::result::Result<(), PresentError> {
        self.history.lock().unwrap().push(Presentation {
            layers: l.clone(),
            damage: d.clone(),
            serial: s,
            at: a,
            acquires: i.iter().map(|i| (i.layer, i.acquire)).collect(),
        });
        self.headless.present(l, d, i, s, a)
    }
    fn drain(&mut self, p: &mut Vec<Presented>, r: &mut Vec<ReleasedImage>) {
        self.headless.drain(p, r);
    }
}
fn caps() -> CompositorCaps {
    CompositorCaps {
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
        validation: true,
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
        at: std::time::Instant::now(),
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
            format: Format::Bgra8Unorm,
            count: 2,
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
                at: std::time::Instant::now(),
            },
        )
        .unwrap();
    assert_eq!(display.statistics().back().unwrap().passes, 0);
    assert_eq!(display.statistics().back().unwrap().submissions, 0);
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
    display.settings(changed);
    display
        .present(&mut q, frame(2, &image, Damage::None))
        .unwrap();
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
    let stages = display.statistics().back().unwrap().passes;
    display.rebuild(compositor(&history), &mut q).unwrap();
    assert_eq!(history.lock().unwrap().len(), 3);
    assert_eq!(display.statistics().len(), 2);
    assert_eq!(display.statistics().back().unwrap().passes, stages);
    let history = history.lock().unwrap();
    assert_eq!(history[1].layers, history[2].layers);
    assert_eq!(history[2].acquires, [(LayerId::Base, Token(0))]);
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
                at: std::time::Instant::now(),
            },
        )
        .unwrap();
    assert_eq!(display.statistics().back().unwrap().passes, 1);
    let history = history.lock().unwrap();
    assert_eq!(history[0].layers.video.len(), 1);
    assert_eq!(history[0].acquires.len(), 2);
}

// spec: HOST-VIEW-3, HOST-VIEW-11
#[test]
fn failed_attachment_keeps_the_ledger_uncommitted_and_reports_the_submitted_read() {
    struct Reject;
    impl Compositor for Reject {
        fn caps(&self) -> CompositorCaps {
            caps()
        }
        fn present(
            &mut self,
            _: &Layers,
            _: &Tiles,
            images: Vec<AcquiredImage>,
            _: u64,
            _: std::time::Instant,
        ) -> std::result::Result<(), PresentError> {
            Err(PresentError {
                error: crate::compositor::Error::SurfaceLost,
                images,
            })
        }
        fn drain(&mut self, _: &mut Vec<Presented>, _: &mut Vec<ReleasedImage>) {}
    }
    let (d, mut q) = Device::open(&queue::DeviceOptions {
        validation: true,
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
            at: std::time::Instant::now(),
        },
    );
    assert!(matches!(
        result,
        Err(super::display::Error::Compositor(
            crate::compositor::Error::SurfaceLost
        ))
    ));
    assert!(display.ledger().history.is_empty());
    assert!(display.statistics().is_empty());
    assert_ne!(display.last_read(), Token(0));
    assert!(
        q.wait(display.last_read(), std::time::Duration::from_secs(20))
            .unwrap()
    );
}
