use super::*;
fn quad(f: &mut Fixture, x: f32, color: [f32; 4]) -> fixed::Draw {
    quad_deep(f, x, 0., color)
}
fn quad_deep(f: &mut Fixture, x: f32, z: f32, color: [f32; 4]) -> fixed::Draw {
    let mut d = draw(f);
    d.vertices = [
        (x, 3.),
        (x + 2., 3.),
        (x, 5.),
        (x, 5.),
        (x + 2., 3.),
        (x + 2., 5.),
    ]
    .map(|(x, y)| Vertex {
        position: [x / 8. - 1., 1. - y / 6., z, 1.],
        color,
        uv: [0.; 2],
    })
    .to_vec()
    .into();
    d
}
fn bounds(f: &mut Fixture, d: fixed::Draw) -> Rect {
    let pointer = f.device;
    let e = env(f);
    damage::bounds(&e, device(&e, pointer).unwrap(), &Operation::Draw(d))
}
fn recorded_bounds(f: &mut Fixture) -> Rect {
    let pointer = f.device;
    let e = env(f);
    let d = device(&e, pointer).unwrap();
    damage::bounds(&e, d, d.frame.commands.last().unwrap())
}

#[test]
// spec: GFX-REC-2, GFX-REC-4, GFX-REC-7
fn sparse_draw_keys_damage_and_pixels_match_complete_constants() {
    check_sparse_constants(false);
    check_sparse_constants(true);
}

