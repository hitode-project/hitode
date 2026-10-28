use super::*;

fn gpu(native: bool) -> Gpu {
    let gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    #[cfg(target_vendor = "apple")]
    {
        gpu.context.pending.lock().unwrap().metal =
            native.then(|| metal::State::new(&gpu.context).unwrap());
    }
    let _ = native;
    gpu
}
fn backends() -> impl Iterator<Item = bool> {
    [false, true]
        .into_iter()
        .take(if cfg!(target_vendor = "apple") { 2 } else { 1 })
}
fn draw() -> ProgrammableDraw {
    let mut draw = ProgrammableDraw {
        instance_count: 1,
        program: Program::new("struct C { f: array<vec4<f32>,256> }; @group(0) @binding(1) var<uniform> c:C; @vertex fn vertex_main(@location(0) p:vec4<f32>)->@builtin(position) vec4<f32> { return p; } @fragment fn fragment_main()->@location(0) vec4<f32> { return c.f[0]; }".into(), vec![]),
        streams: vec![VertexStream { instance: false, stride: 16, bytes: bytemuck::cast_slice(&[[-1f32,-1.,0.5,1.], [3.,-1.,0.5,1.], [-1.,3.,0.5,1.]]).to_vec().into(), attributes: vec![Attribute { location: 0, offset: 0, format: AttributeFormat::Float4 }] }],
        indices: None, vertex_count: 3, vertex_constants: ConstantBlock::default().into(), pixel_constants: ConstantBlock::default().into(), textures: vec![], parameters: [[0;4];40],
    };
    draw.pixel_constants.floats[0] = [0., 1., 0., 1.];
    draw
}
fn read(gpu: &mut Gpu, image: &Image) -> Vec<u8> {
    let mut ticket = gpu.readback(image, None).unwrap();
    ticket.take_completion_job().unwrap()();
    ticket.poll().unwrap().unwrap()
}
fn solid(bytes: &[u8], bgra: [u8; 4]) {
    assert!(bytes.as_chunks::<4>().0.iter().all(|&pixel| pixel == bgra));
}
const FULL: Rect = Rect {
    x: 0,
    y: 0,
    w: 4,
    h: 4,
};

// spec: GFX-PASS-1, GFX-PASS-2
#[test]
fn clear_fuses_with_draw_and_unused_depth_stays_detached() {
    for native in backends() {
        let mut gpu = gpu(native);
        let target = gpu.target(4, 4, true).unwrap();
        let draw = draw();
        let mut state = DrawState::new(4, 4);
        // Warm native texture tracking before measuring the draw pass itself.
        gpu.draw_programmable(&target, &draw, &state).unwrap();
        read(&mut gpu, &target.image);
        gpu.set_clip(Some(FULL));
        let before = gpu.stats();
        gpu.clear_region(&target, FULL, Some([1., 0., 0., 1.]), None, None)
            .unwrap();
        assert_eq!(gpu.stats().submissions, before.submissions);
        state.scissor = Some(Rect {
            x: 1,
            y: 1,
            w: 2,
            h: 2,
        });
        gpu.draw_programmable(&target, &draw, &state).unwrap();
        let pixels = read(&mut gpu, &target.image);
        for y in 0..4 {
            for x in 0..4 {
                let at = (y * 4 + x) * 4;
                assert_eq!(
                    &pixels[at..at + 4],
                    if (1..3).contains(&x) && (1..3).contains(&y) {
                        &[0, 255, 0, 255]
                    } else {
                        &[0, 0, 255, 255]
                    }
                );
            }
        }
        let after = gpu.stats();
        assert_eq!(after.passes - before.passes, 1);
        assert_eq!(after.color_loads - before.color_loads, 0);
        assert_eq!(after.depth_loads - before.depth_loads, 0);
        assert_eq!(after.merged_clears - before.merged_clears, 1);
        println!(
            "backend={:?} native={native} passes=1 color_loads=0 depth_loads=0 merged_clears=1",
            gpu.context.adapter.get_info().backend
        );
    }
}

