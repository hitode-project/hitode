use crate::{Rect, compositor::*, plan::*, recorder::*};
use std::time::{Duration, Instant};

fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect { x, y, w, h }
}
/// Damage of the test canvas.
fn cells(rects: &[Rect]) -> Region {
    let mut region = Region::empty((320, 240));
    for &r in rects {
        region.add(r.into());
    }
    region
}
fn all() -> Region {
    Region::whole((320, 240))
}
fn none() -> Region {
    Region::empty((320, 240))
}
fn frame() -> FrameInfo {
    FrameInfo {
        serial: 50,
        canvas: (320, 240),
        base: BaseKind::Gpu { presentable: true },
        damage: all(),
        video: Default::default(),
        cursor_regions: Default::default(),
        windows: Default::default(),
    }
}
fn settings() -> Settings {
    Settings {
        placement: crate::plan::Placement::Fit,
        scale: 1.,
        view: (640, 480),
        rotation: Rotation::None,
        upscaler: Upscaler::Off,
        background: [0.0; 3],
        damage_overlay: false,
    }
}
fn caps() -> CompositorCaps {
    CompositorCaps {
        fractional_crops: !cfg!(target_os = "android"),
        scales: true,
        nearest: true,
        rotates: true,
        yuv_layers: true,
        solid_color_layers: true,
        damage_hints: true,
        presentable_compressed: false,
        tile: (0, 0),
    }
}
fn run(f: &FrameInfo, s: &Settings, c: &CompositorCaps, p: &PoolState) -> Plan {
    plan(f, s, c, p)
}
fn warm(pool: u8, size: (u32, u32), age: Option<u32>) -> PoolState {
    PoolState {
        pools: vec![Pool {
            id: PoolId(pool),
            size,
            buffers: vec![PoolBuffer { age, in_use: false }],
        }],
        ..Default::default()
    }
}
/// The ledger after one whole frame under these settings.
fn warmed(s: &Settings, c: &CompositorCaps) -> PoolState {
    let mut pools = PoolState::default();
    pools.after_present(&run(&frame(), s, c, &pools));
    pools
}
fn stage_tiles(stage: &Stage) -> Vec<&Tiles> {
    match stage {
        Stage::Ingest { tiles, .. }
        | Stage::Video { tiles, .. }
        | Stage::Upscale { tiles, .. }
        | Stage::Output { tiles, .. } => vec![tiles],
    }
}

// spec: HOST-VIEW-1, HOST-VIEW-9
#[test]
fn scaling_and_viewports() {
    for (view, crop, stages, nearest) in [
        ((320, 240), (320, 240), 0, true),
        ((640, 480), (320, 240), 0, true),
        ((960, 720), (320, 240), 0, true),
        ((800, 600), (640, 480), 1, false),
        ((480, 360), (320, 240), 0, false), // Prescale one.
        ((160, 120), (320, 240), 0, false),
        ((641, 480), (320, 240), 0, true), // One-pixel letterbox.
    ] {
        let p = run(
            &frame(),
            &Settings { view, ..settings() },
            &caps(),
            &PoolState::default(),
        );
        assert_eq!(p.stages.len(), stages, "{view:?}");
        assert_eq!(p.layers.base.crop, rect(0, 0, crop.0, crop.1));
        assert_eq!(p.layers.base.nearest, nearest);
        if let Some(Stage::Output { scale, from, .. }) = p.stages.first() {
            // The pass prescales; the compositor's layer scales linearly.
            assert_eq!(*scale, Scale::Integer { x: 2, y: 2 });
            assert_eq!(*from, Source::Base);
        }
    }
    let p = run(
        &frame(),
        &Settings {
            view: (1001, 800),
            background: [0.1, 0.2, 0.3],
            ..settings()
        },
        &caps(),
        &PoolState::default(),
    );
    assert_eq!(p.layers.base.dst, rect(0, 25, 1001, 750));
    assert_eq!(p.layers.background, [0.1, 0.2, 0.3]);
    assert_eq!(p.targets[0].size, (960, 720)); // Never includes bars.
}

