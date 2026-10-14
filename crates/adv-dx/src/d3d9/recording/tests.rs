use super::super::stretch::tests::Fixture;
use super::*;
use adv_cpu::backend::Guest;
use adv_device::gpu::*;
use adv_device::surface::Rect;

fn env(f: &mut Fixture) -> Env<'_> {
    Env {
        context: f.m.sys.current,
        cpu: &mut f.m.cpu,
        sys: &mut f.m.sys,
        ext: &mut f.m.ext,
        thunk: 0,
    }
}
fn id(f: &mut Fixture) -> Id {
    let pointer = f.device;
    let e = env(f);
    com(&e)
        .identity::<Payload>(&e.sys.objects, pointer)
        .unwrap()
}
#[derive(Default)]
struct Presents {
    serial: u64,
    published: Vec<u64>,
    latest: Option<NativeFrame>,
    fail: bool,
    detached: bool,
}
fn fixture() -> Fixture {
    fixture_with_limits(adv_machine::ObjectLimits::default())
}
fn fixture_with_limits(limits: adv_machine::ObjectLimits) -> Fixture {
    let mut f = Fixture::with_limits(limits);
    f.m.ext.insert(Presents::default());
    let state = f.m.ext.get_mut::<State>();
    state.provider.present_copy = |ext, surface, gpu| {
        if ext.get::<Presents>().fail {
            return Err(Error::Device("authored presentation failure".into()));
        }
        let image = (ext.get::<State>().provider.target)(ext, surface)
            .unwrap()
            .image;
        let serial = ext.get::<Presents>().serial + 1;
        let frame = gpu.present(&image, serial)?;
        let p = ext.get_mut::<Presents>();
        p.serial = serial;
        if !p.detached {
            p.published.push(serial);
        }
        p.latest = Some(frame);
        Ok(serial)
    };
    state.provider.repeat = |ext, _| {
        let p = ext.get_mut::<Presents>();
        p.serial += 1;
        Ok(p.serial)
    };
    state.provider.presentation_serial = |ext| {
        let p = ext.get::<Presents>();
        (!p.detached).then_some(p.serial)
    };
    state.provider.completed = |env, serial| {
        env.sys.frame = serial as u32;
        env.sys.complete_frame();
        None
    };
    f
}
fn present(f: &mut Fixture) {
    f.okay("IDirect3DDevice9::Present", &[f.device, 0, 0, 0, 0]);
}
fn clear(f: &mut Fixture, color: u32) {
    f.okay(
        "IDirect3DDevice9::Clear",
        &[f.device, 0, 0, 1, color, 1f32.to_bits(), 0],
    );
}
fn flush(f: &mut Fixture, reason: Reason) -> Outcome {
    let owner = id(f);
    barrier(&mut env(f), owner, reason, |_| Outcome::Return(0))
}
fn draw(f: &mut Fixture) -> fixed::Draw {
    let target = f.target(f.back);
    fixed::Draw {
        target,
        image: None,
        vertices: vec![
            Vertex {
                position: [-1., -1., 0., 1.],
                color: [0.25; 4],
                uv: [0.; 2],
            },
            Vertex {
                position: [1., -1., 0., 1.],
                color: [0.25; 4],
                uv: [0.; 2],
            },
            Vertex {
                position: [-1., 1., 0., 1.],
                color: [0.25; 4],
                uv: [0.; 2],
            },
        ],
        texcoords: None,
        state: DrawState::new(16, 12),
        programmable: None,
        recording_key: 0,
        program_key: 0,
    }
}
fn record_draw(f: &mut Fixture, d: fixed::Draw) {
    let owner = id(f);
    assert!(matches!(
        record(&mut env(f), owner, Operation::Draw(d)),
        Outcome::Return(0)
    ));
}
fn stats(f: &Fixture) -> Stats {
    f.m.ext.get::<State>().gpu.as_ref().unwrap().stats()
}

// spec: GFX-REC-1 GFX-REC-2
#[test]
fn equal_full_clear_frames_skip_gpu_and_publication_but_complete_each_guest_frame() {
    let mut f = fixture();
    let initial = stats(&f);
    clear(&mut f, 0xff123456);
    assert_eq!(stats(&f), initial);
    present(&mut f);
    let once = stats(&f);
    for _ in 0..3 {
        clear(&mut f, 0xff123456);
        present(&mut f);
    }
    assert_eq!(stats(&f), once);
    let p = f.m.ext.get::<Presents>();
    assert_eq!(p.published, [1]);
    assert_eq!(p.serial, 4);
    assert_eq!(f.m.sys.frame, 4);
    let counts = f.m.ext.get::<State>().frames;
    assert_eq!(counts.recorded_commands, 4);
    assert_eq!(counts.eliminated_frames, 3);
    assert_eq!(counts.flushes[Reason::Present as usize], 4);
    clear(&mut f, 0xffabcdef);
    present(&mut f);
    assert_eq!(f.m.ext.get::<Presents>().published, [1, 5]);
    let back = f.image(f.back);
    assert_eq!(&f.pixels(&back)[..4], &[0xef, 0xcd, 0xab, 0xff]);
}

