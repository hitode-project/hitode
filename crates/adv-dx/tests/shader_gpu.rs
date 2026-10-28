#![cfg(feature = "gpu")]
//! Authored token fixtures, differential against the scalar Rust oracle.
use adv_device::gpu::{ConstantBlock, DrawState, Gpu, GpuBackend, Program, ProgrammableDraw};
use adv_dx::shader::{
    Opcode, Shader, Stage,
    interpreter::{self, Constants, Inputs, NoTextures},
};
use std::sync::Arc;
use std::time::{Duration, Instant};
include!("fixtures/metal_compare.rs");

fn reg(kind: u32, index: u32) -> u32 {
    0x80000000 | ((kind & 7) << 28) | ((kind & 24) << 8) | index
}
fn dst(kind: u32, index: u32) -> u32 {
    reg(kind, index) | 0xf0000
}
fn src(kind: u32, index: u32) -> u32 {
    reg(kind, index) | 0xe40000
}
fn ins(op: Opcode, args: &[u32]) -> Vec<u32> {
    let mut words = vec![op as u32 | ((args.len() as u32) << 24)];
    words.extend(args);
    words
}
fn shader(major: u32, minor: u32, body: Vec<u32>) -> Shader {
    let mut words = vec![0xffff0000 | (major << 8) | minor];
    words.extend(body);
    words.push(0xffff);
    Shader::parse(&words).unwrap()
}
fn full_source(shader: &Shader, input: &Inputs) -> String {
    let mut source = String::from(
        "diagnostic(off, derivative_uniformity);\nstruct ShaderInputs {v:array<vec4<f32>,16>,t:array<vec4<f32>,8>,position:vec4<f32>,front_face:bool};\nstruct ShaderOutputs {position:vec4<f32>,colors:array<vec4<f32>,2>,texcoords:array<vec4<f32>,8>,depth:f32,point_size:f32,fog:f32,generic:array<vec4<f32>,12>};\nstruct ShaderConstants {f:array<vec4<f32>,256>,i:array<vec4<i32>,16>,b:array<vec4<i32>,16>,bump_matrix:array<vec4<f32>,8>,bump_luminance:array<vec4<f32>,8>};\n@group(0) @binding(0) var<uniform> vs_constants:ShaderConstants; @group(0) @binding(1) var<uniform> ps_constants:ShaderConstants;\n",
    );
    source.push_str(&shader.translate().unwrap().source);
    source.push_str("@vertex fn vertex_main(@builtin(vertex_index) index:u32)->@builtin(position) vec4<f32> { let xy=array<vec2<f32>,3>(vec2<f32>(-1.0,-1.0),vec2<f32>(3.0,-1.0),vec2<f32>(-1.0,3.0));return vec4<f32>(xy[index],0.5,1.0);}\n@fragment fn fragment_main()->@location(0) vec4<f32> {var input:ShaderInputs;input.front_face=true;\n");
    for (register, v) in input.v.iter().enumerate() {
        source.push_str(&format!(
            "input.v[{register}]=vec4<f32>({:?},{:?},{:?},{:?});\n",
            v[0], v[1], v[2], v[3]
        ));
    }
    for (register, v) in input.t.iter().enumerate() {
        source.push_str(&format!(
            "input.t[{register}]=vec4<f32>({:?},{:?},{:?},{:?});\n",
            v[0], v[1], v[2], v[3]
        ));
    }
    source.push_str("return ps_run(input).colors[0];}\n");
    Shader::validate_wgsl(&source).unwrap();
    source
}
fn compare_gpu(gpu: &mut Gpu, name: &str, shader: Shader, input: &Inputs, constants: &Constants) {
    let expected = interpreter::run(&shader, input, constants, &mut NoTextures).unwrap();
    let source = full_source(&shader, input);
    let target = gpu.target(16, 12, false).unwrap();
    let draw = ProgrammableDraw {
        instance_count: 1,
        program: Program::new(Arc::from(source), Vec::new()),
        streams: Vec::new(),
        indices: None,
        vertex_count: 3,
        vertex_constants: ConstantBlock::default().into(),
        pixel_constants: ConstantBlock {
            floats: constants.f,
            integers: constants.i,
            booleans: constants.b,
            bump_matrix: constants.bump_matrix,
            bump_luminance: constants.bump_luminance,
        }
        .into(),
        textures: Vec::new(),
        parameters: [[0; 4]; 40],
    };
    gpu.draw_programmable(&target, &draw, &DrawState::new(16, 12))
        .unwrap();
    let mut readback = gpu.readback(&target.image, None).unwrap();
    if let Some(job) = readback.take_completion_job() {
        job();
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let pixels = loop {
        gpu.poll().unwrap();
        if let Some(pixels) = readback.poll() {
            break pixels.unwrap();
        }
        assert!(Instant::now() < deadline, "readback timed out: {name}");
        std::thread::sleep(Duration::from_millis(1));
    };
    #[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
    emit_metal_pixels(&pixels);
    let expected = expected.colors[0].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as i32);
    for (index, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
        for channel in 0..4 {
            let actual = i32::from(pixel[[2, 1, 0, 3][channel]]);
            assert!(
                (actual - expected[channel]).abs() <= 2,
                "{name}: pixel {index} channel {channel}: GPU={actual}, CPU={} ",
                expected[channel]
            );
        }
    }
}

