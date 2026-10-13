//! Actual host-adapter rendering. Readback is explicit validation, never Present.
use std::sync::Arc;
use std::time::{Duration, Instant};

use adv_device::gpu::*;
use adv_device::surface::Rect;

fn read(gpu: &mut Gpu, image: &Image) -> Vec<u8> {
    let wake = Arc::new(adv_sync::WakeEvent::new().unwrap());
    let mut readback = gpu.readback(image, Some(wake.clone())).unwrap();
    // This blocking poll belongs only to a host test, not the guest execution path.
    gpu.context()
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })
        .unwrap();
    gpu.check().unwrap();
    assert!(
        wake.wait(Some(Duration::ZERO)).unwrap(),
        "callback signals the retained wake event"
    );
    let pixels = readback.poll().expect("map callback completed").unwrap();
    assert!(
        readback.poll().is_none(),
        "completion transfers ownership once"
    );
    pixels
}

#[test]
fn child_gpu_reuses_host_context_and_survives_parent_teardown() {
    let mut parent =
        pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).expect("test GPU adapter");
    parent.set_readback_limit(1).unwrap();
    let mut child = parent.fork().unwrap();
    let parent_target = parent.target(2, 1, false).unwrap();
    let child_target = child.target(2, 1, false).unwrap();
    assert!(!parent_target.image.shares_storage(&child_target.image));
    parent
        .clear(&parent_target, Some([1., 0., 0., 1.]), None)
        .unwrap();
    child
        .clear(&child_target, Some([0., 0., 1., 1.]), None)
        .unwrap();
    // The device context is identical: either command producer can explicitly
    // consume an image from it. Each frontend keeps its own transfer budget.
    let mut parent_transfer = parent.readback(&parent_target.image, None).unwrap();
    let mut child_transfer = child.readback(&child_target.image, None).unwrap();
    assert!(matches!(
        parent.readback(&parent_target.image, None),
        Err(Error::Busy)
    ));
    assert!(matches!(
        child.readback(&child_target.image, None),
        Err(Error::Busy)
    ));
    parent_transfer.take_completion_job().unwrap()();
    child_transfer.take_completion_job().unwrap()();
    assert_eq!(
        parent_transfer.poll().unwrap().unwrap(),
        [0, 0, 255, 255].repeat(2)
    );
    assert_eq!(
        child_transfer.poll().unwrap().unwrap(),
        [255, 0, 0, 255].repeat(2)
    );
    assert_eq!(
        read(&mut child, &parent_target.image),
        [0, 0, 255, 255].repeat(2)
    );
    drop(parent_target);
    drop(parent_transfer);
    drop(parent);
    child
        .clear(&child_target, Some([0., 1., 0., 1.]), None)
        .unwrap();
    assert_eq!(
        read(&mut child, &child_target.image),
        [0, 255, 0, 255].repeat(2)
    );
}

#[test]
fn explicit_cancel_of_an_unqueued_map_releases_its_budget_without_waiting_or_losing_the_device() {
    let mut gpu =
        pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).expect("test GPU adapter");
    gpu.set_readback_limit(1).unwrap();
    let target = gpu.target(2, 2, false).unwrap();
    gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
    let wake = std::sync::Arc::new(adv_sync::WakeEvent::new().unwrap());
    let mut ticket = gpu.readback(&target.image, Some(wake.clone())).unwrap();
    assert!(matches!(
        gpu.readback(&target.image, None),
        Err(Error::Busy)
    ));
    ticket.cancel();
    assert!(matches!(ticket.poll(), Some(Err(Error::Device(_)))));
    assert!(wake.wait(Some(std::time::Duration::from_secs(1))).unwrap());
    assert!(gpu.check().is_ok());
    assert_eq!(read(&mut gpu, &target.image), [0, 0, 255, 255].repeat(4));
}

#[test]
fn delayed_native_capture_keeps_source_pixels_dimensions_and_monotonic_serial() {
    use adv_device::surface::{Snapshot, Surfaces};
    let mut gpu =
        pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).expect("test GPU adapter");
    let mut surfaces = Surfaces::new(2, 2, 0x1000, 0x10000, Snapshot::default()).unwrap();
    surfaces.set_deferred_presentation(true).unwrap();
    let first = surfaces.create_native(&gpu, 2, 2, false).unwrap();
    let second = surfaces.create_native(&gpu, 3, 1, false).unwrap();
    gpu.clear(
        surfaces.native_target(first).unwrap(),
        Some([1., 0., 0., 1.]),
        None,
    )
    .unwrap();
    surfaces.present_native(first, &mut gpu).unwrap();
    let source = surfaces.latest_native_frame().unwrap().clone();
    let mut transfer = gpu.readback(&source.image, None).unwrap();
    let completion = transfer.take_completion_job().unwrap();

    // Advance and resize before delivering the original transfer completion.
    gpu.clear(
        surfaces.native_target(second).unwrap(),
        Some([0., 0., 1., 1.]),
        None,
    )
    .unwrap();
    surfaces.present_native(second, &mut gpu).unwrap();
    completion();
    let pixels = transfer.poll().unwrap().unwrap();
    assert_eq!(pixels, [0, 0, 255, 255].repeat(4));
    assert!(!surfaces.capture_native_pixels(1, 2, 2, &pixels).unwrap());
    assert!(
        surfaces
            .capture_native_snapshot_pixels(&source, &pixels)
            .unwrap()
    );
    assert_eq!(surfaces.presenter.frames, 1);
    assert_eq!(
        (surfaces.presenter.width, surfaces.presenter.height),
        (2, 2)
    );
    assert_eq!(surfaces.presenter.bgra, pixels);
    assert!(surfaces.has_pending_frame());
    assert!(
        surfaces
            .capture_native_snapshot_pixels(&source, &pixels[..12])
            .is_err()
    );

    let latest = surfaces.latest_native_frame().unwrap().clone();
    let latest_pixels = read(&mut gpu, &latest.image);
    assert!(
        surfaces
            .capture_native_pixels(2, 3, 1, &latest_pixels)
            .unwrap()
    );
    assert!(
        !surfaces
            .capture_native_snapshot_pixels(&source, &pixels)
            .unwrap()
    );
    assert_eq!(surfaces.presenter.frames, 2);
    assert_eq!(
        (surfaces.presenter.width, surfaces.presenter.height),
        (3, 1)
    );
    assert_eq!(surfaces.presenter.bgra, [255, 0, 0, 255].repeat(3));
    assert!(!surfaces.has_pending_frame());
}