// spec: GFX-REC-1
#[test]
fn render_target_readback_and_gpu_lock_flush_prior_draws() {
    let mut f = fixture();
    clear(&mut f, 0xff123456);
    let d = draw(&mut f);
    record_draw(&mut f, d);
    let system = f.surface(16, 12, 22, 2);
    f.okay(
        "IDirect3DDevice9::GetRenderTargetData",
        &[f.device, f.back, system],
    );
    let locked = f.lock(system, 0, 0);
    let mut bytes = vec![0; 16 * 12 * 4];
    f.m.cpu.read(locked.pBits, &mut bytes).unwrap();
    assert!(bytes.chunks_exact(4).any(|v| v[..3] == [64, 64, 64]));
    f.okay("IDirect3DSurface9::UnlockRect", &[system]);
    let default = f.surface(16, 12, 21, 0);
    let target = f.target(default);
    let owner = id(&mut f);
    let resource = {
        let e = env(&mut f);
        com(&e)
            .identity::<Payload>(&e.sys.objects, default)
            .unwrap()
    };
    let op = Operation::Upload {
        resource,
        level: None,
        target,
        pixels: Arc::new(vec![7; 16 * 12 * 4]),
        pitch: 64,
        rectangle: Rect {
            x: 0,
            y: 0,
            w: 16,
            h: 12,
        },
    };
    assert!(matches!(
        record(&mut env(&mut f), owner, op),
        Outcome::Return(0)
    ));
    let locked = f.lock(default, 0, 0);
    let mut pixel = [0; 4];
    f.m.cpu.read(locked.pBits, &mut pixel).unwrap();
    assert_eq!(pixel, [7; 4]);
    assert!(f.m.ext.get::<State>().frames.flushes[Reason::Readback as usize] > 0);
    assert!(f.m.ext.get::<State>().frames.flushes[Reason::Lock as usize] > 0);
}

// spec: GFX-REC-1
#[test]
fn busy_resumes_the_unexecuted_suffix_once_and_cancellation_releases_flush_ownership() {
    for cancel in [false, true] {
        let mut f = fixture();
        let rt = f.render_target(16, 12, 21);
        let frozen = {
            let image = f.image(rt);
            f.m.ext
                .get_mut::<State>()
                .gpu
                .as_mut()
                .unwrap()
                .completed_frame(image, 1)
                .unwrap()
        };
        let d = draw(&mut f);
        record_draw(&mut f, d);
        f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, rt]);
        clear(&mut f, 0xff112233);
        let before = stats(&f).draws;
        let outcome = flush(&mut f, Reason::Readback);
        assert!(matches!(outcome, Outcome::Block { .. }));
        assert_eq!(stats(&f).draws, before + 1);
        let owner = id(&mut f);
        assert_eq!(frame(&mut env(&mut f), owner).head, 1);
        if cancel {
            super::super::cancel_pending(&mut f.m);
            drop(outcome);
            assert!(frame(&mut env(&mut f), owner).active.is_none());
            drop(frozen);
            let outcome = flush(&mut f, Reason::Readback);
            assert_eq!(f.finish(outcome), 0);
        } else {
            drop(frozen);
            assert_eq!(f.finish(outcome), 0);
        }
        assert_eq!(stats(&f).draws, before + 1);
        let image = f.image(rt);
        assert_eq!(&f.pixels(&image)[..4], &[0x33, 0x22, 0x11, 0xff]);
        assert!(f.m.ext.get::<State>().pending.is_empty());
    }
}

// spec: GFX-REC-2
#[test]
fn overlay_without_clear_and_sampled_upload_versions_are_not_eliminated() {
    let mut f = fixture();
    for _ in 0..2 {
        let mut d = draw(&mut f);
        d.state.blend = Some(Blend {
            source: Factor::One,
            destination: Factor::One,
            operation: BlendOp::Add,
        });
        record_draw(&mut f, d);
        present(&mut f);
    }
    assert_eq!(f.m.ext.get::<State>().frames.eliminated_frames, 0);
    let source = f.surface(16, 12, 21, 0);
    for _ in 0..2 {
        let locked = f.lock(source, 0, 0);
        f.m.cpu
            .write(locked.pBits, &vec![128; 16 * 12 * 4])
            .unwrap();
        f.okay("IDirect3DSurface9::UnlockRect", &[source]);
        clear(&mut f, 0xff000000);
        f.okay(
            "IDirect3DDevice9::StretchRect",
            &[f.device, source, 0, f.back, 0, 0],
        );
        present(&mut f);
    }
    assert_eq!(f.m.ext.get::<State>().frames.eliminated_frames, 0);
    assert_eq!(f.m.ext.get::<Presents>().published.len(), 4);
}

// spec: GFX-REC-3
#[test]
fn verification_executes_repeats_async_and_counts_real_pixel_mismatches() {
    let mut f = fixture();
    f.m.ext.get_mut::<State>().verify_frames = true;
    for _ in 0..2 {
        clear(&mut f, 0xff123456);
        present(&mut f);
    }
    assert_eq!(f.m.ext.get::<Presents>().published, [1, 2]);
    assert_eq!(stats(&f).readbacks, 2);
    assert_eq!(f.m.ext.get::<State>().frames.eliminated_frames, 0);
    assert_eq!(f.m.ext.get::<State>().frames.verification_mismatches, 0);
    // Corrupt the retained reference only: the diagnostic must detect it, rather
    // than merely asserting that two copies of its own hash agree.
    let owner = id(&mut f);
    frame(&mut env(&mut f), owner).previous.as_mut().unwrap().2[0] ^= 1;
    clear(&mut f, 0xff123456);
    present(&mut f);
    assert_eq!(f.m.ext.get::<State>().frames.verification_mismatches, 1);
    assert!(frame(&mut env(&mut f), owner).readback.is_none());
    assert!(f.m.ext.get::<State>().pending.is_empty());
}

