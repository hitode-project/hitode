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

macro_rules! gpu {
    () => {
        test_device!(DeviceOptions::default())
    };
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
    let (device, mut queue) = gpu!();
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
    let (device, mut queue) = gpu!();
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
    shared::<crate::Shader>();
    shared::<crate::RenderPipeline>();
    shared::<crate::ComputePipeline>();
    shared::<crate::pipeline::PipelineCache>();
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
    let (device, mut queue) = gpu!();
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
fn passes_without_work_report_zero_duration() {
    let (device, mut queue) = gpu!();
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
    // A whole-target clear is a load action and an empty compute pass has no
    // dispatch: neither does work, so both report equal timestamps.
    for pair in timestamps.as_chunks::<2>().0 {
        assert_eq!(pair[0], pair[1], "{pair:?}");
    }
}

#[test]
fn multisample_resolves_preserve_pixels_outside_the_area() {
    let (device, mut queue) = gpu!();
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

#[test]
fn passes_with_work_report_their_own_samples() {
    let (device, mut queue) = gpu!();
    if !device.caps().features.contains(Features::TIMESTAMPS) {
        return;
    }
    let image = image(&device, Format::Rgba8Unorm);
    let set = device.create_query_set(QueryKind::Timestamp, 2).unwrap();
    let dst = readback(&device, 16);
    let mut encoder = queue.encoder("timestamps with work");
    // A clear bounded by the area is an internal draw, so the GPU samples it.
    let mut render = color_pass(image.view(), Load::Clear([1.0; 4]), Store::Store, AREA);
    render.timestamps = Some((&set, 0));
    drop(encoder.render_pass(&render));
    encoder.resolve_queries(&set, 0..2, &dst, 0);
    finish(&mut queue, encoder);
    let [start, end] =
        [0, 8].map(|i| u64::from_le_bytes(bytes(&dst)[i..i + 8].try_into().unwrap()));
    assert!(
        start != 0 && start != u64::MAX && end >= start,
        "{start} {end}"
    );
}

const DRAW: &str = "
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.5, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }
";
fn draw_desc(device: &crate::Device) -> RenderPipelineDesc {
    let shader = device
        .create_shader(ShaderDesc {
            source: Source::Wgsl(DRAW),
            identity: 100,
            label: "draw test",
        })
        .unwrap();
    RenderPipelineDesc {
        vertex: shader.entry("vs", Stage::Vertex).unwrap(),
        fragment: Some(shader.entry("fs", Stage::Fragment).unwrap()),
        bindings: vec![],
        primitive: Primitive {
            topology: Topology::Triangles,
            cull: Cull::None,
            front_ccw: true,
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
        label: "draw test",
    }
}
fn upload_words(device: &crate::Device, words: &[u32]) -> crate::Buffer {
    let buffer = device
        .create_buffer(&BufferDesc {
            size: std::mem::size_of_val(words) as u64,
            memory: Memory::Upload,
            usage: BufferUsage::INDEX | BufferUsage::INDIRECT,
            label: "draw arguments",
        })
        .unwrap();
    unsafe {
        std::ptr::copy_nonoverlapping(
            words.as_ptr().cast::<u8>(),
            buffer.mapped().unwrap().as_ptr().cast::<u8>(),
            std::mem::size_of_val(words),
        );
    }
    buffer
}

#[test]
fn all_draw_methods_keep_the_area_and_write_pass_samples() {
    let (device, mut queue) = gpu!();
    let target = image(&device, Format::Rgba8Unorm);
    let pipeline = device
        .create_render_pipeline(&draw_desc(&device), None)
        .unwrap();
    // A prefix tests slice offsets. The indexed draw's start skips another
    // index; base vertex -1 turns [1,2,3] into the full-screen triangle.
    let indices = upload_words(&device, &[9, 9, 1, 2, 3]);
    let args = upload_words(&device, &[99, 3, 1, 0, 2, 3, 1, 1, u32::MAX, 2]);
    let set = device.create_query_set(QueryKind::Occlusion, 4).unwrap();
    let times = device
        .caps()
        .features
        .contains(Features::TIMESTAMPS)
        .then(|| device.create_query_set(QueryKind::Timestamp, 8).unwrap());
    let results = readback(&device, 32);
    let timestamps = readback(&device, 64);
    let pixels = readback(&device, 256);
    let mut encoder = queue.encoder("draw variants");
    drop(encoder.render_pass(&color_pass(
        target.view(),
        Load::Clear([0.0; 4]),
        Store::Store,
        WHOLE,
    )));
    for method in 0..4 {
        let mut desc = color_pass(target.view(), Load::Load, Store::Store, AREA);
        desc.timestamps = times.as_ref().map(|set| (set, method * 2));
        let mut pass = encoder.render_pass(&desc);
        pass.set_pipeline(&pipeline);
        pass.set_scissor(WHOLE);
        pass.set_index_buffer(indices.slice(4..20), IndexFormat::U32);
        pass.begin_occlusion(&set, method);
        match method {
            0 => pass.draw(0..3, 2..3),
            1 => pass.draw_indexed(1..4, -1, 2..3),
            2 => pass.draw_indirect(args.slice(4..20), false),
            _ => pass.draw_indirect(args.slice(20..40), true),
        }
        assert!(pass.inner.work);
        pass.end_occlusion();
    }
    encoder.resolve_queries(&set, 0..4, &results, 0);
    if let Some(set) = &times {
        encoder.resolve_queries(set, 0..8, &timestamps, 0);
    }
    encoder.copy_image_to_buffer(
        ImageCopy {
            image: &target,
            mip: 0,
            layer: 0,
            origin: (0, 0, 0),
        },
        BufferCopy {
            buffer: &pixels,
            offset: 0,
            row_pitch: 0,
            rows: 0,
        },
        target.desc().size,
    );
    finish(&mut queue, encoder);
    for value in bytes(&results).as_chunks::<8>().0 {
        assert_eq!(u64::from_le_bytes(*value), 16);
    }
    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(
                &bytes(&pixels)[(y * 8 + x) * 4..(y * 8 + x + 1) * 4],
                if inside(x, y) { &[255; 4] } else { &[0; 4] }
            );
        }
    }
    if times.is_some() {
        for pair in bytes(&timestamps).as_chunks::<16>().0 {
            let start = u64::from_le_bytes(pair[..8].try_into().unwrap());
            let end = u64::from_le_bytes(pair[8..].try_into().unwrap());
            assert!(
                start > 0 && start != u64::MAX && end >= start,
                "{start} {end}"
            );
        }
    }
}

