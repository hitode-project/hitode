use super::*;
use crate::gpu::native::resources::tests::{executor, read};
use adv_gpu::pipeline::{Compare, Cull, DepthStencil, Primitive, Topology};
use adv_gpu::queue::QueryKind;
use adv_gpu::resource::{BufferDesc, BufferUsage, Dimension, ImageDesc, Memory, Usage, ViewDesc};
use adv_gpu::{Extent, Format, Image};
use sha2::{Digest, Sha256};

const PROBES: &str = include_str!("probes.tsv");
const TRIANGLE: &str = "
@vertex fn main(@builtin(vertex_index) id: u32) -> @builtin(position) vec4<f32> {
    let p = array(vec2(-1., -1.), vec2(3., -1.), vec2(-1., 3.));
    return vec4(p[id], 0.5, 1.);
}";
const WHITE: &str = "@fragment fn main() -> @location(0) vec4<f32> { return vec4(1.); }";

fn shader(source: &str, stage: Stage) -> Arc<ModernShader> {
    let identity = u64::from_le_bytes(Sha256::digest(source.as_bytes())[..8].try_into().unwrap());
    Arc::new(ModernShader::wgsl(identity, source, "main", stage).unwrap())
}

fn image(
    gpu: &Gpu,
    width: u32,
    height: u32,
    format: Format,
    samples: u32,
    mips: u32,
    usage: Usage,
) -> Image {
    gpu.device()
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width,
                height,
                depth_or_layers: 1,
            },
            format,
            mips,
            samples,
            usage,
            view_formats: &[],
            label: "executor test",
        })
        .unwrap()
}

fn target(gpu: &Gpu, width: u32, height: u32, format: Format) -> Image {
    image(
        gpu,
        width,
        height,
        format,
        1,
        1,
        Usage::COLOR | Usage::COPY_SRC,
    )
}

fn area(width: u32, height: u32) -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: width,
        h: height,
    }
}

fn color(image: &Image, load: Load<[f64; 4]>) -> ModernColor {
    ModernColor {
        view: image.view().clone(),
        blend: None,
        write_mask: 15,
        load,
        store: Store::Store,
    }
}

fn draw(
    width: u32,
    height: u32,
    vertex: Arc<ModernShader>,
    fragment: Arc<ModernShader>,
    colors: impl IntoIterator<Item = ModernColor>,
) -> ModernDraw {
    ModernDraw {
        color_slots: None,
        state_ids: [0; 4],
        vertex,
        fragment: Some(fragment),
        colors: colors.into_iter().collect(),
        depth: None,
        samples: 1,
        sample_mask: u32::MAX,
        inexact_sample_mask: false,
        alpha_to_coverage: false,
        primitive: Primitive {
            topology: Topology::Triangles,
            cull: Cull::None,
            front_ccw: true,
            wireframe: false,
            unclipped_depth: false,
        },
        area: area(width, height),
        viewport: [0., 0., width as f32, height as f32, 0., 1.],
        scissor: area(width, height),
        stencil_reference: 0,
        blend_constant: [0.; 4],
        depth_bias: [0.; 3],
        arguments: DrawArguments::Direct {
            vertices: 0..3,
            instances: 0..1,
            base_vertex: 0,
        },
        index: None,
        occlusion: None,
        predicate: None,
    }
}

fn simple(gpu: &mut Gpu, draw: ModernDraw) {
    gpu.draw_modern(draw, &SlotBindings::default(), &SlotBindings::default())
        .unwrap();
}

fn buffer(gpu: &Gpu, size: u64, usage: BufferUsage, memory: Memory) -> Buffer {
    gpu.device()
        .create_buffer(&BufferDesc {
            size,
            usage,
            memory,
            label: "executor test",
        })
        .unwrap()
}

fn upload(gpu: &mut Gpu, image: &Image, mip: u32, pitch: u32, rows: u32, data: &[u8]) {
    let staged = gpu.stage_bytes(pitch, rows, data).unwrap();
    gpu.upload_image(image, mip, 0, staged).unwrap();
}

fn synchronize(gpu: &mut Gpu) {
    let fence = gpu.target(1, 1, false).unwrap();
    gpu.clear(&fence, Some([0.; 4]), None).unwrap();
    read(gpu, &fence.image);
    gpu.check().unwrap();
}

fn mapped(buffer: &Buffer) -> &[u8] {
    // SAFETY: the readback that follows the test work has completed.
    unsafe { buffer.mapped().unwrap().as_ref() }
}

fn read_image(gpu: &mut Gpu, image: &Image, mip: u32, bpp: u32) -> Vec<u8> {
    let pitch = (image.desc().size.width >> mip).max(1) * bpp;
    let rows = (image.desc().size.height >> mip).max(1);
    let buffer = buffer(
        gpu,
        u64::from(pitch * rows),
        BufferUsage::COPY_DST,
        Memory::Readback,
    );
    gpu.copy_image_to_buffer(image, mip, 0, buffer.clone(), pitch)
        .unwrap();
    synchronize(gpu);
    mapped(&buffer)[..(pitch * rows) as usize].to_vec()
}

// spec: GFX-CAP-5
#[test]
fn image_filter_downsamples_and_resolves_with_point_loads() {
    let Some(mut gpu) = executor() else { return };
    let format = Format::R32Float;
    let texture = image(
        &gpu,
        4,
        2,
        format,
        1,
        2,
        Usage::SAMPLED | Usage::STORAGE | Usage::COPY_DST | Usage::COPY_SRC,
    );
    upload(
        &mut gpu,
        &texture,
        0,
        16,
        2,
        bytemuck::bytes_of(&[0f32, 2., 4., 6., 8., 10., 12., 14.]),
    );
    let view = |mip| {
        texture
            .create_view(&ViewDesc {
                format: None,
                dimension: None,
                mips: mip..mip + 1,
                layers: 0..1,
                plane: 0,
            })
            .unwrap()
    };
    let mip = gpu.prepare_image_filter(format, false, false).unwrap();
    gpu.dispatch(
        &mip,
        &SlotBindings {
            images: [view(0)].into(),
            storage: [StorageBinding::Image(view(1))].into(),
            ..Default::default()
        },
        DispatchArguments::Direct([1, 1, 1]),
    )
    .unwrap();
    assert_eq!(
        read_image(&mut gpu, &texture, 1, 4),
        bytemuck::bytes_of(&[5f32, 9.])
    );

    if gpu.device().caps().format_sample_counts(format) & 4 != 0 {
        let source = image(&gpu, 2, 1, format, 4, 1, Usage::COLOR | Usage::SAMPLED);
        let mut draw = draw(
            2,
            1,
            shader(TRIANGLE, Stage::Vertex),
            shader(
                "@fragment fn main(@builtin(sample_index) i: u32) -> @location(0) f32 { return f32(i) * 0.125; }",
                Stage::Fragment,
            ),
            [color(&source, Load::DontCare)],
        );
        draw.samples = 4;
        simple(&mut gpu, draw);
        let resolve = gpu.prepare_image_filter(format, false, true).unwrap();
        gpu.dispatch(
            &resolve,
            &SlotBindings {
                images: [source.view().clone()].into(),
                storage: [StorageBinding::Image(view(1))].into(),
                ..Default::default()
            },
            DispatchArguments::Direct([1, 1, 1]),
        )
        .unwrap();
        assert_eq!(
            read_image(&mut gpu, &texture, 1, 4),
            bytemuck::bytes_of(&[0.1875f32; 2])
        );
        let packed = target(&gpu, 2, 1, Format::Bgra8Unorm);
        gpu.draw_modern(
            ModernDraw::packed_resolve(packed.view().clone(), [31, 31, 31, 1]).unwrap(),
            &SlotBindings::default(),
            &SlotBindings {
                images: [source.view().clone()].into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            read_image(&mut gpu, &packed, 0, 4),
            [0, 0, 49, 255, 0, 0, 49, 255]
        );
    }
}

fn oracle(name: &str, row: usize) -> impl Iterator<Item = &'static str> {
    let prefix = format!("{name}\t{row}\t");
    PROBES
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap()
        .split('\t')
}

fn bits(value: &str) -> u32 {
    u32::from_str_radix(value, 16).unwrap()
}

fn quad_shader(quads: &[[f32; 4]], width: f32, height: f32) -> Arc<ModernShader> {
    let positions = quads
        .iter()
        .flat_map(|&[x0, y0, x1, y1]| [[x0, y0], [x1, y0], [x0, y1], [x1, y0], [x1, y1], [x0, y1]])
        .map(|[x, y]| format!("vec2<f32>({x:?}, {y:?})"))
        .collect::<Vec<_>>()
        .join(", ");
    shader(
        &format!(
            "
@vertex fn main(@builtin(vertex_index) id: u32) -> @builtin(position) vec4<f32> {{
    let positions = array({positions});
    let p = positions[id] / vec2({width:?}, {height:?});
    return vec4(p * vec2(2., -2.) + vec2(-1., 1.), 0.5, 1.);
}}"
        ),
        Stage::Vertex,
    )
}

