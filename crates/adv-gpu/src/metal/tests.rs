use super::*;
use arrayvec::ArrayVec;

const WHOLE: Rect = Rect {
    x: 0,
    y: 0,
    w: 8,
    h: 8,
};
const AREA: Rect = Rect {
    x: 2,
    y: 2,
    w: 4,
    h: 4,
};
const TIMEOUT: Duration = Duration::from_secs(20);

fn gpu() -> (crate::Device, crate::Queue) {
    crate::Device::open(&DeviceOptions::default()).unwrap()
}
fn image(device: &crate::Device, format: Format) -> crate::Image {
    device
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: 8,
                height: 8,
                depth_or_layers: 1,
            },
            format,
            mips: 1,
            samples: 1,
            usage: if format.has_depth() {
                Usage::DEPTH_STENCIL | Usage::COPY_SRC
            } else {
                Usage::COLOR | Usage::COPY_SRC
            },
            view_formats: &[],
            label: "bounded clear test",
        })
        .unwrap()
}
fn readback(device: &crate::Device, size: u64) -> crate::Buffer {
    device
        .create_buffer(&BufferDesc {
            size,
            usage: BufferUsage::COPY_DST,
            memory: Memory::Readback,
            label: "test readback",
        })
        .unwrap()
}
fn finish(queue: &mut crate::Queue, encoder: crate::Encoder) {
    let token = queue.submit(encoder).unwrap();
    assert!(queue.wait(token, TIMEOUT).unwrap());
}
fn bytes(buffer: &crate::Buffer) -> &[u8] {
    // All callers finish the submission before observing mapped bytes.
    unsafe { &*buffer.mapped().unwrap().as_ptr() }
}
fn inside(x: usize, y: usize) -> bool {
    (2..6).contains(&x) && (2..6).contains(&y)
}
fn color_pass<'a>(
    view: &'a crate::ImageView,
    load: Load<[f64; 4]>,
    store: Store,
    area: Rect,
) -> RenderPassDesc<'a> {
    let mut colors = ArrayVec::new();
    colors.push(ColorAttachment {
        view,
        load,
        store,
        resolve: None,
    });
    RenderPassDesc {
        colors,
        depth_stencil: None,
        area,
        timestamps: None,
        label: "bounded clear",
    }
}

