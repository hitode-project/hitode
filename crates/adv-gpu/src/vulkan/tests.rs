use super::*;
use std::time::Duration;

macro_rules! gpu {
    ($disable:expr) => {
        test_device!(DeviceOptions {
            software: !cfg!(target_os = "android"),
            // Shell processes on user builds cannot load layers.
            validation: !cfg!(target_os = "android"),
            disable: $disable,
            ..DeviceOptions::default()
        })
    };
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

#[cfg(target_os = "android")]
#[test]
fn present_batches_reuse_fences_and_preserve_content() {
    for disable in [Modern::empty(), Modern::all()] {
        let (device, mut queue) = gpu!(disable);
        if !device
            .caps()
            .features
            .contains(Features::PRESENTABLE_BUFFERS)
        {
            continue;
        }
        let mut pool = device
            .create_present_pool(&PoolDesc {
                color: None,
                width: 8,
                height: 8,
                format: present::FORMAT,
                count: 2,
                storage: false,
                label: "present batch",
            })
            .unwrap();
        let size = Extent {
            width: 8,
            height: 8,
            depth_or_layers: 1,
        };
        let outputs = [readback(&device, 8 * 8 * 4), readback(&device, 8 * 8 * 4)];
        for frame in 0..4 {
            let mut images: Vec<_> = (0..2)
                .map(|_| pool.acquire(&mut queue).unwrap().unwrap())
                .collect();
            if frame != 0 {
                assert!(images.iter().all(|i| i.age().is_some()));
                // An abandoned recording must leave the foreign state intact.
                let mut abandoned = queue.encoder("abandoned acquire");
                read_image(&mut abandoned, images[0].image(), 0, 0, size, &outputs[0]);
                drop(abandoned);
                assert_eq!(
                    images[0].image().inner.core.states.lock().unwrap()[0].family,
                    vk::QUEUE_FAMILY_FOREIGN_EXT
                );
            }
            let mut encoder = queue.encoder("present batch");
            for (image, output) in images.iter().zip(&outputs) {
                let (area, color) = if frame == 0 {
                    (
                        Rect {
                            x: 0,
                            y: 0,
                            w: 8,
                            h: 8,
                        },
                        [1.0, 0.0, 0.0, 1.0],
                    )
                } else {
                    (
                        Rect {
                            x: 0,
                            y: 0,
                            w: 1,
                            h: 1,
                        },
                        [0.0, 1.0, 0.0, 1.0],
                    )
                };
                drop(encoder.render_pass(&pass(
                    image.image().view(),
                    area,
                    Load::Clear(color),
                    Store::Store,
                    None,
                )));
                read_image(&mut encoder, image.image(), 0, 0, size, output);
            }
            let token = queue
                .submit_present(encoder, &mut images.iter_mut().collect::<Vec<_>>())
                .unwrap();
            for mut image in images {
                let fence = image.acquire_fence();
                // Loop the GPU's sync fd back as the compositor release. The
                // next batch must import it, even before this token completes.
                image.released(fence);
            }
            if frame != 3 {
                continue;
            }
            assert!(queue.wait(token, Duration::from_secs(20)).unwrap());
            for output in &outputs {
                for (i, pixel) in bytes(output).as_chunks::<4>().0.iter().enumerate() {
                    assert_eq!(
                        pixel,
                        if i == 0 {
                            &[0, 255, 0, 255]
                        } else {
                            &[255, 0, 0, 255]
                        }
                    );
                }
            }
        }
    }
}

#[test]
fn views_partial_clears_query_resets_and_resolves() {
    for disable in [Modern::empty(), Modern::all()] {
        let (device, mut queue) = gpu!(disable);
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
            let uploaded = readback(&device, size);
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
            read_image(&mut encoder, &source, 0, 0, desc.size, &uploaded);
            encoder.copy_image(copy(&source), copy(&destination), desc.size);
            read_image(&mut encoder, &destination, 0, 0, desc.size, &results);
            finish(&mut queue, encoder);
            // A Mali-G52 driver (r34) reads back zero chroma here although the
            // format reports transfers; barriers use COLOR, as non-disjoint
            // planar images require. Players never copy planar images on the
            // GPU; decoders write them on the CPU.
            assert_eq!(bytes(&uploaded), pattern, "{format:?} upload");
            assert_eq!(bytes(&results), pattern, "{format:?} image copy");
        }
    }
}

#[test]
fn ring_reservations_retire_in_order_and_loss_is_sticky() {
    let (device, mut queue) = gpu!(Modern::empty());
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
}

