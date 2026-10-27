fn reuse_fixture() -> (Fixture, u32) {
    let mut f = fixture();
    let output = f.packet(0u32);
    f.okay(
        "IDirect3DDevice9::CreateVertexBuffer",
        &[f.device, 96, 0, 0x44, 2, output, 0],
    );
    let buffer = f.m.cpu.read_u32(output).unwrap();
    f.okay("IDirect3DVertexBuffer9::Lock", &[buffer, 0, 0, output, 0]);
    let data = f.m.cpu.read_u32(output).unwrap();
    for (n, (x, y)) in [(1f32, 1f32), (15., 1.), (1., 11.)].into_iter().enumerate() {
        f.m.cpu
            .write(
                data + n as u32 * 32,
                bytemuck::cast_slice(&[
                    x.to_bits(),
                    y.to_bits(),
                    0f32.to_bits(),
                    1f32.to_bits(),
                    0xff4080c0u32,
                    0,
                    0,
                    0,
                ]),
            )
            .unwrap();
    }
    f.okay("IDirect3DVertexBuffer9::Unlock", &[buffer]);
    f.okay(
        "IDirect3DDevice9::SetStreamSource",
        &[f.device, 0, buffer, 0, 32],
    );
    f.okay("IDirect3DDevice9::SetFVF", &[f.device, 0x44]);
    let shader = f.packet([0xffff0200u32, 0x02000001, 0x800f0800, 0xa0e40000, 0xffff]);
    f.okay(
        "IDirect3DDevice9::CreatePixelShader",
        &[f.device, shader, output],
    );
    let pixel = f.m.cpu.read_u32(output).unwrap();
    f.okay("IDirect3DDevice9::SetPixelShader", &[f.device, pixel]);
    let constant = f.packet([0.25f32, 0.5, 0.75, 1.]);
    f.okay(
        "IDirect3DDevice9::SetPixelShaderConstantF",
        &[f.device, 0, constant, 1],
    );
    f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
    (f, data)
}
fn reuse_draw(f: &mut Fixture, indices: &[u32], builds: &std::cell::Cell<u32>) {
    let pointer = f.device;
    let outcome = indexed(&mut env(f), pointer, (4, 1, indices), |env| {
        builds.set(builds.get() + 1);
        programmable::prepare(env, pointer, 4, 1, None, Some(indices), 0)
    });
    assert!(matches!(outcome, Outcome::Return(0)));
    let owner = id(f);
    let cached = frame(&mut env(f), owner).commands.last().unwrap().clone();
    let reference =
        programmable::prepare(&mut env(f), pointer, 4, 1, None, Some(indices), 0).unwrap();
    let a = key::prepare(&mut env(f), owner, cached).unwrap();
    let b = key::prepare(&mut env(f), owner, Operation::Draw(reference)).unwrap();
    assert_eq!(a.key, b.key);
    assert_eq!(a.bytes, b.bytes);
    assert!(a.reads == b.reads && a.writes == b.writes && a.samples == b.samples);
    assert_eq!(a.bounds, b.bounds);
}