#[test]
fn video_over_native_base_keeps_gpu_pixels_and_pins_layers_for_explicit_capture() {
    use adv_device::surface::{BlitRect, Snapshot, Surfaces, VideoFrame, VideoLayer};
    let mut gpu =
        pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).expect("test GPU adapter");
    let mut surfaces = Surfaces::new(3, 1, 0x1000, 0x10000, Snapshot::default()).unwrap();
    surfaces.set_deferred_presentation(true).unwrap();
    let target = surfaces.create_native(&gpu, 3, 1, false).unwrap();
    gpu.clear(
        surfaces.native_target(target).unwrap(),
        Some([1., 0., 0., 1.]),
        None,
    )
    .unwrap();
    surfaces.present_native(target, &mut gpu).unwrap();
    let layer = VideoLayer {
        frame: VideoFrame {
            width: 1,
            height: 1,
            pitch: 4,
            bgra: Arc::from([255, 0, 0, 255]),
        },
        source: Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        },
        destination: BlitRect {
            x: 1,
            y: 0,
            w: 1,
            h: 1,
        },
        background: None,
        clip: Rect {
            x: 0,
            y: 0,
            w: 3,
            h: 1,
        },
        canvas: (3, 1),
        visible: true,
        hide_cursor: true,
    };
    assert_eq!(surfaces.set_video_layer(5, Some(layer.clone())).unwrap(), 2);
    let composition = surfaces.latest_frame().unwrap().clone();
    assert!(composition.native().is_some());
    assert_eq!(gpu.stats().readbacks, 0);
    let mut transfer = gpu
        .readback(&composition.native().unwrap().image, None)
        .unwrap();
    let complete = transfer.take_completion_job().unwrap();
    let mut newer = layer;
    newer.frame.bgra = Arc::from([0, 255, 0, 255]);
    surfaces.set_video_layer(5, Some(newer)).unwrap();
    complete();
    let pixels = transfer.poll().unwrap().unwrap();
    assert!(
        surfaces
            .capture_native_frame_pixels(&composition, &pixels)
            .unwrap()
    );
    assert_eq!(surfaces.presenter.frames, 2);
    assert_eq!(
        surfaces.presenter.bgra,
        [0, 0, 255, 255, 255, 0, 0, 255, 0, 0, 255, 255]
    );
    assert_eq!(surfaces.set_video_layer(5, None).unwrap(), 4);
    let restored = surfaces.latest_native_frame().unwrap().clone();
    let pixels = read(&mut gpu, &restored.image);
    assert!(surfaces.capture_native_pixels(4, 3, 1, &pixels).unwrap());
    assert_eq!(surfaces.presenter.bgra, [0, 0, 255, 255].repeat(3));
}

fn pixel(bytes: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * width + x) * 4) as usize;
    bytes[at..at + 4].try_into().unwrap()
}

fn quad(color: [f32; 4], depth: f32, uv_scale: f32) -> [Vertex; 6] {
    let vertex = |x, y, u, v| Vertex {
        position: [x, y, depth, 1.0],
        color,
        uv: [u, v],
    };
    [
        vertex(-1.0, 1.0, 0.0, 0.0),
        vertex(-1.0, -1.0, 0.0, uv_scale),
        vertex(1.0, 1.0, uv_scale, 0.0),
        vertex(1.0, 1.0, uv_scale, 0.0),
        vertex(-1.0, -1.0, 0.0, uv_scale),
        vertex(1.0, -1.0, uv_scale, uv_scale),
    ]
}

fn near(got: [u8; 4], expected: [u8; 4], tolerance: u8) {
    assert!(
        got.iter()
            .zip(expected)
            .all(|(a, b)| a.abs_diff(b) <= tolerance),
        "BGRA {got:?} differs from {expected:?}"
    );
}

#[test]
// spec: DEV-DEPTH-1
fn independent_depth_survives_color_target_switches_in_all_supported_formats() {
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).unwrap();
    for format in [
        DepthFormat::Float32,
        DepthFormat::Unorm16,
        DepthFormat::Depth24Stencil8,
    ] {
        let depth = gpu.depth(4, 4, format).unwrap();
        let first = gpu
            .target(4, 4, false)
            .unwrap()
            .with_depth(Some(depth.clone()))
            .unwrap();
        let second = gpu
            .target(4, 4, false)
            .unwrap()
            .with_depth(Some(depth))
            .unwrap();
        let mut state = DrawState::new(4, 4);
        state.depth = Some(Compare::Less);
        state.depth_write = true;
        gpu.clear(&first, Some([1., 0., 0., 1.]), Some(1.)).unwrap();
        gpu.draw(&first, &quad([0., 1., 0., 1.], 0.25, 1.), None, state)
            .unwrap();
        gpu.clear(&second, Some([0., 0., 0., 1.]), None).unwrap();
        gpu.draw(&second, &quad([1., 0., 0., 1.], 0.5, 1.), None, state)
            .unwrap();
        assert_eq!(read(&mut gpu, &second.image), [0, 0, 0, 255].repeat(16));
        gpu.draw(&second, &quad([0., 0., 1., 1.], 0.125, 1.), None, state)
            .unwrap();
        assert_eq!(read(&mut gpu, &second.image), [255, 0, 0, 255].repeat(16));
        assert_eq!(read(&mut gpu, &first.image), [0, 255, 0, 255].repeat(16));
    }
    let depth = gpu.depth(8, 8, DepthFormat::Float32).unwrap();
    let smaller = gpu
        .target(4, 4, false)
        .unwrap()
        .with_depth(Some(depth))
        .unwrap();
    gpu.clear(&smaller, Some([1., 0., 0., 1.]), Some(1.))
        .unwrap();
    assert_eq!(read(&mut gpu, &smaller.image), [0, 0, 255, 255].repeat(16));
    assert!(matches!(
        gpu.target(4, 4, false)
            .unwrap()
            .with_depth(Some(gpu.depth(3, 4, DepthFormat::Float32).unwrap())),
        Err(Error::Invalid(_))
    ));
    let foreign = pollster::block_on(Gpu::request(!cfg!(target_os = "macos")))
        .unwrap()
        .depth(4, 4, DepthFormat::Float32)
        .unwrap();
    assert!(matches!(
        gpu.target(4, 4, false).unwrap().with_depth(Some(foreign)),
        Err(Error::Invalid(_))
    ));
    assert!(
        gpu.check().is_ok(),
        "invalid attachment pairing must not poison the device"
    );
}

