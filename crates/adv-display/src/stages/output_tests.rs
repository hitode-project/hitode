use super::output::*;
use crate::{
    Rect,
    plan::{Rotation, Scale, Tiles, Transform},
};
use adv_gpu::{
    encode::{BufferCopy, ImageCopy},
    resource::*,
    *,
};
use std::time::Duration;

fn draw(
    colors: &[[u8; 4]],
    size: (u32, u32),
    out: (u32, u32),
    scale: Scale,
    rotation: Rotation,
    software: bool,
    damage: Option<Rect>,
) -> Vec<u8> {
    let (device, mut queue) = Device::open(&queue::DeviceOptions {
        software: !cfg!(target_vendor = "apple"),
        validation: true,
        ..Default::default()
    })
    .unwrap();
    let upload = device
        .create_buffer(&BufferDesc {
            size: colors.len() as u64 * 4,
            usage: BufferUsage::COPY_SRC | BufferUsage::STORAGE,
            memory: Memory::Upload,
            label: "output source",
        })
        .unwrap();
    unsafe { upload.mapped().unwrap().as_mut() }.copy_from_slice(colors.as_flattened());
    let source = device
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: size.0,
                height: size.1,
                depth_or_layers: 1,
            },
            format: Format::Bgra8Unorm,
            mips: 1,
            samples: 1,
            usage: Usage::SAMPLED | Usage::COPY_DST,
            view_formats: &[],
            label: "output source",
        })
        .unwrap();
    let target = device
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: out.0,
                height: out.1,
                depth_or_layers: 1,
            },
            format: Format::Bgra8Unorm,
            mips: 1,
            samples: 1,
            usage: Usage::COLOR | Usage::COPY_SRC,
            view_formats: &[],
            label: "output target",
        })
        .unwrap();
    let readback = device
        .create_buffer(&BufferDesc {
            size: u64::from(out.0 * out.1 * 4),
            usage: BufferUsage::COPY_DST,
            memory: Memory::Readback,
            label: "output readback",
        })
        .unwrap();
    let pipeline = pipeline(&device, Format::Bgra8Unorm, software).unwrap();
    let mut ring = device.ring(65536).unwrap();
    let mut encoder = queue.encoder("output pixels");
    encoder.copy_buffer_to_image(
        BufferCopy {
            buffer: &upload,
            offset: 0,
            row_pitch: size.0 * 4,
            rows: size.1,
        },
        ImageCopy {
            image: &source,
            mip: 0,
            layer: 0,
            origin: (0, 0, 0),
        },
        source.desc().size,
    );
    let source = if software {
        Source::Software(super::ingest::Source {
            buffer: &upload,
            pitch: size.0 * 4,
            extent: size,
            format: super::ingest::PixelFormat::Bgra32,
            palette: None,
        })
    } else {
        Source::Image(source.view())
    };
    let rect = Rect {
        x: 0,
        y: 0,
        w: out.0,
        h: out.1,
    };
    if damage.is_some() {
        use adv_gpu::encode::{ColorAttachment, Load, RenderPassDesc, Store};
        let _pass = encoder.render_pass(&RenderPassDesc {
            colors: [ColorAttachment {
                view: target.view(),
                load: Load::Clear([0., 0., 0., 1.]),
                store: Store::Store,
                resolve: None,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            area: rect.into(),
            timestamps: None,
            label: "preserved pixels",
        });
    }
    encode(
        &mut encoder,
        source,
        target.view(),
        Transform {
            rotation,
            dst: rect,
        },
        scale,
        &Tiles {
            size: (1, 1),
            rects: vec![damage.unwrap_or(rect)],
        },
        (&pipeline, &mut ring),
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
            row_pitch: out.0 * 4,
            rows: out.1,
        },
        target.desc().size,
    );
    let token = queue.submit(encoder).unwrap();
    assert!(queue.wait(token, Duration::from_secs(20)).unwrap());
    unsafe { readback.mapped().unwrap().as_ref() }.to_vec()
}
// spec: HOST-VIEW-1, HOST-VIEW-3, HOST-VIEW-9
#[test]
fn sharp_bilinear_preserves_integer_pixels_and_blends_fractional_boundaries() {
    let colors = [
        [0, 0, 0, 0],
        [255, 255, 255, 127],
        [255, 255, 255, 1],
        [0, 0, 0, 255],
    ];
    for software in [false, true] {
        for ramp in [
            &[128u8][..],
            &[0, 255],
            &[0, 128, 255],
            &[0, 0, 255, 255],
            &[0, 0, 128, 255, 255],
            &[0, 0, 0, 255, 255, 255],
        ] {
            let n = ramp.len() as u32;
            let bytes = draw(
                &colors,
                (2, 2),
                (n, n),
                Scale::SharpBilinear {
                    prescale: ((n / 2).max(1), (n / 2).max(1)),
                },
                Rotation::None,
                software,
                None,
            );
            for y in 0..n {
                for x in 0..n {
                    let a = f32::from(ramp[x as usize]) / 255.;
                    let b = f32::from(ramp[y as usize]) / 255.;
                    let expected = 255. * (a * (1. - b) + b * (1. - a));
                    let pixel = &bytes[((y * n + x) * 4) as usize..][..4];
                    assert!(
                        pixel[..3]
                            .iter()
                            .all(|v| (f32::from(*v) - expected).abs() <= 1.5),
                        "{software}, {n}, ({x},{y}): {pixel:?}, {expected}"
                    );
                    assert_eq!(pixel[3], 255);
                }
            }
        }
    }
}
// spec: HOST-VIEW-1, HOST-VIEW-3, HOST-VIEW-9
#[test]
fn integer_scales_preserve_encoded_colors_and_force_opaque_alpha() {
    let colors = [
        [32, 64, 192, 0],
        [200, 100, 20, 255],
        [48, 160, 96, 128],
        [240, 16, 224, 7],
    ];
    for software in [false, true] {
        for factor in [1, 2, 3] {
            let n = 2 * factor;
            let bytes = draw(
                &colors,
                (2, 2),
                (n, n),
                Scale::Integer {
                    x: factor,
                    y: factor,
                },
                Rotation::None,
                software,
                None,
            );
            for y in 0..n {
                for x in 0..n {
                    let mut expected = colors[((y / factor) * 2 + x / factor) as usize];
                    expected[3] = 255;
                    assert_eq!(&bytes[((y * n + x) * 4) as usize..][..4], expected);
                }
            }
        }
    }
}
// spec: HOST-VIEW-1
#[test]
fn output_rotates_the_source_in_one_draw() {
    let colors = [
        [1, 2, 3, 0],
        [4, 5, 6, 0],
        [7, 8, 9, 0],
        [10, 11, 12, 0],
        [13, 14, 15, 0],
        [16, 17, 18, 0],
    ];
    for (rotation, out, order) in [
        (Rotation::None, (3, 2), [0, 1, 2, 3, 4, 5]),
        (Rotation::Quarter, (2, 3), [3, 0, 4, 1, 5, 2]),
        (Rotation::Half, (3, 2), [5, 4, 3, 2, 1, 0]),
        (Rotation::ThreeQuarters, (2, 3), [2, 5, 1, 4, 0, 3]),
    ] {
        let bytes = draw(
            &colors,
            (3, 2),
            out,
            Scale::Integer { x: 1, y: 1 },
            rotation,
            false,
            None,
        );
        for (pixel, index) in bytes.as_chunks::<4>().0.iter().zip(order) {
            let mut expected = colors[index];
            expected[3] = 255;
            assert_eq!(*pixel, expected);
        }
    }
}

// spec: HOST-VIEW-1, HOST-VIEW-3
#[test]
fn output_preserves_pixels_outside_its_tiles() {
    for software in [false, true] {
        let bytes = draw(
            &[[17, 34, 51, 0]],
            (1, 1),
            (48, 32),
            Scale::Integer { x: 48, y: 32 },
            Rotation::None,
            software,
            Some(Rect {
                x: 16,
                y: 0,
                w: 16,
                h: 16,
            }),
        );
        for y in 0..32 {
            for x in 0..48 {
                let expected = if (16..32).contains(&x) && y < 16 {
                    [17, 34, 51, 255]
                } else {
                    [0, 0, 0, 255]
                };
                assert_eq!(&bytes[((y * 48 + x) * 4) as usize..][..4], expected);
            }
        }
    }
}
