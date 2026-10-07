//! Native-shaped free-gap queries use the existing VM and executor owners.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PAGE, PARK_ADDR, THUNK_BASE},
};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const GAP: u32 = 0x0080_0000;
const NEXT: u32 = 0x0089_0000;
const OWNER: u32 = 0x008b_0000;
const READONLY: u32 = 0x0091_0000;
const SPLIT: u32 = 0x0092_0000;
const NOACCESS: u32 = 0x0094_0000;
const WRITEONLY: u32 = 0x0095_0000;
const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_FREE: u32 = 0x1_0000;

fn gaps<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/query-gaps",
            "fixture.exe",
            "/nonexistent/query-gaps-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, PAGE, Prot::RX).unwrap();
    machine.cpu.map(DATA, PAGE, Prot::RW).unwrap();
    // This direct backend mapping is intentionally outside Vm.used. Even a
    // PAGE_NOACCESS mapping must terminate a preceding free region.
    machine.cpu.map(NEXT, PAGE, Prot::default()).unwrap();
    assert!(machine.sys.vm.reserve_at(OWNER, 0x3_0000));
    machine
        .cpu
        .map(OWNER + 2 * PAGE, PAGE, Prot::default())
        .unwrap();
    for (address, protection) in [
        (READONLY, Prot::READ),
        (SPLIT, Prot::RW),
        (NOACCESS, Prot::default()),
        (WRITEONLY, Prot::WRITE),
    ] {
        machine.cpu.map(address, PAGE, protection).unwrap();
    }
    let query = machine
        .resolve_import("kernel32.dll", "VirtualQuery")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let cases = [
        (GAP + 0x111, GAP, NEXT - GAP, MEM_FREE, None),
        (
            GAP + 0x32123,
            GAP + 0x32000,
            NEXT - GAP - 0x32000,
            MEM_FREE,
            None,
        ),
        (NEXT + 0x123, NEXT, PAGE, MEM_COMMIT, Some(NEXT)),
        (
            NEXT + PAGE + 0x444,
            NEXT + PAGE,
            OWNER - NEXT - PAGE,
            MEM_FREE,
            None,
        ),
        (
            OWNER + PAGE + 0x123,
            OWNER + PAGE,
            PAGE,
            MEM_RESERVE,
            Some(OWNER),
        ),
        (
            OWNER + 2 * PAGE + 0x3ff,
            OWNER + 2 * PAGE,
            PAGE,
            MEM_COMMIT,
            Some(OWNER),
        ),
        (
            OWNER + 3 * PAGE + 0x222,
            OWNER + 3 * PAGE,
            0x3_0000 - 3 * PAGE,
            MEM_RESERVE,
            Some(OWNER),
        ),
        (
            THUNK_BASE - 2 * PAGE + 0x421,
            THUNK_BASE - 2 * PAGE,
            2 * PAGE,
            MEM_FREE,
            None,
        ),
        (
            0x7ffe_f321,
            0x7ffe_f000,
            PAGE,
            MEM_RESERVE,
            Some(0x7ff0_0000),
        ),
    ];
    let mut code = Asm::new(CODE);
    for (index, &(address, _, _, _, _)) in cases.iter().enumerate() {
        let output = DATA + 0x100 + index as u32 * 64 + 4;
        machine.cpu.write(output - 4, &[0xa5; 36]).unwrap();
        code.push_imm(64)
            .push_imm(output)
            .push_imm(address)
            .call(query)
            .mov_mem_eax(DATA + index as u32 * 4);
    }
    let failures = [READONLY, NOACCESS, SPLIT + PAGE - 16];
    for (index, &output) in failures.iter().enumerate() {
        let bytes = if index == 2 { 16 } else { 28 };
        machine.cpu.write(output, &vec![0x5a; bytes]).unwrap();
        code.push_imm(28)
            .push_imm(output)
            .push_imm(GAP + 0x32123)
            .call(query)
            .mov_mem_eax(DATA + 0x40 + index as u32 * 8);
        code.call(get_error)
            .mov_mem_eax(DATA + 0x44 + index as u32 * 8);
    }
    code.push_imm(28)
        .push_imm(WRITEONLY)
        .push_imm(GAP + 0x32123)
        .call(query)
        .mov_mem_eax(DATA + 0x60);
    machine.cpu.write(DATA + 0x700, &[0xa5; 28]).unwrap();
    code.push_imm(28)
        .push_imm(DATA + 0x700)
        .push_imm(0x7fff_0000)
        .call(query)
        .mov_mem_eax(DATA + 0x64);
    code.call(get_error).mov_mem_eax(DATA + 0x68);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("VirtualQuery free gaps", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (index, &(_, base, size, state, owner)) in cases.iter().enumerate() {
        let output = DATA + 0x100 + index as u32 * 64 + 4;
        assert_eq!(machine.cpu.read_u32(DATA + index as u32 * 4).unwrap(), 28);
        let mut bytes = [0; 36];
        machine.cpu.read(output - 4, &mut bytes).unwrap();
        assert_eq!(&bytes[..4], &[0xa5; 4]);
        assert_eq!(&bytes[32..], &[0xa5; 4]);
        assert_eq!(&bytes[4..8], &base.to_le_bytes());
        assert_eq!(&bytes[16..20], &size.to_le_bytes());
        assert_eq!(&bytes[20..24], &state.to_le_bytes());
        if let Some(owner) = owner {
            assert_eq!(&bytes[8..12], &owner.to_le_bytes());
        }
        if state == MEM_COMMIT {
            assert_eq!(machine.cpu.read_u32(output + 20).unwrap(), 1);
        }
        // AllocationBase/AllocationProtect/Protect/Type of MEM_FREE are
        // undefined in native Win32 and deliberately carry no assertion here.
    }
    for (index, &output) in failures.iter().enumerate() {
        assert_eq!(
            machine
                .cpu
                .read_u32(DATA + 0x40 + index as u32 * 8)
                .unwrap(),
            0
        );
        assert_eq!(
            machine
                .cpu
                .read_u32(DATA + 0x44 + index as u32 * 8)
                .unwrap(),
            998
        );
        let mut bytes = vec![0; if index == 2 { 16 } else { 28 }];
        machine.cpu.read(output, &mut bytes).unwrap();
        assert!(bytes.iter().all(|&byte| byte == 0x5a));
    }
    assert_eq!(machine.cpu.read_u32(DATA + 0x60).unwrap(), 28);
    assert_eq!(machine.cpu.read_u32(WRITEONLY).unwrap(), GAP + 0x32000);
    assert_eq!(machine.cpu.read_u32(DATA + 0x64).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 0x68).unwrap(), 87);
    let mut bytes = [0; 28];
    machine.cpu.read(DATA + 0x700, &mut bytes).unwrap();
    assert_eq!(bytes, [0xa5; 28]);
}

#[test]
fn free_gaps_stop_at_actual_mappings_reservations_and_the_application_limit() {
    gaps(JitCpu::new(PARK_ADDR, false).unwrap());
    gaps(JitCpu::new(PARK_ADDR, true).unwrap());
    gaps(UnicornCpu::new(PARK_ADDR).unwrap());
}
