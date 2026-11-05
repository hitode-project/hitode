//! Shared conformance tests for every `adv-gpu` backend (issue #103).
//!
//! The API freezes once the Vulkan and Metal backends both pass this file.
//! It runs on lavapipe with validation layers in Linux CI, twice: once as is
//! and once with `ADV_GPU_TEST_DISABLE=all`, which masks every modern Vulkan
//! mechanism so the compatibility tier is tested too. On macOS it runs on
//! Metal. Tests for optional features return early when the device lacks
//! them, and every test skips on targets whose backend has not landed (see
//! [`try_gpu`]). Every wait has a timeout, so a broken backend fails instead of
//! hanging.

// Skeleton handles have no drop glue yet; the backends' do. Remove with them.
#![allow(clippy::drop_non_drop)]

use adv_gpu::caps::Modern;
use adv_gpu::encode::{
    Bind, BufferCopy, ColorAttachment, ComputePassDesc, DepthAttachment, ImageCopy, IndexFormat,
    Load, Store,
};
use adv_gpu::pipeline::{
    Binding, BindingKind, BlendComponent, BlendOp, ColorTarget, Compare, ComputePipelineDesc, Cull,
    DepthStencil, Factor, PipelineCache, Primitive, RenderPipelineDesc, Stages, StencilFace,
    StencilOp, Topology,
};
use adv_gpu::queue::{DeviceOptions, QueryKind};
use adv_gpu::resource::{
    Address, BorderColor, BufferDesc, BufferUsage, Dimension, Filter, ImageDesc, Memory,
    SamplerDesc, Usage,
};
use adv_gpu::shader::{ShaderDesc, Source, Stage};
use adv_gpu::{
    Buffer, Device, Error, Extent, Features, Format, FormatCaps, Image, Queue, Rect,
    RenderPassDesc, RenderPipeline, Shader, Token,
};
use arrayvec::ArrayVec;
use std::sync::mpsc;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(20);

struct Gpu {
    device: Device,
    queue: Queue,
}

fn options() -> DeviceOptions {
    DeviceOptions {
        software: !cfg!(any(target_vendor = "apple", target_os = "android")),
        // Shell processes on Android user builds cannot load layers.
        validation: !cfg!(target_os = "android"),
        disable: Modern::from_test_env(),
        ring_size: 4 << 20,
        label: "conformance",
    }
}

/// The device, or `None` when this target has no backend yet. CI sets
/// `ADV_GPU_REQUIRE_BACKEND=1` for every target whose backend has landed, so
/// there a missing device fails instead of skipping.
fn try_gpu() -> Option<Gpu> {
    match Device::open(&options()) {
        Ok((device, queue)) => Some(Gpu { device, queue }),
        Err(Error::Unavailable(reason))
            if std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_none() =>
        {
            eprintln!("skipped: {reason}");
            None
        }
        Err(error) => panic!("open device: {error}"),
    }
}

macro_rules! gpu {
    () => {
        match try_gpu() {
            Some(gpu) => gpu,
            None => return,
        }
    };
}

impl Gpu {
    fn finish(&mut self, token: Token) {
        assert!(
            self.queue.wait(token, TIMEOUT).expect("wait"),
            "submission did not complete within {TIMEOUT:?}"
        );
    }

    fn submit(&mut self, encoder: adv_gpu::Encoder) {
        let token = self.queue.submit(encoder).expect("submit");
        self.finish(token);
    }

    fn target(&self, width: u32, height: u32, format: Format) -> Image {
        self.device
            .create_image(&ImageDesc {
                dimension: Dimension::D2,
                size: extent(width, height),
                format,
                mips: 1,
                samples: 1,
                usage: Usage::COLOR | Usage::SAMPLED | Usage::COPY_SRC | Usage::COPY_DST,
                view_formats: &[],
                label: "target",
            })
            .expect("create target")
    }

    fn depth(&self, width: u32, height: u32, format: Format) -> Image {
        self.device
            .create_image(&ImageDesc {
                dimension: Dimension::D2,
                size: extent(width, height),
                format,
                mips: 1,
                samples: 1,
                usage: Usage::DEPTH_STENCIL,
                view_formats: &[],
                label: "depth",
            })
            .expect("create depth")
    }

    fn buffer(&self, size: u64, usage: BufferUsage, memory: Memory) -> Buffer {
        self.device
            .create_buffer(&BufferDesc {
                size,
                usage,
                memory,
                label: "buffer",
            })
            .expect("create buffer")
    }

    /// An upload buffer holding `bytes`.
    fn upload(&self, bytes: &[u8], usage: BufferUsage) -> Buffer {
        let buffer = self.buffer(bytes.len() as u64, usage, Memory::Upload);
        write(&buffer, 0, bytes);
        buffer
    }

    /// Uniform blocks of 8 floats, one per draw, at the device's alignment.
    fn uniforms(&self, blocks: &[[f32; 8]]) -> (Buffer, u64) {
        let align = u64::from(self.device.caps().limits.uniform_offset_alignment).max(32);
        let buffer = self.buffer(
            align * blocks.len() as u64,
            BufferUsage::UNIFORM,
            Memory::Upload,
        );
        for (i, block) in blocks.iter().enumerate() {
            write(&buffer, align * i as u64, bytemuck_f32(block));
        }
        (buffer, align)
    }

    /// Every texel of mip 0, layer 0, packed by rows.
    fn read(&mut self, image: &Image) -> Vec<u8> {
        let desc = *image.desc();
        let bytes = u64::from(desc.size.width * desc.size.height * texel_bytes(desc.format));
        let readback = self.buffer(bytes, BufferUsage::COPY_DST, Memory::Readback);
        let mut encoder = self.queue.encoder("read");
        encoder.copy_image_to_buffer(
            ImageCopy {
                image,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            BufferCopy {
                buffer: &readback,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            desc.size,
        );
        self.submit(encoder);
        mapped(&readback)[..bytes as usize].to_vec()
    }

    fn read_buffer(&mut self, buffer: &Buffer, size: u64) -> Vec<u8> {
        let readback = self.buffer(size, BufferUsage::COPY_DST, Memory::Readback);
        let mut encoder = self.queue.encoder("read buffer");
        encoder.copy_buffer(buffer.slice(0..size), &readback, 0);
        self.submit(encoder);
        mapped(&readback)[..size as usize].to_vec()
    }

    fn shader(&self, wgsl: &str, identity: u64) -> Shader {
        self.device
            .create_shader(ShaderDesc {
                source: Source::Wgsl(wgsl),
                identity,
                label: "conformance",
            })
            .expect("create shader")
    }

    fn pipeline(&self, desc: &RenderPipelineDesc) -> RenderPipeline {
        self.device
            .create_render_pipeline(desc, None)
            .expect("create render pipeline")
    }

    /// The solid-color pipeline: a full-target triangle at the uniform's depth.
    fn solid_desc(&self, format: Format) -> RenderPipelineDesc {
        let shader = self.shader(SOLID, 1);
        RenderPipelineDesc {
            vertex: shader.entry("vs", Stage::Vertex).expect("vs"),
            fragment: Some(shader.entry("fs", Stage::Fragment).expect("fs")),
            bindings: vec![uniform_binding(0)],
            primitive: primitive(),
            colors: colors(format, None),
            depth_stencil: None,
            samples: 1,
            alpha_to_coverage: false,
            label: "solid",
        }
    }

    fn supports(&self, format: Format, needed: FormatCaps) -> bool {
        self.device.caps().format(format).contains(needed)
    }

    fn has(&self, feature: Features) -> bool {
        self.device.caps().features.contains(feature)
    }
}

/// Full-target triangle; color and depth come from a uniform block.
const SOLID: &str = "
struct Block { color: vec4<f32>, depth: vec4<f32> }
@group(0) @binding(0) var<uniform> block: Block;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, block.depth.x, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return block.color; }
";

/// Positions fetched from a storage buffer: vertex fetch is shader code.
const FETCH: &str = "
@group(3) @binding(0) var<storage, read> positions: array<vec2<f32>>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    return vec4<f32>(positions[i], 0.5, 1.0);
}
@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(0.0, 1.0, 0.0, 1.0); }
";

/// Samples a 2×2 texture across an 8×8 target, tinted by a uniform.
const SAMPLED: &str = "
struct Block { tint: vec4<f32>, unused: vec4<f32> }
@group(0) @binding(0) var<uniform> block: Block;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(2) @binding(0) var nearest: sampler;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.5, 1.0);
}
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureSample(image, nearest, p.xy / 8.0) * block.tint;
}
";