fn check_sparse_constants(verify: bool) {
    let mut f = fixture();
    f.m.ext.get_mut::<State>().verify_frames = verify;
    let output = f.packet(0u32);
    let vs = f.packet([
        0xfffe0200u32,
        0x0200001f,
        0x80000000,
        0x900f0000,
        0x03000002,
        0xc00f0000,
        0x90e40000,
        0xa0e40003,
        0xffff,
    ]);
    f.okay(
        "IDirect3DDevice9::CreateVertexShader",
        &[f.device, vs, output],
    );
    let vs = f.m.cpu.read_u32(output).unwrap();
    let ps = f.packet([0xffff0200u32, 0x02000001, 0x800f0800, 0xa0e40007, 0xffff]);
    f.okay(
        "IDirect3DDevice9::CreatePixelShader",
        &[f.device, ps, output],
    );
    let ps = f.m.cpu.read_u32(output).unwrap();
    f.okay("IDirect3DDevice9::SetVertexShader", &[f.device, vs]);
    f.okay("IDirect3DDevice9::SetPixelShader", &[f.device, ps]);
    f.okay("IDirect3DDevice9::SetFVF", &[f.device, 2]);
    f.okay("IDirect3DDevice9::SetRenderState", &[f.device, 22, 1]);
    let vertices = f.packet([[-0.75f32, 0.5, 0.], [-0.5, 0.5, 0.], [-0.75, 0., 0.]]);
    let mut keys = Vec::new();
    let mut draw_keys = Vec::new();
    let mut frame_pixels = Vec::new();
    let mut frame_bounds = Vec::new();
    for frame_index in 0..3 {
        {
            let owner = id(&mut f);
            let e = env(&mut f);
            // Mutate unused registers in every bank, including nonfinite float bits.
            let Some(Payload::Device(d)) = com(&e).get_identity_mut(&mut e.sys.objects, owner)
            else {
                unreachable!()
            };
            d.shaders
                .vertex_constants
                .floats
                .fill([f32::from_bits(0x7fc00000 | frame_index as u32); 4]);
            d.shaders
                .pixel_constants
                .floats
                .fill([f32::from_bits(0x7f800000 | ((frame_index as u32 & 1) << 31)); 4]);
            d.shaders.vertex_constants.integers.fill([frame_index; 4]);
            d.shaders.pixel_constants.integers.fill([frame_index; 4]);
            d.shaders.vertex_constants.booleans.fill(frame_index);
            d.shaders.pixel_constants.booleans.fill(frame_index);
            d.shaders.vertex_constants.floats[3] =
                [if frame_index == 2 { 0.25 } else { 0. }, 0., 0., 0.];
            d.shaders.pixel_constants.floats[7] = if frame_index == 2 {
                [0., 1., 0., 1.]
            } else {
                [1., 0., 0., 1.]
            };
        }
        let pointer = f.device;
        draw_keys.push(draw_key(&env(&mut f), pointer));
        clear(&mut f, 0xff000000);
        f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
        f.okay(
            "IDirect3DDevice9::DrawPrimitiveUP",
            &[f.device, 4, 1, vertices, 12],
        );
        f.okay("IDirect3DDevice9::EndScene", &[f.device]);
        keys.push(command_keys(&mut env(&mut f), pointer));
        frame_bounds.push(recorded_bounds(&mut f));
        let (mut full, state) = {
            let e = env(&mut f);
            let device = device(&e, pointer).unwrap();
            let Operation::Draw(d) = device.frame.commands.last().unwrap() else {
                unreachable!()
            };
            let p = d.programmable.as_ref().unwrap();
            assert_eq!(
                p.constant_ranges().map(|(_, b)| b.len()).sum::<usize>(),
                672
            );
            assert_eq!(p.vertex_constants.floats[255], [0.; 4]);
            assert_eq!(p.pixel_constants.floats[255], [0.; 4]);
            let mut full = p.clone();
            Arc::make_mut(&mut full).vertex_constants.floats =
                device.shaders.vertex_constants.floats;
            Arc::make_mut(&mut full).vertex_constants.integers =
                device.shaders.vertex_constants.integers;
            Arc::make_mut(&mut full).vertex_constants.booleans = device
                .shaders
                .vertex_constants
                .booleans
                .map(|b| [b, 0, 0, 0]);
            Arc::make_mut(&mut full).pixel_constants.floats = device.shaders.pixel_constants.floats;
            Arc::make_mut(&mut full).pixel_constants.integers =
                device.shaders.pixel_constants.integers;
            Arc::make_mut(&mut full).pixel_constants.booleans = device
                .shaders
                .pixel_constants
                .booleans
                .map(|b| [b, 0, 0, 0]);
            let full_bounds = damage::bounds(
                &e,
                device,
                &Operation::Draw(fixed::Draw {
                    target: d.target.clone(),
                    image: d.image.clone(),
                    vertices: Arc::default(),
                    texcoords: None,
                    state: d.state,
                    programmable: Some(full.clone()),
                    recording_key: d.recording_key,
                    program_key: d.program_key,
                }),
            );
            assert_eq!(full_bounds, *frame_bounds.last().unwrap());
            (full, d.state)
        };
        Arc::make_mut(&mut full).program.constants = [adv_device::constants::ConstantUsage::ALL; 2];
        let mut gpu = f.m.ext.get_mut::<State>().gpu.take().unwrap();
        let target = gpu
            .target(16, 12, false)
            .unwrap()
            .with_alpha_mode(AlphaMode::Opaque);
        gpu.clear(&target, Some([0., 0., 0., 1.]), None).unwrap();
        gpu.draw_programmable(&target, &full, &state).unwrap();
        f.m.ext.get_mut::<State>().gpu = Some(gpu);
        present(&mut f);
        frame_pixels.push(latest(&mut f));
        assert_eq!(*frame_pixels.last().unwrap(), pixels(&mut f, &target.image));
    }
    assert_eq!(draw_keys[0], draw_keys[1]);
    assert_ne!(draw_keys[1], draw_keys[2]);
    assert_eq!(keys[0], keys[1]);
    assert_ne!(keys[1], keys[2]);
    assert_eq!(frame_bounds[0], frame_bounds[1]);
    assert_ne!(frame_bounds[1], frame_bounds[2]);
    assert_eq!(frame_pixels[0], frame_pixels[1]);
    assert_ne!(frame_pixels[1], frame_pixels[2]);
    let counts = f.m.ext.get::<State>().frames;
    assert_eq!(counts.eliminated_frames, u64::from(!verify));
    assert_eq!(counts.clipped_executions, u64::from(!verify));
    assert_eq!(counts.verification_mismatches, 0);
    println!(
        "authored two-register draw: verify={verify}; constant hashing payload 18560 -> 64 bytes/draw; uniform writes 10368 -> 672 bytes/draw; eliminated frames={}; clipped executions={}; mismatches=0",
        counts.eliminated_frames, counts.clipped_executions
    );
}
// spec: GFX-REC-4
#[test]
fn bounds_cover_pretransformed_fixed_pipeline_and_conservative_fallbacks() {
    let mut f = fixture();
    let expected = Rect {
        x: 1,
        y: 2,
        w: 5,
        h: 5,
    };
    f.okay("IDirect3DDevice9::SetFVF", &[f.device, 0x44]);
    f.okay("IDirect3DDevice9::SetRenderState", &[f.device, 22, 1]);
    f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
    let words = [
        2f32.to_bits(),
        3f32.to_bits(),
        0,
        1f32.to_bits(),
        0xffabcdef,
        4f32.to_bits(),
        3f32.to_bits(),
        0,
        1f32.to_bits(),
        0xffabcdef,
        2f32.to_bits(),
        5f32.to_bits(),
        0,
        1f32.to_bits(),
        0xffabcdef,
    ];
    let at = f.packet(words);
    f.okay(
        "IDirect3DDevice9::DrawPrimitiveUP",
        &[f.device, 4, 1, at, 20],
    );
    assert_eq!(recorded_bounds(&mut f), expected);
    f.okay("IDirect3DDevice9::SetFVF", &[f.device, 0x42]);
    f.okay("IDirect3DDevice9::SetRenderState", &[f.device, 137, 0]);
    let words = [
        (-0.5f32).to_bits(),
        0.5f32.to_bits(),
        0,
        0xffffffff,
        0f32.to_bits(),
        0.5f32.to_bits(),
        0,
        0xffffffff,
        (-0.5f32).to_bits(),
        0f32.to_bits(),
        0,
        0xffffffff,
    ];
    let at = f.packet(words);
    f.okay(
        "IDirect3DDevice9::DrawPrimitiveUP",
        &[f.device, 4, 1, at, 16],
    );
    assert_eq!(
        recorded_bounds(&mut f),
        Rect {
            x: 3,
            y: 2,
            w: 7,
            h: 6
        }
    );
    let matrix = f.packet([
        1f32, 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0.25, 0., 0., 1.,
    ]);
    f.okay("IDirect3DDevice9::SetTransform", &[f.device, 256, matrix]);
    f.okay(
        "IDirect3DDevice9::DrawPrimitiveUP",
        &[f.device, 4, 1, at, 16],
    );
    assert_eq!(
        recorded_bounds(&mut f),
        Rect {
            x: 5,
            y: 2,
            w: 7,
            h: 6
        }
    );
    let full = Rect {
        x: 0,
        y: 0,
        w: 16,
        h: 12,
    };
    let mut d = quad(&mut f, 2., [1.; 4]);
    Arc::make_mut(&mut d.vertices)[0].position[3] = 0.;
    assert_eq!(bounds(&mut f, d), full);
    let mut d = quad(&mut f, 2., [1.; 4]);
    let first = d.vertices[0];
    Arc::make_mut(&mut d.vertices).resize(65, first);
    assert_eq!(bounds(&mut f, d), full);
    let mut d = quad(&mut f, 2., [1.; 4]);
    d.state.scissor = Some(Rect {
        x: 3,
        y: 3,
        w: 1,
        h: 1,
    });
    assert_eq!(
        bounds(&mut f, d),
        Rect {
            x: 3,
            y: 3,
            w: 1,
            h: 1
        }
    );
}
fn scene(f: &mut Fixture, colors: [[f32; 4]; 3]) {
    clear(f, 0xff000000);
    for (n, color) in colors.into_iter().enumerate() {
        let d = quad(f, 2. + n as f32 * 4., color);
        record_draw(f, d);
    }
}
fn pixels(f: &mut Fixture, image: &Image) -> Vec<u8> {
    let mut ticket =
        f.m.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .readback(image, None)
            .unwrap();
    ticket.take_completion_job().unwrap()();
    ticket.poll().unwrap().unwrap()
}
fn reference(f: &mut Fixture, colors: [[f32; 4]; 3]) -> Vec<u8> {
    let target =
        f.m.ext
            .get::<State>()
            .gpu
            .as_ref()
            .unwrap()
            .target(16, 12, false)
            .unwrap()
            .with_alpha_mode(AlphaMode::Opaque);
    f.m.ext
        .get_mut::<State>()
        .gpu
        .as_mut()
        .unwrap()
        .clear(&target, Some([0., 0., 0., 1.]), None)
        .unwrap();
    for (n, color) in colors.into_iter().enumerate() {
        let d = quad(f, 2. + n as f32 * 4., color);
        f.m.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .draw(&target, &d.vertices, None, d.state)
            .unwrap();
    }
    pixels(f, &target.image)
}
fn latest(f: &mut Fixture) -> Vec<u8> {
    let frame = f.m.ext.get::<Presents>().latest.as_ref().unwrap().clone();
    frame.pixels(f.m.ext.get_mut::<State>().gpu.as_mut().unwrap())
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn one_changed_quad_replays_in_order_and_clear_preserves_pixels_outside_damage() {
    let mut f = fixture();
    let mut colors = [[1., 0., 0., 1.], [0., 1., 0., 1.], [0., 0., 1., 1.]];
    scene(&mut f, colors);
    present(&mut f);
    colors[0] = [1., 1., 0., 1.];

    scene(&mut f, colors);
    present(&mut f);

    assert_eq!(latest(&mut f), reference(&mut f, colors));
    let counts = f.m.ext.get::<State>().frames;
    assert_eq!(counts.clipped_executions, 1);
    // The quad's 4x4 bounds round to their 8x8 damage cell.
    assert!((counts.clipped_area_sum - (1. + 64. / 192.)).abs() < 1e-9);
    assert_eq!(counts.changed_pixels, 192 + 64);
    assert_eq!(counts.executed_pixels, 192 + 64);
    assert_eq!(counts.extra_pixels, 0);
    assert_eq!(counts.submitted_vertices, 18 + 12);
    assert_eq!(counts.repeated_vertices, 0);
    scene(&mut f, colors);
    present(&mut f);
    let repeated = f.m.ext.get::<State>().frames;
    assert_eq!(repeated.changed_pixels, counts.changed_pixels);
    assert_eq!(repeated.executed_pixels, counts.executed_pixels);
    assert_eq!(repeated.submitted_vertices, counts.submitted_vertices);
}

fn bind_depth(f: &mut Fixture, width: u32, height: u32) -> u32 {
    let output = f.packet(0u32);
    f.okay(
        "IDirect3DDevice9::CreateDepthStencilSurface",
        &[f.device, width, height, 75, 0, 0, 0, output, 0],
    );
    let pointer = f.m.cpu.read_u32(output).unwrap();
    f.okay(
        "IDirect3DDevice9::SetDepthStencilSurface",
        &[f.device, pointer],
    );
    pointer
}

// spec: GFX-REC-2, GFX-REC-3, GFX-REC-4, GFX-REC-5
#[test]
// spec: DEV-DEPTH-1, DEV-DEPTH-2
fn padded_depth_and_stencil_clears_keep_local_redraw_pixel_exact() {
    for mode in 0..3 {
        let mut outputs = Vec::new();
        for verify in [false, true] {
            let mut f = fixture();
            bind_depth(&mut f, 32, 24);
            f.m.ext.get_mut::<State>().verify_frames = verify;
            let mut frames = Vec::new();
            for n in 0..8 {
                f.okay(
                    "IDirect3DDevice9::Clear",
                    &[f.device, 0, 0, 7, 0xff112233, 0.75f32.to_bits(), 3],
                );
                // These are full color-view clears, but only partial writes to Z/S.
                f.okay(
                    "IDirect3DDevice9::Clear",
                    &[f.device, 0, 0, 6, 0, 0.75f32.to_bits(), 3],
                );
                for index in 0..3 {
                    let mut d = quad(
                        &mut f,
                        2. + index as f32 * 4.,
                        [1., if index == 0 { n as f32 / 8. } else { 0. }, 0., 1.],
                    );
                    let device = f.device;
                    d.target = depth::attach(&env(&mut f), device, d.target).unwrap();
                    if mode == 1 {
                        d.state.depth = Some(Compare::Less);
                        d.state.depth_write = true;
                    } else if mode == 2 {
                        let face = StencilFace {
                            compare: Compare::Equal,
                            fail: StencilOp::Keep,
                            depth_fail: StencilOp::Keep,
                            pass: StencilOp::Keep,
                        };
                        d.state.stencil = Some(Stencil {
                            front: face,
                            back: face,
                            read_mask: 255,
                            write_mask: 0,
                        });
                        d.state.stencil_reference = 3;
                    }
                    record_draw(&mut f, d);
                }
                present(&mut f);
                frames.push(latest(&mut f));
            }
            let counts = f.m.ext.get::<State>().frames;
            assert_eq!(counts.verification_mismatches, 0);
            if !verify {
                assert!(counts.clipped_executions >= 7, "mode {mode}");
                assert!(counts.clipped_area_sum <= 8.);
            }
            outputs.push(frames);
        }
        assert_eq!(outputs[0], outputs[1], "mode {mode}");
    }
}

// spec: GFX-REC-4, GFX-REC-5, GFX-PASS-3
#[test]
fn clipped_partial_attachment_clears_preserve_values_seen_by_a_larger_target() {
    let mut outputs = Vec::new();
    for verify in [true, false] {
        let mut f = fixture();
        bind_depth(&mut f, 32, 24);
        f.m.ext.get_mut::<State>().verify_frames = verify;
        let large = f.render_target(32, 24, 21);
        f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, large]);
        f.okay(
            "IDirect3DDevice9::Clear",
            &[f.device, 0, 0, 7, 0xff000000, 0.25f32.to_bits(), 7],
        );
        f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, f.back]);
        present(&mut f);
        for n in 0..4 {
            f.okay(
                "IDirect3DDevice9::Clear",
                &[f.device, 0, 0, 7, 0xff000000, 0.75f32.to_bits(), 3],
            );
            let d = quad(&mut f, 2., [1., n as f32 / 4., 0., 1.]);
            record_draw(&mut f, d);
            present(&mut f);
        }
        f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, large]);
        clear(&mut f, 0xff000000);
        let target = f.target(large);
        let device = f.device;
        let target = depth::attach(&env(&mut f), device, target).unwrap();
        for stencil in [false, true] {
            let mut d = draw(&mut f);
            d.target = target.clone();
            d.state = DrawState::new(32, 24);
            d.vertices = [
                (-1., 1.),
                (-1., -1.),
                (1., 1.),
                (1., 1.),
                (-1., -1.),
                (1., -1.),
            ]
            .map(|(x, y)| Vertex {
                position: [x, y, 0.5, 1.],
                color: if stencil {
                    [0., 0., 1., 1.]
                } else {
                    [0., 1., 0., 1.]
                },
                uv: [0.; 2],
            })
            .to_vec()
            .into();
            if stencil {
                let face = StencilFace {
                    compare: Compare::Equal,
                    fail: StencilOp::Keep,
                    depth_fail: StencilOp::Keep,
                    pass: StencilOp::Keep,
                };
                d.state.stencil = Some(Stencil {
                    front: face,
                    back: face,
                    read_mask: 255,
                    write_mask: 0,
                });
                d.state.stencil_reference = 7;
            } else {
                d.state.depth = Some(Compare::Less);
            }
            record_draw(&mut f, d);
        }
        present(&mut f);
        let actual = pixels(&mut f, &target.image);
        for (n, pixel) in actual.as_chunks::<4>().0.iter().enumerate() {
            assert_eq!(
                pixel,
                if n % 32 < 16 && n / 32 < 12 {
                    &[0, 255, 0, 255]
                } else {
                    &[255, 0, 0, 255]
                },
                "pixel {n}, verify {verify}"
            );
        }
        outputs.push(actual);
    }
    assert_eq!(outputs[0], outputs[1]);
}