// spec: GFX-REC-1
#[test]
fn invalid_commands_return_synchronously_and_device_loss_is_sticky_until_reset() {
    let mut f = fixture();
    let pause =
        f.m.ext
            .get::<State>()
            .gpu
            .as_ref()
            .unwrap()
            .context()
            .pause_submissions()
            .unwrap();
    let invalid = f.call(
        "IDirect3DDevice9::Clear",
        &[f.device, 0, 0, 1, 0, f32::NAN.to_bits(), 0],
    );
    // Z is unused by color-only Clear, preserving the native return value.
    assert!(matches!(invalid, Outcome::Return(0)));
    let mut d = draw(&mut f);
    d.state.viewport.width = 32.;
    let owner = id(&mut f);
    assert!(matches!(
        record(&mut env(&mut f), owner, Operation::Draw(d)),
        Outcome::Return(INVALID)
    ));
    drop(pause);
    f.m.ext.get_mut::<Presents>().fail = true;
    let outcome = f.call("IDirect3DDevice9::Present", &[f.device, 0, 0, 0, 0]);
    assert_eq!(f.finish(outcome), DEVICE_LOST);
    let pointer = f.device;
    assert!(device(&env(&mut f), pointer).unwrap().gpu_lost);
    let outcome = f.call("IDirect3DDevice9::TestCooperativeLevel", &[f.device]);
    assert_eq!(f.finish(outcome), DEVICE_LOST);
}

// spec: GFX-REC-1
#[test]
fn command_and_byte_limits_flush_complete_ordered_work_and_disable_partial_frame_elision() {
    let mut f = fixture();
    let pause =
        f.m.ext
            .get::<State>()
            .gpu
            .as_ref()
            .unwrap()
            .context()
            .pause_submissions()
            .unwrap();
    for _ in 0..4095 {
        clear(&mut f, 0xff123456)
    }
    assert_eq!(stats(&f).submissions, 0);
    let outcome = f.call(
        "IDirect3DDevice9::Clear",
        &[f.device, 0, 0, 1, 0xff123456, 1f32.to_bits(), 0],
    );
    assert!(matches!(outcome, Outcome::Block { .. }));
    drop(pause);
    assert_eq!(f.finish(outcome), 0);
    let owner = id(&mut f);
    assert!(frame(&mut env(&mut f), owner).commands.is_empty());
    present(&mut f);
    assert_eq!(f.m.ext.get::<State>().frames.eliminated_frames, 0);
    // Seed the accumulated byte count near its bound without allocating a huge
    // fixture; the next real vertex payload must trigger exactly one flush.
    frame(&mut env(&mut f), owner).bytes = 64 * 1024 * 1024 - 1;
    let d = draw(&mut f);
    record_draw(&mut f, d);
    assert!(frame(&mut env(&mut f), owner).commands.is_empty());
    assert_eq!(
        f.m.ext.get::<State>().frames.flushes[Reason::Limit as usize],
        2
    );
}

