//! One tile draw scales and rotates encoded RGB; the compositor owns the letterbox.
use super::{bindings, ingest};
use crate::plan::{Rotation, Scale, Tiles, Transform};
use adv_gpu::encode::Bind;
use adv_gpu::pipeline::{ColorTarget, Cull, Primitive, RenderPipelineDesc, Topology};
use adv_gpu::{Device, Extent, Format, ImageView, RenderPass, RenderPipeline, Ring, Span};

pub enum Source<'a> {
    Image(&'a ImageView),
    Retained {
        view: &'a ImageView,
        canvas: (u32, u32),
        rotation: Rotation,
    },
    Software(ingest::Source<'a>),
}
pub fn pipeline(
    device: &Device,
    format: Format,
    software: bool,
) -> adv_gpu::Result<RenderPipeline> {
    let shader = device.create_shader(adv_gpu::shader::ShaderDesc {
        source: adv_gpu::shader::Source::Precompiled {
            spirv: &include!(concat!(env!("OUT_DIR"), "/output.spv.rs")),
            metal: include_bytes!(concat!(env!("OUT_DIR"), "/output.msl")),
            entries: &include!(concat!(env!("OUT_DIR"), "/output.entries.rs")),
        },
        identity: include!(concat!(env!("OUT_DIR"), "/output.identity.rs")),
        label: "display output",
    })?;
    device.create_render_pipeline(
        &RenderPipelineDesc {
            vertex: shader.entry("vs", adv_gpu::shader::Stage::Vertex)?,
            fragment: Some(shader.entry(
                if software { "fs_software" } else { "fs" },
                adv_gpu::shader::Stage::Fragment,
            )?),
            bindings: bindings::OUTPUT
                .into_iter()
                .filter(|b| {
                    !software
                        || !matches!(b.kind, adv_gpu::pipeline::BindingKind::SampledImage { .. })
                })
                .chain(if software {
                    bindings::INGEST.to_vec()
                } else {
                    vec![]
                })
                .collect(),
            primitive: Primitive {
                topology: Topology::Triangles,
                cull: Cull::None,
                front_ccw: true,
                wireframe: false,
                unclipped_depth: false,
            },
            colors: [ColorTarget {
                format,
                blend: None,
                write_mask: 15,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            samples: 1,
            alpha_to_coverage: false,
            label: "display output",
        },
        None,
    )
}

pub fn encode(
    pass: &mut RenderPass<'_>,
    source: Source<'_>,
    size: Extent,
    transform: Transform,
    scale: Scale,
    tiles: &Tiles,
    resources: (&RenderPipeline, &mut Ring),
) -> adv_gpu::Result<arrayvec::ArrayVec<Span, 3>> {
    let (pipeline, ring) = resources;
    if tiles.rects.is_empty() {
        return Ok(Default::default());
    }
    let extent = match &source {
        Source::Retained { canvas, .. } => *canvas,
        Source::Image(v) => (v.size().width, v.size().height),
        Source::Software(s) => s.extent,
    };
    let (prescale, linear) = match scale {
        Scale::Integer { x, y } => ((x, y), 0u32),
        Scale::SharpBilinear { prescale } => (prescale, 1),
    };
    let d = transform.dst;
    let rects_offset = 256u64;
    let rects_len = tiles.rects.len() as u64 * 16;
    let software_offset = (rects_offset + rects_len).next_multiple_of(256);
    let software_len = if matches!(source, Source::Software(_)) {
        ingest::source_bytes(0)
    } else {
        0
    };
    let total = if software_len > 0 {
        software_offset + software_len
    } else {
        rects_offset + rects_len
    };
    let mut span = ring.reserve(total, 256)?;
    {
        let bytes = span.bytes();
        let values = [
            size.width as f32,
            size.height as f32,
            extent.0 as f32,
            extent.1 as f32,
            d.x as f32,
            d.y as f32,
            d.w as f32,
            d.h as f32,
            prescale.0 as f32,
            prescale.1 as f32,
        ];
        for (out, value) in bytes[..40].as_chunks_mut::<4>().0.iter_mut().zip(values) {
            out.copy_from_slice(&value.to_ne_bytes());
        }
        let rotation: u32 = match transform.rotation {
            Rotation::None => 0,
            Rotation::Quarter => 1,
            Rotation::Half => 2,
            Rotation::ThreeQuarters => 3,
        };
        bytes[40..44].copy_from_slice(&rotation.to_ne_bytes());
        bytes[44..48].copy_from_slice(&linear.to_ne_bytes());
        let source_rotation = match &source {
            Source::Retained { rotation, .. } => match rotation {
                Rotation::None => 0u32,
                Rotation::Quarter => 1,
                Rotation::Half => 2,
                Rotation::ThreeQuarters => 3,
            },
            _ => 0,
        };
        bytes[48..52].copy_from_slice(&source_rotation.to_ne_bytes());
        for (out, r) in bytes[rects_offset as usize..(rects_offset + rects_len) as usize]
            .as_chunks_mut::<16>()
            .0
            .iter_mut()
            .zip(&tiles.rects)
        {
            for (out, v) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip([r.x, r.y, r.w, r.h])
            {
                out.copy_from_slice(&v.to_ne_bytes());
            }
        }
        if let Source::Software(s) = &source {
            ingest::write_source(
                &mut bytes[software_offset as usize..(software_offset + software_len) as usize],
                s,
                &Tiles::default(),
            );
        }
    }
    let base = span.offset();
    let buffer = span.buffer();
    pass.set_pipeline(pipeline);
    pass.bind(0, Bind::Uniform(buffer.slice(base..base + 64)));
    pass.bind(
        2,
        Bind::StorageBuffer(buffer.slice(base + rects_offset..base + rects_offset + rects_len)),
    );
    match &source {
        Source::Image(view) | Source::Retained { view, .. } => pass.bind(0, Bind::Sampled(view)),
        Source::Software(s) => ingest::bind_slice(
            pass,
            s,
            buffer.slice(base + software_offset..base + software_offset + software_len),
        ),
    }
    pass.set_viewport(0., 0., size.width as f32, size.height as f32, 0., 1.);
    pass.draw(0..6, 0..tiles.rects.len() as u32);
    let mut data = arrayvec::ArrayVec::new();
    data.push(span);
    Ok(data)
}
