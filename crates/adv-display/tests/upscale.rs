use adv_display::Rect;
use adv_display::plan::Upscaler;
use adv_display::stages::upscale::{Check, Input, Model, Upscaling, model, models};
use adv_gpu::encode::{BufferCopy, ImageCopy};
use adv_gpu::queue::DeviceOptions;
use adv_gpu::resource::{BufferDesc, BufferUsage, Dimension, ImageDesc, Memory, Usage};
use adv_gpu::{Device, Extent, Format, Image, Queue, Ring};
use std::time::Duration;

const CHOICES: [Upscaler; 4] = [
    Upscaler::Anime4k,
    Upscaler::Cunny,
    Upscaler::Artcnn,
    Upscaler::K7,
];

struct Gpu {
    device: Device,
    queue: Queue,
    ring: Ring,
}

fn gpu() -> Gpu {
    let (device, queue) = Device::open(&DeviceOptions {
        software: !cfg!(any(target_vendor = "apple", target_os = "android")),
        validation: !cfg!(target_os = "android"),
        disable: adv_gpu::Modern::from_test_env(),
        ..Default::default()
    })
    .unwrap();
    let ring = device.ring(1 << 20).unwrap();
    Gpu {
        device,
        queue,
        ring,
    }
}

/// `k / 256` as IEEE half-precision bits; exact for k up to 256.
fn half(k: u32) -> [u8; 2] {
    if k == 0 {
        return [0, 0];
    }
    let exponent = 31 - k.leading_zeros();
    let mantissa = ((k << (10 - exponent)) & 0x3ff) as u16;
    let bits = ((exponent + 15 - 8) as u16) << 10 | mantissa;
    bits.to_le_bytes()
}

/// One upscaler with its own doubled output.
struct Scaler {
    upscaling: Upscaling,
    output: Image,
}

impl Gpu {
    /// Whether `choice` runs in half precision here: the device has it and
    /// the display's check finds the half forms agree.
    fn precision(&mut self, choice: Upscaler) -> bool {
        if !self
            .device
            .caps()
            .features
            .contains(adv_gpu::Features::SHADER_F16)
        {
            return false;
        }
        let Some(check) = Check::new(&self.device, model(&self.device, choice, true)).unwrap()
        else {
            return true;
        };
        let mut encoder = self.queue.encoder("upscale check");
        check.encode(&mut encoder, &mut self.ring).unwrap();
        let token = self.queue.submit(encoder).unwrap();
        assert!(self.queue.wait(token, Duration::from_secs(60)).unwrap());
        unsafe { check.agrees() }
    }
    fn scaler(&self, model: Model, size: (u32, u32), half: bool) -> Scaler {
        let output = self
            .device
            .create_image(&ImageDesc {
                dimension: Dimension::D2,
                size: Extent {
                    width: size.0 * 2,
                    height: size.1 * 2,
                    depth_or_layers: 1,
                },
                format: Format::Rgba8Unorm,
                mips: 1,
                samples: 1,
                usage: Usage::STORAGE | Usage::SAMPLED | Usage::COPY_SRC,
                view_formats: &[],
                label: "upscale output",
            })
            .unwrap();
        Scaler {
            upscaling: Upscaling::with_precision(&self.device, model, size, half).unwrap(),
            output,
        }
    }

    fn buffer(&self, size: u64, usage: BufferUsage, memory: Memory) -> adv_gpu::Buffer {
        self.device
            .create_buffer(&BufferDesc {
                size,
                usage,
                memory,
                label: "upscale test",
            })
            .unwrap()
    }