#[test]
fn compiled_states_share_across_dynamic_and_depth_state_changes() {
    let (device, _) = gpu!();
    let mut desc = draw_desc(&device);
    desc.depth_stencil = Some(DepthStencil {
        format: Format::Depth32FloatStencil8,
        depth_compare: Compare::Always,
        depth_write: false,
        stencil: None,
        stencil_read_mask: 255,
        stencil_write_mask: 255,
        depth_bias: false,
    });
    let first = device.create_render_pipeline(&desc, None).unwrap();
    let identity = device.pipeline_identity(&desc);
    desc.primitive.cull = Cull::Back;
    desc.primitive.topology = Topology::Points;
    desc.primitive.front_ccw = false;
    desc.depth_stencil.as_mut().unwrap().depth_compare = Compare::Less;
    desc.depth_stencil.as_mut().unwrap().depth_bias = true;
    assert_eq!(device.pipeline_identity(&desc), identity);
    let second = device.create_render_pipeline(&desc, None).unwrap();
    assert_eq!(first.inner.0.raw, second.inner.0.raw);
    assert_ne!(first.inner.0.depth, second.inner.0.depth);
    desc.depth_stencil.as_mut().unwrap().depth_bias = false;
    let third = device.create_render_pipeline(&desc, None).unwrap();
    assert_eq!(second.inner.0.depth, third.inner.0.depth);
    desc.colors[0].write_mask = 1;
    assert_ne!(device.pipeline_identity(&desc), identity);
    let fourth = device.create_render_pipeline(&desc, None).unwrap();
    assert_ne!(first.inner.0.raw, fourth.inner.0.raw);
}

