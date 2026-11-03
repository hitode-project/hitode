//! Cost of drawing into presentable buffers against private images.
//!
//! Each frame draws one full-target triangle pair of constant color (the
//! compressible case) at 2560×1440. CPU: `acquire + submit_present` against a
//! plain `submit` of the same pass. GPU: pass timestamps of both, which show
//! whether presentable buffers keep framebuffer compression.
//!
//! `cargo run --release -p adv-gpu --example present_cost`

use adv_gpu::caps::{Features, FormatCaps};
use adv_gpu::encode::{ColorAttachment, Load, Store};
use adv_gpu::pipeline::{ColorTarget, Cull, Primitive, RenderPipelineDesc, Topology};
use adv_gpu::present::PoolDesc;
use adv_gpu::queue::{DeviceOptions, QueryKind};
use adv_gpu::resource::{BufferDesc, BufferUsage, Dimension, ImageDesc, Memory, Usage};
use adv_gpu::shader::{ShaderDesc, Source, Stage};
use adv_gpu::{Device, Extent, Format, ImageView, Rect, RenderPassDesc};
use arrayvec::ArrayVec;
use std::time::{Duration, Instant};

const WIDTH: u32 = 2560;
const HEIGHT: u32 = 1440;
const FRAMES: usize = 300;

const SHADER: &str = "
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.5, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(0.2, 0.4, 0.6, 1.0); }
";

fn main() {
    let (device, mut queue) = Device::open(&DeviceOptions {
        ring_size: 1 << 20,
        label: "present cost",
        ..DeviceOptions::default()
    })
    .expect("open device");
    let caps = device.caps();
    println!(
        "{} ({}), tile {:?}",
        caps.adapter, caps.driver, caps.limits.tile
    );
    let Some(format) = [Format::Rgba8Unorm, Format::Bgra8Unorm]
        .into_iter()
        .find(|&f| caps.format(f).contains(FormatCaps::PRESENTABLE))
    else {
        println!("no presentable buffers");
        return;
    };
    println!(
        "{format:?}, compressed: {}",
        caps.format(format)
            .contains(FormatCaps::PRESENTABLE_COMPRESSED)
    );
    let timestamps = caps.features.contains(Features::TIMESTAMPS);
    let shader = device
        .create_shader(ShaderDesc {
            source: Source::Wgsl(SHADER),
            identity: 1,
            label: "present cost",
        })
        .expect("shader");
    let mut colors = ArrayVec::new();
    colors.push(ColorTarget {
        format,
        blend: None,
        write_mask: 0xf,
    });
    let pipeline = device
        .create_render_pipeline(
            &RenderPipelineDesc {
                vertex: shader.entry("vs", Stage::Vertex).unwrap(),
                fragment: Some(shader.entry("fs", Stage::Fragment).unwrap()),
                bindings: vec![],
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
                label: "present cost",
            },
            None,
        )
        .expect("pipeline");
    let set = device
        .create_query_set(QueryKind::Timestamp, 2)
        .expect("query set");
    let results = device
        .create_buffer(&BufferDesc {
            size: 16,
            usage: BufferUsage::COPY_DST,
            memory: Memory::Readback,
            label: "timestamps",
        })
        .expect("results");
    let draw = |encoder: &mut adv_gpu::Encoder, view: &ImageView| {
        let mut colors = ArrayVec::new();
        colors.push(ColorAttachment {
            view,
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
                w: WIDTH,
                h: HEIGHT,
            },
            timestamps: timestamps.then_some((&set, 0)),
            label: "present cost",
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
        drop(pass);
        if timestamps {
            encoder.resolve_queries(&set, 0..2, &results, 0);
        }
    };
    let gpu_time = |queue: &mut adv_gpu::Queue, token| {
        assert!(queue.wait(token, Duration::from_secs(10)).expect("wait"));
        let bytes = unsafe { &*results.mapped().expect("mapped").as_ptr() };
        let tick = |i: usize| u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap());
        Duration::from_nanos(((tick(1) - tick(0)) as f64 * set.period()) as u64)
    };

    let private = device
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: WIDTH,
                height: HEIGHT,
                depth_or_layers: 1,
            },
            format,
            mips: 1,
            samples: 1,
            usage: Usage::COLOR | Usage::SAMPLED,
            view_formats: &[],
            label: "private",
        })
        .expect("private image");
    let (mut cpu, mut gpu) = (Vec::new(), Vec::new());
    for _ in 0..FRAMES {
        let mut encoder = queue.encoder("private");
        draw(&mut encoder, private.view());
        let start = Instant::now();
        let token = queue.submit(encoder).expect("submit");
        cpu.push(start.elapsed());
        gpu.push(gpu_time(&mut queue, token));
    }
    report("private", cpu, gpu, timestamps);

    let mut pool = device
        .create_present_pool(&PoolDesc {
            color: None,
            width: WIDTH,
            height: HEIGHT,
            format,
            count: 3,
            storage: false,
            label: "present cost",
        })
        .expect("pool");
    let (mut cpu, mut gpu) = (Vec::new(), Vec::new());
    for _ in 0..FRAMES {
        let start = Instant::now();
        let mut image = pool
            .acquire(&mut queue)
            .expect("acquire")
            .expect("free buffer");
        let acquired = start.elapsed();
        let mut encoder = queue.encoder("presentable");
        draw(&mut encoder, image.image().view());
        let start = Instant::now();
        let token = queue
            .submit_present(encoder, &mut [&mut image])
            .expect("submit");
        cpu.push(acquired + start.elapsed());
        gpu.push(gpu_time(&mut queue, token));
        // No compositor here: the acquire fence, if any, closes unused.
        image.acquire_fence();
        image.released(None);
    }
    report("presentable", cpu, gpu, timestamps);
}

fn report(name: &str, mut cpu: Vec<Duration>, mut gpu: Vec<Duration>, timestamps: bool) {
    cpu.sort();
    gpu.sort();
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    print!(
        "{name}: CPU {:.1} µs min, {:.1} median",
        us(cpu[0]),
        us(cpu[cpu.len() / 2])
    );
    if timestamps {
        print!(
            "; GPU {:.0} µs min, {:.0} median",
            us(gpu[0]),
            us(gpu[gpu.len() / 2])
        );
    }
    println!();
}
