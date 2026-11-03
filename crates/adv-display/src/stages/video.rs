//! One instanced draw per video: damage tiles sample NV12 or paint the border.

use crate::{Rect, plan::Tiles};
use adv_gpu::{
    Device, Encoder, Format, Image, ImageView, RenderPipeline, Result, Ring, Sampler,
    encode::{Bind, ColorAttachment, Load, RenderPassDesc, Store},
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
    let shader = device.create_shader(ShaderDesc {
        source: Source::Wgsl(SHADER),
        identity: SHADER.bytes().fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        }),
        label: "video",
    })?;
    let pipeline = device.create_render_pipeline(
        &RenderPipelineDesc {
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
        },
        None,
    )?;
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
    let (pipeline, sampler, ring) = resources;
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
        pass.bind(0, Bind::Sampled(&planes[0]));
        pass.bind(1, Bind::Sampled(&planes[1]));
        pass.bind(0, Bind::Sampler(sampler));
        pass.set_viewport(0.0, 0.0, size.width as f32, size.height as f32, 0.0, 1.0);
        pass.draw(0..6, 0..tiles.rects.len() as u32);
    }
    encoder.retain(constants);
    encoder.retain(rectangles);
    Ok(())
}

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

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;
    use adv_gpu::{
        Extent, Rect as GpuRect, Token,
        encode::{BufferCopy, ImageCopy},
        present::PoolDesc,
        queue::DeviceOptions,
        resource::{BufferDesc, BufferUsage, ImageDesc, Memory, Usage, ViewDesc},
    };
    use std::time::Duration;

    #[test]
    // spec: HOST-VIEW-3
    fn video_draw_matches_cpu_reference() {
        let (device, mut queue) = Device::open(&DeviceOptions::default()).unwrap();
        let (pipeline, sampler) = resources(&device, Format::Rgba8Unorm).unwrap();
        let mut ring = device.ring(65536).unwrap();
        let size = Extent {
            width: 20,
            height: 14,
            depth_or_layers: 1,
        };
        let target = device
            .create_image(&ImageDesc {
                dimension: Dimension::D2,
                size,
                format: Format::Rgba8Unorm,
                mips: 1,
                samples: 1,
                usage: Usage::COLOR | Usage::COPY_SRC,
                view_formats: &[],
                label: "video test",
            })
            .unwrap();
        let readback = device
            .create_buffer(&BufferDesc {
                size: 20 * 14 * 4,
                usage: BufferUsage::COPY_DST,
                memory: Memory::Readback,
                label: "video readback",
            })
            .unwrap();
        let y: Vec<u8> = (0..8 * 6).map(|i| (24 + i * 4) as u8).collect();
        let uv: Vec<u8> = (0..4 * 3)
            .flat_map(|i| [(71 + i * 6) as u8, (171 - i * 4) as u8])
            .collect();
        let tiles = Tiles {
            size: (1, 1),
            rects: vec![
                Rect {
                    x: 1,
                    y: 1,
                    w: 7,
                    h: 10,
                },
                Rect {
                    x: 12,
                    y: 0,
                    w: 8,
                    h: 12,
                },
            ],
        };
        for color in [
            YuvColor::Bt601Limited,
            YuvColor::Bt601Full,
            YuvColor::Bt709Limited,
            YuvColor::Bt709Full,
        ] {
            let mut pool = device
                .create_present_pool(&PoolDesc {
                    width: 8,
                    height: 6,
                    format: Format::Nv12,
                    color: Some(color),
                    count: 1,
                    label: "video planes",
                })
                .unwrap();
            let mut image = pool.acquire_cpu().unwrap();
            image.write(|planes| {
                for row in 0..6 {
                    let start = row * planes[0].stride;
                    planes[0].data[start..start + 8].copy_from_slice(&y[row * 8..][..8]);
                }
                for row in 0..3 {
                    let start = row * planes[1].stride;
                    planes[1].data[start..start + 8].copy_from_slice(&uv[row * 8..][..8]);
                }
            });
            let planes = std::array::from_fn(|plane| {
                image
                    .image()
                    .create_view(&ViewDesc {
                        format: Some([Format::R8Unorm, Format::Rg8Unorm][plane]),
                        dimension: None,
                        mips: 0..1,
                        layers: 0..1,
                        plane: plane as u32,
                    })
                    .unwrap()
            });
            let geometry = Geometry {
                destination: Rect {
                    x: 3,
                    y: 2,
                    w: 13,
                    h: 10,
                },
                source_uv: [0.125, 1.0 / 6.0, 0.75, 4.0 / 6.0],
                background: Some((
                    Rect {
                        x: 2,
                        y: 1,
                        w: 16,
                        h: 12,
                    },
                    [30, 60, 90, 255],
                )),
                color,
            };
            let mut encoder = queue.encoder("video test");
            drop(
                encoder.render_pass(&RenderPassDesc {
                    colors: [ColorAttachment {
                        view: target.view(),
                        load: Load::Clear([0.0, 0.0, 0.0, 1.0]),
                        store: Store::Store,
                        resolve: None,
                    }]
                    .into_iter()
                    .collect(),
                    depth_stencil: None,
                    area: GpuRect {
                        x: 0,
                        y: 0,
                        w: 20,
                        h: 14,
                    },
                    timestamps: None,
                    label: "base",
                }),
            );
            encode(
                &mut encoder,
                &target,
                &planes,
                geometry,
                &tiles,
                (&pipeline, &sampler, &mut ring),
            )
            .unwrap();
            encoder.copy_image_to_buffer(
                ImageCopy {
                    image: &target,
                    mip: 0,
                    layer: 0,
                    origin: (0, 0, 0),
                },
                BufferCopy {
                    buffer: &readback,
                    offset: 0,
                    row_pitch: 80,
                    rows: 14,
                },
                size,
            );
            let token: Token = queue.submit(encoder).unwrap();
            assert!(queue.wait(token, Duration::from_secs(20)).unwrap());
            let actual = unsafe { readback.mapped().unwrap().as_ref() };
            for py in 0..14 {
                for px in 0..20 {
                    let inside =
                        |r: Rect| px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h;
                    let mut expected = [0, 0, 0, 255];
                    if tiles.rects.iter().copied().any(inside) {
                        if inside(geometry.destination) {
                            let d = geometry.destination;
                            let sx = geometry.source_uv[0]
                                + (px as f32 + 0.5 - d.x as f32) / d.w as f32
                                    * geometry.source_uv[2];
                            let sy = geometry.source_uv[1]
                                + (py as f32 + 0.5 - d.y as f32) / d.h as f32
                                    * geometry.source_uv[3];
                            let luma = sample(&y, (8, 6), 1, 0, sx, sy);
                            let u = sample(&uv, (4, 3), 2, 0, sx, sy);
                            let v = sample(&uv, (4, 3), 2, 1, sx, sy);
                            expected = rgb(luma, u, v, color);
                        } else if inside(geometry.background.unwrap().0) {
                            expected = [90, 60, 30, 255];
                        }
                    }
                    let at = (py * 20 + px) as usize * 4;
                    for channel in 0..4 {
                        assert!(
                            actual[at + channel].abs_diff(expected[channel]) <= 2,
                            "{color:?} ({px},{py}): {:?} != {expected:?}",
                            &actual[at..at + 4]
                        );
                    }
                }
            }
        }
    }
    fn sample(
        data: &[u8],
        size: (i32, i32),
        channels: usize,
        channel: usize,
        u: f32,
        v: f32,
    ) -> f32 {
        let (x, y) = (u * size.0 as f32 - 0.5, v * size.1 as f32 - 0.5);
        let (x0, y0) = (x.floor() as i32, y.floor() as i32);
        let texel = |x: i32, y: i32| {
            f32::from(
                data[(y.clamp(0, size.1 - 1) * size.0 + x.clamp(0, size.0 - 1)) as usize
                    * channels
                    + channel],
            ) / 255.0
        };
        let (fx, fy) = (x - x.floor(), y - y.floor());
        let a = texel(x0, y0) * (1.0 - fx) + texel(x0 + 1, y0) * fx;
        let b = texel(x0, y0 + 1) * (1.0 - fx) + texel(x0 + 1, y0 + 1) * fx;
        a * (1.0 - fy) + b * fy
    }
    fn rgb(y: f32, u: f32, v: f32, color: YuvColor) -> [u8; 4] {
        let full = matches!(color, YuvColor::Bt601Full | YuvColor::Bt709Full);
        let (kr, kb) = if matches!(color, YuvColor::Bt709Limited | YuvColor::Bt709Full) {
            (0.2126, 0.0722)
        } else {
            (0.299, 0.114)
        };
        let y = if full {
            y
        } else {
            (y - 16.0 / 255.0) * 255.0 / 219.0
        };
        let scale = if full { 1.0 } else { 255.0 / 224.0 };
        let (u, v) = ((u - 128.0 / 255.0) * scale, (v - 128.0 / 255.0) * scale);
        let r = y + 2.0 * (1.0 - kr) * v;
        let b = y + 2.0 * (1.0 - kb) * u;
        let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
        let [r, g, b] = [r, g, b].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
        [r, g, b, 255]
    }
}
