//! Anonymous contracts from the authored x86 D3DX9 math probe.
//! Official declarations/provenance: data/abi/sdk-source.json. Native SSE uses
//! approximate reciprocal/normalization, so numeric comparisons allow 2e-6;
//! pointer returns, layouts, aliases and failed-write preservation are exact.
use super::*;
use adv_abi::{AbiKind, Conv, ImplementationLevel, Outcome, signature};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, Reg, asm::Asm},
};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PARK_ADDR, teb, teb_addr},
};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x600000;
const STACK: u32 = 0x550000;
const READ_ONLY: u32 = 0x610000;
const WRITE_ONLY: u32 = 0x620000;
const UNMAPPED: u32 = 0x630000;
const ERROR: u32 = 0x13579bdf;
const LEFT: u32 = DATA;
const RIGHT: u32 = DATA + 0x100;
const OUTPUT: u32 = DATA + 0x200;
const VIEW: u32 = DATA + 0x300;
const PROJECTION: u32 = DATA + 0x400;
const VECTOR: u32 = DATA + 0x500;
const VIEWPORT: u32 = DATA + 0x600;
const IDENTITY: Matrix = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
];
const A: Matrix = [
    2., 0., 0., 0., 0., 3., 0., 0., 0., 0., 1., 0., 3., -5., 0., 1.,
];
const B: Matrix = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., -2., 4., 0., 1.,
];

fn machine() -> Machine<JitCpu> {
    let cpu = JitCpu::new(PARK_ADDR, false).unwrap();
    let mut registry = Registry::default();
    register(&mut registry);
    Machine::new(cpu, registry, Clock::virtual_time()).unwrap()
}

struct Fixture(Machine<JitCpu>);
impl Fixture {
    fn new() -> Self {
        let mut m = machine();
        for (address, protection) in [
            (DATA, Prot::RW),
            (STACK, Prot::RW),
            (READ_ONLY, Prot::READ),
            (WRITE_ONLY, Prot::WRITE),
            (teb_addr(0), Prot::RW),
        ] {
            m.cpu.map(address, 4096, protection).unwrap();
        }
        m.cpu
            .write_u32(teb_addr(0) + teb::LAST_ERROR, ERROR)
            .unwrap();
        Self(m)
    }
    fn put<T: bytemuck::Pod>(&mut self, address: u32, value: T) {
        self.0
            .cpu
            .write(address, bytemuck::bytes_of(&value))
            .unwrap();
    }
    fn get<const N: usize>(&self, address: u32) -> [f32; N] {
        let mut bytes = vec![0; N * 4];
        self.0.cpu.read(address, &mut bytes).unwrap();
        std::array::from_fn(|i| f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()))
    }
    fn bytes(&self, address: u32, count: usize) -> Vec<u8> {
        let mut value = vec![0; count];
        self.0.cpu.read(address, &mut value).unwrap();
        value
    }
    fn call(&mut self, api: adv_abi::Api, args: &[u32]) -> u32 {
        assert_eq!(
            api.conv,
            Conv::Stdcall {
                argc: args.len() as u8
            }
        );
        self.0.cpu.set_reg(Reg::Esp, STACK);
        self.0.cpu.write_u32(STACK, 0).unwrap();
        for (index, value) in args.iter().enumerate() {
            self.0
                .cpu
                .write_u32(STACK + 4 + index as u32 * 4, *value)
                .unwrap();
        }
        let mut env = Env {
            context: self.0.sys.current,
            cpu: &mut self.0.cpu,
            sys: &mut self.0.sys,
            ext: &mut self.0.ext,
            thunk: 0,
        };
        let Outcome::Return(value) = (api.handler)(&mut env) else {
            panic!("math call must return synchronously")
        };
        assert_eq!(
            self.0.cpu.read_u32(teb_addr(0) + teb::LAST_ERROR).unwrap(),
            ERROR
        );
        value
    }
    fn projection_inputs(&mut self) {
        self.put(LEFT, A);
        self.put(RIGHT, B);
        self.put(VIEW, IDENTITY);
        let mut projection = IDENTITY;
        projection[15] = 2.;
        self.put(PROJECTION, projection);
        self.put(VECTOR, [1f32, 2., 0.25]);
        self.put(
            VIEWPORT,
            D3DVIEWPORT9 {
                X: 13,
                Y: 17,
                Width: 640,
                Height: 480,
                MinZ: 0.2,
                MaxZ: 0.8,
            },
        );
    }
}

fn close<const N: usize>(actual: [f32; N], expected: [f32; N]) {
    for (index, (a, e)) in actual.into_iter().zip(expected).enumerate() {
        assert!(
            (a - e).abs() <= 2e-6 * e.abs().max(1.),
            "component {index}: {a} != {e}"
        );
    }
}
fn native<const N: usize>(words: [u32; N]) -> [f32; N] {
    words.map(f32::from_bits)
}