#[test]
fn stencil_operations_masks_and_failure_paths_write_the_shared_attachment() {
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).unwrap();
    let target = gpu
        .target(4, 4, false)
        .unwrap()
        .with_depth(Some(gpu.depth(4, 4, DepthFormat::Depth24Stencil8).unwrap()))
        .unwrap();
    let face = |compare, fail, depth_fail, pass| StencilFace {
        compare,
        fail,
        depth_fail,
        pass,
    };
    let both = |face, read_mask, write_mask| Stencil {
        front: face,
        back: face,
        read_mask,
        write_mask,
    };
    for (initial, op, reference, mask, expected) in [
        (5, StencilOp::Keep, 37, 255, 5),
        (5, StencilOp::Zero, 37, 255, 0),
        (5, StencilOp::Replace, 37, 255, 37),
        (255, StencilOp::IncrementClamp, 37, 255, 255),
        (0, StencilOp::DecrementClamp, 37, 255, 0),
        (0x55, StencilOp::Invert, 37, 255, 0xaa),
        (255, StencilOp::IncrementWrap, 37, 255, 0),
        (0, StencilOp::DecrementWrap, 37, 255, 255),
        (0xa0, StencilOp::Replace, 0x15, 15, 0xa5),
    ] {
        gpu.clear_with_stencil(&target, Some([0., 0., 0., 1.]), Some(1.), Some(initial))
            .unwrap();
        let mut state = DrawState::new(4, 4);
        state.color_write = 0;
        state.stencil = Some(both(
            face(Compare::Always, StencilOp::Keep, StencilOp::Keep, op),
            255,
            mask,
        ));
        state.stencil_reference = reference;
        gpu.draw(&target, &quad([1.; 4], 0.5, 1.), None, state)
            .unwrap();
        state.color_write = 15;
        state.stencil = Some(both(
            face(
                Compare::Equal,
                StencilOp::Keep,
                StencilOp::Keep,
                StencilOp::Keep,
            ),
            255,
            0,
        ));
        state.stencil_reference = expected;
        gpu.draw(&target, &quad([0., 0., 1., 1.], 0.5, 1.), None, state)
            .unwrap();
        assert_eq!(
            read(&mut gpu, &target.image),
            [255, 0, 0, 255].repeat(16),
            "{op:?}"
        );
    }
    for depth_failure in [false, true] {
        gpu.clear_with_stencil(&target, Some([0., 0., 0., 1.]), Some(0.25), Some(0))
            .unwrap();
        let mut state = DrawState::new(4, 4);
        state.depth = Some(Compare::Less);
        state.stencil_reference = 0xa5;
        state.stencil = Some(both(
            face(
                if depth_failure {
                    Compare::Always
                } else {
                    Compare::Never
                },
                StencilOp::Replace,
                StencilOp::Replace,
                StencilOp::Keep,
            ),
            255,
            255,
        ));
        gpu.draw(&target, &quad([1., 0., 0., 1.], 0.75, 1.), None, state)
            .unwrap();
        state.depth = None;
        state.stencil_reference = 5;
        state.stencil = Some(both(
            face(
                Compare::Equal,
                StencilOp::Keep,
                StencilOp::Keep,
                StencilOp::Keep,
            ),
            15,
            0,
        ));
        gpu.draw(&target, &quad([0., 0., 1., 1.], 0.75, 1.), None, state)
            .unwrap();
        assert_eq!(read(&mut gpu, &target.image), [255, 0, 0, 255].repeat(16));
    }
    let ordinary = gpu.target(4, 4, true).unwrap();
    assert!(matches!(
        gpu.clear_with_stencil(&ordinary, None, None, Some(1)),
        Err(Error::Invalid(_))
    ));
    let mut state = DrawState::new(4, 4);
    state.stencil = Some(both(
        face(
            Compare::Always,
            StencilOp::Keep,
            StencilOp::Keep,
            StencilOp::Keep,
        ),
        255,
        255,
    ));
    assert!(matches!(
        gpu.draw(&ordinary, &quad([1.; 4], 0.5, 1.), None, state),
        Err(Error::Invalid(_))
    ));
    assert!(gpu.check().is_ok());
}