#[test]
// spec: GFX-REC-11
fn d3d9_reuse_sequence_matches_uncached_commands_keys_and_pixels() {
    let (mut f, _) = reuse_fixture();
    let builds = std::cell::Cell::new(0);
    let mut keys = Vec::new();
    let mut pixels = Vec::new();
    for uncached in [false, false, true] {
        clear(&mut f, 0xff000000);
        let owner = id(&mut f);
        for cull in [1, 2, 1] {
            f.okay("IDirect3DDevice9::SetRenderState", &[f.device, 22, cull]);
            if uncached {
                frame(&mut env(&mut f), owner).reuse = Default::default();
            }
            reuse_draw(&mut f, &[0, 1, 2], &builds);
        }
        keys.push(frame(&mut env(&mut f), owner).key);
        assert_eq!(frame(&mut env(&mut f), owner).commands.len(), 4);
        f.okay("IDirect3DDevice9::EndScene", &[f.device]);
        present(&mut f);
        f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
        let image = f.image(f.back);
        pixels.push(f.pixels(&image));
    }
    assert_eq!(builds.get(), 5, "two cold states, then three forced misses");
    assert!(keys.windows(2).all(|w| w[0] == w[1]));
    assert!(pixels.windows(2).all(|w| w[0] == w[1]));
    for cached in [false, true] {
        clear(&mut f, 0xff000000);
        reuse_draw(&mut f, &[0, 1, 2], &builds);
        let pointer = f.device;
        let owner = id(&mut f);
        let start = std::time::Instant::now();
        for _ in 0..220 {
            let build = |env: &mut Env<'_>| {
                programmable::prepare(env, pointer, 4, 1, None, Some(&[0, 1, 2]), 0)
            };
            let outcome = if cached {
                indexed(&mut env(&mut f), pointer, (4, 1, &[0, 1, 2]), build)
            } else {
                let draw = build(&mut env(&mut f)).unwrap();
                record(&mut env(&mut f), owner, Operation::Draw(draw))
            };
            assert!(matches!(outcome, Outcome::Return(0)));
        }
        eprintln!(
            "D3D9 authored 220-draw host batch: cached={cached}, elapsed={:?}",
            start.elapsed()
        );
        f.okay("IDirect3DDevice9::EndScene", &[pointer]);
        present(&mut f);
        f.okay("IDirect3DDevice9::BeginScene", &[pointer]);
    }
}

