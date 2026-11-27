use super::sm::*;
use super::*;
fn reg(kind: u32, index: u32) -> u32 {
    0x80000000 | ((kind & 7) << 28) | ((kind & 24) << 8) | index
}
fn dst(kind: u32, index: u32) -> u32 {
    reg(kind, index) | 0xf0000
}
fn src(kind: u32, index: u32) -> u32 {
    reg(kind, index) | 0xe40000
}
fn instruction(op: Opcode, args: &[u32]) -> Vec<u32> {
    let mut words = vec![op as u32 | ((args.len() as u32) << 24)];
    words.extend(args);
    words
}
fn shader(stage: Stage, major: u8, minor: u8, body: &[u32]) -> Shader {
    let mut words = vec![
        if stage == Stage::Vertex {
            0xfffe0000
        } else {
            0xffff0000
        } | (u32::from(major) << 8)
            | u32::from(minor),
    ];
    words.extend(body);
    words.push(0xffff);
    Shader::parse(&words).unwrap()
}
fn validation_wrapper(shader: &Shader, translation: &Translation) -> String {
    let mut text = String::from(
        "diagnostic(off, derivative_uniformity);\nstruct ShaderInputs {v:array<vec4<f32>,16>,t:array<vec4<f32>,8>,position:vec4<f32>,front_face:bool};\nstruct ShaderOutputs {position:vec4<f32>,colors:array<vec4<f32>,2>,texcoords:array<vec4<f32>,8>,depth:f32,point_size:f32,fog:f32,generic:array<vec4<f32>,12>};\nstruct ShaderConstants {f:array<vec4<f32>,256>,i:array<vec4<i32>,16>,b:array<vec4<i32>,16>,bump_matrix:array<vec4<f32>,8>,bump_luminance:array<vec4<f32>,8>};\nvar<private> vs_constants:ShaderConstants; var<private> ps_constants:ShaderConstants;\n",
    );
    let p = if shader.stage == Stage::Vertex {
        "vs"
    } else {
        "ps"
    };
    for sampler in &translation.samplers {
        text.push_str(&format!("fn {p}_sample_{}(coord:vec4<f32>,mode:u32)->vec4<f32> {{return coord;}}\nfn {p}_sample_grad_{}(coord:vec4<f32>,dx:vec4<f32>,dy:vec4<f32>)->vec4<f32> {{return coord;}}\n",sampler.index,sampler.index));
    }
    text.push_str(&translation.source);
    if shader.stage == Stage::Vertex {
        text.push_str("@vertex fn main()->@builtin(position) vec4<f32> { var input:ShaderInputs; return vs_run(input).position; }");
    } else {
        text.push_str("@fragment fn main()->@location(0) vec4<f32> { var input:ShaderInputs; return ps_run(input).colors[0]; }");
    }
    text
}
fn validate(s: &Shader) {
    let tr = s.translate(false).unwrap();
    let text = validation_wrapper(s, &tr);
    if let Err(e) = Shader::validate_wgsl(&text) {
        panic!("{e}\n{text}");
    }
}