#[test]
fn binding_tiers_render_identical_pixels() {
    let mut results = Vec::new();
    for (disable, tier, source_kind) in [
        (Modern::empty(), Modern::BINDLESS),
        (Modern::BINDLESS, Modern::PUSH_DESCRIPTORS),
        (Modern::BINDLESS | Modern::PUSH_DESCRIPTORS, Modern::empty()),
    ]
    .into_iter()
    .flat_map(|(disable, tier)| (0..3).map(move |source_kind| (disable, tier, source_kind)))
    {
        let (device, mut queue) = gpu!(disable);
        // Devices without a tier compare the tiers they have.
        if !device.caps().modern.contains(tier) {
            continue;
        }
        assert_eq!(device.inner.0.heap.is_some(), tier == Modern::BINDLESS);
        let wgsl = "
            struct Block { color: vec4<f32> }
            @group(0) @binding(0) var<uniform> block: Block;
            @group(0) @binding(1) var<uniform> second: Block;
            @group(1) @binding(0) var image: texture_2d<f32>;
            @group(1) @binding(1) var other: texture_2d<f32>;
            @group(2) @binding(0) var nearest: sampler;
            @group(2) @binding(1) var other_sampler: sampler;
            @group(3) @binding(0) var<storage,read> positions: array<vec2<f32>>;
            struct Positions {data:array<vec2<f32>>}
            @group(3) @binding(1) var<storage,read> other_positions:Positions;
            fn position(i:u32) -> vec2<f32> {return (positions[i]+other_positions.data[i])*0.5;}
            @vertex fn vs(@builtin(vertex_index) i:u32) -> @builtin(position) vec4<f32> {
                return vec4<f32>(position(i), 0.5, 1.0);
            }
            @fragment fn fs() -> @location(0) vec4<f32> {
                return (textureSample(image, nearest, vec2<f32>(0.5))+textureSample(other,other_sampler,vec2<f32>(0.5)))*0.5 * block.color * second.color;
            }
        ";
        let module = naga::front::wgsl::parse_str(wgsl).unwrap();
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        let spirv = naga::back::spv::write_vec(&module, &info, &Default::default(), None).unwrap();
        let source = if source_kind == 0 {
            Source::Wgsl(wgsl)
        } else if source_kind == 1 {
            Source::Ir(Box::new(module))
        } else {
            Source::Precompiled {
                spirv: &spirv,
                metal: b"unused",
                entries: &[],
            }
        };
        let shader = device
            .create_shader(ShaderDesc {
                source,
                identity: 101 + source_kind,
                label: "binding tiers",
            })
            .unwrap();
        assert!(shader.entry("vs", Stage::Fragment).is_err());
        assert!(shader.entry("missing", Stage::Vertex).is_err());
        let mut desc = RenderPipelineDesc {
            vertex: shader.entry("vs", Stage::Vertex).unwrap(),
            fragment: Some(shader.entry("fs", Stage::Fragment).unwrap()),
            bindings: vec![
                Binding {
                    slot: 0,
                    kind: BindingKind::UniformBuffer,
                    stages: Stages::FRAGMENT,
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
                    slot: 0,
                    kind: BindingKind::Sampler { comparison: false },
                    stages: Stages::FRAGMENT,
                },
                Binding {
                    slot: 0,
                    kind: BindingKind::StorageBuffer { writable: false },
                    stages: Stages::VERTEX,
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
                format: Format::Rgba8Unorm,
                blend: None,
                write_mask: 15,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            samples: 1,
            alpha_to_coverage: false,
            label: "binding tiers",
        };
        let extra: Vec<_> = desc
            .bindings
            .iter()
            .map(|b| Binding { slot: 1, ..*b })
            .collect();
        desc.bindings.extend(extra);
        let pipeline = device.create_render_pipeline(&desc, None).unwrap();
        let duplicate = device.create_render_pipeline(&desc, None).unwrap();
        let mut disabled = desc.clone();
        disabled.colors[0].write_mask = 0;
        let disabled = device.create_render_pipeline(&disabled, None).unwrap();
        assert_ne!(
            pipeline.inner.compiled.handle,
            disabled.inner.compiled.handle
        );
        assert_eq!(
            pipeline.inner.compiled.handle,
            duplicate.inner.compiled.handle
        );
        let sampler = device
            .sampler(&SamplerDesc {
                min: Filter::Nearest,
                mag: Filter::Nearest,
                mip: Filter::Nearest,
                address: [Address::Clamp; 3],
                border: BorderColor::TransparentBlack,
                lod_bias: 0,
                lod_min: 0,
                lod_max: 0,
                anisotropy: 1,
                compare: None,
            })
            .unwrap();
        let upload = |data: &[u8], usage| {
            let buffer = device
                .create_buffer(&BufferDesc {
                    size: data.len() as u64,
                    usage,
                    memory: Memory::Upload,
                    label: "tier data",
                })
                .unwrap();
            // SAFETY: New buffers have no GPU users.
            unsafe {
                (&mut *buffer.mapped().unwrap().as_ptr()).copy_from_slice(data);
            }
            buffer
        };
        let floats = |data: &[f32]| {
            data.iter()
                .flat_map(|f| f.to_ne_bytes())
                .collect::<Vec<_>>()
        };
        let storage_stride = device.caps().limits.storage_offset_alignment.max(8) as usize;
        let mut vertices = vec![0; storage_stride + 24];
        vertices[storage_stride..].copy_from_slice(&floats(&[-1.0, -1.0, 3.0, -1.0, -1.0, 3.0]));
        let positions = upload(&vertices, BufferUsage::STORAGE);
        let stride = device.caps().limits.uniform_offset_alignment.max(16) as usize;
        let mut blocks = vec![0; stride + 16];
        blocks[..16].copy_from_slice(&floats(&[1.0, 0.0, 0.0, 1.0]));
        blocks[stride..].copy_from_slice(&floats(&[0.0, 1.0, 0.0, 1.0]));
        let uniforms = upload(&blocks, BufferUsage::UNIFORM);
        let indices = upload(&[0, 0, 1, 0, 2, 0], BufferUsage::INDEX);
        let indirect = upload(
            &[3u32, 1, 0, 0]
                .into_iter()
                .flat_map(u32::to_ne_bytes)
                .collect::<Vec<_>>(),
            BufferUsage::INDIRECT,
        );
        let indexed_args = upload(
            &[3u32, 1, 0, 0, 0]
                .into_iter()
                .flat_map(u32::to_ne_bytes)
                .collect::<Vec<_>>(),
            BufferUsage::INDIRECT,
        );
        let size = Extent {
            width: 8,
            height: 8,
            depth_or_layers: 1,
        };
        let image = create_image(&device, Format::Rgba8Unorm, size, 1, 1);
        let texture = create_image(
            &device,
            Format::Rgba8Unorm,
            Extent {
                width: 1,
                height: 1,
                depth_or_layers: 1,
            },
            1,
            1,
        );
        let output = readback(&device, 256);
        let mut encoder = queue.encoder("tier comparison");
        drop(encoder.render_pass(&pass(
            texture.view(),
            Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            Load::Clear([1.0; 4]),
            Store::Store,
            None,
        )));
        {
            let mut render = encoder.render_pass(&pass(
                image.view(),
                Rect {
                    x: 0,
                    y: 0,
                    w: 8,
                    h: 8,
                },
                Load::Clear([0.0; 4]),
                Store::Store,
                None,
            ));
            render.set_pipeline(&pipeline);
            render.set_pipeline(&duplicate);
            render.bind(0, Bind::Sampled(texture.view()));
            render.bind(1, Bind::Sampled(texture.view()));
            render.bind(0, Bind::Sampler(&sampler));
            render.bind(1, Bind::Sampler(&sampler));
            render.bind(
                0,
                Bind::StorageBuffer(
                    positions.slice(storage_stride as u64..storage_stride as u64 + 24),
                ),
            );
            render.bind(
                1,
                Bind::StorageBuffer(
                    positions.slice(storage_stride as u64..storage_stride as u64 + 24),
                ),
            );
            render.set_index_buffer(indices.slice(0..6), IndexFormat::U16);
            for (i, x) in [0, 4].into_iter().enumerate() {
                render.bind(
                    0,
                    Bind::Uniform(uniforms.slice((i * stride) as u64..(i * stride + 16) as u64)),
                );
                render.bind(
                    1,
                    Bind::Uniform(uniforms.slice((i * stride) as u64..(i * stride + 16) as u64)),
                );
                render.set_scissor(Rect {
                    x,
                    y: 0,
                    w: 4,
                    h: 8,
                });
                render.draw(0..3, 0..1);
                render.draw_indexed(0..3, 0, 0..1);
                render.draw_indirect(indirect.slice(0..16), false);
                render.draw_indirect(indexed_args.slice(0..20), true);
                if i == 0 {
                    render.set_pipeline(&disabled);
                    render.bind(
                        0,
                        Bind::Uniform(uniforms.slice(stride as u64..stride as u64 + 16)),
                    );
                    render.bind(
                        1,
                        Bind::Uniform(uniforms.slice(stride as u64..stride as u64 + 16)),
                    );
                    render.draw(0..3, 0..1);
                    render.set_pipeline(&pipeline);
                }
            }
        }
        read_image(&mut encoder, &image, 0, 0, size, &output);
        drop((
            uniforms,
            positions,
            sampler,
            indices,
            indirect,
            indexed_args,
            pipeline,
            duplicate,
            disabled,
            shader,
            texture,
            image,
        ));
        finish(&mut queue, encoder);
        let pixels = bytes(&output).to_vec();
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(
                    &pixels[(y * 8 + x) * 4..(y * 8 + x + 1) * 4],
                    if x < 4 {
                        &[255, 0, 0, 255]
                    } else {
                        &[0, 255, 0, 255]
                    }
                );
            }
        }
        results.push(pixels);
    }
    for pixels in &results[1..] {
        assert_eq!(&results[0], pixels);
    }
}

