//! Fixed upstream spatial models run after game/video composition in encoded RGB.
//! Intermediate textures are reused after their last reader. Only the selected
//! model is resident; unchanged frames retain its output. The final linear
//! resample fits the existing physical-pixel viewport without changing input.
mod models;

use adv_device::gpu::{NativeContext, Submission};
use adv_device::surface::Rect;

/// Uniform offsets must be 256-byte aligned.
const SLOT: u64 = 256;

/// Each pass's uniform slot: dimensions, then its region's origin in input
/// and output pixels (zero for a whole-frame run).
fn slots_for(size: (u32, u32), regions: &[Option<Rect>]) -> Vec<u8> {
    let mut bytes = vec![0u8; SLOT as usize * regions.len()];
    for (slot, region) in bytes.chunks_exact_mut(SLOT as usize).zip(regions) {
        let (x, y) = region.map_or((0, 0), |r| (r.x, r.y));
        let values = [size.0, size.1, size.0 * 2, size.1 * 2, x, y, x * 2, y * 2];
        for (out, value) in slot.chunks_exact_mut(4).zip(values) {
            out.copy_from_slice(&value.to_le_bytes());
        }
    }
    bytes
}

/// Rewrites a half-precision pass for devices without `SHADER_F16`, keeping
/// its f16-rounded weights.
fn widen(source: &str) -> String {
    let source = source.replacen("enable f16;", "", 1);
    let mut out = String::with_capacity(source.len());
    // Words include dots, so `1.5e-05h` ends in the word `05h`.
    for (index, word) in source
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .enumerate()
    {
        // Replacements keep lengths, so `out.len()` indexes the next separator.
        if index > 0 {
            out.push(source.as_bytes()[out.len()] as char);
        }
        if word == "f16" {
            out.push_str("f32");
        } else if word.starts_with(|c: char| c.is_ascii_digit()) && word.ends_with('h') {
            out.push_str(&word[..word.len() - 1]);
            out.push('f');
        } else {
            out.push_str(word);
        }
    }
    out
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Upscaler {
    #[default]
    Off,
    Anime4k,
    Cunny,
    Artcnn,
    K7,
}

pub(super) enum Binding {
    Uniform,
    Source,
    Texture(usize),
    Nearest,
    Linear,
}
struct Pass {
    source: &'static str,
    bindings: &'static [(u32, Binding)],
    block: u32,
    scale: u32,
    radius: u32,
}
struct Model {
    textures: &'static [(wgpu::TextureFormat, u32)],
    output: usize,
    passes: &'static [Pass],
}
pub(crate) struct Upscaling {
    pub(crate) choice: Upscaler,
    pub(crate) size: (u32, u32),
    pub(crate) input: wgpu::TextureView,
    textures: Vec<wgpu::TextureView>,
    pipelines: Vec<wgpu::ComputePipeline>,
    bindings: Vec<wgpu::BindGroup>,
    uniform: wgpu::Buffer,
    model: Model,
}
impl Upscaling {
    pub(crate) fn new(
        context: &NativeContext,
        choice: Upscaler,
        size: (u32, u32),
    ) -> Result<Self, String> {
        let matrix = context.device.features().contains(
            wgpu::Features::SHADER_F16
                | wgpu::Features::SUBGROUP
                | wgpu::Features::SUBGROUP_BARRIER
                | wgpu::Features::EXPERIMENTAL_COOPERATIVE_MATRIX,
        ) && context
            .adapter
            .cooperative_matrix_properties()
            .iter()
            .any(|p| {
                p.m_size == 8
                    && p.n_size == 8
                    && p.k_size == 8
                    && p.ab_type == wgpu::CooperativeScalarType::F16
                    && p.cr_type == wgpu::CooperativeScalarType::F16
            });
        Self::with_model(
            context,
            choice,
            size,
            if matrix {
                models::matrix_model(choice)
            } else {
                models::fused_model(choice)
            },
        )
    }
    fn with_model(
        context: &NativeContext,
        choice: Upscaler,
        size: (u32, u32),
        model: Model,
    ) -> Result<Self, String> {
        let device = &context.device;
        let max = device.limits().max_texture_dimension_2d;
        if size.0 > max / 2 || size.1 > max / 2 {
            return Err("upscaled game frame exceeds the GPU texture limit".into());
        }
        let errors = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let texture = |format, scale| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("adv upscaling frame"),
                    size: wgpu::Extent3d {
                        width: size.0 * scale,
                        height: size.1 * scale,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::STORAGE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let input = texture(wgpu::TextureFormat::Rgba16Float, 1);
        let textures: Vec<_> = model.textures.iter().map(|&(f, s)| texture(f, s)).collect();
        // One slot per pass: dimensions, then that pass's origin (`run`).
        let slots = model.passes.len() as u64;
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("adv upscaling dimensions"),
            size: SLOT * slots,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        {
            let mut bytes = uniform
                .slice(..)
                .get_mapped_range_mut()
                .map_err(|e| e.to_string())?;
            bytes.copy_from_slice(&slots_for(size, &vec![None; model.passes.len()]));
        }
        uniform.unmap();
        let nearest = device.create_sampler(&Default::default());
        let linear = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let mut pipelines = Vec::new();
        let mut bindings = Vec::new();
        let half = device.features().contains(wgpu::Features::SHADER_F16);
        for (index, pass) in model.passes.iter().enumerate() {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("adv upstream upscaling model"),
                source: wgpu::ShaderSource::Wgsl(if half {
                    pass.source.into()
                } else {
                    widen(pass.source).into()
                }),
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("adv upscaling pass"),
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
            let entries: Vec<_> = pass
                .bindings
                .iter()
                .map(|(binding, resource)| wgpu::BindGroupEntry {
                    binding: *binding,
                    resource: match resource {
                        Binding::Uniform => wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &uniform,
                            offset: SLOT * index as u64,
                            size: std::num::NonZeroU64::new(32),
                        }),
                        Binding::Source => wgpu::BindingResource::TextureView(&input),
                        Binding::Texture(i) => wgpu::BindingResource::TextureView(&textures[*i]),
                        Binding::Nearest => wgpu::BindingResource::Sampler(&nearest),
                        Binding::Linear => wgpu::BindingResource::Sampler(&linear),
                    },
                })
                .collect();
            bindings.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("adv upscaling bindings"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &entries,
            }));
            pipelines.push(pipeline);
        }
        if let Some(error) = pollster::block_on(errors.pop()) {
            return Err(format!("upscaling model validation failed: {error}"));
        }
        Ok(Self {
            choice,
            size,
            input,
            textures,
            pipelines,
            bindings,
            uniform,
            model,
        })
    }
    /// Recompute after the input changed within `damage`, or entirely. Every
    /// layer is a 3×3 convolution; a fused pass retains the sum of its layers'
    /// radii. The final pass also samples the input bilinearly. Earlier passes
    /// cover the next pass's rounded workgroups plus its radius, so reads stay
    /// within values computed this run. Returns the input pixels recomputed by
    /// the last pass.
    pub(crate) fn run(
        &self,
        context: &NativeContext,
        submission: &Submission<'_>,
        damage: Option<Rect>,
    ) -> u64 {
        let full = Rect {
            x: 0,
            y: 0,
            w: self.size.0,
            h: self.size.1,
        };
        let grow = |r: Rect, by: u32| {
            let x = r.x.saturating_sub(by);
            let y = r.y.saturating_sub(by);
            Rect {
                x,
                y,
                w: (r.x + r.w + by).min(self.size.0).saturating_sub(x),
                h: (r.y + r.h + by).min(self.size.1).saturating_sub(y),
            }
        };
        // Work backwards from the last pass. Each pass writes whole workgroups,
        // so its earlier pass must cover that rounded area plus one pixel.
        let mut regions = vec![full; self.model.passes.len()];
        if let Some(damage) = damage {
            let mut needed = grow(damage, self.reach());
            for (spec, region) in self.model.passes.iter().zip(&mut regions).rev() {
                let unit = spec.block / spec.scale;
                *region = Rect {
                    w: needed.w.next_multiple_of(unit),
                    h: needed.h.next_multiple_of(unit),
                    ..needed
                };
                needed = grow(*region, spec.radius);
            }
        }
        if regions.iter().any(|r| r.w == 0 || r.h == 0) {
            return 0;
        }
        let origins = slots_for(
            self.size,
            &regions
                .iter()
                .map(|r| damage.map(|_| *r))
                .collect::<Vec<_>>(),
        );
        submission.write_buffer(&self.uniform, 0, &origins);
        let mut encoder = context.device.create_command_encoder(&Default::default());
        for (((spec, pipeline), binding), r) in self
            .model
            .passes
            .iter()
            .zip(&self.pipelines)
            .zip(&self.bindings)
            .zip(&regions)
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("adv upscaling"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, binding, &[]);
            pass.dispatch_workgroups(
                (r.w * spec.scale).div_ceil(spec.block),
                (r.h * spec.scale).div_ceil(spec.block),
                1,
            );
        }
        submission.submit([encoder.finish()]);
        let last = regions[regions.len() - 1];
        u64::from(last.w) * u64::from(last.h)
    }
    /// Input pixels the last pass recomputes after a change within `damage`.
    pub(crate) fn reach(&self) -> u32 {
        self.model
            .passes
            .iter()
            .map(|pass| pass.radius)
            .sum::<u32>()
            + 1
    }
    pub(crate) fn output(&self) -> &wgpu::TextureView {
        &self.textures[self.model.output]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn write(context: &NativeContext, view: &wgpu::TextureView, pixels: &[[u32; 3]]) {
        let texture = view.texture();
        let bytes: Vec<u8> = pixels
            .iter()
            .flat_map(|p| [half(p[0]), half(p[1]), half(p[2]), [0x00, 0x3c]])
            .flatten()
            .collect();
        let submission = context.submission().unwrap();
        submission.write_texture(
            texture.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(texture.width() * 8),
                rows_per_image: None,
            },
            texture.size(),
        );
    }

    fn read(context: &NativeContext, view: &wgpu::TextureView) -> Vec<Vec<u8>> {
        let texture = view.texture();
        let row = (texture.width() * 8).next_multiple_of(256);
        let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row * texture.height()),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = context.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        let index = context.submission().unwrap().submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        context
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: Some(std::time::Duration::from_secs(30)),
            })
            .unwrap();
        let mapped = buffer.get_mapped_range(..).unwrap();
        mapped
            .chunks_exact(row as usize)
            .map(|line| line[..texture.width() as usize * 8].to_vec())
            .collect()
    }

    // spec: HOST-VIEW-7
    #[test]
    fn half_precision_passes_widen_to_valid_single_precision() {
        use wgpu::naga::valid::{Capabilities, ValidationFlags, Validator};
        assert_eq!(
            widen("enable f16;\nlet a : vec2<f16> = vec2<f16>(1.5e-05h, 2.0h) * x1h;"),
            "\nlet a : vec2<f32> = vec2<f32>(1.5e-05f, 2.0f) * x1h;"
        );
        let mut half = 0;
        for choice in [
            Upscaler::Anime4k,
            Upscaler::Cunny,
            Upscaler::Artcnn,
            Upscaler::K7,
        ] {
            for pass in models::model(choice).passes {
                half += usize::from(pass.source.contains("enable f16;"));
                for (source, capabilities) in [
                    (pass.source.to_owned(), Capabilities::all()),
                    (
                        widen(pass.source),
                        Capabilities::all() - Capabilities::SHADER_FLOAT16,
                    ),
                ] {
                    let module = wgpu::naga::front::wgsl::parse_str(&source).unwrap();
                    Validator::new(ValidationFlags::all(), capabilities)
                        .validate(&module)
                        .unwrap();
                }
            }
        }
        assert_eq!(half, 4 + 6 + 8, "the three Magpie models declare FP16");
        for choice in [
            Upscaler::Anime4k,
            Upscaler::Cunny,
            Upscaler::Artcnn,
            Upscaler::K7,
        ] {
            for pass in models::fused_model(choice).passes {
                for source in [pass.source.to_owned(), widen(pass.source)] {
                    let module = wgpu::naga::front::wgsl::parse_str(&source).unwrap();
                    Validator::new(ValidationFlags::all(), Capabilities::all())
                        .validate(&module)
                        .unwrap();
                }
            }
        }
    }

    // spec: HOST-VIEW-7, HOST-VIEW-10
    #[test]
    fn optimized_layers_match_reference_display_pixels_and_clamped_edges() {
        let mut gpu = pollster::block_on(adv_device::gpu::Gpu::request(false)).unwrap();
        let context = &gpu.context().clone();
        let format = wgpu::TextureFormat::Bgra8Unorm;
        let compositor = crate::native_gpu::Compositor::new(context.clone(), format);
        for choice in [
            Upscaler::Anime4k,
            Upscaler::Cunny,
            Upscaler::Artcnn,
            Upscaler::K7,
        ] {
            let mut maximum = [0u8; 2];
            for (size, patterns) in [((1, 1), 3), ((35, 29), 3), ((130, 97), 3), ((513, 257), 1)] {
                let scalers = [
                    Upscaling::with_model(context, choice, size, models::model(choice)).unwrap(),
                    Upscaling::with_model(context, choice, size, models::fused_model(choice))
                        .unwrap(),
                    Upscaling::new(context, choice, size).unwrap(),
                ];
                if size == (1, 1) {
                    eprintln!(
                        "{choice:?}: passes={} matrix_rgb={}",
                        scalers[2].model.passes.len(),
                        scalers[2].model.passes[0]
                            .source
                            .contains("coopMultiplyAdd")
                    );
                }
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
                    for scaler in &scalers {
                        write(context, &scaler.input, &pixels);
                        scaler.run(context, &context.submission().unwrap(), None);
                    }
                    for output_size in [(size.0 * 2, size.1 * 2), (size.0 * 3, size.1 * 3)] {
                        let destination = gpu.image(output_size.0, output_size.1).unwrap();
                        let mut results = Vec::new();
                        for scaler in &scalers {
                            compositor
                                .compose_view(
                                    scaler.output(),
                                    destination.view(),
                                    format,
                                    [0., 0., output_size.0 as f32, output_size.1 as f32],
                                    &context.submission().unwrap(),
                                )
                                .unwrap();
                            let mut transfer = gpu.readback(&destination, None).unwrap();
                            transfer.take_completion_job().unwrap()();
                            results.push(transfer.poll().unwrap().unwrap());
                        }
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
                                "{choice:?} {size:?} pattern={pattern} output={output_size:?} channel_error={delta}"
                            );
                        }
                        assert!(
                            results
                                .iter()
                                .all(|r| r.chunks_exact(4).all(|p| p[3] == 255))
                        );
                    }
                }
                assert!(scalers.iter().all(|s| s.reach() == scalers[0].reach()));
            }
            eprintln!(
                "{choice:?}: max_channel_error fused={} selected={}",
                maximum[0], maximum[1]
            );
        }
    }

    // spec: HOST-VIEW-10
    #[test]
    fn partial_runs_match_full_runs_within_the_model_radius() {
        let gpu = pollster::block_on(adv_device::gpu::Gpu::request(!cfg!(any(
            target_os = "macos",
            target_os = "android"
        ))))
        .unwrap();
        let context = gpu.context().clone();
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
        let damages = [
            Rect {
                x: 77,
                y: 61,
                w: 3,
                h: 2,
            },
            Rect {
                x: 0,
                y: 0,
                w: 2,
                h: 5,
            },
            Rect {
                x: 150,
                y: 120,
                w: 10,
                h: 8,
            },
            Rect {
                x: 40,
                y: 100,
                w: 31,
                h: 1,
            },
        ];
        for choice in [
            Upscaler::Anime4k,
            Upscaler::Cunny,
            Upscaler::Artcnn,
            Upscaler::K7,
        ] {
            let full = Upscaling::new(&context, choice, size).unwrap();
            let partial = Upscaling::new(&context, choice, size).unwrap();
            for upscaling in [&full, &partial] {
                write(&context, &upscaling.input, &pixels);
                upscaling.run(&context, &context.submission().unwrap(), None);
            }
            let mut frame = pixels.clone();
            for (step, damage) in damages.iter().enumerate() {
                let before = read(&context, full.output());
                for y in damage.y..damage.y + damage.h {
                    for x in damage.x..damage.x + damage.w {
                        frame[(y * size.0 + x) as usize] = [255, step as u32 * 60, 128];
                    }
                }
                for upscaling in [&full, &partial] {
                    write(&context, &upscaling.input, &frame);
                }
                full.run(&context, &context.submission().unwrap(), None);
                partial.run(&context, &context.submission().unwrap(), Some(*damage));
                let after = read(&context, full.output());
                assert!(
                    read(&context, partial.output()) == after,
                    "{choice:?} partial run {step} differs"
                );
                // How far, in input pixels, the change reached in the output.
                let mut reach = 0;
                for (y, (old, new)) in before.iter().zip(&after).enumerate() {
                    for x in 0..old.len() / 8 {
                        if old[x * 8..x * 8 + 8] != new[x * 8..x * 8 + 8] {
                            let (x, y) = (x as u32 / 2, y as u32 / 2);
                            let dx = damage
                                .x
                                .saturating_sub(x)
                                .max(x.saturating_sub(damage.x + damage.w - 1));
                            let dy = damage
                                .y
                                .saturating_sub(y)
                                .max(y.saturating_sub(damage.y + damage.h - 1));
                            reach = reach.max(dx.max(dy));
                        }
                    }
                }
                assert!(reach <= full.reach(), "{choice:?} reaches {reach}");
            }
        }
    }
}