const COMPUTE: &str = "
@group(3) @binding(0) var<storage, read_write> values: array<u32>;
@compute @workgroup_size(64) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    values[id.x] = id.x * 3u + 1u;
}
";

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

fn extent(width: u32, height: u32) -> Extent {
    Extent {
        width,
        height,
        depth_or_layers: 1,
    }
}

fn whole(width: u32, height: u32) -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: width,
        h: height,
    }
}

fn block(color: [f32; 4], depth: f32) -> [f32; 8] {
    [color[0], color[1], color[2], color[3], depth, 0.0, 0.0, 0.0]
}

fn clear(color: [f32; 4]) -> Load<[f64; 4]> {
    Load::Clear(color.map(f64::from))
}

fn rgba8(color: [f32; 4]) -> [u8; 4] {
    color.map(|c| (c * 255.0).round() as u8)
}

fn primitive() -> Primitive {
    Primitive {
        topology: Topology::Triangles,
        cull: Cull::None,
        front_ccw: false,
        wireframe: false,
        unclipped_depth: false,
    }
}

fn colors(
    format: Format,
    blend: Option<(BlendComponent, BlendComponent)>,
) -> ArrayVec<ColorTarget, 8> {
    let mut colors = ArrayVec::new();
    colors.push(ColorTarget {
        format,
        blend,
        write_mask: 0xf,
    });
    colors
}

fn uniform_binding(slot: u32) -> Binding {
    Binding {
        slot,
        kind: BindingKind::UniformBuffer,
        stages: Stages::VERTEX | Stages::FRAGMENT,
    }
}

fn color<'a>(
    view: &'a adv_gpu::ImageView,
    load: Load<[f64; 4]>,
) -> ArrayVec<ColorAttachment<'a>, 8> {
    let mut colors = ArrayVec::new();
    colors.push(ColorAttachment {
        view,
        load,
        store: Store::Store,
        resolve: None,
    });
    colors
}

fn pass<'a>(
    colors: ArrayVec<ColorAttachment<'a>, 8>,
    depth_stencil: Option<DepthAttachment<'a>>,
    area: Rect,
) -> RenderPassDesc<'a> {
    RenderPassDesc {
        colors,
        depth_stencil,
        area,
        timestamps: None,
        label: "conformance",
    }
}

fn texel_bytes(format: Format) -> u32 {
    match format {
        Format::R8Unorm => 1,
        Format::Rg8Unorm | Format::B5g6r5Unorm | Format::Bgr5a1Unorm | Format::Bgra4Unorm => 2,
        Format::R16Float => 2,
        Format::Rgba16Float | Format::Rg32Float => 8,
        Format::Rgba32Float => 16,
        _ => 4,
    }
}

fn mapped(buffer: &Buffer) -> &[u8] {
    let ptr = buffer.mapped().expect("mapped buffer");
    // SAFETY: the mapping lives as long as the buffer, and every caller reads
    // only after the writing submission completed.
    unsafe { &*ptr.as_ptr() }
}

fn write(buffer: &Buffer, offset: u64, bytes: &[u8]) {
    let ptr = buffer.mapped().expect("mapped buffer");
    // SAFETY: no submission that reads this buffer is in flight.
    let all = unsafe { &mut *ptr.as_ptr() };
    all[offset as usize..offset as usize + bytes.len()].copy_from_slice(bytes);
}

fn bytemuck_f32(values: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding and any byte pattern is a valid u8.
    unsafe { std::slice::from_raw_parts(values.as_ptr().cast(), std::mem::size_of_val(values)) }
}