// spec: HOST-VIEW-1, HOST-VIEW-9
#[test]
fn rotations_and_compositor_limits() {
    for rotation in [
        Rotation::None,
        Rotation::Quarter,
        Rotation::Half,
        Rotation::ThreeQuarters,
    ] {
        for rotates in [false, true] {
            for scales in [false, true] {
                let s = Settings {
                    view: if matches!(rotation, Rotation::Quarter | Rotation::ThreeQuarters) {
                        (480, 640)
                    } else {
                        (640, 480)
                    },
                    rotation,
                    ..settings()
                };
                let p = run(
                    &frame(),
                    &s,
                    &CompositorCaps {
                        rotates,
                        scales,
                        ..caps()
                    },
                    &PoolState::default(),
                );
                let needs_output = !scales || (!rotates && rotation != Rotation::None);
                assert_eq!(
                    p.stages.len(),
                    usize::from(needs_output),
                    "{rotation:?} {rotates} {scales}"
                );
                assert_eq!(p.layers.base.dst, rect(0, 0, s.view.0, s.view.1));
                assert_eq!(
                    p.layers.base.rotation,
                    if rotates { rotation } else { Rotation::None }
                );
                if let Some(Stage::Output {
                    transform, scale, ..
                }) = p.stages.first()
                {
                    assert_eq!(
                        transform.rotation,
                        if rotates { Rotation::None } else { rotation }
                    );
                    // A rotation-only pass is 1:1; the layer scales.
                    let factor = if scales { 1 } else { 2 };
                    assert_eq!(
                        *scale,
                        Scale::Integer {
                            x: factor,
                            y: factor
                        }
                    );
                }
            }
        }
    }
    let s = Settings {
        view: (800, 600),
        ..settings()
    };
    let p = run(
        &frame(),
        &s,
        &CompositorCaps {
            fractional_crops: !cfg!(target_os = "android"),
            scales: false,
            ..caps()
        },
        &PoolState::default(),
    );
    assert_eq!(p.targets[0].size, s.view);
    assert_eq!(p.stages.len(), 1);
    // Without compositor scaling the one pass is the whole sharp-bilinear scale.
    let Some(Stage::Output { scale, .. }) = p.stages.first() else {
        panic!("output pass expected");
    };
    assert_eq!(*scale, Scale::SharpBilinear { prescale: (2, 2) });
    // A 1:1 image needs no scaling capability.
    let p = run(
        &frame(),
        &Settings {
            view: (320, 240),
            ..settings()
        },
        &CompositorCaps {
            fractional_crops: !cfg!(target_os = "android"),
            scales: false,
            ..caps()
        },
        &PoolState::default(),
    );
    assert!(p.stages.is_empty());
}

// spec: HOST-VIEW-9
#[test]
fn linear_only_compositors_scale_integers_in_the_output_pass() {
    let linear = CompositorCaps {
        fractional_crops: !cfg!(target_os = "android"),
        nearest: false,
        ..caps()
    };
    // 2x: nearest needs the pass, at the whole viewport.
    let s = Settings {
        view: (640, 480),
        ..settings()
    };
    let p = run(&frame(), &s, &linear, &PoolState::default());
    assert_eq!(p.targets[0].size, (640, 480));
    let Some(Stage::Output { scale, .. }) = p.stages.first() else {
        panic!("output pass expected");
    };
    assert_eq!(*scale, Scale::Integer { x: 2, y: 2 });
    // 2.5x: the integer part as with nearest layers, the rest linear.
    let s = Settings {
        view: (800, 600),
        ..settings()
    };
    assert_eq!(
        run(&frame(), &s, &linear, &PoolState::default()).stages,
        run(&frame(), &s, &caps(), &PoolState::default()).stages
    );
    // 1:1 needs no pass.
    let s = Settings {
        view: (320, 240),
        ..settings()
    };
    assert!(
        run(&frame(), &s, &linear, &PoolState::default())
            .stages
            .is_empty()
    );
}

