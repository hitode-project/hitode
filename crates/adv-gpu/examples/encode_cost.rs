//! CPU cost of the encode hot path: nanoseconds per `bind`×4 + `draw`.
//!
//! Each draw binds a ring-style uniform slice and storage slice (offsets
//! change every draw), one of two textures and a sampler, as a D3D9 front end
//! does. `switch` also changes pipeline every four draws. Only recording is
//! timed; submission and GPU work are not.
//!
//! `cargo run --release -p adv-gpu --example encode_cost`; set
//! `ADV_GPU_TEST_DISABLE=all` for the compatibility tier.

use adv_gpu::caps::{Features, FormatCaps};
use adv_gpu::encode::{Bind, ColorAttachment, ImageCopy, ImageCopyRegion, Load, Store};
use adv_gpu::pipeline::{
    Binding, BindingKind, ColorTarget, Cull, Primitive, RenderPipelineDesc, Stages, Topology,
};
use adv_gpu::present::PoolDesc;
use adv_gpu::queue::{DeviceOptions, QueryKind};
use adv_gpu::resource::{
    Address, BorderColor, BufferDesc, BufferUsage, Dimension, Filter, ImageDesc, Memory,
    SamplerDesc, Usage,
};
use adv_gpu::shader::{ShaderDesc, Source, Stage};
use adv_gpu::{Device, Extent, Format, Rect, RenderPassDesc};
use arrayvec::ArrayVec;
use std::time::{Duration, Instant};

const DRAWS: u32 = 10_000;
const ROUNDS: usize = 9;

const SHADER: &str = "
struct Block { tint: vec4<f32>, unused: vec4<f32> }
@group(0) @binding(0) var<uniform> block: Block;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(2) @binding(0) var nearest: sampler;
@group(3) @binding(0) var<storage, read> positions: array<vec4<f32>>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    return positions[i];
}
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureSample(image, nearest, p.xy / 8.0) * block.tint;
}
";