// spec: GFX-PASS-1
#[test]
fn clear_resolves_at_read_copy_present_upload_and_external_submission() {
    for native in backends() {
        let mut gpu = gpu(native);
        let target = gpu.target(4, 4, false).unwrap();
        let other = gpu.target(4, 4, false).unwrap();
        gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
        solid(&read(&mut gpu, &target.image), [0, 0, 255, 255]);
        gpu.clear(&target, Some([0., 1., 0., 1.]), None).unwrap();
        gpu.copy(&target.image, FULL, &other.image, 0, 0).unwrap();
        solid(&read(&mut gpu, &other.image), [0, 255, 0, 255]);
        gpu.clear(&target, Some([0., 0., 1., 1.]), None).unwrap();
        let frame = gpu.present(&target.image, 1).unwrap();
        solid(&read(&mut gpu, &frame.image), [255, 0, 0, 255]);
        drop(frame);
        gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
        let frame = gpu.completed_frame(target.image.clone(), 2).unwrap();
        solid(&read(&mut gpu, &frame.image), [0, 0, 255, 255]);
        drop(frame);
        gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
        gpu.upload(
            &target.image,
            Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            4,
            &[255, 0, 0, 255],
        )
        .unwrap();
        let pixels = read(&mut gpu, &target.image);
        assert_eq!(&pixels[..4], &[255, 0, 0, 255]);
        solid(&pixels[4..], [0, 0, 255, 255]);
        gpu.clear(&target, Some([0., 1., 0., 1.]), None).unwrap();
        let context = gpu.context().clone();
        let mut encoder = context.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_texture(
            target.image.texture.as_image_copy(),
            other.image.texture.as_image_copy(),
            extent(4, 4),
        );
        context.submission().unwrap().submit([encoder.finish()]);
        assert!(context.pending.lock().unwrap().clear.is_none());
        solid(&read(&mut gpu, &other.image), [0, 255, 0, 255]);
    }
}

// spec: GFX-PASS-1, GFX-PASS-2
#[test]
fn target_switch_and_depth_upgrade_preserve_clear_and_depth_contents() {
    for native in backends() {
        let mut gpu = gpu(native);
        let target = gpu.target(4, 4, true).unwrap();
        let other = gpu.target(4, 4, false).unwrap();
        let draw = draw();
        let mut state = DrawState::new(4, 4);
        gpu.clear(&target, Some([1., 0., 0., 1.]), Some(0.25))
            .unwrap();
        gpu.draw_programmable(&other, &draw, &state).unwrap();
        solid(&read(&mut gpu, &target.image), [0, 0, 255, 255]);
        // First draw has no depth; the second must load the retained 0.25.
        gpu.draw_programmable(&target, &draw, &state).unwrap();
        state.depth = Some(Compare::Less);
        state.depth_write = true;
        let mut red = draw.clone();
        red.pixel_constants.floats[0] = [1., 0., 0., 1.];
        let before_upgrade = gpu.stats();
        gpu.draw_programmable(&target, &red, &state).unwrap();
        assert_eq!(gpu.stats().submissions, before_upgrade.submissions);
        assert_eq!(gpu.stats().passes, before_upgrade.passes + 1);
        solid(&read(&mut gpu, &target.image), [0, 255, 0, 255]);
        gpu.clear(&target, None, Some(0.75)).unwrap();
        let before = gpu.stats();
        gpu.draw_programmable(&target, &red, &state).unwrap();
        solid(&read(&mut gpu, &target.image), [0, 0, 255, 255]);
        assert_eq!(gpu.stats().merged_clears - before.merged_clears, 1);
        assert_eq!(gpu.stats().depth_loads - before.depth_loads, 0);
    }
}

// spec: GFX-PASS-1
#[test]
fn consecutive_component_clears_and_dropped_targets_keep_order() {
    for native in backends() {
        let mut gpu = gpu(native);
        let target = gpu.target(4, 4, true).unwrap();
        let image = target.image.clone();
        gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
        gpu.clear(&target, None, Some(0.75)).unwrap();
        drop(target);
        solid(&read(&mut gpu, &image), [0, 0, 255, 255]);
    }
}

// spec: GFX-PASS-2
#[test]
fn fragment_depth_outputs_keep_attachments_without_enabling_depth_writes() {
    for native in backends() {
        let mut gpu = gpu(native);
        let target = gpu.target(4, 4, true).unwrap();
        gpu.clear(&target, Some([1., 0., 0., 1.]), Some(0.25))
            .unwrap();
        read(&mut gpu, &target.image);
        let mut depth = draw();
        depth.program = Program::new(depth.program.source.replace(
            "@fragment fn fragment_main()->@location(0) vec4<f32> { return c.f[0]; }",
            "struct O { @location(0) color:vec4<f32>, @builtin(frag_depth) depth:f32 }; @fragment fn fragment_main()->O { return O(c.f[0], 0.9); }"
        ).into(), vec![]);
        let mut state = DrawState::new(4, 4);
        gpu.draw_programmable(&target, &depth, &state).unwrap();
        solid(&read(&mut gpu, &target.image), [0, 255, 0, 255]);
        let mut red = draw();
        red.pixel_constants.floats[0] = [1., 0., 0., 1.];
        state.depth = Some(Compare::Less);
        state.depth_write = true;
        gpu.draw_programmable(&target, &red, &state).unwrap();
        solid(&read(&mut gpu, &target.image), [0, 255, 0, 255]);
    }
}