#[test]
fn typed_legacy_math_exports_and_x86_stack_are_exact_without_gpu() {
    assert_eq!(std::mem::size_of::<Matrix>(), 64);
    assert_eq!(std::mem::size_of::<Vector>(), 12);
    assert_eq!(std::mem::size_of::<D3DVIEWPORT9>(), 24);
    let mut registry = Registry::default();
    register(&mut registry);
    let contracts = [
        (multiply_ABI, vec![OUTPUT, LEFT, RIGHT]),
        (transpose_ABI, vec![OUTPUT, LEFT]),
        (scaling_ABI, vec![OUTPUT, 0x3acccccd, 0x3b360b61, 0]),
        (translation_ABI, vec![OUTPUT, 0xbf800000, 0xbf800000, 0]),
        (
            look_at_ABI,
            vec![OUTPUT, VECTOR, VECTOR + 0x10, VECTOR + 0x20],
        ),
        (
            project_ABI,
            vec![OUTPUT, VECTOR, VIEWPORT, PROJECTION, VIEW, LEFT],
        ),
    ];
    let mut m = machine();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.write(LEFT, bytemuck::bytes_of(&IDENTITY)).unwrap();
    m.cpu.write(RIGHT, bytemuck::bytes_of(&IDENTITY)).unwrap();
    m.cpu.write(VIEW, bytemuck::bytes_of(&IDENTITY)).unwrap();
    m.cpu
        .write(PROJECTION, bytemuck::bytes_of(&IDENTITY))
        .unwrap();
    m.cpu
        .write(VECTOR + 0x10, bytemuck::bytes_of(&[0f32, 0., 1.]))
        .unwrap();
    m.cpu
        .write(VECTOR + 0x20, bytemuck::bytes_of(&[0f32, 1., 0.]))
        .unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
    code.mov_mem_eax(DATA + 0x700);
    for (index, (api, args)) in contracts.iter().enumerate() {
        let declared = signature::lookup(api.module, api.name).unwrap();
        assert_eq!(declared.return_kind, AbiKind::Pointer);
        assert_eq!(declared.stack_words(), args.len() as u16);
        for (index, parameter) in declared.parameters.iter().enumerate() {
            let scalar =
                matches!(api.name, "D3DXMatrixScaling" | "D3DXMatrixTranslation") && index != 0;
            assert_eq!(
                parameter.kind,
                if scalar {
                    AbiKind::F32
                } else {
                    AbiKind::Pointer
                }
            );
        }
        assert_eq!(
            registry.level(api.module, api.name),
            Some(ImplementationLevel::Implement)
        );
        assert!(registry.dynamic_available(api.module, api.name));
        assert_eq!(registry.get(api.module, api.name).unwrap().conv, api.conv);
        let address = m.resolve_import(api.module, api.name).unwrap();
        for arg in args.iter().rev() {
            code.push_imm(*arg);
        }
        code.call(address)
            .mov_mem_eax(DATA + 0x710 + index as u32 * 4);
    }
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x704);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("authored D3DX math", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        m.cpu.read_u32(DATA + 0x700).unwrap(),
        m.cpu.read_u32(DATA + 0x704).unwrap()
    );
    for index in 0..contracts.len() as u32 {
        assert_eq!(m.cpu.read_u32(DATA + 0x710 + index * 4).unwrap(), OUTPUT);
    }
    assert!(registry.signature_conflicts().is_empty());
    assert!(!registry.dynamic_available("d3dx9_42.dll", "D3DXMatrixMultiply"));
    assert!(registry.dynamic_available("d3dx9_43.dll", "D3DXCreateEffect"));
}