fn pixel(bytes: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

fn inside(rect: Rect, x: u32, y: u32) -> bool {
    x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h
}

/// Draw the solid pipeline once per block into `view`.
fn draw_solid(
    gpu: &mut Gpu,
    pipeline: &RenderPipeline,
    desc: RenderPassDesc<'_>,
    blocks: &[[f32; 8]],
    scissor: Option<Rect>,
) {
    let (uniforms, stride) = gpu.uniforms(blocks);
    let mut encoder = gpu.queue.encoder("solid");
    {
        let mut pass = encoder.render_pass(&desc);
        pass.set_pipeline(pipeline);
        if let Some(rect) = scissor {
            pass.set_scissor(rect);
        }
        for i in 0..blocks.len() as u64 {
            pass.bind(
                0,
                Bind::Uniform(uniforms.slice(i * stride..i * stride + 32)),
            );
            pass.draw(0..3, 0..1);
        }
    }
    gpu.submit(encoder);
}

#[test]
fn opens_with_sane_caps() {
    let gpu = gpu!();
    let caps = gpu.device.caps();
    assert!(caps.limits.max_image_2d >= 4096);
    assert!(caps.limits.max_color_attachments >= 4);
    assert!(caps.limits.uniform_offset_alignment.is_power_of_two());
    assert!(!caps.adapter.is_empty());
    let color = FormatCaps::SAMPLED | FormatCaps::FILTERED | FormatCaps::COLOR | FormatCaps::BLEND;
    assert!(gpu.supports(Format::Rgba8Unorm, color));
    assert!(gpu.supports(Format::Bgra8Unorm, color));
    assert!(
        [Format::Depth32FloatStencil8, Format::Depth24UnormStencil8]
            .iter()
            .any(|&f| gpu.supports(f, FormatCaps::DEPTH_STENCIL)),
        "no depth format with stencil"
    );
    assert!(gpu.device.lost().is_none());
}

#[test]
fn disable_masks_modern_mechanisms() {
    let options = DeviceOptions {
        disable: Modern::all(),
        ..options()
    };
    if try_gpu().is_none() {
        return;
    }
    let (device, _queue) = Device::open(&options).expect("open masked device");
    assert_eq!(device.caps().modern, Modern::empty());
}

#[test]
fn buffer_copy_and_fill() {
    let mut gpu = gpu!();
    let source: Vec<u8> = (0..=255).collect();
    let upload = gpu.upload(&source, BufferUsage::COPY_SRC);
    let device = gpu.buffer(
        512,
        BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
        Memory::Device,
    );
    let mut encoder = gpu.queue.encoder("copy");
    encoder.copy_buffer(upload.slice(0..256), &device, 0);
    encoder.fill_buffer(device.slice(256..512), 0xa5a5_5a5a);
    gpu.submit(encoder);
    let bytes = gpu.read_buffer(&device, 512);
    assert_eq!(&bytes[..256], &source[..]);
    for word in bytes[256..].chunks(4) {
        assert_eq!(u32::from_le_bytes(word.try_into().unwrap()), 0xa5a5_5a5a);
    }
}

#[test]
fn ring_upload_round_trips_through_an_image() {
    let mut gpu = gpu!();
    let image = gpu.target(16, 16, Format::Rgba8Unorm);
    let texels: Vec<u8> = (0..16 * 16 * 4).map(|i| (i * 7 % 251) as u8).collect();
    let mut ring = gpu.device.ring(64 << 10).expect("ring");
    let mut span = ring.reserve(texels.len() as u64, 256).expect("reserve");
    span.bytes()[..texels.len()].copy_from_slice(&texels);
    let mut encoder = gpu.queue.encoder("upload");
    encoder.copy_buffer_to_image(
        BufferCopy {
            buffer: span.buffer(),
            offset: span.offset(),
            row_pitch: 0,
            rows: 0,
        },
        ImageCopy {
            image: &image,
            mip: 0,
            layer: 0,
            origin: (0, 0, 0),
        },
        extent(16, 16),
    );
    encoder.retain(span);
    gpu.submit(encoder);
    assert_eq!(gpu.read(&image), texels);
}

#[test]
fn ring_reports_busy_until_spans_retire() {
    let mut gpu = gpu!();
    let mut ring = gpu.device.ring(64 << 10).expect("ring");
    let span = ring.reserve(48 << 10, 256).expect("first reserve");
    assert_eq!(ring.reserve(32 << 10, 256).err(), Some(Error::Busy));
    let mut encoder = gpu.queue.encoder("retire");
    encoder.retain(span);
    gpu.submit(encoder);
    gpu.queue.poll().expect("poll");
    let span = ring.reserve(32 << 10, 256).expect("reserve after retire");
    assert_eq!(span.len(), 32 << 10);
    // A span dropped unused retires with the next submission.
    drop(span);
    let encoder = gpu.queue.encoder("empty");
    gpu.submit(encoder);
    gpu.queue.poll().expect("poll");
    ring.reserve(60 << 10, 256)
        .expect("reserve after a dropped span");
}

#[test]
fn tokens_are_ordered_and_callbacks_fire() {
    let mut gpu = gpu!();
    let tokens: Vec<Token> = (0..3)
        .map(|_| {
            let encoder = gpu.queue.encoder("empty");
            gpu.queue.submit(encoder).expect("submit")
        })
        .collect();
    assert!(tokens[0] < tokens[1] && tokens[1] < tokens[2]);
    let (sender, receiver) = mpsc::channel();
    let early = sender.clone();
    gpu.queue
        .on_complete(tokens[2], Box::new(move || sender.send(2).unwrap()));
    gpu.finish(tokens[2]);
    assert!(gpu.queue.poll().expect("poll") >= tokens[2]);
    // A callback for a token that already completed still runs.
    gpu.queue
        .on_complete(tokens[0], Box::new(move || early.send(0).unwrap()));
    let mut seen = [
        receiver.recv_timeout(TIMEOUT).expect("callback"),
        receiver.recv_timeout(TIMEOUT).expect("callback"),
    ];
    seen.sort();
    assert_eq!(seen, [0, 2]);
}

#[test]
fn clears_pack_every_color_format_as_documented() {
    let mut gpu = gpu!();
    // Red at full alpha, as each format stores it, little endian.
    let cases: [(Format, &[u8]); 8] = [
        (Format::Rgba8Unorm, &[255, 0, 0, 255]),
        (Format::Bgra8Unorm, &[0, 0, 255, 255]),
        (Format::B5g6r5Unorm, &[0x00, 0xf8]),
        (Format::Bgr5a1Unorm, &[0x00, 0xfc]),
        (Format::Bgra4Unorm, &[0x00, 0xff]),
        (Format::Rgb10a2Unorm, &[0xff, 0x03, 0x00, 0xc0]),
        (Format::Rgba16Float, &[0x00, 0x3c, 0, 0, 0, 0, 0x00, 0x3c]),
        (Format::R32Float, &[0, 0, 0x80, 0x3f]),
    ];
    for (format, expected) in cases {
        if !gpu.supports(format, FormatCaps::COLOR) {
            eprintln!("{format:?}: not a color format on this device");
            continue;
        }
        let image = gpu.target(4, 4, format);
        let mut encoder = gpu.queue.encoder("clear");
        drop(encoder.render_pass(&pass(color(image.view(), clear(RED)), None, whole(4, 4))));
        gpu.submit(encoder);
        let bytes = gpu.read(&image);
        for texel in bytes.chunks(expected.len()) {
            assert_eq!(texel, expected, "{format:?}");
        }
    }
}

#[test]
fn render_area_bounds_loads_clears_draws_and_stores() {
    let mut gpu = gpu!();
    let (w, h) = (32, 32);
    let image = gpu.target(w, h, Format::Rgba8Unorm);
    let pipeline = gpu.pipeline(&gpu.solid_desc(Format::Rgba8Unorm));
    let cleared = Rect {
        x: 8,
        y: 8,
        w: 16,
        h: 16,
    };
    let loaded = Rect {
        x: 0,
        y: 0,
        w: 8,
        h: 8,
    };
    let discarded = Rect {
        x: 24,
        y: 24,
        w: 8,
        h: 8,
    };

    let mut encoder = gpu.queue.encoder("setup");
    drop(encoder.render_pass(&pass(color(image.view(), clear(BLUE)), None, whole(w, h))));
    // A clear limited to the area leaves the rest of the target alone.
    drop(encoder.render_pass(&pass(color(image.view(), clear(GREEN)), None, cleared)));
    gpu.submit(encoder);
    // A draw covering the target changes only the area.
    let desc = pass(color(image.view(), Load::Load), None, loaded);
    draw_solid(&mut gpu, &pipeline, desc, &[block(RED, 0.5)], None);
    // So does a pass that discards its contents first.
    let desc = pass(color(image.view(), Load::DontCare), None, discarded);
    draw_solid(&mut gpu, &pipeline, desc, &[block(WHITE, 0.5)], None);

    let bytes = gpu.read(&image);
    for y in 0..h {
        for x in 0..w {
            let expected = if inside(loaded, x, y) {
                RED
            } else if inside(discarded, x, y) {
                WHITE
            } else if inside(cleared, x, y) {
                GREEN
            } else {
                BLUE
            };
            assert_eq!(pixel(&bytes, w, x, y), rgba8(expected), "({x}, {y})");
        }
    }
}

#[test]
fn scissor_is_clamped_to_the_area() {
    let mut gpu = gpu!();
    let image = gpu.target(16, 16, Format::Rgba8Unorm);
    let pipeline = gpu.pipeline(&gpu.solid_desc(Format::Rgba8Unorm));
    let mut encoder = gpu.queue.encoder("setup");
    drop(encoder.render_pass(&pass(color(image.view(), clear(BLUE)), None, whole(16, 16))));
    gpu.submit(encoder);
    let area = Rect {
        x: 0,
        y: 0,
        w: 8,
        h: 8,
    };
    let scissor = Rect {
        x: 4,
        y: 4,
        w: 12,
        h: 12,
    };
    let desc = pass(color(image.view(), Load::Load), None, area);
    draw_solid(&mut gpu, &pipeline, desc, &[block(RED, 0.5)], Some(scissor));
    let bytes = gpu.read(&image);
    let red = Rect {
        x: 4,
        y: 4,
        w: 4,
        h: 4,
    };
    for y in 0..16 {
        for x in 0..16 {
            let expected = if inside(red, x, y) { RED } else { BLUE };
            assert_eq!(pixel(&bytes, 16, x, y), rgba8(expected), "({x}, {y})");
        }
    }
}

#[test]
fn vertex_fetch_reads_storage_buffers_with_base_vertex() {
    let mut gpu = gpu!();
    let image = gpu.target(8, 8, Format::Rgba8Unorm);
    let shader = gpu.shader(FETCH, 2);
    let pipeline = gpu.pipeline(&RenderPipelineDesc {
        vertex: shader.entry("vs", Stage::Vertex).unwrap(),
        fragment: Some(shader.entry("fs", Stage::Fragment).unwrap()),
        bindings: vec![Binding {
            slot: 0,
            kind: BindingKind::StorageBuffer { writable: false },
            stages: Stages::VERTEX,
        }],
        primitive: primitive(),
        colors: colors(Format::Rgba8Unorm, None),
        depth_stencil: None,
        samples: 1,
        alpha_to_coverage: false,
        label: "fetch",
    });
    // Two junk vertices, then a quad over the left half; base vertex 2 skips the junk.
    let positions: [f32; 12] = [
        9.0, 9.0, 9.0, 9.0, -1.0, -1.0, 0.0, -1.0, -1.0, 1.0, 0.0, 1.0,
    ];
    let vertices = gpu.upload(bytemuck_f32(&positions), BufferUsage::STORAGE);
    let indices: [u16; 6] = [0, 1, 2, 2, 1, 3];
    // SAFETY: u16 has no padding.
    let index_bytes = unsafe { std::slice::from_raw_parts(indices.as_ptr().cast::<u8>(), 12) };
    let index_buffer = gpu.upload(index_bytes, BufferUsage::INDEX);
    let mut encoder = gpu.queue.encoder("fetch");
    {
        let mut pass =
            encoder.render_pass(&pass(color(image.view(), clear(BLUE)), None, whole(8, 8)));
        pass.set_pipeline(&pipeline);
        pass.bind(0, Bind::StorageBuffer(vertices.slice(0..48)));
        pass.set_index_buffer(index_buffer.slice(0..12), IndexFormat::U16);
        pass.draw_indexed(0..6, 2, 0..1);
    }
    gpu.submit(encoder);
    let bytes = gpu.read(&image);
    for y in 0..8 {
        for x in 0..8 {
            let expected = if x < 4 { GREEN } else { BLUE };
            assert_eq!(pixel(&bytes, 8, x, y), rgba8(expected), "({x}, {y})");
        }
    }
}

#[test]
fn bindings_persist_across_pipelines_by_slot() {
    let mut gpu = gpu!();
    let texture = gpu.target(2, 2, Format::Rgba8Unorm);
    let texels: [u8; 16] = [
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
    ];
    let staging = gpu.upload(&texels, BufferUsage::COPY_SRC);
    let image = gpu.target(8, 8, Format::Rgba8Unorm);
    let shader = gpu.shader(SAMPLED, 3);
    let pipeline = gpu.pipeline(&RenderPipelineDesc {
        vertex: shader.entry("vs", Stage::Vertex).unwrap(),
        fragment: Some(shader.entry("fs", Stage::Fragment).unwrap()),
        bindings: vec![
            uniform_binding(0),
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
        ],
        primitive: primitive(),
        colors: colors(Format::Rgba8Unorm, None),
        depth_stencil: None,
        samples: 1,
        alpha_to_coverage: false,
        label: "sampled",
    });
    let sampler = gpu
        .device
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
    let solid = gpu.pipeline(&gpu.solid_desc(Format::Rgba8Unorm));
    // Tint drops green, so the green and white quadrants show what the uniform did.
    let (uniforms, _) = gpu.uniforms(&[block([1.0, 0.0, 1.0, 1.0], 0.5)]);
    let mut encoder = gpu.queue.encoder("sampled");
    encoder.copy_buffer_to_image(
        BufferCopy {
            buffer: &staging,
            offset: 0,
            row_pitch: 0,
            rows: 0,
        },
        ImageCopy {
            image: &texture,
            mip: 0,
            layer: 0,
            origin: (0, 0, 0),
        },
        extent(2, 2),
    );
    {
        let mut pass = encoder.render_pass(&pass(
            color(image.view(), Load::DontCare),
            None,
            whole(8, 8),
        ));
        // Slots bound before any pipeline survive a pipeline that does not
        // read them and reach the next one that does.
        pass.bind(0, Bind::Sampled(texture.view()));
        pass.bind(0, Bind::Sampler(&sampler));
        pass.bind(0, Bind::Uniform(uniforms.slice(0..32)));
        pass.set_pipeline(&solid);
        pass.draw(0..3, 0..1);
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    }
    gpu.submit(encoder);
    let bytes = gpu.read(&image);
    let quadrant = |x: u32, y: u32| {
        [[RED, [0.0, 0.0, 0.0, 1.0]], [BLUE, [1.0, 0.0, 1.0, 1.0]]][(y / 4) as usize]
            [(x / 4) as usize]
    };
    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(pixel(&bytes, 8, x, y), rgba8(quadrant(x, y)), "({x}, {y})");
        }
    }
}

