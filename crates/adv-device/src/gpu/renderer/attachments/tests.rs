use super::*;
use std::time::Duration;

fn read(gpu: &mut Gpu, target: &Target) -> Vec<u8> {
    let mut pending = gpu.readback(&target.image, None).unwrap();
    gpu.context
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })
        .unwrap();
    pending.poll().unwrap().unwrap()
}
fn quad(z: f32, color: [f32; 4]) -> [Vertex; 6] {
    let v = |x, y| Vertex {
        position: [x, y, z, 1.],
        color,
        uv: [0.; 2],
    };
    [
        v(-1., 1.),
        v(-1., -1.),
        v(1., 1.),
        v(1., 1.),
        v(-1., -1.),
        v(1., -1.),
    ]
}
fn stencil(reference: u32) -> DrawState {
    let mut state = DrawState::new(8, 6);
    let face = StencilFace {
        compare: Compare::Equal,
        fail: StencilOp::Keep,
        depth_fail: StencilOp::Keep,
        pass: StencilOp::Keep,
    };
    state.stencil = Some(Stencil {
        front: face,
        back: face,
        read_mask: 255,
        write_mask: 255,
    });
    state.stencil_reference = reference;
    state
}

#[test]
// spec: DEV-DEPTH-1
fn larger_depth_clear_preserves_outside_depth_and_stencil_across_target_switches() {
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    let depth = gpu.depth(8, 6, DepthFormat::Depth24Stencil8).unwrap();
    let large = gpu
        .target(8, 6, false)
        .unwrap()
        .with_depth(Some(depth.clone()))
        .unwrap();
    let small = gpu
        .target(4, 4, false)
        .unwrap()
        .with_depth(Some(depth))
        .unwrap();
    gpu.clear_with_stencil(&large, Some([0., 0., 0., 1.]), Some(0.75), Some(3))
        .unwrap();
    let before = gpu.stats();
    gpu.clear_with_stencil(&small, Some([1., 0., 0., 0.5]), Some(0.25), Some(7))
        .unwrap();
    assert_eq!(gpu.stats().submissions, before.submissions + 1);
    assert_eq!(gpu.stats().readbacks, before.readbacks);
    assert!(
        read(&mut gpu, &small)
            .chunks_exact(4)
            .all(|pixel| pixel == [0, 0, 255, 128])
    );
    let cache = gpu.attachment_scratch.as_ref().unwrap().id;
    gpu.clear(&large, Some([0., 0., 0., 1.]), None).unwrap();
    let mut state = DrawState::new(8, 6);
    state.depth = Some(Compare::Less);
    gpu.draw(&large, &quad(0.5, [0., 1., 0., 1.]), None, state)
        .unwrap();
    let pixels = read(&mut gpu, &large);
    for y in 0..6 {
        for x in 0..8 {
            let expected = if x < 4 && y < 4 {
                [0, 0, 0, 255]
            } else {
                [0, 255, 0, 255]
            };
            assert_eq!(&pixels[(y * 8 + x) * 4..(y * 8 + x + 1) * 4], &expected);
        }
    }
    gpu.clear(&large, Some([0., 0., 0., 1.]), None).unwrap();
    gpu.draw(&large, &quad(0.5, [0., 0., 1., 1.]), None, stencil(7))
        .unwrap();
    let pixels = read(&mut gpu, &large);
    for y in 0..6 {
        for x in 0..8 {
            let expected = if x < 4 && y < 4 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 0, 255]
            };
            assert_eq!(&pixels[(y * 8 + x) * 4..(y * 8 + x + 1) * 4], &expected);
        }
    }
    gpu.clear(&small, Some([0., 1., 1., 1.]), None).unwrap();
    assert_eq!(gpu.attachment_scratch.as_ref().unwrap().id, cache);
    assert!(
        read(&mut gpu, &small)
            .chunks_exact(4)
            .all(|pixel| pixel == [255, 255, 0, 255])
    );
}