#[test]
fn pixel_probe_fill_and_viewport() {
    let Some(mut gpu) = executor() else { return };
    let image = target(&gpu, 32, 32, Format::Rgba8Unorm);
    let white = shader(WHITE, Stage::Fragment);
    for (name, quad) in [
        ("quad-1-1-5-5", [1., 1., 5., 5.]),
        ("quad-1.5-1.5-5.5-5.5", [1.5, 1.5, 5.5, 5.5]),
        ("quad-1.49-1.49-5.49-5.49", [1.49, 1.49, 5.49, 5.49]),
        ("quad-1.51-1.51-5.51-5.51", [1.51, 1.51, 5.51, 5.51]),
        ("quad-thin-y-1.5-1.9", [1., 1.5, 5., 1.9]),
        ("quad-thin-y-1.4-1.6", [1., 1.4, 5., 1.6]),
    ] {
        let vertex = quad_shader(&[quad], 32., 32.);
        let mut draw = draw(
            32,
            32,
            vertex,
            white.clone(),
            [color(&image, Load::Clear([0.; 4]))],
        );
        draw.arguments = DrawArguments::Direct {
            vertices: 0..6,
            instances: 0..1,
            base_vertex: 0,
        };
        simple(&mut gpu, draw);
        compare_coverage(&mut gpu, &image, name, 7, 7);
    }
    for (name, [x, y, w, h], scissor) in [
        ("viewport-0.5-0.5-8x8", [0.5, 0.5, 8., 8.], area(32, 32)),
        ("viewport-2.25-2.75-4x4", [2.25, 2.75, 4., 4.], area(32, 32)),
        ("viewport-1-1-4.5x4.5", [1., 1., 4.5, 4.5], area(32, 32)),
        (
            "viewport-full-scissor-1-2-5-4",
            [0., 0., 32., 32.],
            Rect {
                x: 1,
                y: 2,
                w: 4,
                h: 2,
            },
        ),
    ] {
        let mut draw = draw(
            32,
            32,
            shader(TRIANGLE, Stage::Vertex),
            white.clone(),
            [color(&image, Load::Clear([0.; 4]))],
        );
        draw.viewport = [x, y, w, h, 0., 1.];
        draw.scissor = scissor;
        simple(&mut gpu, draw);
        compare_coverage(&mut gpu, &image, name, 10, 10);
    }
}

fn compare_coverage(gpu: &mut Gpu, image: &Image, name: &str, width: usize, height: usize) {
    let pixels = read_image(gpu, image, 0, 4);
    let stride = image.desc().size.width as usize * 4;
    for y in 0..height {
        let mask = bits(oracle(name, y).next().unwrap());
        for x in 0..width {
            let pixel = y * stride + x * 4;
            let expected = if mask & (1 << x) != 0 { 255 } else { 0 };
            assert_eq!(
                &pixels[pixel..pixel + 4],
                &[expected; 4],
                "{name} at ({x},{y})"
            );
        }
    }
}

#[test]
fn pixel_probe_standard_multisample_resolve_and_load() {
    let Some(mut gpu) = executor() else { return };
    let resolved = target(&gpu, 17, 19, Format::Rgba8Unorm);
    let mut quads = Vec::new();
    for k in 0..17 {
        let k = k as f32;
        quads.push([k + k / 16., 0., k + 1., 1.]);
        quads.push([0., 2. + k + k / 16., 1., 3. + k]);
    }
    let vertex = quad_shader(&quads, 17., 19.);
    let fragment = shader(WHITE, Stage::Fragment);
    for samples in [4, 8] {
        if gpu.device().caps().format_sample_counts(Format::Rgba8Unorm) & samples == 0 {
            continue;
        }
        let msaa = image(
            &gpu,
            17,
            19,
            Format::Rgba8Unorm,
            samples,
            1,
            Usage::COLOR | Usage::SAMPLED,
        );
        let mut draw_msaa = draw(
            17,
            19,
            vertex.clone(),
            fragment.clone(),
            [color(&msaa, Load::Clear([0.; 4]))],
        );
        draw_msaa.samples = samples;
        draw_msaa.arguments = DrawArguments::Direct {
            vertices: 0..quads.len() as u32 * 6,
            instances: 0..1,
            base_vertex: 0,
        };
        simple(&mut gpu, draw_msaa);
        gpu.resolve(msaa.view().clone(), resolved.view().clone())
            .unwrap();

        let mut slots = SlotBindings::default();
        slots.images.set(111, msaa.view().clone());
        let load = shader(
            "
@group(1) @binding(111) var tex: texture_multisampled_2d<f32>;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    var value = vec4(0.);
    let n = textureNumSamples(tex);
    for (var i = 0u; i < n; i++) { value += textureLoad(tex, vec2<i32>(p.xy), i); }
    return value / f32(n);
}",
            Stage::Fragment,
        );
        let sampled = target(&gpu, 17, 19, Format::Rgba8Unorm);
        gpu.draw_modern(
            draw(
                17,
                19,
                shader(TRIANGLE, Stage::Vertex),
                load,
                [color(&sampled, Load::DontCare)],
            ),
            &SlotBindings::default(),
            &slots,
        )
        .unwrap();
        for image in [&resolved, &sampled] {
            let pixels = read_image(&mut gpu, image, 0, 4);
            for (k, value) in oracle(&format!("msaa{samples}-xsweep-row0"), 0).enumerate() {
                let expected = bits(value).to_le_bytes();
                for (got, want) in pixels[k * 4..k * 4 + 4].iter().zip(expected) {
                    assert!(
                        got.abs_diff(want) <= 1,
                        "x coverage at {samples}x, {k}: {got} vs {want}"
                    );
                }
            }
            for (k, value) in oracle(&format!("msaa{samples}-ysweep-col0"), 0).enumerate() {
                let got = pixels[(k + 2) * 17 * 4];
                let want = bits(value) as u8;
                assert!(
                    got.abs_diff(want) <= 1,
                    "y coverage at {samples}x, {k}: {got} vs {want}"
                );
            }
        }
    }
}

#[test]
#[cfg(target_vendor = "apple")]
fn native_multisample_values_match_the_standard_positions() {
    let Some(mut gpu) = executor() else { return };
    let format = Format::Rgba8Unorm;
    let caps = gpu.device().caps();
    let sample_counts = caps.format_sample_counts(format);
    let vertex = shader(
        "
struct Vertex {
    @builtin(position) position: vec4<f32>,
    @location(0) xy: vec2<f32>,
}
@vertex fn main(@builtin(vertex_index) i: u32) -> Vertex {
    let p = array(vec2(0., 3.), vec2(0., -1.), vec2(4., -1.))[i];
    return Vertex(vec4(p, 0.5, 1.), (p * vec2(1., -1.) + 1.) * 0.5);
}",
        Stage::Vertex,
    );
    let fragment = shader(
        "
struct Pixel {
    @location(0) color: vec4<f32>,
    @builtin(sample_mask) mask: u32,
}
@fragment fn main(@location(0) @interpolate(linear, sample) xy: vec2<f32>,
                  @builtin(sample_index) i: u32) -> Pixel {
    return Pixel(vec4(f32(i + 1u), round(xy * 16.), 0.) / 255., 0x77u);
}",
        Stage::Fragment,
    );
    let load = shader(
        "
@group(1) @binding(0) var source: texture_multisampled_2d<f32>;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(source, vec2(0i), u32(p.x));
}",
        Stage::Fragment,
    );
    let p4 = [[6, 2], [14, 6], [2, 10], [10, 14]];
    let p8 = [
        [9, 5],
        [7, 11],
        [13, 9],
        [5, 3],
        [3, 13],
        [1, 7],
        [11, 15],
        [15, 1],
    ];
    for positions in [p4.as_slice(), p8.as_slice()] {
        let samples = positions.len() as u32;
        if sample_counts & samples == 0 {
            continue;
        }
        let usage = Usage::COLOR | Usage::SAMPLED | Usage::COPY_SRC | Usage::COPY_DST;
        let msaa = image(&gpu, 1, 1, format, samples, 1, usage);
        let copy = image(&gpu, 1, 1, format, samples, 1, usage);
        let mut draw_msaa = draw(
            1,
            1,
            vertex.clone(),
            fragment.clone(),
            [color(&msaa, Load::Clear([0.; 4]))],
        );
        draw_msaa.samples = samples;
        simple(&mut gpu, draw_msaa);
        let region = |image: Image| ImageRegion {
            extent: image.desc().size,
            image,
            mip: 0,
            layer: 0,
            origin: (0, 0, 0),
        };
        gpu.transfer(ModernTransfer::CopyImage {
            source: region(msaa),
            destination: region(copy.clone()),
        })
        .unwrap();
        let sampled = target(&gpu, samples, 1, format);
        gpu.draw_modern(
            draw(
                samples,
                1,
                shader(TRIANGLE, Stage::Vertex),
                load.clone(),
                [color(&sampled, Load::DontCare)],
            ),
            &SlotBindings::default(),
            &SlotBindings {
                images: [copy.view().clone()].into(),
                ..Default::default()
            },
        )
        .unwrap();
        let resolved = target(&gpu, 1, 1, format);
        gpu.resolve(copy.view().clone(), resolved.view().clone())
            .unwrap();

        let expected: Vec<_> = positions
            .iter()
            .enumerate()
            .map(|(i, &[x, y])| {
                // The triangle covers the right half of the pixel.
                if x >= 8 && 0x77 & (1 << i) != 0 {
                    [i as u8 + 1, x, y, 0]
                } else {
                    [0; 4]
                }
            })
            .collect();
        assert_eq!(
            read_image(&mut gpu, &sampled, 0, 4),
            expected.as_flattened()
        );
        for (channel, actual) in read_image(&mut gpu, &resolved, 0, 4)
            .into_iter()
            .enumerate()
        {
            let sum = expected
                .iter()
                .map(|pixel| u32::from(pixel[channel]))
                .sum::<u32>();
            assert!((f64::from(actual) - f64::from(sum) / f64::from(samples)).abs() <= 0.6);
        }
    }
}