// spec: GFX-REC-2
#[test]
fn fingerprints_cover_every_fixed_and_raster_field_including_float_bits() {
    let fixed = fixed::Fixed::new(16, 12);
    let original = fixed_key(&fixed);
    macro_rules! fixed_change {
        ($body:expr) => {{
            let mut changed = fixed.clone();
            ($body)(&mut changed);
            assert_ne!(fixed_key(&changed), original);
        }};
    }
    fixed_change!(|f: &mut fixed::Fixed| f.fvf ^= 1);
    for n in fixed.render.keys() {
        fixed_change!(|f: &mut fixed::Fixed| *f.render.get_mut(n).unwrap() ^= 1);
    }
    for n in fixed.stage.keys() {
        fixed_change!(|f: &mut fixed::Fixed| *f.stage.get_mut(n).unwrap() ^= 1);
    }
    for slot in 0..8 {
        for n in fixed.stage_slots[slot].keys() {
            fixed_change!(|f: &mut fixed::Fixed| *f.stage_slots[slot].get_mut(n).unwrap() ^= 1);
        }
    }
    for n in fixed.sampler.keys() {
        fixed_change!(|f: &mut fixed::Fixed| *f.sampler.get_mut(n).unwrap() ^= 1);
    }
    for slot in 0..20 {
        for n in fixed.sampler_slots[slot].keys() {
            fixed_change!(|f: &mut fixed::Fixed| *f.sampler_slots[slot].get_mut(n).unwrap() ^= 1);
        }
    }
    fixed_change!(|f: &mut fixed::Fixed| f.viewport.X += 1);
    fixed_change!(|f: &mut fixed::Fixed| f.viewport.Y += 1);
    fixed_change!(|f: &mut fixed::Fixed| f.viewport.Width += 1);
    fixed_change!(|f: &mut fixed::Fixed| f.viewport.Height += 1);
    fixed_change!(|f: &mut fixed::Fixed| f.viewport.MinZ = -0.0);
    fixed_change!(|f: &mut fixed::Fixed| f.viewport.MaxZ = 0.5);
    fixed_change!(|f: &mut fixed::Fixed| f.scissor.left += 1);
    fixed_change!(|f: &mut fixed::Fixed| f.scissor.top += 1);
    fixed_change!(|f: &mut fixed::Fixed| f.scissor.right += 1);
    fixed_change!(|f: &mut fixed::Fixed| f.scissor.bottom += 1);
    for m in 0..3 {
        for n in 0..16 {
            fixed_change!(|f: &mut fixed::Fixed| f.transforms[m][n] =
                f32::from_bits(f.transforms[m][n].to_bits() ^ 1));
        }
    }
    for m in 0..6 {
        for n in 0..4 {
            fixed_change!(|f: &mut fixed::Fixed| f.clip_planes[m][n] =
                f32::from_bits(0x7fc00000 + n as u32 + 1));
        }
    }
    let state = DrawState::new(16, 12);
    let original = key::raster(state);
    macro_rules! raster_change {
        ($field:ident,$value:expr) => {{
            let mut changed = state;
            changed.$field = $value;
            assert_ne!(key::raster(changed), original, stringify!($field));
        }};
    }
    raster_change!(topology, Topology::Points);
    raster_change!(last_pixel, false);
    raster_change!(cull, Cull::Clockwise);
    let blend = Blend {
        source: Factor::One,
        destination: Factor::One,
        operation: BlendOp::Add,
    };
    raster_change!(blend, Some(blend));
    raster_change!(alpha_blend, Some(blend));
    raster_change!(depth, Some(Compare::Always));
    raster_change!(depth_write, true);
    raster_change!(stencil_reference, 1);
    let face = StencilFace {
        compare: Compare::Always,
        fail: StencilOp::Keep,
        depth_fail: StencilOp::Keep,
        pass: StencilOp::Keep,
    };
    raster_change!(
        stencil,
        Some(Stencil {
            front: face,
            back: face,
            read_mask: 255,
            write_mask: 255
        })
    );
    for n in 0..6 {
        let mut changed = state;
        let v = &mut changed.viewport;
        match n {
            0 => v.x = -0.0,
            1 => v.y = -0.0,
            2 => v.width = 15.,
            3 => v.height = 11.,
            4 => v.minimum_depth = -0.0,
            _ => v.maximum_depth = 0.5,
        };
        assert_ne!(key::raster(changed), original);
    }
    raster_change!(
        scissor,
        Some(Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1
        })
    );
    for n in 0..7 {
        let mut changed = state;
        let s = &mut changed.sampler;
        match n {
            0 => s.min = Filter::Linear,
            1 => s.mag = Filter::Linear,
            2 => s.mip = Some(Filter::Nearest),
            3 => s.first_level = 1,
            4 => s.u = Address::Repeat,
            5 => s.v = Address::Repeat,
            _ => s.border_color = 1,
        };
        assert_ne!(key::raster(changed), original);
    }
    raster_change!(texture_op, TextureOp::Multiply);
    raster_change!(alpha_texture_op, TextureOp::Multiply);
    raster_change!(color_write, 7);
    raster_change!(alpha_test, Compare::Less);
    raster_change!(alpha_reference, -0.0);
    // Nested Hash-derived blend/stencil fields also remain individually covered.
    let mut s = state;
    s.blend = Some(blend);
    s.alpha_blend = Some(blend);
    s.stencil = Some(Stencil {
        front: face,
        back: face,
        read_mask: 255,
        write_mask: 255,
    });
    let baseline = key::raster(s);
    for alpha in [false, true] {
        for n in 0..3 {
            let mut c = s;
            let b = if alpha {
                c.alpha_blend.as_mut().unwrap()
            } else {
                c.blend.as_mut().unwrap()
            };
            match n {
                0 => b.source = Factor::Zero,
                1 => b.destination = Factor::Zero,
                _ => b.operation = BlendOp::Subtract,
            };
            assert_ne!(key::raster(c), baseline);
        }
    }
    for n in 0..10 {
        let mut c = s;
        let t = c.stencil.as_mut().unwrap();
        match n {
            0 => t.read_mask = 127,
            1 => t.write_mask = 127,
            2 => t.front.compare = Compare::Never,
            3 => t.front.fail = StencilOp::Zero,
            4 => t.front.depth_fail = StencilOp::Zero,
            5 => t.front.pass = StencilOp::Zero,
            6 => t.back.compare = Compare::Never,
            7 => t.back.fail = StencilOp::Zero,
            8 => t.back.depth_fail = StencilOp::Zero,
            _ => t.back.pass = StencilOp::Zero,
        };
        assert_ne!(key::raster(c), baseline);
    }
}
fn fingerprint(f: &mut Fixture, d: fixed::Draw) -> u64 {
    let owner = id(f);
    let e = env(f);
    let p = key::prepare(&e, owner, Operation::Draw(d)).unwrap();
    hash((p.key, p.reads, p.writes))
}
fn programmable_draw(f: &mut Fixture) -> fixed::Draw {
    let mut d = draw(f);
    let mut state = d.state;
    state.texture_op = TextureOp::Texture;
    d.programmable = Some(ProgrammableDraw {
        program: Program::new(
            Arc::from("authored immutable program identity"),
            vec![false],
        ),
        streams: vec![VertexStream {
            bytes: vec![0; 96],
            stride: 16,
            attributes: vec![Attribute {
                location: 0,
                offset: 0,
                format: AttributeFormat::Float4,
            }],
        }],
        indices: Some(vec![0, 1, 2]),
        vertex_count: 6,
        vertex_constants: ConstantBlock::default(),
        pixel_constants: ConstantBlock::default(),
        textures: vec![ProgramTexture {
            image: None,
            sampler: state.sampler,
        }],
        parameters: [[0; 4]; 40],
    });
    d.program_key = 1;
    d
}
// spec: GFX-REC-2
#[test]
fn fingerprints_cover_program_constants_streams_indices_bindings_and_raw_vertices() {
    let mut f = fixture();
    let d = programmable_draw(&mut f);
    let original = fingerprint(&mut f, d);
    macro_rules! changed {
        ($body:expr) => {{
            let mut d = programmable_draw(&mut f);
            ($body)(&mut d);
            assert_ne!(fingerprint(&mut f, d), original);
        }};
    }
    changed!(|d: &mut fixed::Draw| d.recording_key = 1);
    changed!(|d: &mut fixed::Draw| d.program_key = 2);
    for n in 0..10 {
        changed!(|d: &mut fixed::Draw| {
            let v = &mut d.vertices[0];
            if n < 4 {
                v.position[n] = f32::from_bits(v.position[n].to_bits() ^ 1)
            } else if n < 8 {
                v.color[n - 4] = -0.0
            } else {
                v.uv[n - 8] = -0.0
            }
        });
    }
    changed!(|d: &mut fixed::Draw| d.texcoords = Some((1, vec![[0.; 4]; 3])));
    let extra = {
        let mut d = programmable_draw(&mut f);
        d.texcoords = Some((1, vec![[0.; 4]; 3]));
        fingerprint(&mut f, d)
    };
    for n in 0..4 {
        let mut d = programmable_draw(&mut f);
        let mut v = vec![[0.; 4]; 3];
        v[0][n] = -0.0;
        d.texcoords = Some((1, v));
        assert_ne!(fingerprint(&mut f, d), extra);
    }
    changed!(|d: &mut fixed::Draw| d.programmable.as_mut().unwrap().program.textures[0] = true);
    changed!(|d: &mut fixed::Draw| d.programmable.as_mut().unwrap().vertex_count = 5);
    changed!(
        |d: &mut fixed::Draw| d.programmable.as_mut().unwrap().indices.as_mut().unwrap()[2] = 3
    );
    changed!(|d: &mut fixed::Draw| d.programmable.as_mut().unwrap().streams[0].bytes[0] = 1);
    changed!(|d: &mut fixed::Draw| d.programmable.as_mut().unwrap().streams[0].stride = 0);
    changed!(
        |d: &mut fixed::Draw| d.programmable.as_mut().unwrap().streams[0].attributes[0].location =
            1
    );
    changed!(|d: &mut fixed::Draw| {
        let s = &mut d.programmable.as_mut().unwrap().streams[0];
        s.attributes[0].format = AttributeFormat::Float3;
        s.attributes[0].offset = 4;
    });
    changed!(
        |d: &mut fixed::Draw| d.programmable.as_mut().unwrap().streams[0].attributes[0].format =
            AttributeFormat::Float3
    );
    for stage in 0..2 {
        for n in 0..256 * 4 {
            changed!(|d: &mut fixed::Draw| {
                let p = d.programmable.as_mut().unwrap();
                let c = if stage == 0 {
                    &mut p.vertex_constants
                } else {
                    &mut p.pixel_constants
                };
                c.floats[n / 4][n % 4] = f32::from_bits(0x7fc00001);
            });
        }
        for n in 0..16 * 4 {
            for boolean in [false, true] {
                changed!(|d: &mut fixed::Draw| {
                    let p = d.programmable.as_mut().unwrap();
                    let c = if stage == 0 {
                        &mut p.vertex_constants
                    } else {
                        &mut p.pixel_constants
                    };
                    if boolean {
                        c.booleans[n / 4][n % 4] = 2
                    } else {
                        c.integers[n / 4][n % 4] = 1
                    };
                });
            }
        }
        for n in 0..8 * 4 {
            for luminance in [false, true] {
                changed!(|d: &mut fixed::Draw| {
                    let p = d.programmable.as_mut().unwrap();
                    let c = if stage == 0 {
                        &mut p.vertex_constants
                    } else {
                        &mut p.pixel_constants
                    };
                    if luminance {
                        c.bump_luminance[n / 4][n % 4] = -0.0
                    } else {
                        c.bump_matrix[n / 4][n % 4] = -0.0
                    };
                });
            }
        }
    }
    for n in 0..40 * 4 {
        changed!(
            |d: &mut fixed::Draw| d.programmable.as_mut().unwrap().parameters[n / 4][n % 4] = 1
        );
    }
    let texture = f.surface(16, 12, 21, 0);
    let image = f.image(texture);
    changed!(
        |d: &mut fixed::Draw| d.programmable.as_mut().unwrap().textures[0].image = Some(image)
    );
    for n in 0..7 {
        changed!(|d: &mut fixed::Draw| {
            let s = &mut d.programmable.as_mut().unwrap().textures[0].sampler;
            match n {
                0 => s.min = Filter::Linear,
                1 => s.mag = Filter::Linear,
                2 => s.mip = Some(Filter::Nearest),
                3 => s.first_level = 1,
                4 => s.u = Address::Repeat,
                5 => s.v = Address::Repeat,
                _ => s.border_color = 1,
            };
        });
    }
}

