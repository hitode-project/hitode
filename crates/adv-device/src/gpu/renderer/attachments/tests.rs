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
fn larger_depth_clear_preserves_outside_depth_and_stencil_across_target_switches() {
    let mut gpu = pollster::block_on(Gpu::request(true)).unwrap();
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
fn padded_mip_draw_preserves_colour_reads_current_storage_and_reuses_one_scratch() {
    let mut gpu = pollster::block_on(Gpu::request(true)).unwrap();
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
    let mut gpu = pollster::block_on(Gpu::request(true)).unwrap();
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
