//! One instanced draw per video: damage tiles sample YUV or paint the border.
//! Software bases decode inline. Metal samples planes; Android samples the
//! pool's converted RGB. Plane samples interpolate in float before the color matrix.

use super::{bindings, ingest};
use crate::{Rect, plan::Tiles};
use adv_gpu::{
    Device, Format, Image, ImageView, PresentImage, PresentPool, RenderPass, RenderPipeline,
    Result, Ring, Sampler,
    encode::Bind,
    pipeline::{
        BindingKind, ColorTarget, Cull, Primitive, RenderPipelineDesc, Topology, VideoPipelineDesc,
    },
    present::YuvColor,
    resource::{Address, BorderColor, BufferSlice, Filter, SamplerDesc},
    shader::{ShaderDesc, Source, Stage},
};

#[derive(Clone, Copy)]
pub struct Geometry {
    pub destination: Rect,
    /// Normalized source crop, including owner clipping.
    pub source_uv: [f32; 4],
    pub background: Option<(Rect, [u8; 4])>,
    pub color: YuvColor,
}

/// Create once for the canvas format, off the encoding path.
pub fn resources(device: &Device, format: Format) -> Result<(RenderPipeline, Sampler)> {
    resources_for(device, format, false)
}

/// Create the pipeline that decodes the software base in the video draw.
pub fn software_resources(device: &Device, format: Format) -> Result<(RenderPipeline, Sampler)> {
    resources_for(device, format, true)
}
fn resources_for(
    device: &Device,
    format: Format,
    software: bool,
) -> Result<(RenderPipeline, Sampler)> {
    let pipeline = pipeline(device, format, software, None)?;
    let sampler = device.sampler(&SamplerDesc {
        min: Filter::Linear,
        mag: Filter::Linear,
        mip: Filter::Nearest,
        address: [Address::Clamp; 3],
        border: BorderColor::OpaqueBlack,
        lod_bias: 0,
        lod_min: 0,
        lod_max: 0,
        anisotropy: 1,
        compare: None,
    })?;
    Ok((pipeline, sampler))
}

/// Build once for the converted-video pool, off the encoding path.
pub fn converted_resources(
    device: &Device,
    format: Format,
    pool: &PresentPool,
) -> Result<RenderPipeline> {
    pipeline(device, format, false, Some(pool))
}

/// Decode the software base inline with a converted video picture.
pub fn converted_software_resources(
    device: &Device,
    format: Format,
    pool: &PresentPool,
) -> Result<RenderPipeline> {
    pipeline(device, format, true, Some(pool))
}

fn pipeline(
    device: &Device,
    format: Format,
    software: bool,
    pool: Option<&PresentPool>,
) -> Result<RenderPipeline> {
    let converted = pool.is_some() && cfg!(target_os = "android");
    let shader = device.create_shader(ShaderDesc {
        source: if converted {
            Source::Precompiled {
                spirv: &include!(concat!(env!("OUT_DIR"), "/video_converted.spv.rs")),
                metal: include_bytes!(concat!(env!("OUT_DIR"), "/video_converted.msl")),
                entries: &include!(concat!(env!("OUT_DIR"), "/video_converted.entries.rs")),
            }
        } else {
            Source::Precompiled {
                spirv: &include!(concat!(env!("OUT_DIR"), "/video.spv.rs")),
                metal: include_bytes!(concat!(env!("OUT_DIR"), "/video.msl")),
                entries: &include!(concat!(env!("OUT_DIR"), "/video.entries.rs")),
            }
        },
        identity: if converted {
            include!(concat!(env!("OUT_DIR"), "/video_converted.identity.rs"))
        } else {
            include!(concat!(env!("OUT_DIR"), "/video.identity.rs"))
        },
        label: "video",
    })?;
    let mut desc = RenderPipelineDesc {
        vertex: shader.entry(if software { "vs_software" } else { "vs" }, Stage::Vertex)?,
        fragment: Some(shader.entry(if software { "fs_software" } else { "fs" }, Stage::Fragment)?),
        bindings: if software {
            bindings::VIDEO
                .iter()
                .copied()
                .filter(|b| b.slot != 2)
                .chain(bindings::INGEST)
                .collect()
        } else {
            bindings::VIDEO.to_vec()
        },
        primitive: Primitive {
            topology: Topology::Triangles,
            cull: Cull::None,
            front_ccw: false,
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
        label: "video",
    };
    if let Some(pool) = pool {
        desc.bindings.retain(|b| {
            matches!(
                b.kind,
                BindingKind::UniformBuffer | BindingKind::StorageBuffer { .. }
            )
        });
        device.create_video_pipeline(
            &VideoPipelineDesc {
                pipeline: &desc,
                pool,
            },
            None,
        )
    } else {
        device.create_render_pipeline(&desc, None)
    }
}

/// Compose into an existing canvas. Tiles outside the video and its background
/// preserve the base. `resources` and the producer's ring are reused each frame.
pub fn encode(
    pass: &mut RenderPass<'_>,
    destination: &Image,
    planes: &[ImageView; 2],
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &Sampler, &mut Ring),
) -> Result<arrayvec::ArrayVec<adv_gpu::Span, 2>> {
    encode_base(
        pass,
        destination,
        Picture::Planes(planes, resources.1),
        geometry,
        tiles,
        (resources.0, resources.2),
        None,
    )
}