#[test]
fn opaque_images_sample_one_and_preserve_semantics_across_frame_rotation() {
    use adv_device::surface::{Snapshot, Surfaces, frame_mailbox};
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).unwrap();
    let opaque = gpu
        .target(2, 2, false)
        .unwrap()
        .with_alpha_mode(AlphaMode::Opaque);
    let output = gpu.target(2, 2, false).unwrap();
    let mut state = DrawState::new(2, 2);
    for (factor, expected) in [
        (Factor::DestinationAlpha, [0, 0, 255, 255]),
        (Factor::OneMinusDestinationAlpha, [0, 0, 0, 255]),
        (Factor::SourceAlphaSaturated, [0, 0, 0, 255]),
    ] {
        gpu.draw(
            &opaque,
            &quad([1., 1., 1., 0.25], 0., 1.),
            None,
            DrawState::new(2, 2),
        )
        .unwrap();
        state.blend = Some(Blend {
            source: factor,
            destination: Factor::Zero,
            operation: BlendOp::Add,
        });
        gpu.draw(&opaque, &quad([1., 0., 0., 0.5], 0., 1.), None, state)
            .unwrap();
        let mut sampled = DrawState::new(2, 2);
        sampled.texture_op = TextureOp::Texture;
        sampled.alpha_texture_op = TextureOp::Texture;
        gpu.draw(
            &output,
            &quad([1.; 4], 0., 1.),
            Some(&opaque.image),
            sampled,
        )
        .unwrap();
        assert_eq!(read(&mut gpu, &output.image), expected.repeat(4));
    }
    let frame = gpu.present(&opaque.image, 1).unwrap();
    assert_eq!(frame.image.alpha_mode(), AlphaMode::Opaque);
    drop(frame);
    let frame = gpu.present(&output.image, 2).unwrap();
    assert_eq!(frame.image.alpha_mode(), AlphaMode::Stored);
    drop(frame);
    let mut surfaces = Surfaces::new(2, 2, 0x1000, 0x1000, Snapshot::default()).unwrap();
    let id = surfaces.insert_native(opaque).unwrap();
    let (producer, mut consumer) = frame_mailbox();
    surfaces.set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
    for _ in 0..3 {
        surfaces.present_native_rotating(id, &mut gpu).unwrap();
        assert!(consumer.update());
        assert_eq!(
            consumer.current().native().unwrap().image.alpha_mode(),
            AlphaMode::Opaque
        );
        assert_eq!(
            surfaces.native_target(id).unwrap().image.alpha_mode(),
            AlphaMode::Opaque
        );
    }
}

#[test]
fn actual_gpu_color_texture_blend_clip_depth_and_native_frame_readback() {
    color_texture_blend_clip_depth_and_native_frame_readback(false);
}

fn color_texture_blend_clip_depth_and_native_frame_readback(fallback: bool) {
    let started = Instant::now();
    let mut gpu = pollster::block_on(Gpu::request(fallback)).expect("actual host GPU adapter");
    eprintln!("adapter: {:?}", gpu.context().adapter.get_info());
    let target = gpu.target(63, 31, true).unwrap();
    gpu.clear(&target, Some([0.0, 0.0, 0.0, 1.0]), Some(1.0))
        .unwrap();
    let mut state = DrawState::new(63, 31);
    gpu.draw(&target, &quad([1.0, 0.0, 0.0, 1.0], 0.5, 1.0), None, state)
        .unwrap();
    let pixels = read(&mut gpu, &target.image);
    assert_eq!(pixels.len(), 63 * 31 * 4, "alignment padding is stripped");
    assert_eq!(pixel(&pixels, 63, 31, 15), [0, 0, 255, 255]);

    let image = gpu.image(2, 2).unwrap();
    gpu.upload(
        &image,
        Rect {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
        },
        12,
        &[
            0, 0, 255, 255, 0, 255, 0, 255, 19, 19, 19, 19, 255, 0, 0, 255, 255, 255, 255, 255,
        ],
    )
    .unwrap();
    state.texture_op = TextureOp::Texture;
    state.alpha_texture_op = TextureOp::Texture;
    gpu.draw(&target, &quad([1.0; 4], 0.5, 1.0), Some(&image), state)
        .unwrap();
    let pixels = read(&mut gpu, &target.image);
    assert_eq!(pixel(&pixels, 63, 8, 4), [0, 0, 255, 255]);
    assert_eq!(pixel(&pixels, 63, 54, 4), [0, 255, 0, 255]);
    assert_eq!(pixel(&pixels, 63, 8, 26), [255, 0, 0, 255]);
    assert_eq!(pixel(&pixels, 63, 54, 26), [255, 255, 255, 255]);

    state.sampler.min = Filter::Linear;
    state.sampler.mag = Filter::Linear;
    gpu.draw(&target, &quad([1.0; 4], 0.5, 1.0), Some(&image), state)
        .unwrap();
    // D3D's 8-bit subtexel addressing permits fixed-point filter rounding;
    // Vulkan RTX returns 128/128/128, DX12 WARP returns 128/126/128 here.
    // Limit that tolerance to the filtered sample, keeping solid/nearest exact.
    // https://microsoft.github.io/DirectX-Specs/d3d/archive/D3D11_3_FunctionalSpec.htm#7.18.16.1%20Texture%20Addressing%20and%20LOD%20Precision
    near(
        pixel(&read(&mut gpu, &target.image), 63, 31, 15),
        [128, 128, 128, 255],
        2,
    );

    state = DrawState::new(63, 31);
    gpu.clear(&target, Some([0.0, 0.0, 1.0, 1.0]), Some(1.0))
        .unwrap();
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
        x: 10,
        y: 5,
        w: 40,
        h: 20,
    });
    gpu.draw(&target, &quad([1.0, 0.0, 0.0, 0.5], 0.5, 1.0), None, state)
        .unwrap();
    let pixels = read(&mut gpu, &target.image);
    near(pixel(&pixels, 63, 31, 15), [128, 0, 128, 128], 1);
    assert_eq!(pixel(&pixels, 63, 2, 2), [255, 0, 0, 255]);

    state = DrawState::new(63, 31);
    state.depth = Some(Compare::Less);
    state.depth_write = true;
    gpu.clear(&target, Some([0.0, 0.0, 0.0, 1.0]), Some(1.0))
        .unwrap();
    gpu.draw(&target, &quad([0.0, 1.0, 0.0, 1.0], 0.2, 1.0), None, state)
        .unwrap();
    gpu.draw(&target, &quad([1.0, 0.0, 0.0, 1.0], 0.8, 1.0), None, state)
        .unwrap();
    assert_eq!(
        pixel(&read(&mut gpu, &target.image), 63, 31, 15),
        [0, 255, 0, 255]
    );
    state.depth = None;
    state.alpha_test = Compare::Greater;
    state.alpha_reference = 0.7;
    gpu.draw(&target, &quad([1.0, 0.0, 0.0, 0.5], 0.1, 1.0), None, state)
        .unwrap();
    assert_eq!(
        pixel(&read(&mut gpu, &target.image), 63, 31, 15),
        [0, 255, 0, 255]
    );

    let readbacks = gpu.stats().readbacks;
    let frame = gpu.present(&target.image, 7).unwrap();
    assert_eq!(
        gpu.stats().readbacks,
        readbacks,
        "native Present never performs CPU readback"
    );
    assert_eq!(frame.serial, 7);
    gpu.clear(&target, Some([1.0, 0.0, 0.0, 1.0]), None)
        .unwrap();
    assert_eq!(
        pixel(&read(&mut gpu, &frame.image), 63, 31, 15),
        [0, 255, 0, 255],
        "retained native frame is not mutated by the next frame"
    );
    assert_eq!(gpu.stats().native_frames, 1);
    gpu.poll().unwrap();
    eprintln!("GPU slice {:?}, {:?}", gpu.stats(), started.elapsed());
}