#[test]
fn scaling_and_row_vector_translation_assign_native_scalar_bits_without_reading_output() {
    // Four authored native calls (finite/negative/special values) are preserved
    // in target/engine-loop/20261007-resume/d3dx-affine-native/native.jsonl.
    let mut f = Fixture::new();
    for (api, scalars, expected) in [
        (
            scaling_ABI,
            [0x3acccccd, 0x3b360b61, 0],
            [
                0x3acccccd, 0, 0, 0, 0, 0x3b360b61, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x3f800000,
            ],
        ),
        (
            translation_ABI,
            [0xbf800000, 0xbf800000, 0],
            [
                0x3f800000, 0, 0, 0, 0, 0x3f800000, 0, 0, 0, 0, 0x3f800000, 0, 0xbf800000,
                0xbf800000, 0, 0x3f800000,
            ],
        ),
        (
            scaling_ABI,
            [0x80000000, 0x7f800000, 0x7fc12345],
            [
                0x80000000, 0, 0, 0, 0, 0x7f800000, 0, 0, 0, 0, 0x7fc12345, 0, 0, 0, 0, 0x3f800000,
            ],
        ),
        (
            translation_ABI,
            [0x80000000, 0x7f800000, 0x7fc12345],
            [
                0x3f800000, 0, 0, 0, 0, 0x3f800000, 0, 0, 0, 0, 0x3f800000, 0, 0x80000000,
                0x7f800000, 0x7fc12345, 0x3f800000,
            ],
        ),
    ] {
        for output in [OUTPUT, OUTPUT + 4, WRITE_ONLY, STACK + 4] {
            // This may overlap the argument slots themselves: typed Arguments
            // snapshots all three FLOAT values before publishing the matrix.
            f.0.cpu.write(output, &[0xcd; 64]).unwrap();
            assert_eq!(
                f.call(api, &[output, scalars[0], scalars[1], scalars[2]]),
                output
            );
            assert_eq!(f.get::<16>(output).map(f32::to_bits), expected);
        }
    }
    // Confirm the two constructors compose in the existing row-vector product:
    // scaling followed by translation keeps the offset in row 4.
    assert_eq!(
        f.call(
            scaling_ABI,
            &[LEFT, 2f32.to_bits(), 3f32.to_bits(), 4f32.to_bits()]
        ),
        LEFT
    );
    assert_eq!(
        f.call(
            translation_ABI,
            &[RIGHT, (-1f32).to_bits(), 5f32.to_bits(), 0]
        ),
        RIGHT
    );
    assert_eq!(f.call(multiply_ABI, &[OUTPUT, LEFT, RIGHT]), OUTPUT);
    assert_eq!(
        f.get::<16>(OUTPUT),
        [
            2., 0., 0., 0., 0., 3., 0., 0., 0., 0., 4., 0., -1., 5., 0., 1.
        ]
    );
}

#[test]
fn multiply_and_transpose_snapshot_sources_including_partial_aliases() {
    let mut f = Fixture::new();
    let mut expected = A;
    expected[12] = 1.;
    expected[13] = -1.;
    for output in [OUTPUT, LEFT, RIGHT, LEFT + 4] {
        f.put(LEFT, A);
        f.put(RIGHT, B);
        assert_eq!(f.call(multiply_ABI, &[output, LEFT, RIGHT]), output);
        assert_eq!(f.get::<16>(output), expected);
    }
    f.put(LEFT, A);
    f.put(RIGHT, B);
    let mut reverse = A;
    reverse[12] = -1.;
    reverse[13] = 7.;
    assert_eq!(f.call(multiply_ABI, &[OUTPUT, RIGHT, LEFT]), OUTPUT);
    assert_eq!(
        f.get::<16>(OUTPUT),
        reverse,
        "row-vector multiplication is noncommutative"
    );
    f.put(LEFT, A);
    assert_eq!(f.call(multiply_ABI, &[LEFT, LEFT, LEFT]), LEFT);
    assert_eq!(
        f.get::<16>(LEFT),
        [
            4., 0., 0., 0., 0., 9., 0., 0., 0., 0., 1., 0., 9., -20., 0., 1.
        ]
    );
    let numbered: Matrix = std::array::from_fn(|i| i as f32);
    for output in [OUTPUT, LEFT, LEFT + 4] {
        f.put(LEFT, numbered);
        assert_eq!(f.call(transpose_ABI, &[output, LEFT]), output);
        assert_eq!(
            f.get::<16>(output),
            [
                0., 4., 8., 12., 1., 5., 9., 13., 2., 6., 10., 14., 3., 7., 11., 15.
            ]
        );
    }
}

