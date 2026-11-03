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
        Scale::Model => unreachable!("model output belongs to the upscaler"),
    };
    let d = transform.dst;
    let mut constants = ring.reserve(64, 256)?;
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
    let source_rotation = match &source {
        Source::Retained { rotation, .. } => match rotation {
            Rotation::None => 0u32,
            Rotation::Quarter => 1,
            Rotation::Half => 2,
            Rotation::ThreeQuarters => 3,
        },
        _ => 0,
    };
    constants.bytes()[48..52].copy_from_slice(&source_rotation.to_ne_bytes());

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
        Source::Image(view) | Source::Retained { view, .. } => pass.bind(0, Bind::Sampled(view)),
        Source::Software(s) => ingest::bind(pass, s, software_data.as_ref().unwrap()),
    }
    pass.set_viewport(0., 0., size.width as f32, size.height as f32, 0., 1.);
    pass.draw(0..6, 0..tiles.rects.len() as u32);
    let mut data = arrayvec::ArrayVec::new();
    data.push(constants);
    data.push(rectangles);
    if let Some(software) = software_data {
        data.push(software);
    }
    Ok(data)
}
