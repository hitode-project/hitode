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
        let device = &context.device;
        let model = models::model(choice);
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
        for (index, pass) in model.passes.iter().enumerate() {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("adv upstream upscaling model"),
                source: wgpu::ShaderSource::Wgsl(pass.source.into()),
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
    /// pass is a 3×3 convolution, reading one input pixel around each output
    /// (the final pass also samples the input bilinearly). The last pass covers
    /// everything the change can reach; each earlier pass covers one pixel more
    /// than the pass after it, so no pass reads values outside what it was just
    /// given, even where a texture still holds another tensor. Returns the
    /// input pixels the last pass recomputed.
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
                needed = grow(*region, 1);
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
        self.model.passes.len() as u32 + 1
    }
    pub(crate) fn output(&self) -> &wgpu::TextureView {
        &self.textures[self.model.output]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `k / 256` as IEEE half-precision bits; exact for k below 256.
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