#[test]
#[cfg(target_vendor = "apple")]
fn metal_sample_mask_output_reduces_input_coverage() {
    let Some(mut gpu) = executor() else { return };
    let format = Format::Rgba8Unorm;
    let msaa = image(&gpu, 1, 1, format, 4, 1, Usage::COLOR | Usage::SAMPLED);
    let sampled = target(&gpu, 4, 1, format);
    let edge = shader(
        "
@vertex fn main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = array(vec2(0., 3.), vec2(0., -1.), vec2(4., -1.));
    return vec4(p[i], 0.5, 1.);
}",
        Stage::Vertex,
    );
    let load = shader(
        "
@group(1) @binding(0) var source: texture_multisampled_2d<f32>;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(source, vec2(0i), u32(p.x));
}
",
        Stage::Fragment,
    );
    let vertex = shader(TRIANGLE, Stage::Vertex);
    let slots = SlotBindings {
        images: [msaa.view().clone()].into(),
        ..Default::default()
    };
    // The edge covers samples 1 and 3. D3D11 exposes both bits to each
    // invocation even when the shader masks out sample 3.
    for (fragment, expected) in [
        (
            "@fragment fn main(@builtin(sample_index) i: u32, @builtin(sample_mask) mask: u32)
                -> @location(0) vec4<f32> {
                return vec4(f32(i + 1u), f32(mask), 0., 0.) / 255.;
            }",
            [0, 0, 0, 0, 2, 10, 0, 0, 0, 0, 0, 0, 4, 10, 0, 0],
        ),
        (
            "struct Pixel { @location(0) color: vec4<f32>, @builtin(sample_mask) mask: u32 }
            @fragment fn main(@builtin(sample_index) i: u32, @builtin(sample_mask) mask: u32)
                -> Pixel {
                return Pixel(vec4(f32(i + 1u), f32(mask), 0., 0.) / 255., 0x77u);
            }",
            [0, 0, 0, 0, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        ),
    ] {
        let mut raster = draw(
            1,
            1,
            edge.clone(),
            shader(fragment, Stage::Fragment),
            [color(&msaa, Load::Clear([0.; 4]))],
        );
        raster.samples = 4;
        simple(&mut gpu, raster);
        gpu.draw_modern(
            draw(
                4,
                1,
                vertex.clone(),
                load.clone(),
                [color(&sampled, Load::DontCare)],
            ),
            &SlotBindings::default(),
            &slots,
        )
        .unwrap();
        assert_eq!(read_image(&mut gpu, &sampled, 0, 4), expected);
    }
}

// spec: GFX-PASS-1 GFX-PASS-3
#[test]
fn mrt_compute_boundaries_predication_and_queries() {
    let Some(mut gpu) = executor() else { return };
    let storage = image(
        &gpu,
        2,
        2,
        Format::R32Uint,
        1,
        1,
        Usage::STORAGE | Usage::SAMPLED,
    );
    let depth = image(
        &gpu,
        2,
        2,
        Format::Depth32FloatStencil8,
        1,
        1,
        Usage::DEPTH_STENCIL,
    );
    let outputs = std::array::from_fn::<_, 8, _>(|_| target(&gpu, 2, 2, Format::R32Uint));
    let cs = shader(
        "
@group(3) @binding(7) var img: texture_storage_2d<r32uint, write>;
@compute @workgroup_size(1) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    textureStore(img, vec2<i32>(id.xy), vec4<u32>(37u));
}",
        Stage::Compute,
    );
    let dispatch = gpu.prepare_dispatch(cs).unwrap();
    let mut slots = SlotBindings::default();
    slots
        .storage
        .set(7, StorageBinding::Image(storage.view().clone()));
    let vertex = shader(TRIANGLE, Stage::Vertex);
    let fragment = shader(
        "
@group(1) @binding(99) var img: texture_2d<u32>;
struct Output {
    @location(0) a: u32, @location(1) b: u32, @location(2) c: u32, @location(3) d: u32,
    @location(4) e: u32, @location(5) f: u32, @location(6) g: u32, @location(7) h: u32,
}
@fragment fn main(@builtin(position) pos: vec4<f32>) -> Output {
    let v = textureLoad(img, vec2<i32>(pos.xy), 0).r;
    return Output(v, v+1, v+2, v+3, v+4, v+5, v+6, v+7);
}",
        Stage::Fragment,
    );
    let legacy = gpu.target(2, 2, true).unwrap();
    gpu.clear(&legacy, Some([0., 0., 1., 1.]), Some(0.25))
        .unwrap();
    gpu.dispatch(&dispatch, &slots, DispatchArguments::Direct([2, 2, 1]))
        .unwrap();
    let indirect = buffer(&gpu, 12, BufferUsage::INDIRECT, Memory::Upload);
    // SAFETY: the newly allocated upload buffer has no GPU users yet.
    unsafe {
        indirect
            .mapped()
            .unwrap()
            .as_mut()
            .copy_from_slice(bytemuck::cast_slice(&[2u32, 2, 1]));
    }
    indirect.flush(std::iter::once(0..12));
    gpu.dispatch(
        &dispatch,
        &slots,
        DispatchArguments::Indirect(BufferRange {
            buffer: indirect,
            range: 0..12,
        }),
    )
    .unwrap();

    let mut legacy_state = crate::gpu::DrawState::new(2, 2);
    legacy_state.depth = Some(crate::gpu::Compare::Less);
    let vertices = [[-1., -1.], [3., -1.], [-1., 3.]].map(|[x, y]| crate::gpu::Vertex {
        position: [x, y, 0.5, 1.],
        color: [1., 0., 0., 1.],
        uv: [0.; 2],
    });
    gpu.draw(&legacy, &vertices, None, legacy_state).unwrap();

    let occlusion = Arc::new(
        gpu.device()
            .create_query_set(QueryKind::Occlusion, 2)
            .unwrap(),
    );
    let query = |index| QuerySlot {
        set: occlusion.clone(),
        index,
    };
    let mut sampled = SlotBindings::default();
    sampled.images.set(99, storage.view().clone());
    let make_draw = |compare, clear| {
        let mut draw = draw(
            2,
            2,
            vertex.clone(),
            fragment.clone(),
            outputs.iter().map(|o| {
                color(
                    o,
                    if clear {
                        Load::Clear([0.; 4])
                    } else {
                        Load::Load
                    },
                )
            }),
        );
        draw.state_ids[3] = if compare == Compare::Never { 1 } else { 2 };
        draw.depth = Some(ModernDepth {
            view: depth.view().clone(),
            state: DepthStencil {
                format: Format::Depth32FloatStencil8,
                depth_compare: compare,
                depth_write: false,
                stencil: None,
                stencil_read_mask: 0,
                stencil_write_mask: 0,
                depth_bias: false,
            },
            depth: (
                if clear { Load::Clear(1.) } else { Load::Load },
                Store::Store,
            ),
            stencil: (
                if clear { Load::Clear(0) } else { Load::Load },
                Store::Store,
            ),
            read_only: false,
        });
        draw
    };
    let mut hidden = make_draw(Compare::Never, true);
    hidden.occlusion = Some(query(0));
    gpu.draw_modern(hidden, &SlotBindings::default(), &sampled)
        .unwrap();
    let mut rejected = make_draw(Compare::Always, false);
    rejected.predicate = Some(Predicate {
        query: query(0),
        inverted: false,
    });
    gpu.draw_modern(rejected, &SlotBindings::default(), &sampled)
        .unwrap();
    assert_eq!(read_image(&mut gpu, &outputs[0], 0, 4), [0; 16]);

    let mut visible = make_draw(Compare::Always, false);
    visible.predicate = Some(Predicate {
        query: query(0),
        inverted: true,
    });
    visible.occlusion = Some(query(1));
    gpu.draw_modern(visible, &SlotBindings::default(), &sampled)
        .unwrap();
    let result = buffer(&gpu, 16, BufferUsage::COPY_DST, Memory::Readback);
    gpu.read_query(query(0), result.clone(), 0).unwrap();
    gpu.read_query(query(1), result.clone(), 8).unwrap();
    synchronize(&mut gpu);

    assert_eq!(mapped(&result), bytemuck::cast_slice::<_, u8>(&[0u64, 4]));
    assert_eq!(read(&mut gpu, &legacy.image), [255, 0, 0, 255].repeat(4));
    for (i, image) in outputs.iter().enumerate() {
        assert_eq!(
            read_image(&mut gpu, image, 0, 4),
            (37 + i as u32).to_le_bytes().repeat(4)
        );
    }
    assert!(gpu.stats().pipeline_cache_hits >= 1);

    if gpu
        .device()
        .caps()
        .features
        .contains(adv_gpu::Features::TIMESTAMPS)
    {
        let set = Arc::new(
            gpu.device()
                .create_query_set(QueryKind::Timestamp, 4)
                .unwrap(),
        );
        let stamp = |index| QuerySlot {
            set: set.clone(),
            index,
        };
        let disjoint = gpu.timestamp_disjoint(Some(&set));
        assert!(!disjoint.disjoint && disjoint.frequency > 0);
        gpu.timestamp(stamp(0)).unwrap();
        gpu.timestamp(stamp(2)).unwrap();
        gpu.read_query(stamp(1), result.clone(), 0).unwrap();
        gpu.read_query(stamp(3), result.clone(), 8).unwrap();
        synchronize(&mut gpu);
        let timestamps: &[u64] = bytemuck::cast_slice(mapped(&result));
        assert!(
            timestamps[0] > 0 && timestamps[1] >= timestamps[0],
            "{timestamps:?}"
        );
    }
}

fn sampler(
    gpu: &Gpu,
    address: adv_gpu::resource::Address,
    filter: adv_gpu::resource::Filter,
    compare: Option<Compare>,
) -> Sampler {
    use adv_gpu::resource::{BorderColor, Filter, SamplerDesc};
    gpu.device()
        .sampler(&SamplerDesc {
            min: filter,
            mag: filter,
            mip: Filter::Nearest,
            address: [address; 3],
            border: BorderColor::TransparentBlack,
            lod_bias: 0,
            lod_min: 0,
            lod_max: u16::MAX,
            anisotropy: 1,
            compare,
        })
        .unwrap()
}

fn sample(
    gpu: &mut Gpu,
    view: ImageView,
    sampler: Option<Sampler>,
    source: &str,
    width: u32,
) -> Vec<f32> {
    let out = target(gpu, width, 1, Format::Rgba32Float);
    let slots = SlotBindings {
        images: [view].into(),
        samplers: sampler.map(|s| [s].into()).unwrap_or_default(),
        ..Default::default()
    };
    gpu.draw_modern(
        draw(
            width,
            1,
            shader(TRIANGLE, Stage::Vertex),
            shader(source, Stage::Fragment),
            [color(&out, Load::DontCare)],
        ),
        &SlotBindings::default(),
        &slots,
    )
    .unwrap();
    read_image(gpu, &out, 0, 16)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect()
}