#[test]
fn bounded_color_clears_and_unused_attachments_preserve_contents() {
    let (device, mut queue) = gpu();
    for format in [Format::Rgba8Unorm, Format::R32Uint] {
        let image = image(&device, format);
        let dst = readback(&device, 256);
        let initial = if format == Format::R32Uint {
            f64::from(0xff00_0001u32)
        } else {
            1.0
        };
        let cleared = if format == Format::R32Uint {
            f64::from(0x8000_0003u32)
        } else {
            0.0
        };
        let mut encoder = queue.encoder("bounded color");
        drop(encoder.render_pass(&color_pass(
            image.view(),
            Load::Clear([initial; 4]),
            Store::Store,
            WHOLE,
        )));
        drop(encoder.render_pass(&color_pass(
            image.view(),
            Load::Clear([cleared; 4]),
            Store::DontCare,
            AREA,
        )));
        drop(encoder.render_pass(&color_pass(image.view(), Load::None, Store::Store, WHOLE)));
        drop(encoder.render_pass(&color_pass(
            image.view(),
            Load::Clear([0.5; 4]),
            Store::None,
            WHOLE,
        )));
        encoder.copy_image_to_buffer(
            ImageCopy {
                image: &image,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            BufferCopy {
                buffer: &dst,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            image.desc().size,
        );
        finish(&mut queue, encoder);
        for y in 0..8 {
            for x in 0..8 {
                let word = &bytes(&dst)[(y * 8 + x) * 4..(y * 8 + x + 1) * 4];
                let expected = if format == Format::R32Uint {
                    if inside(x, y) {
                        0x8000_0003u32.to_le_bytes()
                    } else {
                        0xff00_0001u32.to_le_bytes()
                    }
                } else if inside(x, y) {
                    [0; 4]
                } else {
                    [255; 4]
                };
                assert_eq!(word, expected, "{format:?} ({x}, {y})");
            }
        }
    }
}

#[test]
fn bounded_depth_and_stencil_clears_preserve_the_other_aspect() {
    let (device, mut queue) = gpu();
    let image = image(&device, Format::Depth32FloatStencil8);
    let depth = readback(&device, 256);
    let stencil = readback(&device, 64);
    let mut encoder = queue.encoder("bounded depth stencil");
    for (area, depth, stencil) in [
        (WHOLE, Load::Clear(0.75), Load::Clear(7)),
        (AREA, Load::Clear(0.25), Load::Load),
        (AREA, Load::Load, Load::Clear(3)),
    ] {
        drop(encoder.render_pass(&RenderPassDesc {
            colors: ArrayVec::new(),
            depth_stencil: Some(DepthAttachment {
                view: image.view(),
                depth: (depth, Store::Store),
                stencil: (stencil, Store::Store),
                read_only: false,
            }),
            area,
            timestamps: None,
            label: "clear depth stencil",
        }));
    }
    let blit = encoder.inner.command.blitCommandEncoder().unwrap();
    for (dst, pitch, option) in [
        (&depth, 32, MTLBlitOption::DepthFromDepthStencil),
        (&stencil, 8, MTLBlitOption::StencilFromDepthStencil),
    ] {
        unsafe {
            blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage_options(
            &image.inner.data.raw, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 }, MTLSize { width: 8, height: 8, depth: 1 },
            &dst.inner.0.raw, 0, pitch, pitch * 8, option);
        }
    }
    blit.endEncoding();
    finish(&mut queue, encoder);
    for y in 0..8 {
        for x in 0..8 {
            let offset = (y * 8 + x) * 4;
            let got = f32::from_le_bytes(bytes(&depth)[offset..offset + 4].try_into().unwrap());
            assert_eq!(
                got,
                if inside(x, y) { 0.25 } else { 0.75 },
                "depth ({x}, {y})"
            );
            assert_eq!(
                bytes(&stencil)[y * 8 + x],
                if inside(x, y) { 3 } else { 7 },
                "stencil ({x}, {y})"
            );
        }
    }
}

#[test]
fn public_handles_meet_the_thread_contract() {
    fn shared<T: Send + Sync>() {}
    fn movable<T: Send>() {}
    shared::<crate::Device>();
    shared::<crate::Image>();
    shared::<crate::ImageView>();
    shared::<crate::Buffer>();
    shared::<crate::Sampler>();
    shared::<crate::queue::QuerySet>();
    movable::<crate::Queue>();
    movable::<crate::Ring>();
    movable::<crate::Span>();
}

#[test]
fn visibility_queries_count_internal_draws() {
    let (device, mut queue) = gpu();
    let image = image(&device, Format::Rgba8Unorm);
    let set = device.create_query_set(QueryKind::Occlusion, 2).unwrap();
    let dst = readback(&device, 16);
    let mut encoder = queue.encoder("visibility");
    for index in 0..2 {
        let mut pass = encoder.render_pass(&color_pass(
            image.view(),
            Load::DontCare,
            Store::Store,
            AREA,
        ));
        pass.begin_occlusion(&set, index);
        pass.set_scissor(WHOLE);
        device.inner.clear(
            &pass.inner.raw,
            &color_pass(image.view(), Load::Clear([1.0; 4]), Store::Store, AREA),
            AREA,
        );
        pass.end_occlusion();
    }
    encoder.resolve_queries(&set, 0..2, &dst, 0);
    finish(&mut queue, encoder);
    for value in bytes(&dst).as_chunks::<8>().0 {
        assert_eq!(u64::from_le_bytes(*value), 16);
    }
}

#[test]
fn both_pass_types_write_nonzero_timestamp_boundaries() {
    let (device, mut queue) = gpu();
    if !device.caps().features.contains(Features::TIMESTAMPS) {
        return;
    }
    let image = image(&device, Format::Rgba8Unorm);
    let set = device.create_query_set(QueryKind::Timestamp, 64).unwrap();
    let dst = readback(&device, 64 * 8);
    let mut encoder = queue.encoder("timestamp boundaries");
    for first in (0..64).step_by(4) {
        let mut render = color_pass(image.view(), Load::Clear([1.0; 4]), Store::Store, WHOLE);
        render.timestamps = Some((&set, first));
        drop(encoder.render_pass(&render));
        drop(encoder.compute_pass(&ComputePassDesc {
            timestamps: Some((&set, first + 2)),
            label: "compute timestamps",
        }));
    }
    encoder.resolve_queries(&set, 0..64, &dst, 0);
    finish(&mut queue, encoder);
    let timestamps: Vec<_> = bytes(&dst)
        .as_chunks::<8>()
        .0
        .iter()
        .map(|v| u64::from_le_bytes(*v))
        .collect();
    assert!(
        timestamps.iter().all(|&v| v != 0 && v != u64::MAX),
        "{timestamps:?}"
    );
    for pair in timestamps.as_chunks::<2>().0 {
        assert!(pair[0] <= pair[1], "{pair:?}");
    }
}

#[test]
fn multisample_resolves_preserve_pixels_outside_the_area() {
    let (device, mut queue) = gpu();
    let resolved = image(&device, Format::Rgba8Unorm);
    let dst = readback(&device, 256);
    for (transient, area) in [(false, AREA), (true, WHOLE), (true, AREA)] {
        let msaa = device
            .create_image(&ImageDesc {
                samples: 4,
                usage: Usage::COLOR
                    | if transient {
                        Usage::TRANSIENT
                    } else {
                        Usage::empty()
                    },
                ..*resolved.desc()
            })
            .unwrap();
        let mut encoder = queue.encoder("multisample resolve");
        drop(encoder.render_pass(&color_pass(
            resolved.view(),
            Load::Clear([0.0, 0.0, 1.0, 1.0]),
            Store::Store,
            WHOLE,
        )));
        if !transient {
            drop(encoder.render_pass(&color_pass(
                msaa.view(),
                Load::Clear([1.0, 0.0, 0.0, 1.0]),
                Store::Store,
                WHOLE,
            )));
        }
        let mut desc = color_pass(
            msaa.view(),
            Load::Clear([0.0, 1.0, 0.0, 1.0]),
            if transient {
                Store::DontCare
            } else {
                Store::Store
            },
            area,
        );
        desc.colors[0].resolve = Some(resolved.view());
        drop(encoder.render_pass(&desc));
        encoder.copy_image_to_buffer(
            ImageCopy {
                image: &resolved,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            BufferCopy {
                buffer: &dst,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            resolved.desc().size,
        );
        finish(&mut queue, encoder);
        for y in 0..8 {
            for x in 0..8 {
                let expected = if area == WHOLE || inside(x, y) {
                    [0, 255, 0, 255]
                } else {
                    [0, 0, 255, 255]
                };
                assert_eq!(
                    &bytes(&dst)[(y * 8 + x) * 4..(y * 8 + x + 1) * 4],
                    expected,
                    "({x}, {y}), transient={transient}"
                );
            }
        }
    }
}
