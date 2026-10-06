//! Arbitrary border colors must work without backend clamp-to-border features.
#![cfg(feature = "gpu")]
use adv_device::{gpu::*, surface::Rect};
use std::time::Duration;

fn read(gpu: &mut Gpu, image: &Image) -> [u8; 4] {
    let mut readback = gpu.readback(image, None).unwrap();
    gpu.context()
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(5)),
        })
        .unwrap();
    gpu.check().unwrap();
    readback.poll().unwrap().unwrap().try_into().unwrap()
}
fn quad(center: [f32; 2], span: f32) -> [Vertex; 6] {
    let vertex = |x: f32, y: f32| Vertex {
        position: [x, y, 0., 1.],
        color: [1.; 4],
        uv: [center[0] + x * span * 0.5, center[1] - y * span * 0.5],
    };
    [
        vertex(-1., 1.),
        vertex(-1., -1.),
        vertex(1., 1.),
        vertex(1., 1.),
        vertex(-1., -1.),
        vertex(1., -1.),
    ]
}
fn sample(gpu: &mut Gpu, image: &Image, sampler: Sampler, uv: [f32; 2], span: f32) -> [u8; 4] {
    let target = gpu.target(1, 1, false).unwrap();
    let mut state = DrawState::new(1, 1);
    state.texture_op = TextureOp::Texture;
    state.alpha_texture_op = TextureOp::Texture;
    state.sampler = sampler;
    gpu.draw(&target, &quad(uv, span), Some(image), state)
        .unwrap();
    read(gpu, &target.image)
}
fn near(actual: [u8; 4], expected: [u8; 4]) {
    assert!(
        actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1),
        "BGRA {actual:?}, expected {expected:?}"
    );
}

fn verify(backend: GpuBackend) {
    let mut gpu = pollster::block_on(Gpu::request_backend(false, backend)).unwrap();
    let image = gpu.image(2, 2).unwrap();
    // Top row red/blue, bottom row green/white.
    gpu.upload(
        &image,
        Rect {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
        },
        8,
        &[
            0, 0, 255, 255, 255, 0, 0, 255, 0, 255, 0, 255, 255, 255, 255, 255,
        ],
    )
    .unwrap();
    let mut sampler = DrawState::new(1, 1).sampler;
    sampler.u = Address::Border;
    sampler.v = Address::Border;
    sampler.border_color = 0x8040_2010; // RGBA [16,32,64,128].
    assert_eq!(
        sample(&mut gpu, &image, sampler, [-0.1, 0.25], 0.),
        [64, 32, 16, 128]
    );
    assert_eq!(
        sample(&mut gpu, &image, sampler, [1., 0.25], 0.),
        [64, 32, 16, 128]
    );
    assert_eq!(
        sample(&mut gpu, &image, sampler, [0.25, 0.25], 0.),
        [0, 0, 255, 255]
    );
    sampler.mag = Filter::Linear;
    near(
        sample(&mut gpu, &image, sampler, [0., 0.25], 0.),
        [32, 16, 136, 192],
    );
    near(
        sample(&mut gpu, &image, sampler, [0., 0.], 0.),
        [48, 24, 76, 160],
    );
    sampler.v = Address::Repeat;
    near(
        sample(&mut gpu, &image, sampler, [0., 1.25], 0.),
        [32, 16, 136, 192],
    );
    sampler.v = Address::Mirror;
    near(
        sample(&mut gpu, &image, sampler, [0., 1.25], 0.),
        [32, 144, 8, 192],
    );
    sampler.v = Address::Clamp;
    near(
        sample(&mut gpu, &image, sampler, [0., -1.], 0.),
        [32, 16, 136, 192],
    );
    let mut opaque_target = gpu.target(2, 2, false).unwrap();
    opaque_target.image = image.clone();
    let opaque = opaque_target.with_alpha_mode(AlphaMode::Opaque).image;
    // Native XRGB treats even arbitrary border alpha as implicitly one.
    assert_eq!(
        sample(&mut gpu, &opaque, sampler, [-1., 0.25], 0.),
        [64, 32, 16, 255]
    );
    near(
        sample(&mut gpu, &opaque, sampler, [0., 0.25], 0.),
        [32, 16, 136, 255],
    );

    let chain = gpu.target_levels(4, 4, 3).unwrap();
    for (level, color) in [[1., 0., 0., 1.], [0., 1., 0., 1.], [0., 0., 1., 1.]]
        .into_iter()
        .enumerate()
    {
        gpu.clear(&chain.mip_level(level as u32).unwrap(), Some(color), None)
            .unwrap();
    }
    sampler.v = Address::Clamp;
    sampler.border_color = 0xff00_0000;
    sampler.min = Filter::Nearest;
    // Positive LOD selects MINFILTER even though MAGFILTER is linear.
    assert_eq!(
        sample(&mut gpu, &chain.image, sampler, [0., 0.5], 1.),
        [0, 0, 255, 255]
    );
    sampler.min = Filter::Linear;
    sampler.mip = Some(Filter::Linear);
    near(
        sample(
            &mut gpu,
            &chain.image,
            sampler,
            [0., 0.5],
            std::f32::consts::FRAC_1_SQRT_2,
        ),
        [64, 64, 0, 255],
    );
    sampler.mip = Some(Filter::Nearest);
    near(
        sample(&mut gpu, &chain.image, sampler, [0., 0.5], 1.),
        [128, 0, 0, 255],
    );
    sampler.mip = None;
    sampler.first_level = 1;
    sampler.min = Filter::Nearest;
    // Clamping to a coarser mip preserves magnification's linear filter.
    near(
        sample(&mut gpu, &chain.image, sampler, [0., 0.5], 0.),
        [0, 128, 0, 255],
    );
    assert!(gpu.check().is_ok());
}
#[test]
fn border_nearest_linear_mixed_address_and_mips_are_portable() {
    verify(GpuBackend::Vulkan);
    verify(GpuBackend::Gl);
}
