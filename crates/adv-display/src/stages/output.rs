//! One tile draw scales and rotates encoded RGB; the compositor owns the letterbox.
use super::{bindings, ingest};
use crate::plan::{Rotation, Scale, Tiles, Transform};
use adv_gpu::encode::{Bind, ColorAttachment, RenderPassDesc, Store};
use adv_gpu::pipeline::{ColorTarget, Cull, Primitive, RenderPipelineDesc, Topology};
use adv_gpu::{Device, Encoder, Format, ImageView, RenderPipeline, Ring};

pub enum Source<'a> {
    Image(&'a ImageView),
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
    encoder: &mut Encoder,
    source: Source<'_>,
    destination: &ImageView,
    transform: Transform,
    scale: Scale,
    tiles: &Tiles,
    resources: (&RenderPipeline, &mut Ring),
) -> adv_gpu::Result<()> {
    let (pipeline, ring) = resources;
    if tiles.rects.is_empty() {
        return Ok(());
    }
    let extent = match &source {
        Source::Image(v) => (v.size().width, v.size().height),
        Source::Software(s) => s.extent,
    };
    let (prescale, linear) = match scale {
        Scale::Integer { x, y } => ((x, y), 0u32),
        Scale::SharpBilinear { prescale } => (prescale, 1),
        Scale::Model => unreachable!("model output belongs to the upscaler"),
    };
    let d = transform.dst;
    let size = destination.size();
    let mut constants = ring.reserve(48, 256)?;
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
    for (out, value) in constants.bytes()[..40]
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(values)
    {
        out.copy_from_slice(&value.to_ne_bytes());
    }
    let rotation: u32 = match transform.rotation {
        Rotation::None => 0,
        Rotation::Quarter => 1,
        Rotation::Half => 2,
        Rotation::ThreeQuarters => 3,
    };
    constants.bytes()[40..44].copy_from_slice(&rotation.to_ne_bytes());
    constants.bytes()[44..48].copy_from_slice(&linear.to_ne_bytes());
    let mut rectangles = ring.reserve(tiles.rects.len() as u64 * 16, 256)?;
    for (out, r) in rectangles
        .bytes()
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
    // Software bindings carry their own source-sized tile list; output uses slot 2.
    let software_data = match &source {
        Source::Software(s) => Some(ingest::source(ring, s, &Tiles::default())?),
        _ => None,
    };
    let (area, load) = ingest::coverage(tiles);
    {
        let mut pass = encoder.render_pass(&RenderPassDesc {
            colors: [ColorAttachment {
                view: destination,
                load,
                store: Store::Store,
                resolve: None,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            area,
            timestamps: None,
            label: "display output",
        });
        pass.set_pipeline(pipeline);
        pass.bind(
            0,
            Bind::Uniform(
                constants
                    .buffer()
                    .slice(constants.offset()..constants.offset() + constants.len()),
            ),
        );
        pass.bind(
            2,
            Bind::StorageBuffer(
                rectangles
                    .buffer()
                    .slice(rectangles.offset()..rectangles.offset() + rectangles.len()),
            ),
        );
        match &source {
            Source::Image(view) => pass.bind(0, Bind::Sampled(view)),
            Source::Software(s) => ingest::bind(&mut pass, s, software_data.as_ref().unwrap()),
        }
        pass.set_viewport(0., 0., size.width as f32, size.height as f32, 0., 1.);
        pass.draw(0..6, 0..tiles.rects.len() as u32);
    }
    encoder.retain(constants);
    encoder.retain(rectangles);
    if let Some(data) = software_data {
        encoder.retain(data);
    }
    Ok(())
}
