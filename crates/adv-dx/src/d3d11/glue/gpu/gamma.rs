//! The exclusive output's gamma ramp, applied after resolving multisampling.
use super::*;
use adv_abi::types::DXGI_GAMMA_CONTROL;
use adv_device::gpu::{
    DrawArguments, Error as GpuError, Image, ModernColor, ModernDraw, ModernShader, SlotBindings,
    StorageBinding,
};
use adv_gpu::{
    encode::{Load, Store},
    pipeline::{Cull, Primitive, Topology},
    shader::Stage,
};
use std::sync::Arc;

pub(super) struct Pass {
    pub image: Image,
    view: ImageView,
    vertex: Arc<ModernShader>,
    fragment: Arc<ModernShader>,
    curve: BufferRange,
}

impl Pass {
    pub(super) fn new(
        gpu: &Gpu,
        size: (u32, u32),
        gamma: DXGI_GAMMA_CONTROL,
    ) -> Result<Self, Error> {
        let curve = std::array::from_fn::<_, 256, _>(|i| {
            let value = gamma.GammaCurve[i];
            [value.Red, value.Green, value.Blue, 0.0]
        });
        let bytes = bytemuck::cast_slice(&curve);
        let buffer = resource::upload(gpu, bytes, BufferUsage::STORAGE, bytes.len() as u64)?;
        let shader = |text: &str, stage| {
            ModernShader::wgsl(crate::record::key::hash(text) as u64, text, "main", stage)
                .map(Arc::new)
        };
        let target = gpu.device().create_image(&adv_gpu::resource::ImageDesc {
            dimension: adv_gpu::resource::Dimension::D2,
            format: adv_gpu::Format::Bgra8Unorm,
            size: Extent {
                width: size.0,
                height: size.1,
                depth_or_layers: 1,
            },
            mips: 1,
            samples: 1,
            usage: adv_gpu::resource::Usage::COLOR | adv_gpu::resource::Usage::COPY_SRC,
            view_formats: &[],
            label: "DXGI gamma output",
        })?;
        Ok(Self {
            view: target.view().clone(),
            image: Image::from_native(target),
            vertex: shader(
                "@vertex fn main(@builtin(vertex_index) id: u32) -> @builtin(position) vec4<f32> {
                    let corners = array(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
                    return vec4(corners[id], 0.0, 1.0);
                }",
                Stage::Vertex,
            )?,
            fragment: shader(
                "@group(1) @binding(0) var source: texture_2d<f32>;
                 @group(3) @binding(0) var<storage, read> curve: array<vec4<f32>, 256>;
                 @fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
                    let color = textureLoad(source, vec2<i32>(p.xy), 0);
                    let index = clamp(color.rgb, vec3(0.0), vec3(1.0)) * 255.0;
                    let i = vec3<u32>(index);
                    let j = min(i + vec3(1u), vec3(255u));
                    let lower = vec3(curve[i.r].r, curve[i.g].g, curve[i.b].b);
                    let upper = vec3(curve[j.r].r, curve[j.g].g, curve[j.b].b);
                    return vec4(mix(lower, upper, fract(index)), color.a);
                 }",
                Stage::Fragment,
            )?,
            curve: BufferRange {
                buffer,
                range: 0..bytes.len() as u64,
            },
        })
    }

    pub(super) fn size(&self) -> (u32, u32) {
        (self.image.width(), self.image.height())
    }

    pub(super) fn apply(
        &self,
        gpu: &mut Gpu,
        source: &adv_gpu::Image,
        copies: &[adv_gpu::Rect],
    ) -> Result<(), GpuError> {
        let (width, height) = self.size();
        let fragment = SlotBindings {
            images: vec![Some(source.view().clone())],
            storage: vec![Some(StorageBinding::Buffer(self.curve.clone()))],
            ..Default::default()
        };
        for &scissor in copies {
            gpu.draw_modern(
                ModernDraw {
                    state_ids: [0; 4],
                    vertex: self.vertex.clone(),
                    fragment: Some(self.fragment.clone()),
                    colors: std::iter::once(ModernColor {
                        view: self.view.clone(),
                        blend: None,
                        write_mask: 0xf,
                        load: Load::Load,
                        store: Store::Store,
                    })
                    .collect(),
                    color_slots: None,
                    depth: None,
                    samples: 1,
                    sample_mask: u32::MAX,
                    inexact_sample_mask: false,
                    alpha_to_coverage: false,
                    primitive: Primitive {
                        topology: Topology::Triangles,
                        cull: Cull::None,
                        front_ccw: true,
                        wireframe: false,
                        unclipped_depth: false,
                    },
                    area: scissor,
                    viewport: [0.0, 0.0, width as f32, height as f32, 0.0, 1.0],
                    scissor,
                    stencil_reference: 0,
                    blend_constant: [0.0; 4],
                    depth_bias: [0.0; 3],
                    arguments: DrawArguments::Direct {
                        vertices: 0..3,
                        instances: 0..1,
                        base_vertex: 0,
                    },
                    index: None,
                    occlusion: None,
                    predicate: None,
                },
                &SlotBindings::default(),
                &fragment,
            )?;
        }
        Ok(())
    }
}