#[test]
fn alpha_blending_mixes_with_the_target() {
    let mut gpu = gpu!();
    let image = gpu.target(4, 4, Format::Rgba8Unorm);
    let mut desc = gpu.solid_desc(Format::Rgba8Unorm);
    let over = BlendComponent {
        src: Factor::SrcAlpha,
        dst: Factor::OneMinusSrcAlpha,
        op: BlendOp::Add,
    };
    let keep_src = BlendComponent {
        src: Factor::One,
        dst: Factor::Zero,
        op: BlendOp::Add,
    };
    desc.colors = colors(Format::Rgba8Unorm, Some((over, keep_src)));
    let pipeline = gpu.pipeline(&desc);
    let desc = pass(color(image.view(), clear(BLUE)), None, whole(4, 4));
    draw_solid(
        &mut gpu,
        &pipeline,
        desc,
        &[block([1.0, 0.0, 0.0, 0.5], 0.5)],
        None,
    );
    for texel in gpu.read(&image).chunks(4) {
        let expected = [128u8, 0, 128, 128];
        for (got, want) in texel.iter().zip(expected) {
            assert!(got.abs_diff(want) <= 1, "{texel:?}");
        }
    }
}

#[test]
fn depth_and_stencil_tests_reject_fragments() {
    let mut gpu = gpu!();
    let format = [Format::Depth32FloatStencil8, Format::Depth24UnormStencil8]
        .into_iter()
        .find(|&f| gpu.supports(f, FormatCaps::DEPTH_STENCIL))
        .expect("depth format with stencil");
    let image = gpu.target(8, 8, Format::Rgba8Unorm);
    let face = |compare, pass| StencilFace {
        compare,
        fail: StencilOp::Keep,
        depth_fail: StencilOp::Keep,
        pass,
    };
    let state = |compare, pass, depth_compare| DepthStencil {
        format,
        depth_compare,
        depth_write: true,
        stencil: Some((face(compare, pass), face(compare, pass))),
        stencil_read_mask: 0xff,
        stencil_write_mask: 0xff,
        depth_bias: false,
    };
    let mut mark = gpu.solid_desc(Format::Rgba8Unorm);
    mark.colors[0].write_mask = 0;
    mark.depth_stencil = Some(state(Compare::Always, StencilOp::Replace, Compare::Always));
    let mark = gpu.pipeline(&mark);
    let mut test = gpu.solid_desc(Format::Rgba8Unorm);
    let mut tested = state(Compare::Equal, StencilOp::Keep, Compare::Less);
    tested.depth_write = false;
    test.depth_stencil = Some(tested);
    let test = gpu.pipeline(&test);

    // Mark the left half with stencil 1 at depth 0.5, then in a second pass
    // draw red at 0.7 (fails depth) and green at 0.3 (passes where marked).
    let (uniforms, stride) = gpu.uniforms(&[block(WHITE, 0.5), block(RED, 0.7), block(GREEN, 0.3)]);
    // A depth attachment larger than the color one serves the color's size.
    for (width, height) in [(8, 8), (12, 10)] {
        let depth = gpu.depth(width, height, format);
        let mut encoder = gpu.queue.encoder("depth");
        {
            let mut pass = encoder.render_pass(&pass(
                color(image.view(), clear(BLUE)),
                Some(DepthAttachment {
                    view: depth.view(),
                    depth: (Load::Clear(1.0), Store::Store),
                    stencil: (Load::Clear(0), Store::Store),
                    read_only: false,
                }),
                whole(8, 8),
            ));
            pass.set_stencil_reference(1);
            pass.set_pipeline(&mark);
            pass.set_scissor(Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 8,
            });
            pass.bind(0, Bind::Uniform(uniforms.slice(0..32)));
            pass.draw(0..3, 0..1);
        }
        // A pass that tests depth and stencil without writing them still
        // binds them (`Store::None`).
        {
            let mut pass = encoder.render_pass(&pass(
                color(image.view(), Load::Load),
                Some(DepthAttachment {
                    view: depth.view(),
                    depth: (Load::Load, Store::None),
                    stencil: (Load::Load, Store::None),
                    read_only: false,
                }),
                whole(8, 8),
            ));
            pass.set_stencil_reference(1);
            pass.set_pipeline(&test);
            for i in 1..3 {
                pass.bind(
                    0,
                    Bind::Uniform(uniforms.slice(i * stride..i * stride + 32)),
                );
                pass.draw(0..3, 0..1);
            }
        }
        gpu.submit(encoder);
        let bytes = gpu.read(&image);
        for y in 0..8 {
            for x in 0..8 {
                let expected = if x < 4 { GREEN } else { BLUE };
                assert_eq!(pixel(&bytes, 8, x, y), rgba8(expected), "({x}, {y})");
            }
        }
    }
}

#[test]
fn compute_writes_storage_buffers() {
    let mut gpu = gpu!();
    let shader = gpu.shader(COMPUTE, 4);
    let pipeline = gpu
        .device
        .create_compute_pipeline(
            &ComputePipelineDesc {
                entry: shader.entry("main", Stage::Compute).unwrap(),
                bindings: vec![Binding {
                    slot: 0,
                    kind: BindingKind::StorageBuffer { writable: true },
                    stages: Stages::COMPUTE,
                }],
                label: "compute",
            },
            None,
        )
        .expect("compute pipeline");
    let values = gpu.buffer(
        256 * 4,
        BufferUsage::STORAGE | BufferUsage::COPY_SRC,
        Memory::Device,
    );
    let indirect = gpu.has(Features::INDIRECT);
    let args = gpu.upload(bytemuck_u32(&[2, 1, 1]), BufferUsage::INDIRECT);
    let mut encoder = gpu.queue.encoder("compute");
    {
        let mut pass = encoder.compute_pass(&ComputePassDesc {
            timestamps: None,
            label: "compute",
        });
        pass.set_pipeline(&pipeline);
        pass.bind(0, Bind::StorageBuffer(values.slice(0..512)));
        pass.dispatch(2, 1, 1);
        // The second half, at a bound offset, through indirect arguments where supported.
        pass.bind(0, Bind::StorageBuffer(values.slice(512..1024)));
        if indirect {
            pass.dispatch_indirect(args.slice(0..12));
        } else {
            pass.dispatch(2, 1, 1);
        }
    }
    gpu.submit(encoder);
    let bytes = gpu.read_buffer(&values, 1024);
    for (i, word) in bytes.chunks(4).enumerate() {
        let i = (i % 128) as u32;
        assert_eq!(u32::from_le_bytes(word.try_into().unwrap()), i * 3 + 1);
    }
}

