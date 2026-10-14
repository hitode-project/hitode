//! Mip views share one physical image; every transfer uses its selected level.
#![cfg(feature = "gpu")]
use adv_device::{gpu::*, surface::Rect};
use std::time::Duration;

fn read(gpu: &mut Gpu, image: &Image) -> Vec<u8> {
    let mut readback = gpu.readback(image, None).unwrap();
    gpu.context()
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(5)),
        })
        .unwrap();
    gpu.check().unwrap();
    readback.poll().expect("completed map").unwrap()
}
fn quad() -> [Vertex; 6] {
    let vertex = |x, y, u, v| Vertex {
        position: [x, y, 0., 1.],
        color: [1.; 4],
        uv: [u, v],
    };
    [
        vertex(-1., 1., 0., 0.),
        vertex(-1., -1., 0., 1.),
        vertex(1., 1., 1., 0.),
        vertex(1., 1., 1., 0.),
        vertex(-1., -1., 0., 1.),
        vertex(1., -1., 1., 1.),
    ]
}

#[test]
fn shared_mip_views_clear_upload_copy_and_read_the_actual_subresource() {
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    let chain = gpu.target_levels(8, 4, 4).unwrap();
    assert_eq!(chain.image.mip_levels(), 4);
    assert!(chain.mip_level(4).is_err());
    assert!(gpu.target_levels(8, 4, 0).is_err());
    assert!(gpu.target_levels(8, 4, 5).is_err());
    let rgba = [
        [1., 0., 0., 1.],
        [0., 1., 0., 1.],
        [0., 0., 1., 1.],
        [1., 1., 1., 1.],
    ];
    let bgra = [
        [0, 0, 255, 255],
        [0, 255, 0, 255],
        [255, 0, 0, 255],
        [255, 255, 255, 255],
    ];
    for level in 0..4 {
        let view = chain.mip_level(level).unwrap();
        assert!(chain.image.shares_storage(&view.image));
        assert_eq!(
            (view.image.width(), view.image.height()),
            ((8 >> level).max(1), (4 >> level).max(1))
        );
        gpu.clear(&view, Some(rgba[level as usize]), None).unwrap();
    }
    for level in 0..4 {
        let image = chain.image.mip_level(level).unwrap();
        assert_eq!(
            read(&mut gpu, &image),
            bgra[level as usize].repeat((image.width() * image.height()) as usize)
        );
    }
    let view = chain.image.mip_level(1).unwrap();
    gpu.upload(
        &view,
        Rect {
            x: 1,
            y: 0,
            w: 2,
            h: 2,
        },
        12,
        &[33, 22, 11, 255].repeat(6),
    )
    .unwrap();
    let pixels = read(&mut gpu, &view);
    for y in 0..2 {
        for x in 0..4 {
            let expected = if (1..=2).contains(&x) {
                [33, 22, 11, 255]
            } else {
                bgra[1]
            };
            assert_eq!(&pixels[((y * 4 + x) * 4)..((y * 4 + x) * 4 + 4)], &expected);
        }
    }
    let copied = gpu.image(4, 2).unwrap();
    gpu.copy(
        &view,
        Rect {
            x: 0,
            y: 0,
            w: 4,
            h: 2,
        },
        &copied,
        0,
        0,
    )
    .unwrap();
    assert_eq!(read(&mut gpu, &copied), pixels);
    assert_eq!(read(&mut gpu, &chain.image), bgra[0].repeat(32));
}

#[test]
fn minification_and_clamped_first_level_sample_written_lower_mips() {
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    let chain = gpu.target_levels(4, 4, 3).unwrap();
    for (level, color) in [[1., 0., 0., 1.], [0., 1., 0., 1.], [0., 0., 1., 1.]]
        .into_iter()
        .enumerate()
    {
        gpu.clear(&chain.mip_level(level as u32).unwrap(), Some(color), None)
            .unwrap();
    }
    let target = gpu.target(1, 1, false).unwrap();
    let mut state = DrawState::new(1, 1);
    state.texture_op = TextureOp::Texture;
    state.alpha_texture_op = TextureOp::Texture;
    state.sampler.mip = Some(Filter::Nearest);
    gpu.draw(&target, &quad(), Some(&chain.image), state)
        .unwrap();
    assert_eq!(read(&mut gpu, &target.image), [255, 0, 0, 255]);
    state.sampler.mip = None;
    gpu.draw(&target, &quad(), Some(&chain.image), state)
        .unwrap();
    assert_eq!(read(&mut gpu, &target.image), [0, 0, 255, 255]);
    state.sampler.first_level = u32::MAX;
    gpu.draw(&target, &quad(), Some(&chain.image), state)
        .unwrap();
    assert_eq!(read(&mut gpu, &target.image), [255, 0, 0, 255]);
    let linear_target = gpu.target(2, 2, false).unwrap();
    state = DrawState::new(2, 2);
    state.texture_op = TextureOp::Texture;
    state.alpha_texture_op = TextureOp::Texture;
    state.sampler.mip = Some(Filter::Linear);
    let mut vertices = quad();
    for vertex in &mut vertices {
        for uv in &mut vertex.uv {
            *uv *= std::f32::consts::SQRT_2;
        }
    }
    gpu.draw(&linear_target, &vertices, Some(&chain.image), state)
        .unwrap();
    for pixel in read(&mut gpu, &linear_target.image).as_chunks::<4>().0 {
        assert!(
            pixel[0].abs_diff(128) <= 2
                && pixel[1].abs_diff(128) <= 2
                && pixel[2] == 0
                && pixel[3] == 255,
            "trilinear mip blend: {pixel:?}"
        );
    }
    // Sampling the complete chain overlaps this attachment, even at another
    // level. Reject before command encoding rather than poisoning the device.
    assert!(
        gpu.draw(
            &chain.mip_level(1).unwrap(),
            &quad(),
            Some(&chain.image),
            DrawState::new(2, 2)
        )
        .is_err()
    );
    assert!(gpu.check().is_ok());
}

#[test]
// spec: DEV-DEPTH-2
fn replacing_a_public_target_image_revalidates_the_depth_attachment() {
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    let original = gpu.target(4, 4, true).unwrap();
    let mut bad = original.clone();
    // Larger depth attachments are supported; a smaller one cannot cover the color target.
    bad.image = gpu.image(8, 8).unwrap();
    assert!(gpu.clear(&bad, Some([0.; 4]), Some(1.)).is_err());
    assert!(gpu.draw(&bad, &quad(), None, DrawState::new(8, 8)).is_err());
    let other = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    bad.image = other.image(4, 4).unwrap();
    assert!(gpu.clear(&bad, Some([0.; 4]), Some(1.)).is_err());
    assert!(gpu.draw(&bad, &quad(), None, DrawState::new(4, 4)).is_err());
    assert!(gpu.check().is_ok());
    gpu.clear(&original, Some([1., 0., 0., 1.]), Some(1.))
        .unwrap();
    assert_eq!(read(&mut gpu, &original.image), [0, 0, 255, 255].repeat(16));
}