#[test]
fn ir_compute_uses_workgroup_dimensions_and_runtime_slice_lengths() {
    let (device, mut queue) = gpu!();
    let source = "
        @group(0) @binding(3) var<uniform> add: vec4<u32>;
        @group(3) @binding(30) var<storage, read> unused: array<vec4<u32>>;
        @group(3) @binding(0) var<storage, read_write> values: array<u32>;
        @group(3) @binding(1) var image: texture_storage_2d<rgba8unorm, write>;
        @compute @workgroup_size(2, 3) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
            let i = id.y * 2u + id.x;
            values[i] = arrayLength(&values) + add.x;
            textureStore(image, vec2<i32>(id.xy), vec4<f32>(1.0));
        }";
    let module = naga::front::wgsl::parse_str(source).unwrap();
    let shader = device
        .create_shader(ShaderDesc {
            source: Source::Ir(Box::new(module)),
            identity: 101,
            label: "IR compute",
        })
        .unwrap();
    assert!(shader.entry("main", Stage::Vertex).is_err());
    let desc = ComputePipelineDesc {
        entry: shader.entry("main", Stage::Compute).unwrap(),
        bindings: vec![
            Binding {
                slot: 3,
                kind: BindingKind::UniformBuffer,
                stages: Stages::COMPUTE,
            },
            Binding {
                slot: 0,
                kind: BindingKind::StorageBuffer { writable: true },
                stages: Stages::COMPUTE,
            },
            Binding {
                slot: 1,
                kind: BindingKind::StorageImage {
                    format: Format::Rgba8Unorm,
                    writable: true,
                },
                stages: Stages::COMPUTE,
            },
        ],
        label: "IR compute",
    };
    let cache = crate::pipeline::PipelineCache::load(&[]);
    let pipeline = device.create_compute_pipeline(&desc, Some(&cache)).unwrap();
    let same = device.create_compute_pipeline(&desc, None).unwrap();
    assert_eq!(pipeline.inner.0.raw, same.inner.0.raw);
    let uniforms = upload_words(&device, &[0, 0, 0, 0, 7, 0, 0, 0]);
    let values = device
        .create_buffer(&BufferDesc {
            size: 64,
            memory: Memory::Readback,
            usage: BufferUsage::STORAGE,
            label: "values",
        })
        .unwrap();
    let image = device
        .create_image(&ImageDesc {
            usage: Usage::STORAGE | Usage::COPY_SRC,
            dimension: Dimension::D2,
            size: Extent {
                width: 8,
                height: 8,
                depth_or_layers: 1,
            },
            format: Format::Rgba8Unorm,
            mips: 1,
            samples: 1,
            view_formats: &[],
            label: "storage image",
        })
        .unwrap();
    let args = upload_words(&device, &[99, 1, 1, 1]);
    let times = device
        .caps()
        .features
        .contains(Features::TIMESTAMPS)
        .then(|| device.create_query_set(QueryKind::Timestamp, 4).unwrap());
    let timestamps = readback(&device, 32);
    let pixels = readback(&device, 256);
    let mut encoder = queue.encoder("compute dimensions");
    for i in 0..2 {
        let mut pass = encoder.compute_pass(&ComputePassDesc {
            timestamps: times.as_ref().map(|set| (set, i * 2)),
            label: "compute",
        });
        pass.set_pipeline(&pipeline);
        pass.bind(3, Bind::Uniform(uniforms.slice(16..32)));
        // Changing only the slice length updates naga's size table without
        // redundantly setting the same native buffer and offset.
        pass.bind(
            0,
            Bind::StorageBuffer(values.slice(i as u64 * 32..i as u64 * 32 + 28)),
        );
        pass.bind(
            0,
            Bind::StorageBuffer(values.slice(i as u64 * 32..i as u64 * 32 + 24)),
        );
        pass.bind(1, Bind::StorageImage(image.view()));
        if i == 0 {
            pass.dispatch(1, 1, 1);
        } else {
            pass.dispatch_indirect(args.slice(4..16));
        }
        assert!(pass.inner.work);
    }
    if let Some(set) = &times {
        encoder.resolve_queries(set, 0..4, &timestamps, 0);
    }
    encoder.copy_image_to_buffer(
        ImageCopy {
            image: &image,
            mip: 0,
            layer: 0,
            origin: (0, 0, 0),
        },
        BufferCopy {
            buffer: &pixels,
            offset: 0,
            row_pitch: 0,
            rows: 0,
        },
        image.desc().size,
    );
    finish(&mut queue, encoder);
    for y in 0..3 {
        for x in 0..2 {
            assert_eq!(
                &bytes(&pixels)[(y * 8 + x) * 4..(y * 8 + x + 1) * 4],
                &[255; 4]
            );
        }
    }
    for offset in [0, 32] {
        for word in bytes(&values)[offset..offset + 24].as_chunks::<4>().0 {
            assert_eq!(u32::from_le_bytes(*word), 13);
        }
    }
    let data = cache.serialize().unwrap();
    let loaded = crate::pipeline::PipelineCache::load(&data);
    // A fresh backend has no in-memory pipeline object and must consult the archive.
    let (another, _) = gpu!();
    another
        .create_compute_pipeline(&desc, Some(&loaded))
        .unwrap();
    if times.is_some() {
        for pair in bytes(&timestamps).as_chunks::<16>().0 {
            let start = u64::from_le_bytes(pair[..8].try_into().unwrap());
            let end = u64::from_le_bytes(pair[8..].try_into().unwrap());
            assert!(
                start > 0 && start != u64::MAX && end >= start,
                "{start} {end}"
            );
        }
    }
}