#[test]
fn invalid_commands_are_rejected_before_gpu_submission_and_context_can_be_rebuilt() {
    let mut gpu = pollster::block_on(Gpu::request(false)).unwrap();
    assert!(gpu.target(0, 16, false).is_err());
    let target = gpu.target(16, 16, false).unwrap();
    let mut state = DrawState::new(16, 16);
    let vertices = quad([1.0; 4], 0.5, 1.0);
    let before = gpu.stats().submissions;
    assert!(gpu.draw(&target, &vertices[..2], None, state).is_err());
    assert!(
        gpu.draw(&target, &vertices, Some(&target.image), state)
            .is_err()
    );
    state.depth = Some(Compare::Less);
    assert!(gpu.draw(&target, &vertices, None, state).is_err());
    state = DrawState::new(16, 16);
    state.scissor = Some(Rect {
        x: u32::MAX,
        y: 0,
        w: 1,
        h: 1,
    });
    assert!(gpu.draw(&target, &vertices, None, state).is_err());
    assert!(
        gpu.upload(
            &target.image,
            Rect {
                x: 0,
                y: 0,
                w: 16,
                h: 16
            },
            64,
            &[0; 4]
        )
        .is_err()
    );
    assert_eq!(gpu.stats().submissions, before);
    gpu.check().unwrap();

    let mut rebuilt = pollster::block_on(Gpu::request(false)).unwrap();
    let new_target = rebuilt.target(16, 16, false).unwrap();
    assert!(
        rebuilt
            .copy(
                &target.image,
                Rect {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1
                },
                &new_target.image,
                0,
                0
            )
            .is_err()
    );
    drop(gpu);
    rebuilt
        .clear(&new_target, Some([0.0, 0.0, 1.0, 1.0]), None)
        .unwrap();
    assert_eq!(
        pixel(&read(&mut rebuilt, &new_target.image), 16, 8, 8),
        [255, 0, 0, 255]
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn actual_software_adapter_clear_and_draw() {
    color_texture_blend_clip_depth_and_native_frame_readback(true);
}

#[test]
fn native_surfaces_share_ids_mailbox_and_reusable_bounded_frame_ownership() {
    use adv_device::surface::{PixelFormat, Snapshot, SurfaceMemory, Surfaces, frame_mailbox};
    use std::borrow::Cow;
    struct Memory(Vec<u8>);
    impl SurfaceMemory for Memory {
        fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
            Ok(Cow::Borrowed(&self.0[addr as usize..addr as usize + len]))
        }
        fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
            self.0[addr as usize..addr as usize + data.len()].copy_from_slice(data);
            Ok(())
        }
        fn writable(&self, _addr: u32, _len: usize) -> Result<(), String> {
            Ok(())
        }
        fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
            self.0
                .copy_within(src as usize..src as usize + len, dst as usize);
            Ok(())
        }
    }
    let mut gpu = pollster::block_on(Gpu::request(false)).unwrap();
    let mut surfaces = Surfaces::new(16, 16, 0x1000, 0x10000, Snapshot::default()).unwrap();
    let mut memory = Memory(vec![0; 0x11000]);
    let software = surfaces
        .create(16, 16, false, PixelFormat::Bgra32, &mut memory)
        .unwrap();
    let native = surfaces.create_native(&gpu, 16, 16, false).unwrap();
    assert_ne!(native, software);
    assert!(surfaces.contains(native));
    assert_eq!(surfaces.live_surfaces(), 2);
    let image = surfaces.native_target(native).unwrap().clone();
    gpu.clear(&image, Some([0.0, 1.0, 0.0, 1.0]), None).unwrap();
    let (producer, mut consumer) = frame_mailbox();
    surfaces.set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
    assert!(
        surfaces
            .fill_raw(
                native,
                Rect {
                    x: 0,
                    y: 0,
                    w: 16,
                    h: 16
                },
                0,
                &mut memory
            )
            .is_err()
    );
    assert!(
        surfaces.present(native, &memory).is_err(),
        "never treat native pixels as guest address zero"
    );
    for serial in 1..=100_u32 {
        surfaces.present_native(native, &mut gpu).unwrap();
        if serial % 2 == 0 {
            assert!(consumer.update());
        }
    }
    assert_eq!(gpu.stats().readbacks, 0);
    assert!(
        gpu.stats().native_frame_allocations <= 4,
        "triple mailbox uses bounded reusable GPU frames"
    );
    let native_frame = consumer.current().native().unwrap().clone();
    assert_eq!(native_frame.serial, 100);
    assert!(
        consumer
            .current()
            .snapshot(&mut Snapshot::default())
            .is_err(),
        "CPU snapshot is explicit"
    );
    surfaces.present(software, &memory).unwrap();
    assert!(consumer.update());
    assert!(
        consumer.current().native().is_none(),
        "software publish clears a recycled native slot"
    );
    let mut snapshot = Snapshot::default();
    consumer.current().snapshot(&mut snapshot).unwrap();
    assert_eq!(snapshot.frames, 101);
    assert_eq!(
        pixel(&read(&mut gpu, &native_frame.image), 16, 8, 8),
        [0, 255, 0, 255]
    );
    assert!(surfaces.delete(native));
    assert!(!surfaces.contains(native));
    assert!(surfaces.native_target(native).is_none());
    assert_eq!(surfaces.live_surfaces(), 1);
    eprintln!("mailbox GPU stats: {:?}", gpu.stats());
}