fn compare_samples(name: &str, actual: &[f32], tolerance: f32) {
    for (p, values) in oracle(name, 0).enumerate() {
        for (c, expected) in values.split(',').enumerate() {
            let expected = f32::from_bits(bits(expected));
            let got = actual[p * 4 + c];
            if tolerance == 0. && name.starts_with("bc") {
                // Compare the decoded UNORM8 code. Division by 255 and
                // multiplication by its reciprocal can differ by one float ULP.
                assert_eq!(
                    (got * 255.).round(),
                    (expected * 255.).round(),
                    "{name}[{p}][{c}]"
                );
                continue;
            }
            assert!(
                (got - expected).abs() <= tolerance,
                "{name}[{p}][{c}] = {got}; expected {expected}"
            );
        }
    }
}

#[test]
fn sampling_probe_address_srgb_and_compression() {
    use adv_gpu::resource::{Address, Filter};
    let Some(mut gpu) = executor() else { return };
    let texels = image(
        &gpu,
        4,
        1,
        Format::Rgba8Unorm,
        1,
        1,
        Usage::SAMPLED | Usage::COPY_DST,
    );
    upload(
        &mut gpu,
        &texels,
        0,
        16,
        1,
        &[16, 80, 144, 208]
            .into_iter()
            .flat_map(|v| [v, 0, 0, 255])
            .collect::<Vec<_>>(),
    );
    let source = "
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(2) @binding(0) var s: sampler;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureSampleLevel(tex, s, vec2((floor(p.x)-31.5)/16., 0.5), 0.);
}";
    for (name, address) in [
        ("address-wrap-u=(i-32+0.5)/16", Address::Repeat),
        ("address-mirror", Address::Mirror),
        ("address-clamp", Address::Clamp),
        ("address-mirror-once", Address::MirrorOnce),
    ] {
        let s = sampler(&gpu, address, Filter::Nearest, None);
        let results = sample(&mut gpu, texels.view().clone(), Some(s), source, 64);
        compare_samples(name, &results, f32::EPSILON);
    }

    let srgb = gpu
        .device()
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: 256,
                height: 1,
                depth_or_layers: 1,
            },
            format: Format::Rgba8Unorm,
            mips: 1,
            samples: 1,
            usage: Usage::SAMPLED | Usage::COPY_DST,
            view_formats: &[Format::Rgba8Srgb],
            label: "sRGB probe",
        })
        .unwrap();
    let ramp = (0..=255u8).flat_map(|r| [r, 0, 0, 255]).collect::<Vec<_>>();
    upload(&mut gpu, &srgb, 0, 1024, 1, &ramp);
    let srgb_view = srgb
        .create_view(&ViewDesc {
            format: Some(Format::Rgba8Srgb),
            dimension: None,
            mips: 0..1,
            layers: 0..1,
            plane: 0,
        })
        .unwrap();
    for phase in 0..4 {
        let source = format!(
            "
@group(1) @binding(0) var tex: texture_2d<f32>;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {{
    return textureLoad(tex, vec2(i32(p.x)*4+{phase}, 0), 0);
}}"
        );
        let actual = sample(&mut gpu, srgb_view.clone(), None, &source, 64);
        let phase = if phase == 0 {
            String::new()
        } else {
            format!("+{phase}")
        };
        compare_samples(&format!("srgb-decode-texel-4i{phase}"), &actual, 0.5 / 255.);
    }
    let two = image(
        &gpu,
        2,
        1,
        Format::Rgba8Srgb,
        1,
        1,
        Usage::SAMPLED | Usage::COPY_DST,
    );
    upload(&mut gpu, &two, 0, 8, 1, &[0, 0, 0, 255, 255, 255, 255, 255]);
    let s = sampler(&gpu, Address::Clamp, Filter::Linear, None);
    let actual = sample(
        &mut gpu,
        two.view().clone(),
        Some(s),
        "
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(2) @binding(0) var s: sampler;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureSampleLevel(tex, s, vec2((0.5+floor(p.x)/64.)/2., 0.5), 0.);
}",
        64,
    );
    compare_samples(
        "srgb-filter-black-white-u=(0.5+i/64)/2",
        &actual,
        0.5 / 255.,
    );

    let mut indices = 0u64;
    for i in 0..16 {
        indices |= (i as u64 % 8) << (3 * i);
    }
    let bc4 = |a, b| {
        let mut block = [0; 8];
        block[..2].copy_from_slice(&[a, b]);
        block[2..].copy_from_slice(&indices.to_le_bytes()[..6]);
        block
    };
    let bc1 = [0, 0xf8, 0x1f, 0, 0xe4, 0xe4, 0xe4, 0xe4];
    let bc3 = [bc4(255, 0), bc1].concat();
    let mut bc7 = 1u128 << 6;
    for channel in 0..4 {
        bc7 |= 127 << (7 + 14 * channel);
    }
    bc7 |= 1 << 63;
    for i in 1..16 {
        bc7 |= (i as u128) << (64 + 4 * i);
    }
    for (name, format, data, tolerance) in [
        (
            "bc1-4color-red-blue",
            Format::Bc1Unorm,
            bc1.to_vec(),
            1. / 255.,
        ),
        (
            "bc1-3color-blue-red-transparent",
            Format::Bc1Unorm,
            vec![0x1f, 0, 0, 0xf8, 0xe4, 0xe4, 0xe4, 0xe4],
            0.,
        ),
        (
            "bc1-odd-endpoints-0x0841",
            Format::Bc1Unorm,
            vec![0x41, 0x08, 0, 0, 0xe4, 0xe4, 0xe4, 0xe4],
            1. / 255.,
        ),
        ("bc3-alpha8value-color4", Format::Bc3Unorm, bc3, 0.03),
        (
            "bc4-unorm-8value",
            Format::Bc4Unorm,
            bc4(255, 0).to_vec(),
            0.03,
        ),
        (
            "bc4-unorm-6value",
            Format::Bc4Unorm,
            bc4(0, 255).to_vec(),
            0.03,
        ),
        (
            "bc4-snorm-127-to-minus127",
            Format::Bc4Snorm,
            bc4(0x7f, 0x81).to_vec(),
            0.03,
        ),
        (
            "bc7-mode6-255-to-0-index-i",
            Format::Bc7Unorm,
            bc7.to_le_bytes().to_vec(),
            0.,
        ),
    ] {
        let tex = gpu
            .block_texture(ImageDesc {
                dimension: Dimension::D2,
                size: Extent {
                    width: 4,
                    height: 4,
                    depth_or_layers: 1,
                },
                format,
                mips: 1,
                samples: 1,
                usage: Usage::empty(),
                view_formats: &[],
                label: "BC probe",
            })
            .unwrap();
        gpu.upload_blocks(&tex, 0, 0, data.len() as u32, &data)
            .unwrap();
        let source = "
@group(1) @binding(0) var tex: texture_2d<f32>;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let i = i32(p.x);
    return textureLoad(tex, vec2(i%4, i/4), 0);
}";
        let actual = sample(&mut gpu, tex.image().view().clone(), None, source, 16);
        compare_samples(name, &actual, tolerance);
    }
}

#[test]
fn alpha_swizzle_vertex_and_fragment_banks_and_generate_mips() {
    let Some(mut gpu) = executor() else { return };
    let alpha = image(
        &gpu,
        2,
        2,
        Format::A8Unorm,
        1,
        1,
        Usage::SAMPLED | Usage::COPY_DST | Usage::COLOR,
    );
    upload(&mut gpu, &alpha, 0, 2, 2, &[191; 4]);
    let alpha_write = alpha
        .create_view(&ViewDesc {
            format: Some(Format::R8Unorm),
            dimension: None,
            mips: 0..1,
            layers: 0..1,
            plane: 0,
        })
        .unwrap();
    let mut alpha_target = color(&alpha, Load::DontCare);
    alpha_target.view = alpha_write;
    simple(
        &mut gpu,
        draw(
            2,
            2,
            shader(TRIANGLE, Stage::Vertex),
            shader(
                "@fragment fn main() -> @location(0) vec4<f32> { return vec4(191./255., 0., 0., 1.); }",
                Stage::Fragment,
            ),
            [alpha_target],
        ),
    );
    let mips = image(
        &gpu,
        4,
        4,
        Format::Rgba8Unorm,
        1,
        3,
        Usage::SAMPLED | Usage::COPY_DST | Usage::COPY_SRC,
    );
    upload(&mut gpu, &mips, 0, 16, 4, &[64, 128, 192, 255].repeat(16));
    gpu.generate_mips(mips.view().clone()).unwrap();

    let vertex = shader(
        "
@group(1) @binding(127) var a: texture_2d<f32>;
struct Output { @builtin(position) p: vec4<f32>, @location(0) alpha: vec4<f32> }
@vertex fn main(@builtin(vertex_index) id: u32) -> Output {
    let p = array(vec2(-1.,-1.), vec2(3.,-1.), vec2(-1.,3.));
    return Output(vec4(p[id], 0., 1.), textureLoad(a, vec2(0), 0));
}",
        Stage::Vertex,
    );
    let fragment = shader(
        "
@group(1) @binding(127) var color: texture_2d<f32>;
@fragment fn main(@location(0) a: vec4<f32>) -> @location(0) vec4<f32> {
    let v = textureLoad(color, vec2(0), 2);
    return vec4(v.rgb + a.rgb, a.a);
}",
        Stage::Fragment,
    );
    let mut vs = SlotBindings::default();
    vs.images.set(127, alpha.view().clone());
    let mut fs = SlotBindings::default();
    fs.images.set(127, mips.view().clone());
    let out = target(&gpu, 1, 1, Format::Rgba8Unorm);
    gpu.draw_modern(
        draw(1, 1, vertex, fragment, [color(&out, Load::DontCare)]),
        &vs,
        &fs,
    )
    .unwrap();
    assert_eq!(read_image(&mut gpu, &out, 0, 4), [64, 128, 192, 191]);

    let color_space = gpu
        .device()
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: 2,
                height: 2,
                depth_or_layers: 1,
            },
            format: Format::Rgba8Unorm,
            mips: 2,
            samples: 1,
            usage: Usage::COPY_SRC | Usage::COPY_DST | Usage::SAMPLED,
            view_formats: &[Format::Rgba8Srgb],
            label: "mip sRGB test",
        })
        .unwrap();
    upload(
        &mut gpu,
        &color_space,
        0,
        8,
        2,
        &[0, 0, 0, 255, 255, 255, 255, 255].repeat(2),
    );
    let srgb = color_space
        .create_view(&ViewDesc {
            format: Some(Format::Rgba8Srgb),
            dimension: None,
            mips: 0..2,
            layers: 0..1,
            plane: 0,
        })
        .unwrap();
    gpu.generate_mips(srgb).unwrap();
    let mip = read_image(&mut gpu, &color_space, 1, 4);
    assert!(
        mip[..3]
            .iter()
            .all(|&channel| (187..=188).contains(&channel)),
        "{mip:?}"
    );
    assert_eq!(mip[3], 255);
}

