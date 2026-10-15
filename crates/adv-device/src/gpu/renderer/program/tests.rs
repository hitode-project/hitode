use super::*;

fn gpu() -> Gpu {
    let gpu = pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).unwrap();
    // These tests inspect the wgpu batching and binding caches directly.
    #[cfg(target_vendor = "apple")]
    {
        gpu.context.pending.lock().unwrap().metal = None;
    }
    gpu
}
fn read(gpu: &mut Gpu, target: &Target) -> Vec<u8> {
    let mut ticket = gpu.readback(&target.image, None).unwrap();
    ticket.take_completion_job().unwrap()();
    ticket.poll().unwrap().unwrap()
}
fn quad(color: [f32; 4]) -> [Vertex; 6] {
    [
        (-1., -1.),
        (1., -1.),
        (-1., 1.),
        (-1., 1.),
        (1., -1.),
        (1., 1.),
    ]
    .map(|(x, y)| Vertex {
        position: [x, y, 0., 1.],
        color,
        uv: [0.; 2],
    })
}
fn draw() -> ProgrammableDraw {
    ProgrammableDraw {
        program: Program::new("struct C { f: array<vec4<f32>,256> }; @group(0) @binding(1) var<uniform> c:C; @vertex fn vertex_main(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> { let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.)); return vec4(p[i],0.,1.); } @fragment fn fragment_main()->@location(0) vec4<f32> { return c.f[0]; }".into(), vec![]),
        streams: vec![], indices: None, vertex_count: 3,
        vertex_constants: ConstantBlock::default(), pixel_constants: ConstantBlock::default(),
        textures: vec![], parameters: [[0;4];40],
    }
}

#[test]
// spec: GFX-BUFFER-2, GFX-CACHE-1
fn adjacent_constants_reuse_offsets_and_clones_reuse_pipelines() {
    let mut gpu = gpu();
    let target = gpu.target(2, 1, false).unwrap();
    let mut draw = draw();
    let state = DrawState::new(2, 1);
    draw.pixel_constants.floats[0] = [1., 0., 0., 1.];
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    draw.program = draw.program.clone();
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    {
        let pending = gpu.context.pending.lock().unwrap();
        assert_eq!(pending.last_constants.start, 0);
        assert_eq!(
            pending.uniforms.bytes.len(),
            (CONSTANT_SIZE * 2 + PARAMETER_SIZE) as usize
        );
    }
    draw.pixel_constants.floats[0] = [0., 1., 0., 1.];
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    assert_eq!(
        gpu.context.pending.lock().unwrap().last_constants.start,
        UNIFORM_STRIDE as usize
    );
    assert_eq!(gpu.programmable.entries.len(), 1);
    assert_eq!(gpu.stats().pipeline_cache_hits, 2);
    assert_eq!(gpu.stats().bind_group_creations, 2);
    assert_eq!(read(&mut gpu, &target), [0, 255, 0, 255].repeat(2));
    assert_eq!(gpu.stats().last_flush_buffer_writes, 1);
}

#[test]
// spec: GFX-BUFFER-2
fn fixed_and_programmable_share_ordered_uploads_and_external_flushes() {
    let mut gpu = gpu();
    let target = gpu.target(3, 1, false).unwrap();
    gpu.clear(&target, Some([0.; 4]), None).unwrap();
    let before = gpu.stats();
    let mut state = DrawState::new(3, 1);
    let mut draw = draw();
    draw.pixel_constants.floats[0] = [0., 1., 0., 1.];
    gpu.draw(&target, &quad([1., 0., 0., 1.]), None, state)
        .unwrap();
    state.scissor = Some(Rect {
        x: 1,
        y: 0,
        w: 2,
        h: 1,
    });
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    state.scissor = Some(Rect {
        x: 2,
        y: 0,
        w: 1,
        h: 1,
    });
    state.alpha_reference = 0.5;
    gpu.draw(&target, &quad([0., 0., 1., 1.]), None, state)
        .unwrap();
    assert_eq!(gpu.stats().submissions, before.submissions);
    let pending = gpu.context.pending.lock().unwrap();
    assert_eq!(pending.draws, 3);
    assert_ne!(pending.last_constants.start, 0);
    drop(pending);
    assert_eq!(
        read(&mut gpu, &target),
        [0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255]
    );
    assert_eq!(gpu.stats().last_flush_buffer_writes, 2);
    assert_eq!(gpu.stats().buffer_writes - before.buffer_writes, 2);

    // A fixed draw must flush before a texture update, even with the same view.
    let texture = gpu.image(1, 1).unwrap();
    gpu.upload(
        &texture,
        Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        },
        4,
        &[0, 0, 255, 255],
    )
    .unwrap();
    state = DrawState::new(3, 1);
    state.texture_op = TextureOp::Texture;
    gpu.draw(&target, &quad([1.; 4]), Some(&texture), state)
        .unwrap();
    gpu.upload(
        &texture,
        Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        },
        4,
        &[0, 255, 0, 255],
    )
    .unwrap();
    state.scissor = Some(Rect {
        x: 1,
        y: 0,
        w: 2,
        h: 1,
    });
    gpu.draw(&target, &quad([1.; 4]), Some(&texture), state)
        .unwrap();
    assert_eq!(
        read(&mut gpu, &target),
        [0, 0, 255, 255, 0, 255, 0, 255, 0, 255, 0, 255]
    );
}