const SHARED: &str = "
@group(3) @binding(0) var<storage, read_write> values: array<u32>;
var<workgroup> a: array<u32, 64>;
var<workgroup> b: array<vec2<u32>, 64>;
@compute @workgroup_size(64) fn main(@builtin(local_invocation_index) i: u32, @builtin(workgroup_id) g: vec3<u32>) {
    a[i] = i + g.x * 100u;
    b[i] = vec2<u32>(i * 2u, 7u);
    workgroupBarrier();
    values[g.x * 64u + i] = a[63u - i] + b[(i + 1u) % 64u].x * b[i].y;
}
";

/// Workgroup arrays are shared across a workgroup's invocations (Metal sets
/// threadgroup lengths; Vulkan drops their explicit layouts).
#[test]
fn compute_shares_workgroup_memory() {
    let mut gpu = gpu!();
    let shader = gpu.shader(SHARED, 6);
    let pipeline = gpu
        .device
        .create_compute_pipeline(
            &ComputePipelineDesc {
                entry: shader.entry("main", Stage::Compute).unwrap(),
                bindings: vec![Binding {
                    slot: 0,
                    kind: BindingKind::StorageBuffer { writable: true },
                    stages: Stages::COMPUTE,
                }],
                label: "shared",
            },
            None,
        )
        .expect("compute pipeline");
    let values = gpu.buffer(
        128 * 4,
        BufferUsage::STORAGE | BufferUsage::COPY_SRC,
        Memory::Device,
    );
    let mut encoder = gpu.queue.encoder("shared");
    {
        let mut pass = encoder.compute_pass(&ComputePassDesc {
            timestamps: None,
            label: "shared",
        });
        pass.set_pipeline(&pipeline);
        pass.bind(0, Bind::StorageBuffer(values.slice(0..512)));
        pass.dispatch(2, 1, 1);
    }
    gpu.submit(encoder);
    for (n, word) in gpu.read_buffer(&values, 512).chunks(4).enumerate() {
        let (g, i) = (n as u32 / 64, n as u32 % 64);
        let expected = (63 - i + g * 100) + ((i + 1) % 64) * 2 * 7;
        assert_eq!(
            u32::from_le_bytes(word.try_into().unwrap()),
            expected,
            "invocation {n}"
        );
    }
}

const STORE: &str = "
@group(3) @binding(0) var image: texture_storage_2d<rgba8unorm, write>;
@compute @workgroup_size(8, 8) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    textureStore(image, id.xy, vec4<f32>(vec2<f32>(id.xy) / 255.0, 0.5, 1.0));
}
";

/// Compute writes storage images, and the presentable buffers of a storage
/// pool, which the upscaler's final layer writes (HOST-VIEW-7).
#[test]
fn compute_writes_storage_images_and_storage_pools() {
    let mut gpu = gpu!();
    let shader = gpu.shader(STORE, 5);
    let pipeline = gpu
        .device
        .create_compute_pipeline(
            &ComputePipelineDesc {
                entry: shader.entry("main", Stage::Compute).unwrap(),
                bindings: vec![Binding {
                    slot: 0,
                    kind: BindingKind::StorageImage {
                        format: Format::Rgba8Unorm,
                        writable: true,
                    },
                    stages: Stages::COMPUTE,
                }],
                label: "store",
            },
            None,
        )
        .expect("compute pipeline");
    let check = |gpu: &mut Gpu, image: &Image| {
        let mut encoder = gpu.queue.encoder("store");
        {
            let mut pass = encoder.compute_pass(&ComputePassDesc {
                timestamps: None,
                label: "store",
            });
            pass.set_pipeline(&pipeline);
            pass.bind(0, Bind::StorageImage(image.view()));
            pass.dispatch(4, 2, 1);
        }
        gpu.submit(encoder);
        let bgra = image.desc().format == Format::Bgra8Unorm;
        for (i, texel) in gpu.read(image).chunks(4).enumerate() {
            let (x, y) = ((i % 32) as u8, (i / 32) as u8);
            let expected = if bgra {
                [128, y, x, 255]
            } else {
                [x, y, 128, 255]
            };
            assert_eq!(
                texel,
                expected,
                "texel {x},{y} of {:?}",
                image.desc().format
            );
        }
    };
    let image = gpu
        .device
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: extent(32, 16),
            format: Format::Rgba8Unorm,
            mips: 1,
            samples: 1,
            usage: Usage::STORAGE | Usage::COPY_SRC,
            view_formats: &[],
            label: "storage image",
        })
        .expect("storage image");
    check(&mut gpu, &image);
    if !gpu.has(Features::PRESENTABLE_BUFFERS) {
        return;
    }
    let format = [Format::Rgba8Unorm, Format::Bgra8Unorm]
        .into_iter()
        .find(|&f| gpu.supports(f, FormatCaps::PRESENTABLE | FormatCaps::STORAGE))
        .expect("storage presentable format");
    let mut pool = gpu
        .device
        .create_present_pool(&adv_gpu::present::PoolDesc {
            color: None,
            width: 32,
            height: 16,
            format,
            count: 1,
            storage: true,
            label: "storage pool",
        })
        .expect("storage pool");
    let buffer = pool
        .acquire(&mut gpu.queue)
        .expect("acquire")
        .expect("buffer");
    assert!(buffer.image().desc().usage.contains(Usage::STORAGE));
    check(&mut gpu, buffer.image());
}

fn bytemuck_u32(values: &[u32]) -> &[u8] {
    // SAFETY: u32 has no padding.
    unsafe { std::slice::from_raw_parts(values.as_ptr().cast(), std::mem::size_of_val(values)) }
}

#[test]
fn pipeline_identity_counts_driver_objects() {
    let gpu = gpu!();
    let base = gpu.solid_desc(Format::Rgba8Unorm);
    assert_eq!(
        gpu.device.pipeline_identity(&base),
        gpu.device.pipeline_identity(&base.clone())
    );
    let mut blended = base.clone();
    blended.colors[0].blend = Some((
        BlendComponent {
            src: Factor::One,
            dst: Factor::One,
            op: BlendOp::Add,
        },
        BlendComponent {
            src: Factor::One,
            dst: Factor::One,
            op: BlendOp::Add,
        },
    ));
    assert_ne!(
        gpu.device.pipeline_identity(&base),
        gpu.device.pipeline_identity(&blended)
    );
    // Cull mode is dynamic on Metal and on Vulkan with extended dynamic state,
    // so it shares the driver object there and keys it elsewhere.
    let mut culled = base.clone();
    culled.primitive.cull = Cull::Back;
    let dynamic = cfg!(target_vendor = "apple")
        || gpu
            .device
            .caps()
            .modern
            .contains(Modern::EXTENDED_DYNAMIC_STATE);
    assert_eq!(
        gpu.device.pipeline_identity(&base) == gpu.device.pipeline_identity(&culled),
        dynamic
    );
}

#[test]
fn pipeline_cache_round_trips_and_rejects_garbage() {
    let gpu = gpu!();
    if !gpu.has(Features::PIPELINE_CACHE) {
        return;
    }
    let desc = gpu.solid_desc(Format::Rgba8Unorm);
    let cache = PipelineCache::load(&[]);
    gpu.device
        .create_render_pipeline(&desc, Some(&cache))
        .expect("pipeline with empty cache");
    let data = cache.serialize().expect("serialize");
    let reloaded = PipelineCache::load(&data);
    gpu.device
        .create_render_pipeline(&desc, Some(&reloaded))
        .expect("pipeline with reloaded cache");
    let garbage = PipelineCache::load(&[0xde, 0xad, 0xbe, 0xef]);
    gpu.device
        .create_render_pipeline(&desc, Some(&garbage))
        .expect("pipeline with discarded cache");
}