#[test]
fn pixel_probe_render_target_conversions() {
    let Some(mut gpu) = executor() else { return };
    let vertex = shader(TRIANGLE, Stage::Vertex);
    let fragment = shader(
        "
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let values = array(0.5, 0.49999, 127.5/255., 127.49/255., 127.51/255., 0.5/255.,
                       -0.25, 2., bitcast<f32>(0x7fc00000u), 1e-9);
    return vec4(values[u32(p.x)]);
}",
        Stage::Fragment,
    );
    for (name, format, bpp, widths) in [
        ("rgba8", Format::Rgba8Unorm, 4, &[8, 8, 8, 8][..]),
        ("rgba8-srgb", Format::Rgba8Srgb, 4, &[8, 8, 8, 8][..]),
        ("r16unorm", Format::R16Unorm, 2, &[16][..]),
        ("r8snorm", Format::R8Snorm, 1, &[8][..]),
        ("b5g6r5", Format::B5g6r5Unorm, 2, &[5, 6, 5][..]),
        ("rgb10a2", Format::Rgb10a2Unorm, 4, &[10, 10, 10, 2][..]),
        ("r11g11b10f", Format::Rg11b10Float, 4, &[11, 11, 10][..]),
        ("r16f", Format::R16Float, 2, &[16][..]),
    ] {
        if !gpu
            .device()
            .caps()
            .format(format)
            .contains(adv_gpu::FormatCaps::COLOR)
        {
            continue;
        }
        let output = target(&gpu, 10, 1, format);
        simple(
            &mut gpu,
            draw(
                10,
                1,
                vertex.clone(),
                fragment.clone(),
                [color(&output, Load::DontCare)],
            ),
        );
        let converted = read_image(&mut gpu, &output, 0, bpp);
        for (i, expected) in oracle(&format!("convert-{name}"), 0).map(bits).enumerate() {
            let mut got = [0; 4];
            got[..bpp as usize].copy_from_slice(&converted[i * bpp as usize..][..bpp as usize]);
            let expected = if name.starts_with("rgba8") {
                expected.swap_bytes()
            } else {
                expected
            };
            compare_conversion(name, i, u32::from_le_bytes(got), expected, widths);
        }
        let mut clear = draw(
            10,
            1,
            vertex.clone(),
            fragment.clone(),
            [color(&output, Load::Clear([0.5; 4]))],
        );
        clear.arguments = DrawArguments::Direct {
            vertices: 0..0,
            instances: 0..1,
            base_vertex: 0,
        };
        simple(&mut gpu, clear);
        let cleared = read_image(&mut gpu, &output, 0, bpp);
        let mut got = [0; 4];
        got[..bpp as usize].copy_from_slice(&cleared[..bpp as usize]);
        let expected = bits(oracle(&format!("clear-127.5-{name}"), 0).next().unwrap());
        let expected = if name.starts_with("rgba8") {
            expected.swap_bytes()
        } else {
            expected
        };
        compare_conversion(name, 2, u32::from_le_bytes(got), expected, widths);
    }
}

fn compare_conversion(name: &str, input: usize, mut got: u32, mut expected: u32, widths: &[u32]) {
    for &width in widths {
        let mask = (1 << width) - 1;
        let x = got & mask;
        let y = expected & mask;
        let float_mantissa = match name {
            "r11g11b10f" => Some(width - 5),
            "r16f" => Some(10),
            _ => None,
        };
        if let Some(mantissa) = float_mantissa.filter(|_| input == 8) {
            assert_eq!((x >> mantissa) & 31, 31, "{name} NaN exponent");
            assert_ne!(x & ((1 << mantissa) - 1), 0, "{name} NaN payload");
        } else {
            // Nearest-code disagreements are allowed around a half step.
            let tolerance = u32::from(matches!(input, 0..=5));
            assert!(
                x.abs_diff(y) <= tolerance,
                "{name}[{input}]: {x:#x} vs {y:#x}"
            );
        }
        got >>= width;
        expected >>= width;
    }
}

fn depth_state(image: &Image, value: Load<f32>, stencil: Load<u32>) -> ModernDepth {
    ModernDepth {
        view: image.view().clone(),
        state: DepthStencil {
            format: image.desc().format,
            depth_compare: Compare::Always,
            depth_write: true,
            stencil: None,
            stencil_read_mask: 255,
            stencil_write_mask: 255,
            depth_bias: false,
        },
        depth: (value, Store::Store),
        stencil: (stencil, Store::Store),
        read_only: false,
    }
}

fn sample_aspect(image: &Image, plane: u32) -> ImageView {
    image
        .create_view(&ViewDesc {
            format: None,
            dimension: None,
            mips: 0..1,
            layers: 0..1,
            plane,
        })
        .unwrap()
}

