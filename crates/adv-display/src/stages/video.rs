//! One instanced draw per video: damage tiles sample YUV or paint the border.
//! Software bases decode inline. Metal samples planes; Android samples the
//! pool's converted RGB. Plane samples interpolate in float before the color matrix.

use super::{bindings, ingest};
use crate::{Rect, plan::Tiles};
use adv_gpu::{
    Device, Encoder, Format, Image, ImageView, PresentImage, PresentPool, RenderPipeline, Result,
    Ring, Sampler,
    encode::{Bind, ColorAttachment, Load, RenderPassDesc, Store},
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
            }
        } else {
            Source::Precompiled {
                spirv: &include!(concat!(env!("OUT_DIR"), "/video.spv.rs")),
                metal: include_bytes!(concat!(env!("OUT_DIR"), "/video.msl")),
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
    encoder: &mut Encoder,
    destination: &Image,
    planes: &[ImageView; 2],
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &Sampler, &mut Ring),
) -> Result<()> {
    encode_base(
        encoder,
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
    encoder: &mut Encoder,
    destination: &Image,
    software: &ingest::Source<'_>,
    planes: &[ImageView; 2],
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &Sampler, &mut Ring),
) -> Result<()> {
    encode_base(
        encoder,
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
    encoder: &mut Encoder,
    destination: &Image,
    image: &PresentImage,
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &mut Ring),
) -> Result<()> {
    encode_base(
        encoder,
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
    encoder: &mut Encoder,
    destination: &Image,
    software: &ingest::Source<'_>,
    image: &PresentImage,
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &mut Ring),
) -> Result<()> {
    encode_base(
        encoder,
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
    encoder: &mut Encoder,
    destination: &Image,
    picture: Picture<'_>,
    geometry: Geometry,
    tiles: &Tiles,
    resources: (&RenderPipeline, &mut Ring),
    software: Option<&ingest::Source<'_>>,
) -> Result<()> {
    if tiles.rects.is_empty() {
        return Ok(());
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
    let (area, coverage_load) = ingest::coverage(tiles);
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
    {
        let mut pass = encoder.render_pass(&RenderPassDesc {
            colors: [ColorAttachment {
                view: destination.view(),
                load: if software.is_some() {
                    coverage_load
                } else {
                    Load::Load
                },
                store: Store::Store,
                resolve: None,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            area,
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
        if let Some(software) = software {
            ingest::bind(&mut pass, software, &rectangles);
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
    }
    encoder.retain(constants);
    encoder.retain(rectangles);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_gpu::{
        Extent, Rect as GpuRect, Token,
        encode::{BufferCopy, ImageCopy},
        queue::DeviceOptions,
        resource::{BufferDesc, BufferUsage, Dimension, ImageDesc, Memory, Usage},
    };
    #[cfg(target_vendor = "apple")]
    use adv_gpu::{present::PoolDesc, resource::ViewDesc};
    use std::time::Duration;

    #[test]
    #[cfg_attr(
        target_os = "android",
        ignore = "Android samples converted external-format pools, which tests/video.rs covers"
    )]
    // spec: HOST-VIEW-3
    fn video_draw_matches_cpu_reference() {
        let (device, mut queue) = Device::open(&DeviceOptions {
            software: !cfg!(target_vendor = "apple"),
            validation: true,
            ..Default::default()
        })
        .unwrap();
        let (pipeline, sampler) = resources(&device, Format::Rgba8Unorm).unwrap();
        let (software_pipeline, _) = software_resources(&device, Format::Rgba8Unorm).unwrap();
        let mut ring = device.ring(65536).unwrap();
        let source_buffer = device
            .create_buffer(&BufferDesc {
                size: 140,
                usage: BufferUsage::STORAGE,
                memory: Memory::Upload,
                label: "software base",
            })
            .unwrap();
        unsafe { source_buffer.mapped().unwrap().as_mut() }.fill(0x99);
        let mut palette = [[0; 4]; 256];
        palette[9] = [5, 15, 25, 255];
        let software_source = ingest::Source {
            buffer: &source_buffer,
            pitch: 10,
            extent: (20, 14),
            format: ingest::PixelFormat::Indexed4,
            palette: Some(&palette),
        };
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
            #[cfg(target_vendor = "apple")]
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
            #[cfg(target_vendor = "apple")]
            let mut image = pool.acquire_cpu().unwrap();
            #[cfg(target_vendor = "apple")]
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
            #[cfg(target_vendor = "apple")]
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
            #[cfg(not(target_vendor = "apple"))]
            let planes = {
                let images = std::array::from_fn::<_, 2, _>(|plane| {
                    device
                        .create_image(&ImageDesc {
                            dimension: Dimension::D2,
                            size: Extent {
                                width: [8, 4][plane],
                                height: [6, 3][plane],
                                depth_or_layers: 1,
                            },
                            format: [Format::R8Unorm, Format::Rg8Unorm][plane],
                            mips: 1,
                            samples: 1,
                            usage: Usage::SAMPLED | Usage::COPY_DST,
                            view_formats: &[],
                            label: "video plane",
                        })
                        .unwrap()
                });
                let upload = device
                    .create_buffer(&BufferDesc {
                        size: 72,
                        usage: BufferUsage::COPY_SRC,
                        memory: Memory::Upload,
                        label: "video plane bytes",
                    })
                    .unwrap();
                let bytes = unsafe { upload.mapped().unwrap().as_mut() };
                bytes[..48].copy_from_slice(&y);
                bytes[48..].copy_from_slice(&uv);
                let mut encoder = queue.encoder("video plane upload");
                for (plane, image) in images.iter().enumerate() {
                    encoder.copy_buffer_to_image(
                        BufferCopy {
                            buffer: &upload,
                            offset: [0, 48][plane],
                            row_pitch: 8,
                            rows: [6, 3][plane],
                        },
                        ImageCopy {
                            image,
                            mip: 0,
                            layer: 0,
                            origin: (0, 0, 0),
                        },
                        image.desc().size,
                    );
                }
                let token = queue.submit(encoder).unwrap();
                assert!(queue.wait(token, Duration::from_secs(20)).unwrap());
                images.map(|image| image.view().clone())
            };
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
            for software in [false, true] {
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
                if software {
                    encode_software(
                        &mut encoder,
                        &target,
                        &software_source,
                        &planes,
                        geometry,
                        &tiles,
                        (&software_pipeline, &sampler, &mut ring),
                    )
                    .unwrap();
                } else {
                    encode(
                        &mut encoder,
                        &target,
                        &planes,
                        geometry,
                        &tiles,
                        (&pipeline, &sampler, &mut ring),
                    )
                    .unwrap();
                }
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
                            if software {
                                expected = [25, 15, 5, 255];
                            }
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
                                "{color:?}, software={software} ({px},{py}): {:?} != {expected:?}",
                                &actual[at..at + 4]
                            );
                        }
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