#[test]
// spec: DEV-DEPTH-2
fn padded_mip_draw_preserves_colour_reads_current_storage_and_reuses_one_scratch() {
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    let base = gpu.target_levels(8, 8, 3).unwrap();
    gpu.clear(&base, Some([1., 0., 0., 1.]), None).unwrap();
    let mip = base
        .mip_level(1)
        .unwrap()
        .with_depth(Some(gpu.depth(8, 6, DepthFormat::Unorm16).unwrap()))
        .unwrap();
    gpu.clear(&mip, Some([0., 0., 1., 1.]), Some(1.)).unwrap();
    let id = gpu.attachment_scratch.as_ref().unwrap().id;
    let mut state = DrawState::new(4, 4);
    state.depth = Some(Compare::Less);
    state.depth_write = true;
    state.scissor = Some(Rect {
        x: 1,
        y: 1,
        w: 2,
        h: 2,
    });
    gpu.draw(&mip, &quad(0.5, [0., 1., 0., 1.]), None, state)
        .unwrap();
    let pixels = read(&mut gpu, &mip);
    for y in 0..4 {
        for x in 0..4 {
            let expected = if (1..3).contains(&x) && (1..3).contains(&y) {
                [0, 255, 0, 255]
            } else {
                [255, 0, 0, 255]
            };
            assert_eq!(&pixels[(y * 4 + x) * 4..(y * 4 + x + 1) * 4], &expected);
        }
    }
    assert!(
        read(&mut gpu, &base)
            .chunks_exact(4)
            .all(|pixel| pixel == [0, 0, 255, 255])
    );
    assert_eq!(gpu.attachment_scratch.as_ref().unwrap().id, id);
    let before = gpu.stats();
    assert!(
        gpu.target(9, 6, false)
            .unwrap()
            .with_depth(mip.depth.clone())
            .is_err()
    );
    assert!(gpu.clear(&mip, None, Some(f32::NAN)).is_err());
    assert_eq!(gpu.stats(), before);
}

#[test]
fn programmable_draw_uses_logical_viewport_and_shared_depth_without_losing_color() {
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    let target = gpu
        .target(4, 4, false)
        .unwrap()
        .with_depth(Some(gpu.depth(8, 6, DepthFormat::Unorm16).unwrap()))
        .unwrap();
    let draw = ProgrammableDraw {
        program: Program { source: "@vertex fn vertex_main(@builtin(vertex_index) i:u32) -> @builtin(position) vec4<f32> { let p=array<vec2<f32>,3>(vec2(-1.,1.),vec2(-1.,-3.),vec2(3.,1.)); return vec4(p[i],0.5,1.); } @fragment fn fragment_main() -> @location(0) vec4<f32> { return vec4(1.,0.,1.,1.); }".into(), textures: vec![] },
        streams: vec![], indices: None, vertex_count: 3, vertex_constants: ConstantBlock::default(), pixel_constants: ConstantBlock::default(), textures: vec![], parameters: [[0;4];40],
    };
    let mut state = DrawState::new(4, 4);
    state.depth = Some(Compare::Less);
    state.depth_write = true;
    gpu.clear(&target, Some([0., 1., 0., 1.]), Some(0.25))
        .unwrap();
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    assert!(
        read(&mut gpu, &target)
            .chunks_exact(4)
            .all(|pixel| pixel == [0, 255, 0, 255])
    );
    gpu.clear(&target, None, Some(0.75)).unwrap();
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    assert!(
        read(&mut gpu, &target)
            .chunks_exact(4)
            .all(|pixel| pixel == [255, 0, 255, 255])
    );
    let id = gpu.attachment_scratch.as_ref().unwrap().id;
    let other = gpu
        .target(2, 3, false)
        .unwrap()
        .with_depth(Some(gpu.depth(10, 8, DepthFormat::Float32).unwrap()))
        .unwrap();
    gpu.clear(&other, Some([0., 0., 1., 1.]), Some(1.)).unwrap();
    assert_ne!(gpu.attachment_scratch.as_ref().unwrap().id, id);
    assert_eq!(gpu.attachment_scratch.as_ref().unwrap().width, 10);
    assert!(
        read(&mut gpu, &target)
            .chunks_exact(4)
            .all(|pixel| pixel == [255, 0, 255, 255])
    );
}