#[test]
fn left_handed_camera_and_viewport_projection_match_native_conventions() {
    let mut f = Fixture::new();
    let general = native([
        0x3f7e6f2b, 0x3d36c496, 0xbdcee115, 0, 0x80000000, 0x3f6a2be1, 0x3ecee115, 0, 0x3de229ed,
        0xbecd9d29, 0x3f68bd38, 0, 0xbfd4074f, 0xbec230de, 0x4055581e, 0x3f800000,
    ]);
    for (eye, at, up, expected) in [
        (
            [1f32, 2., 3.],
            [1f32, 2., 4.],
            [0f32, 1., 0.],
            [
                1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., -1., -2., -3., 1.,
            ],
        ),
        ([2f32, -1., -3.], [1f32, 3., 6.], [0f32, 1., 0.], general),
    ] {
        for output in [OUTPUT, VECTOR] {
            f.put(VECTOR, eye);
            f.put(VECTOR + 0x10, at);
            f.put(VECTOR + 0x20, up);
            assert_eq!(
                f.call(look_at_ABI, &[output, VECTOR, VECTOR + 0x10, VECTOR + 0x20]),
                output
            );
            close(f.get::<16>(output), expected);
        }
    }
    f.put(VECTOR, [0f32; 3]);
    f.put(VECTOR + 0x10, [0f32; 3]);
    assert_eq!(
        f.call(look_at_ABI, &[OUTPUT, VECTOR, VECTOR + 0x10, VECTOR + 0x20]),
        OUTPUT
    );
    let mut degenerate = [0f32; 16];
    degenerate[15] = 1.;
    close(f.get::<16>(OUTPUT), degenerate);
    for output in [OUTPUT, VECTOR, LEFT + 4] {
        f.projection_inputs();
        assert_eq!(
            f.call(
                project_ABI,
                &[output, VECTOR, VIEWPORT, PROJECTION, VIEW, LEFT]
            ),
            output
        );
        close(
            f.get::<3>(output),
            native([0x448d9fff, 0x43090000, 0x3e8ccccd]),
        );
    }
    // View translation must occur after world scaling/translation. The projected
    // point is outside the viewport; Project computes it and does not clip it.
    f.projection_inputs();
    f.put(VIEW, B);
    assert_eq!(
        f.call(
            project_ABI,
            &[OUTPUT, VECTOR, VIEWPORT, PROJECTION, VIEW, LEFT]
        ),
        OUTPUT
    );
    close(f.get::<3>(OUTPUT), [813., -343., 0.275]);
    let mut zero_w = IDENTITY;
    zero_w[15] = 0.;
    f.put(PROJECTION, zero_w);
    assert_eq!(
        f.call(
            project_ABI,
            &[OUTPUT, VECTOR, VIEWPORT, PROJECTION, VIEW, LEFT]
        ),
        OUTPUT
    );
    assert!(f.get::<3>(OUTPUT).into_iter().all(f32::is_nan));
}

#[test]
fn unreadable_inputs_and_unwritable_outputs_fail_without_partial_writes() {
    let mut f = Fixture::new();
    f.projection_inputs();
    f.put(VECTOR + 0x10, [0f32, 0., 1.]);
    f.put(VECTOR + 0x20, [0f32, 1., 0.]);
    let contracts = [
        (multiply_ABI, vec![OUTPUT, LEFT, RIGHT], 64),
        (transpose_ABI, vec![OUTPUT, LEFT], 64),
        (scaling_ABI, vec![OUTPUT, 0x3acccccd, 0x3b360b61, 0], 64),
        (translation_ABI, vec![OUTPUT, 0xbf800000, 0xbf800000, 0], 64),
        (
            look_at_ABI,
            vec![OUTPUT, VECTOR, VECTOR + 0x10, VECTOR + 0x20],
            64,
        ),
        (
            project_ABI,
            vec![OUTPUT, VECTOR, VIEWPORT, PROJECTION, VIEW, LEFT],
            12,
        ),
    ];
    for (api, args, size) in contracts {
        for index in 1..args.len() {
            if signature::lookup(api.module, api.name).unwrap().parameters[index].kind
                != AbiKind::Pointer
            {
                continue;
            }
            for bad in [0, UNMAPPED, WRITE_ONLY, DATA + 4090, u32::MAX - 8] {
                f.0.cpu.write(OUTPUT, &[0xcd; 64]).unwrap();
                let mut args = args.clone();
                args[index] = bad;
                assert_eq!(
                    f.call(api, &args),
                    0,
                    "{} input {index} address {bad:#x}",
                    api.name
                );
                assert_eq!(f.bytes(OUTPUT, 64), vec![0xcd; 64]);
            }
        }
        for output in [0, READ_ONLY, UNMAPPED, DATA + 4088, u32::MAX - 8] {
            f.0.cpu.write(READ_ONLY, &[0x77; 64]).unwrap();
            f.0.cpu.write(DATA + 4088, &[0x66; 8]).unwrap();
            let mut args = args.clone();
            args[0] = output;
            assert_eq!(
                f.call(api, &args),
                0,
                "{} {size}-byte output {output:#x}",
                api.name
            );
            assert_eq!(f.bytes(READ_ONLY, 64), vec![0x77; 64]);
            assert_eq!(f.bytes(DATA + 4088, 8), vec![0x66; 8]);
        }
    }
}