    /// Writes `pixels` (in 1/256 steps) into the input and runs the model.
    fn run(
        &mut self,
        scaler: &Scaler,
        pixels: &[[u32; 3]],
        damage: Option<&[Rect]>,
        exact: bool,
        records: &[adv_display::stages::upscale::DrawRecord],
    ) -> u64 {
        let bytes: Vec<u8> = pixels
            .iter()
            .flat_map(|p| [half(p[0]), half(p[1]), half(p[2]), [0x00, 0x3c]])
            .flatten()
            .collect();
        let upload = self.buffer(bytes.len() as u64, BufferUsage::COPY_SRC, Memory::Upload);
        let mapped = unsafe { upload.mapped().unwrap().as_mut() };
        mapped[..bytes.len()].copy_from_slice(&bytes);
        let input = scaler.upscaling.input(exact);
        let mut encoder = self.queue.encoder("upscale test");
        let size = input.desc().size;
        let whole = [Rect {
            x: 0,
            y: 0,
            w: size.width,
            h: size.height,
        }];
        for r in damage.unwrap_or(&whole) {
            encoder.copy_buffer_to_image(
                BufferCopy {
                    buffer: &upload,
                    offset: u64::from((r.y * size.width + r.x) * 8),
                    row_pitch: size.width * 8,
                    rows: size.height,
                },
                ImageCopy {
                    image: input,
                    mip: 0,
                    layer: 0,
                    origin: (r.x, r.y, 0),
                },
                Extent {
                    width: r.w,
                    height: r.h,
                    depth_or_layers: 1,
                },
            );
        }
        let computed = scaler
            .upscaling
            .encode(
                (&mut encoder, &mut self.ring),
                scaler.output.view(),
                damage,
                if exact { Input::Exact } else { Input::Composed },
                records,
                None,
            )
            .unwrap();
        let token = self.queue.submit(encoder).unwrap();
        assert!(self.queue.wait(token, Duration::from_secs(60)).unwrap());
        computed
    }