#[test]
fn dropped_resources_outlive_their_submissions() {
    let mut gpu = gpu!();
    let image = gpu.target(64, 64, Format::Rgba8Unorm);
    let mut encoder = gpu.queue.encoder("drop");
    drop(encoder.render_pass(&pass(color(image.view(), clear(RED)), None, whole(64, 64))));
    drop(image);
    let token = gpu.queue.submit(encoder).expect("submit");
    gpu.finish(token);
    gpu.queue.poll().expect("poll");
}

#[test]
fn timestamps_bracket_a_pass() {
    let mut gpu = gpu!();
    if !gpu.has(Features::TIMESTAMPS) {
        return;
    }
    let set = gpu
        .device
        .create_query_set(QueryKind::Timestamp, 2)
        .expect("query set");
    assert!(set.period() > 0.0);
    let image = gpu.target(16, 16, Format::Rgba8Unorm);
    let results = gpu.buffer(16, BufferUsage::COPY_DST, Memory::Readback);
    let mut encoder = gpu.queue.encoder("timestamps");
    let mut desc = pass(color(image.view(), clear(RED)), None, whole(16, 16));
    desc.timestamps = Some((&set, 0));
    drop(encoder.render_pass(&desc));
    encoder.resolve_queries(&set, 0..2, &results, 0);
    gpu.submit(encoder);
    let bytes = mapped(&results);
    let start = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    let end = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    assert!(end >= start, "{start} > {end}");
}

#[test]
fn occlusion_counts_samples() {
    let mut gpu = gpu!();
    if !gpu.has(Features::OCCLUSION_QUERIES) {
        return;
    }
    let set = gpu
        .device
        .create_query_set(QueryKind::Occlusion, 1)
        .expect("query set");
    let image = gpu.target(16, 16, Format::Rgba8Unorm);
    let pipeline = gpu.pipeline(&gpu.solid_desc(Format::Rgba8Unorm));
    let (uniforms, _) = gpu.uniforms(&[block(RED, 0.5)]);
    let results = gpu.buffer(8, BufferUsage::COPY_DST, Memory::Readback);
    let mut encoder = gpu.queue.encoder("occlusion");
    {
        let mut pass = encoder.render_pass(&pass(
            color(image.view(), Load::DontCare),
            None,
            whole(16, 16),
        ));
        pass.set_pipeline(&pipeline);
        pass.bind(0, Bind::Uniform(uniforms.slice(0..32)));
        pass.begin_occlusion(&set, 0);
        pass.draw(0..3, 0..1);
        pass.end_occlusion();
    }
    encoder.resolve_queries(&set, 0..1, &results, 0);
    gpu.submit(encoder);
    assert_eq!(
        u64::from_le_bytes(mapped(&results)[..8].try_into().unwrap()),
        256
    );
}

#[test]
fn multisampled_targets_resolve() {
    let mut gpu = gpu!();
    if !gpu.has(Features::MULTISAMPLE) || gpu.device.caps().limits.sample_counts & (1 << 2) == 0 {
        return;
    }
    let msaa = gpu
        .device
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: extent(8, 8),
            format: Format::Rgba8Unorm,
            mips: 1,
            samples: 4,
            usage: Usage::COLOR | Usage::TRANSIENT,
            view_formats: &[],
            label: "msaa",
        })
        .expect("msaa target");
    let resolved = gpu.target(8, 8, Format::Rgba8Unorm);
    let mut desc = gpu.solid_desc(Format::Rgba8Unorm);
    desc.samples = 4;
    let pipeline = gpu.pipeline(&desc);
    let mut colors = ArrayVec::new();
    colors.push(ColorAttachment {
        view: msaa.view(),
        load: Load::DontCare,
        store: Store::DontCare,
        resolve: Some(resolved.view()),
    });
    draw_solid(
        &mut gpu,
        &pipeline,
        pass(colors, None, whole(8, 8)),
        &[block(GREEN, 0.5)],
        None,
    );
    for texel in gpu.read(&resolved).chunks(4) {
        assert_eq!(texel, rgba8(GREEN));
    }
}

#[test]
fn present_pools_track_ages_and_releases() {
    let mut gpu = gpu!();
    if !gpu.has(Features::PRESENTABLE_BUFFERS) {
        return;
    }
    let format = [Format::Rgba8Unorm, Format::Bgra8Unorm]
        .into_iter()
        .find(|&f| gpu.supports(f, FormatCaps::PRESENTABLE))
        .expect("presentable format");
    let mut pool = gpu
        .device
        .create_present_pool(&adv_gpu::present::PoolDesc {
            color: None,
            width: 64,
            height: 64,
            format,
            count: 2,
            storage: false,
            label: "pool",
        })
        .expect("pool");
    let acquire = |pool: &mut adv_gpu::PresentPool, queue: &mut adv_gpu::Queue| {
        pool.acquire(queue).expect("acquire")
    };
    let mut first = acquire(&mut pool, &mut gpu.queue).expect("first buffer");
    let second = acquire(&mut pool, &mut gpu.queue).expect("second buffer");
    assert!(
        acquire(&mut pool, &mut gpu.queue).is_none(),
        "pool of two handed out a third buffer"
    );
    assert_eq!(first.age(), None);
    assert!(!first.image().desc().usage.contains(Usage::STORAGE));
    assert!(!matches!(first.export(), adv_gpu::present::Export::None));
    let mut encoder = gpu.queue.encoder("present");
    drop(encoder.render_pass(&pass(
        color(first.image().view(), clear(RED)),
        None,
        whole(64, 64),
    )));
    let token = gpu
        .queue
        .submit_present(encoder, &mut [&mut first])
        .expect("submit");
    // Android compositors wait on a sync fd, absent once the batch has
    // signaled; Apple's attach after the token.
    let fence = first.acquire_fence();
    assert!(cfg!(target_os = "android") || fence.is_none());
    first.released(None);
    drop(second);
    gpu.finish(token);
    // The newest content comes back first.
    let newest = acquire(&mut pool, &mut gpu.queue).expect("presented buffer back");
    let fresh = acquire(&mut pool, &mut gpu.queue).expect("unpresented buffer back");
    assert_eq!((newest.age(), fresh.age()), (Some(1), None));
    drop((newest, fresh));
    pool.trim(1);
}

