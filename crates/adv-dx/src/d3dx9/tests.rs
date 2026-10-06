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
        assert!(
            declared
                .parameters
                .iter()
                .all(|p| p.kind == AbiKind::Pointer)
        );
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
    for index in 0..4 {
        assert_eq!(m.cpu.read_u32(DATA + 0x710 + index * 4).unwrap(), OUTPUT);
    }
    assert!(registry.signature_conflicts().is_empty());
    assert!(!registry.dynamic_available("d3dx9_42.dll", "D3DXMatrixMultiply"));
    assert!(registry.dynamic_available("d3dx9_43.dll", "D3DXCreateEffect"));
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
