#![cfg(feature = "gpu")]
//! Authored line pixels; no guest image, native window or game.
use adv_device::gpu::*;
use adv_device::surface::Rect;
#[path = "fixtures/line_ref.rs"]
mod reference;

const SIZE: u32 = 16;

fn vertex(x: f32, y: f32, color: [f32; 4], z: f32) -> Vertex {
    Vertex {
        position: [
            (x + 0.5) / SIZE as f32 * 2. - 1.,
            1. - (y + 0.5) / SIZE as f32 * 2.,
            z,
            1.,
        ],
        color,
        uv: [0., 0.],
    }
}

fn read(gpu: &mut Gpu, image: &Image) -> Vec<u8> {
    let mut ticket = gpu.readback(image, None).unwrap();
    ticket.take_completion_job().unwrap()();
    ticket.poll().unwrap().unwrap()
}

#[test]
fn integer_lines_match_reference_pixels_for_both_endpoint_states() {
    let mut gpu = pollster::block_on(Gpu::request(true)).unwrap();
    let target = gpu
        .target(SIZE, SIZE, false)
        .unwrap()
        .with_depth(Some(gpu.depth(SIZE, SIZE, DepthFormat::Unorm16).unwrap()))
        .unwrap();
    assert_eq!(reference::CASES.len(), 30);
    for case in reference::CASES {
        gpu.clear(&target, Some([0., 0., 0., 1.]), Some(0.5))
            .unwrap();
        let vertices: Vec<_> = case
            .vertices
            .iter()
            .map(|&(x, y, z, argb)| {
                vertex(x as f32 / 4., y as f32 / 4., rgba(argb), f32::from_bits(z))
            })
            .collect();
        let mut state = DrawState::new(SIZE, SIZE);
        state.topology = if case.strip {
            Topology::LineStrip
        } else {
            Topology::Lines
        };
        state.last_pixel = case.last_pixel;
        state.blend = case.blend.then_some(alpha_blend());
        state.depth = case.depth.then_some(Compare::Less);
        state.depth_write = case.depth;
        gpu.draw(&target, &vertices, None, state).unwrap();
        let pixels = read(&mut gpu, &target.image);
        let mut expected = vec![0xff000000u32; (SIZE * SIZE) as usize];
        for &(x, y, argb) in case.pixels {
            expected[(y * SIZE + x) as usize] = argb;
        }
        for (at, (actual, &expected)) in pixels.chunks_exact(4).zip(&expected).enumerate() {
            let expected = expected.to_le_bytes();
            assert_eq!(
                actual[..3].iter().any(|&c| c != 0),
                expected[..3].iter().any(|&c| c != 0),
                "{} last_pixel={} coverage at {},{}",
                case.name,
                case.last_pixel,
                at % SIZE as usize,
                at / SIZE as usize
            );
            assert!(
                actual
                    .iter()
                    .zip(expected)
                    .all(|(&a, e)| a.abs_diff(e) <= 1),
                "{} last_pixel={} pixel {},{} actual={actual:?} expected={expected:?}",
                case.name,
                case.last_pixel,
                at % SIZE as usize,
                at / SIZE as usize
            );
        }
    }
}

fn rgba(argb: u32) -> [f32; 4] {
    [
        (argb >> 16 & 255) as f32 / 255.,
        (argb >> 8 & 255) as f32 / 255.,
        (argb & 255) as f32 / 255.,
        (argb >> 24) as f32 / 255.,
    ]
}

fn alpha_blend() -> Blend {
    Blend {
        source: Factor::SourceAlpha,
        destination: Factor::OneMinusSourceAlpha,
        operation: BlendOp::Add,
    }
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * SIZE + x) * 4) as usize;
    pixels[at..at + 4].try_into().unwrap()
}