// spec: GFX-REC-2, GFX-REC-4, GFX-REC-5
#[test]
fn attachment_only_changes_do_not_publish_color_damage() {
    let mut f = fixture();
    bind_depth(&mut f, 32, 24);
    let colors = [[1., 0., 0., 1.], [0., 1., 0., 1.], [0., 0., 1., 1.]];
    scene(&mut f, colors);
    present(&mut f);
    let expected = latest(&mut f);
    for n in 0..3 {
        f.okay(
            "IDirect3DDevice9::Clear",
            &[f.device, 0, 0, 6, 0, (n as f32 / 4.).to_bits(), n],
        );
        present(&mut f);
        assert_eq!(latest(&mut f), expected);
        assert!(
            f.m.ext
                .get::<Presents>()
                .damage
                .last()
                .cloned()
                .flatten()
                .is_some_and(|d| d.is_empty())
        );
    }
}

// spec: GFX-REC-4, GFX-REC-5
#[test]
fn repeated_partial_clear_overwrites_its_region_without_accumulating_damage() {
    let mut outputs = Vec::new();
    for verify in [false, true] {
        let mut f = fixture();
        f.m.ext.get_mut::<State>().verify_frames = verify;
        clear(&mut f, 0xff112233);
        present(&mut f);
        let mut frames = Vec::new();
        for (x, color) in [
            (2, 0xffff0000u32),
            (2, 0xffff0000),
            (6, 0xff00ff00),
            (6, 0xff00ff00),
        ] {
            let owner = id(&mut f);
            let target = f.target(f.back);
            let outcome = record(
                &mut env(&mut f),
                owner,
                Operation::Clear {
                    target,
                    rectangle: Rect {
                        x,
                        y: 3,
                        w: 2,
                        h: 2,
                    },
                    color: Some([
                        f64::from((color >> 16) & 255) / 255.,
                        f64::from((color >> 8) & 255) / 255.,
                        f64::from(color & 255) / 255.,
                        1.,
                    ]),
                    depth: None,
                    stencil: None,
                },
            );
            assert_eq!(f.finish(outcome), 0);
            present(&mut f);
            frames.push(latest(&mut f));
        }
        assert_eq!(frames[0], frames[1]);
        assert_eq!(frames[2], frames[3]);
        if !verify {
            let damage = &f.m.ext.get::<Presents>().damage;
            assert!(damage[2].as_ref().is_some_and(|d| d.is_empty()));
            assert!(damage[4].as_ref().is_some_and(|d| d.is_empty()));
        }
        outputs.push(frames);
    }
    assert_eq!(outputs[0], outputs[1]);
}
#[derive(Default)]
struct Rotation {
    targets: Vec<Target>,
    next: usize,
    damage: Vec<Option<adv_device::surface::Region>>,
}
fn rotate(f: &mut Fixture, count: usize) {
    let mut targets = vec![f.target(f.back)];
    for _ in 1..count {
        targets.push(
            f.m.ext
                .get::<State>()
                .gpu
                .as_ref()
                .unwrap()
                .target(16, 12, false)
                .unwrap()
                .with_alpha_mode(AlphaMode::Opaque),
        );
    }
    f.m.ext.insert(Rotation {
        targets,
        ..Default::default()
    });
    f.m.ext.get_mut::<State>().provider.present = |ext, surface, damage, gpu| {
        let provider = ext.get::<State>().provider;
        let image = (provider.target)(ext, surface).unwrap().image;
        let serial = ext.get::<Presents>().serial + 1;
        let native = test_support::completed_frame(gpu, image, damage.clone(), serial)?;
        let r = ext.get_mut::<Rotation>();
        r.damage.push(damage);
        r.next = (r.next + 1) % r.targets.len();
        let replacement = r.targets[r.next].clone();
        (provider.replace)(ext, surface, replacement).unwrap();
        let p = ext.get_mut::<Presents>();
        p.serial = serial;
        p.published.push(serial);
        p.latest = Some(native);
        Ok(serial)
    };
}
// spec: GFX-REC-5 HOST-VIEW-10
#[test]
fn age_two_and_three_rotating_targets_match_full_execution_bit_for_bit() {
    for count in [2, 3] {
        let mut f = fixture();
        rotate(&mut f, count);
        let mut colors = [[1., 0., 0., 1.], [0., 1., 0., 1.], [0., 0., 1., 1.]];
        let mut previous: Option<Vec<u8>> = None;
        for frame in 0..12 {
            colors[frame % 3][frame % 2] = (frame as f32 + 1.) / 16.;
            scene(&mut f, colors);
            present(&mut f);
            let pixels = latest(&mut f);
            assert_eq!(
                pixels,
                reference(&mut f, colors),
                "rotation {count}, frame {frame}"
            );
            // The published damage covers every pixel that differs from the previous present.
            let published = f.m.ext.get::<Rotation>().damage.last().cloned().flatten();
            if let (Some(r), Some(previous)) = (published, &previous) {
                for (i, (a, b)) in pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(previous.as_chunks::<4>().0.iter())
                    .enumerate()
                {
                    let (x, y) = (i as u32 % 16, i as u32 / 16);
                    let inside = r.intersects(Rect { x, y, w: 1, h: 1 }.into());
                    assert!(
                        inside || a == b,
                        "rotation {count}, frame {frame}, pixel {x},{y}"
                    );
                }
            }
            previous = Some(pixels);
        }
        assert!(f.m.ext.get::<State>().frames.clipped_executions >= 7);
        let damage = &f.m.ext.get::<Rotation>().damage;
        assert!(damage.iter().filter(|d| d.is_some()).count() >= 7);
    }
}
// spec: GFX-REC-3 GFX-REC-4 GFX-REC-5
#[test]
fn verification_checks_outside_changed_bounds_and_reads_presented_rotation_slot() {
    let mut f = fixture();
    rotate(&mut f, 3);
    f.m.ext.get_mut::<State>().verify_frames = true;
    let mut colors = [[1., 0., 0., 1.], [0., 1., 0., 1.], [0., 0., 1., 1.]];
    for n in 0..5 {
        colors[0][1] = n as f32 / 8.;
        scene(&mut f, colors);
        present(&mut f);
        assert_eq!(latest(&mut f), reference(&mut f, colors));
    }
    assert_eq!(f.m.ext.get::<State>().frames.verification_mismatches, 0);
    let owner = id(&mut f);
    // A pixel outside the changed quad's damage cell.
    frame(&mut env(&mut f), owner).previous.as_mut().unwrap().2[(11 * 16 + 15) * 4] ^= 1;
    colors[0][1] = 0.75;
    scene(&mut f, colors);
    present(&mut f);
    assert_eq!(f.m.ext.get::<State>().frames.verification_mismatches, 1);
}
// spec: GFX-REC-4
#[test]
fn shader_bounds_use_float_inputs_and_fall_back_for_nonfloat_or_unavailable_execution() {
    let mut f = fixture();
    let code = f.packet([
        0xfffe0200u32,
        0x0200001f,
        0x80000000,
        0x900f0000,
        0x02000001,
        0xc00f0000,
        0x90e40000,
        0xffff,
    ]);
    let output = f.packet(0u32);
    f.okay(
        "IDirect3DDevice9::CreateVertexShader",
        &[f.device, code, output],
    );
    let shader = f.m.cpu.read_u32(output).unwrap();
    f.okay("IDirect3DDevice9::SetVertexShader", &[f.device, shader]);
    let make = |f: &mut Fixture| {
        let mut d = programmable_draw(f);
        Arc::make_mut(&mut d.vertices).clear();
        let p = Arc::make_mut(d.programmable.as_mut().unwrap());
        p.vertex_count = 3;
        p.streams[0].bytes = bytemuck::cast_slice(&[
            [-0.5f32, 0.5, 0., 1.],
            [0., 0.5, 0., 1.],
            [-0.5, 0., 0., 1.],
        ])
        .to_vec()
        .into();
        d
    };
    let d = make(&mut f);
    assert_eq!(
        bounds(&mut f, d),
        Rect {
            x: 3,
            y: 2,
            w: 7,
            h: 6
        }
    );
    let full = Rect {
        x: 0,
        y: 0,
        w: 16,
        h: 12,
    };
    let mut d = make(&mut f);
    Arc::make_mut(d.programmable.as_mut().unwrap()).streams[0].attributes[0].format =
        AttributeFormat::Bgra8Unorm;
    assert_eq!(bounds(&mut f, d), full);
    let mut d = make(&mut f);
    Arc::make_mut(d.programmable.as_mut().unwrap()).vertex_count = 65;
    assert_eq!(bounds(&mut f, d), full);
    let mut d = make(&mut f);
    Arc::make_mut(d.programmable.as_mut().unwrap()).streams[0].bytes[12..16]
        .copy_from_slice(&(-1f32).to_le_bytes());
    assert_eq!(bounds(&mut f, d), full);
}
// spec: GFX-REC-4 GFX-REC-5
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn readbacks_keep_damage_and_clip_slots_they_did_not_write() {
    for back_first in [false, true] {
        let mut f = fixture();
        let source = f.render_target(16, 12, 21);
        // Readback destinations match the source format.
        let system = f.surface(16, 12, if back_first { 22 } else { 21 }, 2);
        let mut colors = [[1., 0., 0., 1.], [0., 1., 0., 1.], [0., 0., 1., 1.]];
        for _ in 0..2 {
            if back_first {
                scene(&mut f, colors);
                f.okay(
                    "IDirect3DDevice9::GetRenderTargetData",
                    &[f.device, f.back, system],
                );
            } else {
                f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, source]);
                clear(&mut f, 0xff00ff00);
                f.okay(
                    "IDirect3DDevice9::GetRenderTargetData",
                    &[f.device, source, system],
                );
                f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, f.back]);
                scene(&mut f, colors);
            }
            present(&mut f);
            colors[0] = [1., 1., 0., 1.];
        }
        assert_eq!(latest(&mut f), reference(&mut f, colors));
        let counts = f.m.ext.get::<State>().frames;
        // A source cleared alike each frame is read where it already is.
        assert_eq!(
            counts.flushes[Reason::Readback as usize],
            if back_first { 2 } else { 1 }
        );
        // Only the first present changes the whole surface.
        assert_eq!(counts.whole, [0, 1, 0, 0]);
        // A back buffer the readback flushed runs whole that frame.
        assert_eq!(counts.clipped_executions, u64::from(!back_first));
    }
}