#[cfg(not(target_os = "android"))]
#[test]
fn cpu_yuv_pools_write_sample_and_release() {
    use adv_gpu::{
        present::{PoolDesc, YuvColor},
        resource::ViewDesc,
    };
    let mut gpu = gpu!();
    let Some(format) = [Format::Nv12, Format::Nv21]
        .into_iter()
        .find(|&f| gpu.supports(f, FormatCaps::PRESENTABLE))
    else {
        assert!(
            !(cfg!(target_vendor = "apple")
                && std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_some()),
            "required device has no presentable YUV format"
        );
        return;
    };
    let mut pool = gpu
        .device
        .create_present_pool(&PoolDesc {
            width: 18,
            height: 10,
            format,
            color: Some(YuvColor::Bt709Limited),
            count: 2,
            storage: false,
            label: "CPU YUV",
        })
        .unwrap();
    let mut first = pool.acquire_cpu().unwrap();
    let second = pool.acquire_cpu().unwrap();
    assert!(pool.acquire_cpu().is_none());
    let first_id = first.image().id();
    first.write(|planes| {
        assert_eq!(planes.len(), 2);
        for y in 0..10 {
            for x in 0..18 {
                planes[0].data[y * planes[0].stride + x] = (x * 9 + y * 3) as u8;
            }
        }
        for y in 0..5 {
            for x in 0..9 {
                let at = y * planes[1].stride + x * 2;
                planes[1].data[at..at + 2]
                    .copy_from_slice(&[(x * 11 + 23) as u8, (y * 21 + 97) as u8]);
            }
        }
    });
    assert!(first.acquire_fence().is_none());
    let views: [_; 2] = std::array::from_fn(|plane| {
        first
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
    assert_eq!(views[1].size(), extent(9, 5));
    let shader = gpu.shader(
        r#"
        @group(1) @binding(0) var y: texture_2d<f32>;
        @group(1) @binding(1) var uv: texture_2d<f32>;
        @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
            let p = array<vec2<f32>, 3>(vec2(-1., -1.), vec2(3., -1.), vec2(-1., 3.));
            return vec4(p[i], 0., 1.);
        }
        @fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
            let xy = vec2<i32>(p.xy);
            return vec4(textureLoad(y, xy, 0).r, textureLoad(uv, xy / 2, 0).rg, 1.);
        }
    "#,
        0x795556,
    );
    let pipeline = gpu.pipeline(&RenderPipelineDesc {
        vertex: shader.entry("vs", Stage::Vertex).unwrap(),
        fragment: Some(shader.entry("fs", Stage::Fragment).unwrap()),
        bindings: (0..2)
            .map(|slot| Binding {
                slot,
                kind: BindingKind::SampledImage {
                    dimension: Dimension::D2,
                    depth: false,
                    multisampled: false,
                },
                stages: Stages::FRAGMENT,
            })
            .collect(),
        primitive: primitive(),
        colors: colors(Format::Rgba8Unorm, None),
        depth_stencil: None,
        samples: 1,
        alpha_to_coverage: false,
        label: "sample YUV planes",
    });
    let target = gpu.target(18, 10, Format::Rgba8Unorm);
    let mut encoder = gpu.queue.encoder("sample CPU planes");
    {
        let mut pass = encoder.render_pass(&pass(
            color(target.view(), Load::DontCare),
            None,
            whole(18, 10),
        ));
        pass.set_pipeline(&pipeline);
        pass.bind(0, Bind::Sampled(&views[0]));
        pass.bind(1, Bind::Sampled(&views[1]));
        pass.draw(0..3, 0..1);
    }
    gpu.submit(encoder);
    let pixels = gpu.read(&target);
    for y in 0..10usize {
        for x in 0..18usize {
            assert_eq!(
                &pixels[(y * 18 + x) * 4..][..4],
                &[
                    (x * 9 + y * 3) as u8,
                    (x / 2 * 11 + 23) as u8,
                    (y / 2 * 21 + 97) as u8,
                    255,
                ]
            );
        }
    }
    first.released(None);
    let mut next = pool.acquire_cpu().unwrap();
    assert_eq!(next.image().id(), first_id);
    assert_eq!(next.age(), Some(1));
    next.write(|p| p[0].data[0] = 99);
    assert!(
        pool.acquire_cpu().is_none(),
        "screen-owned lease was reused"
    );
    drop(second);
    let other = pool.acquire_cpu().unwrap();
    assert_ne!(other.image().id(), first_id);
}

#[cfg(target_os = "android")]
#[test]
fn cpu_yuv_pools_write_sample_and_release() {
    use adv_gpu::{
        present::{PoolDesc, YuvColor},
        resource::ViewDesc,
    };
    let mut gpu = gpu!();
    let Some(format) = [Format::Nv12, Format::Nv21]
        .into_iter()
        .find(|&format| gpu.supports(format, FormatCaps::PRESENTABLE))
    else {
        assert!(
            !(cfg!(any(target_vendor = "apple", target_os = "android"))
                && std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_some()),
            "required device has no presentable YUV format"
        );
        return;
    };
    for color in [YuvColor::Bt601Limited, YuvColor::Bt709Full] {
        let mut pool = gpu
            .device
            .create_present_pool(&PoolDesc {
                width: 18,
                height: 10,
                format,
                color: Some(color),
                count: 2,
                storage: false,
                label: "CPU YUV",
            })
            .unwrap();
        let mut first = pool.acquire_cpu().unwrap();
        let second = pool.acquire_cpu().unwrap();
        assert!(pool.acquire_cpu().is_none());
        let first_id = first.image().id();
        assert_eq!(first.color(), Some(color));
        let shader = gpu.shader(
            r#"
            @group(4) @binding(0) var video: texture_2d<f32>;
            @group(4) @binding(1) var filtering: sampler;
            @vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
                let p = array<vec2<f32>, 3>(vec2(-1., -1.), vec2(3., -1.), vec2(-1., 3.));
                return vec4(p[i], 0., 1.);
            }
            @fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
                return textureSampleLevel(video, filtering, p.xy / vec2(18., 10.), 0.);
            }
        "#,
            0x795556,
        );
        let desc = RenderPipelineDesc {
            vertex: shader.entry("vs", Stage::Vertex).unwrap(),
            fragment: Some(shader.entry("fs", Stage::Fragment).unwrap()),
            bindings: vec![],
            primitive: primitive(),
            colors: colors(Format::Rgba8Unorm, None),
            depth_stencil: None,
            samples: 1,
            alpha_to_coverage: false,
            label: "sample YUV",
        };
        let pipeline = gpu
            .device
            .create_video_pipeline(
                &adv_gpu::pipeline::VideoPipelineDesc {
                    pipeline: &desc,
                    pool: &pool,
                },
                None,
            )
            .unwrap();
        let target = gpu.target(18, 10, Format::Rgba8Unorm);
        for [y, u, v] in [
            [80, 109, 175],
            [150, 164, 91],
            [16, 128, 128],
            [235, 128, 128],
        ] {
            first.write(|planes| {
                assert_eq!(planes.len(), 2);
                for row in 0..10 {
                    let at = row * planes[0].stride;
                    planes[0].data[at..at + 18].fill(y);
                }
                for row in 0..5 {
                    let at = row * planes[1].stride;
                    for pair in planes[1].data[at..at + 18].as_chunks_mut::<2>().0 {
                        *pair = if format == Format::Nv21 {
                            [v, u]
                        } else {
                            [u, v]
                        };
                    }
                }
            });
            assert!(first.acquire_fence().is_none());
            assert!(
                first
                    .image()
                    .create_view(&ViewDesc {
                        format: Some(Format::R8Unorm),
                        dimension: None,
                        mips: 0..1,
                        layers: 0..1,
                        plane: 0,
                    })
                    .is_err(),
                "external formats have no plane views"
            );
            let mut encoder = gpu.queue.encoder("sample CPU YUV");
            {
                let mut pass = encoder.render_pass(&pass(
                    crate::color(target.view(), Load::DontCare),
                    None,
                    whole(18, 10),
                ));
                pass.set_pipeline(&pipeline);
                pass.bind_video(&first);
                pass.draw(0..3, 0..1);
            }
            gpu.submit(encoder);
            let pixels = gpu.read(&target);
            let expected = yuv_rgb(y, u, v, color);
            for actual in pixels.as_chunks::<4>().0 {
                for channel in 0..4 {
                    assert!(
                        actual[channel].abs_diff(expected[channel]) <= 2,
                        "{color:?}: {actual:?} != {expected:?}"
                    );
                }
            }
        }
        first.released(None);
        let mut next = pool.acquire_cpu().unwrap();
        assert_eq!(next.image().id(), first_id);
        assert_eq!(next.age(), Some(1));
        next.write(|p| p[0].data[0] = 99);
        assert!(
            pool.acquire_cpu().is_none(),
            "screen-owned lease was reused"
        );
        drop(second);
        let other = pool.acquire_cpu().unwrap();
        assert_ne!(other.image().id(), first_id);
    }
}

#[cfg(target_os = "android")]
fn yuv_rgb(y: u8, u: u8, v: u8, color: adv_gpu::present::YuvColor) -> [u8; 4] {
    use adv_gpu::present::YuvColor::*;
    let full = matches!(color, Bt601Full | Bt709Full);
    let (kr, kb) = if matches!(color, Bt709Limited | Bt709Full) {
        (0.2126, 0.0722)
    } else {
        (0.299, 0.114)
    };
    let y = if full {
        f64::from(y) / 255.0
    } else {
        (f64::from(y) - 16.0) / 219.0
    };
    let scale = if full { 255.0 } else { 224.0 };
    let (u, v) = (
        (f64::from(u) - 128.0) / scale,
        (f64::from(v) - 128.0) / scale,
    );
    let (r, b) = (y + 2.0 * (1.0 - kr) * v, y + 2.0 * (1.0 - kb) * u);
    let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
    let [r, g, b] = [r, g, b].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
    [r, g, b, 255]
}