#[test]
fn binding_tiers_dispatch_storage_images_and_indirect_work() {
    for disable in [
        Modern::empty(),
        Modern::BINDLESS,
        Modern::BINDLESS | Modern::PUSH_DESCRIPTORS,
    ] {
        let (device, mut queue) = gpu!(disable);
        let shader = device
            .create_shader(ShaderDesc {
                source: Source::Wgsl(
                    "
            @group(3) @binding(0) var output_image:texture_storage_2d<rgba8unorm,write>;
            @group(3) @binding(1) var<storage,read_write> values:array<u32>;
            @compute @workgroup_size(1) fn main(@builtin(global_invocation_id) id:vec3<u32>) {
                values[id.x]=id.x+17u;
                textureStore(output_image,vec2<i32>(id.xy),vec4<f32>(1.0,0.0,0.0,1.0));
            }
        ",
                ),
                identity: 102,
                label: "storage dispatch",
            })
            .unwrap();
        let desc = ComputePipelineDesc {
            entry: shader.entry("main", Stage::Compute).unwrap(),
            bindings: vec![
                Binding {
                    slot: 0,
                    kind: BindingKind::StorageImage {
                        format: Format::Rgba8Unorm,
                        writable: true,
                    },
                    stages: Stages::COMPUTE,
                },
                Binding {
                    slot: 1,
                    kind: BindingKind::StorageBuffer { writable: true },
                    stages: Stages::COMPUTE,
                },
            ],
            label: "storage dispatch",
        };
        let pipeline = device.create_compute_pipeline(&desc, None).unwrap();
        let duplicate = device.create_compute_pipeline(&desc, None).unwrap();
        assert_eq!(
            pipeline.inner.compiled.handle,
            duplicate.inner.compiled.handle
        );
        let target = device
            .create_image(&ImageDesc {
                dimension: Dimension::D2,
                size: Extent {
                    width: 2,
                    height: 1,
                    depth_or_layers: 1,
                },
                format: Format::Rgba8Unorm,
                mips: 1,
                samples: 1,
                usage: Usage::STORAGE | Usage::COPY_SRC,
                view_formats: &[],
                label: "storage output",
            })
            .unwrap();
        let buffer = device
            .create_buffer(&BufferDesc {
                size: 8,
                usage: BufferUsage::STORAGE | BufferUsage::COPY_SRC,
                memory: Memory::Device,
                label: "storage values",
            })
            .unwrap();
        let args = device
            .create_buffer(&BufferDesc {
                size: 12,
                usage: BufferUsage::INDIRECT,
                memory: Memory::Upload,
                label: "dispatch args",
            })
            .unwrap();
        // SAFETY: The new indirect buffer has no GPU users.
        unsafe {
            (&mut *args.mapped().unwrap().as_ptr()).copy_from_slice(
                &[2u32, 1, 1]
                    .into_iter()
                    .flat_map(u32::to_ne_bytes)
                    .collect::<Vec<_>>(),
            );
        }
        let output = readback(&device, 16);
        let mut encoder = queue.encoder("storage dispatch");
        {
            let mut pass = encoder.compute_pass(&ComputePassDesc {
                timestamps: None,
                label: "dispatch",
            });
            pass.set_pipeline(&pipeline);
            pass.bind(0, Bind::StorageImage(target.view()));
            pass.bind(1, Bind::StorageBuffer(buffer.slice(0..8)));
            pass.dispatch(2, 1, 1);
            pass.set_pipeline(&duplicate);
            pass.dispatch_indirect(args.slice(0..12));
        }
        encoder.copy_image_to_buffer(
            ImageCopy {
                image: &target,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            BufferCopy {
                buffer: &output,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            target.desc().size,
        );
        encoder.copy_buffer(buffer.slice(0..8), &output, 8);
        drop((pipeline, duplicate, target, buffer, args, shader));
        finish(&mut queue, encoder);
        assert_eq!(&bytes(&output)[..8], &[255, 0, 0, 255, 255, 0, 0, 255]);
        assert_eq!(
            u32::from_ne_bytes(bytes(&output)[8..12].try_into().unwrap()),
            17
        );
        assert_eq!(
            u32::from_ne_bytes(bytes(&output)[12..16].try_into().unwrap()),
            18
        );
    }
}

fn storage_pipeline(device: &crate::Device, source: &str) -> crate::RenderPipeline {
    let shader = device
        .create_shader(ShaderDesc {
            source: Source::Wgsl(source),
            identity: 110,
            label: "storage addressing",
        })
        .unwrap();
    device
        .create_render_pipeline(
            &RenderPipelineDesc {
                vertex: shader.entry("vs", Stage::Vertex).unwrap(),
                fragment: Some(shader.entry("fs", Stage::Fragment).unwrap()),
                bindings: vec![Binding {
                    slot: 0,
                    kind: BindingKind::StorageBuffer { writable: false },
                    stages: Stages::VERTEX,
                }],
                primitive: Primitive {
                    topology: Topology::Triangles,
                    cull: Cull::None,
                    front_ccw: false,
                    wireframe: false,
                    unclipped_depth: false,
                },
                colors: [ColorTarget {
                    format: Format::Rgba8Unorm,
                    blend: None,
                    write_mask: 15,
                }]
                .into_iter()
                .collect(),
                depth_stencil: None,
                samples: 1,
                alpha_to_coverage: false,
                label: "storage addressing",
            },
            None,
        )
        .unwrap()
}

#[test]
fn bindless_ring_offsets_keep_one_descriptor_for_ten_thousand_draws() {
    let (device, mut queue) = gpu!(Modern::empty());
    if !device.caps().modern.contains(Modern::BINDLESS) {
        return;
    }
    let heap = device.inner.0.heap.as_ref().unwrap();
    let before = heap.used(3);
    let alignment = u64::from(device.caps().limits.storage_offset_alignment).max(16);
    let stride = 48u64.next_multiple_of(alignment);
    let mut ring = device.ring(9999 * stride + 48).unwrap();
    assert_eq!(heap.used(3), before + 1);
    let pipeline = storage_pipeline(
        &device,
        "
        @group(3) @binding(0) var<storage,read> positions:array<vec4<f32>>;
        @vertex fn vs(@builtin(vertex_index) i:u32) -> @builtin(position) vec4<f32> {
            if arrayLength(&positions)!=3u {return vec4<f32>(2.0,2.0,0.0,1.0);}
            return positions[i];
        }
        @fragment fn fs() -> @location(0) vec4<f32> {return vec4<f32>(0.0,1.0,0.0,1.0);}
    ",
    );
    let size = Extent {
        width: 4,
        height: 4,
        depth_or_layers: 1,
    };
    let image = create_image(&device, Format::Rgba8Unorm, size, 1, 1);
    let results = readback(&device, 64);
    let mut encoder = queue.encoder("ring offsets");
    let mut spans = Vec::new();
    {
        let mut render = encoder.render_pass(&pass(
            image.view(),
            Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 4,
            },
            Load::Clear([0.0; 4]),
            Store::Store,
            None,
        ));
        render.set_pipeline(&pipeline);
        for i in 0..10000 {
            let mut span = ring.reserve(48, alignment).unwrap();
            let positions = if i == 9999 {
                [
                    -1.0f32, -1.0, 0.0, 1.0, 3.0, -1.0, 0.0, 1.0, -1.0, 3.0, 0.0, 1.0,
                ]
            } else {
                [0.0; 12]
            };
            for (dst, value) in span
                .bytes()
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(positions)
            {
                dst.copy_from_slice(&value.to_ne_bytes());
            }
            // The descriptor covers the buffer, so a short bound size does not
            // replace the shader's own runtime-array bounds.
            render.bind(
                0,
                Bind::StorageBuffer(span.buffer().slice(span.offset()..span.offset() + 16)),
            );
            render.draw(0..3, 0..1);
            assert_eq!(heap.used(3), before + 1);
            spans.push(span);
        }
    }
    for span in spans {
        encoder.retain(span);
    }
    read_image(&mut encoder, &image, 0, 0, size, &results);
    finish(&mut queue, encoder);
    assert_eq!(heap.used(3), before + 1);
    for pixel in bytes(&results).as_chunks::<4>().0 {
        assert_eq!(*pixel, [0, 255, 0, 255]);
    }
    // Destruction waits for the submissions that may use the ring.
    drop(ring);
    queue.poll().unwrap();
    assert_eq!(heap.used(3), before);
}

#[test]
fn bindless_sixteen_images_and_samplers_fit_128_push_constant_bytes() {
    let (device, mut queue) = Device::open_with_push_limit(
        &DeviceOptions {
            software: !cfg!(target_os = "android"),
            // Shell processes on user builds cannot load layers.
            validation: !cfg!(target_os = "android"),
            ..Default::default()
        },
        128,
    )
    .unwrap();
    if !device.caps().modern.contains(Modern::BINDLESS) {
        return;
    }
    assert_eq!(device.caps().limits.push_constant_bytes, 128);
    assert!(device.caps().limits.max_sampled_images >= 16);
    assert!(device.caps().limits.max_samplers >= 16);
    let mut source = String::from(
        "@vertex fn vs(@builtin(vertex_index) i:u32) -> @builtin(position) vec4<f32> {let p=vec2<f32>(f32((i<<1u)&2u),f32(i&2u));return vec4<f32>(p*2.0-1.0,0.5,1.0);}\n",
    );
    let mut bindings = Vec::new();
    for slot in 0..16 {
        source.push_str(&format!("@group(1) @binding({slot}) var image{slot}:texture_2d<f32>;\n@group(2) @binding({slot}) var sampler{slot}:sampler;\n"));
        bindings.push(Binding {
            slot,
            kind: BindingKind::SampledImage {
                dimension: Dimension::D2,
                depth: false,
                multisampled: false,
            },
            stages: Stages::FRAGMENT,
        });
        bindings.push(Binding {
            slot,
            kind: BindingKind::Sampler { comparison: false },
            stages: Stages::FRAGMENT,
        });
    }
    source.push_str("@fragment fn fs() -> @location(0) vec4<f32> {var color=vec4<f32>(0.0);\n");
    for slot in 0..16 {
        source.push_str(&format!("color += (textureSample(image{slot},sampler{slot},vec2<f32>(0.5))+textureSample(image{slot},sampler{slot},vec2<f32>(2.0)))*{weight}.0;\n",weight=slot+1));
    }
    source.push_str("return color/272.0;}");
    let shader = device
        .create_shader(ShaderDesc {
            source: Source::Wgsl(&source),
            identity: 111,
            label: "sixteen textures",
        })
        .unwrap();
    let mut desc = RenderPipelineDesc {
        vertex: shader.entry("vs", Stage::Vertex).unwrap(),
        fragment: Some(shader.entry("fs", Stage::Fragment).unwrap()),
        bindings,
        primitive: Primitive {
            topology: Topology::Triangles,
            cull: Cull::None,
            front_ccw: false,
            wireframe: false,
            unclipped_depth: false,
        },
        colors: [ColorTarget {
            format: Format::Rgba8Unorm,
            blend: None,
            write_mask: 15,
        }]
        .into_iter()
        .collect(),
        depth_stencil: None,
        samples: 1,
        alpha_to_coverage: false,
        label: "sixteen textures",
    };
    let pipeline = device.create_render_pipeline(&desc, None).unwrap();
    assert_eq!(pipeline.inner.compiled.push.words, 16);
    // Sparse register numbers use the same dense budget as contiguous slots.
    let mut sparse = desc.bindings.clone();
    for binding in &mut sparse {
        binding.slot += 100;
    }
    assert_eq!(
        binding::PushLayout::new(
            &sparse,
            device.inner.0.heap.as_ref().unwrap(),
            128,
            &HashMap::new()
        )
        .unwrap()
        .words,
        16
    );
    for slot in 16..66 {
        desc.bindings.push(Binding {
            slot,
            kind: BindingKind::Sampler { comparison: false },
            stages: Stages::FRAGMENT,
        });
    }
    assert!(matches!(
        device.create_render_pipeline(&desc, None),
        Err(Error::Invalid(_))
    ));
    let compute = device
        .create_shader(ShaderDesc {
            source: Source::Wgsl("@compute @workgroup_size(1) fn main() {}"),
            identity: 112,
            label: "capacity",
        })
        .unwrap();
    assert!(matches!(
        device.create_compute_pipeline(
            &ComputePipelineDesc {
                entry: compute.entry("main", Stage::Compute).unwrap(),
                bindings: desc.bindings.clone(),
                label: "capacity"
            },
            None
        ),
        Err(Error::Invalid(_))
    ));
    let size = Extent {
        width: 4,
        height: 4,
        depth_or_layers: 1,
    };
    let image = create_image(&device, Format::Rgba8Unorm, size, 1, 1);
    let mut textures = Vec::new();
    let mut samplers = Vec::new();
    let mut expected = [0.0; 4];
    let mut encoder = queue.encoder("sixteen textures");
    for slot in 0..16 {
        let texture = create_image(
            &device,
            Format::Rgba8Unorm,
            Extent {
                width: 1,
                height: 1,
                depth_or_layers: 1,
            },
            1,
            1,
        );
        let texel = [slot * 10, 255 - slot * 10, slot * 5, 255];
        let border = match slot % 3 {
            0 => BorderColor::TransparentBlack,
            1 => BorderColor::OpaqueBlack,
            _ => BorderColor::OpaqueWhite,
        };
        let clamp = slot % 2 != 0;
        let sampler = device
            .sampler(&SamplerDesc {
                min: Filter::Nearest,
                mag: Filter::Nearest,
                mip: Filter::Nearest,
                address: [if clamp {
                    Address::Clamp
                } else {
                    Address::Border
                }; 3],
                border,
                lod_bias: slot as i16,
                lod_min: 0,
                lod_max: 0,
                anisotropy: 1,
                compare: None,
            })
            .unwrap();
        drop(encoder.render_pass(&pass(
            texture.view(),
            Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            Load::Clear(texel.map(|v| f64::from(v) / 255.0)),
            Store::Store,
            None,
        )));
        let outside = if clamp {
            texel
        } else {
            match border {
                BorderColor::TransparentBlack => [0; 4],
                BorderColor::OpaqueBlack => [0, 0, 0, 255],
                BorderColor::OpaqueWhite => [255; 4],
            }
        };
        for channel in 0..4 {
            expected[channel] += (f64::from(texel[channel]) + f64::from(outside[channel]))
                * f64::from(slot + 1)
                / 272.0;
        }
        textures.push(texture);
        samplers.push(sampler);
    }
    {
        let mut render = encoder.render_pass(&pass(
            image.view(),
            Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 4,
            },
            Load::DontCare,
            Store::Store,
            None,
        ));
        render.set_pipeline(&pipeline);
        for slot in 0..16 {
            render.bind(slot as u32, Bind::Sampled(textures[slot].view()));
            render.bind(slot as u32, Bind::Sampler(&samplers[slot]));
        }
        render.draw(0..3, 0..1);
        // Unchanged bindings do not require another push-constant command.
        render.draw(0..3, 0..1);
    }
    let results = readback(&device, 64);
    read_image(&mut encoder, &image, 0, 0, size, &results);
    finish(&mut queue, encoder);
    for pixel in bytes(&results).as_chunks::<4>().0 {
        for channel in 0..4 {
            assert!(
                pixel[channel].abs_diff(expected[channel].round() as u8) <= 1,
                "{pixel:?} != {expected:?}"
            );
        }
    }
}

