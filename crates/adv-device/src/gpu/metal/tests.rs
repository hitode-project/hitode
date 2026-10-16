use super::*;
fn gpu(native: bool) -> Gpu {
    let gpu = pollster::block_on(Gpu::request_backend(false, GpuBackend::Metal)).unwrap();
    gpu.context.pending.lock().unwrap().metal = native.then(|| State::new(&gpu.context).unwrap());
    gpu
}
fn read(gpu: &mut Gpu, target: &Target) -> Vec<u8> {
    let mut ticket = gpu.readback(&target.image, None).unwrap();
    ticket.take_completion_job().unwrap()();
    ticket.poll().unwrap().unwrap()
}
fn draw() -> ProgrammableDraw {
    ProgrammableDraw {
        program: Program::new("struct C { f: array<vec4<f32>,256> }; @group(0) @binding(1) var<uniform> c:C; @vertex fn vertex_main(@location(0) p:vec4<f32>)->@builtin(position) vec4<f32> { return p; } @fragment fn fragment_main()->@location(0) vec4<f32> { return c.f[0]; }".into(), vec![]),
        streams: vec![VertexStream { stride: 16, bytes: bytemuck::cast_slice(&[[-1f32,-1.,0.,1.], [3.,-1.,0.,1.], [-1.,3.,0.,1.]]).to_vec(), attributes: vec![Attribute { location: 0, offset: 0, format: AttributeFormat::Float4 }] }],
        indices: None, vertex_count: 3, vertex_constants: ConstantBlock::default(), pixel_constants: ConstantBlock::default(), textures: vec![], parameters: [[0;4];40],
    }
}
// spec: GFX-METAL-1, GFX-BUFFER-2
#[test]
fn native_triangle_matches_wgpu_with_fresh_and_cleared_targets() {
    let mut native = gpu(true);
    let mut reference = gpu(false);
    let mut draw = draw();
    draw.pixel_constants.floats[0] = [0.13, 0.71, 0.29, 0.53];
    for clear in [false, true] {
        let target = native.target(8, 8, false).unwrap();
        let expected = reference.target(8, 8, false).unwrap();
        if clear {
            native.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
            reference
                .clear(&expected, Some([1., 0., 0., 1.]), None)
                .unwrap();
        }
        let mut state = DrawState::new(8, 8);
        state.scissor = Some(Rect {
            x: 1,
            y: 2,
            w: 5,
            h: 4,
        });
        native.draw_programmable(&target, &draw, &state).unwrap();
        let written = native.stats().ring_bytes;
        native.draw_programmable(&target, &draw, &state).unwrap();
        assert_eq!(
            native.stats().ring_bytes - written,
            52,
            "only vertices and their size are rewritten"
        );
        reference
            .draw_programmable(&expected, &draw, &state)
            .unwrap();
        reference
            .draw_programmable(&expected, &draw, &state)
            .unwrap();
        assert_eq!(read(&mut native, &target), read(&mut reference, &expected));
    }
    assert_eq!(native.stats().metal_draws, 4);
}

#[test]
// spec: GFX-METAL-1, GFX-REC-7
fn sparse_native_constants_match_full_wgpu_and_ignore_unused_registers() {
    let mut native = gpu(true);
    let mut reference = gpu(false);
    let target = native.target(4, 4, false).unwrap();
    let expected = reference.target(4, 4, false).unwrap();
    let mut draw = draw();
    draw.program.constants = [crate::constants::ConstantUsage::default(); 2];
    draw.program.constants[1].set(0);
    draw.pixel_constants.floats[0] = [0.13, 0.71, 0.29, 0.53];
    let state = DrawState::new(4, 4);
    let before = native.stats().ring_bytes;
    native.draw_programmable(&target, &draw, &state).unwrap();
    assert_eq!(native.stats().ring_bytes - before, 16 + 640 + 52);
    let written = native.stats().ring_bytes;
    draw.vertex_constants.floats.fill([f32::NAN; 4]);
    draw.pixel_constants.floats[255] = [f32::INFINITY; 4];
    native.draw_programmable(&target, &draw, &state).unwrap();
    assert_eq!(native.stats().ring_bytes - written, 52);
    draw.program.constants = [crate::constants::ConstantUsage::ALL; 2];
    reference
        .draw_programmable(&expected, &draw, &state)
        .unwrap();
    assert_eq!(read(&mut native, &target), read(&mut reference, &expected));
}