#[test]
fn sampling_probe_comparison_depth_and_stencil() {
    use adv_gpu::pipeline::{StencilFace, StencilOp};
    use adv_gpu::resource::{Address, Filter};
    let Some(mut gpu) = executor() else { return };
    let vertex = shader(TRIANGLE, Stage::Vertex);
    let depth = image(
        &gpu,
        2,
        1,
        Format::Depth32Float,
        1,
        1,
        Usage::DEPTH_STENCIL | Usage::SAMPLED,
    );
    let write_depth = shader(
        "
@fragment fn main(@builtin(position) p: vec4<f32>) -> @builtin(frag_depth) f32 {
    return select(0.25, 0.75, p.x >= 1.);
}",
        Stage::Fragment,
    );
    let mut initialize = draw(2, 1, vertex.clone(), write_depth, []);
    initialize.depth = Some(depth_state(&depth, Load::DontCare, Load::None));
    simple(&mut gpu, initialize);
    for (name, filter, op, width, coord, reference) in [
        (
            "cmp-point-less-ref=i/32-at-texel0",
            Filter::Nearest,
            Compare::Less,
            33,
            "0.25",
            "floor(p.x)/32.",
        ),
        (
            "cmp-point-lessequal-ref=i/32-at-texel0",
            Filter::Nearest,
            Compare::LessEqual,
            33,
            "0.25",
            "floor(p.x)/32.",
        ),
        (
            "cmp-linear-less-ref0.5-u=(i)/64",
            Filter::Linear,
            Compare::Less,
            64,
            "floor(p.x)/64.",
            "0.5",
        ),
    ] {
        let lookup = if filter == Filter::Linear {
            // The Metal comparison sampler quantizes the filtered result to
            // UNORM8. Point comparisons and linear weights reproduce D3D's
            // compare-before-filtering rule exactly for this two-texel probe.
            format!(
                "
    let texel = ({coord}) * 2. - 0.5;
    let a = (floor(texel) + 0.5) / 2.;
    let b = a + 0.5;
    let ca = textureSampleCompareLevel(tex, s, vec2(a, 0.5), {reference});
    let cb = textureSampleCompareLevel(tex, s, vec2(b, 0.5), {reference});
    return vec4(mix(ca, cb, fract(texel)));"
            )
        } else {
            format!(
                "return vec4(textureSampleCompareLevel(tex, s, vec2({coord}, 0.5), {reference}));"
            )
        };
        let source = format!(
            "
@group(1) @binding(0) var tex: texture_depth_2d;
@group(2) @binding(0) var s: sampler_comparison;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {{
    {lookup}
}}"
        );
        let s = sampler(&gpu, Address::Clamp, Filter::Nearest, Some(op));
        let actual = sample(&mut gpu, depth.view().clone(), Some(s), &source, width);
        compare_samples(name, &actual, 0.);
    }

    let mut depths = std::array::from_fn::<_, 64, _>(|i| i as f32 / 63.);
    depths[..10].copy_from_slice(&[
        0.5,
        1. / 3.,
        0.5 + 0.5 / 65535.,
        0.5 + 0.49 / 65535.,
        0.5 + 0.5 / 16777215.,
        0.5 + 0.49 / 16777215.,
        1. - 1. / 16777216.,
        1e-30,
        1.5,
        -0.5,
    ]);
    let inputs = depths
        .iter()
        .map(|z| format!("{}u", z.to_bits()))
        .collect::<Vec<_>>()
        .join(", ");
    let write_depth = shader(
        &format!(
            "
@fragment fn main(@builtin(position) p: vec4<f32>) -> @builtin(frag_depth) f32 {{
    let depths = array({inputs});
    return clamp(bitcast<f32>(depths[u32(p.x)]), 0., 1.);
}}"
        ),
        Stage::Fragment,
    );
    let load = "
@group(1) @binding(0) var depth: texture_depth_2d;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4(textureLoad(depth, vec2(i32(p.x), 0), 0));
}";
    for (name, format, epsilon) in [
        ("d16", Format::Depth16Unorm, 1. / 65535.),
        ("d32f", Format::Depth32Float, 0.),
        ("d32fs8", Format::Depth32FloatStencil8, 0.),
    ] {
        let depth = image(
            &gpu,
            64,
            1,
            format,
            1,
            1,
            Usage::DEPTH_STENCIL | Usage::SAMPLED,
        );
        let mut raster = draw(64, 1, vertex.clone(), write_depth.clone(), []);
        raster.depth = Some(depth_state(&depth, Load::DontCare, Load::None));
        simple(&mut gpu, raster);
        let view = if format.has_stencil() {
            sample_aspect(&depth, 2)
        } else {
            depth.view().clone()
        };
        let actual = sample(&mut gpu, view.clone(), None, load, 64);
        compare_samples(
            &format!(
                "depth-{name}-svdepth-0.5,1/3,0.5+half16,0.5+0.49_16,0.5+half24,0.5+0.49_24,1-2^-24,1e-30,1.5,-0.5,i/63"
            ),
            &actual,
            epsilon,
        );
        let mut clear = draw(64, 1, vertex.clone(), write_depth.clone(), []);
        clear.arguments = DrawArguments::Direct {
            vertices: 0..0,
            instances: 0..1,
            base_vertex: 0,
        };
        clear.depth = Some(depth_state(&depth, Load::Clear(1. / 3.), Load::None));
        simple(&mut gpu, clear);
        let actual = sample(&mut gpu, view, None, load, 64);
        compare_samples(&format!("depth-{name}-clear-1/3"), &actual, epsilon);
    }

    for (format_name, format) in [
        ("d32fs8", Format::Depth32FloatStencil8),
        ("d24s8", Format::Depth24UnormStencil8),
    ] {
        let depth = image(
            &gpu,
            1,
            1,
            format,
            1,
            1,
            Usage::DEPTH_STENCIL | Usage::SAMPLED,
        );
        let stencil_view = sample_aspect(&depth, 1);
        let fragment = shader("@fragment fn main() {}", Stage::Fragment);
        for (id, (name, op, from, reference, mask, instances)) in [
            (
                "incrsat-from-fe-x2",
                StencilOp::IncrementClamp,
                0xfe,
                0,
                0xff,
                2,
            ),
            (
                "incr-from-fe-x2",
                StencilOp::IncrementWrap,
                0xfe,
                0,
                0xff,
                2,
            ),
            (
                "decrsat-from-1-x2",
                StencilOp::DecrementClamp,
                1,
                0,
                0xff,
                2,
            ),
            ("decr-from-1-x2", StencilOp::DecrementWrap, 1, 0, 0xff, 2),
            ("invert-from-5a", StencilOp::Invert, 0x5a, 0, 0xff, 1),
            (
                "replace-ff-writemask-0f-from-0",
                StencilOp::Replace,
                0,
                0xff,
                0x0f,
                1,
            ),
            (
                "incr-writemask-f0-from-0f",
                StencilOp::IncrementWrap,
                0x0f,
                0,
                0xf0,
                1,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let mut draw = draw(1, 1, vertex.clone(), fragment.clone(), []);
            draw.state_ids[3] = 1 + id as u64;
            draw.stencil_reference = reference;
            draw.arguments = DrawArguments::Direct {
                vertices: 0..3,
                instances: 0..instances,
                base_vertex: 0,
            };
            let mut state = depth_state(&depth, Load::Clear(1.), Load::Clear(from));
            let face = StencilFace {
                compare: Compare::Always,
                fail: StencilOp::Keep,
                depth_fail: StencilOp::Keep,
                pass: op,
            };
            state.state.depth_write = false;
            state.state.stencil = Some((face, face));
            state.state.stencil_write_mask = mask;
            draw.depth = Some(state);
            simple(&mut gpu, draw);
            let actual = sample(
                &mut gpu,
                stencil_view.clone(),
                None,
                "
@group(1) @binding(0) var stencil: texture_2d<u32>;
@fragment fn main() -> @location(0) vec4<f32> {
    return vec4(f32(textureLoad(stencil, vec2(0), 0).g));
}",
                1,
            );
            let expected = bits(
                oracle(&format!("stencil-{format_name}-{name}"), 0)
                    .next()
                    .unwrap(),
            );
            assert_eq!(actual[0] as u32, expected, "{name}");
        }
    }
}

#[test]
fn sampling_probe_mip_selection_and_clamp() {
    use adv_gpu::resource::{Address, BorderColor, Filter, SamplerDesc};
    let Some(mut gpu) = executor() else { return };
    let mips = image(
        &gpu,
        64,
        64,
        Format::Rgba8Unorm,
        1,
        7,
        Usage::SAMPLED | Usage::COPY_DST,
    );
    for mip in 0..7 {
        let edge = 64 >> mip;
        let pixels = [16 + 32 * mip as u8, 0, 0, 255].repeat((edge * edge) as usize);
        upload(&mut gpu, &mips, mip, edge * 4, edge, &pixels);
    }
    for (name, filter, bias, min, max, isotropic) in [
        (
            "lod-mip-point-lambda=i/8",
            Filter::Nearest,
            0.,
            0,
            u16::MAX,
            true,
        ),
        (
            "lod-mip-linear-lambda=i/8",
            Filter::Linear,
            0.,
            0,
            u16::MAX,
            true,
        ),
        (
            "lod-mip-point-bias1",
            Filter::Nearest,
            1.,
            0,
            u16::MAX,
            true,
        ),
        (
            "lod-mip-linear-minlod1.5-maxlod3",
            Filter::Linear,
            0.,
            384,
            768,
            true,
        ),
        (
            "lod-mip-point-bias-0.25",
            Filter::Nearest,
            -0.25,
            0,
            u16::MAX,
            true,
        ),
        (
            "lod-anisotropic-gradient-x=2^(i/8)/64-y=1/64",
            Filter::Linear,
            0.,
            0,
            u16::MAX,
            false,
        ),
    ] {
        // The sampler bias is in log space. Applying it to gradients is also
        // available on Metal families without a sampler bias field.
        let y = if isotropic { "g" } else { "1./64." };
        let sampling = if filter == Filter::Nearest {
            "return textureSampleLevel(tex, s, vec2(0.5), floor(lod + 0.5));".into()
        } else {
            format!(
                "let g = exp2(lod) / 64.; return textureSampleGrad(tex, s, vec2(0.5), vec2(g, 0.), vec2(0., {y}));"
            )
        };
        let source = format!(
            "
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(2) @binding(0) var s: sampler;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {{
    let lod = floor(p.x)/8. + {bias:?};
    {sampling}
}}"
        );
        let sampler = gpu
            .device()
            .sampler(&SamplerDesc {
                min: Filter::Nearest,
                mag: Filter::Nearest,
                mip: filter,
                address: [Address::Clamp; 3],
                border: BorderColor::TransparentBlack,
                lod_bias: 0,
                lod_min: min,
                lod_max: max,
                anisotropy: 1,
                compare: None,
            })
            .unwrap();
        let actual = sample(&mut gpu, mips.view().clone(), Some(sampler), &source, 56);
        let epsilon = if filter == Filter::Linear {
            // D3D11.3 §7.18.16.2 allows filtering at the format's precision.
            1. / 255.
        } else {
            f32::EPSILON
        };
        compare_samples(name, &actual, epsilon);
    }
}

#[test]
fn pixel_probe_texel_mapping_and_filter_weights() {
    use adv_gpu::resource::{Address, Filter};
    let Some(mut gpu) = executor() else { return };
    let pattern = image(
        &gpu,
        4,
        4,
        Format::Rgba8Unorm,
        1,
        1,
        Usage::COPY_DST | Usage::SAMPLED,
    );
    let data = (0..4)
        .flat_map(|y| (0..4).flat_map(move |x| [x * 64 + 16, y * 64 + 16, 0, 255]))
        .collect::<Vec<_>>();
    upload(&mut gpu, &pattern, 0, 16, 4, &data);
    let output = target(&gpu, 32, 32, Format::Rgba8Unorm);
    for (name, start, end, lo, hi, filter, address) in [
        (
            "tex-1to1-int-point",
            0.,
            4.,
            0.,
            1.,
            Filter::Nearest,
            Address::Clamp,
        ),
        (
            "tex-1to1-plus-half-point",
            0.5,
            4.5,
            0.,
            1.,
            Filter::Nearest,
            Address::Clamp,
        ),
        (
            "tex-1to1-int-linear",
            0.,
            4.,
            0.,
            1.,
            Filter::Linear,
            Address::Clamp,
        ),
        (
            "tex-1to1-plus-half-linear",
            0.5,
            4.5,
            0.,
            1.,
            Filter::Linear,
            Address::Clamp,
        ),
        (
            "tex-2x-linear",
            0.,
            8.,
            0.,
            1.,
            Filter::Linear,
            Address::Clamp,
        ),
        (
            "tex-wrap-uv-minus-0.5-1.5",
            0.,
            8.,
            -0.5,
            1.5,
            Filter::Linear,
            Address::Repeat,
        ),
    ] {
        let vertex = quad_shader(&[[start, start, end, end]], 32., 32.);
        let fragment = shader(
            &format!(
                "
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(2) @binding(0) var s: sampler;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {{
    let uv = (p.xy - {start:?}) / ({end:?} - {start:?});
    return textureSampleLevel(tex, s, mix(vec2({lo:?}), vec2({hi:?}), uv), 0.);
}}"
            ),
            Stage::Fragment,
        );
        let mut draw = draw(
            32,
            32,
            vertex,
            fragment,
            [color(&output, Load::Clear([0.; 4]))],
        );
        draw.arguments = DrawArguments::Direct {
            vertices: 0..6,
            instances: 0..1,
            base_vertex: 0,
        };
        let s = sampler(&gpu, address, filter, None);
        let slots = SlotBindings {
            images: [pattern.view().clone()].into(),
            samplers: [s].into(),
            ..Default::default()
        };
        gpu.draw_modern(draw, &SlotBindings::default(), &slots)
            .unwrap();
        let result = read_image(&mut gpu, &output, 0, 4);
        for y in 0..8 {
            for (x, expected) in oracle(name, y).map(bits).enumerate() {
                let offset = (y * 32 + x) * 4;
                assert_eq!(
                    &result[offset..offset + 4],
                    &expected.to_be_bytes(),
                    "{name} ({x},{y})"
                );
            }
        }
    }
    let ramp = image(
        &gpu,
        2,
        1,
        Format::Rgba8Unorm,
        1,
        1,
        Usage::COPY_DST | Usage::SAMPLED,
    );
    upload(
        &mut gpu,
        &ramp,
        0,
        8,
        1,
        &[0, 0, 0, 255, 255, 255, 255, 255],
    );
    let output = target(&gpu, 64, 1, Format::Rgba8Unorm);
    let fragment = shader(
        "
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(2) @binding(0) var s: sampler;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureSampleLevel(tex, s, vec2(0.25 + floor(p.x)/128., 0.5), 0.);
}",
        Stage::Fragment,
    );
    let s = sampler(&gpu, Address::Clamp, Filter::Linear, None);
    let slots = SlotBindings {
        images: [ramp.view().clone()].into(),
        samplers: [s].into(),
        ..Default::default()
    };
    gpu.draw_modern(
        draw(
            64,
            1,
            shader(TRIANGLE, Stage::Vertex),
            fragment,
            [color(&output, Load::DontCare)],
        ),
        &SlotBindings::default(),
        &slots,
    )
    .unwrap();
    let result = read_image(&mut gpu, &output, 0, 4);
    for (x, expected) in oracle("filter-ramp-64-steps", 0).map(bits).enumerate() {
        let expected = expected.to_be_bytes();
        for (got, expected) in result[x * 4..x * 4 + 4].iter().zip(expected) {
            assert!(
                got.abs_diff(expected) <= 1,
                "bilinear ramp at {x}: {got} vs {expected}"
            );
        }
    }
}