    /// The output's rows of RGBA8 texels.
    fn read(&mut self, image: &Image) -> Vec<Vec<u8>> {
        let size = image.desc().size;
        let bytes = u64::from(size.width * size.height * 4);
        let readback = self.buffer(bytes, BufferUsage::COPY_DST, Memory::Readback);
        let mut encoder = self.queue.encoder("upscale readback");
        encoder.copy_image_to_buffer(
            ImageCopy {
                image,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            BufferCopy {
                buffer: &readback,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            size,
        );
        let token = self.queue.submit(encoder).unwrap();
        assert!(self.queue.wait(token, Duration::from_secs(60)).unwrap());
        let mapped = unsafe { readback.mapped().unwrap().as_ref() };
        mapped[..bytes as usize]
            .chunks_exact(size.width as usize * 4)
            .map(<[u8]>::to_vec)
            .collect()
    }
}

fn index(choice: Upscaler) -> usize {
    CHOICES.iter().position(|&c| c == choice).unwrap()
}

// spec: HOST-VIEW-7, HOST-VIEW-10
#[test]
fn optimized_layers_match_reference_display_pixels_and_clamped_edges() {
    let mut gpu = gpu();
    for choice in CHOICES {
        let mut maximum = [0u8; 2];
        let half = gpu.precision(choice);
        let selected = model(&gpu.device, choice, half);
        for (size, patterns) in [((1, 1), 3), ((35, 29), 3), ((130, 97), 3), ((513, 257), 1)] {
            let scalers = [
                gpu.scaler(models::REFERENCE[index(choice)], size, half),
                gpu.scaler(models::FUSED[index(choice)], size, half),
                gpu.scaler(selected, size, half),
            ];
            let mut seed = 0x2545_f491u32;
            for pattern in 0..patterns {
                let pixels: Vec<_> = (0..size.0 * size.1)
                    .map(|i| match pattern {
                        0 => [0; 3].map(|_| {
                            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                            (seed >> 16) % 257
                        }),
                        1 => {
                            if (i % size.0 + i / size.0) % 2 == 0 {
                                [0, 256, 0]
                            } else {
                                [256, 0, 256]
                            }
                        }
                        _ => [127, 128, 129],
                    })
                    .collect();
                let results: Vec<_> = scalers
                    .iter()
                    .map(|scaler| {
                        gpu.run(scaler, &pixels, None, true, &[]);
                        gpu.read(&scaler.output).concat()
                    })
                    .collect();
                for (maximum, result) in maximum.iter_mut().zip(&results[1..]) {
                    let delta = results[0]
                        .iter()
                        .zip(result)
                        .map(|(a, b)| a.abs_diff(*b))
                        .max()
                        .unwrap();
                    *maximum = (*maximum).max(delta);
                    assert!(
                        delta <= 2,
                        "{choice:?} {size:?} pattern={pattern} channel_error={delta}"
                    );
                }
                assert!(
                    results
                        .iter()
                        .all(|r| r.as_chunks::<4>().0.iter().all(|p| p[3] == 255))
                );
            }
            assert!(
                scalers
                    .iter()
                    .all(|s| s.upscaling.model().reach() == scalers[0].upscaling.model().reach())
            );
        }
        eprintln!(
            "{choice:?}: passes={} max_channel_error fused={} selected={}",
            selected.passes.len(),
            maximum[0],
            maximum[1]
        );
    }
}

// spec: HOST-VIEW-10
#[test]
fn partial_runs_match_full_runs_within_the_model_radius() {
    let mut gpu = gpu();
    let size = (160, 128);
    let mut seed = 0x2545_f491u32;
    let pixels: Vec<[u32; 3]> = (0..size.0 * size.1)
        .map(|_| {
            [0; 3].map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (seed >> 16) % 256
            })
        })
        .collect();
    let rect = |x, y, w, h| Rect { x, y, w, h };
    // Single changes, edges, and disjoint changes far apart in one run.
    let damages = [
        vec![rect(77, 61, 3, 2)],
        vec![rect(0, 0, 2, 5)],
        vec![rect(150, 120, 10, 8)],
        vec![rect(40, 100, 31, 1)],
        vec![rect(3, 120, 4, 4), rect(140, 6, 9, 3), rect(80, 64, 1, 1)],
    ];
    for choice in CHOICES {
        let half = gpu.precision(choice);
        let selected = model(&gpu.device, choice, half);
        let full = gpu.scaler(selected, size, half);
        let partial = gpu.scaler(selected, size, half);
        for scaler in [&full, &partial] {
            gpu.run(scaler, &pixels, None, true, &[]);
        }
        let reach = selected.reach();
        let mut frame = pixels.clone();
        for (step, damage) in damages.iter().enumerate() {
            let before = gpu.read(&full.output);
            for r in damage {
                for y in r.y..r.y + r.h {
                    for x in r.x..r.x + r.w {
                        frame[(y * size.0 + x) as usize] = [255, step as u32 * 50, 128];
                    }
                }
            }
            gpu.run(&full, &frame, None, true, &[]);
            let computed = gpu.run(&partial, &frame, Some(damage), true, &[]);
            assert!(
                computed < u64::from(size.0 * size.1) / 2,
                "{choice:?} partial run {step} computed {computed} pixels"
            );
            let after = gpu.read(&full.output);
            assert!(
                gpu.read(&partial.output) == after,
                "{choice:?} partial run {step} differs"
            );
            // How far, in input pixels, the change reached in the output.
            let mut reached = 0;
            for (y, (old, new)) in before.iter().zip(&after).enumerate() {
                for x in 0..old.len() / 4 {
                    if old[x * 4..x * 4 + 4] != new[x * 4..x * 4 + 4] {
                        let (x, y) = (x as u32 / 2, y as u32 / 2);
                        let distance = damage
                            .iter()
                            .map(|r| {
                                let dx = r.x.saturating_sub(x).max(x.saturating_sub(r.x + r.w - 1));
                                let dy = r.y.saturating_sub(y).max(y.saturating_sub(r.y + r.h - 1));
                                dx.max(dy)
                            })
                            .min()
                            .unwrap();
                        reached = reached.max(distance);
                    }
                }
            }
            assert!(reached <= reach, "{choice:?} reaches {reached}");
        }
        // Nothing changed: nothing is computed.
        let mut encoder = gpu.queue.encoder("upscale test");
        let computed = partial
            .upscaling
            .encode(
                (&mut encoder, &mut gpu.ring),
                partial.output.view(),
                Some(&[]),
                Input::Exact,
                &[],
                None,
            )
            .unwrap();
        assert_eq!(computed, 0);
    }
}

