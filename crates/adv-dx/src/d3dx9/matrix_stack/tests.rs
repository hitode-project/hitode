//! Authored D3DX9 matrix-stack oracle source SHA256:
//! FE6063286513CC9A440F4D8E16759A4F9ABB282B55E27F6294EC16D358E9DF60.
use super::*;
use adv_abi::{Conv, ImplementationLevel, Outcome, guest::memory_failure};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x600000;
const STACK_CELL: u32 = DATA;
const BASE: u32 = DATA + 4;
const PUSHED: u32 = DATA + 8;
const LAST_LEVEL: u32 = DATA + 12;
const TABLE: u32 = DATA + 16;
const RESULT: u32 = DATA + 0x100;
const MATRIX_A: u32 = DATA + 0x800;
const MATRIX_B: u32 = DATA + 0x840;
const AXIS: u32 = DATA + 0x880;
const IID: u32 = DATA + 0x900;
const IID_UNKNOWN: u32 = IID + 16;
const BAD_IID: u32 = IID + 32;
const QI: u32 = IID + 64;
const READ_ONLY: u32 = 0x610000;
const SNAPSHOTS: u32 = DATA + 0x1000;
const A: Matrix = [
    2., 0., 0., 0., 0., 3., 0., 0., 0., 0., 4., 0., 5., 7., 11., 1.,
];
const B: Matrix = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 1., 2., 3., 1.,
];

fn push_memory(code: &mut Asm, cell: u32) {
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&cell.to_le_bytes());
}
fn call(code: &mut Asm, target: u32, arguments: &[u32]) {
    for argument in arguments.iter().rev() {
        code.push_imm(*argument);
    }
    code.call(target);
}
fn method(code: &mut Asm, slot: u8, arguments: &[u32]) {
    for argument in arguments.iter().rev() {
        code.push_imm(*argument);
    }
    push_memory(code, STACK_CELL);
    // Fetch the guest object's vtable and invoke its slot, rather than calling
    // a private Rust handler or routing through the current external function.
    code.mov_eax_mem(STACK_CELL).mov_eax_ptr_eax();
    code.bytes
        .extend_from_slice(&[0x8b, 0x40, slot * 4, 0xff, 0xd0]);
}
fn snapshot(code: &mut Asm, helper: u32, index: u32) {
    method(code, 17, &[]);
    code.push_imm(SNAPSHOTS + index * 64).bytes.push(0x50); // push eax.
    code.call(helper);
}
fn read_matrix(machine: &Machine<JitCpu>, index: u32) -> Matrix {
    let mut bytes = [0; 64];
    machine
        .cpu
        .read(SNAPSHOTS + index * 64, &mut bytes)
        .unwrap();
    std::array::from_fn(|i| f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()))
}
fn close(actual: Matrix, expected: Matrix) {
    for (a, e) in actual.into_iter().zip(expected) {
        assert!((a - e).abs() <= 2e-6 * e.abs().max(1.), "{a} != {e}");
    }
}

