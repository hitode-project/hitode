use super::*;
use std::time::Duration;

fn read(gpu: &mut Gpu, target: &Target) -> Vec<u8> {
    let mut pending = gpu.readback(&target.image, None).unwrap();
    gpu.context()
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })
        .unwrap();
    pending.poll().unwrap().unwrap()
}
fn quad(color: [f32; 4], z: f32) -> [Vertex; 6] {
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
fn assert_pixels(bytes: &[u8], expected: impl Fn(u32, u32) -> [u8; 4]) {
    for y in 0..4 {
        for x in 0..4 {
            let at = ((y * 4 + x) * 4) as usize;
            assert_eq!(&bytes[at..at + 4], &expected(x, y), "pixel ({x},{y})");
        }
    }
}
const FULL: Rect = Rect {
    x: 0,
    y: 0,
    w: 4,
    h: 4,
};

// spec: GFX-PASS-1
#[test]
fn partial_color_clear_preserves_outside_alpha_and_full_region_keeps_the_fast_path() {
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    for alpha in [AlphaMode::Stored, AlphaMode::Opaque] {
        let target = gpu.target(4, 4, false).unwrap().with_alpha_mode(alpha);
        let before = gpu.stats();
        gpu.clear_region(&target, FULL, Some([1., 0., 0., 0.25]), None, None)
            .unwrap();
        assert_eq!(gpu.stats().draws, before.draws);
        assert_eq!(gpu.stats().submissions, before.submissions);
        let before = gpu.stats();
        gpu.clear_region(
            &target,
            Rect {
                x: 1,
                y: 1,
                w: 2,
                h: 2,
            },
            Some([0., 1., 0., 0.5]),
            None,
            None,
        )
        .unwrap();
        let after = gpu.stats();
        assert_eq!(after.draws, before.draws + 1);
        assert_eq!(after.vertices, before.vertices + 6);
        // Native first use initializes wgpu ownership before entering Metal.
        let initialization = if after.metal_draws > before.metal_draws {
            2
        } else {
            0
        };
        assert_eq!(after.submissions, before.submissions + initialization);
        assert_eq!(after.readbacks, before.readbacks);
        assert_eq!(
            after.native_frame_allocations,
            before.native_frame_allocations
        );
        assert_pixels(&read(&mut gpu, &target), |x, y| {
            let inside = (1..3).contains(&x) && (1..3).contains(&y);
            let a = if alpha == AlphaMode::Opaque {
                255
            } else if inside {
                128
            } else {
                64
            };
            if inside {
                [0, 255, 0, a]
            } else {
                [0, 0, 255, a]
            }
        });
    }
}

#[test]
fn selected_partial_depth_and_stencil_preserve_other_components_and_outside_values() {
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    let target = gpu
        .target(4, 4, false)
        .unwrap()
        .with_depth(Some(gpu.depth(4, 4, DepthFormat::Depth24Stencil8).unwrap()))
        .unwrap();
    gpu.clear_region(&target, FULL, Some([0., 0., 0., 1.]), Some(0.75), Some(3))
        .unwrap();
    let before = gpu.stats();
    gpu.clear_region(
        &target,
        Rect {
            x: 0,
            y: 0,
            w: 2,
            h: 4,
        },
        None,
        Some(0.25),
        None,
    )
    .unwrap();
    gpu.clear_region(
        &target,
        Rect {
            x: 0,
            y: 0,
            w: 4,
            h: 2,
        },
        None,
        None,
        Some(7),
    )
    .unwrap();
    let after = gpu.stats();
    assert_eq!(after.draws, before.draws + 2);
    // Depth bootstrap adds one submission to native first-use initialization.
    let initialization = if after.metal_draws > before.metal_draws {
        3
    } else {
        0
    };
    assert_eq!(after.submissions, before.submissions + initialization);
    assert_eq!(after.readbacks, before.readbacks);
    assert_eq!(
        after.native_frame_allocations,
        before.native_frame_allocations
    );
    assert_pixels(&read(&mut gpu, &target), |_, _| [0, 0, 0, 255]);
    let mut state = DrawState::new(4, 4);
    state.depth = Some(Compare::Less);
    gpu.draw(&target, &quad([0., 1., 0., 1.], 0.5), None, state)
        .unwrap();
    assert_pixels(&read(&mut gpu, &target), |x, _| {
        if x < 2 {
            [0, 0, 0, 255]
        } else {
            [0, 255, 0, 255]
        }
    });
    gpu.clear_region(&target, FULL, Some([0., 0., 0., 1.]), None, None)
        .unwrap();
    let face = StencilFace {
        compare: Compare::Equal,
        fail: StencilOp::Keep,
        depth_fail: StencilOp::Keep,
        pass: StencilOp::Keep,
    };
    let mut state = DrawState::new(4, 4);
    state.stencil = Some(Stencil {
        front: face,
        back: face,
        read_mask: 255,
        write_mask: 0,
    });
    state.stencil_reference = 7;
    gpu.draw(&target, &quad([1., 0., 0., 1.], 0.5), None, state)
        .unwrap();
    assert_pixels(&read(&mut gpu, &target), |_, y| {
        if y < 2 {
            [0, 0, 255, 255]
        } else {
            [0, 0, 0, 255]
        }
    });
    state.stencil_reference = 3;
    gpu.draw(&target, &quad([0., 0., 1., 1.], 0.5), None, state)
        .unwrap();
    assert_pixels(&read(&mut gpu, &target), |_, y| {
        if y < 2 {
            [0, 0, 255, 255]
        } else {
            [255, 0, 0, 255]
        }
    });
}

#[test]
fn empty_and_invalid_regions_validate_before_submission_without_mutating_the_image() {
    let mut gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    let target = gpu.target(4, 4, false).unwrap();
    gpu.clear_region(&target, FULL, Some([1., 0., 0., 1.]), None, None)
        .unwrap();
    let before = gpu.stats();
    let empty = Rect {
        x: 4,
        y: 4,
        w: 0,
        h: 0,
    };
    gpu.clear_region(&target, empty, Some([0., 1., 0., 1.]), None, None)
        .unwrap();
    for result in [
        gpu.clear_region(&target, empty, Some([f64::NAN, 0., 0., 1.]), None, None),
        gpu.clear_region(&target, empty, None, Some(0.5), None),
        gpu.clear_region(&target, empty, None, None, Some(1)),
        gpu.clear_region(&target, empty, None, None, None),
        gpu.clear_region(
            &target,
            Rect {
                x: u32::MAX,
                y: 0,
                w: 1,
                h: 1,
            },
            Some([0.; 4]),
            None,
            None,
        ),
        gpu.clear_region(
            &target,
            Rect {
                x: 3,
                y: 0,
                w: 2,
                h: 1,
            },
            Some([0.; 4]),
            None,
            None,
        ),
    ] {
        assert!(matches!(result, Err(Error::Invalid(_))));
    }
    let foreign_gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    let foreign = foreign_gpu.target(4, 4, false).unwrap();
    assert!(matches!(
        gpu.clear_region(&foreign, empty, Some([0.; 4]), None, None),
        Err(Error::Invalid(_))
    ));
    assert_eq!(gpu.stats(), before);
    assert_pixels(&read(&mut gpu, &target), |_, _| [0, 0, 255, 255]);
    assert!(gpu.check().is_ok());
}