// Names the variant a driver computes wrongly: each model's reference and
// fused tables in each precision the display runs, run twice, against the
// reference in the same precision and against single precision. Half
// precision the check accepts stays close to single on a larger canvas.
// spec: HOST-VIEW-7
#[test]
fn variants_agree_in_each_precision_and_repeat_exactly() {
    let mut gpu = gpu();
    let size = (35, 29);
    let mut seed = 0x2545_f491u32;
    let pixels: Vec<[u32; 3]> = (0..size.0 * size.1)
        .map(|_| {
            [0; 3].map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (seed >> 16) % 257
            })
        })
        .collect();
    let delta = |a: &[u8], b: &[u8]| a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
    let mut report = vec![];
    let mut wrong = false;
    for choice in CHOICES {
        let mut single = None;
        let half = gpu.precision(choice);
        for precision in [false, true].into_iter().filter(|&h| h <= half) {
            let mut reference = None;
            for (table, models) in [("reference", models::REFERENCE), ("fused", models::FUSED)] {
                let scaler = gpu.scaler(models[index(choice)], size, precision);
                let runs: Vec<_> = (0..2)
                    .map(|_| {
                        gpu.run(&scaler, &pixels, None, true, &[]);
                        gpu.read(&scaler.output).concat()
                    })
                    .collect();
                let base = reference.get_or_insert_with(|| runs[0].clone());
                let single = single.get_or_insert_with(|| runs[0].clone());
                let (error, from_single) = (delta(base, &runs[0]), delta(single, &runs[0]));
                let repeats = runs[0] == runs[1];
                wrong |= error > 2 || !repeats || from_single > 8;
                report.push(format!(
                    "{choice:?} {table} {}: error={error} vs_f32={from_single} repeats={repeats}",
                    if precision { "f16" } else { "f32" }
                ));
            }
        }
    }
    eprintln!("{}", report.join("\n"));
    assert!(!wrong, "{}", report.join("; "));
}

