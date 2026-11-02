use super::*;
use std::time::Duration;

fn gpu(disable: Modern) -> (crate::Device, crate::Queue) {
    crate::Device::open(&DeviceOptions {
        software: !cfg!(target_os = "android"),
        validation: true,
        disable,
        ..DeviceOptions::default()
    })
    .unwrap()
}
fn pass<'a>(
    view: &'a crate::ImageView,
    area: Rect,
    load: Load<[f64; 4]>,
    store: Store,
    resolve: Option<&'a crate::ImageView>,
) -> RenderPassDesc<'a> {
    RenderPassDesc {
        colors: [ColorAttachment {
            view,
            load,
            store,
            resolve,
        }]
        .into_iter()
        .collect(),
        depth_stencil: None,
        area,
        timestamps: None,
        label: "internal pass",
    }
}
fn finish(queue: &mut crate::Queue, encoder: crate::Encoder) {
    let token = queue.submit(encoder).unwrap();
    assert!(queue.wait(token, Duration::from_secs(20)).unwrap());
}
fn create_image(
    device: &crate::Device,
    format: Format,
    size: Extent,
    mips: u32,
    samples: u32,
) -> crate::Image {
    device
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size,
            format,
            mips,
            samples,
            usage: Usage::COLOR
                | Usage::SAMPLED
                | if samples == 1 {
                    Usage::COPY_SRC | Usage::COPY_DST
                } else {
                    Usage::empty()
                },
            // Exercises the mutable image with a format list.
            view_formats: if format == Format::Rgba8Unorm {
                &[Format::Rgba8Srgb]
            } else {
                &[]
            },
            label: "internal image",
        })
        .unwrap()
}
fn readback(device: &crate::Device, size: u64) -> crate::Buffer {
    device
        .create_buffer(&BufferDesc {
            size,
            usage: BufferUsage::COPY_DST,
            memory: Memory::Readback,
            label: "internal results",
        })
        .unwrap()
}
fn read_image(
    encoder: &mut crate::Encoder,
    image: &crate::Image,
    mip: u32,
    layer: u32,
    size: Extent,
    dst: &crate::Buffer,
) {
    encoder.copy_image_to_buffer(
        ImageCopy {
            image,
            mip,
            layer,
            origin: (0, 0, 0),
        },
        BufferCopy {
            buffer: dst,
            offset: 0,
            row_pitch: 0,
            rows: 0,
        },
        size,
    );
}
fn bytes(buffer: &crate::Buffer) -> &[u8] {
    // SAFETY: Every caller waits for the writing submission before reading.
    unsafe { &*buffer.mapped().unwrap().as_ptr() }
}

