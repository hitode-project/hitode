use adv_display::{
    Rect,
    plan::Tiles,
    stages::{ingest, video::*},
};
#[cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_gpu::present::PoolDesc;
use adv_gpu::{
    Device, Format,
    encode::{ColorAttachment, Load, RenderPassDesc, Store},
    present::YuvColor,
    resource::Dimension,
};
use adv_gpu::{
    Extent, Rect as GpuRect, Token,
    encode::{BufferCopy, ImageCopy},
    queue::DeviceOptions,
    resource::{BufferDesc, BufferUsage, ImageDesc, Memory, Usage},
};
#[cfg(target_vendor = "apple")]
use adv_gpu::{ImageView, resource::ViewDesc};
use std::time::Duration;

#[test]
// spec: HOST-VIEW-3
fn video_draw_matches_cpu_reference() {
    let (device, mut queue) = Device::open(&DeviceOptions {
        software: !cfg!(any(target_vendor = "apple", target_os = "android")),
        validation: true,
        ..Default::default()
    })
    .unwrap();
    #[cfg(not(target_os = "android"))]
    let (pipeline, sampler) = resources(&device, Format::Rgba8Unorm).unwrap();
    #[cfg(not(target_os = "android"))]
    let (software_pipeline, _) = software_resources(&device, Format::Rgba8Unorm).unwrap();
    let software_buffer = device
        .create_buffer(&BufferDesc {
            size: 20 * 14 * 4,
            usage: BufferUsage::STORAGE,
            memory: Memory::Upload,
            label: "software video base",
        })
        .unwrap();
    for pixel in unsafe { software_buffer.mapped().unwrap().as_mut() }
        .as_chunks_mut::<4>()
        .0
    {
        *pixel = [147, 23, 94, 255];
    }
    let software_source = ingest::Source {
        buffer: &software_buffer,
        pitch: 80,
        extent: (20, 14),
        format: ingest::PixelFormat::Bgra32,
        palette: None,
    };
    let indexed_buffer = device
        .create_buffer(&BufferDesc {
            size: 140,
            usage: BufferUsage::STORAGE,
            memory: Memory::Upload,
            label: "indexed video base",
        })
        .unwrap();
    unsafe { indexed_buffer.mapped().unwrap().as_mut() }.fill(0x99);
    let mut palette = [[0; 4]; 256];
    palette[9] = [5, 15, 25, 255];
    let indexed_source = ingest::Source {
        buffer: &indexed_buffer,
        pitch: 10,
        extent: (20, 14),
        format: ingest::PixelFormat::Indexed4,
        palette: Some(&palette),
    };
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
    #[cfg(not(target_os = "android"))]
    let y: Vec<u8> = (0..8 * 6).map(|i| (24 + i * 4) as u8).collect();
    #[cfg(not(target_os = "android"))]
    let uv: Vec<u8> = (0..4 * 3)
        .flat_map(|i| [(71 + i * 6) as u8, (171 - i * 4) as u8])
        .collect();
    #[cfg(target_os = "android")]
    let (y, uv) = (vec![80; 8 * 6], [109, 175].repeat(4 * 3));
    #[cfg(any(target_vendor = "apple", target_os = "android"))]
    let format = [Format::Nv12, Format::Nv21]
        .into_iter()
        .find(|&f| {
            device
                .caps()
                .format(f)
                .contains(adv_gpu::FormatCaps::PRESENTABLE)
        })
        .expect("required presentable YUV format");
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
        #[cfg(any(target_vendor = "apple", target_os = "android"))]
        let mut pool = device
            .create_present_pool(&PoolDesc {
                width: 8,
                height: 6,
                format,
                color: Some(color),
                count: 1,
                label: "video planes",
            })
            .unwrap();
        #[cfg(target_vendor = "apple")]
        assert!(matches!(
            converted_resources(&device, Format::Rgba8Unorm, &pool),
            Err(adv_gpu::Error::Unsupported(_))
        ));
        #[cfg(target_os = "android")]
        let pipeline = converted_resources(&device, Format::Rgba8Unorm, &pool).unwrap();
        #[cfg(target_os = "android")]
        let software_pipeline =
            converted_software_resources(&device, Format::Rgba8Unorm, &pool).unwrap();
        #[cfg(any(target_vendor = "apple", target_os = "android"))]
        let mut image = pool.acquire_cpu().unwrap();
        #[cfg(any(target_vendor = "apple", target_os = "android"))]
        image.write(|planes| {
            for row in 0..6 {
                let start = row * planes[0].stride;
                planes[0].data[start..start + 8].copy_from_slice(&y[row * 8..][..8]);
            }
            for row in 0..3 {
                let start = row * planes[1].stride;
                for (dst, src) in planes[1].data[start..start + 8]
                    .as_chunks_mut::<2>()
                    .0
                    .iter_mut()
                    .zip(uv[row * 8..][..8].as_chunks::<2>().0)
                {
                    *dst = if format == Format::Nv21 {
                        [src[1], src[0]]
                    } else {
                        *src
                    };
                }
            }
        });
        #[cfg(target_vendor = "apple")]
        let planes: [ImageView; 2] = std::array::from_fn(|plane| {
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
        #[cfg(not(any(target_vendor = "apple", target_os = "android")))]
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
        for base in 0..3 {
            let software = base != 0;
            let software_source = if base == 2 {
                &indexed_source
            } else {
                &software_source
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
            if software {
                #[cfg(not(target_os = "android"))]
                encode_software(
                    &mut encoder,
                    &target,
                    software_source,
                    &planes,
                    geometry,
                    &tiles,
                    (&software_pipeline, &sampler, &mut ring),
                )
                .unwrap();
                #[cfg(target_os = "android")]
                encode_converted_software(
                    &mut encoder,
                    &target,
                    software_source,
                    &image,
                    geometry,
                    &tiles,
                    (&software_pipeline, &mut ring),
                )
                .unwrap();
            } else {
                #[cfg(not(target_os = "android"))]
                encode(
                    &mut encoder,
                    &target,
                    &planes,
                    geometry,
                    &tiles,
                    (&pipeline, &sampler, &mut ring),
                )
                .unwrap();
                #[cfg(target_os = "android")]
                encode_converted(
                    &mut encoder,
                    &target,
                    &image,
                    geometry,
                    &tiles,
                    (&pipeline, &mut ring),
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
                            expected = if base == 2 {
                                [25, 15, 5, 255]
                            } else {
                                [94, 23, 147, 255]
                            };
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
                            "{color:?} ({px},{py}): {:?} != {expected:?}",
                            &actual[at..at + 4]
                        );
                    }
                }
            }
        }
    }
}
fn sample(data: &[u8], size: (i32, i32), channels: usize, channel: usize, u: f32, v: f32) -> f32 {
    let (x, y) = (u * size.0 as f32 - 0.5, v * size.1 as f32 - 0.5);
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let texel = |x: i32, y: i32| {
        f32::from(
            data[(y.clamp(0, size.1 - 1) * size.0 + x.clamp(0, size.0 - 1)) as usize * channels
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