// spec: HOST-VIEW-10
#[test]
fn conservative_damage_matches_whole_frames_in_all_forms_and_buffer_ages() {
    use adv_display::{plan::*, stages::output};
    use adv_gpu::encode::{ColorAttachment, RenderPassDesc, Store};
    let mut gpu = gpu();
    let size = (129, 97);
    let doubled = (size.0 * 2, size.1 * 2);
    let rect = |x, y, w, h| Rect { x, y, w, h };
    let caps = CompositorCaps {
        fractional_crops: true,
        scales: true,
        nearest: true,
        rotates: true,
        yuv_layers: true,
        solid_color_layers: true,
        damage_hints: true,
        presentable_compressed: false,
        tile: gpu.device.caps().limits.tile,
    };
    let presentation_pipeline = output::pipeline(&gpu.device, Format::Rgba8Unorm, false).unwrap();
    let matrix = adv_gpu::Features::MATRIX_F16_8X8
        | adv_gpu::Features::SHADER_F16
        | adv_gpu::Features::SUBGROUP;
    for (m, choice) in CHOICES.into_iter().enumerate() {
        for half in [false, true] {
            if half
                && !gpu
                    .device
                    .caps()
                    .features
                    .contains(adv_gpu::Features::SHADER_F16)
            {
                continue;
            }
            for (form, table) in [("fused", models::FUSED), ("matrix", models::MATRIX)] {
                if form == "matrix" && (!half || !gpu.device.caps().features.contains(matrix)) {
                    continue;
                }
                let full = gpu.scaler(table[m], size, half);
                let partial = gpu.scaler(table[m], size, half);
                let settings = Settings {
                    view: doubled,
                    upscaler: choice,
                    placement: Placement::Fit,
                    scale: 1.,
                    rotation: Rotation::None,
                    background: [0.; 3],
                    damage_overlay: false,
                };
                // Independent presentation pools exercise actual planner repair
                // and the output shader at ages one, two and three.
                let mut pools: Vec<_> = (1..=3)
                    .map(|count| {
                        let images: Vec<_> = (0..count)
                            .map(|_| {
                                gpu.device
                                    .create_image(&ImageDesc {
                                        dimension: Dimension::D2,
                                        size: Extent {
                                            width: doubled.0,
                                            height: doubled.1,
                                            depth_or_layers: 1,
                                        },
                                        format: Format::Rgba8Unorm,
                                        mips: 1,
                                        samples: 1,
                                        usage: Usage::COLOR | Usage::COPY_SRC,
                                        view_formats: &[],
                                        label: "aged presentation",
                                    })
                                    .unwrap()
                            })
                            .collect();
                        (
                            PoolState {
                                pools: vec![Pool {
                                    id: PoolId(2),
                                    size: doubled,
                                    buffers: vec![PoolBuffer::default(); count],
                                }],
                                ..Default::default()
                            },
                            images,
                        )
                    })
                    .collect();
                let mut seed = 0x2545_f491u32;
                let mut next = || {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (seed >> 16) % 257
                };
                let mut pixels: Vec<_> = (0..size.0 * size.1)
                    .map(|_| [next(), next(), next()])
                    .collect();
                for step in 0..15 {
                    let mut damage = adv_gpu::Region::empty(size);
                    if step == 0 {
                        damage = adv_gpu::Region::whole(size);
                    }
                    if step == 1 {
                        // Whole S and just one changed input cell.
                        pixels[(48 * size.0 + 64) as usize] = [256, 0, 128];
                        damage = adv_gpu::Region::whole(size);
                    } else if step == 3 {
                        for (x, y) in [(0, 0), (size.0 - 1, size.1 - 1)] {
                            pixels[(y * size.0 + x) as usize] = [next(), next(), next()];
                            damage.add(rect(x, y, 1, 1).into());
                        }
                    } else if step == 4 {
                        // Two paced publications merge before encoding. One
                        // intermediate change is cancelled by the newest frame.
                        let old = pixels[5];
                        pixels[5] = [next(), next(), next()];
                        damage.add(rect(5, 0, 1, 1).into());
                        pixels[(80 * size.0 + 90) as usize] = [next(), next(), next()];
                        pixels[5] = old;
                        damage.add(rect(90, 80, 1, 1).into());
                    } else if step >= 5 && step != 8 {
                        for _ in 0..3 {
                            let (x, y) = (next() % size.0, next() % size.1);
                            pixels[(y * size.0 + x) as usize] = [next(), next(), next()];
                            // Conservative quads include many unchanged cells.
                            let producer = if step == 10 {
                                rect(x, y, 1, 1)
                            } else {
                                rect(x.saturating_sub(16), y.saturating_sub(16), 40, 40)
                            };
                            damage.add(producer.into());
                        }
                    }
                    if step == 2 || step == 8 {
                        damage = adv_gpu::Region::whole(size);
                    }
                    let exact = step == 0 || step == 10;
                    let rects: Vec<Rect> = damage.rects().into_iter().map(Into::into).collect();
                    gpu.run(&partial, &pixels, Some(&rects), exact, &[]);
                    gpu.run(&full, &pixels, None, true, &[]);
                    let expected = gpu.read(&full.output);
                    assert_eq!(
                        gpu.read(&partial.output),
                        expected,
                        "{choice:?} {form} half={half} frame={step}"
                    );
                    let info = FrameInfo {
                        serial: step as u64,
                        canvas: size,
                        base: BaseKind::Gpu { presentable: false },
                        damage,
                        video: Default::default(),
                        cursor_regions: Default::default(),
                    };
                    for (state, images) in &mut pools {
                        let selected = step % images.len();
                        let pool = state.pools.iter_mut().find(|p| p.id == PoolId(2)).unwrap();
                        for (i, b) in pool.buffers.iter_mut().enumerate() {
                            b.in_use = i != selected;
                        }
                        if step >= images.len() {
                            assert_eq!(pool.buffers[selected].age, Some(images.len() as u32));
                        }
                        let plan = plan(&info, &settings, &caps, state);
                        let Stage::Output {
                            transform,
                            scale,
                            tiles,
                            ..
                        } = plan.stages.last().unwrap()
                        else {
                            panic!("output stage")
                        };
                        let target = &images[selected];
                        let mut encoder = gpu.queue.encoder("aged presentation");
                        if !tiles.rects.is_empty() {
                            let area = rect(0, 0, doubled.0, doubled.1).into();
                            let load = adv_gpu::encode::Load::Load;
                            let mut pass = encoder.render_pass(&RenderPassDesc {
                                colors: [ColorAttachment {
                                    view: target.view(),
                                    load,
                                    store: Store::Store,
                                    resolve: None,
                                }]
                                .into_iter()
                                .collect(),
                                depth_stencil: None,
                                area,
                                timestamps: None,
                                label: "aged presentation",
                            });
                            let spans = output::encode(
                                &mut pass,
                                output::Source::Image(partial.output.view()),
                                target.desc().size,
                                *transform,
                                *scale,
                                tiles,
                                (&presentation_pipeline, &mut gpu.ring),
                            )
                            .unwrap();
                            drop(pass);
                            for span in spans {
                                encoder.retain(span);
                            }
                        }
                        let token = gpu.queue.submit(encoder).unwrap();
                        assert!(gpu.queue.wait(token, Duration::from_secs(60)).unwrap());
                        state.after_present(&plan);
                        assert_eq!(
                            gpu.read(target),
                            expected,
                            "{choice:?} {form} half={half} frame={step} age={}",
                            images.len()
                        );
                    }
                }
            }
        }
    }
}