#[test]
// spec: GFX-CACHE-6
fn texture_bindings_track_view_lifetimes_sampler_layout_and_lru() {
    let mut gpu = gpu();
    let layout = gpu.bindings.clone();
    let state = DrawState::new(1, 1);
    let empty_layout = || {
        gpu.context
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: None,
                entries: &[],
            })
    };
    let first_layout = empty_layout();
    let second_layout = empty_layout();
    assert_ne!(
        gpu.texture_binding(&first_layout, &[], false),
        gpu.texture_binding(&second_layout, &[], false)
    );
    let texture = gpu.image_levels(2, 2, 2).unwrap();
    let mip = texture.mip_level(1).unwrap();
    let group = gpu.texture_binding(&layout, &[(&texture, state.sampler)], true);
    assert_eq!(
        gpu.texture_binding(&layout, &[(&texture, state.sampler)], true),
        group
    );
    assert_ne!(
        gpu.texture_binding(&layout, &[(&mip, state.sampler)], true),
        group
    );
    let mut sampler = state.sampler;
    sampler.u = Address::Repeat;
    assert_ne!(
        gpu.texture_binding(&layout, &[(&texture, sampler)], true),
        group
    );
    let old_key = TextureKey(layout.clone(), vec![(mip.view.clone(), state.sampler)]);
    drop(mip);
    assert_eq!(
        gpu.programmable.texture_bindings[&old_key].lives[0].strong_count(),
        0
    );
    let replacement = texture.mip_level(1).unwrap();
    gpu.texture_binding(&layout, &[(&replacement, state.sampler)], true);
    assert!(!gpu.programmable.texture_bindings.contains_key(&old_key));
    drop(texture);
    drop(replacement);

    let texture = gpu.image(1, 1).unwrap();
    let views: Vec<_> = (0..1025).map(|_| texture.mip_level(0).unwrap()).collect();
    for view in &views[..1024] {
        gpu.texture_binding(&layout, &[(view, state.sampler)], true);
    }
    let first = gpu.texture_binding(&layout, &[(&views[0], state.sampler)], true);
    gpu.texture_binding(&layout, &[(&views[1024], state.sampler)], true);
    assert_eq!(gpu.programmable.texture_bindings.len(), 1024);
    assert_eq!(
        gpu.texture_binding(&layout, &[(&views[0], state.sampler)], true),
        first
    );
    assert!(!gpu.programmable.texture_bindings.contains_key(&TextureKey(
        layout.clone(),
        vec![(views[1].view.clone(), state.sampler)]
    )));
}

#[test]
// spec: GFX-BUFFER-2
fn oversized_fixed_upload_submits_immediately() {
    let mut gpu = gpu();
    let target = gpu.target(1, 1, false).unwrap();
    let vertices = quad([1., 0., 0., 1.]).repeat(
        (MAX_UPLOAD / UNIFORM_SLOTS / std::mem::size_of::<Vertex>() as u64 / 6 + 1) as usize,
    );
    let before = gpu.stats().submissions;
    gpu.draw(&target, &vertices, None, DrawState::new(1, 1))
        .unwrap();
    assert_eq!(gpu.stats().submissions, before + 1);
    assert_eq!(gpu.stats().last_flush_buffer_writes, 2);
    assert_eq!(read(&mut gpu, &target), [0, 0, 255, 255]);
}
