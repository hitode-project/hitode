use crate::{Rect, compositor::*, plan::*, recorder::*};
use std::time::{Duration, Instant};

fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect { x, y, w, h }
}
fn frame() -> FrameInfo {
    FrameInfo {
        serial: 50,
        canvas: (320, 240),
        base: BaseKind::Gpu { presentable: true },
        damage: Damage::Whole,
        video: Default::default(),
        cursor_regions: Default::default(),
    }
}
fn settings() -> Settings {
    Settings {
        view: (640, 480),
        rotation: Rotation::None,
        upscaler: Upscaler::Off,
        background: [0.0; 3],
    }
}
fn caps() -> CompositorCaps {
    CompositorCaps {
        scales: true,
        rotates: true,
        yuv_layers: true,
        solid_color_layers: true,
        damage_hints: true,
        presentable_compressed: false,
        tile: (0, 0),
    }
}
fn cost() -> CostModel {
    CostModel {
        ns_per_megapixel: [
            (Tier::Selected, 1_000_000.0),
            (Tier::Small, 500_000.0),
            (Tier::Spatial, 100_000.0),
        ]
        .into_iter()
        .collect(),
        thermal: Thermal::Nominal,
        frame_budget_ns: 10_000_000,
    }
}
fn run(f: &FrameInfo, s: &Settings, c: &CompositorCaps, p: &PoolState) -> Plan {
    plan(f, s, c, &cost(), p)
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
fn stage_tiles(stage: &Stage) -> Vec<&Tiles> {
    match stage {
        Stage::Ingest { tiles, .. }
        | Stage::Video { tiles, .. }
        | Stage::Output { tiles, .. }
        | Stage::Refine { tiles, .. } => vec![tiles],
        Stage::Upscale { regions, .. } => regions.iter().map(|(tiles, _)| tiles).collect(),
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
            scales: false,
            ..caps()
        },
        &PoolState::default(),
    );
    assert!(p.stages.is_empty());
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
            ((640, 480), Upscaler::Cunny, 1),
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
                Some(Stage::Output { from, .. } | Stage::Upscale { from, .. }) => {
                    assert_eq!(*from, Source::Base)
                }
                None => assert_eq!(p.layers.base.source, Source::Base),
                other => panic!("unexpected {other:?}"),
            }
        }
    }
}