#[test]
fn rotation_projection_and_vec4_native_math_aliases_and_x86_float_contracts() {
    // Authored five-row native probe, source SHA256
    // DA63E284798101E1970E2FCC7C34A34875C6335EB730BF85E3E237BC5C99D5D7.
    // DLL SHA is the same published oracle in data/abi/sdk-source.json.
    let homogeneous: Matrix = [
        2., 0., 0., 0., 0., 3., 0., 0., 0., 0., 4., 0.5, 5., 7., 11., 1.,
    ];
    let vector: Vector4 = [2., -3., 4., 2.];
    let rotation = native([
        0x3f0faf32, 0x3f291324, 0xbeff60e0, 0, 0xbf2f5aee, 0x3f357d40, 0x3e2bf68a, 0, 0x3eedd5be,
        0x3e7d5776, 0x3f59ad4e, 0, 0, 0, 0, 0x3f800000,
    ]);
    let rotation_y = native([
        0x3f60a940, 0, 0xbef57744, 0, 0, 0x3f800000, 0, 0, 0x3ef57744, 0, 0x3f60a940, 0, 0, 0, 0,
        0x3f800000,
    ]);
    let projection = native([
        0x3f9c339d, 0, 0, 0, 0, 0x3fea4d6b, 0, 0, 0, 0, 0x3f8020cd, 0x3f800000, 0, 0, 0xbdcd0148, 0,
    ]);
    let constructors = [
        (
            rotation_yaw_pitch_roll_ABI,
            vec![0.5f32.to_bits(), (-0.25f32).to_bits(), 0.75f32.to_bits()],
            rotation,
        ),
        (rotation_y_ABI, vec![0.5f32.to_bits()], rotation_y),
        (
            perspective_fov_lh_ABI,
            vec![
                1f32.to_bits(),
                1.5f32.to_bits(),
                0.1f32.to_bits(),
                100f32.to_bits(),
            ],
            projection,
        ),
    ];
    // These typed calls also exercise the actual stdcall return path: the
    // 4/2/5/3 DWORD argument slots, pointer return and ESP are all observable.
    let mut registry = Registry::default();
    register(&mut registry);
    let mut machine = machine();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
        .cpu
        .write(LEFT, bytemuck::bytes_of(&homogeneous))
        .unwrap();
    machine
        .cpu
        .write(VECTOR, bytemuck::bytes_of(&vector))
        .unwrap();
    let mut contracts: Vec<_> = constructors
        .iter()
        .map(|(api, scalars, _)| {
            let mut arguments = vec![OUTPUT];
            arguments.extend_from_slice(scalars);
            (*api, arguments)
        })
        .collect();
    contracts.push((transform4_ABI, vec![OUTPUT, VECTOR, LEFT]));
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x700);
    for (index, (api, args)) in contracts.iter().enumerate() {
        let declared = signature::lookup(api.module, api.name).unwrap();
        assert_eq!(declared.return_kind, AbiKind::Pointer);
        assert_eq!(declared.stack_words(), args.len() as u16);
        for (index, parameter) in declared.parameters.iter().enumerate() {
            assert_eq!(
                parameter.kind,
                if api.name != "D3DXVec4Transform" && index > 0 {
                    AbiKind::F32
                } else {
                    AbiKind::Pointer
                }
            );
        }
        assert_eq!(
            registry.level(api.module, api.name),
            Some(ImplementationLevel::Implement)
        );
        assert!(registry.dynamic_available(api.module, api.name));
        let entry = machine.resolve_import(api.module, api.name).unwrap();
        for arg in args.iter().rev() {
            code.push_imm(*arg);
        }
        code.call(entry)
            .mov_mem_eax(DATA + 0x710 + index as u32 * 4);
    }
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x704);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored D3DX rotation/projection", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        machine.cpu.read_u32(DATA + 0x700).unwrap(),
        machine.cpu.read_u32(DATA + 0x704).unwrap()
    );
    for index in 0..4 {
        assert_eq!(
            machine.cpu.read_u32(DATA + 0x710 + index * 4).unwrap(),
            OUTPUT
        );
    }
    assert!(registry.signature_conflicts().is_empty());

    let mut fixture = Fixture::new();
    for (api, scalars, expected) in constructors {
        for output in [OUTPUT, WRITE_ONLY, STACK + 4] {
            let mut arguments = vec![output];
            arguments.extend_from_slice(&scalars);
            assert_eq!(fixture.call(api, &arguments), output);
            close(fixture.get::<16>(output), expected);
        }
    }
    for output in [OUTPUT, VECTOR, VECTOR + 4, LEFT, LEFT + 4, WRITE_ONLY] {
        fixture.put(LEFT, homogeneous);
        fixture.put(VECTOR, vector);
        assert_eq!(
            fixture.call(transform4_ABI, &[output, VECTOR, LEFT]),
            output
        );
        assert_eq!(fixture.get::<4>(output), [14., 5., 38., 4.]);
    }
    // LH near/far map to normalized depth 0/1, without dividing inside Vec4.
    fixture.put(LEFT, projection);
    for (z, expected) in [(0.1, 0.), (100., 1.)] {
        fixture.put(VECTOR, [0f32, 0., z, 1.]);
        assert_eq!(
            fixture.call(transform4_ABI, &[OUTPUT, VECTOR, LEFT]),
            OUTPUT
        );
        let value = fixture.get::<4>(OUTPUT);
        close([value[2] / value[3]], [expected]);
        close([value[3]], [z]);
    }
    assert_eq!(
        fixture.call(
            perspective_fov_lh_ABI,
            &[
                OUTPUT,
                1f32.to_bits(),
                1.5f32.to_bits(),
                1f32.to_bits(),
                1f32.to_bits()
            ]
        ),
        OUTPUT
    );
    let value = fixture.get::<16>(OUTPUT);
    assert_eq!(value[10], f32::INFINITY);
    assert_eq!(value[14], f32::NEG_INFINITY);
    assert_eq!(value[11], 1.);
    // A failed full-range input or output guard preserves every writable byte.
    for (api, arguments) in contracts {
        for output in [0, READ_ONLY, DATA + 4088, u32::MAX - 8] {
            fixture.0.cpu.write(READ_ONLY, &[0x77; 64]).unwrap();
            fixture.0.cpu.write(DATA + 4088, &[0x66; 8]).unwrap();
            let mut arguments = arguments.clone();
            arguments[0] = output;
            assert_eq!(fixture.call(api, &arguments), 0);
            assert_eq!(fixture.bytes(READ_ONLY, 64), vec![0x77; 64]);
            assert_eq!(fixture.bytes(DATA + 4088, 8), vec![0x66; 8]);
        }
    }
    for source in [0, WRITE_ONLY, DATA + 4090, u32::MAX - 8] {
        fixture.0.cpu.write(OUTPUT, &[0xcd; 16]).unwrap();
        assert_eq!(fixture.call(transform4_ABI, &[OUTPUT, source, LEFT]), 0);
        assert_eq!(fixture.bytes(OUTPUT, 16), vec![0xcd; 16]);
        assert_eq!(fixture.call(transform4_ABI, &[OUTPUT, VECTOR, source]), 0);
        assert_eq!(fixture.bytes(OUTPUT, 16), vec![0xcd; 16]);
    }
}