#[test]
fn views_partial_clears_query_resets_and_resolves() {
    for disable in [Modern::empty(), Modern::all()] {
        let (device, mut queue) = gpu(disable);
        let extent = Extent {
            width: 8,
            height: 8,
            depth_or_layers: 2,
        };
        let image = create_image(&device, Format::Rgba8Unorm, extent, 2, 1);
        let view = image
            .create_view(&ViewDesc {
                format: Some(Format::Rgba8Srgb),
                dimension: Some(Dimension::D2),
                mips: 1..2,
                layers: 1..2,
                plane: 0,
            })
            .unwrap();
        assert_eq!(
            view.size(),
            Extent {
                width: 4,
                height: 4,
                depth_or_layers: 1
            }
        );
        assert_eq!(view.image().id(), image.id());
        assert_eq!(
            image.clone().view().inner.0.handle,
            image.view().inner.0.handle
        );
        let area = Rect {
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        };
        let results = readback(&device, 64);
        let queries = device.create_query_set(QueryKind::Occlusion, 1).unwrap();
        let counts = readback(&device, 16);
        // An abandoned encoder must not publish its image layouts.
        let mut abandoned = queue.encoder("abandon");
        drop(abandoned.render_pass(&pass(
            &view,
            area,
            Load::Clear([1.0, 1.0, 1.0, 1.0]),
            Store::Store,
            None,
        )));
        drop(abandoned);
        let mut encoder = queue.encoder("subresources");
        for (i, load) in [
            Load::Clear([0.0, 0.0, 1.0, 1.0]),
            Load::Clear([1.0, 0.0, 0.0, 1.0]),
        ]
        .into_iter()
        .enumerate()
        {
            let area = if i == 0 {
                area
            } else {
                Rect {
                    x: 1,
                    y: 1,
                    w: 2,
                    h: 2,
                }
            };
            {
                let mut render = encoder.render_pass(&pass(&view, area, load, Store::Store, None));
                render.begin_occlusion(&queries, 0);
                render.end_occlusion();
            }
            encoder.resolve_queries(&queries, 0..1, &counts, i as u64 * 8);
        }
        drop(encoder.render_pass(&pass(&view, area, Load::None, Store::None, None)));
        read_image(&mut encoder, &image, 1, 1, view.size(), &results);
        finish(&mut queue, encoder);
        assert_eq!(bytes(&counts), [0; 16]);
        for y in 0..4 {
            for x in 0..4 {
                let expected = if (1..3).contains(&x) && (1..3).contains(&y) {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                };
                assert_eq!(
                    bytes(&results)[((y * 4 + x) * 4)..((y * 4 + x) * 4 + 4)],
                    expected
                );
            }
        }
        if device.caps().limits.sample_counts & 4 != 0 {
            let size = Extent {
                width: 4,
                height: 4,
                depth_or_layers: 1,
            };
            let msaa = create_image(&device, Format::Rgba8Unorm, size, 1, 4);
            let resolved = create_image(&device, Format::Rgba8Unorm, size, 1, 1);
            let mut encoder = queue.encoder("resolve clear");
            drop(encoder.render_pass(&pass(
                msaa.view(),
                area,
                Load::Clear([0.0, 1.0, 0.0, 1.0]),
                Store::DontCare,
                Some(resolved.view()),
            )));
            read_image(&mut encoder, &resolved, 0, 0, size, &results);
            finish(&mut queue, encoder);
            for pixel in bytes(&results).as_chunks::<4>().0 {
                assert_eq!(*pixel, [0, 255, 0, 255]);
            }
        }
        let integer = create_image(
            &device,
            Format::R32Uint,
            Extent {
                width: 4,
                height: 4,
                depth_or_layers: 1,
            },
            1,
            1,
        );
        let mut encoder = queue.encoder("integer clear");
        drop(encoder.render_pass(&pass(
            integer.view(),
            area,
            Load::Clear([7.0, 0.0, 0.0, 0.0]),
            Store::Store,
            None,
        )));
        read_image(&mut encoder, &integer, 0, 0, integer.desc().size, &results);
        finish(&mut queue, encoder);
        for word in bytes(&results).as_chunks::<4>().0 {
            assert_eq!(u32::from_le_bytes(*word), 7);
        }
        let copy_only = device
            .create_image(&ImageDesc {
                usage: Usage::COPY_SRC | Usage::COPY_DST,
                ..*integer.desc()
            })
            .unwrap();
        let upload = device
            .create_buffer(&BufferDesc {
                size: 64,
                usage: BufferUsage::COPY_SRC,
                memory: Memory::Upload,
                label: "internal upload",
            })
            .unwrap();
        let pattern: Vec<u8> = (0..64).collect();
        // SAFETY: The upload has not been submitted, so the CPU owns its mapping.
        unsafe {
            (&mut *upload.mapped().unwrap().as_ptr()).copy_from_slice(&pattern);
        }
        let mut encoder = queue.encoder("image copy");
        encoder.copy_buffer_to_image(
            BufferCopy {
                buffer: &upload,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            ImageCopy {
                image: &copy_only,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            copy_only.desc().size,
        );
        encoder.copy_image(
            ImageCopy {
                image: &copy_only,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            ImageCopy {
                image: &integer,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            integer.desc().size,
        );
        read_image(&mut encoder, &integer, 0, 0, integer.desc().size, &results);
        drop(upload);
        drop(copy_only);
        finish(&mut queue, encoder);
        assert_eq!(bytes(&results), pattern);
        let storage = device
            .create_image(&ImageDesc {
                format: Format::Rgba8Unorm,
                usage: Usage::SAMPLED | Usage::STORAGE,
                view_formats: &[Format::Rgba8Srgb],
                ..*integer.desc()
            })
            .unwrap();
        storage
            .create_view(&ViewDesc {
                format: Some(Format::Rgba8Srgb),
                dimension: None,
                mips: 0..1,
                layers: 0..1,
                plane: 0,
            })
            .unwrap();
        let desc = SamplerDesc {
            min: Filter::Nearest,
            mag: Filter::Linear,
            mip: Filter::Nearest,
            address: [Address::Clamp; 3],
            border: BorderColor::TransparentBlack,
            lod_bias: 0,
            lod_min: 0,
            lod_max: 256,
            anisotropy: 1,
            compare: None,
        };
        let first = device.sampler(&desc).unwrap();
        let second = device.sampler(&desc).unwrap();
        assert!(Arc::ptr_eq(&first.inner.0, &second.inner.0));
        for format in [Format::Nv12, Format::P010] {
            if !device.caps().format(format).contains(FormatCaps::SAMPLED) {
                continue;
            }
            let desc = ImageDesc {
                format,
                usage: Usage::SAMPLED | Usage::COPY_SRC | Usage::COPY_DST,
                ..*integer.desc()
            };
            let source = device.create_image(&desc).unwrap();
            let destination = device.create_image(&desc).unwrap();
            let size = if format == Format::Nv12 { 24 } else { 48 };
            let upload = device
                .create_buffer(&BufferDesc {
                    size,
                    usage: BufferUsage::COPY_SRC,
                    memory: Memory::Upload,
                    label: "planar upload",
                })
                .unwrap();
            let pattern: Vec<u8> = if format == Format::Nv12 {
                (0..size as u8).collect()
            } else {
                (0..size as u16 / 2)
                    .flat_map(|v| ((v * 7) << 6).to_le_bytes())
                    .collect()
            };
            // SAFETY: This mapping has no GPU users until the submission below.
            unsafe {
                (&mut *upload.mapped().unwrap().as_ptr()).copy_from_slice(&pattern);
            }
            let results = readback(&device, size);
            let copy = |image| ImageCopy {
                image,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            };
            let mut encoder = queue.encoder("planar copy");
            encoder.copy_buffer_to_image(
                BufferCopy {
                    buffer: &upload,
                    offset: 0,
                    row_pitch: 0,
                    rows: 0,
                },
                copy(&source),
                desc.size,
            );
            encoder.copy_image(copy(&source), copy(&destination), desc.size);
            read_image(&mut encoder, &destination, 0, 0, desc.size, &results);
            finish(&mut queue, encoder);
            assert_eq!(bytes(&results), pattern);
        }
    }
}

#[test]
fn ring_reservations_retire_in_order_and_loss_is_sticky() {
    let (device, mut queue) = gpu(Modern::empty());
    let mut ring = device.ring(1024).unwrap();
    let first = ring.reserve(512, 16).unwrap();
    let second = ring.reserve(512, 16).unwrap();
    drop(second);
    let encoder = queue.encoder("unused later span");
    finish(&mut queue, encoder);
    assert_eq!(ring.reserve(1, 1).err(), Some(Error::Busy));
    let mut encoder = queue.encoder("first span");
    encoder.retain(first);
    finish(&mut queue, encoder);
    let span = ring.reserve(1024, 16).unwrap();
    assert_eq!(span.offset(), 0);
    drop(span);
    let error = device
        .inner
        .0
        .result::<()>(Err(vk::Result::ERROR_DEVICE_LOST))
        .unwrap_err();
    assert!(matches!(error, Error::DeviceLost(_)));
    assert!(device.lost().is_some());
    assert!(matches!(queue.poll(), Err(Error::DeviceLost(_))));
    assert!(matches!(
        queue.wait(Token(0), Duration::ZERO),
        Err(Error::DeviceLost(_))
    ));
    assert!(matches!(device.ring(1024), Err(Error::DeviceLost(_))));
    assert!(matches!(ring.reserve(1, 1), Err(Error::DeviceLost(_))));
}