// spec: HOST-VIEW-9, HOST-VIEW-11
#[test]
fn bases_and_ingest_merging() {
    for base in [
        BaseKind::Gpu { presentable: true },
        BaseKind::Gpu { presentable: false },
        BaseKind::Software { indexed: false },
        BaseKind::Software { indexed: true },
    ] {
        for (view, upscaler, count) in [
            ((640, 480), Upscaler::Off, 0),
            ((800, 600), Upscaler::Off, 1),
            ((640, 480), Upscaler::Cunny, 2),
        ] {
            let software = matches!(base, BaseKind::Software { .. });
            let p = run(
                &FrameInfo { base, ..frame() },
                &Settings {
                    view,
                    upscaler,
                    ..settings()
                },
                &caps(),
                &PoolState::default(),
            );
            assert_eq!(
                p.stages.len(),
                if count == 0 {
                    usize::from(software || matches!(base, BaseKind::Gpu { presentable: false }))
                } else {
                    count
                }
            );
            match p.stages.first() {
                Some(Stage::Ingest { .. }) => assert!(software && count == 0),
                Some(Stage::Output { from, .. }) => assert_eq!(*from, Source::Base),
                Some(Stage::Upscale { video, .. }) => assert!(!video),
                None => assert_eq!(p.layers.base.source, Source::Base),
                other => panic!("unexpected {other:?}"),
            }
        }
    }
}

// spec: HOST-VIEW-3, HOST-VIEW-7
#[test]
fn video_layers_or_canvas_composition() {
    for (upscaler, source, yuv_layers, cursor, layered) in [
        (Upscaler::Off, (160, 120), true, None, true),
        (
            Upscaler::Off,
            (160, 120),
            true,
            Some(rect(5, 5, 2, 2)),
            true,
        ),
        (
            Upscaler::Off,
            (160, 120),
            true,
            Some(rect(15, 25, 2, 2)),
            false,
        ),
        (Upscaler::Off, (160, 120), false, None, false),
        (Upscaler::Anime4k, (160, 120), true, None, false),
        (Upscaler::Anime4k, (1280, 720), true, None, true),
        (Upscaler::Anime4k, (1920, 1080), true, None, true),
        (Upscaler::Anime4k, (1280, 720), false, None, false),
    ] {
        let mut f = frame();
        f.damage = none();
        f.video.push(VideoGeometry {
            source_uv: None,
            background: None,
            id: 7,
            source,
            rect: rect(10, 20, 100, 80),
            changed: true,
        });
        if let Some(cursor) = cursor {
            f.cursor_regions.push(cursor);
        }
        let p = run(
            &f,
            &Settings {
                upscaler,
                ..settings()
            },
            &CompositorCaps {
                yuv_layers,
                ..caps()
            },
            &PoolState::default(),
        );
        assert_eq!(p.layers.video.len(), usize::from(layered));
        let hd = u64::from(source.0) * u64::from(source.1) >= 1280 * 720;
        if layered {
            let (id, layer) = &p.layers.video[0];
            assert_eq!(*id, 7);
            assert_eq!(layer.source, Source::Video(7));
            assert_eq!(layer.crop, rect(0, 0, source.0, source.1));
            assert_eq!(layer.dst, rect(20, 40, 200, 160));
            assert_eq!(layer.damage, Region::whole(source));
            assert!(p.stages.is_empty());
        } else if upscaler == Upscaler::Off || hd {
            assert!(matches!(
                p.stages[0],
                Stage::Video {
                    from: Source::Base,
                    ..
                }
            ));
            assert_eq!(p.targets[0].size, f.canvas);
        } else {
            // Video composes straight into the model's input.
            assert!(matches!(p.stages[0], Stage::Upscale { video: true, .. }));
            assert_eq!(p.targets[0].size, f.canvas);
            assert_eq!(p.damage, cells(&[rect(10, 20, 100, 80)]));
        }
    }
    let mut f = frame();
    f.base = BaseKind::Software { indexed: true };
    f.video.push(VideoGeometry {
        source_uv: None,
        background: None,
        id: 1,
        source: (10, 10),
        rect: rect(0, 0, 10, 10),
        changed: false,
    });
    let p = run(
        &f,
        &settings(),
        &CompositorCaps {
            fractional_crops: !cfg!(target_os = "android"),
            yuv_layers: false,
            ..caps()
        },
        &PoolState::default(),
    );
    assert_eq!(p.stages.len(), 1); // Decode merges into canvas composition.
    assert!(matches!(p.stages[0], Stage::Video { .. }));
}