// spec: GFX-PASS-2
#[test]
fn unused_depth_rebinding_keeps_a_color_only_pass() {
    for native in backends() {
        let mut gpu = gpu(native);
        let a = gpu.target(4, 4, true).unwrap();
        let b = a
            .clone()
            .with_depth(Some(gpu.depth(4, 4, DepthFormat::Float32).unwrap()))
            .unwrap();
        for target in [&a, &b] {
            gpu.clear(target, Some([0.; 4]), Some(1.)).unwrap();
            read(&mut gpu, &target.image);
        }
        let draw = draw();
        let mut state = DrawState::new(4, 4);
        gpu.draw_programmable(&a, &draw, &state).unwrap();
        read(&mut gpu, &a.image);
        // Initialize both depth images, then defer one more clear of a's.
        state.depth = Some(Compare::Always);
        gpu.draw_programmable(&a, &draw, &state).unwrap();
        gpu.draw_programmable(&b, &draw, &state).unwrap();
        state.depth = None;
        gpu.clear(&a, None, Some(1.)).unwrap();
        read(&mut gpu, &a.image);
        let before = gpu.stats();
        state.depth = Some(Compare::Always);
        gpu.draw_programmable(&a, &draw, &state).unwrap();
        state.depth = None;
        gpu.draw_programmable(&b, &draw, &state).unwrap();
        gpu.draw_programmable(&a, &draw, &state).unwrap();
        solid(&read(&mut gpu, &a.image), [0, 255, 0, 255]);
        assert_eq!(gpu.stats().passes - before.passes, 2);
        // The deferred depth clear becomes that pass's load action.
        assert_eq!(gpu.stats().depth_loads - before.depth_loads, 0);
    }
}

// spec: GFX-PASS-3
#[test]
fn depth_clears_wait_for_a_pass_that_attaches_depth() {
    for native in backends() {
        let mut gpu = gpu(native);
        let target = gpu.target(4, 4, true).unwrap();
        let green = draw();
        let mut red = draw();
        red.pixel_constants.floats[0] = [1., 0., 0., 1.];
        let mut state = DrawState::new(4, 4);
        // Attach depth once at 1.0, so a skipped clear would let red pass.
        gpu.clear(&target, None, Some(1.)).unwrap();
        state.depth = Some(Compare::Always);
        gpu.draw_programmable(&target, &green, &state).unwrap();
        read(&mut gpu, &target.image);
        let before = gpu.stats();
        gpu.clear(&target, Some([0., 0., 1., 1.]), Some(0.25))
            .unwrap();
        state.depth = None;
        gpu.draw_programmable(&target, &green, &state).unwrap();
        solid(&read(&mut gpu, &target.image), [0, 255, 0, 255]);
        let after = gpu.stats();
        assert_eq!(after.passes - before.passes, 1);
        assert_eq!(after.merged_clears - before.merged_clears, 1);
        assert_eq!(after.depth_loads - before.depth_loads, 0);
        state.depth = Some(Compare::Less);
        gpu.draw_programmable(&target, &red, &state).unwrap();
        solid(&read(&mut gpu, &target.image), [0, 255, 0, 255]);
        assert_eq!(gpu.stats().depth_loads - after.depth_loads, 0);
    }
}

// spec: GFX-PASS-3
#[test]
fn clipped_repeated_depth_clears_stay_deferred_until_depth_is_used() {
    for native in backends() {
        let mut gpu = gpu(native);
        let target = gpu.target(4, 4, true).unwrap();
        let green = draw();
        let mut red = draw();
        red.pixel_constants.floats[0] = [1., 0., 0., 1.];
        gpu.clear(&target, Some([0., 0., 0., 1.]), Some(1.))
            .unwrap();
        let mut state = DrawState::new(4, 4);
        state.depth = Some(Compare::Never);
        gpu.draw_programmable(&target, &green, &state).unwrap();
        read(&mut gpu, &target.image);
        let before = gpu.stats();
        state.depth = None;
        for width in [2, 1, 2] {
            gpu.set_clip(Some(Rect { w: width, ..FULL }));
            gpu.clear_region(&target, FULL, Some([1., 0., 0., 1.]), Some(0.25), None)
                .unwrap();
            gpu.draw_programmable(&target, &green, &state).unwrap();
            gpu.clear_region(&target, FULL, None, Some(0.25), None)
                .unwrap();
            read(&mut gpu, &target.image);
        }
        let after = gpu.stats();
        assert_eq!(after.passes - before.passes, 3);
        assert_eq!(after.color_loads - before.color_loads, 3);
        assert_eq!(after.depth_loads - before.depth_loads, 0);
        gpu.set_clip(None);
        state.depth = Some(Compare::Less);
        gpu.draw_programmable(&target, &red, &state).unwrap();
        let pixels = read(&mut gpu, &target.image);
        for (at, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
            assert_eq!(
                *pixel,
                if at % 4 < 2 {
                    [0, 255, 0, 255]
                } else {
                    [0, 0, 255, 255]
                },
                "pixel {at}"
            );
        }
    }
}