#[test]
fn endpoint_commands_preserve_list_order_scissor_alpha_test_and_stencil() {
    let mut gpu = pollster::block_on(Gpu::request(true)).unwrap();
    let target = gpu
        .target(SIZE, SIZE, false)
        .unwrap()
        .with_depth(Some(
            gpu.depth(SIZE, SIZE, DepthFormat::Depth24Stencil8).unwrap(),
        ))
        .unwrap();
    gpu.clear_with_stencil(&target, Some([0., 0., 0., 1.]), Some(1.), Some(0))
        .unwrap();
    let vertices = [
        vertex(2., 3., rgba(0x80ff0000), 0.5),
        vertex(7., 3., rgba(0x80ff0000), 0.5),
        vertex(7., 3., rgba(0x800000ff), 0.5),
        vertex(7., 8., rgba(0x800000ff), 0.5),
    ];
    let mut state = DrawState::new(SIZE, SIZE);
    state.topology = Topology::Lines;
    state.blend = Some(alpha_blend());
    let face = StencilFace {
        compare: Compare::Always,
        fail: StencilOp::Keep,
        depth_fail: StencilOp::Keep,
        pass: StencilOp::IncrementClamp,
    };
    state.stencil = Some(Stencil {
        front: face,
        back: face,
        read_mask: 255,
        write_mask: 255,
    });
    gpu.draw(&target, &vertices, None, state).unwrap();
    let pixels = read(&mut gpu, &target.image);
    let joint = pixel(&pixels, 7, 3);
    assert!(
        joint[0].abs_diff(128) <= 1 && joint[2].abs_diff(64) <= 1,
        "list endpoint must precede later blue segment: {joint:?}"
    );
    assert_eq!(&pixel(&pixels, 7, 8)[..3], &[128, 0, 0]);
    // Stencil increment occurs for both list contributions, including the point.
    gpu.clear(&target, Some([0., 0., 0., 1.]), None).unwrap();
    state.blend = None;
    state.stencil_reference = 2;
    let face = StencilFace {
        compare: Compare::Equal,
        pass: StencilOp::Keep,
        ..face
    };
    state.stencil = Some(Stencil {
        front: face,
        back: face,
        read_mask: 255,
        write_mask: 0,
    });
    gpu.draw(
        &target,
        &[vertex(2., 3., [1.; 4], 0.5), vertex(7., 3., [1.; 4], 0.5)],
        None,
        state,
    )
    .unwrap();
    let pixels = read(&mut gpu, &target.image);
    assert_eq!(pixel(&pixels, 7, 3), [255; 4]);
    assert_eq!(&pixel(&pixels, 6, 3)[..3], &[0; 3]);
    state.stencil = None;
    state.alpha_test = Compare::Greater;
    state.alpha_reference = 0.5;
    state.scissor = Some(Rect {
        x: 0,
        y: 0,
        w: 8,
        h: 4,
    });
    gpu.clear(&target, Some([0., 0., 0., 1.]), None).unwrap();
    gpu.draw(
        &target,
        &[
            vertex(2., 3., [1., 0., 0., 1.], 0.5),
            vertex(7., 3., [1., 0., 0., 0.25], 0.5),
        ],
        None,
        state,
    )
    .unwrap();
    assert_eq!(&pixel(&read(&mut gpu, &target.image), 7, 3)[..3], &[0; 3]);
    state.alpha_test = Compare::Always;
    state.scissor = Some(Rect {
        x: 0,
        y: 0,
        w: 7,
        h: 4,
    });
    gpu.draw(
        &target,
        &[vertex(2., 3., [1.; 4], 0.5), vertex(7., 3., [1.; 4], 0.5)],
        None,
        state,
    )
    .unwrap();
    assert_eq!(&pixel(&read(&mut gpu, &target.image), 7, 3)[..3], &[0; 3]);
}

#[test]
fn unsupported_inclusive_profiles_fail_before_submission_and_triangles_ignore_the_flag() {
    let mut gpu = pollster::block_on(Gpu::request(true)).unwrap();
    let target = gpu.target(SIZE, SIZE, false).unwrap();
    gpu.clear(&target, Some([0., 0., 0., 1.]), None).unwrap();
    let mut state = DrawState::new(SIZE, SIZE);
    state.topology = Topology::Lines;
    let segment = [vertex(2., 3., [1.; 4], 0.5), vertex(7., 3., [1.; 4], 0.5)];
    let mut subpixel = segment;
    subpixel[0] = vertex(2.25, 3.25, [1.; 4], 0.5);
    let mut perspective = segment;
    perspective[0].position[3] = 2.;
    let mut clipped_depth = segment;
    clipped_depth[1].position[2] = 2.;
    for vertices in [
        &subpixel[..],
        &perspective[..],
        &clipped_depth[..],
        &segment.repeat(4097)[..],
    ] {
        let before = gpu.stats();
        assert!(matches!(
            gpu.draw(&target, vertices, None, state),
            Err(Error::Unavailable(_))
        ));
        assert_eq!(gpu.stats().submissions, before.submissions);
    }
    let texture = gpu.target_levels(4, 4, 3).unwrap();
    state.sampler.mip = Some(Filter::Nearest);
    assert!(matches!(
        gpu.draw(&target, &segment, Some(&texture.image), state),
        Err(Error::Unavailable(_))
    ));
    state.sampler.mip = None;
    gpu.draw(&target, &segment, Some(&texture.image), state)
        .unwrap();
    state.sampler.mip = Some(Filter::Linear);
    let one_level = texture.mip_level(1).unwrap();
    gpu.draw(&target, &segment, Some(&one_level.image), state)
        .unwrap();
    state.topology = Topology::Triangles;
    let triangle = [
        vertex(2.25, 2.25, [1.; 4], 0.5),
        vertex(7.25, 2.25, [1.; 4], 0.5),
        vertex(2.25, 7.25, [1.; 4], 0.5),
    ];
    for last_pixel in [false, true] {
        state.last_pixel = last_pixel;
        gpu.clear(&target, Some([0., 0., 0., 1.]), None).unwrap();
        gpu.draw(&target, &triangle, None, state).unwrap();
        let pixels = read(&mut gpu, &target.image);
        assert_eq!(pixel(&pixels, 3, 3), [255; 4]);
    }
}