#[test]
fn native_frame_saturation_reports_busy_and_release_reuses_storage() {
    let mut gpu = pollster::block_on(Gpu::request(false)).unwrap();
    let target = gpu.target(8, 8, false).unwrap();
    gpu.clear(&target, Some([1.0, 0.0, 0.0, 1.0]), None)
        .unwrap();
    let mut held = Vec::new();
    for serial in 1..=8 {
        held.push(gpu.present(&target.image, serial).unwrap());
    }
    let before = gpu.stats().submissions;
    assert!(matches!(gpu.present(&target.image, 9), Err(Error::Busy)));
    assert_eq!(gpu.stats().submissions, before);
    held.pop();
    assert_eq!(gpu.present(&target.image, 9).unwrap().serial, 9);
    assert_eq!(gpu.stats().native_frame_allocations, 8);
    assert_eq!(
        pixel(&read(&mut gpu, &held[0].image), 8, 4, 4),
        [0, 0, 255, 255]
    );
}

#[test]
fn readback_reservations_survive_ticket_cancellation_until_callback_completion() {
    let mut gpu = pollster::block_on(Gpu::request(false)).unwrap();
    gpu.set_readback_limit(1).unwrap();
    let target = gpu.target(8, 8, false).unwrap();
    gpu.clear(&target, Some([0.0, 1.0, 0.0, 1.0]), None)
        .unwrap();
    let cancelled = gpu.readback(&target.image, None).unwrap();
    drop(cancelled);
    let before = gpu.stats().submissions;
    assert!(matches!(
        gpu.readback(&target.image, None),
        Err(Error::Busy)
    ));
    assert_eq!(gpu.stats().submissions, before);
    gpu.context()
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })
        .unwrap();
    assert_eq!(
        pixel(&read(&mut gpu, &target.image), 8, 4, 4),
        [0, 255, 0, 255]
    );
}

#[test]
fn repeated_drawing_reuses_upload_buffer_and_preserves_queue_order() {
    let mut gpu = pollster::block_on(Gpu::request(false)).unwrap();
    let target = gpu.target(64, 64, false).unwrap();
    let state = DrawState::new(64, 64);
    gpu.clear(&target, Some([0.0, 0.0, 0.0, 1.0]), None)
        .unwrap();
    gpu.draw(&target, &quad([1.0; 4], 0.5, 1.0), None, state)
        .unwrap();
    gpu.context()
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })
        .unwrap();
    let start = Instant::now();
    for i in 0..256 {
        let color = if i % 2 == 0 {
            [0.0, 1.0, 0.0, 1.0]
        } else {
            [1.0, 0.0, 0.0, 1.0]
        };
        gpu.draw(&target, &quad(color, 0.5, 1.0), None, state)
            .unwrap();
    }
    let encoding = start.elapsed();
    assert_eq!(gpu.stats().vertex_buffer_allocations, 1);
    assert_eq!(gpu.stats().draws, 257);
    assert_eq!(
        pixel(&read(&mut gpu, &target.image), 64, 32, 32),
        [0, 0, 255, 255]
    );
    eprintln!(
        "256 reused-buffer draws: encode {encoding:?}, finish {:?}; {:?}",
        start.elapsed(),
        gpu.stats()
    );
}

#[test]
fn destroyed_device_is_reported_and_commands_never_return_success_after_loss() {
    let mut gpu = pollster::block_on(Gpu::request(false)).unwrap();
    let target = gpu.target(8, 8, false).unwrap();
    gpu.context().device.destroy();
    let _ = gpu.poll();
    assert!(matches!(gpu.check(), Err(Error::Device(_))));
    let before = gpu.stats().submissions;
    assert!(
        gpu.clear(&target, Some([0.0, 0.0, 0.0, 1.0]), None)
            .is_err()
    );
    assert!(gpu.present(&target.image, 1).is_err());
    assert!(gpu.readback(&target.image, None).is_err());
    assert_eq!(gpu.stats().submissions, before);
}