#[test]
// spec: GFX-REC-7
fn usage_includes_matrix_rows_int_bool_and_excludes_all_local_definitions() {
    let mut body = instruction(Opcode::Def, &[dst(2, 4), 0, 0, 0, 0]);
    body.extend(instruction(Opcode::Defi, &[dst(7, 2), 1, 0, 1, 0]));
    body.extend(instruction(Opcode::Defb, &[dst(14, 2), 1]));
    body.extend(instruction(
        Opcode::M4x4,
        &[dst(4, 0), src(2, 0), src(2, 3)],
    ));
    for n in [1, 2] {
        body.extend(instruction(Opcode::Rep, &[src(7, n)]));
        body.extend(instruction(Opcode::If, &[src(14, n)]));
        body.extend(instruction(Opcode::EndIf, &[]));
        body.extend(instruction(Opcode::EndRep, &[]));
    }
    let usage = shader(Stage::Vertex, 2, 0, &body)
        .translate(false)
        .unwrap()
        .constants;
    assert!(!usage.relative);
    assert_eq!(
        usage
            .ranges()
            .flat_map(|r| r.start / 16..r.end / 16)
            .collect::<Vec<_>>(),
        [0, 3, 5, 6, 257, 273]
    );
    for (op, rows) in [
        (Opcode::M4x3, 3),
        (Opcode::M3x4, 4),
        (Opcode::M3x3, 3),
        (Opcode::M3x2, 2),
    ] {
        let s = shader(
            Stage::Vertex,
            2,
            0,
            &instruction(op, &[dst(4, 0), src(2, 63), src(2, 127)]),
        );
        let used = s.translate(false).unwrap().constants;
        assert_eq!(
            used.ranges()
                .flat_map(|r| r.start / 16..r.end / 16)
                .collect::<Vec<_>>(),
            std::iter::once(63)
                .chain(127..127 + rows)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
// spec: GFX-REC-7
fn relative_usage_covers_all_floats_but_keeps_local_definitions_private() {
    let body = [
        Opcode::Def as u32,
        dst(2, 5),
        0,
        0,
        0,
        0,
        Opcode::Mov as u32,
        dst(4, 0),
        src(2, 3) | 0x2000,
    ];
    let s = shader(Stage::Vertex, 1, 1, &body);
    let usage = s.translate(false).unwrap().constants;
    assert!(usage.relative);
    let registers = usage
        .ranges()
        .flat_map(|r| r.start / 16..r.end / 16)
        .collect::<Vec<_>>();
    assert_eq!(registers, (0..256).filter(|n| *n != 5).collect::<Vec<_>>());
}

#[test]
// spec: GFX-REC-7
#[allow(
    clippy::single_range_in_vec_init,
    reason = "the expected result contains one constant range"
)]
fn legacy_bump_usage_covers_only_the_implicit_stage_parameters() {
    for op in [Opcode::Texbem, Opcode::Texbeml] {
        let s = shader(Stage::Pixel, 1, 1, &[op as u32, dst(3, 1), src(3, 0)]);
        let usage = s.translate(false).unwrap().constants;
        let expected = if op == Opcode::Texbem {
            vec![289]
        } else {
            vec![289, 297]
        };
        assert_eq!(
            usage
                .ranges()
                .flat_map(|r| r.start / 16..r.end / 16)
                .collect::<Vec<_>>(),
            expected
        );
    }
    let s = shader(
        Stage::Pixel,
        1,
        4,
        &[Opcode::Bem as u32, dst(0, 1), src(0, 0), src(0, 2)],
    );
    assert_eq!(
        s.translate(false)
            .unwrap()
            .constants
            .ranges()
            .collect::<Vec<_>>(),
        [289 * 16..290 * 16]
    );
}
#[test]
// spec: GFX-SHADER-1, GFX-BUFFER-3
fn versions_comments_end_and_immediates() {
    for (stage, major, minor) in [
        (Stage::Vertex, 1, 1),
        (Stage::Pixel, 1, 1),
        (Stage::Pixel, 1, 2),
        (Stage::Pixel, 1, 3),
        (Stage::Pixel, 1, 4),
        (Stage::Vertex, 2, 0),
        (Stage::Pixel, 2, 0),
        (Stage::Vertex, 3, 0),
        (Stage::Pixel, 3, 0),
    ] {
        let s = shader(stage, major, minor, &[0x0001fffe, 0x0000ffff]);
        assert_eq!(s.bytecode().len(), 4);
        validate(&s);
    }
    let s = shader(
        Stage::Vertex,
        2,
        0,
        &instruction(Opcode::Def, &[dst(2, 0), 0xffff, 0, 0, 0]),
    );
    assert_eq!(s.instructions[0].immediate[0], 0xffff);
    validate(&s);
}
#[test]
// spec: GFX-SHADER-1, GFX-BUFFER-3
fn malformed_is_bounded() {
    for words in [
        &[][..],
        &[0][..],
        &[0xfffe0400, 0xffff][..],
        &[0xfffe0200][..],
        &[0xfffe0200, 0x000ffffe][..],
        &[0xfffe0200, 0x02000001, dst(0, 0)][..],
        &[0xfffe0200, 0x01000043, 0xffff][..],
        &[0xfffe0200, 0x02000001, dst(0, 32), src(2, 0), 0xffff][..],
    ] {
        assert!(Shader::parse(words).is_err(), "{words:?}");
    }
}
#[test]
fn arithmetic_all_validates() {
    use Opcode::*;
    for op in [
        Mov, Add, Sub, Mad, Mul, Rcp, Rsq, Dp3, Dp4, Min, Max, Slt, Sge, Exp, Log, Lit, Dst, Lrp,
        Frc, M4x4, M4x3, M3x4, M3x3, M3x2, Pow, Crs, Sgn, Abs, Nrm, SinCos, Expp, Logp,
    ] {
        let (_, ns, _) = op.arity(Version { major: 3, minor: 0 });
        let mut args = vec![dst(0, 0)];
        for n in 0..ns {
            args.push(src(2, n as u32));
        }
        validate(&shader(Stage::Vertex, 3, 0, &instruction(op, &args)));
    }
    for op in [Cmp, Dp2add, Dsx, Dsy] {
        let (_, ns, _) = op.arity(Version { major: 3, minor: 0 });
        let mut args = vec![dst(0, 0)];
        for n in 0..ns {
            args.push(src(2, n as u32));
        }
        validate(&shader(Stage::Pixel, 3, 0, &instruction(op, &args)));
    }
}
#[test]
// spec: GFX-SHADER-1
fn modifiers_masks_and_relative() {
    let mut body = instruction(Opcode::Dcl, &[0x80000000, dst(6, 0)]);
    body.extend(instruction(Opcode::Mova, &[dst(3, 0), src(2, 0)]));
    for modifier in 0..=13 {
        body.extend(instruction(
            Opcode::Mov,
            &[
                (dst(0, 0) & !0xf0000) | 0x50000,
                src(2, 1) | (modifier << 24),
            ],
        ));
    }
    body.extend(instruction(
        Opcode::Mov,
        &[dst(6, 0), src(2, 0) | 0x2000, reg(3, 0)],
    ));
    let s = shader(Stage::Vertex, 3, 0, &body);
    validate(&s);
    assert!(s.disassemble().contains("AbsNegate"));
    let mut constants = Constants::default();
    constants.f[0] = [1.0; 4];
    constants.f[1] = [0.1, 0.2, 0.3, 0.4];
    let out = run(&s, &Inputs::default(), &constants, &mut NoTextures).unwrap();
    assert_eq!(out.position, constants.f[1]);
}
#[test]
// spec: GFX-SHADER-1
fn control_flow_predicates_calls() {
    let mut body = instruction(Opcode::Dcl, &[0x80000000, dst(6, 0)]);
    body.extend(instruction(Opcode::Defi, &[dst(7, 0), 3, 1, 2, 0]));
    body.extend(instruction(Opcode::Mov, &[dst(0, 0), src(2, 0)]));
    body.extend(instruction(Opcode::Loop, &[src(15, 0), src(7, 0)]));
    body.extend(instruction(Opcode::If, &[src(14, 0)]));
    body.extend(instruction(Opcode::Break, &[]));
    body.extend(instruction(Opcode::EndIf, &[]));
    body.extend(instruction(Opcode::Add, &[dst(0, 0), src(0, 0), src(2, 1)]));
    body.extend(instruction(Opcode::EndLoop, &[]));
    body.extend(instruction(Opcode::Call, &[src(18, 0)]));
    body.extend(instruction(Opcode::Mov, &[dst(6, 0), src(0, 0)]));
    body.extend(instruction(Opcode::Ret, &[]));
    body.extend(instruction(Opcode::Label, &[src(18, 0)]));
    body.extend(instruction(
        Opcode::Setp,
        &[dst(19, 0), src(2, 1), src(2, 0)],
    ));
    let comparison_index = body.len() - 4;
    body[comparison_index] |= 1 << 16;
    let mut predicated = instruction(Opcode::Add, &[dst(0, 0), src(19, 0), src(0, 0), src(2, 1)]);
    predicated[0] |= 0x10000000;
    body.extend(predicated);
    body.extend(instruction(Opcode::Ret, &[]));
    let s = shader(Stage::Vertex, 3, 0, &body);
    validate(&s);
    let mut constants = Constants::default();
    constants.f[1] = [1.0; 4];
    let out = run(&s, &Inputs::default(), &constants, &mut NoTextures).unwrap();
    assert_eq!(out.position, [4.0; 4]);
    constants.b[0] = [1; 4];
    let out = run(&s, &Inputs::default(), &constants, &mut NoTextures).unwrap();
    assert_eq!(out.position, [1.0; 4]);
}
#[test]
fn texture_metadata_and_sampling() {
    let mut body = instruction(Opcode::Dcl, &[0x90000000, dst(10, 3)]);
    body.extend(instruction(
        Opcode::Tex,
        &[dst(0, 0), src(3, 0), src(10, 3)],
    ));
    body.extend(instruction(
        Opcode::Texldd,
        &[dst(0, 0), src(3, 0), src(10, 3), src(2, 0), src(2, 1)],
    ));
    body.extend(instruction(
        Opcode::Texldl,
        &[dst(8, 0), src(0, 0), src(10, 3)],
    ));
    let s = shader(Stage::Pixel, 3, 0, &body);
    validate(&s);
    let tr = s.translate(false).unwrap();
    assert_eq!(
        tr.samplers,
        vec![SamplerBinding {
            index: 3,
            dimension: SamplerDimension::D2
        }]
    );
}
#[test]
fn sm3_outputs_normalized() {
    let mut body = instruction(Opcode::Dcl, &[0x80000000, dst(6, 2)]);
    body.extend(instruction(Opcode::Dcl, &[0x80010005, dst(6, 5)]));
    body.extend(instruction(Opcode::Dcl, &[0x8000000a, dst(6, 1)]));
    body.extend(instruction(Opcode::Mov, &[dst(6, 2), src(2, 0)]));
    body.extend(instruction(Opcode::Mov, &[dst(6, 5), src(2, 1)]));
    body.extend(instruction(Opcode::Mov, &[dst(6, 1), src(2, 2)]));
    let s = shader(Stage::Vertex, 3, 0, &body);
    validate(&s);
    let mut constants = Constants::default();
    constants.f[0] = [1.0; 4];
    constants.f[1] = [2.0; 4];
    constants.f[2] = [3.0; 4];
    let out = run(&s, &Inputs::default(), &constants, &mut NoTextures).unwrap();
    assert_eq!(out.position, [1.0; 4]);
    assert_eq!(out.texcoords[1], [2.0; 4]);
    assert_eq!(out.colors[0], [3.0; 4]);
}
#[test]
// spec: GFX-SHADER-1
fn illegal_recursive_and_broken_flow() {
    for body in [
        instruction(Opcode::Break, &[]),
        instruction(Opcode::If, &[src(14, 0)]),
        [
            instruction(Opcode::Call, &[src(18, 0)]),
            instruction(Opcode::Ret, &[]),
            instruction(Opcode::Label, &[src(18, 0)]),
            instruction(Opcode::Call, &[src(18, 0)]),
            instruction(Opcode::Ret, &[]),
        ]
        .concat(),
    ] {
        let mut words = vec![0xfffe0300];
        words.extend(body);
        words.push(0xffff);
        assert!(Shader::parse(&words).is_err());
    }
}
#[test]
// spec: GFX-SHADER-1
fn arbitrary_words_never_panic() {
    let mut seed = 0xabcdef01u32;
    for len in 0..256 {
        let mut tokens = vec![0xfffe0300];
        for _ in 0..len {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            tokens.push(seed);
        }
        tokens.push(0xffff);
        let result = std::panic::catch_unwind(|| Shader::parse(&tokens));
        assert!(result.is_ok());
    }
}
#[test]
fn guest_reader_frames_comment_and_immediate_end_words() {
    let words = [
        0xffff0200, 0x0002fffe, 0xffff, 0xffff, 0x05000051, 0xa00f0000, 0xffff, 0xffff, 0xffff,
        0xffff, 0xffff, 0x12345678,
    ];
    let read = read_bytecode(|n| words.get(n).copied().ok_or("missing memory")).unwrap();
    assert_eq!(read, &words[..11]);
    assert!(read_bytecode(|_| Ok::<_, String>(0xfffe0200)).is_err());
    let s = Shader::parse(&[
        0xfffe0101, 0x1f, 0x80000000, 0x900f0000, 1, 0xc00f0000, 0x90e40000, 0xffff,
    ])
    .unwrap();
    assert_eq!(s.inputs.len(), 1);
    validate(&s);
}
#[test]
fn vs1_implicit_relative_address_rounds_before_read() {
    let s = shader(
        Stage::Vertex,
        1,
        1,
        &[
            Opcode::Mov as u32,
            (dst(3, 0) & !0xf0000) | 0x10000,
            reg(2, 0),
            Opcode::Mov as u32,
            dst(4, 0),
            src(2, 3) | 0x2000,
        ],
    );
    validate(&s);
    let mut constants = Constants::default();
    constants.f[0] = [1.6; 4];
    constants.f[5] = [0.1, 0.3, 0.5, 1.0];
    assert_eq!(
        run(&s, &Inputs::default(), &constants, &mut NoTextures)
            .unwrap()
            .position,
        constants.f[5]
    );
}
#[test]
// spec: GFX-NUM-1
fn predication_lit_pow_and_saturate_follow_reference() {
    let mut body = Vec::new();
    for (register, usage) in [(0, 0), (1, 5), (2, 10)] {
        body.extend(instruction(
            Opcode::Dcl,
            &[0x80000000 | usage, dst(6, register)],
        ));
    }
    let mut setp = instruction(Opcode::Setp, &[dst(19, 0), src(2, 1), src(2, 0)]);
    setp[0] |= 1 << 16;
    body.extend(setp);
    body.extend(instruction(Opcode::Mov, &[dst(0, 0), src(2, 2)]));
    let mut predicated = instruction(Opcode::Mov, &[dst(0, 0), src(19, 0), src(2, 3)]);
    predicated[0] |= 0x10000000;
    body.extend(predicated);
    body.extend(instruction(Opcode::Mov, &[dst(6, 0), src(0, 0)]));
    body.extend(instruction(Opcode::Lit, &[dst(6, 1), src(2, 4)]));
    body.extend(instruction(
        Opcode::Pow,
        &[dst(0, 1), reg(2, 5), reg(2, 5) | 0x550000],
    ));
    body.extend(instruction(
        Opcode::Mov,
        &[(dst(0, 1) & !0xf0000) | 0x120000, reg(2, 6)],
    ));
    body.extend(instruction(
        Opcode::Mov,
        &[(dst(6, 2) & !0xf0000) | 0x30000, src(0, 1)],
    ));
    let s = shader(Stage::Vertex, 3, 0, &body);
    validate(&s);
    let mut constants = Constants::default();
    constants.f[1] = [1.0, 0.0, 1.0, 0.0];
    constants.f[2] = [5.0; 4];
    constants.f[3] = [7.0; 4];
    constants.f[4] = [1.0, -1.0, 0.0, 0.0];
    constants.f[6] = [f32::NAN; 4];
    let out = s
        .program()
        .unwrap()
        .run(&Inputs::default(), &constants, &mut NoTextures)
        .unwrap();
    // Predication selects per component, lit leaves z zero unless y > 0,
    // pow(0, 0) is one and saturate maps NaN to zero.
    assert_eq!(out.position, [7.0, 5.0, 7.0, 5.0]);
    assert_eq!(out.texcoords[0], [1.0, 1.0, 0.0, 1.0]);
    assert_eq!(out.colors[0][..2], [1.0, 0.0]);
}

fn run(
    shader: &Shader,
    input: &Inputs,
    constants: &Constants,
    sampler: &mut dyn Sampler,
) -> Result<Outputs, Error> {
    Program::new(shader)?.run(input, constants, sampler)
}
#[test]
// spec: GFX-NUM-1
fn relative_reads_take_definitions_from_one_table() {
    // aL runs 2, 3, 4: c[aL + 1] reads the defined c3, the bound c4 and the defined c5.
    let mut body = Vec::new();
    body.extend(instruction(
        Opcode::Def,
        &[dst(2, 3), 1f32.to_bits(), 0, 0, 0],
    ));
    body.extend(instruction(
        Opcode::Def,
        &[dst(2, 5), 4f32.to_bits(), 0, 0, 0],
    ));
    body.extend(instruction(Opcode::Defi, &[dst(7, 0), 3, 2, 1, 0]));
    body.extend(instruction(Opcode::Mov, &[dst(0, 0), src(2, 0)]));
    body.extend(instruction(Opcode::Loop, &[src(15, 0), src(7, 0)]));
    body.extend(instruction(
        Opcode::Add,
        &[dst(0, 0), src(0, 0), src(2, 1) | 0x2000, reg(15, 0)],
    ));
    body.extend(instruction(Opcode::EndLoop, &[]));
    body.extend(instruction(Opcode::Mov, &[dst(8, 0), src(0, 0)]));
    let s = shader(Stage::Pixel, 3, 0, &body);
    validate(&s);
    let mut constants = Constants::default();
    constants.f[3] = [100.0; 4];
    constants.f[4] = [2.0; 4];
    let out = run(&s, &Inputs::default(), &constants, &mut NoTextures).unwrap();
    assert_eq!(out.colors[0][0], 7.0);

    // Many definitions and relative reads stay within the translation size limit.
    let mut body = instruction(Opcode::Dcl, &[0x80000000, dst(6, 0)]);
    for n in 0..256 {
        body.extend(instruction(Opcode::Def, &[dst(2, n), n, 0, 0, 0]));
    }
    body.extend(instruction(
        Opcode::Mova,
        &[(dst(3, 0) & !0xf0000) | 0x10000, src(2, 0)],
    ));
    for _ in 0..160 {
        body.extend(instruction(
            Opcode::Add,
            &[dst(0, 0), src(0, 0), src(2, 0) | 0x2000, reg(3, 0)],
        ));
    }
    body.extend(instruction(Opcode::Mov, &[dst(6, 0), src(0, 0)]));
    let s = shader(Stage::Vertex, 3, 0, &body);
    validate(&s);
}

struct Texel(Vec4);

impl Sampler for Texel {
    fn sample(&mut self, _: u8, _: Vec4, _: u32) -> Result<Vec4, String> {
        Ok(self.0)
    }
}

/// An SM1 instruction, whose opcode token carries no length.
fn sm1(op: Opcode, args: &[u32]) -> Vec<u32> {
    let mut words = vec![op as u32];
    words.extend(args);
    words
}

fn predicated(mut words: Vec<u32>) -> Vec<u32> {
    words[0] |= 0x10000000;
    words
}

#[test]
// spec: GFX-SHADER-1
fn nested_subroutines_stay_shared_bodies() {
    // Main calls l1 four times, l1 calls l2 eight times and l2 calls l3 eight
    // times; l3 holds four nrm and one add, so inlining would emit 256 copies.
    let mut body = instruction(Opcode::Dcl, &[0x80000000, dst(6, 0)]);
    body.extend(instruction(Opcode::Mov, &[dst(0, 0), src(2, 0)]));
    for _ in 0..4 {
        body.extend(instruction(Opcode::Call, &[src(18, 1)]));
    }
    body.extend(instruction(Opcode::Mov, &[dst(6, 0), src(0, 0)]));
    body.extend(instruction(Opcode::Ret, &[]));
    for (label, callee) in [(1, 2), (2, 3)] {
        body.extend(instruction(Opcode::Label, &[src(18, label)]));
        for _ in 0..8 {
            body.extend(instruction(Opcode::Call, &[src(18, callee)]));
        }
        body.extend(instruction(Opcode::Ret, &[]));
    }
    body.extend(instruction(Opcode::Label, &[src(18, 3)]));
    for n in 0..4 {
        body.extend(instruction(Opcode::Nrm, &[dst(0, 1 + n), src(2, 2)]));
    }
    body.extend(instruction(Opcode::Add, &[dst(0, 0), src(0, 0), src(2, 1)]));
    body.extend(instruction(Opcode::Ret, &[]));
    let s = shader(Stage::Vertex, 3, 0, &body);
    let translation = s.translate(false).unwrap();
    assert!(translation.source.len() < 64 << 10);
    validate(&s);
    let mut constants = Constants::default();
    constants.f[1] = [1.0; 4];
    constants.f[2] = [1.0, 2.0, 2.0, 3.0];
    let out = run(&s, &Inputs::default(), &constants, &mut NoTextures).unwrap();
    assert_eq!(out.position, [256.0; 4]);
}

#[test]
// spec: GFX-NUM-1
fn texkill_follows_predicate_model_and_subroutines() {
    // (p0.x) texkill r0 with r0 negative discards only under a true predicate.
    let kill = |setp: bool| {
        let mut body = instruction(Opcode::Mov, &[dst(0, 0), src(2, 0)]);
        if setp {
            let mut test = instruction(Opcode::Setp, &[dst(19, 0), src(2, 1), src(2, 2)]);
            test[0] |= 1 << 16;
            body.extend(test);
        }
        body.extend(predicated(instruction(
            Opcode::Texkill,
            &[dst(0, 0), reg(19, 0)],
        )));
        body.extend(instruction(Opcode::Mov, &[dst(8, 0), src(2, 1)]));
        shader(Stage::Pixel, 3, 0, &body)
    };
    let mut constants = Constants::default();
    constants.f[0] = [-1.0; 4];
    constants.f[1] = [1.0; 4];
    for setp in [false, true] {
        let s = kill(setp);
        validate(&s);
        let out = run(&s, &Inputs::default(), &constants, &mut NoTextures).unwrap();
        assert_eq!(out.discarded, setp);
    }

    // SM2 tests w; PS 1.x does not.
    let mut body = instruction(Opcode::Dcl, &[0x80000000, dst(3, 0)]);
    body.extend(instruction(Opcode::Texkill, &[dst(3, 0)]));
    body.extend(instruction(Opcode::Mov, &[dst(8, 0), src(2, 1)]));
    let s = shader(Stage::Pixel, 2, 0, &body);
    validate(&s);
    let mut input = Inputs::default();
    input.t[0] = [1.0, 1.0, 1.0, -1.0];
    assert!(
        run(&s, &input, &constants, &mut NoTextures)
            .unwrap()
            .discarded
    );
    let mut body = sm1(Opcode::Texkill, &[dst(3, 0)]);
    body.extend(sm1(Opcode::Mov, &[dst(0, 0), src(2, 1)]));
    let s = shader(Stage::Pixel, 1, 1, &body);
    assert!(
        !run(&s, &input, &constants, &mut NoTextures)
            .unwrap()
            .discarded
    );

    // A discard inside a subroutine ends the program.
    let mut body = instruction(Opcode::Mov, &[dst(0, 0), src(2, 0)]);
    body.extend(instruction(Opcode::Call, &[src(18, 0)]));
    body.extend(instruction(Opcode::Mov, &[dst(8, 0), src(2, 1)]));
    body.extend(instruction(Opcode::Ret, &[]));
    body.extend(instruction(Opcode::Label, &[src(18, 0)]));
    body.extend(instruction(Opcode::Texkill, &[dst(0, 0)]));
    body.extend(instruction(Opcode::Ret, &[]));
    let s = shader(Stage::Pixel, 3, 0, &body);
    validate(&s);
    assert!(
        run(&s, &Inputs::default(), &constants, &mut NoTextures)
            .unwrap()
            .discarded
    );
}

#[test]
// spec: GFX-NUM-1
fn vertex_rounding_normalize_fog_and_masked_samples() {
    let mut body = Vec::new();
    for (usage, register, mask) in [
        (0, 0, 0xf0000),
        (5, 1, 0xf0000),
        (5, 2, 0xf0000),
        (11, 3, 0x80000),
    ] {
        body.extend(instruction(
            Opcode::Dcl,
            &[
                0x80000000 | usage | (u32::from(register == 2) << 16),
                (dst(6, register) & !0xf0000) | mask,
            ],
        ));
    }
    body.extend(instruction(
        Opcode::Dcl,
        &[0x80000000 | (2 << 27), dst(10, 2)],
    ));
    // A sample whose lanes are all predicated off needs no texture.
    body.extend(predicated(instruction(
        Opcode::Texldl,
        &[dst(0, 1), reg(19, 0), src(2, 0), src(10, 2)],
    )));
    // mova rounds 0.49999997 to 0, so c[a0.x + 1] reads c1.
    body.extend(instruction(
        Opcode::Mova,
        &[(dst(3, 0) & !0xf0000) | 0x10000, src(2, 0)],
    ));
    body.extend(instruction(
        Opcode::Mov,
        &[dst(6, 0), src(2, 1) | 0x2000, reg(3, 0)],
    ));
    // nrm scales every lane and uses FLT_MAX for zero length.
    body.extend(instruction(Opcode::Nrm, &[dst(6, 1), src(2, 3)]));
    body.extend(instruction(Opcode::Nrm, &[dst(6, 2), src(2, 4)]));
    // Fog declared in o3.w reads w.
    body.extend(instruction(
        Opcode::Mov,
        &[(dst(6, 3) & !0xf0000) | 0x80000, src(2, 5)],
    ));
    let s = shader(Stage::Vertex, 3, 0, &body);
    validate(&s);
    let mut constants = Constants::default();
    constants.f[0] = [f32::from_bits(0x3effffff); 4];
    constants.f[1] = [1.0; 4];
    constants.f[2] = [2.0; 4];
    constants.f[3] = [2.0, 0.0, 0.0, 0.5];
    constants.f[4] = [0.0, 0.0, 0.0, 1.0];
    constants.f[5] = [0.25, 0.0, 0.0, 0.75];
    let out = run(&s, &Inputs::default(), &constants, &mut NoTextures).unwrap();
    assert_eq!(out.position, [1.0; 4]);
    assert_eq!(out.texcoords[0], [1.0, 0.0, 0.0, 0.25]);
    assert_eq!(out.texcoords[1], [0.0, 0.0, 0.0, f32::MAX]);
    assert_eq!(out.fog, 0.75);
}

#[test]
// spec: GFX-NUM-1
fn pixel_swizzles_depth_and_coissued_cnd() {
    // texldl r0, v0, s3.zyxw reorders the texel.
    let mut body = instruction(Opcode::Dcl, &[0x80000005, dst(1, 0)]);
    body.extend(instruction(
        Opcode::Dcl,
        &[0x80000000 | (2 << 27), dst(10, 3)],
    ));
    body.extend(instruction(
        Opcode::Texldl,
        &[dst(0, 0), src(1, 0), reg(10, 3) | 0xc60000],
    ));
    body.extend(instruction(Opcode::Mov, &[dst(8, 0), src(0, 0)]));
    let s = shader(Stage::Pixel, 3, 0, &body);
    validate(&s);
    let out = run(
        &s,
        &Inputs::default(),
        &Constants::default(),
        &mut Texel([0.125, 0.25, 0.5, 1.0]),
    )
    .unwrap();
    assert_eq!(out.colors[0], [0.5, 0.25, 0.125, 1.0]);

    // texdepth writes one for a zero denominator.
    for (value, depth) in [([0.25, 0.5, 0.0, 0.0], 0.5), ([0.5, 0.0, 0.0, 0.0], 1.0)] {
        let mut body = sm1(Opcode::Mov, &[dst(0, 5), src(2, 0)]);
        body.extend(sm1(Opcode::Phase, &[]));
        body.extend(sm1(Opcode::Texdepth, &[dst(0, 5)]));
        body.extend(sm1(Opcode::Mov, &[dst(0, 0), src(2, 0)]));
        let s = shader(Stage::Pixel, 1, 4, &body);
        validate(&s);
        let mut constants = Constants::default();
        constants.f[0] = value;
        let out = run(&s, &Inputs::default(), &constants, &mut NoTextures).unwrap();
        assert_eq!(out.depth, depth);
    }

    // The colour half of a co-issued PS 1.1 cnd selects src1.
    for alpha in [0.25, 0.75] {
        let mut body = sm1(Opcode::Mov, &[(dst(0, 0) & !0xf0000) | 0x80000, src(2, 0)]);
        let mut cnd = sm1(
            Opcode::Cnd,
            &[
                (dst(0, 0) & !0xf0000) | 0x70000,
                reg(0, 0) | 0xff0000,
                src(2, 1),
                src(2, 2),
            ],
        );
        cnd[0] |= 0x40000000;
        body.extend(cnd);
        let s = shader(Stage::Pixel, 1, 1, &body);
        validate(&s);
        let mut constants = Constants::default();
        constants.f[0] = [0.0, 0.0, 0.0, alpha];
        constants.f[1] = [1.0, 0.5, 0.25, 0.0];
        constants.f[2] = [0.0; 4];
        let out = run(&s, &Inputs::default(), &constants, &mut NoTextures).unwrap();
        assert_eq!(out.colors[0], [1.0, 0.5, 0.25, alpha]);
    }

    // ps_2_x calls through a predicate.
    let mut body = instruction(Opcode::Mov, &[dst(0, 0), src(2, 0)]);
    let mut test = instruction(Opcode::Setp, &[dst(19, 0), src(2, 1), src(2, 0)]);
    test[0] |= 1 << 16;
    body.extend(test);
    body.extend(instruction(Opcode::CallNz, &[src(18, 0), reg(19, 0)]));
    body.extend(instruction(Opcode::Mov, &[dst(8, 0), src(0, 0)]));
    body.extend(instruction(Opcode::Ret, &[]));
    body.extend(instruction(Opcode::Label, &[src(18, 0)]));
    body.extend(instruction(Opcode::Mov, &[dst(0, 0), src(2, 1)]));
    body.extend(instruction(Opcode::Ret, &[]));
    let s = shader(Stage::Pixel, 2, 1, &body);
    validate(&s);
    let mut constants = Constants::default();
    constants.f[1] = [0.5; 4];
    let out = run(&s, &Inputs::default(), &constants, &mut NoTextures).unwrap();
    assert_eq!(out.colors[0], [0.5; 4]);
}
