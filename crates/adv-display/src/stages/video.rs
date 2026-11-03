//! One instanced draw per video: damage tiles sample YUV or paint the border.

use crate::{Rect, plan::Tiles};
use adv_gpu::{
    Device, Encoder, Format, Image, ImageView, PresentImage, PresentPool, RenderPipeline, Result,
    Ring, Sampler,
    encode::{Bind, ColorAttachment, Load, RenderPassDesc, Store},
    pipeline::VideoPipelineDesc,
    pipeline::{
        Binding, BindingKind, ColorTarget, Cull, Primitive, RenderPipelineDesc, Stages, Topology,
    },
    present::YuvColor,
    resource::{Address, BorderColor, BufferSlice, Dimension, Filter, SamplerDesc},
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
    let pipeline = pipeline(device, format, None)?;
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

/// Build once when the converted-video pool is created, never during a frame.
pub fn converted_resources(
    device: &Device,
    format: Format,
    pool: &PresentPool,
) -> Result<RenderPipeline> {
    pipeline(device, format, Some(pool))
}

fn pipeline(device: &Device, format: Format, pool: Option<&PresentPool>) -> Result<RenderPipeline> {
    let source = if pool.is_some() && cfg!(target_os = "android") {
        SHADER.replace(PLANE_BINDINGS, CONVERTED_BINDINGS).replace(
            PLANE_SAMPLE,
            "return textureSampleLevel(video, filtering, uv, 0.);",
        )
    } else {
        SHADER.into()
    };
    let shader = device.create_shader(ShaderDesc {
        source: Source::Wgsl(&source),
        identity: source.bytes().fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        }),
        label: "video",
    })?;
    let mut desc = RenderPipelineDesc {
        vertex: shader.entry("vs", Stage::Vertex)?,
        fragment: Some(shader.entry("fs", Stage::Fragment)?),
        bindings: vec![
            Binding {
                slot: 0,
                kind: BindingKind::UniformBuffer,
                stages: Stages::VERTEX | Stages::FRAGMENT,
            },
            Binding {
                slot: 0,
                kind: BindingKind::StorageBuffer { writable: false },
                stages: Stages::VERTEX,
            },
            Binding {
                slot: 0,
                kind: BindingKind::SampledImage {
                    dimension: Dimension::D2,
                    depth: false,
                    multisampled: false,
                },
                stages: Stages::FRAGMENT,
            },
            Binding {
                slot: 1,
                kind: BindingKind::SampledImage {
                    dimension: Dimension::D2,
                    depth: false,
                    multisampled: false,
                },
                stages: Stages::FRAGMENT,
            },
            Binding {
                slot: 0,
                kind: BindingKind::Sampler { comparison: false },
                stages: Stages::FRAGMENT,
            },
        ],
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
    encoder: &mut Encoder,
    destination: &Image,
    planes: &[ImageView; 2],
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &Sampler, &mut Ring),
) -> Result<()> {
    encode_picture(
        encoder,
        destination,
        Picture::Planes(planes, resources.1),
        geometry,
        tiles,
        (resources.0, resources.2),
    )
}

/// Compose a CPU-written Android buffer through its pool's immutable conversion.
pub fn encode_converted(
    encoder: &mut Encoder,
    destination: &Image,
    image: &PresentImage,
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &mut Ring),
) -> Result<()> {
    encode_picture(
        encoder,
        destination,
        Picture::Converted(image),
        geometry,
        tiles,
        resources,
    )
}