// spec: WINE-GFX-10
#[test]
fn window_layers_keep_the_host_scale_over_any_viewport() {
    let window = |id, at, size, changed| WindowGeometry {
        id,
        at,
        size,
        changed,
    };
    let mut f = frame();
    // A dialog centered on the 320 × 240 canvas, a small one at its corner.
    f.windows.push(window(5, (110, 90), (100, 60), true));
    f.windows.push(window(6, (-4, 0), (20, 10), false));
    let mut s = settings();
    s.scale = 2.;
    for (view, viewport) in [
        ((640, 480), rect(0, 0, 640, 480)),
        ((1000, 480), rect(180, 0, 640, 480)),
    ] {
        s.view = view;
        let p = run(&f, &s, &caps(), &PoolState::default());
        assert_eq!(p.layers.base.dst, viewport);
        let [(5, dialog), (6, corner)] = &p.layers.windows[..] else {
            panic!("two window layers")
        };
        // Twice its pixels, centered where the game shows its center.
        assert_eq!(dialog.dst, rect(viewport.x + 220, 180, 200, 120));
        assert_eq!(
            (dialog.crop, dialog.source),
            (rect(0, 0, 100, 60), Source::Window(5))
        );
        assert!(dialog.nearest && dialog.damage.is_whole());
        // Kept inside the view, its damage empty while unchanged.
        assert_eq!(corner.dst, rect(viewport.x.saturating_sub(8), 0, 40, 20));
        assert!(corner.damage.is_empty());
    }
    // Fractional host scales round to whole window pixels; a view too small
    // for that scale takes the largest that fits.
    s.view = (640, 480);
    s.scale = 2.6;
    let p = run(&f, &s, &caps(), &PoolState::default());
    assert_eq!(
        (p.layers.windows[0].1.dst.w, p.layers.windows[0].1.dst.h),
        (300, 180)
    );
    s.view = (250, 160);
    let p = run(&f, &s, &caps(), &PoolState::default());
    assert_eq!(
        (p.layers.windows[0].1.dst.w, p.layers.windows[0].1.dst.h),
        (200, 120)
    );
    // Windows never change the base plan.
    let plain = run(&frame(), &s, &caps(), &PoolState::default());
    assert_eq!((p.stages, p.layers.base), (plain.stages, plain.layers.base));
}

// spec: HOST-VIEW-7, HOST-VIEW-9
#[test]
fn upscaled_output_sizes_and_final_rotation() {
    for upscaler in [
        Upscaler::Anime4k,
        Upscaler::Cunny,
        Upscaler::Artcnn,
        Upscaler::K7,
    ] {
        // The layer magnifies the doubled image when the viewport holds it.
        for (view, expected, rotation, scales, magnified) in [
            ((960, 720), (640, 480), Rotation::None, true, true),
            ((480, 360), (480, 360), Rotation::None, true, false),
            ((720, 960), (480, 640), Rotation::Quarter, true, true),
            ((360, 480), (360, 480), Rotation::ThreeQuarters, true, false),
            ((960, 720), (960, 720), Rotation::Half, false, false),
        ] {
            let p = run(
                &frame(),
                &Settings {
                    view,
                    rotation,
                    upscaler,
                    ..settings()
                },
                &CompositorCaps {
                    fractional_crops: !cfg!(target_os = "android"),
                    rotates: false,
                    scales,
                    ..caps()
                },
                &PoolState::default(),
            );
            assert_eq!(p.targets[0].size, (320, 240));
            assert_eq!(p.targets[1].size, expected);
            assert_eq!(p.stages.len(), 2);
            assert!(matches!(
                p.stages[0],
                Stage::Upscale {
                    video: false,
                    into: UPSCALE,
                    ..
                }
            ));
            let Stage::Output {
                from,
                transform,
                scale,
                ..
            } = &p.stages[1]
            else {
                unreachable!()
            };
            assert_eq!(*from, Source::Pool(UPSCALE));
            assert_eq!(
                *scale,
                if magnified {
                    Scale::Integer { x: 1, y: 1 }
                } else {
                    Scale::SharpBilinear { prescale: (1, 1) }
                }
            );
            assert_eq!(transform.rotation, rotation);
            assert_eq!(transform.dst, rect(0, 0, expected.0, expected.1));
            assert_eq!(p.layers.base.rotation, Rotation::None);
            assert!(!p.layers.base.nearest);
        }
    }
    for view in [(320, 240), (160, 120)] {
        let p = run(
            &frame(),
            &Settings {
                view,
                upscaler: Upscaler::Cunny,
                ..settings()
            },
            &caps(),
            &PoolState::default(),
        );
        assert!(p.stages.is_empty());
    }
}