#[test]
fn bindless_storage_offsets_index_by_array_stride() {
    let (device, mut queue) = gpu!(Modern::empty());
    if !device.caps().modern.contains(Modern::BINDLESS) {
        return;
    }
    let shader = device
        .create_shader(ShaderDesc {
            source: Source::Wgsl(
                "
        @group(3) @binding(0) var<storage,read_write> values:array<array<u32,3>>;
        @compute @workgroup_size(1) fn main() {values[0][0]=7u;}
    ",
            ),
            identity: 113,
            label: "storage alignment",
        })
        .unwrap();
    let pipeline = device
        .create_compute_pipeline(
            &ComputePipelineDesc {
                entry: shader.entry("main", Stage::Compute).unwrap(),
                bindings: vec![Binding {
                    slot: 0,
                    kind: BindingKind::StorageBuffer { writable: true },
                    stages: Stages::COMPUTE,
                }],
                label: "storage alignment",
            },
            None,
        )
        .unwrap();
    let alignment = u64::from(device.caps().limits.storage_offset_alignment);
    let offset = alignment * 3;
    let buffer = device
        .create_buffer(&BufferDesc {
            size: offset + 12,
            usage: BufferUsage::STORAGE | BufferUsage::COPY_SRC,
            memory: Memory::Upload,
            label: "aligned storage",
        })
        .unwrap();
    // SAFETY: The new buffer has no GPU users.
    unsafe {
        (&mut *buffer.mapped().unwrap().as_ptr()).fill(0);
    }
    let mut encoder = queue.encoder("aligned storage offset");
    {
        let mut pass = encoder.compute_pass(&ComputePassDesc {
            timestamps: None,
            label: "alignment",
        });
        pass.set_pipeline(&pipeline);
        pass.bind(0, Bind::StorageBuffer(buffer.slice(offset..offset + 1)));
        pass.dispatch(1, 1, 1);
    }
    let output = readback(&device, offset + 12);
    encoder.copy_buffer(buffer.slice(0..offset + 12), &output, 0);
    finish(&mut queue, encoder);
    for (i, word) in bytes(&output).as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(
            u32::from_ne_bytes(*word),
            if i as u64 * 4 == offset { 7 } else { 0 }
        );
    }
}