#[test]
fn vec3_transforms_and_catmull_rom_match_native_with_aliases_and_atomic_bounds() {
    // Six-row authored native probe, source SHA256
    // 18377D6CA6258329B2011CB4C95D41AE93E094802826D6F79E529E765B6EE2BB.
    let matrix: Matrix = [
        2., 0., 0., 0., 0., 3., 0., 0., 0., 0., 4., 0.5, 5., 7., 11., 1.,
    ];
    let vector: Vector = [2., -3., 4.];
    let points: [Vector; 4] = [[1., 2., 3.], [2., -1., 4.], [-3., 5., 0.], [4., 1., -2.]];
    let control: [u32; 4] = std::array::from_fn(|i| VECTOR + i as u32 * 0x20);
    let coord = native([0x403fffff, 0xbf2aaaaa, 0x410fffff]);
    let contracts = [
        (transform3_ABI, vec![OUTPUT, VECTOR, LEFT]),
        (transform_coord_ABI, vec![OUTPUT, VECTOR, LEFT]),
        (
            catmull_rom_ABI,
            vec![
                OUTPUT,
                control[0],
                control[1],
                control[2],
                control[3],
                0.25f32.to_bits(),
            ],
        ),
    ];
    let mut registry = Registry::default();
    register(&mut registry);
    let mut machine = machine();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
        .cpu
        .write(LEFT, bytemuck::bytes_of(&matrix))
        .unwrap();
    for (address, value) in control.into_iter().zip(points) {
        machine
            .cpu
            .write(address, bytemuck::bytes_of(&value))
            .unwrap();
    }
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x700);
    for (index, (api, arguments)) in contracts.iter().enumerate() {
        let declared = signature::lookup(api.module, api.name).unwrap();
        assert_eq!(declared.return_kind, AbiKind::Pointer);
        assert_eq!(declared.stack_words(), arguments.len() as u16);
        assert_eq!(
            registry.level(api.module, api.name),
            Some(ImplementationLevel::Implement)
        );
        assert!(registry.dynamic_available(api.module, api.name));
        for (slot, parameter) in declared.parameters.iter().enumerate() {
            assert_eq!(
                parameter.kind,
                if api.name == "D3DXVec3CatmullRom" && slot == 5 {
                    AbiKind::F32
                } else {
                    AbiKind::Pointer
                }
            );
        }
        let entry = machine.resolve_import(api.module, api.name).unwrap();
        for argument in arguments.iter().rev() {
            code.push_imm(*argument);
        }
        code.call(entry)
            .mov_mem_eax(DATA + 0x710 + index as u32 * 4);
    }
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x704);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored D3DX Vec3", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        machine.cpu.read_u32(DATA + 0x700).unwrap(),
        machine.cpu.read_u32(DATA + 0x704).unwrap()
    );
    for index in 0..3 {
        assert_eq!(
            machine.cpu.read_u32(DATA + 0x710 + index * 4).unwrap(),
            OUTPUT
        );
    }
    assert!(registry.signature_conflicts().is_empty());

    let mut fixture = Fixture::new();
    for output in [OUTPUT, VECTOR, VECTOR + 4, LEFT, LEFT + 4, WRITE_ONLY] {
        fixture.put(LEFT, matrix);
        fixture.put(VECTOR, vector);
        assert_eq!(
            fixture.call(transform3_ABI, &[output, VECTOR, LEFT]),
            output
        );
        assert_eq!(fixture.get::<4>(output), [9., -2., 27., 3.]);
        fixture.put(LEFT, matrix);
        fixture.put(VECTOR, vector);
        assert_eq!(
            fixture.call(transform_coord_ABI, &[output, VECTOR, LEFT]),
            output
        );
        close(fixture.get::<3>(output), coord);
    }
    let mut zero_w = matrix;
    zero_w[11] = 0.;
    zero_w[15] = 0.;
    fixture.put(LEFT, zero_w);
    fixture.put(VECTOR, vector);
    assert_eq!(
        fixture.call(transform_coord_ABI, &[OUTPUT, VECTOR, LEFT]),
        OUTPUT
    );
    assert_eq!(fixture.get::<3>(OUTPUT).map(f32::to_bits), [0xffc00000; 3]);
    for (s, expected) in [
        (0.25f32, native([0x3f640000, 0x3dd00000, 0x40538000])),
        (-0.5, native([0xbf900000, 0x403c0000, 0x40340000])),
        (1.5, native([0x40180000, 0x3f300000, 0x3d800000])),
        (0., points[1]),
        (1., points[2]),
    ] {
        for output in [
            OUTPUT,
            control[0],
            control[1] + 4,
            control[2],
            control[3],
            WRITE_ONLY,
            STACK + 4,
        ] {
            for (address, point) in control.into_iter().zip(points) {
                fixture.put(address, point);
            }
            assert_eq!(
                fixture.call(
                    catmull_rom_ABI,
                    &[
                        output,
                        control[0],
                        control[1],
                        control[2],
                        control[3],
                        s.to_bits()
                    ]
                ),
                output
            );
            close(fixture.get::<3>(output), expected);
        }
    }
    // A Vec3 ends at its third DWORD. Do not read an implicit fourth DWORD;
    // Coord writes twelve bytes, whereas Transform requires sixteen bytes.
    let exact_vec3 = DATA + 4096 - 12;
    fixture.put(LEFT, matrix);
    fixture.put(exact_vec3, vector);
    assert_eq!(
        fixture.call(transform3_ABI, &[OUTPUT, exact_vec3, LEFT]),
        OUTPUT
    );
    assert_eq!(fixture.get::<4>(OUTPUT), [9., -2., 27., 3.]);
    fixture.put(VECTOR, vector);
    assert_eq!(
        fixture.call(transform_coord_ABI, &[exact_vec3, VECTOR, LEFT]),
        exact_vec3
    );
    let before = fixture.bytes(exact_vec3, 12);
    assert_eq!(fixture.call(transform3_ABI, &[exact_vec3, VECTOR, LEFT]), 0);
    assert_eq!(fixture.bytes(exact_vec3, 12), before);
    for (api, arguments) in contracts {
        for slot in 1..arguments.len() {
            if signature::lookup(api.module, api.name).unwrap().parameters[slot].kind
                != AbiKind::Pointer
            {
                continue;
            }
            fixture.0.cpu.write(OUTPUT, &[0xcd; 16]).unwrap();
            let mut arguments = arguments.clone();
            arguments[slot] = DATA + 4090;
            assert_eq!(fixture.call(api, &arguments), 0);
            assert_eq!(fixture.bytes(OUTPUT, 16), vec![0xcd; 16]);
        }
        for output in [0, READ_ONLY, DATA + 4088, u32::MAX - 8] {
            fixture.0.cpu.write(READ_ONLY, &[0x77; 16]).unwrap();
            fixture.0.cpu.write(DATA + 4088, &[0x66; 8]).unwrap();
            let mut arguments = arguments.clone();
            arguments[0] = output;
            assert_eq!(fixture.call(api, &arguments), 0);
            assert_eq!(fixture.bytes(READ_ONLY, 16), vec![0x77; 16]);
            assert_eq!(fixture.bytes(DATA + 4088, 8), vec![0x66; 8]);
        }
    }
}