// spec: HOST-VIEW-10
#[test]
fn damage_dilation_mapping_alignment_and_merging() {
    let mut f = FrameInfo {
        damage: cells(&[rect(15, 15, 2, 2), rect(18, 18, 2, 2)]),
        ..frame()
    };
    f.base = BaseKind::Software { indexed: false };
    let p = run(&f, &settings(), &caps(), &warm(0, f.canvas, Some(1)));
    assert_eq!(stage_tiles(&p.stages[0])[0].rects, [rect(0, 0, 32, 32)]);
    assert_eq!(p.layers.base.damage, cells(&[rect(0, 0, 32, 32)]));
    for model in [
        Upscaler::Anime4k,
        Upscaler::Cunny,
        Upscaler::Artcnn,
        Upscaler::K7,
    ] {
        let radius = crate::stages::upscale::reach(model);
        let f = FrameInfo {
            damage: cells(&[rect(40, 50, 1, 1)]),
            ..frame()
        };
        let s = Settings {
            upscaler: model,
            ..settings()
        };
        let p = run(&f, &s, &caps(), &warmed(&s, &caps()));
        // The model recomputes from the change's cell (40, 48, 8, 8); the
        // output pass covers its reach.
        assert_eq!(stage_tiles(&p.stages[0])[0].rects, [rect(32, 48, 16, 16)]);
        let x = (40 - radius) * 2 / 16 * 16;
        let y = (48 - radius) * 2 / 16 * 16;
        let right = ((48 + radius) * 2).div_ceil(16) * 16;
        let bottom = ((56 + radius) * 2).div_ceil(16) * 16;
        assert_eq!(
            stage_tiles(&p.stages[1])[0].rects,
            [rect(x, y, right - x, bottom - y)]
        );
    }
    let p = run(
        &f,
        &settings(),
        &CompositorCaps {
            tile: (24, 32),
            ..caps()
        },
        &warm(0, f.canvas, Some(1)),
    );
    assert_eq!(stage_tiles(&p.stages[0])[0].size, (48, 32));
    assert_eq!(stage_tiles(&p.stages[0])[0].rects, [rect(0, 0, 48, 32)]);
    for damage in [all(), none()] {
        let p = run(
            &FrameInfo {
                damage: damage.clone(),
                ..f.clone()
            },
            &settings(),
            &caps(),
            &warm(0, f.canvas, Some(1)),
        );
        let expected = if damage.is_whole() {
            vec![rect(0, 0, 320, 240)]
        } else {
            vec![]
        };
        assert_eq!(stage_tiles(&p.stages[0])[0].rects, expected);
    }
    // Rounding clips to target edges, including after a clockwise rotation.
    let f = FrameInfo {
        damage: cells(&[rect(319, 239, 1, 1)]),
        ..frame()
    };
    let s = Settings {
        view: (480, 640),
        rotation: Rotation::Quarter,
        upscaler: Upscaler::Cunny,
        ..settings()
    };
    let c = CompositorCaps {
        rotates: false,
        ..caps()
    };
    let p = run(&f, &s, &c, &warmed(&s, &c));
    // The corner cell (312, 232, 8, 8) and its reach end at the target edges.
    assert_eq!(stage_tiles(&p.stages[1])[0].rects, [rect(0, 608, 32, 32)]);
    // Distant changes stay apart instead of merging into their bounds.
    let d: Vec<_> = (0..5).map(|n| rect(n * 20, 0, 1, 1)).collect();
    assert_eq!(cells(&d).rects().len(), 5);
}