// spec: HOST-VIEW-10
#[test]
fn integer_translations_preserve_interior_bits_in_every_kernel_form() {
    let mut gpu = gpu();
    let matrix = adv_gpu::Features::MATRIX_F16_8X8
        | adv_gpu::Features::SHADER_F16
        | adv_gpu::Features::SUBGROUP;
    let size = (97u32, 81u32);
    let mut seed = 0x1649_b307u32;
    let pixels: Vec<_> = (0..size.0 * size.1)
        .map(|_| {
            std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (seed >> 16) % 257
            })
        })
        .collect();
    let mut failures = Vec::new();
    for half in [false, true] {
        if half
            && !gpu
                .device
                .caps()
                .features
                .contains(adv_gpu::Features::SHADER_F16)
        {
            continue;
        }
        for (form, table) in [models::REFERENCE, models::FUSED, models::MATRIX]
            .into_iter()
            .enumerate()
        {
            if form == 2 && (!half || !gpu.device.caps().features.contains(matrix)) {
                continue;
            }
            for (m, model) in table.into_iter().enumerate() {
                let original = gpu.scaler(model, size, half);
                let shifted = gpu.scaler(model, size, half);
                gpu.run(&original, &pixels, None, true, &[]);
                let expected = gpu.read(&original.output);
                for (dx, dy) in [
                    (1u32, 0u32),
                    (0, 1),
                    (1, 1),
                    (8, 0),
                    (0, 8),
                    (8, 8),
                    (16, 16),
                ] {
                    let translated: Vec<_> = (0..size.1)
                        .flat_map(|y| {
                            let pixels = &pixels;
                            (0..size.0).map(move |x| {
                                pixels[(y.saturating_sub(dy) * size.0 + x.saturating_sub(dx))
                                    as usize]
                            })
                        })
                        .collect();
                    gpu.run(&shifted, &translated, None, true, &[]);
                    let actual = gpu.read(&shifted.output);
                    let reach = model.reach();
                    let mut different = 0;
                    for y in (reach + dy) * 2..(size.1 - reach) * 2 {
                        for x in (reach + dx) * 2..(size.0 - reach) * 2 {
                            let a = &actual[y as usize][x as usize * 4..][..4];
                            let b =
                                &expected[(y - dy * 2) as usize][(x - dx * 2) as usize * 4..][..4];
                            different += usize::from(a != b);
                        }
                    }
                    if different != 0 && model.translations() {
                        failures.push(format!("model={m} form={form} half={half} offset=({dx},{dy}) pixels={different}"));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// spec: HOST-VIEW-13
#[test]
fn previous_output_proposals_preserve_pans_borders_and_wrong_candidates() {
    use adv_display::stages::upscale::DrawRecord;
    let mut gpu = gpu();
    let size = (129u32, 97u32);
    let mut seed = 0x741c_2e09u32;
    let source: Vec<[u32; 3]> = (0..size.0 * size.1)
        .map(|_| {
            std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (seed >> 16) % 257
            })
        })
        .collect();
    for half in [false, true] {
        if half
            && !gpu
                .device
                .caps()
                .features
                .contains(adv_gpu::Features::SHADER_F16)
        {
            continue;
        }
        for (choice, draw_records) in CHOICES.into_iter().flat_map(|c| [(c, true), (c, false)]) {
            let model = model(&gpu.device, choice, half);
            let partial = gpu.scaler(model, size, half);
            let full = gpu.scaler(model, size, half);
            for (frame, phase) in [[0, 0], [1, 0], [2, 0], [2, 1], [-1, 1], [0, 0], [17, -9]]
                .into_iter()
                .enumerate()
            {
                let pixels: Vec<_> = (0..size.1)
                    .flat_map(|y| {
                        let source = &source;
                        (0..size.0).map(move |x| {
                            let sx = (x as i32 - phase[0]).clamp(0, size.0 as i32 - 1) as u32;
                            let sy = (y as i32 - phase[1]).clamp(0, size.1 as i32 - 1) as u32;
                            source[(sy * size.0 + sx) as usize]
                        })
                    })
                    .collect();
                let proposal = DrawRecord {
                    target: Rect {
                        x: 0,
                        y: 0,
                        w: size.0,
                        h: size.1,
                    },
                    content: 1,
                    phase: if frame == 6 { [100, 100] } else { phase },
                    motion: None,
                };
                let records = if draw_records {
                    vec![proposal]
                } else {
                    Vec::new()
                };
                gpu.run(&partial, &pixels, None, false, &records);
                gpu.run(&full, &pixels, None, true, &[]);
                assert_eq!(
                    gpu.read(&partial.output),
                    gpu.read(&full.output),
                    "{choice:?} half={half} frame={frame}"
                );
            }
        }
    }
}

// spec: HOST-VIEW-10
#[test]
fn constant_inputs_have_one_exact_two_by_two_output_pattern() {
    let mut gpu = gpu();
    let size = (97u32, 81u32);
    for half in [false, true] {
        if half
            && !gpu
                .device
                .caps()
                .features
                .contains(adv_gpu::Features::SHADER_F16)
        {
            continue;
        }
        for choice in CHOICES {
            let scaler = gpu.scaler(model(&gpu.device, choice, half), size, half);
            let cached = gpu.scaler(model(&gpu.device, choice, half), size, half);
            for colour in [
                [0, 0, 0],
                [256, 256, 256],
                [128, 128, 128],
                [1, 1, 1],
                [127, 127, 127],
                [255, 255, 255],
                [256, 0, 128],
                [17, 203, 91],
                [0, 0, 0],
                [128, 128, 128],
            ] {
                gpu.run(
                    &scaler,
                    &vec![colour; (size.0 * size.1) as usize],
                    None,
                    true,
                    &[],
                );
                gpu.run(
                    &cached,
                    &vec![colour; (size.0 * size.1) as usize],
                    None,
                    false,
                    &[],
                );
                let expected = gpu.read(&scaler.output);
                let cached_output = gpu.read(&cached.output);
                let mismatches = expected
                    .iter()
                    .flatten()
                    .zip(cached_output.iter().flatten())
                    .filter(|(a, b)| a != b)
                    .count();
                assert_eq!(
                    mismatches, 0,
                    "constant reconstruction {choice:?} half={half} colour={colour:?}"
                );
                let output = gpu.read(&scaler.output);
                let mut mismatches = 0;
                for y in 0..size.1 * 2 {
                    for x in 0..size.0 * 2 {
                        mismatches += usize::from(
                            output[y as usize][x as usize * 4..][..4]
                                != output[(y % 2) as usize][(x % 2) as usize * 4..][..4],
                        );
                    }
                }
                assert_eq!(mismatches, 0, "{choice:?} half={half} colour={colour:?}");
            }
        }
    }
}