#[test]
fn storage_slice_struct_atomics_keep_dispatch_offsets() {
    for disable in [
        Modern::empty(),
        Modern::PUSH_DESCRIPTORS,
        Modern::BINDLESS,
        Modern::BINDLESS | Modern::PUSH_DESCRIPTORS,
    ] {
        let (device, mut queue) = gpu!(disable);
        let stride = u64::from(device.caps().limits.storage_offset_alignment).max(16);
        let buffer = |size, label| {
            device
                .create_buffer(&BufferDesc {
                    size,
                    usage: BufferUsage::STORAGE | BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
                    memory: Memory::Device,
                    label,
                })
                .unwrap()
        };
        // Keep descriptor zero occupied: heap atomics must address the bound buffer.
        let zero = buffer(stride * 5, "descriptor zero");
        let records = buffer(stride * 5, "atomic records");
        let output = buffer(64 * 4, "atomic returns");
        if device.caps().modern.contains(Modern::BINDLESS) {
            assert_eq!(zero.inner.0.storage, Some(0));
            assert!(records.inner.0.storage.unwrap() >= 1);
        }
        let bindings: Vec<_> = (0..2)
            .map(|slot| Binding {
                slot,
                kind: BindingKind::StorageBuffer { writable: true },
                stages: Stages::COMPUTE,
            })
            .collect();
        let pipeline = |source| {
            let compiled = crate::shader::precompile(source, &bindings).unwrap();
            let shader = device
                .create_shader(ShaderDesc {
                    source: Source::Precompiled {
                        spirv: &compiled.spirv,
                        metal: &[],
                        entries: &[],
                    },
                    identity: compiled.identity,
                    label: "sliced storage",
                })
                .unwrap();
            device
                .create_compute_pipeline(
                    &ComputePipelineDesc {
                        entry: shader.entry("main", Stage::Compute).unwrap(),
                        bindings: bindings.clone(),
                        label: "sliced storage",
                    },
                    None,
                )
                .unwrap()
        };
        let atomic = pipeline(
            "struct Record { x: atomic<u32>, y: u32, z: u32, count: u32 }
             @group(3) @binding(0) var<storage, read_write> output: array<u32>;
             @group(3) @binding(1) var<storage, read_write> record: Record;
             @compute @workgroup_size(64)
             fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                 output[id.x] = atomicAdd(&record.x, 1u);
             }",
        );
        let plain = pipeline(
            "struct Record { x: u32, y: u32, z: u32, count: u32 }
             @group(3) @binding(1) var<storage, read_write> record: Record;
             @compute @workgroup_size(1) fn main() { record.y = 7u; }",
        );
        let result = readback(&device, stride * 10 + 64 * 4);
        let mut encoder = queue.encoder("sliced storage atomics");
        encoder.fill_buffer(zero.slice(0..stride * 5), 0);
        encoder.fill_buffer(records.slice(0..stride * 5), 0);
        {
            let mut pass = encoder.compute_pass(&ComputePassDesc {
                timestamps: None,
                label: "sliced storage atomics",
            });
            pass.bind(0, Bind::StorageBuffer(output.slice(0..64 * 4)));
            for record in [1, 2, 3, 4, 1] {
                let offset = stride * record;
                pass.set_pipeline(&plain);
                pass.bind(1, Bind::StorageBuffer(records.slice(offset..offset + 16)));
                pass.dispatch(1, 1, 1);
                // Slots persist when switching between heap and ordinary layouts.
                pass.set_pipeline(&atomic);
                pass.dispatch(1, 1, 1);
            }
        }
        encoder.copy_buffer(records.slice(0..stride * 5), &result, 0);
        encoder.copy_buffer(zero.slice(0..stride * 5), &result, stride * 5);
        encoder.copy_buffer(output.slice(0..64 * 4), &result, stride * 10);
        finish(&mut queue, encoder);
        let words: Vec<_> = bytes(&result)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
        for (i, &value) in words[..(stride * 10 / 4) as usize].iter().enumerate() {
            let offset = i as u64 * 4;
            let record = offset / stride;
            let expected = match (record, offset % stride) {
                (1, 0) => 128,
                (2..=4, 0) => 64,
                (1..=4, 4) => 7,
                _ => 0,
            };
            assert_eq!(value, expected, "{disable:?}, byte {offset}");
        }
        let mut returns = words[(stride * 10 / 4) as usize..].to_vec();
        returns.sort_unstable();
        assert_eq!(returns, (64..128).collect::<Vec<_>>());
    }
}