// spec: GFX-PASS-3
#[test]
fn a_partial_depth_clear_follows_the_deferred_whole_clear() {
    for native in backends() {
        let mut gpu = gpu(native);
        let target = gpu.target(4, 4, true).unwrap();
        let mut red = draw();
        red.pixel_constants.floats[0] = [1., 0., 0., 1.];
        gpu.clear(&target, Some([0., 1., 0., 1.]), Some(0.25))
            .unwrap();
        gpu.set_clip(Some(Rect {
            x: 0,
            y: 0,
            w: 2,
            h: 4,
        }));
        gpu.clear_region(&target, FULL, None, Some(0.75), None)
            .unwrap();
        gpu.set_clip(None);
        let mut state = DrawState::new(4, 4);
        state.depth = Some(Compare::Less);
        gpu.draw_programmable(&target, &red, &state).unwrap();
        let pixels = read(&mut gpu, &target.image);
        for (at, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
            let expected = if at % 4 < 2 {
                [0, 0, 255, 255]
            } else {
                [0, 255, 0, 255]
            };
            assert_eq!(*pixel, expected, "pixel {at}");
        }
    }
}

// spec: GFX-SAMPLER-4
#[test]
fn staged_uploads_land_after_pending_clears_and_recycle_their_buffer() {
    for native in backends() {
        let mut gpu = gpu(native);
        let target = gpu.target(3, 2, false).unwrap();
        gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
        // Three texels per row stage at a 256-byte aligned pitch.
        let rows: Vec<Vec<u8>> = (0..2u8)
            .map(|y| (0..12).map(|i| y * 16 + i).collect())
            .collect();
        let staged = gpu
            .stage(2, 2, |write| {
                rows.iter()
                    .enumerate()
                    .for_each(|(y, row)| write(y as u32, &row[4..]));
                Ok::<(), ()>(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(staged.pitch(), 256);
        let rectangle = Rect {
            x: 1,
            y: 0,
            w: 2,
            h: 2,
        };
        gpu.upload_staged(&target.image, rectangle, &staged)
            .unwrap();
        let red = [0, 0, 255, 255];
        let expected: Vec<u8> = rows
            .iter()
            .flat_map(|row| red.iter().chain(&row[4..]).copied().collect::<Vec<_>>())
            .collect();
        assert_eq!(read(&mut gpu, &target.image), expected, "native {native}");
        drop(staged);
        gpu.context
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .unwrap();
        assert_eq!(gpu.staging.lock().unwrap().len(), 1);
        let again = gpu.stage(2, 2, |_| Ok::<(), ()>(())).unwrap().unwrap();
        assert!(
            gpu.staging.lock().unwrap().is_empty(),
            "the pooled buffer is reused"
        );
        drop(again);
    }
}

// spec: DEV-UPDATE-4, GFX-PASS-1
#[test]
fn compact_alpha_upload_keeps_padding_and_pixels_outside_the_rectangle() {
    for native in backends() {
        let mut gpu = gpu(native);
        let image = gpu.image(4, 4).unwrap();
        let target = Target {
            image: image.clone(),
            depth: None,
        };
        gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
        gpu.upload_alpha(
            &image,
            Rect {
                x: 1,
                y: 1,
                w: 2,
                h: 2,
            },
            3,
            &[64, 128, 99, 255, 17],
        )
        .unwrap();
        let pixels = read(&mut gpu, &image);
        for (n, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
            let expected = match n {
                5 => [0, 0, 0, 64],
                6 => [0, 0, 0, 128],
                9 => [0, 0, 0, 255],
                10 => [0, 0, 0, 17],
                _ => [0, 0, 255, 255],
            };
            assert_eq!(*pixel, expected);
        }
    }
}