#[test]
// spec: GFX-REC-11
fn d3d9_reuse_invalidates_all_draw_inputs() {
    let (mut f, data) = reuse_fixture();
    let builds = std::cell::Cell::new(0);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    assert_eq!(builds.get(), 1);
    let d = f.device;
    let constant = f.packet([0.25f32; 4]);
    let viewport = f.packet(adv_abi::types::D3DVIEWPORT9 {
        X: 0,
        Y: 0,
        Width: 15,
        Height: 12,
        MinZ: 0.,
        MaxZ: 1.,
    });
    let output = f.packet([0u32; 2]);
    f.m.ext
        .get_mut::<State>()
        .tables
        .insert("IDirect3DTexture9", 0);
    f.okay(
        "IDirect3DDevice9::CreateTexture",
        &[d, 2, 2, 1, 0, 21, 1, output, 0],
    );
    let texture = f.m.cpu.read_u32(output).unwrap();
    let declaration = f.packet([
        [0u8, 0, 0, 0, 3, 0, 9, 0],
        [0, 0, 16, 0, 4, 0, 10, 0],
        [255, 0, 0, 0, 17, 0, 0, 0],
    ]);
    f.okay(
        "IDirect3DDevice9::CreateVertexDeclaration",
        &[d, declaration, output],
    );
    let declaration = f.m.cpu.read_u32(output).unwrap();
    f.okay(
        "IDirect3DDevice9::CreateIndexBuffer",
        &[d, 6, 0, 101, 2, output, 0],
    );
    let index_buffer = f.m.cpu.read_u32(output).unwrap();
    let shader = f.packet([
        0xfffe0200u32,
        0x0200001f,
        0x80000000,
        0x900f0000,
        0x02000001,
        0xc00f0000,
        0x90e40000,
        0xffff,
    ]);
    f.okay("IDirect3DDevice9::CreateVertexShader", &[d, shader, output]);
    let vertex = f.m.cpu.read_u32(output).unwrap();
    for (name, args) in [
        ("SetRenderState", vec![d, 22, 1]),
        ("SetSamplerState", vec![d, 0, 5, 2]),
        ("SetTexture", vec![d, 0, texture]),
        ("SetPixelShaderConstantF", vec![d, 0, constant, 1]),
        ("SetViewport", vec![d, viewport]),
        ("SetVertexShader", vec![d, vertex]),
        ("SetIndices", vec![d, index_buffer]),
        ("SetFVF", vec![d, 0x144]),
        ("SetVertexDeclaration", vec![d, declaration]),
        ("SetStreamSource", vec![d, 0, 0, 0, 0]),
    ] {
        let before = builds.get();
        if name == "SetStreamSource" {
            // Keep a valid stream while changing its stride and captured span.
            let buffer = device(&env(&mut f), d).unwrap().buffers.streams[0]
                .buffer
                .unwrap();
            let Some(Payload::Buffer(buffer)) =
                com(&env(&mut f)).get_identity(&f.m.sys.objects, buffer)
            else {
                panic!()
            };
            let pointer = buffer.pointer;
            f.okay("IDirect3DDevice9::SetStreamSource", &[d, 0, pointer, 0, 0]);
        } else {
            f.okay(&format!("IDirect3DDevice9::{name}"), &args);
        }
        reuse_draw(&mut f, &[0, 1, 2], &builds);
        assert_eq!(builds.get(), before + 1, "{name}");
    }
    let before = builds.get();
    f.m.cpu.write_u32(data + 16, 0xff123456).unwrap();
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    assert_eq!(builds.get(), before + 1, "buffer contents");
    f.m.cpu.write_u32(data + 16, 0xff4080c0).unwrap();
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    f.m.cpu.write_u32(data + 16, 0xff123456).unwrap();
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    assert_eq!(
        builds.get(),
        before + 1,
        "returning buffer contents reuse both variants"
    );
    reuse_draw(&mut f, &[1, 0, 2], &builds);
    assert_eq!(builds.get(), before + 2, "index order");
    f.okay("IDirect3DTexture9::LockRect", &[texture, 0, output, 0, 0]);
    let lock = GuestPtr::<adv_abi::types::D3DLOCKED_RECT>::new(output)
        .read(&f.m.cpu)
        .unwrap();
    f.m.cpu.write_u32(lock.pBits, 0xffabcdef).unwrap();
    f.okay("IDirect3DTexture9::UnlockRect", &[texture, 0]);
    reuse_draw(&mut f, &[1, 0, 2], &builds);
    assert_eq!(builds.get(), before + 3, "texture contents");
    f.okay(
        "IDirect3DIndexBuffer9::Lock",
        &[index_buffer, 0, 0, output, 0],
    );
    let indices = f.m.cpu.read_u32(output).unwrap();
    f.m.cpu
        .write(indices, bytemuck::cast_slice(&[2u16, 1, 0]))
        .unwrap();
    f.okay("IDirect3DIndexBuffer9::Unlock", &[index_buffer]);
    f.okay(
        "IDirect3DDevice9::DrawIndexedPrimitive",
        &[d, 4, 0, 0, 3, 0, 1],
    );
    let owner = id(&mut f);
    let Operation::Draw(draw) = frame(&mut env(&mut f), owner)
        .commands
        .last()
        .unwrap()
        .clone()
    else {
        panic!()
    };
    assert_eq!(
        draw.programmable
            .as_ref()
            .unwrap()
            .indices
            .as_ref()
            .unwrap()
            .as_slice(),
        &[2, 1, 0]
    );
    f.okay("IDirect3DDevice9::SetPixelShader", &[d, 0]);
    reuse_draw(&mut f, &[1, 0, 2], &builds);
    assert_eq!(builds.get(), before + 4, "shader");
}