// spec: HOST-VIEW-10
#[test]
fn buffer_age_table() {
    let f = FrameInfo {
        base: BaseKind::Software { indexed: false },
        damage: cells(&[rect(1, 1, 1, 1)]),
        ..frame()
    };
    for (age, count, expected) in [
        (Some(1), 0, vec![rect(0, 0, 16, 16)]),
        (Some(2), 1, vec![rect(0, 0, 16, 16), rect(64, 64, 16, 16)]),
        (Some(3), 1, vec![rect(0, 0, 320, 240)]),
        (None, 0, vec![rect(0, 0, 320, 240)]),
        (Some(0), 0, vec![rect(0, 0, 320, 240)]),
    ] {
        let mut pools = warm(0, f.canvas, age);
        for _ in 0..count {
            pools.history.push_back(cells(&[rect(65, 65, 1, 1)]));
        }
        let p = run(&f, &settings(), &caps(), &pools);
        assert_eq!(stage_tiles(&p.stages[0])[0].rects, expected, "{age:?}");
    }
}

// spec: HOST-VIEW-10, HOST-VIEW-11
#[test]
fn pool_present_release_invalidation_and_trim() {
    let f = FrameInfo {
        base: BaseKind::Software { indexed: false },
        ..frame()
    };
    let mut pools = PoolState::default();
    let p = run(&f, &settings(), &caps(), &pools);
    pools.after_present(&p);
    assert_eq!(pools.pools[0].buffers[0].age, Some(1));
    pools.occupy([p.targets[0]]);
    assert!(pools.pools[0].buffers[0].in_use);
    let next = run(&f, &settings(), &caps(), &pools);
    assert_eq!(next.targets[0].buffer, 1);
    pools.after_present(&next);
    assert_eq!(pools.pools[0].buffers[0].age, Some(2));
    pools.occupy([next.targets[0]]);
    assert!(!pools.pools[0].buffers[0].in_use);
    let next = run(&f, &settings(), &caps(), &pools);
    assert_eq!(next.targets[0].buffer, 0);
    pools.occupy([]);
    for _ in 0..HISTORY + 2 {
        pools.after_present(&next);
    }
    assert_eq!(pools.history.len(), HISTORY);
    pools.trim_to_one();
    assert_eq!(pools.pools[0].buffers.len(), 1);
    let quiet = FrameInfo {
        damage: none(),
        ..f.clone()
    };
    let p = run(&quiet, &settings(), &caps(), &pools);
    assert_eq!(stage_tiles(&p.stages[0])[0].rects, [rect(0, 0, 320, 240)]);
    for (view, rotation, upscaler) in [
        ((640, 480), Rotation::None, Upscaler::Off),
        ((800, 600), Rotation::None, Upscaler::Off),
        ((480, 640), Rotation::Quarter, Upscaler::Off),
        ((640, 480), Rotation::None, Upscaler::Anime4k),
    ] {
        pools.invalidate(); // Same entry point for setting changes and device loss.
        assert!(pools.history.is_empty());
        let p = run(
            &quiet,
            &Settings {
                view,
                rotation,
                upscaler,
                ..settings()
            },
            &caps(),
            &pools,
        );
        for stage in &p.stages {
            assert!(!stage_tiles(stage).is_empty());
            assert!(stage_tiles(stage).iter().all(|t| !t.rects.is_empty()));
        }
    }
    let mut pools = warm(0, (1, 1), Some(1)); // Size mismatch alone is undefined.
    let p = run(&quiet, &settings(), &caps(), &pools);
    assert_eq!(stage_tiles(&p.stages[0])[0].rects, [rect(0, 0, 320, 240)]);
    pools.after_present(&p);
    assert_eq!(pools.pools[0].size, quiet.canvas);
}

