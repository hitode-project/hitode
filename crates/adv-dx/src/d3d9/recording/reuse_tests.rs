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