#[test]
fn sampled_heap_entries_are_taken_on_first_sample() {
    let (device, mut queue) = gpu!(Modern::empty());
    if !device.caps().modern.contains(Modern::BINDLESS) {
        return;
    }
    let heap = device.inner.0.heap.as_ref().unwrap();
    let before = heap.used(0);
    let size = Extent {
        width: 8,
        height: 8,
        depth_or_layers: 1,
    };
    let image = create_image(&device, Format::Bgra8Unorm, size, 4, 1);
    let levels: Vec<_> = (0..4)
        .map(|mip| {
            image
                .create_view(&ViewDesc {
                    format: None,
                    dimension: None,
                    mips: mip..mip + 1,
                    layers: 0..1,
                    plane: 0,
                })
                .unwrap()
        })
        .collect();
    // Copy and render views never reach a shader's heap index.
    assert_eq!(heap.used(0), before);
    let index = levels[0].inner.0.sampled_index().unwrap();
    assert_eq!(levels[0].inner.0.sampled_index().unwrap(), index);
    assert_eq!(heap.used(0), before + 1);
    drop(levels);
    drop(image);
    queue.poll().unwrap();
    assert_eq!(heap.used(0), before);
}

#[test]
fn retired_submissions_reuse_one_command_pool_and_its_buffers() {
    let (device, mut queue) = gpu!(Modern::empty());
    let size = Extent {
        width: 4,
        height: 4,
        depth_or_layers: 1,
    };
    let image = create_image(&device, Format::Bgra8Unorm, size, 1, 1);
    let area = Rect {
        x: 0,
        y: 0,
        w: 4,
        h: 4,
    };
    for _ in 0..32 {
        let mut encoder = queue.encoder("passes");
        for _ in 0..3 {
            drop(encoder.render_pass(&pass(
                image.view(),
                area,
                Load::Clear([0.0; 4]),
                Store::Store,
                None,
            )));
        }
        finish(&mut queue, encoder);
        queue.poll().unwrap();
    }
    // An abandoned encoder returns its pool unsubmitted.
    drop(queue.encoder("abandoned"));
    let state = queue.inner.shared.state.lock().unwrap();
    assert_eq!(state.pools.len(), 1);
    assert!(state.pools[0].buffers.len() <= 4);
}