#[test]
fn explicit_completion_job_drives_one_readback_and_wakes_without_a_poll_loop() {
    let mut gpu = pollster::block_on(Gpu::request(false)).unwrap();
    let target = gpu.target(8, 8, false).unwrap();
    let wake = Arc::new(adv_sync::WakeEvent::new().unwrap());
    gpu.clear(&target, Some([1.0, 0.0, 0.0, 1.0]), None)
        .unwrap();
    let mut readback = gpu.readback(&target.image, Some(wake.clone())).unwrap();
    let mut builder = adv_sync::background::Builder::new((*wake).clone()).unwrap();
    let mut sender = builder.sender(1, 1).unwrap();
    let mut service = builder.start().unwrap();
    let completion = readback.take_completion_job().unwrap();
    assert!(readback.take_completion_job().is_none());
    let ticket = sender
        .try_submit(
            adv_sync::background::Priority::Urgent,
            adv_sync::background::Task::new(completion)
                .finish_on_shutdown()
                .without_wake(),
        )
        .unwrap();
    assert!(wake.wait(Some(Duration::from_secs(10))).unwrap());
    assert_eq!(
        pixel(&readback.poll().unwrap().unwrap(), 8, 4, 4),
        [0, 0, 255, 255]
    );
    assert!(readback.poll().is_none());
    service.shutdown();
    assert_eq!(ticket.status(), adv_sync::background::Status::Completed);
}

#[test]
fn concurrent_unpinned_image_clone_releases_notify_capacity_after_their_refcounts_drop() {
    let mut gpu = pollster::block_on(Gpu::request(false)).unwrap();
    let image = gpu.image(8, 8).unwrap();
    let context = gpu.context().clone();
    let wake = adv_sync::WakeEvent::new().unwrap();
    assert!(context.set_execution_wake(wake.clone()).is_ok());
    let frames: Vec<_> = (0..8)
        .map(|serial| gpu.present(&image, serial).unwrap())
        .collect();
    let mut saved: Vec<_> = frames.iter().map(|frame| frame.image.clone()).collect();
    drop(frames); // Saved Images keep each slot unavailable without FramePins.
    assert!(matches!(gpu.present(&image, 9), Err(Error::Busy)));
    let first = saved.pop().unwrap();
    let second = first.clone();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let first_barrier = barrier.clone();
    let second_barrier = barrier.clone();
    let a = std::thread::spawn(move || {
        first_barrier.wait();
        drop(first);
    });
    let b = std::thread::spawn(move || {
        second_barrier.wait();
        drop(second);
    });
    barrier.wait();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match gpu.present(&image, 9) {
            Ok(_) => break,
            Err(Error::Busy) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "last Image release lost its capacity wake"
                );
                assert!(wake.wait(Some(remaining)).unwrap());
            }
            Err(error) => panic!("unexpected retry failure: {error}"),
        }
    }
    a.join().unwrap();
    b.join().unwrap();
}

#[test]
fn ownership_frames_freeze_all_writes_without_a_copy_and_unfreeze_after_the_last_clone() {
    let mut gpu = pollster::block_on(Gpu::request(false)).unwrap();
    let target = gpu.target(8, 8, false).unwrap();
    let source = gpu.image(8, 8).unwrap();
    gpu.clear(&target, Some([0.0, 1.0, 0.0, 1.0]), None)
        .unwrap();
    let before = gpu.stats();
    let frame = gpu.completed_frame(target.image.clone(), 1).unwrap();
    assert_eq!(gpu.stats().submissions, before.submissions);
    assert_eq!(gpu.stats().native_frame_allocations, 0);
    let retained = frame.clone();
    drop(frame);
    let before = gpu.stats();
    assert!(matches!(
        gpu.upload(
            &target.image,
            Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 8
            },
            32,
            &[255; 256]
        ),
        Err(Error::Busy)
    ));
    assert!(matches!(
        gpu.clear(&target, Some([1.0, 0.0, 0.0, 1.0]), None),
        Err(Error::Busy)
    ));
    assert!(matches!(
        gpu.draw(
            &target,
            &quad([1.0; 4], 0.5, 1.0),
            None,
            DrawState::new(8, 8)
        ),
        Err(Error::Busy)
    ));
    assert!(matches!(
        gpu.copy(
            &source,
            Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 8
            },
            &target.image,
            0,
            0
        ),
        Err(Error::Busy)
    ));
    assert_eq!(gpu.stats(), before);
    assert_eq!(
        pixel(&read(&mut gpu, &retained.image), 8, 4, 4),
        [0, 255, 0, 255]
    );
    drop(retained);
    gpu.clear(&target, Some([1.0, 0.0, 0.0, 1.0]), None)
        .unwrap();
    assert_eq!(
        pixel(&read(&mut gpu, &target.image), 8, 4, 4),
        [0, 0, 255, 255]
    );
}