#[test]
fn pixel_probe_output_merger() {
    use adv_gpu::pipeline::{BlendComponent, BlendOp, Factor};
    let Some(mut gpu) = executor() else { return };
    let blend = |src, dst| BlendComponent {
        src,
        dst,
        op: BlendOp::Add,
    };
    let add = blend(Factor::One, Factor::One);
    let rgba = target(&gpu, 32, 32, Format::Rgba8Unorm);
    let mut surface = color(&rgba, Load::Clear([0.; 4]));
    surface.blend = Some((add, add));
    let mut raster = draw(
        32,
        32,
        quad_shader(&[[0., 0., 8., 8.], [2.3, 1.7, 6.1, 6.6]], 32., 32.),
        shader(
            "@fragment fn main() -> @location(0) vec4<f32> { return vec4(0.25); }",
            Stage::Fragment,
        ),
        [surface],
    );
    raster.state_ids[2] = 1;
    raster.arguments = DrawArguments::Direct {
        vertices: 0..12,
        instances: 0..1,
        base_vertex: 0,
    };
    simple(&mut gpu, raster);
    let pixels = read_image(&mut gpu, &rgba, 0, 4);
    for y in 0..9 {
        for (x, expected) in oracle("shared-edges-additive-quarter", y)
            .map(bits)
            .enumerate()
        {
            let offset = (y * 32 + x) * 4;
            assert_eq!(
                &pixels[offset..offset + 4],
                &expected.to_be_bytes(),
                "shared edges ({x},{y})"
            );
        }
    }

    let alpha = shader(
        "
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let a = array(128./255., 127./255., 1./255., 0.5);
    return vec4(1., 1., 1., a[u32(p.x)]);
}",
        Stage::Fragment,
    );
    let multiply = shader(
        "@fragment fn main() -> @location(0) vec4<f32> { return vec4(vec3(128. / 255.), 1.); }",
        Stage::Fragment,
    );
    let over = blend(Factor::SrcAlpha, Factor::OneMinusSrcAlpha);
    let replace = blend(Factor::One, Factor::Zero);
    let dest = blend(Factor::Dst, Factor::Zero);
    for (name, format, bpp) in [
        ("rgba8", Format::Rgba8Unorm, 4),
        ("rgba8-srgb", Format::Rgba8Srgb, 4),
        ("b5g6r5", Format::B5g6r5Unorm, 2),
    ] {
        if !gpu
            .device()
            .caps()
            .format(format)
            .contains(adv_gpu::FormatCaps::BLEND)
        {
            continue;
        }
        let surface = target(&gpu, 6, 1, format);
        let base = f64::from(64. / 255_f32);
        let mut first = color(&surface, Load::Clear([base, base, base, 1.]));
        first.blend = Some((over, replace));
        let vertex = shader(TRIANGLE, Stage::Vertex);
        let mut raster = draw(6, 1, vertex.clone(), alpha.clone(), [first]);
        raster.state_ids[2] = 2;
        raster.scissor.w = 4;
        simple(&mut gpu, raster);

        let mut second = color(&surface, Load::Load);
        second.blend = Some((dest, replace));
        let mut raster = draw(6, 1, vertex, multiply.clone(), [second]);
        raster.state_ids[2] = 3;
        raster.scissor = Rect {
            x: 4,
            y: 0,
            w: 1,
            h: 1,
        };
        simple(&mut gpu, raster);
        let pixels = read_image(&mut gpu, &surface, 0, bpp);
        let probe = format!("blend-{name}-a128-a127-a1-a0.5-over-64-then-destcolor-128");
        for (i, value) in oracle(&probe, 0).map(bits).enumerate() {
            let offset = i * bpp as usize;
            if bpp == 4 {
                for (actual, expected) in pixels[offset..offset + 4].iter().zip(value.to_be_bytes())
                {
                    assert!(
                        actual.abs_diff(expected) <= 1,
                        "{name} blend {i}: {actual} vs {expected}"
                    );
                }
            } else {
                let actual = u16::from_le_bytes(pixels[offset..offset + 2].try_into().unwrap());
                for (shift, mask) in [(0, 31), (5, 63), (11, 31)] {
                    assert!(
                        ((actual >> shift) & mask).abs_diff(((value as u16) >> shift) & mask) <= 1,
                        "{name} blend {i}: {actual:#x} vs {value:#x}"
                    );
                }
            }
        }
    }
}

#[test]
fn pixel_probe_line_and_point_coverage() {
    let Some(mut gpu) = executor() else { return };
    let target = target(&gpu, 32, 32, Format::Rgba8Unorm);
    let fragment = shader(WHITE, Stage::Fragment);
    for (name, topology, points, edge) in [
        (
            "lines-1,1-6,4-and-1.5,6.5-6.5,1.5",
            Topology::Lines,
            &[[1., 1.], [6., 4.], [1.5, 6.5], [6.5, 1.5]][..],
            8,
        ),
        (
            "points-1.5,1.5-3,3-5.25,5.75",
            Topology::Points,
            &[[1.5, 1.5], [3., 3.], [5.25, 5.75]][..],
            7,
        ),
    ] {
        let positions = points
            .iter()
            .map(|[x, y]| format!("vec2({x:?}, {y:?})"))
            .collect::<Vec<_>>()
            .join(", ");
        let vertex = shader(
            &format!(
                "
@vertex fn main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {{
    let xy = array({positions});
    return vec4(xy[i] * vec2(0.0625, -0.0625) + vec2(-1., 1.), 0.5, 1.);
}}"
            ),
            Stage::Vertex,
        );
        let mut draw = draw(
            32,
            32,
            vertex,
            fragment.clone(),
            [color(&target, Load::Clear([0.; 4]))],
        );
        draw.primitive.topology = topology;
        draw.state_ids[0] = if topology == Topology::Lines { 1 } else { 2 };
        draw.arguments = DrawArguments::Direct {
            vertices: 0..points.len() as u32,
            instances: 0..1,
            base_vertex: 0,
        };
        simple(&mut gpu, draw);
        compare_coverage(&mut gpu, &target, name, edge, edge);
    }
}

#[test]
fn sampling_probe_custom_border() {
    let Some(mut gpu) = executor() else { return };
    let texels = image(
        &gpu,
        4,
        1,
        Format::Rgba8Unorm,
        1,
        1,
        Usage::SAMPLED | Usage::COPY_DST,
    );
    upload(
        &mut gpu,
        &texels,
        0,
        16,
        1,
        &[16, 80, 144, 208]
            .into_iter()
            .flat_map(|v| [v, 0, 0, 255])
            .collect::<Vec<_>>(),
    );
    let source = "
@group(1) @binding(0) var tex: texture_2d<f32>;
fn tap(x: i32) -> vec4<f32> {
    if x < 0 || x >= 4 { return round(vec4(1., 0.25, 0., 1.) * 255.) / 255.; }
    return textureLoad(tex, vec2(x, 0), 0);
}
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return tap(i32(floor((floor(p.x) - 31.5) / 4.)));
}";
    let actual = sample(&mut gpu, texels.view().clone(), None, source, 64);
    compare_samples("address-border-1,0.25,0,1", &actual, f32::EPSILON);
    let source = "
@group(1) @binding(0) var tex: texture_2d<f32>;
fn tap(x: i32) -> vec4<f32> {
    if x < 0 || x >= 4 { return round(vec4(1., 0.25, 0., 1.) * 255.) / 255.; }
    return textureLoad(tex, vec2(x, 0), 0);
}
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let u = (floor(p.x) - 8.) / 16. - 0.5;
    return mix(tap(i32(floor(u))), tap(i32(floor(u))+1), fract(u));
}";
    let actual = sample(&mut gpu, texels.view().clone(), None, source, 24);
    compare_samples("address-border-linear-edge-u=(i-8)/64", &actual, 0.6 / 255.);
}