/// Decode a software publication and compose video in the same tile draw.
pub fn encode_software(
    pass: &mut RenderPass<'_>,
    destination: &Image,
    software: &ingest::Source<'_>,
    planes: &[ImageView; 2],
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &Sampler, &mut Ring),
) -> Result<arrayvec::ArrayVec<adv_gpu::Span, 2>> {
    encode_base(
        pass,
        destination,
        Picture::Planes(planes, resources.1),
        geometry,
        tiles,
        (resources.0, resources.2),
        Some(software),
    )
}
/// Compose through the pool's immutable YUV conversion.
pub fn encode_converted(
    pass: &mut RenderPass<'_>,
    destination: &Image,
    image: &PresentImage,
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &mut Ring),
) -> Result<arrayvec::ArrayVec<adv_gpu::Span, 2>> {
    encode_base(
        pass,
        destination,
        Picture::Converted(image),
        geometry,
        tiles,
        resources,
        None,
    )
}

/// Decode a software publication and converted video in one draw.
pub fn encode_converted_software(
    pass: &mut RenderPass<'_>,
    destination: &Image,
    software: &ingest::Source<'_>,
    image: &PresentImage,
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &mut Ring),
) -> Result<arrayvec::ArrayVec<adv_gpu::Span, 2>> {
    encode_base(
        pass,
        destination,
        Picture::Converted(image),
        geometry,
        tiles,
        resources,
        Some(software),
    )
}

enum Picture<'a> {
    Planes(&'a [ImageView; 2], &'a Sampler),
    Converted(&'a PresentImage),
}
fn encode_base(
    pass: &mut RenderPass<'_>,
    destination: &Image,
    picture: Picture<'_>,
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &mut Ring),
    software: Option<&ingest::Source<'_>>,
) -> Result<arrayvec::ArrayVec<adv_gpu::Span, 2>> {
    if tiles.rects.is_empty() {
        return Ok(Default::default());
    }
    if let Some(software) = software {
        debug_assert_eq!(
            software.extent,
            (
                destination.desc().size.width,
                destination.desc().size.height
            )
        );
    }
    let (pipeline, ring) = resources;
    let size = destination.desc().size;
    let d = geometry.destination;
    let (bg, color) = geometry.background.unwrap_or((Rect::default(), [0; 4]));
    let full = matches!(geometry.color, YuvColor::Bt601Full | YuvColor::Bt709Full);
    let (kr, kb) = match geometry.color {
        YuvColor::Bt601Limited | YuvColor::Bt601Full => (0.299, 0.114),
        YuvColor::Bt709Limited | YuvColor::Bt709Full => (0.2126, 0.0722),
    };
    let kg = 1.0 - kr - kb;
    let values: [f32; 36] = [
        size.width as f32,
        size.height as f32,
        0.0,
        0.0,
        d.x as f32,
        d.y as f32,
        d.w as f32,
        d.h as f32,
        geometry.source_uv[0],
        geometry.source_uv[1],
        geometry.source_uv[2],
        geometry.source_uv[3],
        bg.x as f32,
        bg.y as f32,
        bg.w as f32,
        bg.h as f32,
        color[2] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[0] as f32 / 255.0,
        1.0,
        if full { 1.0 } else { 255.0 / 219.0 },
        if full { 0.0 } else { 16.0 / 255.0 },
        if full { 1.0 } else { 255.0 / 224.0 },
        128.0 / 255.0,
        1.0,
        0.0,
        2.0 * (1.0 - kr),
        0.0,
        1.0,
        -2.0 * kb * (1.0 - kb) / kg,
        -2.0 * kr * (1.0 - kr) / kg,
        0.0,
        1.0,
        2.0 * (1.0 - kb),
        0.0,
        0.0,
    ];
    // Uniform offsets on both backends are at most 256-byte aligned.
    let mut constants = ring.reserve(144, 256)?;
    for (out, value) in constants
        .bytes()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(values)
    {
        out.copy_from_slice(&value.to_ne_bytes());
    }
    let rectangles = if let Some(software) = software {
        ingest::source(ring, software, tiles)?
    } else {
        let mut rectangles = ring.reserve(tiles.rects.len() as u64 * 16, 256)?;
        for (out, r) in rectangles
            .bytes()
            .as_chunks_mut::<16>()
            .0
            .iter_mut()
            .zip(&tiles.rects)
        {
            for (out, value) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip([r.x, r.y, r.w, r.h])
            {
                out.copy_from_slice(&(value as f32).to_ne_bytes());
            }
        }
        rectangles
    };
    pass.set_pipeline(pipeline);
    pass.bind(
        0,
        Bind::Uniform(BufferSlice {
            buffer: constants.buffer(),
            offset: constants.offset(),
            size: constants.len(),
        }),
    );
    if let Some(software) = software {
        ingest::bind(pass, software, &rectangles);
    } else {
        pass.bind(
            2,
            Bind::StorageBuffer(BufferSlice {
                buffer: rectangles.buffer(),
                offset: rectangles.offset(),
                size: rectangles.len(),
            }),
        );
    }
    match picture {
        Picture::Planes(planes, sampler) => {
            pass.bind(0, Bind::Sampled(&planes[0]));
            pass.bind(1, Bind::Sampled(&planes[1]));
            pass.bind(0, Bind::Sampler(sampler));
        }
        Picture::Converted(image) => pass.bind_video(image),
    }
    pass.set_viewport(0.0, 0.0, size.width as f32, size.height as f32, 0.0, 1.0);
    pass.draw(0..6, 0..tiles.rects.len() as u32);
    Ok([constants, rectangles].into_iter().collect())
}