fn textured_draw(
    program: &Program,
    stride: usize,
    indexed: bool,
    topology: Topology,
) -> ProgrammableDraw {
    let positions = [
        [-0.8f32, -0.8, 0.5, 1.],
        [0.8, -0.8, 0.5, 1.],
        [-0.8, 0.8, 0.5, 1.],
        [0.8, 0.8, 0.5, 1.],
    ];
    let sequence: &[usize] = match topology {
        Topology::Triangles => &[0, 1, 2, 2, 1, 3],
        _ => &[0, 1, 2, 3],
    };
    let vertices = if indexed {
        vec![0, 1, 2, 3]
    } else {
        sequence.to_vec()
    };
    let mut bytes = vec![0u8; stride * vertices.len()];
    let mut attributes = Vec::new();
    for (i, &vertex) in vertices.iter().enumerate() {
        bytes[i * stride + 8..i * stride + 24]
            .copy_from_slice(bytemuck::bytes_of(&positions[vertex]));
        attributes.extend_from_slice(bytemuck::cast_slice(&[
            (vertex % 2) as f32,
            (vertex / 2) as f32,
        ]));
        attributes.extend_from_slice(&[128, 192, 64, 191]);
    }
    let mut result = draw();
    result.program = program.clone();
    result.streams = vec![
        VertexStream {
            bytes,
            stride: stride as u32,
            attributes: vec![Attribute {
                location: 0,
                offset: 8,
                format: AttributeFormat::Float4,
            }],
        },
        VertexStream {
            bytes: attributes,
            stride: 12,
            attributes: vec![
                Attribute {
                    location: 1,
                    offset: 0,
                    format: AttributeFormat::Float2,
                },
                Attribute {
                    location: 2,
                    offset: 8,
                    format: AttributeFormat::Unorm8x4,
                },
            ],
        },
    ];
    result.vertex_count = vertices.len() as u32;
    result.indices = indexed.then(|| sequence.iter().map(|&v| v as u32).collect());
    result.pixel_constants.floats[0] = [0.8, 0.7, 0.9, 0.6];
    result
}
fn render_states(
    gpu: &mut Gpu,
    mut draw: ProgrammableDraw,
    topology: Topology,
    depth: DepthFormat,
    filter: Filter,
) -> Vec<u8> {
    let target = gpu
        .target(16, 12, false)
        .unwrap()
        .with_depth(Some(gpu.depth(16, 12, depth).unwrap()))
        .unwrap();
    gpu.clear_with_stencil(
        &target,
        Some([0.125, 0.25, 0.375, 0.5]),
        Some(1.),
        depth.has_stencil().then_some(0),
    )
    .unwrap();
    let texture = gpu.image_levels(2, 2, 2).unwrap();
    let pixels = [
        13, 77, 241, 192, 201, 101, 33, 128, 177, 211, 55, 64, 91, 141, 131, 255,
    ];
    gpu.upload(
        &texture,
        Rect {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
        },
        8,
        &pixels,
    )
    .unwrap();
    let texture = texture.mip_level(0).unwrap();
    let mut state = DrawState::new(16, 12);
    state.topology = topology;
    state.viewport = Viewport {
        x: 1.,
        y: 1.,
        width: 14.,
        height: 10.,
        minimum_depth: 0.1,
        maximum_depth: 0.9,
    };
    state.depth = Some(Compare::LessEqual);
    state.depth_write = true;
    state.blend = Some(Blend {
        source: Factor::SourceAlpha,
        destination: Factor::OneMinusSourceAlpha,
        operation: BlendOp::Add,
    });
    state.alpha_blend = Some(Blend {
        source: Factor::One,
        destination: Factor::Zero,
        operation: BlendOp::Add,
    });
    state.scissor = Some(Rect {
        x: 2,
        y: 2,
        w: 12,
        h: 8,
    });
    state.sampler.min = filter;
    state.sampler.mag = filter;
    if depth.has_stencil() {
        let face = StencilFace {
            compare: Compare::Always,
            fail: StencilOp::Keep,
            depth_fail: StencilOp::Keep,
            pass: StencilOp::Replace,
        };
        state.stencil = Some(Stencil {
            front: face,
            back: face,
            read_mask: 255,
            write_mask: 255,
        });
        state.stencil_reference = 3;
    }
    draw.textures = vec![ProgramTexture {
        image: Some(texture.clone()),
        sampler: state.sampler,
    }];
    gpu.set_clip(Some(Rect {
        x: 3,
        y: 1,
        w: 10,
        h: 10,
    }));
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    gpu.set_clip(None);
    let copy = gpu.image(16, 12).unwrap();
    gpu.copy(
        &target.image,
        Rect {
            x: 0,
            y: 0,
            w: 16,
            h: 12,
        },
        &copy,
        0,
        0,
    )
    .unwrap();
    draw.textures[0].image = Some(copy);
    draw.pixel_constants.floats[0] = [0.25, 0.75, 0.5, 1.];
    state.color_write = 13;
    if let Some(stencil) = &mut state.stencil {
        stencil.front.compare = Compare::Equal;
        stencil.back.compare = Compare::Equal;
    }
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    // Exercise a wgpu upload after a native sample of the same allocation.
    gpu.upload(
        &texture,
        Rect {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
        },
        8,
        &[255u8; 16],
    )
    .unwrap();
    draw.textures[0].image = Some(texture);
    state.depth = Some(Compare::Greater);
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    read(gpu, &target)
}
// spec: GFX-METAL-1
#[test]
fn native_states_topologies_streams_indices_and_up_match_wgpu_exactly() {
    let program = Program::new(r#"
struct C { f:array<vec4<f32>,256> }; @group(0) @binding(1) var<uniform> c:C;
@group(1) @binding(0) var image:texture_2d<f32>; @group(1) @binding(1) var samp:sampler;
struct V { @builtin(position) p:vec4<f32>, @location(0) uv:vec2<f32>, @location(1) color:vec4<f32> };
@vertex fn vertex_main(@location(0) p:vec4<f32>,@location(1) uv:vec2<f32>,@location(2) color:vec4<f32>)->V { return V(p,uv,color); }
@fragment fn fragment_main(v:V)->@location(0) vec4<f32> { return textureSampleLevel(image,samp,v.uv,0.)*v.color*c.f[0]; }
"#.into(),vec![false]);
    let mut native = gpu(true);
    let mut reference = gpu(false);
    for topology in [
        Topology::Points,
        Topology::Lines,
        Topology::LineStrip,
        Topology::Triangles,
        Topology::TriangleStrip,
    ] {
        for indexed in [false, true] {
            for (depth, stride, filter) in [
                (DepthFormat::Float32, 32, Filter::Nearest),
                (DepthFormat::Unorm16, 48, Filter::Linear),
                (DepthFormat::Depth24Stencil8, 32, Filter::Linear),
            ] {
                let draw = textured_draw(&program, stride, indexed, topology);
                let actual = render_states(&mut native, draw.clone(), topology, depth, filter);
                let expected = render_states(&mut reference, draw, topology, depth, filter);
                assert_eq!(
                    actual, expected,
                    "{topology:?} indexed={indexed} depth={depth:?} stride={stride}"
                );
            }
        }
    }
}

// spec: GFX-METAL-1
#[test]
fn ring_wraps_and_busy_leaves_draw_state_and_uploads_unchanged() {
    let mut gpu = gpu(true);
    let target = gpu.target(2, 2, false).unwrap();
    gpu.clear(&target, Some([0.; 4]), None).unwrap();
    read(&mut gpu, &target);
    let wake = adv_sync::WakeEvent::new().unwrap();
    assert!(gpu.context.set_execution_wake(wake.clone()).is_ok());
    let (device, queue) = {
        let pending = gpu.context.pending.lock().unwrap();
        let metal = pending.metal.as_ref().unwrap();
        (metal.device.clone(), metal.queue.clone())
    };
    struct Release(Object<dyn MTLSharedEvent>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.setSignaledValue(1);
        }
    }
    let release = Release(device.newSharedEvent().unwrap());
    let blocker = queue.commandBuffer().unwrap();
    blocker.encodeWaitForEvent_value(release.0.as_ref(), 1);
    blocker.commit();
    let mut draw = draw();
    draw.streams[0].bytes.resize(16 * 1024 * 1024, 0);
    draw.pixel_constants.floats[0] = [1., 0., 0., 1.];
    let state = DrawState::new(2, 2);
    let mut busy = false;
    for _ in 0..8 {
        match gpu.draw_programmable(&target, &draw, &state) {
            Ok(()) => {}
            Err(Error::Busy) => {
                busy = true;
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    let before = gpu.stats();
    let head = gpu
        .context
        .pending
        .lock()
        .unwrap()
        .metal
        .as_ref()
        .unwrap()
        .ring
        .head;
    let result = gpu.draw_programmable(&target, &draw, &state);
    let after = gpu.stats();
    let after_head = gpu
        .context
        .pending
        .lock()
        .unwrap()
        .metal
        .as_ref()
        .unwrap()
        .ring
        .head;
    drop(release);
    assert!(busy);
    assert!(matches!(result, Err(Error::Busy)));
    assert_eq!(before.draws, after.draws);
    assert_eq!(before.submissions, after.submissions);
    assert_eq!(before.ring_bytes, after.ring_bytes);
    assert_eq!(head, after_head);
    assert_eq!(after.ring_busy, before.ring_busy + 1);
    assert!(wake.wait(Some(std::time::Duration::from_secs(10))).unwrap());
    assert_eq!(read(&mut gpu, &target), [0, 0, 255, 255].repeat(4));
    for n in 0..8 {
        let green = n % 2 == 0;
        draw.pixel_constants.floats[0] = if green {
            [0., 1., 0., 1.]
        } else {
            [0., 0., 1., 1.]
        };
        gpu.draw_programmable(&target, &draw, &state).unwrap();
        assert_eq!(
            read(&mut gpu, &target),
            if green {
                [0, 255, 0, 255].repeat(4)
            } else {
                [255, 0, 0, 255].repeat(4)
            }
        );
    }
    assert!(gpu.stats().ring_bytes > CAPACITY * 2);
}

// spec: GFX-METAL-1
#[test]
fn small_draws_keep_batching_in_the_second_half_of_the_ring() {
    let mut gpu = gpu(true);
    let target = gpu.target(1, 1, false).unwrap();
    let state = DrawState::new(1, 1);
    let mut large = draw();
    large.streams[0].bytes.resize(16 * 1024 * 1024, 0);
    large.pixel_constants.floats[0] = [1.; 4];
    for _ in 0..2 {
        gpu.draw_programmable(&target, &large, &state).unwrap();
        read(&mut gpu, &target);
    }
    assert!(
        gpu.context
            .pending
            .lock()
            .unwrap()
            .metal
            .as_ref()
            .unwrap()
            .ring
            .head
            % CAPACITY
            > CAPACITY / 2
    );
    let mut small = draw();
    small.pixel_constants.floats[0] = [1.; 4];
    let before = gpu.stats();
    for _ in 0..64 {
        gpu.draw_programmable(&target, &small, &state).unwrap();
    }
    assert_eq!(
        gpu.stats().submissions - before.submissions,
        3,
        "one wgpu boundary and two native batches"
    );
    assert_eq!(read(&mut gpu, &target), [255; 4]);
}

// spec: GFX-METAL-1
#[test]
fn vertex_and_fragment_sampler_slots_are_independent() {
    let program = Program::new(r#"
@group(1) @binding(0) var pixel_image:texture_2d<f32>; @group(1) @binding(1) var pixel_sampler:sampler;
@group(1) @binding(32) var vertex_image:texture_2d<f32>; @group(1) @binding(33) var vertex_sampler:sampler;
struct V { @builtin(position) p:vec4<f32>, @location(0) color:vec4<f32> };
@vertex fn vertex_main(@location(0) p:vec4<f32>)->V { return V(p,textureSampleLevel(vertex_image,vertex_sampler,vec2(0.5),0.)); }
@fragment fn fragment_main(v:V)->@location(0) vec4<f32> { return v.color*textureSampleLevel(pixel_image,pixel_sampler,vec2(0.5),0.); }
"#.into(), [vec![false;16],vec![true]].concat());
    let render = |native| {
        let mut gpu = gpu(native);
        let target = gpu.target(2, 2, false).unwrap();
        let mut draw = draw();
        draw.program = program.clone();
        let state = DrawState::new(2, 2);
        draw.textures = (0..17)
            .map(|n| {
                let image = gpu.image(1, 1).unwrap();
                gpu.upload(
                    &image,
                    Rect {
                        x: 0,
                        y: 0,
                        w: 1,
                        h: 1,
                    },
                    4,
                    &[64 + n * 7, 192 - n * 7, 128, 255],
                )
                .unwrap();
                ProgramTexture {
                    image: Some(image),
                    sampler: state.sampler,
                }
            })
            .collect();
        gpu.draw_programmable(&target, &draw, &state).unwrap();
        read(&mut gpu, &target)
    };
    assert_eq!(render(true), render(false));
}

// spec: GFX-METAL-1, GFX-PASS-1
#[test]
fn unsupported_shader_bindings_return_an_error_without_a_draw() {
    let mut gpu = gpu(true);
    let target = gpu.target(2, 2, false).unwrap();
    gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
    let mut draw = draw();
    draw.program = Program::new(
        draw.program
            .source
            .replace("@group(0) @binding(1)", "@group(2) @binding(1)")
            .into(),
        vec![],
    );
    assert!(
        gpu.draw_programmable(&target, &draw, &DrawState::new(2, 2))
            .is_err()
    );
    assert_eq!(gpu.stats().metal_draws, 0);
    assert_eq!(gpu.stats().ring_bytes, 0);
    assert_eq!(read(&mut gpu, &target), [0, 0, 255, 255].repeat(4));
}

// spec: GFX-CACHE-7, GFX-CACHE-8, GFX-CACHE-10, GFX-METAL-1
#[test]
fn native_warm_restart_prebuilds_exact_pipelines_for_all_depth_formats() {
    let directory = std::env::temp_dir().join(format!(
        "adv-metal-warm-{}-{}",
        std::process::id(),
        NEXT_RESOURCE.fetch_add(1, Ordering::Relaxed)
    ));
    let formats = [
        None,
        Some(DepthFormat::Float32),
        Some(DepthFormat::Unorm16),
        Some(DepthFormat::Depth24Stencil8),
    ];
    let mut expected = Vec::new();
    let mut cold = gpu(true);
    cold.set_pipeline_cache_directory(Some(&directory));
    let mut command = draw();
    command.pixel_constants.floats[0] = [0.13, 0.71, 0.29, 0.53];
    for format in formats {
        let target = cold
            .target(4, 4, false)
            .unwrap()
            .with_depth(format.map(|f| cold.depth(4, 4, f).unwrap()))
            .unwrap();
        let mut state = DrawState::new(4, 4);
        state.depth = format.map(|_| Compare::Always);
        state.depth_write = format.is_some();
        cold.draw_programmable(&target, &command, &state).unwrap();
        expected.push(read(&mut cold, &target));
    }
    cold.flush_pipeline_cache();
    assert_eq!(cold.stats().persisted_pipelines, 4);
    assert_eq!(cold.stats().prebuild_misses, 4);
    assert!(cold.stats().compile_waits > 0);
    drop(cold);
    let mut hot = gpu(true);
    hot.set_pipeline_cache_directory(Some(&directory));
    hot.programmable.warm.drain();
    command.program = Program::new(command.program.source.clone(), vec![]);
    for (format, pixels) in formats.into_iter().zip(expected) {
        let target = hot
            .target(4, 4, false)
            .unwrap()
            .with_depth(format.map(|f| hot.depth(4, 4, f).unwrap()))
            .unwrap();
        let mut state = DrawState::new(4, 4);
        state.depth = format.map(|_| Compare::Always);
        state.depth_write = format.is_some();
        hot.set_clip(Some(Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        }));
        hot.draw_programmable(&target, &command, &state).unwrap();
        hot.set_clip(None);
        hot.draw_programmable(&target, &command, &state).unwrap();
        assert_eq!(read(&mut hot, &target), pixels);
        command.program = Program::new(command.program.source.clone(), vec![]);
        hot.draw_programmable(&target, &command, &state).unwrap();
        assert_eq!(read(&mut hot, &target), pixels);
    }
    assert_eq!(hot.stats().metal_draws, 8);
    assert_eq!(hot.stats().compile_waits, 0);
    assert_eq!(hot.stats().prebuild_hits, 8);
    assert_eq!(hot.stats().prebuild_misses, 0);
    hot.set_pipeline_cache_directory(None);
    std::fs::remove_dir_all(directory).unwrap();
}

// spec: GFX-METAL-1
#[test]
fn backend_switch_reasons_partition_the_total() {
    let mut gpu = gpu(true);
    let target = gpu.target(4, 4, false).unwrap();
    let image = gpu.image(4, 4).unwrap();
    let region = Rect {
        x: 0,
        y: 0,
        w: 2,
        h: 2,
    };
    let state = DrawState::new(4, 4);
    let mut command = draw();
    command.pixel_constants.floats[0] = [1.; 4];
    for reason in [
        SwitchReason::PartialClear,
        SwitchReason::Copy,
        SwitchReason::Upload,
        SwitchReason::FixedDraw,
        SwitchReason::Readback,
        SwitchReason::Other,
    ] {
        gpu.draw_programmable(&target, &command, &state).unwrap();
        let before = gpu.stats();
        match reason {
            SwitchReason::PartialClear => gpu
                .clear_region(&target, region, Some([0.; 4]), None, None)
                .unwrap(),
            SwitchReason::Copy => gpu.copy(&target.image, region, &image, 0, 0).unwrap(),
            SwitchReason::Upload => gpu.upload(&image, region, 8, &[255; 16]).unwrap(),
            SwitchReason::FixedDraw => gpu
                .draw(
                    &target,
                    &[Vertex {
                        position: [0., 0., 0., 1.],
                        color: [1.; 4],
                        uv: [0.; 2],
                    }; 3],
                    None,
                    state,
                )
                .unwrap(),
            SwitchReason::Readback => {
                read(&mut gpu, &target);
            }
            SwitchReason::Other => {
                gpu.context.submission().unwrap().submit([]);
            }
        }
        let after = gpu.stats();
        assert_eq!(
            after.backend_switches_by_reason[reason as usize],
            before.backend_switches_by_reason[reason as usize] + 1
        );
        assert_eq!(
            after.backend_switches_by_reason.iter().sum::<u64>(),
            after.backend_switches
        );
    }
}