#[test]
fn guest_matrix_stack_vtable_preserves_writable_tops_order_aliases_and_com_lifetime() {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    super::super::register(&mut registry);
    let signatures: Vec<_> = adv_abi::signature::com_signatures()
        .iter()
        .filter(|s| s.module == "ID3DXMatrixStack")
        .collect();
    assert_eq!(signatures.len(), 18);
    for signature in signatures {
        let name = format!("ID3DXMatrixStack::{}", signature.name);
        let api = registry.get("d3dx9_43.dll", &name).unwrap();
        assert_eq!(
            api.conv,
            Conv::Stdcall {
                argc: signature.stack_words() as u8
            }
        );
        assert_eq!(
            registry.level("d3dx9_43.dll", &name),
            Some(ImplementationLevel::Implement)
        );
        assert!(registry.dynamic_available("d3dx9_43.dll", &name));
    }
    assert!(registry.signature_conflicts().is_empty());
    registry.add(
        "fixture.dll",
        "snapshot",
        Conv::Stdcall { argc: 2 },
        |env| match GuestPtr::<Matrix>::new(env.arg(0))
            .read(env.cpu)
            .and_then(|value| GuestPtr::new(env.arg(1)).write(env.cpu, value))
        {
            Ok(()) => Outcome::Return(0),
            Err(error) => memory_failure(error),
        },
    );
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/d3dx-stack-tests");
    adv_win32::Win32::install(
        &mut machine,
        adv_win32::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, 8192, Prot::RX).unwrap();
    machine.cpu.map(DATA, 8192, Prot::RW).unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    machine.cpu.write(READ_ONLY, &[0xcd; 4]).unwrap();
    machine.cpu.write(MATRIX_A, bytemuck::bytes_of(&A)).unwrap();
    machine.cpu.write(MATRIX_B, bytemuck::bytes_of(&B)).unwrap();
    machine
        .cpu
        .write(AXIS, bytemuck::bytes_of(&[0f32, 2., 0.]))
        .unwrap();
    machine.cpu.write(IID, &IID_STACK).unwrap();
    machine
        .cpu
        .write(IID_UNKNOWN, &adv_os::com::IUNKNOWN)
        .unwrap();
    machine.cpu.write(BAD_IID, &[0xab; 16]).unwrap();
    let create = machine
        .resolve_import("d3dx9_43.dll", "D3DXCreateMatrixStack")
        .unwrap();
    let helper = machine.resolve_import("fixture.dll", "snapshot").unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(RESULT + 200);
    call(&mut code, create, &[1, QI]);
    code.mov_mem_eax(RESULT);
    call(&mut code, create, &[0, READ_ONLY]);
    code.mov_mem_eax(RESULT + 4);
    call(&mut code, create, &[0, STACK_CELL]);
    code.mov_mem_eax(RESULT + 8);
    code.mov_eax_mem(STACK_CELL)
        .mov_eax_ptr_eax()
        .mov_mem_eax(TABLE);
    method(&mut code, 17, &[]);
    code.mov_mem_eax(BASE);
    snapshot(&mut code, helper, 0);
    method(&mut code, 3, &[]);
    code.mov_mem_eax(RESULT + 12);
    method(&mut code, 17, &[]);
    code.mov_mem_eax(RESULT + 16);
    // COM QI sees the declared identity and increments the same shared count.
    method(&mut code, 0, &[IID_UNKNOWN, QI]);
    code.mov_mem_eax(RESULT + 20);
    code.mov_eax_mem(QI).mov_mem_eax(RESULT + 24);
    method(&mut code, 0, &[IID, QI]);
    code.mov_mem_eax(RESULT + 28);
    code.mov_eax_mem(QI).mov_mem_eax(RESULT + 32);
    method(&mut code, 0, &[IID, READ_ONLY]);
    code.mov_mem_eax(RESULT + 36);
    method(&mut code, 1, &[]);
    code.mov_mem_eax(RESULT + 40);
    for i in [44, 48, 52] {
        method(&mut code, 2, &[]);
        code.mov_mem_eax(RESULT + i);
    }
    method(&mut code, 0, &[BAD_IID, QI]);
    code.mov_mem_eax(RESULT + 56);
    code.mov_eax_mem(QI).mov_mem_eax(RESULT + 60);
    // Write through GetTop in guest code; Push must read these actual bytes.
    code.mov_eax_mem(BASE)
        .bytes
        .extend_from_slice(&[0xc7, 0x40, 48]);
    code.bytes.extend_from_slice(&2f32.to_bits().to_le_bytes());
    method(&mut code, 4, &[]);
    code.mov_mem_eax(RESULT + 64);
    method(&mut code, 17, &[]);
    code.mov_mem_eax(PUSHED);
    snapshot(&mut code, helper, 1);
    method(
        &mut code,
        13,
        &[3f32.to_bits(), 4f32.to_bits(), 5f32.to_bits()],
    );
    snapshot(&mut code, helper, 2);
    method(&mut code, 3, &[]);
    method(&mut code, 17, &[]);
    code.mov_mem_eax(RESULT + 68);
    snapshot(&mut code, helper, 3);
    method(
        &mut code,
        14,
        &[3f32.to_bits(), 4f32.to_bits(), 5f32.to_bits()],
    );
    snapshot(&mut code, helper, 4);
    method(&mut code, 6, &[MATRIX_A]);
    method(&mut code, 7, &[MATRIX_B]);
    snapshot(&mut code, helper, 5);
    method(&mut code, 6, &[MATRIX_A]);
    method(&mut code, 8, &[MATRIX_B]);
    snapshot(&mut code, helper, 6);
    // An input which aliases GetTop must be fully captured before replacement.
    method(&mut code, 17, &[]);
    code.bytes.push(0x50);
    push_memory(&mut code, STACK_CELL);
    code.mov_eax_mem(STACK_CELL)
        .mov_eax_ptr_eax()
        .bytes
        .extend_from_slice(&[0x8b, 0x40, 28, 0xff, 0xd0]);
    snapshot(&mut code, helper, 7);
    method(&mut code, 7, &[DATA + 8192 - 32]);
    code.mov_mem_eax(RESULT + 72);
    snapshot(&mut code, helper, 8);
    method(&mut code, 5, &[]);
    method(&mut code, 9, &[AXIS, 0.5f32.to_bits()]);
    snapshot(&mut code, helper, 9);
    method(&mut code, 5, &[]);
    method(
        &mut code,
        11,
        &[0.5f32.to_bits(), (-0.25f32).to_bits(), 0.75f32.to_bits()],
    );
    snapshot(&mut code, helper, 10);
    method(&mut code, 17, &[]);
    code.mov_mem_eax(RESULT + 76);
    method(&mut code, 4, &[]);
    method(&mut code, 17, &[]);
    code.mov_mem_eax(LAST_LEVEL);
    method(&mut code, 2, &[]);
    code.mov_mem_eax(RESULT + 80);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(RESULT + 204);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored D3DX matrix stack", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let word = |at| machine.cpu.read_u32(RESULT + at).unwrap();
    let pointer = machine.cpu.read_u32(STACK_CELL).unwrap();
    let base = machine.cpu.read_u32(BASE).unwrap();
    let pushed = machine.cpu.read_u32(PUSHED).unwrap();
    assert_ne!(pointer, 0);
    assert_ne!(base, 0);
    assert_ne!(pushed, base);
    for (offset, expected) in [
        (0, INVALID),
        (4, INVALID),
        (8, OK),
        (12, OK),
        (16, base),
        (20, OK),
        (24, pointer),
        (28, OK),
        (32, pointer),
        (36, POINTER),
        (40, 4),
        (44, 3),
        (48, 2),
        (52, 1),
        (56, NO_INTERFACE),
        (60, 0),
        (64, OK),
        (68, base),
        (72, INVALID),
        (76, base),
        (80, 0),
    ] {
        assert_eq!(word(offset), expected, "result offset {offset}");
    }
    assert_eq!(word(200), word(204), "stdcall calls balance the x86 stack");
    assert_eq!(machine.cpu.read_u32(READ_ONLY).unwrap(), 0xcdcdcdcd);
    close(read_matrix(&machine, 0), IDENTITY);
    let mut translated = IDENTITY;
    translated[12] = 2.;
    close(read_matrix(&machine, 1), translated);
    close(read_matrix(&machine, 3), translated);
    let mut post_scale = scaling_value(3., 4., 5.);
    post_scale[12] = 6.;
    close(read_matrix(&machine, 2), post_scale);
    let mut local_scale = scaling_value(3., 4., 5.);
    local_scale[12] = 2.;
    close(read_matrix(&machine, 4), local_scale);
    let mut post = A;
    post[12..15].copy_from_slice(&[6., 9., 14.]);
    let mut local = A;
    local[12..15].copy_from_slice(&[7., 13., 23.]);
    close(read_matrix(&machine, 5), post);
    close(read_matrix(&machine, 6), local);
    close(
        read_matrix(&machine, 7),
        [
            4., 0., 0., 0., 0., 9., 0., 0., 0., 0., 16., 0., 21., 52., 115., 1.,
        ],
    );
    close(read_matrix(&machine, 8), read_matrix(&machine, 7));
    close(
        read_matrix(&machine, 9),
        [
            f32::from_bits(0x3f60a940),
            0.,
            f32::from_bits(0xbef57743),
            0.,
            0.,
            1.,
            0.,
            0.,
            f32::from_bits(0x3ef57743),
            0.,
            f32::from_bits(0x3f60a940),
            0.,
            0.,
            0.,
            0.,
            1.,
        ],
    );
    close(
        read_matrix(&machine, 10),
        [
            f32::from_bits(0x3f0faf32),
            f32::from_bits(0x3f291324),
            f32::from_bits(0xbeff60e0),
            0.,
            f32::from_bits(0xbf2f5aee),
            f32::from_bits(0x3f357d40),
            f32::from_bits(0x3e2bf68a),
            0.,
            f32::from_bits(0x3eedd5be),
            f32::from_bits(0x3e7d5776),
            f32::from_bits(0x3f59ad4e),
            0.,
            0.,
            0.,
            0.,
            1.,
        ],
    );
    let store_com = machine.ext.get::<Store>().com;
    let table = machine.ext.get::<Store>().table;
    assert!(
        store_com
            .identity::<Stack>(&machine.sys.objects, pointer)
            .is_none()
    );
    let heap = machine.ext.get::<Heaps>().process_heap;
    for address in [pointer, base, machine.cpu.read_u32(LAST_LEVEL).unwrap()] {
        assert_eq!(machine.ext.get_mut::<Heaps>().size(heap, address), None);
    }
    assert_eq!(table, machine.cpu.read_u32(TABLE).unwrap());
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert!(diagnostics.warnings.keys().any(|(api, reason)| api
        == "d3dx9_43.dll!D3DXCreateMatrixStack"
        && reason.contains("flags")));
}