#[cfg(target_vendor = "apple")]
#[test]
fn yuv_pool_color_attachments() {
    use adv_gpu::present::{Export, PoolDesc, YuvColor};
    use objc2_core_foundation::CFString;
    use objc2_io_surface::{IOSurfaceRef, kIOSurfaceColorSpace};
    #[link(name = "IOSurface", kind = "framework")]
    unsafe extern "C" {
        static kIOSurfaceYCbCrMatrix: &'static CFString;
    }
    let gpu = gpu!();
    for color in [
        YuvColor::Bt601Limited,
        YuvColor::Bt601Full,
        YuvColor::Bt709Limited,
        YuvColor::Bt709Full,
    ] {
        let desc = PoolDesc {
            width: 18,
            height: 10,
            format: Format::Nv12,
            color: Some(color),
            count: 1,
            storage: false,
            label: "YUV color",
        };
        assert!(
            gpu.device
                .create_present_pool(&PoolDesc {
                    color: None,
                    ..desc
                })
                .is_err()
        );
        assert!(
            gpu.device
                .create_present_pool(&PoolDesc {
                    storage: true,
                    ..desc
                })
                .is_err(),
            "storage writes are RGB only"
        );
        let mut pool = gpu.device.create_present_pool(&desc).unwrap();
        let image = pool.acquire_cpu().unwrap();
        let Export::IoSurface(pointer, _) = image.export() else {
            unreachable!()
        };
        let surface = unsafe { pointer.cast::<IOSurfaceRef>().as_ref() };
        let full = matches!(color, YuvColor::Bt601Full | YuvColor::Bt709Full);
        assert_eq!(
            surface.pixel_format(),
            u32::from_be_bytes(if full { *b"420f" } else { *b"420v" })
        );
        let matrix = surface
            .value(unsafe { kIOSurfaceYCbCrMatrix })
            .unwrap()
            .downcast::<CFString>()
            .unwrap();
        assert_eq!(
            matrix.to_string(),
            if matches!(color, YuvColor::Bt709Limited | YuvColor::Bt709Full) {
                "ITU_R_709_2"
            } else {
                "ITU_R_601_4"
            }
        );
        assert!(surface.value(unsafe { kIOSurfaceColorSpace }).is_some());
    }
}

#[test]
fn vertex_and_fragment_samplers_use_independent_stage_indices() {
    let mut gpu = gpu!();
    let image = gpu.target(2, 1, Format::Rgba8Unorm);
    let target = gpu.target(1, 1, Format::Rgba8Unorm);
    let source = gpu.upload(&[255, 0, 0, 255, 0, 255, 0, 255], BufferUsage::COPY_SRC);
    let mut encoder = gpu.queue.encoder("sampler upload");
    encoder.copy_buffer_to_image(
        BufferCopy {
            buffer: &source,
            offset: 0,
            row_pitch: 8,
            rows: 1,
        },
        ImageCopy {
            image: &image,
            mip: 0,
            layer: 0,
            origin: (0, 0, 0),
        },
        extent(2, 1),
    );
    gpu.submit(encoder);
    let shader = gpu.shader(
        r#"
@group(1) @binding(0) var image:texture_2d<f32>;
@group(2) @binding(0) var point:sampler;
@group(2) @binding(16) var linear:sampler;
struct V { @builtin(position) position:vec4<f32>, @location(0) red:f32 }
fn vertex(i:u32, red:f32)->V {
    let p=vec2<f32>(f32((i<<1u)&2u),f32(i&2u));
    return V(vec4<f32>(p*2.0-1.0,0.0,1.0),red);
}
@vertex fn vs(@builtin(vertex_index) i:u32)->V {
    return vertex(i,textureSampleLevel(image,linear,vec2<f32>(0.375,0.5),0.0).r);
}
@vertex fn vs_point(@builtin(vertex_index) i:u32)->V {
    return vertex(i,textureSampleLevel(image,point,vec2<f32>(0.375,0.5),0.0).r);
}
@fragment fn fs(v:V)->@location(0) vec4<f32> {
    return vec4<f32>(v.red,textureSampleLevel(image,point,vec2<f32>(0.375,0.5),0.0).g,0.0,1.0);
}
"#,
        1001,
    );
    let mut desc = RenderPipelineDesc {
        vertex: shader.entry("vs", Stage::Vertex).unwrap(),
        fragment: Some(shader.entry("fs", Stage::Fragment).unwrap()),
        bindings: vec![
            Binding {
                slot: 0,
                kind: BindingKind::SampledImage {
                    dimension: Dimension::D2,
                    depth: false,
                    multisampled: false,
                },
                stages: Stages::VERTEX | Stages::FRAGMENT,
            },
            Binding {
                slot: 0,
                kind: BindingKind::Sampler { comparison: false },
                stages: Stages::VERTEX | Stages::FRAGMENT,
            },
            Binding {
                slot: 16,
                kind: BindingKind::Sampler { comparison: false },
                stages: Stages::VERTEX,
            },
        ],
        primitive: primitive(),
        colors: colors(Format::Rgba8Unorm, None),
        depth_stencil: None,
        samples: 1,
        alpha_to_coverage: false,
        label: "per-stage samplers",
    };
    let linear_pipeline = gpu.pipeline(&desc);
    desc.vertex = shader.entry("vs_point", Stage::Vertex).unwrap();
    let point_pipeline = gpu.pipeline(&desc);
    let sampler = |filter| {
        gpu.device
            .sampler(&SamplerDesc {
                min: filter,
                mag: filter,
                mip: Filter::Nearest,
                address: [Address::Clamp; 3],
                border: BorderColor::TransparentBlack,
                lod_bias: 0,
                lod_min: 0,
                lod_max: 0,
                anisotropy: 1,
                compare: None,
            })
            .unwrap()
    };
    let (point, linear) = (sampler(Filter::Nearest), sampler(Filter::Linear));
    for (pipelines, expected) in [
        (vec![&linear_pipeline], [191, 0, 0, 255]),
        (vec![&linear_pipeline, &point_pipeline], [255, 0, 0, 255]),
        (
            vec![&linear_pipeline, &point_pipeline, &linear_pipeline],
            [191, 0, 0, 255],
        ),
    ] {
        let mut encoder = gpu.queue.encoder("per-stage sampler draw");
        {
            let mut render = encoder.render_pass(&pass(
                color(target.view(), Load::DontCare),
                None,
                whole(1, 1),
            ));
            render.bind(0, Bind::Sampled(image.view()));
            render.bind(0, Bind::Sampler(&point));
            render.bind(16, Bind::Sampler(&linear));
            for pipeline in pipelines {
                render.set_pipeline(pipeline);
                render.draw(0..3, 0..1);
            }
        }
        gpu.submit(encoder);
        assert_eq!(gpu.read(&target), expected);
    }
}

#[cfg(target_vendor = "apple")]
#[test]
fn metal_rejects_sampler_aliases_within_one_stage_at_pipeline_creation() {
    let gpu = gpu!();
    // Precompiled entries use the pipeline's declarations as their used mask.
    // This checks the pipeline boundary independently of the MSL compiler.
    let shader = gpu
        .device
        .create_shader(ShaderDesc {
            source: Source::Precompiled {
                spirv: &[],
                metal: br#"#include <metal_stdlib>
using namespace metal;
vertex float4 vs(uint i [[vertex_id]], sampler s [[sampler(0)]]) {
    return float4(float(i), 0.0, 0.0, 1.0);
}
"#,
                entries: &[adv_gpu::shader::PrecompiledEntry {
                    name: "vs",
                    function: "vs",
                    workgroup: [1; 3],
                    threadgroup: &[],
                }],
            },
            identity: 1002,
            label: "colliding sampler slots",
        })
        .unwrap();
    let desc = RenderPipelineDesc {
        vertex: shader.entry("vs", Stage::Vertex).unwrap(),
        fragment: None,
        bindings: vec![
            Binding {
                slot: 0,
                kind: BindingKind::SampledImage {
                    dimension: Dimension::D2,
                    depth: false,
                    multisampled: false,
                },
                stages: Stages::VERTEX,
            },
            Binding {
                slot: 0,
                kind: BindingKind::Sampler { comparison: false },
                stages: Stages::VERTEX,
            },
            Binding {
                slot: 16,
                kind: BindingKind::Sampler { comparison: false },
                stages: Stages::VERTEX,
            },
        ],
        primitive: primitive(),
        colors: ArrayVec::new(),
        depth_stencil: None,
        samples: 1,
        alpha_to_coverage: false,
        label: "colliding sampler slots",
    };
    assert!(
        matches!(gpu.device.create_render_pipeline(&desc, None), Err(Error::Invalid(reason)) if reason.contains("collide modulo 16"))
    );
}