#[test]
// spec: GFX-REC-11
fn d3d9_reuse_rebases_onto_a_rotated_back_buffer_image() {
    let (mut f, _) = reuse_fixture();
    let builds = std::cell::Cell::new(0);
    clear(&mut f, 0xff000000);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    f.okay("IDirect3DDevice9::EndScene", &[f.device]);
    present(&mut f);
    let old = f.image(f.back);
    let expected = f.pixels(&old);
    // Presentation hands the same back buffer another image of its shape.
    let spare = f.render_target(old.width(), old.height(), 22);
    let target = f.target(spare);
    assert_eq!(
        hash(key::target_key(&target)),
        hash(key::target_key(&f.target(f.back)))
    );
    let back = f.back;
    let e = env(&mut f);
    let surface = resources::resource(&e, back).unwrap().surface.unwrap();
    let replace = e.ext.get::<State>().provider.replace;
    replace(e.ext, surface, target).unwrap();
    f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
    clear(&mut f, 0xff000000);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    assert_eq!(
        builds.get(),
        1,
        "the rotated image reuses the prepared draw"
    );
    f.okay("IDirect3DDevice9::EndScene", &[f.device]);
    present(&mut f);
    let new = f.image(f.back);
    assert_ne!(new.view(), old.view());
    assert_eq!(f.pixels(&new), expected);
    // Retained entries must not pin the rotated-out image, or presentation
    // can never reuse it.
    let device_pointer = f.device;
    let e = env(&mut f);
    let d = device(&e, device_pointer).unwrap();
    assert!(!d.frame.reuse.retains(&old));
    assert!(d.frame.reuse.retains(&new));
}

#[test]
// spec: GFX-REC-1, GFX-REC-2, GFX-REC-4, GFX-REC-11, GFX-SHADER-4
fn d3d9_differential_frames_match_full_commands_damage_and_pixels_with_stateblocks() {
    let mut results = Vec::new();
    let (mut f, _) = reuse_fixture();
    let d = f.device;
    let output = f.packet(0u32);
    f.okay("IDirect3DDevice9::BeginStateBlock", &[d]);
    f.okay("IDirect3DDevice9::SetRenderState", &[d, 22, 3]);
    f.okay("IDirect3DDevice9::SetSamplerState", &[d, 0, 5, 1]);
    f.okay("IDirect3DDevice9::EndStateBlock", &[d, output]);
    let block = f.m.cpu.read_u32(output).unwrap();
    let mut templates = Vec::new();
    for n in 0..4 {
        let mut template = draw(&mut f);
        let x = -0.9 + n as f32 * 0.45;
        template.vertices = Arc::new(vec![
            Vertex {
                position: [x, -0.7, 0., 1.],
                color: [1., 0., 0., 1.],
                uv: [0.; 2],
            },
            Vertex {
                position: [x + 0.15, -0.7, 0., 1.],
                color: [1., 0., 0., 1.],
                uv: [0.; 2],
            },
            Vertex {
                position: [x, -0.3, 0., 1.],
                color: [1., 0., 0., 1.],
                uv: [0.; 2],
            },
        ]);
        templates.push(template);
    }
    for full in [false, true] {
        let owner = id(&mut f);
        *frame(&mut env(&mut f), owner) = Recording::default();
        let builds = std::cell::Cell::new(0);
        let mut frames = Vec::new();
        for sequence in [&[0, 1, 2][..], &[0, 3, 1, 2], &[0, 1, 2], &[0, 1, 2]] {
            clear(&mut f, 0xff000000);
            for &n in sequence {
                f.okay("IDirect3DStateBlock9::Capture", &[block]);
                let fingerprints = device(&env(&mut f), d).unwrap().fingerprints;
                f.okay("IDirect3DDevice9::SetRenderState", &[d, 22, 1]);
                f.okay("IDirect3DDevice9::SetSamplerState", &[d, 0, 5, 2]);
                f.okay("IDirect3DStateBlock9::Apply", &[block]);
                assert_eq!(device(&env(&mut f), d).unwrap().fingerprints, fingerprints);
                f.okay("IDirect3DDevice9::SetSamplerState", &[d, 0, 5, 1]);
                let owner = id(&mut f);
                let outcome = if full {
                    record(
                        &mut env(&mut f),
                        owner,
                        Operation::Draw(templates[n as usize].clone()),
                    )
                } else {
                    indexed(&mut env(&mut f), d, (4, 1, &[n]), |_| {
                        builds.set(builds.get() + 1);
                        Ok(templates[n as usize].clone())
                    })
                };
                assert_eq!(f.finish(outcome), 0);
            }
            let owner = id(&mut f);
            let key = frame(&mut env(&mut f), owner).key;
            let published = f.m.ext.get::<Presents>().damage.len();
            f.okay("IDirect3DDevice9::EndScene", &[d]);
            present(&mut f);
            let damage = f.m.ext.get::<Presents>().damage.get(published).copied();
            let image = f.image(f.back);
            frames.push((key, damage, f.pixels(&image)));
            f.okay("IDirect3DDevice9::BeginScene", &[d]);
        }
        if !full {
            assert_eq!(builds.get(), 4, "three initial entries and one insertion");
        }
        results.push(frames);
    }
    for (n, (cached, full)) in results[0].iter().zip(&results[1]).enumerate() {
        assert_eq!(cached.0, full.0, "frame {n} commands");
        assert_eq!(cached.1, full.1, "frame {n} damage");
        assert_eq!(cached.2, full.2, "frame {n} pixels");
    }
}