enum Picture<'a> {
    Planes(&'a [ImageView; 2], &'a Sampler),
    Converted(&'a PresentImage),
}
fn encode_picture(
    encoder: &mut Encoder,
    destination: &Image,
    picture: Picture<'_>,
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &mut Ring),
) -> Result<()> {
    let Some(first) = tiles.rects.first() else {
        return Ok(());
    };
    let area = tiles.rects.iter().skip(1).fold(*first, |a, b| {
        let (x, y) = (a.x.min(b.x), a.y.min(b.y));
        Rect {
            x,
            y,
            w: (a.x + a.w).max(b.x + b.w) - x,
            h: (a.y + a.h).max(b.y + b.h) - y,
        }
    });
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
    {
        let mut pass = encoder.render_pass(&RenderPassDesc {
            colors: [ColorAttachment {
                view: destination.view(),
                load: Load::Load,
                store: Store::Store,
                resolve: None,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            area: area.into(),
            timestamps: None,
            label: "video",
        });
        pass.set_pipeline(pipeline);
        pass.bind(
            0,
            Bind::Uniform(BufferSlice {
                buffer: constants.buffer(),
                offset: constants.offset(),
                size: constants.len(),
            }),
        );
        pass.bind(
            0,
            Bind::StorageBuffer(BufferSlice {
                buffer: rectangles.buffer(),
                offset: rectangles.offset(),
                size: rectangles.len(),
            }),
        );
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
    }
    encoder.retain(constants);
    encoder.retain(rectangles);
    Ok(())
}

const PLANE_BINDINGS: &str = r#"@group(1) @binding(0) var luma: texture_2d<f32>;
@group(1) @binding(1) var chroma: texture_2d<f32>;
@group(2) @binding(0) var filtering: sampler;"#;
const CONVERTED_BINDINGS: &str = r#"@group(4) @binding(0) var video: texture_2d<f32>;
@group(4) @binding(1) var filtering: sampler;"#;
const PLANE_SAMPLE: &str = r#"let y = (textureSampleLevel(luma, filtering, uv, 0.).r - p.range.y) * p.range.x;
        let cbcr = (textureSampleLevel(chroma, filtering, uv, 0.).rg - vec2(p.range.w)) * p.range.z;
        let yuv = vec3(y, cbcr);
        return vec4(dot(p.r.xyz, yuv), dot(p.g.xyz, yuv), dot(p.b.xyz, yuv), 1.);"#;

const SHADER: &str = r#"
struct Params {
    canvas: vec4<f32>, dst: vec4<f32>, uv: vec4<f32>, bg: vec4<f32>,
    color: vec4<f32>, range: vec4<f32>, r: vec4<f32>, g: vec4<f32>, b: vec4<f32>,
}
@group(0) @binding(0) var<uniform> p: Params;
@group(3) @binding(0) var<storage, read> tiles: array<vec4<f32>>;
@group(1) @binding(0) var luma: texture_2d<f32>;
@group(1) @binding(1) var chroma: texture_2d<f32>;
@group(2) @binding(0) var filtering: sampler;
@vertex fn vs(@builtin(vertex_index) vertex: u32, @builtin(instance_index) tile: u32) -> @builtin(position) vec4<f32> {
    let corners = array<vec2<f32>, 6>(vec2(0., 0.), vec2(1., 0.), vec2(0., 1.), vec2(0., 1.), vec2(1., 0.), vec2(1., 1.));
    let pos = tiles[tile].xy + corners[vertex] * tiles[tile].zw;
    return vec4(pos / p.canvas.xy * vec2(2., -2.) + vec2(-1., 1.), 0., 1.);
}
fn inside(pos: vec2<f32>, r: vec4<f32>) -> bool { return all(pos >= r.xy) && all(pos < r.xy + r.zw); }
@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    if inside(pos.xy, p.dst) {
        let uv = p.uv.xy + (pos.xy - p.dst.xy) / p.dst.zw * p.uv.zw;
        let y = (textureSampleLevel(luma, filtering, uv, 0.).r - p.range.y) * p.range.x;
        let cbcr = (textureSampleLevel(chroma, filtering, uv, 0.).rg - vec2(p.range.w)) * p.range.z;
        let yuv = vec3(y, cbcr);
        return vec4(dot(p.r.xyz, yuv), dot(p.g.xyz, yuv), dot(p.b.xyz, yuv), 1.);
    }
    if inside(pos.xy, p.bg) { return p.color; }
    discard;
}
"#;
