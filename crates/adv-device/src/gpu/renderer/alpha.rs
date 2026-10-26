//! Compact alpha uploads expand on the GPU into the shared color representation.
use super::*;
use wgpu::util::DeviceExt;

pub(super) struct Upload {
    pipeline: wgpu::RenderPipeline,
}
impl Upload {
    fn new(context: &NativeContext) -> Self {
        let shader = context
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("alpha upload"),
                source: wgpu::ShaderSource::Wgsl(
                    r#"
@group(0) @binding(0) var alpha:texture_2d<f32>;
@group(0) @binding(1) var<uniform> origin:vec4<i32>;
@vertex fn vertex_main(@builtin(vertex_index) n:u32)->@builtin(position) vec4<f32> {
    let p=array<vec2<f32>,3>(vec2<f32>(-1.0,-1.0),vec2<f32>(3.0,-1.0),vec2<f32>(-1.0,3.0));
    return vec4<f32>(p[n],0.0,1.0);
}
@fragment fn fragment_main(@builtin(position) p:vec4<f32>)->@location(0) vec4<f32> {
    return vec4<f32>(0.0,0.0,0.0,textureLoad(alpha,vec2<i32>(p.xy)-origin.xy,0).r);
}
"#
                    .into(),
                ),
            });
        Self {
            pipeline: context
                .device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("alpha upload"),
                    layout: None,
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vertex_main"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fragment_main"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: FORMAT,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                }),
        }
    }
}
impl Gpu {
    pub fn upload_alpha(
        &self,
        image: &Image,
        rectangle: Rect,
        pitch: u32,
        alpha: &[u8],
    ) -> Result<(), Error> {
        self.own(image)?;
        image.writable()?;
        rectangle_in(image, rectangle)?;
        let required = u64::from(pitch) * u64::from(rectangle.h - 1) + u64::from(rectangle.w);
        if pitch < rectangle.w || required > alpha.len() as u64 {
            return Err(invalid("alpha upload pitch or storage is too small"));
        }
        let submission = self.context.submission_for(SwitchReason::Upload)?;
        let uploader = self.alpha_upload.get_or_init(|| Upload::new(&self.context));
        let source = self
            .context
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("compact alpha upload"),
                size: extent(rectangle.w, rectangle.h),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
        submission.write_texture(
            source.as_image_copy(),
            alpha,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pitch),
                rows_per_image: Some(rectangle.h),
            },
            extent(rectangle.w, rectangle.h),
        );
        let origin = self
            .context
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("alpha upload origin"),
                contents: bytemuck::bytes_of(&[rectangle.x as i32, rectangle.y as i32, 0, 0]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let binding = self
            .context
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("alpha upload"),
                layout: &uploader.pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &source.create_view(&Default::default()),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: origin.as_entire_binding(),
                    },
                ],
            });
        let mut encoder = self
            .context
            .device
            .create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("alpha upload"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &image.render_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&uploader.pipeline);
            pass.set_bind_group(0, &binding, &[]);
            pass.set_viewport(
                rectangle.x as f32,
                rectangle.y as f32,
                rectangle.w as f32,
                rectangle.h as f32,
                0.,
                1.,
            );
            pass.set_scissor_rect(rectangle.x, rectangle.y, rectangle.w, rectangle.h);
            pass.draw(0..3, 0..1);
        }
        submission.submit([encoder.finish()]);
        self.check()
    }
}