// spec: GFX-REC-4 GFX-REC-5
#[test]
fn depth_shared_by_two_slots_clips_color_and_keeps_depth_exact() {
    let mut outputs = Vec::new();
    for verify in [false, true] {
        let mut f = fixture();
        bind_depth(&mut f, 16, 12);
        f.m.ext.get_mut::<State>().verify_frames = verify;
        let source = f.render_target(16, 12, 21);
        let mut frames = Vec::new();
        for n in 0..5 {
            let tested = n == 4;
            // The back buffer clears depth first and the offscreen target
            // last, so a skipped offscreen clear would leave the wrong depth.
            let flags = if tested { 1 } else { 3 };
            f.okay(
                "IDirect3DDevice9::Clear",
                &[f.device, 0, 0, flags, 0xff000000, 0.75f32.to_bits(), 0],
            );
            for index in 0..3 {
                let green = if index == 0 { n as f32 / 8. } else { 0. };
                let z = if tested { 0.25 } else { 0. };
                let mut d = quad_deep(&mut f, 2. + index as f32 * 4., z, [1., green, 0., 1.]);
                if tested {
                    let device = f.device;
                    d.target = depth::attach(&env(&mut f), device, d.target).unwrap();
                    d.state.depth = Some(Compare::Equal);
                }
                record_draw(&mut f, d);
            }
            if !tested {
                f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, source]);
                f.okay(
                    "IDirect3DDevice9::Clear",
                    &[f.device, 0, 0, 3, 0xff00ff00, 0.25f32.to_bits(), 0],
                );
                f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, f.back]);
            }
            present(&mut f);
            frames.push(latest(&mut f));
        }
        let counts = f.m.ext.get::<State>().frames;
        assert_eq!(counts.verification_mismatches, 0);
        if !verify {
            // Only the first present changes the whole surface.
            assert_eq!(counts.whole, [0, 1, 0, 0]);
            assert_eq!(counts.clipped_executions, 4);
        }
        outputs.push(frames);
    }
    assert_eq!(outputs[0], outputs[1]);
}