#[test]
// spec: GFX-BUFFER-2
fn programmable_batches_preserve_each_upload_and_flush_large_draws_and_consumers() {
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    let target = gpu.target(64, 1, false).unwrap();
    gpu.clear(&target, Some([0.; 4]), None).unwrap();
    let before = gpu.stats().submissions;
    let mut draw = ProgrammableDraw {
        program: Program {
            source: "struct C { f: array<vec4<f32>,256> }; @group(0) @binding(1) var<uniform> c:C; struct V { @builtin(position) p:vec4<f32>, @location(0) color:vec4<f32> }; @vertex fn vertex_main(@builtin(vertex_index) i:u32, @location(0) color:vec4<f32>)->V { let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.)); return V(vec4(p[i%3],0.,1.),color); } @fragment fn fragment_main(v:V)->@location(0) vec4<f32> { return vec4(v.color.x,c.f[0].x,0.,1.); }".into(),
            textures: vec![],
        },
        streams: vec![VertexStream { bytes: vec![], stride: 16, attributes: vec![Attribute { location: 0, offset: 0, format: AttributeFormat::Float4 }] }],
        indices: Some(vec![]), vertex_count: 6, vertex_constants: ConstantBlock::default(),
        pixel_constants: ConstantBlock::default(), textures: vec![], parameters: [[0;4];40],
    };
    let mut state = DrawState::new(64, 1);
    for i in 0..64 {
        let red = (i % 4) as f32 / 3.;
        let vertices = [[red, 0., 0., 1.]; 3]
            .into_iter()
            .chain([[1. - red, 0., 0., 1.]; 3])
            .collect::<Vec<_>>();
        draw.streams[0].bytes = bytemuck::cast_slice(&vertices).to_vec();
        draw.indices = Some((0..3).map(|n| n + (i % 2) * 3).collect());
        draw.pixel_constants.floats[0][0] = i as f32 / 63.;
        state.scissor = Some(Rect {
            x: i,
            y: 0,
            w: 1,
            h: 1,
        });
        gpu.draw_programmable(&target, &draw, &state).unwrap();
    }
    assert_eq!(gpu.stats().submissions - before, 2);
    let pixels = read(&mut gpu, &target);
    for (i, pixel) in pixels.chunks_exact(4).enumerate() {
        let red = (i % 4) as f32 / 3.;
        let expected = [
            0.,
            i as f32 / 63. * 255.,
            if i % 2 == 0 { red } else { 1. - red } * 255.,
            255.,
        ];
        assert!(
            pixel
                .iter()
                .zip(expected)
                .all(|(&v, e)| (v as f32 - e).abs() <= 1.),
            "pixel {i}: {pixel:?}"
        );
    }
    draw.streams[0]
        .bytes
        .resize((MAX_UPLOAD / 32 + 16) as usize, 0);
    state.scissor = Some(Rect {
        x: 0,
        y: 0,
        w: 1,
        h: 1,
    });
    let before = gpu.stats().submissions;
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    assert_eq!(gpu.stats().submissions, before + 1);
    draw.streams[0].bytes.truncate(96);
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    assert_eq!(gpu.stats().submissions, before + 1);
    let frame = gpu.completed_frame(target.image.clone(), 1).unwrap();
    assert_eq!(gpu.stats().submissions, before + 2);
    drop(frame);
    let other = gpu.target(64, 1, false).unwrap();
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    gpu.draw_programmable(&other, &draw, &state).unwrap();
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    assert_eq!(&read(&mut gpu, &other)[..4], &[0, 255, 0, 255]);
    let before = gpu.stats().submissions;
    let mut child = gpu.fork().unwrap();
    gpu.draw_programmable(&target, &draw, &state).unwrap();
    child.draw_programmable(&target, &draw, &state).unwrap();
    assert_eq!(gpu.stats().submissions, before + 1);
    child.clear(&target, Some([0., 1., 0., 1.]), None).unwrap();
    assert_eq!(child.stats().submissions, 2);
    assert!(
        read(&mut gpu, &target)
            .chunks_exact(4)
            .all(|p| p == [0, 255, 0, 255])
    );
}
