use adv_display::Rect;
use adv_display::plan::Upscaler;
use adv_display::stages::upscale::{Model, Upscaling, model, models};
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
    fn scaler(&self, model: Model, size: (u32, u32)) -> Scaler {
        let half = self
            .device
            .caps()
            .features
            .contains(adv_gpu::Features::SHADER_F16);
        self.scaler_at(model, size, half)
    }
    fn scaler_at(&self, model: Model, size: (u32, u32), half: bool) -> Scaler {
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
                usage: Usage::STORAGE | Usage::COPY_SRC,
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
    fn run(&mut self, scaler: &Scaler, pixels: &[[u32; 3]], damage: Option<&[Rect]>) -> u64 {
        let bytes: Vec<u8> = pixels
            .iter()
            .flat_map(|p| [half(p[0]), half(p[1]), half(p[2]), [0x00, 0x3c]])
            .flatten()
            .collect();
        let upload = self.buffer(bytes.len() as u64, BufferUsage::COPY_SRC, Memory::Upload);
        let mapped = unsafe { upload.mapped().unwrap().as_mut() };
        mapped[..bytes.len()].copy_from_slice(&bytes);
        let input = scaler.upscaling.input();
        let mut encoder = self.queue.encoder("upscale test");
        encoder.copy_buffer_to_image(
            BufferCopy {
                buffer: &upload,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            ImageCopy {
                image: input,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            input.desc().size,
        );
        let computed = scaler
            .upscaling
            .encode(&mut encoder, &mut self.ring, scaler.output.view(), damage)
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
        let selected = model(&gpu.device, choice);
        for (size, patterns) in [((1, 1), 3), ((35, 29), 3), ((130, 97), 3), ((513, 257), 1)] {
            let scalers = [
                gpu.scaler(models::REFERENCE[index(choice)], size),
                gpu.scaler(models::FUSED[index(choice)], size),
                gpu.scaler(selected, size),
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
                        gpu.run(scaler, &pixels, None);
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
        let selected = model(&gpu.device, choice);
        let full = gpu.scaler(selected, size);
        let partial = gpu.scaler(selected, size);
        for scaler in [&full, &partial] {
            gpu.run(scaler, &pixels, None);
        }
        let reach = selected.reach();
        let mut tightest = 0;
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
            gpu.run(&full, &frame, None);
            let computed = gpu.run(&partial, &frame, Some(damage));
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
            tightest = tightest.max(reached);
        }
        assert_eq!(tightest, reach, "{choice:?} reach is not tight");
        // Nothing changed: nothing is computed.
        let mut encoder = gpu.queue.encoder("upscale test");
        let computed = partial
            .upscaling
            .encode(
                &mut encoder,
                &mut gpu.ring,
                partial.output.view(),
                Some(&[]),
            )
            .unwrap();
        assert_eq!(computed, 0);
    }
}

// Names the variant a driver computes wrongly: each model's reference and
// fused tables in each precision, run twice, against the reference in the
// same precision and against single precision.
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
    let half = gpu
        .device
        .caps()
        .features
        .contains(adv_gpu::Features::SHADER_F16);
    let delta = |a: &[u8], b: &[u8]| a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
    let mut report = vec![];
    let mut wrong = false;
    for choice in CHOICES {
        let mut single = None;
        for precision in [false, true].into_iter().filter(|&h| h <= half) {
            let mut reference = None;
            for (table, models) in [("reference", models::REFERENCE), ("fused", models::FUSED)] {
                let scaler = gpu.scaler_at(models[index(choice)], size, precision);
                let runs: Vec<_> = (0..2)
                    .map(|_| {
                        gpu.run(&scaler, &pixels, None);
                        gpu.read(&scaler.output).concat()
                    })
                    .collect();
                let base = reference.get_or_insert_with(|| runs[0].clone());
                let single = single.get_or_insert_with(|| runs[0].clone());
                let (error, from_single) = (delta(base, &runs[0]), delta(single, &runs[0]));
                let repeats = runs[0] == runs[1];
                wrong |= error > 2 || !repeats;
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