#[test]
// spec: GFX-REC-1, GFX-REC-11
fn d3d9_differential_early_readback_keeps_the_remaining_frame_on_the_full_path() {
    let (mut f, _) = reuse_fixture();
    let builds = std::cell::Cell::new(0);
    clear(&mut f, 0xff000000);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    let outcome = flush(&mut f, Reason::Readback);
    assert_eq!(f.finish(outcome), 0);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    assert_eq!(builds.get(), 3);
    f.okay("IDirect3DDevice9::EndScene", &[f.device]);
    present(&mut f);
    f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
    clear(&mut f, 0xff000000);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    assert_eq!(builds.get(), 4, "the next frame can share entries again");
}

#[test]
// spec: GFX-REC-11
fn d3d9_reuse_signature_prefix_rechecks_locked_streams() {
    let (mut f, _) = reuse_fixture();
    let builds = std::cell::Cell::new(0);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    assert_eq!(builds.get(), 1);
    let d = f.device;
    let e = env(&mut f);
    let buffer = device(&e, d).unwrap().buffers.streams[0].buffer.unwrap();
    let Some(Payload::Buffer(buffer)) = com(&e).get_identity(&e.sys.objects, buffer) else {
        panic!()
    };
    let buffer = buffer.pointer;
    let output = f.packet(0u32);
    for _ in 0..2 {
        f.okay("IDirect3DVertexBuffer9::Lock", &[buffer, 0, 0, output, 0]);
    }
    for _ in 0..2 {
        let outcome = indexed(&mut env(&mut f), d, (4, 1, &[0, 1, 2]), |env| {
            builds.set(builds.get() + 1);
            programmable::prepare(env, d, 4, 1, None, Some(&[0, 1, 2]), 0)
        });
        assert!(matches!(outcome, Outcome::Return(INVALID)));
        f.okay("IDirect3DVertexBuffer9::Unlock", &[buffer]);
    }
    assert_eq!(builds.get(), 3);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    assert_eq!(builds.get(), 3, "unlocked streams can reuse retained draws");
}

#[test]
// spec: GFX-REC-11
fn d3d9_reuse_signature_prefix_rechecks_target_shape() {
    let (mut f, _) = reuse_fixture();
    let builds = std::cell::Cell::new(0);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    assert_eq!(builds.get(), 1);
    let spare = f.render_target(32, 24, 22);
    let target = f.target(spare);
    let back = f.back;
    let e = env(&mut f);
    let surface = resources::resource(&e, back).unwrap().surface.unwrap();
    let replace = e.ext.get::<State>().provider.replace;
    replace(e.ext, surface, target).unwrap();
    reuse_draw(&mut f, &[0, 1, 2], &builds);
    assert_eq!(builds.get(), 2);
}