#[test]
fn unavailable_surface_encoding_clears_output_without_reading_surface() {
    let mut registry = Registry::default();
    register(&mut registry);
    let api = registry
        .get("d3dx9_43.dll", "D3DXSaveSurfaceToFileInMemory")
        .unwrap();
    let api = adv_abi::Api {
        module: "d3dx9_43.dll",
        name: "D3DXSaveSurfaceToFileInMemory",
        conv: api.conv,
        env_leaf: None,
        handler: api.handler,
        level: ImplementationLevel::SafeFail,
    };
    let mut fixture = Fixture::new();
    for output in [0, UNMAPPED, READ_ONLY, OUTPUT] {
        fixture.put(OUTPUT, 0x13579bdfu32);
        assert_eq!(
            fixture.call(api, &[output, 1, UNMAPPED, UNMAPPED, UNMAPPED]),
            0x8876086c
        );
        assert_eq!(
            fixture.0.cpu.read_u32(OUTPUT).unwrap(),
            if output == OUTPUT { 0 } else { 0x13579bdf }
        );
    }
}

// spec: WIN32-API-15, A64-CTL-12
#[test]
fn math_leaves_match_all_exports_and_decline_invalid_ranges_without_writes() {
    for module in ["d3dx9_43.dll", "d3dx9_29.dll"] {
        for api in [
            multiply_ABI,
            transpose_ABI,
            scaling_ABI,
            translation_ABI,
            rotation_yaw_pitch_roll_ABI,
            rotation_y_ABI,
            rotation_x_ABI,
            rotation_z_ABI,
            perspective_fov_lh_ABI,
            transform4_ABI,
            transform3_ABI,
            transform_coord_ABI,
            transform2_ABI,
            transform2_coord_ABI,
            transform2_normal_ABI,
            catmull_rom_ABI,
            look_at_ABI,
            project_ABI,
        ] {
            let mut args = match api.name {
                "D3DXMatrixMultiply" => vec![OUTPUT, LEFT, RIGHT],
                "D3DXMatrixTranspose" => vec![OUTPUT, LEFT],
                "D3DXMatrixLookAtLH" => vec![OUTPUT, VECTOR, VECTOR, VECTOR],
                "D3DXVec3CatmullRom" => {
                    vec![OUTPUT, VECTOR, VECTOR, VECTOR, VECTOR, 0.4f32.to_bits()]
                }
                "D3DXVec3Project" => vec![OUTPUT, VECTOR, VIEWPORT, PROJECTION, VIEW, LEFT],
                name if name.starts_with("D3DXVec") => vec![OUTPUT, VECTOR, LEFT],
                _ => {
                    let Conv::Stdcall { argc } = api.conv else {
                        unreachable!()
                    };
                    let mut args = vec![0.4f32.to_bits(); usize::from(argc)];
                    args[0] = OUTPUT;
                    args
                }
            };
            let mut cases = Vec::new();
            for output in [OUTPUT, LEFT, READ_ONLY, UNMAPPED, DATA + 4092] {
                args[0] = output;
                cases.push(args.clone());
            }
            args[0] = OUTPUT;
            for i in 1..args.len() {
                if [LEFT, RIGHT, VECTOR, VIEWPORT, PROJECTION, VIEW].contains(&args[i]) {
                    let mut invalid = args.clone();
                    invalid[i] = UNMAPPED;
                    cases.push(invalid);
                }
            }
            for args in cases {
                let output = args[0];
                let mut fast = Fixture::new();
                let mut ordinary = Fixture::new();
                for fixture in [&mut fast, &mut ordinary] {
                    fixture.projection_inputs();
                    fixture.put(OUTPUT, [0xababababu32; 16]);
                }
                let expected = ordinary.call(api, &args);
                let entry = fast.0.resolve_import(module, api.name).unwrap();
                let thunk =
                    (entry - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
                fast.0.cpu.set_reg(Reg::Esp, STACK);
                for (i, arg) in args.iter().enumerate() {
                    fast.0
                        .cpu
                        .write_u32(STACK + 4 + i as u32 * 4, *arg)
                        .unwrap();
                }
                let before = fast.bytes(DATA, 4096);
                let binding = fast.0.sys.thunks.get(thunk).f;
                let mut env = Env {
                    context: fast.0.sys.current,
                    cpu: &mut fast.0.cpu,
                    sys: &mut fast.0.sys,
                    ext: &mut fast.0.ext,
                    thunk,
                };
                let value = binding.env_leaf.unwrap()(&mut env);
                if [READ_ONLY, UNMAPPED, DATA + 4092].contains(&output) || args.contains(&UNMAPPED)
                {
                    assert_eq!(value, None, "{}", api.name);
                    assert_eq!(fast.bytes(DATA, 4096), before);
                    assert_eq!(fast.call(api, &args), expected);
                } else {
                    assert_eq!(value.unwrap() as u32, expected, "{}", api.name);
                }
                assert_eq!(
                    fast.bytes(DATA, 4096),
                    ordinary.bytes(DATA, 4096),
                    "{}",
                    api.name
                );
                assert_eq!(
                    fast.0.cpu.read_u32(teb_addr(0) + teb::LAST_ERROR).unwrap(),
                    ERROR
                );
            }
        }
    }
}