#[test]
fn sampling_probe_depth_bias_clip_and_viewport_depth() {
    let Some(mut gpu) = executor() else { return };
    let fragment = shader("@fragment fn main() {}", Stage::Fragment);
    let load = "
@group(1) @binding(0) var dep: texture_depth_2d;
@fragment fn main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4(textureLoad(dep, vec2(i32(p.x), 0), 0));
}";
    for (name, format, tolerance) in [
        ("d16", Format::Depth16Unorm, 1. / 65535.),
        ("d32f", Format::Depth32Float, 0.),
        ("d32fs8", Format::Depth32FloatStencil8, 0.),
    ] {
        let depth = image(
            &gpu,
            64,
            1,
            format,
            1,
            1,
            Usage::DEPTH_STENCIL | Usage::SAMPLED,
        );
        let view = if format.has_stencil() {
            sample_aspect(&depth, 2)
        } else {
            depth.view().clone()
        };
        for (i, (test, [constant, slope, clamp], [z0, z1, z2], unclipped, [min, max])) in [
            ("bias1-at-0.5", [1., 0., 0.], [0.5; 3], false, [0., 1.]),
            ("bias100-at-0.5", [100., 0., 0.], [0.5; 3], false, [0., 1.]),
            ("bias-1-at-0.5", [-1., 0., 0.], [0.5; 3], false, [0., 1.]),
            (
                "slope2-sloped-at-0.5",
                [0., 2., 0.],
                [0.25, 1.25, 0.25],
                false,
                [0., 1.],
            ),
            (
                "bias100-clamp1e-5-at-0.5",
                [100., 0., 1e-5],
                [0.5; 3],
                false,
                [0., 1.],
            ),
            (
                "none-sloped-at-0.5",
                [0., 0., 0.],
                [0.25, 1.25, 0.25],
                false,
                [0., 1.],
            ),
            ("noclip-z1.5", [0., 0., 0.], [1.5; 3], true, [0., 1.]),
            (
                "viewport-0.25-0.75-z0",
                [0., 0., 0.],
                [0.; 3],
                false,
                [0.25, 0.75],
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let vertex = shader(
                &format!(
                    "
@vertex fn main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {{
    let p = array(vec3(-1., -1., {z0:?}), vec3(3., -1., {z1:?}), vec3(-1., 3., {z2:?}));
    return vec4(p[i], 1.);
}}"
                ),
                Stage::Vertex,
            );
            let mut draw = draw(64, 1, vertex, fragment.clone(), []);
            draw.state_ids[1] = i as u64;
            draw.state_ids[3] = i as u64;
            draw.primitive.unclipped_depth = unclipped;
            draw.viewport[4] = min;
            draw.viewport[5] = max;
            let mut depth_state = depth_state(&depth, Load::Clear(1.), Load::None);
            depth_state.state.depth_bias = constant != 0. || slope != 0.;
            draw.depth = Some(depth_state);
            draw.depth_bias = [constant, slope, clamp];
            simple(&mut gpu, draw);
            let actual = sample(&mut gpu, view.clone(), None, load, 64);
            compare_samples(&format!("depth-{name}-{test}"), &actual, tolerance);
        }
    }
}

// spec: GFX-CAP-4
#[test]
fn maximum_textures_samplers_constants_and_uavs_execute_together() {
    use adv_gpu::resource::{Address, Filter};
    use std::fmt::Write;

    let Some(mut gpu) = executor() else { return };
    let constants = buffer(&gpu, 14 * 65536, BufferUsage::UNIFORM, Memory::Upload);
    let mut slots = SlotBindings::default();
    let mut source = String::new();
    let mut sum = String::from("var value = 0u;\n");
    for i in 0..14 {
        let first = i * 65536;
        let last = first + 65536;
        let value = 100 + i as u32;
        // SAFETY: no commands reference the upload buffer until dispatch.
        unsafe {
            constants.mapped().unwrap().as_mut()[(last - 16) as usize..(last - 12) as usize]
                .copy_from_slice(&value.to_ne_bytes());
        }
        slots.uniforms.set(
            slots.uniforms.len(),
            BufferRange {
                buffer: constants.clone(),
                range: first..last,
            }
            .into(),
        );
        writeln!(
            source,
            "@group(0) @binding({i}) var<uniform> c{i}: array<vec4<u32>, 4096>;"
        )
        .unwrap();
        let weight = i as u32 + 1;
        writeln!(sum, "value += {weight}u * c{i}[4095].x;").unwrap();
    }
    constants.flush(std::iter::once(0..14 * 65536));

    for i in 0..128 {
        let input = image(
            &gpu,
            1,
            1,
            Format::Rgba8Unorm,
            1,
            1,
            Usage::SAMPLED | Usage::COPY_DST,
        );
        upload(&mut gpu, &input, 0, 4, 1, &[i as u8 + 1, 0, 0, 255]);
        slots.images.set(slots.images.len(), input.view().clone());
        writeln!(source, "@group(1) @binding({i}) var t{i}: texture_2d<f32>;").unwrap();
        if i < 16 {
            slots.samplers.set(
                slots.samplers.len(),
                sampler(&gpu, Address::Clamp, Filter::Nearest, None),
            );
            writeln!(source, "@group(2) @binding({i}) var s{i}: sampler;").unwrap();
        }
        let weight = i + 1;
        if i < 16 || (88..96).contains(&i) {
            writeln!(
                sum,
                "value += {weight}u * u32(round(textureSampleLevel(t{i}, s{}, vec2(0.5), 0.).x * 255.));",
                i % 16
            )
            .unwrap();
        } else {
            writeln!(
                sum,
                "value += {weight}u * u32(round(textureLoad(t{i}, vec2(0), 0).x * 255.));"
            )
            .unwrap();
        }
    }

    let outputs: Vec<_> = (0..8)
        .map(|i| {
            let output = image(
                &gpu,
                1,
                1,
                Format::R32Uint,
                1,
                1,
                Usage::STORAGE | Usage::COPY_SRC,
            );
            slots.storage.set(
                slots.storage.len(),
                StorageBinding::Image(output.view().clone()),
            );
            writeln!(
                source,
                "@group(3) @binding({i}) var u{i}: texture_storage_2d<r32uint, write>;"
            )
            .unwrap();
            writeln!(sum, "textureStore(u{i}, vec2(0), vec4(value + {i}u));").unwrap();
            output
        })
        .collect();
    writeln!(source, "@compute @workgroup_size(1) fn main() {{ {sum} }}").unwrap();

    let dispatch = gpu
        .prepare_dispatch(shader(&source, Stage::Compute))
        .unwrap();
    gpu.dispatch(&dispatch, &slots, DispatchArguments::Direct([1; 3]))
        .unwrap();
    for (i, output) in outputs.iter().enumerate() {
        assert_eq!(
            read_image(&mut gpu, output, 0, 4),
            (718_674 + i as u32).to_le_bytes()
        );
    }
}

// spec: GFX-CAP-4
#[test]
fn vertex_streams_buffer_srvs_and_constant_buffers_keep_their_offsets_and_lengths() {
    use std::fmt::Write;

    let Some(mut gpu) = executor() else { return };
    let stride = u64::from(gpu.device().caps().limits.storage_offset_alignment).max(256);
    const CONSTANTS: u64 = 14;
    const STREAMS: u64 = 32;
    const BUFFER_SRVS: u64 = 128;
    let ranges = CONSTANTS + STREAMS + BUFFER_SRVS;
    let data = buffer(
        &gpu,
        stride * ranges,
        BufferUsage::STORAGE | BufferUsage::UNIFORM | BufferUsage::COPY_SRC,
        Memory::Upload,
    );
    let mut slots = SlotBindings::default();
    let mut source = String::new();
    let mut sum = String::from("var value = 0u;\n");
    for i in 0..ranges {
        let offset = stride * i;
        // SAFETY: the bindings have no GPU users yet.
        unsafe {
            data.mapped().unwrap().as_mut()[offset as usize..offset as usize + 4]
                .copy_from_slice(&(i as u32 + 1).to_ne_bytes());
        }
        if i < CONSTANTS {
            slots.uniforms.set(
                slots.uniforms.len(),
                BufferRange {
                    buffer: data.clone(),
                    range: offset..offset + 16,
                }
                .into(),
            );
            writeln!(
                source,
                "@group(0) @binding({i}) var<uniform> c{i}: vec4<u32>;"
            )
            .unwrap();
            writeln!(sum, "value += c{i}.x;").unwrap();
        } else {
            let register = i - CONSTANTS;
            slots.storage.set(
                slots.storage.len(),
                StorageBinding::Buffer(BufferRange {
                    buffer: data.clone(),
                    range: offset..offset + (register % STREAMS + 1) * 4,
                }),
            );
            writeln!(
                source,
                "@group(3) @binding({register}) var<storage, read> data{register}: array<u32>;"
            )
            .unwrap();
            writeln!(
                sum,
                "value += data{register}[0] + arrayLength(&data{register});"
            )
            .unwrap();
        }
    }
    data.flush(std::iter::once(0..stride * ranges));
    writeln!(
        source,
        "
struct Output {{
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) value: u32,
}}
@vertex fn main(@builtin(vertex_index) id: u32) -> Output {{
    {sum}
    let p = array(vec2(-1., -1.), vec2(3., -1.), vec2(-1., 3.));
    return Output(vec4(p[id], 0., 1.), value);
}}"
    )
    .unwrap();

    let output = target(&gpu, 1, 1, Format::R32Uint);
    let fragment = shader(
        "@fragment fn main(@location(0) @interpolate(flat) value: u32) -> @location(0) u32 { return value; }",
        Stage::Fragment,
    );
    gpu.draw_modern(
        draw(
            1,
            1,
            shader(&source, Stage::Vertex),
            fragment,
            [color(&output, Load::DontCare)],
        ),
        &slots,
        &SlotBindings::default(),
    )
    .unwrap();
    let expected = (1..=ranges).sum::<u64>() + 5 * (1..=STREAMS).sum::<u64>();
    assert_eq!(
        read_image(&mut gpu, &output, 0, 4),
        (expected as u32).to_le_bytes()
    );
}