#[test]
fn sampled_target_changes_damage_the_consuming_draw_and_copy_clips_keep_offsets() {
    let mut f = fixture();
    let source = f.render_target(16, 12, 21);
    let owner = id(&mut f);
    let resource = {
        let e = env(&mut f);
        com(&e).identity::<Payload>(&e.sys.objects, source).unwrap()
    };
    let target = f.target(source);
    let mut keys = Vec::new();
    for color in [0xffff0000, 0xff00ff00] {
        f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, source]);
        clear(&mut f, color);
        f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, f.back]);
        clear(&mut f, 0xff000000);
        if let Some(Payload::Device(d)) =
            com(&env(&mut f)).get_identity_mut(&mut f.m.sys.objects, owner)
        {
            d.texture = Some(resource);
        }
        let mut d = quad(&mut f, 2., [1.; 4]);
        d.image = Some(target.image.clone());
        d.state.texture_op = TextureOp::Texture;
        record_draw(&mut f, d);
        keys.push(frame(&mut env(&mut f), owner).key);
        present(&mut f);
    }
    assert_ne!(keys[0], keys[1]);
    assert_eq!(f.m.ext.get::<State>().frames.clipped_executions, 1);
    // The consuming quad's damage cell.
    assert!((f.m.ext.get::<State>().frames.clipped_area_sum - (1. + 64. / 192.)).abs() < 1e-9);
    // Source and destination offsets must move together in an unscaled copy.
    let gpu = f.m.ext.get_mut::<State>().gpu.as_mut().unwrap();
    let a = gpu.target(4, 4, false).unwrap();
    let b = gpu.target(4, 4, false).unwrap();
    gpu.upload(
        &a.image,
        Rect {
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        },
        16,
        &(0..64u8).collect::<Vec<_>>(),
    )
    .unwrap();
    gpu.clear(&b, Some([0.; 4]), None).unwrap();
    gpu.set_clip(Some(Rect {
        x: 2,
        y: 2,
        w: 1,
        h: 1,
    }));
    gpu.copy(
        &a.image,
        Rect {
            x: 0,
            y: 0,
            w: 3,
            h: 3,
        },
        &b.image,
        1,
        1,
    )
    .unwrap();
    gpu.set_clip(None);
    let pixels = f.pixels(&b.image);
    for n in 0..16 {
        assert_eq!(
            &pixels[n * 4..n * 4 + 4],
            if n == 10 { &[20, 21, 22, 23] } else { &[0; 4] }
        );
    }
}
// spec: GFX-REC-1 GFX-REC-5
#[test]
fn accumulation_and_early_reads_restore_the_latest_frame_after_rotation() {
    for early in [false, true] {
        let mut f = fixture();
        rotate(&mut f, 3);
        let colors = [[1., 0., 0., 1.], [0., 1., 0., 1.], [0., 0., 1., 1.]];
        scene(&mut f, colors);
        present(&mut f);
        let changed = [[0.5, 0., 0., 1.], colors[1], colors[2]];
        scene(&mut f, changed);
        present(&mut f);
        let before = latest(&mut f);
        assert_ne!(f.m.ext.get::<Rotation>().next, 0);
        if early {
            let outcome = flush(&mut f, Reason::Readback);
            assert_eq!(f.finish(outcome), 0);
            let image = f.image(f.back);
            assert_eq!(f.pixels(&image), before);
        }
        let d = quad(&mut f, 2., [1., 1., 1., 1.]);
        record_draw(&mut f, d);
        if early {
            let outcome = flush(&mut f, Reason::Readback);
            assert_eq!(f.finish(outcome), 0);
        }
        present(&mut f);
        let expected = reference(&mut f, [[1., 1., 1., 1.], colors[1], colors[2]]);
        assert_eq!(latest(&mut f), expected);
    }
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn programmable_clipped_replay_matches_full_shader_execution() {
    let mut f = fixture();
    let output = f.packet(0u32);
    let vs = f.packet([
        0xfffe0200u32,
        0x0200001f,
        0x80000000,
        0x900f0000,
        0x02000001,
        0xc00f0000,
        0x90e40000,
        0xffff,
    ]);
    f.okay(
        "IDirect3DDevice9::CreateVertexShader",
        &[f.device, vs, output],
    );
    let vs = f.m.cpu.read_u32(output).unwrap();
    let ps = f.packet([0xffff0200u32, 0x02000001, 0x800f0800, 0xa0e40000, 0xffff]);
    f.okay(
        "IDirect3DDevice9::CreatePixelShader",
        &[f.device, ps, output],
    );
    let ps = f.m.cpu.read_u32(output).unwrap();
    f.okay("IDirect3DDevice9::SetVertexShader", &[f.device, vs]);
    f.okay("IDirect3DDevice9::SetPixelShader", &[f.device, ps]);
    f.okay("IDirect3DDevice9::SetFVF", &[f.device, 2]);
    f.okay("IDirect3DDevice9::SetRenderState", &[f.device, 22, 1]);
    let positions = [
        [-0.75f32, 0.5, 0.],
        [-0.5, 0.5, 0.],
        [-0.75, 0., 0.],
        [-0.75, 0., 0.],
        [-0.5, 0.5, 0.],
        [-0.5, 0., 0.],
    ];
    let vertices = f.packet(positions);
    for color in [[1f32, 0., 0., 1.], [0., 1., 0., 1.]] {
        let constants = f.packet(color);
        f.okay(
            "IDirect3DDevice9::SetPixelShaderConstantF",
            &[f.device, 0, constants, 1],
        );
        clear(&mut f, 0xff000000);
        f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
        f.okay(
            "IDirect3DDevice9::DrawPrimitiveUP",
            &[f.device, 4, 2, vertices, 12],
        );
        f.okay("IDirect3DDevice9::EndScene", &[f.device]);
        let mut gpu = f.m.ext.get_mut::<State>().gpu.take().unwrap();
        let target = gpu
            .target(16, 12, false)
            .unwrap()
            .with_alpha_mode(AlphaMode::Opaque);
        gpu.clear(&target, Some([0., 0., 0., 1.]), None).unwrap();
        {
            let pointer = f.device;
            let e = env(&mut f);
            let d = device(&e, pointer).unwrap();
            let Operation::Draw(d) = d.frame.commands.last().unwrap() else {
                panic!("recorded shader draw");
            };
            gpu.draw_programmable(&target, d.programmable.as_ref().unwrap(), &d.state)
                .unwrap();
        }
        f.m.ext.get_mut::<State>().gpu = Some(gpu);
        present(&mut f);
        let expected = pixels(&mut f, &target.image);
        assert_eq!(latest(&mut f), expected);
    }
    assert_eq!(f.m.ext.get::<State>().frames.clipped_executions, 1);
}
/// A 64x48 back buffer, larger than the cells so one cell's change stays
/// local, and a composite drawing a texture through a half-texel offset
/// with linear filtering.
struct Composite {
    vs: u32,
    ps: u32,
    half: u32,
    /// The whole target, and its left half reading the texture's left half.
    full: u32,
    left: u32,
}
fn composite_fixture(verify: bool) -> (Fixture, Composite) {
    let mut f = fixture();
    f.m.ext.get_mut::<State>().verify_frames = verify;
    let parameters = f.packet(D3DPRESENT_PARAMETERS {
        BackBufferWidth: 64,
        BackBufferHeight: 48,
        BackBufferFormat: 22,
        BackBufferCount: 1,
        SwapEffect: 1,
        hDeviceWindow: 0x7788,
        Windowed: 1,
        ..D3DPRESENT_PARAMETERS::default()
    });
    f.okay("IDirect3DDevice9::Reset", &[f.device, parameters]);
    let pointer = f.device;
    let e = env(&mut f);
    let back = device(&e, pointer).unwrap().back_buffer.unwrap();
    let Some(Payload::Resource(r)) = com(&e).get_identity(&e.sys.objects, back) else {
        unreachable!()
    };
    f.back = r.pointer;
    f.m.ext
        .get_mut::<State>()
        .tables
        .insert("IDirect3DTexture9", 0);
    let output = f.packet([0u32; 2]);
    // mov oPos, v0; mov oT0, v1
    let vs = f.packet([
        0xfffe0200u32,
        0x0200001f,
        0x80000000,
        0x900f0000,
        0x0200001f,
        0x80000005,
        0x900f0001,
        0x02000001,
        0xc00f0000,
        0x90e40000,
        0x02000001,
        0xe00f0000,
        0x90e40001,
        0xffff,
    ]);
    f.okay(
        "IDirect3DDevice9::CreateVertexShader",
        &[f.device, vs, output],
    );
    let vs = f.m.cpu.read_u32(output).unwrap();
    // add r0.xy, t0, c0; texld r0, r0, s0; mov oC0, r0
    let ps = f.packet([
        0xffff0200u32,
        0x0200001f,
        0x80000000,
        0xb0030000,
        0x0200001f,
        0x90000000,
        0xa00f0800,
        0x03000002,
        0x80030000,
        0xb0e40000,
        0xa0e40000,
        0x03000042,
        0x800f0000,
        0x80e40000,
        0xa0e40800,
        0x02000001,
        0x800f0800,
        0x80e40000,
        0xffff,
    ]);
    f.okay(
        "IDirect3DDevice9::CreatePixelShader",
        &[f.device, ps, output],
    );
    let ps = f.m.cpu.read_u32(output).unwrap();
    let half = f.packet([0.5f32 / 64., 0.5 / 48., 0., 0.]);
    let full = f.packet([
        [-1f32, 1., 0., 0., 0.],
        [1., 1., 0., 1., 0.],
        [-1., -1., 0., 0., 1.],
        [1., -1., 0., 1., 1.],
    ]);
    let left = f.packet([
        [-1f32, 1., 0., 0., 0.],
        [0., 1., 0., 0.5, 0.],
        [-1., -1., 0., 0., 1.],
        [0., -1., 0., 0.5, 1.],
    ]);
    (
        f,
        Composite {
            vs,
            ps,
            half,
            full,
            left,
        },
    )
}
/// A 64x48 render-target texture and its surface.
fn layer(f: &mut Fixture) -> (u32, u32) {
    let output = f.packet([0u32; 2]);
    f.okay(
        "IDirect3DDevice9::CreateTexture",
        &[f.device, 64, 48, 1, 1, 21, 0, output, 0],
    );
    let texture = f.m.cpu.read_u32(output).unwrap();
    f.okay("IDirect3DTexture9::GetSurfaceLevel", &[texture, 0, output]);
    (texture, f.m.cpu.read_u32(output).unwrap())
}
/// Clears `target` black and draws one fixed-function quad on it.
fn paint(f: &mut Fixture, target: u32, x: f32, color: [f32; 4]) {
    f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, target]);
    clear(f, 0xff000000);
    let mut d = quad_deep(f, x, 0., color);
    d.target = f.target(target);
    record_draw(f, d);
}
/// Draws `texture` through `quad` into `target`, after clearing it black
/// when `clear` is set.
fn composite(f: &mut Fixture, c: &Composite, texture: u32, target: u32, quad: u32, wipe: bool) {
    f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, target]);
    if wipe {
        clear(f, 0xff000000);
    }
    for (name, args) in [
        ("SetVertexShader", vec![f.device, c.vs]),
        ("SetPixelShader", vec![f.device, c.ps]),
        ("SetPixelShaderConstantF", vec![f.device, 0, c.half, 1]),
        ("SetTexture", vec![f.device, 0, texture]),
        ("SetSamplerState", vec![f.device, 0, 1, 3]),
        ("SetSamplerState", vec![f.device, 0, 2, 3]),
        ("SetSamplerState", vec![f.device, 0, 5, 2]),
        ("SetSamplerState", vec![f.device, 0, 6, 2]),
        ("SetFVF", vec![f.device, 0x102]),
        ("SetRenderState", vec![f.device, 22, 1]),
        ("BeginScene", vec![f.device]),
        ("DrawPrimitiveUP", vec![f.device, 5, 2, quad, 20]),
        ("EndScene", vec![f.device]),
        ("SetTexture", vec![f.device, 0, 0]),
        ("SetVertexShader", vec![f.device, 0]),
        ("SetPixelShader", vec![f.device, 0]),
    ] {
        f.okay(&format!("IDirect3DDevice9::{name}"), &args);
    }
}
/// Runs `frame` for `count` presents with and without verification, after
/// `setup`, checks that clipped execution leaves the pixels a full replay
/// does, and returns the clipped run's counters after the first present and
/// at the end.
fn against_full_replay<T>(
    count: usize,
    setup: impl Fn(&mut Fixture) -> T,
    frame: impl Fn(&mut Fixture, &Composite, &T, usize),
) -> (Counters, Counters) {
    let mut outputs = Vec::new();
    let mut counters = Vec::new();
    for verify in [false, true] {
        let (mut f, c) = composite_fixture(verify);
        let resources = setup(&mut f);
        let mut frames = Vec::new();
        let mut first = None;
        for n in 0..count {
            frame(&mut f, &c, &resources, n);
            present(&mut f);
            frames.push(latest(&mut f));
            first.get_or_insert(f.m.ext.get::<State>().frames);
        }
        assert_eq!(f.m.ext.get::<State>().frames.verification_mismatches, 0);
        counters.push((first.unwrap(), f.m.ext.get::<State>().frames));
        outputs.push(frames);
    }
    for (n, (clipped, full)) in outputs[0].iter().zip(&outputs[1]).enumerate() {
        assert!(clipped == full, "frame {n} differs from a full replay");
    }
    counters[0]
}
/// Pixels submitted per present after the first, as a fraction of the
/// first, whole, present.
fn steady_share(first: &Counters, end: &Counters, presents: usize) -> f64 {
    (end.submitted_pixels - first.submitted_pixels) as f64
        / (presents - 1) as f64
        / first.submitted_pixels as f64
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn half_texel_composite_keeps_local_damage_and_full_replay_pixels() {
    let (first, end) = against_full_replay(4, layer, |f, c, &(t, l), n| {
        paint(f, l, 1., [1., n as f32 / 8., 0., 1.]);
        composite(f, c, t, f.back, c.full, true);
    });
    // The layer and the composite redraw only the moving quad's cells.
    assert_eq!(end.clipped_executions, 3, "{end:?}");
    assert!(steady_share(&first, &end, 4) < 0.25, "{end:?}");
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn removed_draw_before_changed_composite_keeps_damage_local() {
    let (first, end) = against_full_replay(3, layer, |f, c, &(texture, surface), n| {
        paint(f, surface, 1., [0., 0., 1., 1.]);
        composite(f, c, texture, f.back, c.full, true);
        if n == 0 {
            let text = quad(f, 8., [1.; 4]);
            record_draw(f, text);
        }
        paint(f, surface, 1., [1., n as f32 / 8., 0., 1.]);
        composite(f, c, texture, f.back, c.full, false);
    });
    assert_eq!(end.clipped_executions, 2, "{end:?}");
    assert!(steady_share(&first, &end, 3) < 0.5, "{end:?}");
}
// spec: GFX-REC-4 GFX-REC-5 GFX-COPY-3
#[test]
fn disjoint_uploads_before_sampling_preserve_both_patches() {
    against_full_replay(3, layer, |f, c, &(texture, surface), n| {
        let patches = match n {
            0 => vec![(
                Rect {
                    x: 0,
                    y: 0,
                    w: 64,
                    h: 48,
                },
                [0, 0, 0, 255],
            )],
            1 => vec![
                (
                    Rect {
                        x: 16,
                        y: 16,
                        w: 8,
                        h: 8,
                    },
                    [255, 0, 0, 255],
                ),
                (
                    Rect {
                        x: 40,
                        y: 16,
                        w: 8,
                        h: 8,
                    },
                    [0, 255, 0, 255],
                ),
            ],
            _ => vec![],
        };
        for (rectangle, color) in patches {
            let target = f.target(surface);
            let owner = id(f);
            let e = env(f);
            let resource = com(&e)
                .identity::<Payload>(&e.sys.objects, texture)
                .unwrap();
            let bytes = color.repeat((rectangle.w * rectangle.h) as usize);
            let key = hash(&bytes) as u64;
            assert!(matches!(
                record(
                    &mut env(f),
                    owner,
                    Operation::Upload {
                        resource,
                        level: Some(0),
                        target,
                        pixels: Pixels::Host(Arc::new(bytes), UploadKey::Admission(key)),
                        pitch: rectangle.w * 4,
                        rectangle,
                    }
                ),
                Outcome::Return(0)
            ));
        }
        composite(f, c, texture, f.back, c.full, true);
        let cursor = quad(f, 1. + n as f32, [1.; 4]);
        record_draw(f, cursor);
    });
}
// spec: GFX-REC-2 GFX-REC-4 GFX-REC-5
#[test]
fn repeated_full_upload_keeps_cursor_damage_local() {
    let (_, end) = against_full_replay(3, layer, |f, c, &(texture, surface), n| {
        let mut bytes = [0x33u8; 64 * 48 * 4];
        if n == 2 {
            bytes[0] = 255;
        }
        let address = f.packet(bytes);
        let target = f.target(surface);
        let owner = id(f);
        let e = env(f);
        let resource = com(&e)
            .identity::<Payload>(&e.sys.objects, texture)
            .unwrap();
        let state = f.m.ext.get_mut::<State>();
        let (pixels, pitch) = format::snapshot(
            &f.m.cpu,
            state.gpu.as_mut().unwrap(),
            &mut state.upload_admission,
            &mut state.upload_scratch,
            (21, address, 64 * 4),
            (64, 48),
            true,
        )
        .unwrap();
        let result = record(
            &mut env(f),
            owner,
            Operation::Upload {
                resource,
                level: Some(0),
                target,
                pixels,
                pitch,
                rectangle: Rect {
                    x: 0,
                    y: 0,
                    w: 64,
                    h: 48,
                },
            },
        );
        assert_eq!(f.finish(result), 0);
        composite(f, c, texture, f.back, c.full, true);
        let cursor = quad(f, 1. + n as f32, [1.; 4]);
        record_draw(f, cursor);
    });
    assert_eq!(end.clipped_executions, 1, "{end:?}");
    assert_eq!(end.submissions, 11, "{end:?}");
}
// spec: GFX-REC-2 GFX-REC-4 GFX-REC-5
#[test]
fn changing_copy_presentation_rectangles_keeps_render_damage_local() {
    let mut outputs = Vec::new();
    for verify in [false, true] {
        let mut f = fixture();
        f.m.ext.get_mut::<State>().verify_frames = verify;
        let pointer = f.device;
        let mut parameters = device(&env(&mut f), pointer).unwrap().parameters;
        parameters.SwapEffect = 3;
        let parameters = f.packet(parameters);
        f.okay("IDirect3DDevice9::Reset", &[pointer, parameters]);
        let full = f.packet([0u32, 0, 16, 12, 0, 1f32.to_bits()]);
        let changed = f.packet([2u32, 2, 2, 2, 0, 1f32.to_bits()]);
        let part = f.packet(RECT {
            left: 0,
            top: 0,
            right: 8,
            bottom: 8,
        });
        let mut frames = Vec::new();
        for n in 0..3 {
            let before = f.m.ext.get::<State>().frames;
            f.okay("IDirect3DDevice9::SetViewport", &[pointer, full]);
            clear(&mut f, 0xff000000);
            let color = if n == 0 { 0xff0000ff } else { 0xff00ff00 };
            f.okay("IDirect3DDevice9::SetViewport", &[pointer, changed]);
            clear(&mut f, color);
            let rectangle = if n == 1 { part } else { 0 };
            f.okay(
                "IDirect3DDevice9::Present",
                &[pointer, rectangle, rectangle, 0, 0],
            );
            frames.push(latest(&mut f));
            if !verify && n > 0 {
                let after = f.m.ext.get::<State>().frames;
                let area = after.executed_pixels - before.executed_pixels;
                assert_eq!(area, if n == 1 { 64 } else { 0 });
                assert_eq!(after.changed_pixels - before.changed_pixels, area);
                assert_eq!(after.eliminated_frames, 0);
            }
        }
        outputs.push(frames);
    }
    assert!(outputs[0] == outputs[1]);
}
// spec: GFX-REC-3 GFX-REC-5
#[test]
fn composite_counts_vertices_again_for_each_disjoint_scissor() {
    let (first, end) = against_full_replay(2, layer, |f, c, &(t, l), n| {
        let color = [1., n as f32 / 8., 0., 1.];
        paint(f, l, 1., color);
        let mut d = quad(f, 11., color);
        d.target = f.target(l);
        d.state = DrawState::new(64, 48);
        record_draw(f, d);
        composite(f, c, t, f.back, c.full, true);
    });
    // The four-vertex composition strip meets both scissors.
    assert_eq!(end.repeated_vertices - first.repeated_vertices, 4);
    assert!(end.executed_pixels - first.executed_pixels < 64 * 48);
}
// spec: GFX-REC-3 GFX-REC-5
#[test]
fn retained_intermediate_reads_count_their_own_whole_chain_promotions() {
    let (first, end) = against_full_replay(4, layer, |f, c, &(t, l), n| {
        paint(f, l, 1., [1., n as f32 / 8., 0., 1.]);
        composite(f, c, t, f.back, c.full, true);
        f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, l]);
        let mut d = quad(f, 11., [0., 0., 1., 1.]);
        d.target = f.target(l);
        record_draw(f, d);
        composite(f, c, t, f.back, c.left, false);
    });
    assert_eq!(first.intermediate_promotions, 0);
    assert_eq!(end.intermediate_promotions, 3);
    assert_eq!(end.promotions, 0);
}
/// A fixed-function quad on `target` testing the bound depth buffer.
fn tested(f: &mut Fixture, target: u32, x: f32, z: f32, write: bool, color: [f32; 4]) {
    let mut d = quad_deep(f, x, z, color);
    d.target = f.target(target);
    let device = f.device;
    d.target = depth::attach(&env(f), device, d.target).unwrap();
    d.state.depth = Some(Compare::Less);
    d.state.depth_write = write;
    record_draw(f, d);
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn layer_generations_serve_their_own_reads() {
    // The layer is drawn twice a frame, each time from a clear; the first
    // generation fills the target and the second, changing one, its left
    // half. Executing the first only where its read needs it, then the
    // second over it, keeps both reads exact.
    let (first, end) = against_full_replay(4, layer, |f, c, &(t, l), n| {
        paint(f, l, 6., [0., 0., 1., 1.]);
        composite(f, c, t, f.back, c.full, true);
        paint(f, l, 1., [1., n as f32 / 8., 0., 1.]);
        composite(f, c, t, f.back, c.left, false);
    });
    assert_eq!(end.promotions, 0);
    assert!(steady_share(&first, &end, 4) < 0.25, "{end:?}");
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn ping_pong_filters_follow_their_reads_and_in_place_reuse_runs_whole() {
    // A to B and back once: every version read is either transient or a
    // final one the slot holds, so execution stays local.
    let (first, end) = against_full_replay(
        4,
        |f| (layer(f), layer(f)),
        |f, c, &((a, sa), (b, sb)), n| {
            paint(f, sa, 1., [1., n as f32 / 8., 0., 1.]);
            composite(f, c, a, sb, c.full, true);
            composite(f, c, b, sa, c.full, true);
            composite(f, c, a, f.back, c.full, true);
        },
    );
    assert_eq!(end.promotions, 0);
    assert!(steady_share(&first, &end, 4) < 0.35, "{end:?}");
    // Twice: B's first version, written in place, costs B's final chain its
    // cells, whose inputs come from A's final chain, which in turn must cover
    // what A's first version wrote: the demand feeds back through shared
    // storage, so the chains run whole and pixels stay exact.
    let (_, end) = against_full_replay(
        3,
        |f| (layer(f), layer(f)),
        |f, c, &((a, sa), (b, sb)), n| {
            paint(f, sa, 1., [1., n as f32 / 8., 0., 1.]);
            composite(f, c, a, sb, c.full, true);
            composite(f, c, b, sa, c.full, true);
            composite(f, c, a, sb, c.full, true);
            composite(f, c, b, f.back, c.full, true);
        },
    );
    assert!(end.promotions > 0, "{end:?}");
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn shared_tested_depth_keeps_each_version_exact() {
    // The layer writes depth that the back buffer's quad then tests; the
    // layer itself is composited with linear filtering.
    let (first, end) = against_full_replay(
        4,
        |f| {
            bind_depth(f, 64, 48);
            layer(f)
        },
        |f, c, &(t, l), n| {
            f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, l]);
            f.okay(
                "IDirect3DDevice9::Clear",
                &[f.device, 0, 0, 3, 0xff000000, 0.75f32.to_bits(), 0],
            );
            tested(f, l, 1. + n as f32 * 0.25, 0.5, true, [1., 0., 0., 1.]);
            composite(f, c, t, f.back, c.full, true);
            tested(f, f.back, 2., 0.6, false, [1.; 4]);
        },
    );
    assert_eq!(end.promotions, 0);
    assert!(steady_share(&first, &end, 4) < 0.3, "{end:?}");
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn retained_depth_keeps_skipped_cells_for_later_tests() {
    // Depth is cleared and a static quad writes it, while another quad
    // changes elsewhere; the last frame stops clearing and tests against
    // the depth the skipped static quad left.
    against_full_replay(
        4,
        |f| bind_depth(f, 64, 48),
        |f, _, _, n| {
            let back = f.back;
            if n < 3 {
                f.okay(
                    "IDirect3DDevice9::Clear",
                    &[f.device, 0, 0, 3, 0xff000000, 0.75f32.to_bits(), 0],
                );
                tested(f, back, 1., 0.25, true, [0., 0., 1., 1.]);
                tested(f, back, 9., 0.25, true, [1., n as f32 / 8., 0., 1.]);
            } else {
                tested(f, back, 1.5, 0.5, false, [1.; 4]);
            }
        },
    );
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn a_final_version_read_before_any_overwrite_is_settled() {
    // Each frame rebuilds the layer twice from clears, until the last frame
    // reads it without drawing it at all.
    against_full_replay(4, layer, |f, c, &(t, l), n| {
        if n < 3 {
            paint(f, l, 1., [1., n as f32 / 8., 0., 1.]);
            composite(f, c, t, f.back, c.left, true);
            paint(f, l, 6., [0., n as f32 / 8., 1., 1.]);
        }
        composite(f, c, t, f.back, c.full, n == 3);
    });
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn a_fill_after_a_read_keeps_the_earlier_read_local() {
    // The layer is composited, then filled from the CPU and sampled again by
    // a small quad: each read binds its own version.
    let (first, end) = against_full_replay(
        4,
        |f| {
            let small = f.packet([
                [0.125f32, 0.5, 0., 36. / 64., 12. / 48.],
                [0.375, 0.5, 0., 44. / 64., 12. / 48.],
                [0.125, 1. / 6., 0., 36. / 64., 20. / 48.],
                [0.375, 1. / 6., 0., 44. / 64., 20. / 48.],
            ]);
            (layer(f), small)
        },
        |f, c, &((t, l), small), n| {
            paint(f, l, 1., [1., n as f32 / 8., 0., 1.]);
            composite(f, c, t, f.back, c.full, true);
            f.okay(
                "IDirect3DDevice9::ColorFill",
                &[f.device, l, 0, 0xff000010 + n as u32],
            );
            composite(f, c, t, f.back, small, false);
        },
    );
    assert_eq!(end.promotions, 0);
    // The fill itself runs whole, a quarter of the first present; the rest
    // only in the moving quad's and the small quad's cells.
    assert!(steady_share(&first, &end, 4) < 0.5, "{end:?}");
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn an_accumulating_layer_changes_only_where_this_frame_drew() {
    let (first, end) = against_full_replay(6, layer, |f, c, &(t, l), n| {
        f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, l]);
        if n == 0 {
            clear(f, 0xff000000);
        }
        let mut d = quad(f, 1. + 2. * n as f32, [1., n as f32 / 8., 0., 1.]);
        d.target = f.target(l);
        record_draw(f, d);
        composite(f, c, t, f.back, c.full, true);
    });
    assert_eq!(end.promotions, 0);
    assert!(steady_share(&first, &end, 6) < 0.1, "{end:?}");
}
// spec: GFX-REC-1 GFX-REC-4 GFX-REC-5
#[test]
fn readback_executes_only_what_the_observed_target_depends_on() {
    // A hit mask cleared with the shared depth is read back mid-frame,
    // between the generations of an unrelated layer the back buffer
    // composites; it is static, or changes every frame.
    for moving in [false, true] {
        readback_closure(moving);
    }
}
fn readback_closure(moving: bool) {
    let mut outputs = Vec::new();
    let mut counters = Vec::new();
    for verify in [false, true] {
        let (mut f, c) = composite_fixture(verify);
        bind_depth(&mut f, 64, 48);
        let (texture, l) = layer(&mut f);
        let mask = f.render_target(64, 48, 21);
        let system = f.surface(64, 48, 21, 2);
        let mut frames = Vec::new();
        let mut first = None;
        for n in 0..4 {
            paint(&mut f, l, 6., [0., 0., 1., 1.]);
            let back = f.back;
            composite(&mut f, &c, texture, back, c.full, true);
            f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, mask]);
            f.okay(
                "IDirect3DDevice9::Clear",
                &[f.device, 0, 0, 3, 0xff000000, 1f32.to_bits(), 0],
            );
            let x = if moving { 3. + n as f32 } else { 3. };
            let mut d = quad_deep(&mut f, x, 0., [1.; 4]);
            d.target = f.target(mask);
            record_draw(&mut f, d);
            f.okay(
                "IDirect3DDevice9::GetRenderTargetData",
                &[f.device, mask, system],
            );
            let locked = f.lock(system, 0, 0);
            let mut bytes = vec![0; 64 * 48 * 4];
            f.m.cpu.read(locked.pBits, &mut bytes).unwrap();
            f.okay("IDirect3DSurface9::UnlockRect", &[system]);
            paint(&mut f, l, 1., [1., n as f32 / 8., 0., 1.]);
            composite(&mut f, &c, texture, back, c.left, false);
            present(&mut f);
            frames.push((bytes, latest(&mut f)));
            first.get_or_insert(f.m.ext.get::<State>().frames);
        }
        let end = f.m.ext.get::<State>().frames;
        assert_eq!(end.verification_mismatches, 0);
        // An unchanged mask is read where it already is.
        let flushes = if verify || moving { 4 } else { 1 };
        assert_eq!(end.flushes[Reason::Readback as usize], flushes);
        counters.push((first.unwrap(), end));
        outputs.push(frames);
    }
    for (n, (clipped, full)) in outputs[0].iter().zip(&outputs[1]).enumerate() {
        assert!(clipped.0 == full.0, "frame {n} reads back other pixels");
        assert!(clipped.1 == full.1, "frame {n} differs from a full replay");
    }
    let (first, end) = counters[0];
    // The mask runs early, whole (its clear and quad), only while it
    // changes; the layer and the back buffer stay local.
    assert_eq!(end.promotions, 0);
    let mask = if moving { 2. * 64. * 48. } else { 0. };
    let rest = steady_share(&first, &end, 4) - mask / first.submitted_pixels as f64;
    assert!(rest < 0.25, "{rest} {end:?}");
}
// spec: GFX-REC-1 GFX-REC-3 GFX-REC-5
#[test]
fn verification_busy_readback_retains_the_presented_allocation() {
    let mut f = fixture();
    rotate(&mut f, 3);
    f.m.ext.get_mut::<State>().verify_frames = true;
    let colors = [[1., 0., 0., 1.], [0., 1., 0., 1.], [0., 0., 1., 1.]];
    scene(&mut f, colors);
    present(&mut f);
    let gpu = f.m.ext.get_mut::<State>().gpu.as_mut().unwrap();
    gpu.set_readback_limit(1).unwrap();
    let other = gpu.target(2, 2, false).unwrap();
    let mut blocked = gpu.readback(&other.image, None).unwrap();
    blocked.take_completion_job().unwrap()();
    scene(&mut f, [[0.5, 0., 0., 1.], colors[1], colors[2]]);
    let outcome = f.call("IDirect3DDevice9::Present", &[f.device, 0, 0, 0, 0]);
    // Usually the verification transfer is still pending here, but a fast GPU
    // (Adreno) can finish it first; either way the rotated allocation is read.
    assert_ne!(f.m.ext.get::<Rotation>().next, 0);
    drop(blocked);
    assert_eq!(f.finish(outcome), 0);
    assert_eq!(f.m.ext.get::<State>().frames.verification_mismatches, 0);
    assert_eq!(
        latest(&mut f),
        reference(&mut f, [[0.5, 0., 0., 1.], colors[1], colors[2]])
    );
}

fn sample_binding(f: &mut Fixture, surface: Option<u32>) {
    let resource = surface.map(|s| {
        let e = env(f);
        com(&e).identity::<Payload>(&e.sys.objects, s).unwrap()
    });
    let owner = id(f);
    let e = env(f);
    let Some(Payload::Device(d)) = com(&e).get_identity_mut(&mut e.sys.objects, owner) else {
        unreachable!()
    };
    d.texture = resource;
    d.versions[12] += 1;
}

// spec: GFX-REC-4 GFX-REC-5
#[test]
fn local_damage_crosses_scaled_copy_and_fixed_or_shader_composition_exactly() {
    for shader in [false, true] {
        for flipped in [false, true] {
            let mut outputs = Vec::new();
            for verify in [false, true] {
                let mut f = fixture();
                f.m.ext.get_mut::<State>().verify_frames = verify;
                rotate(&mut f, 3);
                let source = f.render_target(16, 12, 21);
                let middle = f.render_target(32, 24, 21);
                let target =
                    f.m.ext
                        .get::<State>()
                        .gpu
                        .as_ref()
                        .unwrap()
                        .target_levels(32, 24, 6)
                        .unwrap()
                        .with_alpha_mode(f.image(middle).alpha_mode());
                let surface = {
                    let e = env(&mut f);
                    let identity = com(&e).identity::<Payload>(&e.sys.objects, middle).unwrap();
                    let Some(Payload::Resource(r)) = com(&e).get_identity(&e.sys.objects, identity)
                    else {
                        unreachable!()
                    };
                    r.surface.unwrap()
                };
                (f.m.ext.get::<State>().provider.replace)(&mut f.m.ext, surface, target).unwrap();
                let output = f.packet(0u32);
                // POSITIONT must bypass this bound shader, whose output has w=0.
                let words = f.packet([0xfffe0200u32, 0x02000001, 0xc00f0000, 0xa0e40000, 0xffff]);
                f.okay(
                    "IDirect3DDevice9::CreateVertexShader",
                    &[f.device, words, output],
                );
                let unused_vertex = f.m.cpu.read_u32(output).unwrap();
                let mut frames = Vec::new();
                let ps = if shader {
                    let output = f.packet(0u32);
                    let words = f.packet([
                        0xffff0200u32,
                        0x0200001f,
                        0x90000000,
                        0xa00f0800,
                        0x03000042,
                        0x800f0000,
                        0xb0e40000,
                        0xa0e40800,
                        0x02000001,
                        0x800f0800,
                        0x80e40000,
                        0xffff,
                    ]);
                    f.okay(
                        "IDirect3DDevice9::CreatePixelShader",
                        &[f.device, words, output],
                    );
                    f.m.cpu.read_u32(output).unwrap()
                } else {
                    0
                };
                for n in 0..10 {
                    f.okay("IDirect3DDevice9::SetPixelShader", &[f.device, 0]);
                    sample_binding(&mut f, None);
                    f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, source]);
                    clear(&mut f, 0xff112233);
                    if n != 7 {
                        let mut q = quad(
                            &mut f,
                            if n < 5 { 2. } else { 4. },
                            [1., n as f32 / 10., 0., 1.],
                        );
                        q.target = f.target(source);
                        record_draw(&mut f, q);
                    }
                    let owner = id(&mut f);
                    let resources = {
                        let e = env(&mut f);
                        [source, middle]
                            .map(|s| com(&e).identity::<Payload>(&e.sys.objects, s).unwrap())
                    };
                    let operation = Operation::Copy {
                        source: f.image(source),
                        rectangle: Rect {
                            x: 0,
                            y: 0,
                            w: 16,
                            h: 12,
                        },
                        destination: f.target(middle),
                        region: Rect {
                            x: 0,
                            y: 0,
                            w: 32,
                            h: 24,
                        },
                        filter: Filter::Linear,
                        resources,
                    };
                    assert!(matches!(
                        record(&mut env(&mut f), owner, operation),
                        Outcome::Return(0)
                    ));
                    f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, f.back]);
                    clear(&mut f, 0xff000000);
                    sample_binding(&mut f, Some(middle));
                    let mut q = draw(&mut f);
                    q.state.texture_op = TextureOp::Texture;
                    q.state.sampler.u = adv_device::gpu::Address::Clamp;
                    q.state.sampler.v = adv_device::gpu::Address::Clamp;
                    q.state.sampler.min = Filter::Linear;
                    q.state.sampler.mag = Filter::Linear;
                    q.image = Some(f.image(middle));
                    q.vertices = [
                        (0., 0.),
                        (16., 0.),
                        (0., 12.),
                        (0., 12.),
                        (16., 0.),
                        (16., 12.),
                    ]
                    .map(|(x, y)| Vertex {
                        position: [x / 8. - 1., 1. - y / 6., 0., 1.],
                        color: [1.; 4],
                        uv: [if flipped { 1. - x / 16. } else { x / 16. }, y / 12.],
                    })
                    .to_vec()
                    .into();
                    if shader {
                        f.okay("IDirect3DDevice9::SetPixelShader", &[f.device, ps]);
                        f.okay(
                            "IDirect3DDevice9::SetVertexShader",
                            &[f.device, unused_vertex],
                        );
                        f.okay("IDirect3DDevice9::SetFVF", &[f.device, 0x144]);
                        f.okay("IDirect3DDevice9::SetRenderState", &[f.device, 22, 1]);
                        f.okay("IDirect3DDevice9::SetRenderState", &[f.device, 7, 0]);
                        f.okay("IDirect3DDevice9::SetSamplerState", &[f.device, 0, 1, 3]);
                        f.okay("IDirect3DDevice9::SetSamplerState", &[f.device, 0, 2, 3]);
                        f.okay("IDirect3DDevice9::SetSamplerState", &[f.device, 0, 5, 2]);
                        f.okay("IDirect3DDevice9::SetSamplerState", &[f.device, 0, 6, 2]);
                        let raw: Vec<[u32; 7]> = q
                            .vertices
                            .iter()
                            .map(|v| {
                                [
                                    ((v.position[0] + 1.) * 8. - 0.5).to_bits(),
                                    ((1. - v.position[1]) * 6. - 0.5).to_bits(),
                                    0,
                                    1f32.to_bits(),
                                    0xffffffff,
                                    v.uv[0].to_bits(),
                                    v.uv[1].to_bits(),
                                ]
                            })
                            .collect();
                        let raw = f.packet(<[[u32; 7]; 6]>::try_from(raw).unwrap());
                        f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
                        f.okay(
                            "IDirect3DDevice9::DrawPrimitiveUP",
                            &[f.device, 4, 2, raw, 28],
                        );
                        f.okay("IDirect3DDevice9::EndScene", &[f.device]);
                    } else {
                        record_draw(&mut f, q);
                    }
                    present(&mut f);
                    frames.push(latest(&mut f));
                    // Each intermediate hop rounds to 8x8 cells, which on this
                    // 2x2-cell target reach everything; the tracker's unit tests
                    // cover locality. Verification checks the damage covers every
                    // change.
                }
                assert_eq!(f.m.ext.get::<State>().frames.verification_mismatches, 0);
                outputs.push(frames);
            }
            assert_eq!(outputs[0], outputs[1], "shader={shader} flip={flipped}");
        }
    }
}
/// A `w`×`h` quad at `x`,`y` in the 16×12 fixture target.
fn quad_at(f: &mut Fixture, (x, y, w, h): (f32, f32, f32, f32), color: [f32; 4]) -> fixed::Draw {
    let mut d = draw(f);
    d.vertices = [
        (x, y),
        (x + w, y),
        (x, y + h),
        (x, y + h),
        (x + w, y),
        (x + w, y + h),
    ]
    .map(|(x, y)| Vertex {
        position: [x / 8. - 1., 1. - y / 6., 0., 1.],
        color,
        uv: [0.; 2],
    })
    .to_vec()
    .into();
    d
}
// spec: GFX-REC-4 GFX-REC-5
#[test]
fn distant_changes_replay_in_separate_scissors_without_losing_unchanged_draws_between() {
    // Two diagonal cells change; the cell between them holds an unchanged
    // draw that meets neither. One scissor around both would make the clear
    // erase it unless it ran too; each scissor alone leaves it untouched.
    let quads = [(1., 1., 2., 2.), (10., 1., 2., 2.), (10., 9., 2., 2.)];
    let mut colors = [[1., 0., 0., 1.], [0., 1., 0., 1.], [0., 0., 1., 1.]];
    let mut f = fixture();
    let frame = |f: &mut Fixture, colors: [[f32; 4]; 3]| {
        clear(f, 0xff000000);
        for (q, color) in quads.into_iter().zip(colors) {
            let d = quad_at(f, q, color);
            record_draw(f, d);
        }
        present(f);
    };
    frame(&mut f, colors);
    colors[0] = [1., 1., 0., 1.];
    colors[2] = [1., 1., 1., 1.];
    frame(&mut f, colors);

    let target =
        f.m.ext
            .get::<State>()
            .gpu
            .as_ref()
            .unwrap()
            .target(16, 12, false)
            .unwrap()
            .with_alpha_mode(AlphaMode::Opaque);
    let gpu = f.m.ext.get_mut::<State>().gpu.as_mut().unwrap();
    gpu.clear(&target, Some([0., 0., 0., 1.]), None).unwrap();
    for (q, color) in quads.into_iter().zip(colors) {
        let d = quad_at(&mut f, q, color);
        let gpu = f.m.ext.get_mut::<State>().gpu.as_mut().unwrap();
        gpu.draw(&target, &d.vertices, None, d.state).unwrap();
    }
    assert_eq!(latest(&mut f), pixels(&mut f, &target.image));
    let counts = f.m.ext.get::<State>().frames;
    assert_eq!(counts.clipped_executions, 1);
    // The 8x8 top-left and 8x4 bottom-right cells of the 192-pixel target.
    assert!((counts.clipped_area_sum - (1. + 96. / 192.)).abs() < 1e-9);
}