// spec: HOST-VIEW-3
#[test]
fn video_layers_or_canvas_composition() {
    for (upscaler, yuv_layers, cursor, layered) in [
        (Upscaler::Off, true, None, true),
        (Upscaler::Off, true, Some(rect(5, 5, 2, 2)), true),
        (Upscaler::Off, true, Some(rect(15, 25, 2, 2)), false),
        (Upscaler::Off, false, None, false),
        (Upscaler::Anime4k, true, None, false),
    ] {
        let mut f = frame();
        f.damage = Damage::None;
        f.video.push(VideoGeometry {
            id: 7,
            source: (160, 120),
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
        if layered {
            let (id, layer) = &p.layers.video[0];
            assert_eq!(*id, 7);
            assert_eq!(layer.source, Source::Video(7));
            assert_eq!(layer.crop, rect(0, 0, 160, 120));
            assert_eq!(layer.dst, rect(20, 40, 200, 160));
            assert_eq!(layer.damage, Damage::Whole);
            assert!(p.stages.is_empty());
        } else {
            assert!(matches!(
                p.stages[0],
                Stage::Video {
                    from: Source::Base,
                    ..
                }
            ));
            assert_eq!(p.targets[0].size, f.canvas);
            if upscaler != Upscaler::Off {
                assert!(matches!(
                    p.stages[1],
                    Stage::Upscale {
                        from: Source::Pool(PoolId(1)),
                        ..
                    }
                ));
            }
            assert_eq!(p.damage, Damage::rects([rect(10, 20, 100, 80)]));
        }
    }
    let mut f = frame();
    f.base = BaseKind::Software { indexed: true };
    f.video.push(VideoGeometry {
        id: 1,
        source: (10, 10),
        rect: rect(0, 0, 10, 10),
        changed: false,
    });
    let p = run(
        &f,
        &settings(),
        &CompositorCaps {
            yuv_layers: false,
            ..caps()
        },
        &PoolState::default(),
    );
    assert_eq!(p.stages.len(), 1); // Decode merges into canvas composition.
    assert!(matches!(p.stages[0], Stage::Video { .. }));
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
        for (view, expected, rotation, scales) in [
            ((960, 720), (640, 480), Rotation::None, true),
            ((480, 360), (480, 360), Rotation::None, true),
            ((720, 960), (480, 640), Rotation::Quarter, true),
            ((360, 480), (360, 480), Rotation::ThreeQuarters, true),
            ((960, 720), (960, 720), Rotation::Half, false),
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
                    rotates: false,
                    scales,
                    ..caps()
                },
                &PoolState::default(),
            );
            assert_eq!(p.targets[0].size, expected);
            assert_eq!(p.stages.len(), 1);
            assert!(matches!(
                p.stages[0],
                Stage::Upscale {
                    scale: Scale::Model,
                    ..
                }
            ));
            let Stage::Upscale { transform, .. } = &p.stages[0] else {
                unreachable!()
            };
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
        damage: Damage::rects([rect(15, 15, 2, 2), rect(18, 18, 2, 2)]),
        ..frame()
    };
    f.base = BaseKind::Software { indexed: false };
    let p = run(&f, &settings(), &caps(), &warm(0, f.canvas, Some(1)));
    assert_eq!(stage_tiles(&p.stages[0])[0].rects, [rect(0, 0, 32, 32)]);
    assert_eq!(p.layers.base.damage, Damage::rects([rect(0, 0, 32, 32)]));
    for (model, radius) in MODEL_RADII {
        let f = FrameInfo {
            damage: Damage::rects([rect(40, 50, 1, 1)]),
            ..frame()
        };
        let p = run(
            &f,
            &Settings {
                upscaler: model,
                ..settings()
            },
            &caps(),
            &warm(2, (640, 480), Some(1)),
        );
        let x = (40 - radius) * 2 / 16 * 16;
        let y = (50 - radius) * 2 / 16 * 16;
        let right = ((41 + radius) * 2).div_ceil(16) * 16;
        let bottom = ((51 + radius) * 2).div_ceil(16) * 16;
        assert_eq!(
            stage_tiles(&p.stages[0])[0].rects,
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
    for damage in [Damage::Whole, Damage::None] {
        let p = run(
            &FrameInfo {
                damage: damage.clone(),
                ..f.clone()
            },
            &settings(),
            &caps(),
            &warm(0, f.canvas, Some(1)),
        );
        let expected = if damage == Damage::Whole {
            vec![rect(0, 0, 320, 240)]
        } else {
            vec![]
        };
        assert_eq!(stage_tiles(&p.stages[0])[0].rects, expected);
    }
    // Rounding clips to target edges, including after a clockwise rotation.
    let f = FrameInfo {
        damage: Damage::rects([rect(319, 239, 1, 1)]),
        ..frame()
    };
    let p = run(
        &f,
        &Settings {
            view: (480, 640),
            rotation: Rotation::Quarter,
            upscaler: Upscaler::Cunny,
            ..settings()
        },
        &caps(),
        &warm(2, (480, 640), Some(1)),
    );
    assert_eq!(stage_tiles(&p.stages[0])[0].rects, [rect(0, 608, 32, 32)]);
    let d = Damage::rects((0..5).map(|n| rect(n * 10, 0, 1, 1)));
    assert_eq!(d, Damage::rects([rect(0, 0, 41, 1)]));
}

// spec: HOST-VIEW-10
#[test]
fn buffer_age_table() {
    let f = FrameInfo {
        base: BaseKind::Software { indexed: false },
        damage: Damage::rects([rect(1, 1, 1, 1)]),
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
            pools.history.push_back(Damage::rects([rect(65, 65, 1, 1)]));
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
    assert!(pools.pools[0].buffers[0].in_use);
    let next = run(&f, &settings(), &caps(), &pools);
    assert_eq!(next.targets[0].buffer, 1);
    pools.after_present(&next);
    assert_eq!(pools.pools[0].buffers[0].age, Some(2));
    pools.release(PoolId(0), 0);
    let next = run(&f, &settings(), &caps(), &pools);
    assert_eq!(next.targets[0].buffer, 0);
    pools.release(PoolId(0), 1);
    for _ in 0..HISTORY + 2 {
        pools.after_present(&next);
        pools.release(PoolId(0), 0);
    }
    assert_eq!(pools.history.len(), HISTORY);
    pools.trim_to_one();
    assert_eq!(pools.pools[0].buffers.len(), 1);
    let quiet = FrameInfo {
        damage: Damage::None,
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
        assert!(pools.unrefined.is_empty());
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
fn budget_and_thermal_tier_table() {
    for (thermal, budget, expected) in [
        (Thermal::Nominal, 307_200, Tier::Selected),
        (Thermal::Fair, 307_200, Tier::Selected),
        (Thermal::Nominal, 307_199, Tier::Small),
        (Thermal::Nominal, 100_000, Tier::Spatial),
        (Thermal::Nominal, 0, Tier::Spatial),
        (Thermal::Serious, 1_000_000, Tier::Small),
        (Thermal::Critical, 1_000_000, Tier::Spatial),
    ] {
        let c = CostModel {
            thermal,
            frame_budget_ns: budget,
            ..cost()
        };
        let p = plan(
            &frame(),
            &Settings {
                upscaler: Upscaler::Cunny,
                ..settings()
            },
            &caps(),
            &c,
            &PoolState::default(),
        );
        let Stage::Upscale { regions, .. } = &p.stages[0] else {
            panic!()
        };
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].1, expected, "{thermal:?} {budget}");
        assert_eq!(p.unrefined.is_empty(), expected == Tier::Selected);
    }
    // Remaining budget is shared across distinct damaged regions.
    let f = FrameInfo {
        damage: Damage::rects([rect(40, 40, 1, 1), rect(200, 180, 1, 1)]),
        ..frame()
    };
    let c = CostModel {
        frame_budget_ns: 5_000,
        ..cost()
    };
    let p = plan(
        &f,
        &Settings {
            upscaler: Upscaler::Cunny,
            ..settings()
        },
        &caps(),
        &c,
        &warm(2, (640, 480), Some(1)),
    );
    let Stage::Upscale { regions, .. } = &p.stages[0] else {
        panic!()
    };
    assert_eq!(
        regions.iter().map(|(_, t)| *t).collect::<Vec<_>>(),
        [Tier::Selected, Tier::Spatial]
    );
}

#[test]
fn quiet_refinement_oldest_first_and_new_damage_reset() {
    let mut pools = warm(2, (640, 480), Some(1));
    pools.unrefined = vec![
        Unrefined {
            rect: rect(200, 160, 16, 16),
            tier: Tier::Spatial,
            since: 20,
        },
        Unrefined {
            rect: rect(32, 32, 16, 16),
            tier: Tier::Small,
            since: 10,
        },
    ];
    let f = FrameInfo {
        damage: Damage::None,
        ..frame()
    };
    let s = Settings {
        upscaler: Upscaler::Cunny,
        ..settings()
    };
    let c = CostModel {
        frame_budget_ns: 10_000,
        ..cost()
    };
    let p = plan(&f, &s, &caps(), &c, &pools);
    assert!(matches!(
        p.stages.last(),
        Some(Stage::Refine {
            from: Source::Base,
            ..
        })
    ));
    assert_eq!(p.unrefined.len(), 1);
    assert_eq!(p.unrefined[0].since, 20);
    assert_ne!(p.damage, Damage::None);
    // One smaller budget still makes progress on a large quiet region.
    pools.unrefined[0].rect = rect(160, 120, 128, 96);
    let p = plan(&f, &s, &caps(), &c, &pools);
    assert!(p.stages.iter().any(|s| matches!(s, Stage::Refine { .. })));
    let f = FrameInfo {
        serial: 100,
        damage: Damage::rects([rect(32, 32, 16, 16)]),
        ..frame()
    };
    let c = CostModel {
        frame_budget_ns: 0,
        ..cost()
    };
    let p = plan(&f, &s, &caps(), &c, &pools);
    assert!(
        p.unrefined
            .iter()
            .any(|r| r.since == 100 && r.tier == Tier::Spatial)
    );
    assert!(!p.unrefined.iter().any(|r| r.since == 10));
    for thermal in [Thermal::Serious, Thermal::Critical] {
        let p = plan(
            &FrameInfo {
                damage: Damage::None,
                ..frame()
            },
            &s,
            &caps(),
            &CostModel { thermal, ..cost() },
            &pools,
        );
        assert!(!p.stages.iter().any(|s| matches!(s, Stage::Refine { .. })));
    }
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
        for damage in [
            Damage::None,
            Damage::Whole,
            Damage::rects([rect(1, 2, 3, 4)]),
        ] {
            for base in [
                BaseKind::Gpu { presentable: false },
                BaseKind::Software { indexed: true },
            ] {
                let mut f = FrameInfo {
                    damage: damage.clone(),
                    base,
                    ..frame()
                };
                for video in [false, true] {
                    if video {
                        f.video.push(VideoGeometry {
                            id: 3,
                            source: (10, 10),
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
                    vec![],
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
        frame: frame(),
        settings: settings(),
        caps: caps(),
        cost: cost(),
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
fn quiet_frames_converge_with_a_chunk_budget() {
    let s = Settings {
        upscaler: Upscaler::Cunny,
        ..settings()
    };
    let mut pools = PoolState::default();
    let low = CostModel {
        frame_budget_ns: 0,
        ..cost()
    };
    let first = plan(&frame(), &s, &caps(), &low, &pools);
    pools.after_present(&first);
    pools.release(PoolId(2), 0);
    let c = CostModel {
        frame_budget_ns: 10_000,
        ..cost()
    };
    for serial in 51..351 {
        let f = FrameInfo {
            serial,
            damage: Damage::None,
            ..frame()
        };
        let p = plan(&f, &s, &caps(), &c, &pools);
        pools.after_present(&p);
        pools.release(PoolId(2), p.targets[0].buffer);
        if pools.unrefined.is_empty() {
            return;
        }
    }
    panic!("quiet regions did not converge to the selected tier");
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
                Damage::Whole
            } else {
                Damage::rects([rect(40, 40, 20, 20)])
            },
            video: [VideoGeometry {
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
        let c = CostModel {
            frame_budget_ns: 100_000,
            ..cost()
        };
        let p = plan(&f, &s, &caps(), &c, &pools);
        recorder.record(Event::Present(Box::new(PresentRecord {
            frame: f,
            settings: s,
            caps: caps(),
            cost: c,
            pools: pools.clone(),
            plan: p.clone(),
            stage_times: vec![Duration::from_nanos(12), Duration::from_nanos(34)],
        })));
        pools.after_present(&p);
        for target in &p.targets {
            pools.release(target.pool, target.buffer);
        }
    }
    let parsed = parse(&recorder.dump()).unwrap();
    assert_eq!(parsed, recorder.events().cloned().collect::<Vec<_>>());
    assert_eq!(replay(&parsed), Ok(()));
    // A complete selected redraw already refines the aged target.
    let mut r = record();
    r.settings.upscaler = Upscaler::Cunny;
    r.pools.unrefined.push(Unrefined {
        rect: rect(0, 0, 16, 16),
        tier: Tier::Spatial,
        since: 1,
    });
    let p = plan(&r.frame, &r.settings, &r.caps, &r.cost, &r.pools);
    assert!(!p.stages.iter().any(|s| matches!(s, Stage::Refine { .. })));
    assert!(p.unrefined.is_empty());
}