#[test]
fn swap_role_depends_only_on_settings_caps_and_canvas() {
    for (view, upscaler, compressed, expected) in [
        (
            (640, 480),
            Upscaler::Off,
            false,
            SwapRole::Presentable { canvas: (320, 240) },
        ),
        ((800, 600), Upscaler::Off, false, SwapRole::Private),
        ((640, 480), Upscaler::Cunny, false, SwapRole::Private),
        (
            (640, 480),
            Upscaler::Cunny,
            true,
            SwapRole::Presentable { canvas: (320, 240) },
        ),
    ] {
        for damage in [none(), all(), cells(&[rect(1, 2, 3, 4)])] {
            for base in [
                BaseKind::Gpu { presentable: false },
                BaseKind::Software { indexed: true },
            ] {
                let mut f = FrameInfo {
                    damage: damage.clone(),
                    base,
                    ..frame()
                };
                for source in [None, Some((10, 10)), Some((1920, 1080))] {
                    f.video.clear();
                    if let Some(source) = source {
                        f.video.push(VideoGeometry {
                            source_uv: None,
                            background: None,
                            id: 3,
                            source,
                            rect: rect(0, 0, 10, 10),
                            changed: true,
                        });
                    }
                    let p = run(
                        &f,
                        &Settings {
                            view,
                            upscaler,
                            ..settings()
                        },
                        &CompositorCaps {
                            fractional_crops: !cfg!(target_os = "android"),
                            presentable_compressed: compressed,
                            yuv_layers: false,
                            ..caps()
                        },
                        &PoolState::default(),
                    );
                    assert_eq!(p.swap_role, expected);
                }
            }
        }
    }
}

#[test]
fn headless_records_and_reports_in_order_and_is_object_safe() {
    let p = run(&frame(), &settings(), &caps(), &PoolState::default());
    let mut headless = HeadlessCompositor::new(caps());
    let at = Instant::now();
    let damage = Tiles {
        size: (16, 16),
        rects: vec![rect(0, 0, 320, 240)],
    };
    let compositor: &mut dyn Compositor = &mut headless;
    assert_eq!(compositor.caps(), caps());
    for serial in 0..3 {
        assert!(
            compositor
                .present(
                    &p.layers,
                    &damage,
                    &mut [],
                    serial,
                    at + Duration::from_millis(serial)
                )
                .is_ok()
        );
    }
    let mut shown = vec![];
    let mut released = vec![];
    compositor.drain(&mut shown, &mut released);
    assert_eq!(shown.len(), 3);
    for (serial, report) in shown.iter().enumerate() {
        assert_eq!(report.serial, serial as u64);
        assert_eq!(
            report.latched,
            Some(at + Duration::from_millis(serial as u64))
        );
        assert_eq!(report.shown, report.latched);
    }
    compositor.drain(&mut shown, &mut released);
    assert_eq!(shown.len(), 3);
    assert!(released.is_empty());
    assert_eq!(headless.presentations.len(), 3);
    assert_eq!(headless.presentations[0].damage, damage);
    assert_eq!(headless.presentations[0].layers, p.layers);
}

fn record() -> PresentRecord {
    PresentRecord {
        statistics: Default::default(),
        token: 0,
        published_at: Duration::ZERO,
        at: Duration::ZERO,
        cpu_time: Duration::ZERO,
        frame: frame(),
        settings: settings(),
        caps: caps(),
        pools: PoolState::default(),
        plan: run(&frame(), &settings(), &caps(), &PoolState::default()),
        stage_times: vec![Duration::from_nanos(123)],
    }
}

#[test]
fn recorder_round_trip_ring_and_replay_first_difference() {
    let mut recorder = Recorder::default();
    recorder.record(Event::Lifecycle("created".into()));
    recorder.record(Event::Present(Box::new(record())));
    recorder.record(Event::Released { pool: 2, buffer: 1 });
    let dump = recorder.dump();
    assert_eq!(dump.lines().count(), 3);
    let mut events = parse(&dump).unwrap();
    assert_eq!(events, recorder.events().cloned().collect::<Vec<_>>());
    assert_eq!(replay(&events), Ok(()));
    events.push(Event::Present(Box::new(record())));
    let Event::Present(r) = &mut events[1] else {
        panic!()
    };
    r.plan.layers.base.nearest = !r.plan.layers.base.nearest;
    let mismatch = replay(&events).unwrap_err();
    assert_eq!(mismatch.record, 1);
    assert_eq!(mismatch.serial, 50);
    assert_ne!(mismatch.recorded, mismatch.replayed);
    for serial in 0..CAPACITY + 2 {
        recorder.record(Event::Released {
            pool: 0,
            buffer: serial,
        });
    }
    let events = parse(&recorder.dump()).unwrap();
    assert_eq!(events.len(), CAPACITY);
    assert_eq!(events[0], Event::Released { pool: 0, buffer: 2 });
    assert_eq!(
        events[CAPACITY - 1],
        Event::Released {
            pool: 0,
            buffer: CAPACITY + 1
        }
    );
    assert!(parse("invalid\n").is_err());
    assert!(parse("").unwrap().is_empty());
}