#[test]
fn rotating_surface_uses_one_logical_id_no_present_submits_and_a_bounded_immutable_pool() {
    use adv_device::surface::{Snapshot, Surfaces, frame_mailbox};
    let mut gpu = pollster::block_on(Gpu::request(false)).unwrap();
    let mut surfaces = Surfaces::new(8, 8, 0x1000, 0x10000, Snapshot::default()).unwrap();
    let context = gpu.context().clone();
    let wake = adv_sync::WakeEvent::new().unwrap();
    assert!(context.set_execution_wake(wake.clone()).is_ok());
    let id = surfaces.create_native(&gpu, 8, 8, false).unwrap();
    let (producer, mut consumer) = frame_mailbox();
    surfaces.set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
    let mut first = None;
    for serial in 1..=100_u32 {
        gpu.clear(
            surfaces.native_target(id).unwrap(),
            Some([f64::from(serial) / 255.0, 0.0, 0.0, 1.0]),
            None,
        )
        .unwrap();
        let before = gpu.stats().submissions;
        assert_eq!(
            surfaces.present_native_rotating(id, &mut gpu).unwrap(),
            u64::from(serial)
        );
        assert_eq!(
            gpu.stats().submissions,
            before,
            "Present ownership performs zero Queue submissions"
        );
        if serial == 1 || serial % 2 == 0 {
            assert!(consumer.update());
            if serial == 1 {
                first = Some(consumer.current().native().unwrap().clone());
            }
        }
    }
    assert_eq!(surfaces.live_surfaces(), 1);
    assert!(surfaces.native_pool_size(id).unwrap() <= 6);
    assert_eq!(gpu.stats().native_frame_allocations, 0);
    assert_eq!(gpu.stats().readbacks, 0);
    let last = consumer.current().native().unwrap().clone();
    assert_eq!(
        pixel(&read(&mut gpu, &first.as_ref().unwrap().image), 8, 4, 4),
        [0, 0, 1, 255]
    );
    assert_eq!(
        pixel(&read(&mut gpu, &last.image), 8, 4, 4),
        [0, 0, 100, 255]
    );
    drop(first);
    drop(last);
    assert!(
        !wake.wait(Some(Duration::ZERO)).unwrap(),
        "ordinary presentation release never wakes execution"
    );
    let mut held = Vec::new();
    while surfaces.native_pool_size(id).unwrap() < 8 {
        surfaces.present_native_rotating(id, &mut gpu).unwrap();
        consumer.update();
        held.push(consumer.current().native().unwrap().clone());
    }
    // Fill the last reusable slots too, retaining each completed frame.
    loop {
        let serial = surfaces.stats.presentations;
        match surfaces.present_native_rotating(id, &mut gpu) {
            Ok(_) => {
                consumer.update();
                held.push(consumer.current().native().unwrap().clone());
            }
            Err(Error::Busy) => {
                assert_eq!(surfaces.stats.presentations, serial);
                break;
            }
            Err(error) => panic!("unexpected rotation failure: {error}"),
        }
    }
    assert_eq!(surfaces.native_pool_size(id), Some(8));
    held.clear();
    assert!(
        wake.wait(Some(Duration::ZERO)).unwrap(),
        "real frame-capacity release resumes the pending producer"
    );
    assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
    surfaces.present_native_rotating(id, &mut gpu).unwrap();
    let before = surfaces.stats.presentations;
    let pause = context.pause_submissions().unwrap();
    assert!(matches!(
        surfaces.present_native_rotating(id, &mut gpu),
        Err(Error::Busy)
    ));
    assert_eq!(surfaces.stats.presentations, before);
    drop(pause);
    let replacement = gpu.target(16, 12, false).unwrap();
    surfaces.replace_native(id, replacement).unwrap();
    assert_eq!(surfaces.native_pool_size(id), Some(1));
    assert_eq!(
        (
            surfaces.get(id).unwrap().width,
            surfaces.get(id).unwrap().height
        ),
        (16, 12)
    );
    assert!(surfaces.delete(id));
    assert_eq!(surfaces.live_surfaces(), 0);
    assert_eq!(surfaces.native_pool_size(id), None);
}

#[test]
fn accepted_transfer_after_device_loss_finishes_with_a_typed_error_and_wake() {
    let mut gpu = pollster::block_on(Gpu::request(false)).unwrap();
    let target = gpu.target(8, 8, false).unwrap();
    let wake = Arc::new(adv_sync::WakeEvent::new().unwrap());
    gpu.clear(&target, Some([1.0, 0.0, 0.0, 1.0]), None)
        .unwrap();
    let mut readback = gpu.readback(&target.image, Some(wake.clone())).unwrap();
    let completion = readback.take_completion_job().unwrap();
    gpu.context().device.destroy();
    let mut builder = adv_sync::background::Builder::new((*wake).clone()).unwrap();
    let mut sender = builder.sender(1, 1).unwrap();
    let mut service = builder.start().unwrap();
    let ticket = sender
        .try_submit(
            adv_sync::background::Priority::Urgent,
            adv_sync::background::Task::new(completion)
                .finish_on_shutdown()
                .without_wake(),
        )
        .unwrap();
    service.shutdown();
    assert_eq!(ticket.status(), adv_sync::background::Status::Completed);
    assert!(wake.wait(Some(Duration::ZERO)).unwrap());
    assert!(matches!(readback.poll().unwrap(), Err(Error::Device(_))));
    assert!(readback.poll().is_none());
    assert!(gpu.check().is_err());
}

#[test]
fn configuration_pause_rejects_all_queue_operations_before_observable_mutation() {
    let mut gpu = pollster::block_on(Gpu::request(false)).unwrap();
    let target = gpu.target(8, 8, false).unwrap();
    let destination = gpu.image(8, 8).unwrap();
    gpu.clear(&target, Some([0.0, 1.0, 0.0, 1.0]), None)
        .unwrap();
    let context = gpu.context().clone();
    let wake = adv_sync::WakeEvent::new().unwrap();
    assert!(context.set_execution_wake(wake.clone()).is_ok());
    let pause = context.pause_submissions().unwrap();
    pause.wait_drained().unwrap();
    let before = gpu.stats();
    assert!(matches!(
        gpu.upload(
            &target.image,
            Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 8
            },
            32,
            &[255; 256]
        ),
        Err(Error::Busy)
    ));
    assert!(matches!(
        gpu.clear(&target, Some([1.0, 0.0, 0.0, 1.0]), None),
        Err(Error::Busy)
    ));
    assert!(matches!(
        gpu.draw(
            &target,
            &quad([1.0; 4], 0.5, 1.0),
            None,
            DrawState::new(8, 8)
        ),
        Err(Error::Busy)
    ));
    assert!(matches!(
        gpu.copy(
            &target.image,
            Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 8
            },
            &destination,
            0,
            0
        ),
        Err(Error::Busy)
    ));
    assert!(matches!(gpu.present(&target.image, 1), Err(Error::Busy)));
    assert!(matches!(
        gpu.readback(&target.image, None),
        Err(Error::Busy)
    ));
    assert_eq!(gpu.stats(), before);
    assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
    drop(pause);
    assert!(wake.wait(Some(Duration::ZERO)).unwrap());
    assert_eq!(
        pixel(&read(&mut gpu, &target.image), 8, 4, 4),
        [0, 255, 0, 255]
    );
}