#[test]
fn precompiled_msl_and_shader_validation() {
    let (device, mut queue) = gpu!();
    let shader = device.create_shader(ShaderDesc { source: Source::Precompiled { spirv: &[u32::MAX], metal: b"#include <metal_stdlib>\nusing namespace metal;\nkernel void main0(device uint* out [[buffer(29)]], uint i [[thread_position_in_grid]]) { out[i] = i + 5; }", entries: &[crate::shader::PrecompiledEntry { name: "main", function: "main0", workgroup: [1, 1, 1], threadgroup: &[] }] }, identity: 102, label: "precompiled" }).unwrap();
    let pipeline = device
        .create_compute_pipeline(
            &ComputePipelineDesc {
                entry: shader.entry("main", Stage::Compute).unwrap(),
                bindings: vec![Binding {
                    slot: 0,
                    kind: BindingKind::StorageBuffer { writable: true },
                    stages: Stages::COMPUTE,
                }],
                label: "precompiled",
            },
            None,
        )
        .unwrap();
    let values = device
        .create_buffer(&BufferDesc {
            size: 16,
            memory: Memory::Readback,
            usage: BufferUsage::STORAGE,
            label: "values",
        })
        .unwrap();
    let mut encoder = queue.encoder("precompiled");
    {
        let mut pass = encoder.compute_pass(&ComputePassDesc {
            timestamps: None,
            label: "precompiled",
        });
        pass.set_pipeline(&pipeline);
        pass.bind(0, Bind::StorageBuffer(values.slice(0..16)));
        pass.dispatch(4, 1, 1);
    }
    finish(&mut queue, encoder);
    for (i, word) in bytes(&values).as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(u32::from_le_bytes(*word), i as u32 + 5);
    }
    assert!(matches!(
        device.create_shader(ShaderDesc {
            source: Source::Wgsl("invalid WGSL"),
            identity: 103,
            label: "invalid"
        }),
        Err(Error::Compile(_))
    ));
    let mut invalid =
        naga::front::wgsl::parse_str("@compute @workgroup_size(1) fn main() {}").unwrap();
    invalid.entry_points[0].workgroup_size = [0; 3];
    assert!(matches!(
        device.create_shader(ShaderDesc {
            source: Source::Ir(Box::new(invalid)),
            identity: 104,
            label: "invalid"
        }),
        Err(Error::Compile(_))
    ));
}
