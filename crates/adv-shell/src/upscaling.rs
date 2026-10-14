//! Fixed upstream spatial models run after game/video composition in encoded RGB.
//! Intermediate textures are reused after their last reader. Only the selected
//! model is resident; unchanged frames retain its output. The final linear
//! resample fits the existing physical-pixel viewport without changing input.
mod models;

use adv_device::gpu::{NativeContext, Submission};

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
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let input = texture(wgpu::TextureFormat::Rgba16Float, 1);
        let textures: Vec<_> = model.textures.iter().map(|&(f, s)| texture(f, s)).collect();
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("adv upscaling dimensions"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: true,
        });
        {
            let mut bytes = uniform
                .slice(..)
                .get_mapped_range_mut()
                .map_err(|e| e.to_string())?;
            let mut packed = [0u8; 16];
            for (out, value) in
                packed
                    .chunks_exact_mut(4)
                    .zip([size.0, size.1, size.0 * 2, size.1 * 2])
            {
                out.copy_from_slice(&value.to_le_bytes());
            }
            bytes.copy_from_slice(&packed);
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
        for pass in model.passes {
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
                        Binding::Uniform => uniform.as_entire_binding(),
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
            model,
        })
    }
    pub(crate) fn run(&self, context: &NativeContext, submission: &Submission<'_>) {
        let mut encoder = context.device.create_command_encoder(&Default::default());
        for ((spec, pipeline), binding) in self
            .model
            .passes
            .iter()
            .zip(&self.pipelines)
            .zip(&self.bindings)
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("adv upscaling"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, binding, &[]);
            pass.dispatch_workgroups(
                (self.size.0 * spec.scale).div_ceil(spec.block),
                (self.size.1 * spec.scale).div_ceil(spec.block),
                1,
            );
        }
        submission.submit([encoder.finish()]);
    }
    pub(crate) fn output(&self) -> &wgpu::TextureView {
        &self.textures[self.model.output]
    }
}