fn main() {
    let (device, mut queue) = Device::open(&DeviceOptions {
        software: !cfg!(any(target_vendor = "apple", target_os = "android")),
        validation: false,
        disable: adv_gpu::caps::Modern::from_test_env(),
        ring_size: 4 << 20,
        label: "encode cost",
        ..Default::default()
    })
    .expect("open device");
    let caps = device.caps();
    println!("{} / {:?}", caps.adapter, caps.modern);
    let size = Extent {
        width: 8,
        height: 8,
        depth_or_layers: 1,
    };
    let image = |usage| {
        device
            .create_image(&ImageDesc {
                dimension: Dimension::D2,
                size,
                format: Format::Rgba8Unorm,
                mips: 1,
                samples: 1,
                usage,
                view_formats: &[],
                label: "image",
            })
            .expect("image")
    };
    let target = image(Usage::COLOR);
    let textures = [image(Usage::SAMPLED), image(Usage::SAMPLED)];
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
        .expect("sampler");
    let uniform_step = u64::from(caps.limits.uniform_offset_alignment).max(32);
    let storage_step = u64::from(caps.limits.storage_offset_alignment).max(48);
    let buffer = |usage, step: u64| {
        device
            .create_buffer(&BufferDesc {
                size: step * 64,
                usage,
                memory: Memory::Upload,
                label: "stream",
            })
            .expect("buffer")
    };
    let uniforms = buffer(BufferUsage::UNIFORM, uniform_step);
    let vertices = buffer(BufferUsage::STORAGE, storage_step);
    let shader = device
        .create_shader(ShaderDesc {
            source: Source::Wgsl(SHADER),
            identity: 1,
            label: "encode cost",
        })
        .expect("shader");
    let pipeline = |cull| {
        let mut colors = ArrayVec::new();
        colors.push(ColorTarget {
            format: Format::Rgba8Unorm,
            blend: None,
            write_mask: 0xf,
        });
        device
            .create_render_pipeline(
                &RenderPipelineDesc {
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
                        cull,
                        front_ccw: false,
                        wireframe: false,
                        unclipped_depth: false,
                    },
                    colors,
                    depth_stencil: None,
                    samples: 1,
                    alpha_to_coverage: false,
                    label: "encode cost",
                },
                None,
            )
            .expect("pipeline")
    };
    let pipelines = [pipeline(Cull::None), pipeline(Cull::Back)];
    for (name, switch) in [("steady", false), ("switch", true)] {
        let mut samples = Vec::with_capacity(ROUNDS);
        for _ in 0..ROUNDS {
            let mut encoder = queue.encoder("encode cost");
            let start = Instant::now();
            {
                let mut colors = ArrayVec::new();
                colors.push(ColorAttachment {
                    view: target.view(),
                    load: Load::DontCare,
                    store: Store::Store,
                    resolve: None,
                });
                let mut pass = encoder.render_pass(&RenderPassDesc {
                    colors,
                    depth_stencil: None,
                    area: Rect {
                        x: 0,
                        y: 0,
                        w: 8,
                        h: 8,
                    },
                    timestamps: None,
                    label: "encode cost",
                });
                pass.set_pipeline(&pipelines[0]);
                for i in 0..DRAWS {
                    if switch && i % 4 == 0 {
                        pass.set_pipeline(&pipelines[(i / 4 % 2) as usize]);
                    }
                    let n = u64::from(i % 64);
                    let u = n * uniform_step;
                    let v = n * storage_step;
                    pass.bind(0, Bind::Uniform(uniforms.slice(u..u + 32)));
                    pass.bind(0, Bind::StorageBuffer(vertices.slice(v..v + 48)));
                    pass.bind(0, Bind::Sampled(textures[(i / 2 % 2) as usize].view()));
                    pass.bind(0, Bind::Sampler(&sampler));
                    pass.draw(0..3, 0..1);
                }
            }
            samples.push(start.elapsed());
            let token = queue.submit(encoder).expect("submit");
            assert!(queue.wait(token, Duration::from_secs(60)).expect("wait"));
        }
        samples.sort();
        let ns = |d: Duration| d.as_nanos() as f64 / f64::from(DRAWS);
        println!(
            "{name}: {:.0} ns/draw min, {:.0} ns/draw median",
            ns(samples[0]),
            ns(samples[ROUNDS / 2])
        );
    }

    // Dirty-rect microbenchmarks
    let big = Extent {
        width: 1280,
        height: 720,
        depth_or_layers: 1,
    };
    let big_image = |usage| {
        device
            .create_image(&ImageDesc {
                dimension: Dimension::D2,
                size: big,
                format: Format::Rgba8Unorm,
                mips: 1,
                samples: 1,
                usage,
                view_formats: &[],
                label: "dirty bench",
            })
            .expect("big image")
    };
    let dirty_dst = big_image(Usage::COLOR | Usage::COPY_SRC | Usage::COPY_DST);
    let dirty_src = big_image(Usage::COLOR | Usage::COPY_SRC | Usage::COPY_DST);

    // 1. Partial-area Load::DontCare pass (single rect, distant corner rects, 64 fragmented rects).
    const PASS_ITERS: u32 = 200;
    let partial_cases: [(&str, Rect, u32); 3] = [
        (
            "partial_dont_care_single",
            Rect {
                x: 100,
                y: 100,
                w: 64,
                h: 64,
            },
            1,
        ),
        (
            "partial_dont_care_corners",
            Rect {
                x: 0,
                y: 0,
                w: 1280,
                h: 720,
            },
            2,
        ),
        (
            "partial_dont_care_64rects",
            Rect {
                x: 0,
                y: 0,
                w: 1024,
                h: 512,
            },
            64,
        ),
    ];
    for (name, area, instances) in partial_cases {
        let mut samples = Vec::with_capacity(ROUNDS);
        for _ in 0..ROUNDS {
            let mut encoder = queue.encoder(name);
            let start = Instant::now();
            for _ in 0..PASS_ITERS {
                let mut colors = ArrayVec::new();
                colors.push(ColorAttachment {
                    view: dirty_dst.view(),
                    load: Load::DontCare,
                    store: Store::Store,
                    resolve: None,
                });
                let mut pass = encoder.render_pass(&RenderPassDesc {
                    colors,
                    depth_stencil: None,
                    area,
                    timestamps: None,
                    label: name,
                });
                pass.set_pipeline(&pipelines[0]);
                pass.bind(0, Bind::Uniform(uniforms.slice(0..32)));
                pass.bind(0, Bind::StorageBuffer(vertices.slice(0..48)));
                pass.bind(0, Bind::Sampled(textures[0].view()));
                pass.bind(0, Bind::Sampler(&sampler));
                pass.draw(0..3, 0..instances);
            }
            samples.push(start.elapsed());
            let token = queue.submit(encoder).expect("submit");
            assert!(queue.wait(token, Duration::from_secs(60)).expect("wait"));
        }
        samples.sort();
        let us = |d: Duration| d.as_secs_f64() * 1e6 / f64::from(PASS_ITERS);
        println!(
            "{name}: {:.2} µs/pass min, {:.2} µs/pass median",
            us(samples[0]),
            us(samples[ROUNDS / 2])
        );
    }

    // 2. 32 disjoint copy_image vs copy_image_regions.
    let copy_regions: Vec<ImageCopyRegion> = (0..32u32)
        .map(|i| ImageCopyRegion {
            src_mip: 0,
            src_layer: 0,
            src_origin: ((i % 8) * 64, (i / 8) * 64, 0),
            dst_mip: 0,
            dst_layer: 0,
            dst_origin: ((i % 8) * 64, (i / 8) * 64, 0),
            size: Extent {
                width: 32,
                height: 32,
                depth_or_layers: 1,
            },
        })
        .collect();
    for (name, batched) in [
        ("copy_32_individual", false),
        ("copy_32_batched_regions", true),
    ] {
        let mut samples = Vec::with_capacity(ROUNDS);
        for _ in 0..ROUNDS {
            let mut encoder = queue.encoder(name);
            let start = Instant::now();
            for _ in 0..PASS_ITERS {
                if batched {
                    encoder.copy_image_regions(&dirty_src, &dirty_dst, &copy_regions);
                } else {
                    for r in &copy_regions {
                        encoder.copy_image(
                            ImageCopy {
                                image: &dirty_src,
                                mip: r.src_mip,
                                layer: r.src_layer,
                                origin: r.src_origin,
                            },
                            ImageCopy {
                                image: &dirty_dst,
                                mip: r.dst_mip,
                                layer: r.dst_layer,
                                origin: r.dst_origin,
                            },
                            r.size,
                        );
                    }
                }
            }
            samples.push(start.elapsed());
            let token = queue.submit(encoder).expect("submit");
            assert!(queue.wait(token, Duration::from_secs(60)).expect("wait"));
        }
        samples.sort();
        let us = |d: Duration| d.as_secs_f64() * 1e6 / f64::from(PASS_ITERS);
        println!(
            "{name}: {:.2} µs/32-copies min, {:.2} µs/32-copies median",
            us(samples[0]),
            us(samples[ROUNDS / 2])
        );
    }

    // 3. End-to-end Ring::reserve + render_pass + submit_present (with vs without timestamps).
    if let Some(format) = [Format::Rgba8Unorm, Format::Bgra8Unorm]
        .into_iter()
        .find(|&f| caps.format(f).contains(FormatCaps::PRESENTABLE))
    {
        let mut colors = ArrayVec::new();
        colors.push(ColorTarget {
            format,
            blend: None,
            write_mask: 0xf,
        });
        let present_pipeline = device
            .create_render_pipeline(
                &RenderPipelineDesc {
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
                    colors,
                    depth_stencil: None,
                    samples: 1,
                    alpha_to_coverage: false,
                    label: "present bench",
                },
                None,
            )
            .expect("present pipeline");
        let mut ring = device.ring(1 << 20).expect("ring");
        let mut pool = device
            .create_present_pool(&PoolDesc {
                color: None,
                width: 1280,
                height: 720,
                format,
                count: 3,
                storage: false,
                label: "e2e bench",
            })
            .expect("pool");
        let has_ts = caps.features.contains(Features::TIMESTAMPS);
        let qset = has_ts.then(|| {
            device
                .create_query_set(QueryKind::Timestamp, 2)
                .expect("qset")
        });
        let ts_buf = device
            .create_buffer(&BufferDesc {
                size: 16,
                usage: BufferUsage::COPY_DST,
                memory: Memory::Readback,
                label: "ts readback",
            })
            .expect("ts buf");
        for (name, with_ts) in [
            ("e2e_present_no_timestamps", false),
            ("e2e_present_with_timestamps", true),
        ] {
            if with_ts && !has_ts {
                continue;
            }
            let mut cpu = Vec::with_capacity(60);
            let mut e2e = Vec::with_capacity(60);
            for _ in 0..60 {
                let start = Instant::now();
                let mut image = pool
                    .acquire(&mut queue)
                    .expect("acquire")
                    .expect("free image");
                let mut span = ring.reserve(256, 256).expect("reserve");
                span.bytes()[..32].fill(0);
                let mut encoder = queue.encoder(name);
                {
                    let mut colors = ArrayVec::new();
                    colors.push(ColorAttachment {
                        view: image.image().view(),
                        load: Load::DontCare,
                        store: Store::Store,
                        resolve: None,
                    });
                    let mut pass = encoder.render_pass(&RenderPassDesc {
                        colors,
                        depth_stencil: None,
                        area: Rect {
                            x: 0,
                            y: 0,
                            w: 64,
                            h: 64,
                        },
                        timestamps: with_ts.then(|| (qset.as_ref().unwrap(), 0)),
                        label: name,
                    });
                    pass.set_pipeline(&present_pipeline);
                    pass.bind(
                        0,
                        Bind::Uniform(span.buffer().slice(span.offset()..span.offset() + 32)),
                    );
                    pass.bind(0, Bind::StorageBuffer(vertices.slice(0..48)));
                    pass.bind(0, Bind::Sampled(textures[0].view()));
                    pass.bind(0, Bind::Sampler(&sampler));
                    pass.draw(0..3, 0..1);
                }
                encoder.retain(span);
                if with_ts {
                    encoder.resolve_queries(qset.as_ref().unwrap(), 0..2, &ts_buf, 0);
                }
                let token = queue
                    .submit_present(encoder, &mut [&mut image])
                    .expect("submit_present");
                cpu.push(start.elapsed());
                assert!(queue.wait(token, Duration::from_secs(10)).expect("wait"));
                e2e.push(start.elapsed());
                image.acquire_fence();
                image.released(None);
            }
            cpu.sort();
            e2e.sort();
            let us = |d: Duration| d.as_secs_f64() * 1e6;
            println!(
                "{name}: CPU {:.1} µs median, wait-to-done {:.1} µs median",
                us(cpu[cpu.len() / 2]),
                us(e2e[e2e.len() / 2])
            );
        }
    }
}