#[test]
fn recorder_replays_stateful_upscales_and_composed_video() {
    let mut recorder = Recorder::default();
    let mut pools = PoolState::default();
    for (serial, model) in [
        Upscaler::Anime4k,
        Upscaler::Cunny,
        Upscaler::Artcnn,
        Upscaler::K7,
    ]
    .into_iter()
    .enumerate()
    {
        let f = FrameInfo {
            serial: serial as u64,
            damage: if serial == 0 {
                all()
            } else {
                cells(&[rect(40, 40, 20, 20)])
            },
            video: [VideoGeometry {
                source_uv: None,
                background: None,
                id: 7,
                source: (20, 20),
                rect: rect(50, 50, 20, 20),
                changed: serial % 2 == 0,
            }]
            .into_iter()
            .collect(),
            ..frame()
        };
        let s = Settings {
            upscaler: model,
            rotation: Rotation::Half,
            background: [0.1, 0.2, 0.3],
            ..settings()
        };
        let p = plan(&f, &s, &caps(), &pools);
        recorder.record(Event::Present(Box::new(PresentRecord {
            statistics: Default::default(),
            token: 0,
            published_at: Duration::ZERO,
            at: Duration::ZERO,
            cpu_time: Duration::ZERO,
            frame: f,
            settings: s,
            caps: caps(),
            pools: pools.clone(),
            plan: p.clone(),
            stage_times: vec![Duration::from_nanos(12), Duration::from_nanos(34)],
        })));
        pools.after_present(&p);
    }
    let parsed = parse(&recorder.dump()).unwrap();
    assert_eq!(parsed, recorder.events().cloned().collect::<Vec<_>>());
    assert_eq!(replay(&parsed), Ok(()));
}

#[test]
fn executor_recorder_keeps_600_presents_independently_of_releases() {
    let mut recorder = Recorder::present_history();
    for serial in 0..CAPACITY + 5 {
        let mut record = record();
        record.frame.serial = serial as u64;
        recorder.record(Event::Present(Box::new(record)));
        for buffer in 0..8 {
            recorder.record(Event::Released { pool: 0, buffer });
        }
    }
    let records = recorder
        .events()
        .filter_map(|e| {
            if let Event::Present(r) = e {
                Some(r)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(records.len(), CAPACITY);
    assert_eq!(records[0].frame.serial, 5);
    assert_eq!(records.last().unwrap().frame.serial, (CAPACITY + 4) as u64);
    assert!(recorder.events().count() <= CAPACITY * 9);
}

// spec: HOST-VIEW-12
#[test]
fn disabled_damage_setting_preserves_legacy_plans_at_buffer_ages_one_to_three() {
    let mut old = serde_json::to_value(settings()).unwrap();
    old.as_object_mut().unwrap().remove("damage_overlay");
    let legacy: Settings = serde_json::from_value(old).unwrap();
    let mut explicit = settings();
    explicit.damage_overlay = false;
    for age in 1..=3 {
        let mut pools = PoolState::default();
        let mut info = frame();
        info.base = BaseKind::Gpu { presentable: false };
        for serial in 0..6 {
            info.damage = cells(&[rect(serial * 16, 16, 8, 8)]);
            for pool in &mut pools.pools {
                for buffer in &mut pool.buffers {
                    buffer.age = Some(age);
                }
            }
            let expected = plan(&info, &legacy, &caps(), &pools);
            assert_eq!(plan(&info, &explicit, &caps(), &pools), expected);
            pools.after_present(&expected);
        }
    }
}
