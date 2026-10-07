//! Four no-HWND native cases: target/engine-loop/20261007-resume/inflate-rect-native.
//! Authored source SHA256 3CB8D44BF66C0CA82A4795A5408459DB2A8B5DCA456C9879FF2F0355461EDF46.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use std::sync::atomic::{AtomicUsize, Ordering};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;
const BOUNDARY: u32 = 0x430000;
const WRITE_ONLY: u32 = 0x440000;
const MISSING_TAIL: u32 = 0x450000;
const LAST_ERROR: u32 = 0x12345678;
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn copy_preserves_signed_coordinates_aliases_and_failed_outputs() {
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    machine
        .spawn("rectangle copy", PARK_ADDR, &[], 65536)
        .unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    let original = bytes([i32::MIN, 8, -7, i32::MAX]);
    machine.cpu.write(DATA, &original).unwrap();
    let mut env = Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    set_last_error(&mut env, LAST_ERROR);
    assert_eq!(
        copy(&mut env, GuestPtr::new(DATA + 4), GuestPtr::new(DATA)),
        1
    );
    let mut observed = [0; 16];
    env.cpu.read(DATA + 4, &mut observed).unwrap();
    assert_eq!(observed.as_slice(), original);
    assert_eq!(
        copy(&mut env, GuestPtr::new(DATA + 4), GuestPtr::new(DATA + 4)),
        1
    );
    assert_eq!(copy(&mut env, GuestPtr::new(DATA + 4), GuestPtr::new(0)), 0);
    assert_eq!(crate::util::last_error(&env), LAST_ERROR);
    assert_eq!(
        copy(&mut env, GuestPtr::new(READ_ONLY - 8), GuestPtr::new(DATA)),
        0
    );
    assert_eq!(crate::util::last_error(&env), 998);
    env.cpu.read(DATA + 4, &mut observed).unwrap();
    assert_eq!(observed.as_slice(), original);
}

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/inflate-rect-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert_eq!(
        registry.get("user32.dll", "InflateRect").unwrap().conv,
        adv_abi::Conv::Stdcall { argc: 3 }
    );
    assert!(registry.dynamic_available("user32.dll", "InflateRect"));
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(root.join("game"), "fixture.exe", root.join("data")),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}

fn bytes(rect: [i32; 4]) -> Vec<u8> {
    rect.into_iter().flat_map(i32::to_le_bytes).collect()
}
fn observe<B: Backend>(
    machine: &mut Machine<B>,
    code: &mut Asm,
    pointer: u32,
    dx: i32,
    dy: i32,
    record: u32,
) {
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let inflate = machine.resolve_import("user32.dll", "InflateRect").unwrap();
    code.push_imm(LAST_ERROR).call(set_error);
    code.push_imm(dy as u32)
        .push_imm(dx as u32)
        .push_imm(pointer)
        .call(inflate)
        .mov_mem_eax(record);
    code.call(get_error).mov_mem_eax(record + 4);
}
fn run<B: Backend>(machine: &mut Machine<B>, mut code: Asm) {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("InflateRect guest", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}

fn native_contract<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let mut code = Asm::new(CODE);
    for (index, (input, dx, dy, expected)) in [
        ([0, 0, 10, 10], -1, -1, [1, 1, 9, 9]),
        ([5, 7, 3, 7], -1, -2, [6, 9, 2, 5]),
        (
            [i32::MIN, i32::MAX, i32::MAX, i32::MIN],
            1,
            i32::MIN,
            [i32::MAX, -1, i32::MIN, 0],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let pointer = DATA + index as u32 * 16;
        machine.cpu.write(pointer, &bytes(input)).unwrap();
        observe(
            &mut machine,
            &mut code,
            pointer,
            dx,
            dy,
            DATA + 0x800 + index as u32 * 8,
        );
        machine
            .cpu
            .write(DATA + 0x100 + index as u32 * 16, &bytes(expected))
            .unwrap();
    }
    observe(&mut machine, &mut code, 0, 1, 1, DATA + 0x818);
    run(&mut machine, code);
    for index in 0..3u32 {
        let mut actual = [0; 16];
        let mut expected = [0; 16];
        machine.cpu.read(DATA + index * 16, &mut actual).unwrap();
        machine
            .cpu
            .read(DATA + 0x100 + index * 16, &mut expected)
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(machine.cpu.read_u32(DATA + 0x800 + index * 8).unwrap(), 1);
        assert_eq!(
            machine.cpu.read_u32(DATA + 0x804 + index * 8).unwrap(),
            LAST_ERROR
        );
    }
    assert_eq!(machine.cpu.read_u32(DATA + 0x818).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 0x81c).unwrap(), LAST_ERROR);
}
#[test]
fn native_shrink_inverted_zero_area_wrapping_and_null_contract_match_two_guest_backends() {
    native_contract(JitCpu::new(PARK_ADDR, false).unwrap());
    native_contract(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn guarded<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    machine.cpu.map(WRITE_ONLY, 4096, Prot::WRITE).unwrap();
    machine.cpu.map(BOUNDARY, 4096, Prot::RW).unwrap();
    machine.cpu.map(BOUNDARY + 4096, 4096, Prot::READ).unwrap();
    machine.cpu.map(MISSING_TAIL, 4096, Prot::RW).unwrap();
    let original = bytes([1, 2, 3, 4]);
    for pointer in [READ_ONLY, WRITE_ONLY, BOUNDARY + 4096 - 8] {
        machine.cpu.write(pointer, &original).unwrap();
    }
    machine
        .cpu
        .write(MISSING_TAIL + 4096 - 8, &original[..8])
        .unwrap();
    let mut code = Asm::new(CODE);
    let pointers = [
        READ_ONLY,
        WRITE_ONLY,
        BOUNDARY + 4096 - 8,
        MISSING_TAIL + 4096 - 8,
        0xdead0000,
        u32::MAX - 7,
    ];
    for (index, pointer) in pointers.into_iter().enumerate() {
        // Even zero deltas require the whole in/out READ|WRITE contract.
        observe(
            &mut machine,
            &mut code,
            pointer,
            0,
            0,
            DATA + index as u32 * 8,
        );
    }
    run(&mut machine, code);
    for index in 0..pointers.len() as u32 {
        assert_eq!(machine.cpu.read_u32(DATA + index * 8).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(DATA + index * 8 + 4).unwrap(), 998);
    }
    machine.cpu.protect(WRITE_ONLY, 4096, Prot::RW).unwrap();
    for pointer in [READ_ONLY, WRITE_ONLY, BOUNDARY + 4096 - 8] {
        let mut actual = [0; 16];
        machine.cpu.read(pointer, &mut actual).unwrap();
        assert_eq!(actual.as_slice(), original);
    }
    let mut prefix = [0; 8];
    machine
        .cpu
        .read(MISSING_TAIL + 4096 - 8, &mut prefix)
        .unwrap();
    assert_eq!(prefix, original[..8]);
}
#[test]
fn inaccessible_or_wrapped_rectangles_never_partially_modify_readable_coordinates() {
    guarded(JitCpu::new(PARK_ADDR, false).unwrap());
    guarded(UnicornCpu::new(PARK_ADDR).unwrap());
}