// spec: HOST-VIEW-10
#[test]
fn precompiled_workgroup_atomic_arrays_start_zero_after_round_trip() {
    let (device, mut queue) = gpu!(Modern::empty());
    let bindings = vec![Binding {
        slot: 0,
        kind: BindingKind::StorageBuffer { writable: true },
        stages: Stages::COMPUTE,
    }];
    let compiled = crate::shader::precompile(
        "var<workgroup> values: array<atomic<u32>, 4>;
         @group(3) @binding(0) var<storage, read_write> output: array<u32>;
         @compute @workgroup_size(4)
         fn main(@builtin(local_invocation_index) lane: u32, @builtin(workgroup_id) group: vec3<u32>) {
             output[group.x * 4u + lane] = atomicLoad(&values[lane]);
             workgroupBarrier();
             atomicStore(&values[lane], 0xffffffffu);
         }",
        &bindings,
    ).unwrap();
    let shader = device
        .create_shader(ShaderDesc {
            source: Source::Precompiled {
                spirv: &compiled.spirv,
                metal: &[],
                entries: &[],
            },
            identity: compiled.identity,
            label: "workgroup initialization",
        })
        .unwrap();
    let pipeline = device
        .create_compute_pipeline(
            &ComputePipelineDesc {
                entry: shader.entry("main", Stage::Compute).unwrap(),
                bindings,
                label: "workgroup initialization",
            },
            None,
        )
        .unwrap();
    let output = device
        .create_buffer(&BufferDesc {
            size: 256,
            usage: BufferUsage::STORAGE | BufferUsage::COPY_SRC,
            memory: Memory::Device,
            label: "workgroup initialization output",
        })
        .unwrap();
    let result = readback(&device, 256);
    for _ in 0..2 {
        let mut encoder = queue.encoder("workgroup initialization");
        {
            let mut pass = encoder.compute_pass(&ComputePassDesc {
                timestamps: None,
                label: "workgroup initialization",
            });
            pass.set_pipeline(&pipeline);
            pass.bind(0, Bind::StorageBuffer(output.slice(0..256)));
            pass.dispatch(16, 1, 1);
        }
        encoder.copy_buffer(output.slice(0..256), &result, 0);
        finish(&mut queue, encoder);
        assert!(bytes(&result).iter().all(|&byte| byte == 0));
    }
}