// spec: GFX-REC-1 GFX-REC-2
#[test]
fn repeat_keeps_refresh_wait_and_reset_release_and_unsupported_capture_commit_commands() {
    let mut f = fixture();
    f.m.ext.get_mut::<State>().provider.visible = |_, _, _| Some(true);
    for _ in 0..2 {
        clear(&mut f, 0xff123456);
        let outcome = f.call("IDirect3DDevice9::Present", &[f.device, 0, 0, 0, 0]);
        let Outcome::Block {
            wait: Wait::Until(at),
            then,
            ..
        } = outcome
        else {
            panic!("refresh wait required")
        };
        assert!(at > f.m.sys.clock.now_us());
        assert!(at <= f.m.sys.clock.now_us() + 16667);
        assert!(matches!(
            then(
                &mut env(&mut f),
                adv_machine::Resume::Woke(adv_machine::WakeReason::Timeout)
            ),
            Outcome::Return(0)
        ));
    }
    assert_eq!(f.m.sys.frame, 2);
    assert_eq!(f.m.ext.get::<Presents>().published, [1]);
    clear(&mut f, 0xff112233);
    let outcome = f.call("IDirect3DDevice9::GetFrontBufferData", &[f.device, 0, 0]);
    assert!(
        matches!(outcome, Outcome::Fail(_)),
        "strict unsupported ABI failure is preserved"
    );
    let owner = id(&mut f);
    assert!(frame(&mut env(&mut f), owner).commands.is_empty());
    assert_eq!(
        f.m.ext.get::<State>().frames.flushes[Reason::Readback as usize],
        1
    );
    let previous = f.image(f.back);
    clear(&mut f, 0xffabcdef);
    let parameters = f.packet(D3DPRESENT_PARAMETERS {
        BackBufferWidth: 16,
        BackBufferHeight: 12,
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
    assert_eq!(&f.pixels(&previous)[..4], &[0xef, 0xcd, 0xab, 0xff]);
    assert_eq!(
        f.m.ext.get::<State>().frames.flushes[Reason::Reset as usize],
        1
    );
    let final_image = f.image(f.back);
    clear(&mut f, 0xff445566);
    let outcome = f.call("IDirect3DDevice9::Release", &[f.device]);
    assert_eq!(f.finish(outcome), 0);
    assert_eq!(
        f.m.ext.get::<State>().frames.flushes[Reason::Release as usize],
        1
    );
    let mut ticket =
        f.m.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .readback(&final_image, None)
            .unwrap();
    ticket.take_completion_job().unwrap()();
    assert_eq!(
        &ticket.poll().unwrap().unwrap()[..4],
        &[0x66, 0x55, 0x44, 0xff]
    );
}

// spec: GFX-REC-2
#[test]
fn logical_versions_include_sample_identity_and_contents_and_retire_with_resources() {
    let mut f = fixture();
    let source = f.surface(16, 12, 21, 0);
    let other = f.surface(16, 12, 21, 0);
    let owner = id(&mut f);
    let identity = |f: &mut Fixture, pointer| {
        let e = env(f);
        com(&e)
            .identity::<Payload>(&e.sys.objects, pointer)
            .unwrap()
    };
    let first = identity(&mut f, source);
    let second = identity(&mut f, other);
    let pointer = f.device;
    let baseline = draw_key(&env(&mut f), pointer);
    for n in 0..6 {
        let d = match com(&env(&mut f)).get_identity_mut(&mut f.m.sys.objects, owner) {
            Some(Payload::Device(d)) => d,
            _ => unreachable!(),
        };
        match n {
            0 => d.shaders.vertex = Some(first),
            1 => d.shaders.pixel = Some(first),
            2 => d.buffers.declaration = Some(first),
            3 => d.shaders.vertex_constants.floats[0][0] = -0.0,
            4 => d.shaders.pixel_constants.integers[0][0] = 1,
            _ => d.shaders.pixel_constants.booleans[0] = 2,
        };
        assert_ne!(draw_key(&env(&mut f), pointer), baseline);
        if let Some(Payload::Device(d)) =
            com(&env(&mut f)).get_identity_mut(&mut f.m.sys.objects, owner)
        {
            d.shaders = shaders::Bindings::default();
            d.buffers = buffers::Bindings::default();
        }
    }
    let mut keys = Vec::new();
    for resource in [first, second] {
        if let Some(Payload::Device(d)) =
            com(&env(&mut f)).get_identity_mut(&mut f.m.sys.objects, owner)
        {
            d.texture = Some(resource);
        }
        clear(&mut f, 0xff000000);
        let mut d = draw(&mut f);
        d.image = Some(f.image(if resource == first { source } else { other }));
        record_draw(&mut f, d);
        keys.push(frame(&mut env(&mut f), owner).key);
        present(&mut f);
    }
    assert_ne!(keys[0], keys[1]);
    if let Some(Payload::Device(d)) =
        com(&env(&mut f)).get_identity_mut(&mut f.m.sys.objects, owner)
    {
        d.texture = None;
    }
    let locked = f.lock(source, 0, 0);
    f.m.cpu.write(locked.pBits, &vec![64; 16 * 12 * 4]).unwrap();
    f.okay("IDirect3DSurface9::UnlockRect", &[source]);
    present(&mut f);
    assert!(
        frame(&mut env(&mut f), owner)
            .contents
            .keys()
            .any(|k| k.0 == first)
    );
    let outcome = f.call("IDirect3DSurface9::Release", &[source]);
    assert_eq!(f.finish(outcome), 0);
    assert!(
        !frame(&mut env(&mut f), owner)
            .contents
            .keys()
            .any(|k| k.0 == first)
    );
}

// spec: GFX-REC-2
#[test]
fn another_front_publication_prevents_eliding_an_unchanged_device_frame() {
    let mut f = fixture();
    clear(&mut f, 0xff123456);
    present(&mut f);
    let gpu = f.m.ext.get_mut::<State>().gpu.as_mut().unwrap();
    let other = gpu.target(16, 12, false).unwrap();
    gpu.clear(&other, Some([0., 0., 1., 1.]), None).unwrap();
    let snapshot = gpu.present(&other.image, 2).unwrap();
    let p = f.m.ext.get_mut::<Presents>();
    p.serial = 2;
    p.latest = Some(snapshot);
    p.published.push(2);
    clear(&mut f, 0xff123456);
    present(&mut f);
    assert_eq!(f.m.ext.get::<Presents>().published, [1, 2, 3]);
    assert_eq!(f.m.ext.get::<State>().frames.eliminated_frames, 0);
    let image =
        f.m.ext
            .get::<Presents>()
            .latest
            .as_ref()
            .unwrap()
            .image
            .clone();
    assert_eq!(&f.pixels(&image)[..4], &[0x56, 0x34, 0x12, 0xff]);
}

// spec: GFX-REC-1
#[test]
fn execution_time_device_failure_is_deferred_to_cooperative_level_and_present() {
    let mut f = fixture();
    clear(&mut f, 0xff123456);
    let gpu = f.m.ext.get_mut::<State>().gpu.as_mut().unwrap();
    gpu.context().device.destroy();
    let _ = gpu.poll();
    let outcome = flush(&mut f, Reason::Readback);
    assert_eq!(f.finish(outcome), 0, "flush defers device loss");
    let owner = id(&mut f);
    assert!(frame(&mut env(&mut f), owner).commands.is_empty());
    let outcome = f.call("IDirect3DDevice9::TestCooperativeLevel", &[f.device]);
    assert_eq!(f.finish(outcome), DEVICE_LOST);
    let outcome = f.call("IDirect3DDevice9::Present", &[f.device, 0, 0, 0, 0]);
    assert_eq!(f.finish(outcome), DEVICE_LOST);
    assert!(f.m.ext.get::<Presents>().published.is_empty());
}

// spec: GFX-REC-1 GFX-REC-3
#[test]
fn cancelling_verification_releases_the_map_and_never_republishes_its_completed_present() {
    use adv_sync::background::{Priority, Task};
    let mut f = fixture();
    f.m.ext.get_mut::<State>().verify_frames = true;
    // Stall the worker using a real queued task, so the diagnostic fence cannot
    // complete until after cancellation. GPU work stays on the normal backend.
    let (release, blocked) = std::sync::mpsc::channel::<()>();
    let (ready, started) = std::sync::mpsc::channel();
    let sender = f.m.ext.get::<State>().sender.clone().unwrap();
    sender
        .borrow_mut()
        .try_submit(
            Priority::Urgent,
            Task::new(move || {
                ready.send(()).unwrap();
                blocked.recv().unwrap();
            }),
        )
        .unwrap();
    started
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    clear(&mut f, 0xff123456);
    let outcome = f.call("IDirect3DDevice9::Present", &[f.device, 0, 0, 0, 0]);
    assert!(matches!(outcome, Outcome::Block { .. }));
    assert_eq!(f.m.ext.get::<Presents>().published, [1]);
    let owner = id(&mut f);
    assert!(frame(&mut env(&mut f), owner).readback.is_some());
    super::super::cancel_pending(&mut f.m);
    drop(outcome);
    release.send(()).unwrap();
    assert!(frame(&mut env(&mut f), owner).readback.is_none());
    assert!(frame(&mut env(&mut f), owner).presented.is_none());
    clear(&mut f, 0xff123456);
    present(&mut f);
    assert_eq!(f.m.ext.get::<Presents>().published, [1, 2]);
    assert_eq!(f.m.sys.frame, 2);
    assert!(f.m.ext.get::<State>().pending.is_empty());
    assert_eq!(f.m.ext.get::<State>().frames.verification_mismatches, 0);
}

// spec: GFX-REC-1 GFX-REC-2
#[test]
fn failed_busy_wait_admission_keeps_successful_prefix_but_rejects_incoming_upload() {
    let mut f = fixture_with_limits(adv_machine::ObjectLimits {
        references: 3,
        ..adv_machine::ObjectLimits::default()
    });
    let source = f.surface(16, 12, 21, 0);
    let target = f.target(source);
    let frozen =
        f.m.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .completed_frame(target.image.clone(), 1)
            .unwrap();
    clear(&mut f, 0xff123456);
    let owner = id(&mut f);
    let e = env(&mut f);
    let resource = com(&e).identity::<Payload>(&e.sys.objects, source).unwrap();
    frame(&mut env(&mut f), owner).force_limit();
    let outcome = record(
        &mut env(&mut f),
        owner,
        Operation::Upload {
            resource,
            level: None,
            target: target.clone(),
            pixels: Arc::new(vec![7; 16 * 12 * 4]),
            pitch: 64,
            rectangle: Rect {
                x: 0,
                y: 0,
                w: 16,
                h: 12,
            },
        },
    );
    assert!(matches!(outcome, Outcome::Return(OUT_OF_MEMORY)));
    let mut e = env(&mut f);
    let fqueue = frame(&mut e, owner);
    assert_eq!(fqueue.commands.len(), 1);
    assert_eq!(fqueue.head, 1);
    assert!(!fqueue.contents.keys().any(|k| k.0 == resource));
    assert!(fqueue.active.is_none());
    assert_eq!(f.m.ext.get::<State>().frames.recorded_commands, 1);
    drop(frozen);
    assert_eq!(f.pixels(&target.image), vec![0; 16 * 12 * 4]);
    let back = f.image(f.back);
    assert_eq!(&f.pixels(&back)[..4], &[0x56, 0x34, 0x12, 0xff]);
}

// spec: GFX-REC-2
#[test]
fn private_background_presentations_do_not_seed_foreground_elision() {
    let mut f = fixture();
    clear(&mut f, 0xff123456);
    present(&mut f);
    f.m.ext.get_mut::<Presents>().detached = true;
    for _ in 0..2 {
        clear(&mut f, 0xff123456);
        present(&mut f);
    }
    assert_eq!(f.m.ext.get::<Presents>().published, [1]);
    let owner = id(&mut f);
    assert_eq!(frame(&mut env(&mut f), owner).last_serial, None);
    // Handoff can preserve the same maximum serial when the former foreground
    // advanced private frames; regaining the output must still publish once.
    f.m.ext.get_mut::<Presents>().detached = false;
    clear(&mut f, 0xff123456);
    present(&mut f);
    assert_eq!(f.m.ext.get::<Presents>().published, [1, 4]);
    clear(&mut f, 0xff123456);
    present(&mut f);
    assert_eq!(f.m.ext.get::<Presents>().published, [1, 4]);
    assert_eq!(f.m.ext.get::<State>().frames.eliminated_frames, 1);
}

// spec: GFX-REC-1 GFX-REC-2
#[test]
fn transient_vertex_buffer_release_preserves_captured_vertices_and_frame_elision() {
    let mut f = fixture();
    f.okay("IDirect3DDevice9::SetFVF", &[f.device, 0x44]);
    f.okay("IDirect3DDevice9::SetRenderState", &[f.device, 22, 1]);
    for _ in 0..2 {
        let output = f.packet(0u32);
        f.okay(
            "IDirect3DDevice9::CreateVertexBuffer",
            &[f.device, 60, 0, 0x44, 2, output, 0],
        );
        let buffer = f.m.cpu.read_u32(output).unwrap();
        let mapped = f.packet(0u32);
        f.okay("IDirect3DVertexBuffer9::Lock", &[buffer, 0, 0, mapped, 0]);
        let address = f.m.cpu.read_u32(mapped).unwrap();
        let mut words = Vec::new();
        for (x, y) in [(1f32, 1f32), (15., 1.), (1., 11.)] {
            words.extend([
                x.to_bits(),
                y.to_bits(),
                0f32.to_bits(),
                1f32.to_bits(),
                0xff4080c0,
            ]);
        }
        f.m.cpu
            .write(address, bytemuck::cast_slice(&words))
            .unwrap();
        f.okay("IDirect3DVertexBuffer9::Unlock", &[buffer]);
        f.okay(
            "IDirect3DDevice9::SetStreamSource",
            &[f.device, 0, buffer, 0, 20],
        );
        clear(&mut f, 0xff000000);
        f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
        f.okay("IDirect3DDevice9::DrawPrimitive", &[f.device, 4, 0, 1]);
        f.okay("IDirect3DDevice9::SetStreamSource", &[f.device, 0, 0, 0, 0]);
        let outcome = f.call("IDirect3DVertexBuffer9::Release", &[buffer]);
        assert_eq!(f.finish(outcome), 0);
        let owner = id(&mut f);
        assert_eq!(
            frame(&mut env(&mut f), owner).commands.len(),
            2,
            "transient release must not execute the frame early"
        );
        f.okay("IDirect3DDevice9::EndScene", &[f.device]);
        present(&mut f);
    }
    assert_eq!(f.m.ext.get::<Presents>().published, [1]);
    assert_eq!(f.m.ext.get::<State>().frames.eliminated_frames, 1);
    let back = f.image(f.back);
    let pixels = f.pixels(&back);
    assert_eq!(
        &pixels[(2 * 16 + 2) * 4..(2 * 16 + 2) * 4 + 4],
        &[192, 128, 64, 255]
    );
}

// spec: GFX-REC-1
#[test]
fn releasing_the_last_child_device_owner_flushes_before_indirect_device_teardown() {
    for block in [false, true] {
        let mut f = fixture();
        let output = f.packet(0u32);
        if block {
            f.okay("IDirect3DDevice9::CreateStateBlock", &[f.device, 1, output]);
        } else {
            f.okay(
                "IDirect3DDevice9::CreateVertexBuffer",
                &[f.device, 60, 0, 0, 2, output, 0],
            );
        }
        let child = f.m.cpu.read_u32(output).unwrap();
        let image = f.image(f.back);
        let outcome = f.call("IDirect3DDevice9::Release", &[f.device]);
        assert_eq!(f.finish(outcome), 1);
        clear(&mut f, 0xff123456);
        let outcome = f.call(
            if block {
                "IDirect3DStateBlock9::Release"
            } else {
                "IDirect3DVertexBuffer9::Release"
            },
            &[child],
        );
        assert_eq!(f.finish(outcome), 0);
        let pointer = f.device;
        assert!(device(&env(&mut f), pointer).is_none());
        let mut ticket =
            f.m.ext
                .get_mut::<State>()
                .gpu
                .as_mut()
                .unwrap()
                .readback(&image, None)
                .unwrap();
        ticket.take_completion_job().unwrap()();
        assert_eq!(
            &ticket.poll().unwrap().unwrap()[..4],
            &[0x56, 0x34, 0x12, 0xff]
        );
    }
}