#[test]
// spec: GFX-NUM-1
fn gpu_matches_reference_masks_modifiers_flow_and_specials() {
    let backend = std::env::var("ADV_TEST_GPU_BACKEND").unwrap_or_default();
    let (fallback, backend) = match backend.as_str() {
        "vulkan" => (false, GpuBackend::Vulkan),
        "gles" => (false, GpuBackend::Gl),
        "dx12" => (false, GpuBackend::Direct3D12),
        "metal" => (false, GpuBackend::Metal),
        _ => (
            !cfg!(any(target_os = "macos", target_os = "android")),
            GpuBackend::Automatic,
        ),
    };
    let mut gpu = pollster::block_on(Gpu::request_backend(fallback, backend))
        .expect("differential GPU adapter");
    let mut constants = Constants::default();
    constants.f[0] = [0.2, 0.35, 0.65, 0.8];
    constants.f[1] = [0.6, 0.45, 0.3, 0.1];
    constants.f[2] = [0.25; 4];
    constants.f[3] = [2.0; 4];
    constants.f[4] = [0.125; 4];
    constants.f[5] = [0.0; 4];
    constants.f[6] = [-0.25, -0.5, 0.25, 0.5];
    constants.f[7] = [f32::INFINITY; 4];
    constants.i[0] = [3, 1, 1, 0];
    let mut input = Inputs::default();
    input.v[0] = [0.5, 0.25, 0.75, 1.0];
    for op in [
        Opcode::Add,
        Opcode::Sub,
        Opcode::Mul,
        Opcode::Min,
        Opcode::Max,
        Opcode::Dp3,
        Opcode::Dp4,
        Opcode::Pow,
        Opcode::Slt,
        Opcode::Sge,
    ] {
        let body = ins(op, &[dst(8, 0), src(2, 0), src(2, 1)]);
        compare_gpu(
            &mut gpu,
            &format!("{op:?}"),
            shader(3, 0, body),
            &input,
            &constants,
        );
    }
    let mut body = ins(Opcode::Mov, &[dst(0, 0), src(2, 0)]);
    body.extend(ins(
        Opcode::Mad,
        &[
            (dst(0, 0) & !0xf0000) | 0x50000,
            (src(2, 1) & !0xff0000) | 0x1b0000,
            src(2, 2),
            src(2, 4),
        ],
    ));
    body.extend(ins(Opcode::Mov, &[dst(8, 0), src(0, 0)]));
    compare_gpu(
        &mut gpu,
        "write masks and reverse swizzle",
        shader(2, 0, body),
        &input,
        &constants,
    );
    for modifier in [0, 1, 2, 3, 4, 5, 6, 7, 8, 11, 12] {
        let mut body = ins(Opcode::Mov, &[dst(0, 0), src(2, 0) | (modifier << 24)]);
        body.extend(ins(
            Opcode::Mad,
            &[dst(8, 0), src(0, 0), src(2, 2), src(2, 2)],
        ));
        compare_gpu(
            &mut gpu,
            &format!("source modifier {modifier}"),
            shader(3, 0, body),
            &input,
            &constants,
        );
    }
    let mut body = ins(Opcode::Mov, &[dst(0, 0), src(2, 5)]);
    body.extend(ins(Opcode::Loop, &[src(15, 0), src(7, 0)]));
    body.extend(ins(
        Opcode::Add,
        &[dst(0, 0), src(0, 0), src(2, 3) | 0x2000, reg(15, 0)],
    ));
    body.extend(ins(Opcode::EndLoop, &[]));
    body.extend(ins(
        Opcode::Mad,
        &[dst(8, 0), src(0, 0), src(2, 4), src(2, 2)],
    ));
    compare_gpu(
        &mut gpu,
        "loop aL relative constants",
        shader(3, 0, body),
        &input,
        &constants,
    );
    let mut body = ins(Opcode::Mov, &[dst(0, 0), src(2, 0)]);
    let mut setp = ins(Opcode::Setp, &[dst(19, 0), src(2, 1), src(2, 0)]);
    setp[0] |= 1 << 16;
    body.extend(setp);
    let mut pred = ins(Opcode::Add, &[dst(0, 0), src(19, 0), src(0, 0), src(2, 2)]);
    pred[0] |= 0x10000000;
    body.extend(pred);
    body.extend(ins(Opcode::If, &[src(14, 0)]));
    body.extend(ins(Opcode::Mov, &[dst(0, 0), src(2, 2)]));
    body.extend(ins(Opcode::Else, &[]));
    body.extend(ins(Opcode::Add, &[dst(0, 0), src(0, 0), src(2, 4)]));
    body.extend(ins(Opcode::EndIf, &[]));
    body.extend(ins(Opcode::Mov, &[dst(8, 0), src(0, 0)]));
    compare_gpu(
        &mut gpu,
        "predicates and branch false",
        shader(3, 0, body.clone()),
        &input,
        &constants,
    );
    constants.b[0] = [1; 4];
    compare_gpu(
        &mut gpu,
        "predicates and branch true",
        shader(3, 0, body),
        &input,
        &constants,
    );
    let mut body = ins(Opcode::Rcp, &[dst(0, 0), src(2, 5)]);
    body.extend(ins(
        Opcode::Mov,
        &[(dst(8, 0) & !0xf0000) | 0x10000 | 0x100000, src(0, 0)],
    ));
    body.extend(ins(Opcode::Log, &[dst(0, 0), src(2, 5)]));
    body.extend(ins(
        Opcode::Mov,
        &[(dst(8, 0) & !0xf0000) | 0x20000 | 0x100000, src(0, 0)],
    ));
    body.extend(ins(Opcode::Rsq, &[dst(0, 0), src(2, 6)]));
    body.extend(ins(
        Opcode::Mul,
        &[(dst(8, 0) & !0xf0000) | 0xc0000, src(0, 0), src(2, 2)],
    ));
    compare_gpu(
        &mut gpu,
        "rcp zero log zero rsq negative",
        shader(3, 0, body),
        &input,
        &constants,
    );
    let mut body = ins(Opcode::Rcp, &[dst(0, 0), src(2, 5)]);
    body.extend(ins(Opcode::Mul, &[dst(0, 1), src(0, 0), src(2, 5)]));
    let mut equality = ins(Opcode::Setp, &[dst(19, 0), src(0, 1), src(0, 1)]);
    equality[0] |= 2 << 16;
    body.extend(equality);
    body.extend(ins(Opcode::Mov, &[dst(0, 2), src(2, 1)]));
    let mut pred = ins(Opcode::Mov, &[dst(0, 2), src(19, 0), src(2, 0)]);
    pred[0] |= 0x10000000;
    body.extend(pred);
    body.extend(ins(Opcode::Mov, &[dst(8, 0), src(0, 2)]));
    compare_gpu(
        &mut gpu,
        "zero times infinity yields NaN and self comparison false",
        shader(3, 0, body),
        &input,
        &constants,
    );
    for minor in 1..=4 {
        let mut words = vec![
            Opcode::Mul as u32,
            dst(0, 0),
            src(1, 0),
            src(2, 0),
            Opcode::Add as u32,
            dst(0, 0),
            src(0, 0),
            src(2, 2),
        ];
        if minor < 4 {
            words[4] |= 0x40000000;
            words[5] = (dst(0, 0) & !0xf0000) | 0x80000;
        }
        compare_gpu(
            &mut gpu,
            &format!("ps1.{minor} arithmetic coissue"),
            shader(1, minor, words),
            &input,
            &constants,
        );
    }
    assert_eq!(Stage::Pixel, shader(3, 0, Vec::new()).stage);
}

// spec: GFX-METAL-1
#[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
#[test]
fn shader_fixtures_match_native_metal_and_wgpu_exactly() {
    compare_metal_fixture("gpu_matches_reference_masks_modifiers_flow_and_specials");
}
