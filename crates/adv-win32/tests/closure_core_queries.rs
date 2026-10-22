//! Authored Win7 feature probes through the guest ABI and shared virtual clock.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

fn queries<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/core-queries",
            "fixture.exe",
            "/nonexistent/core-query-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
        .cpu
        .write(DATA + 512, &[b'a', 0, b'b', 0, 0, 0])
        .unwrap();
    machine.cpu.write(DATA + 4094, &[b'a', 0]).unwrap();
    machine.cpu.write(DATA + 1024, &[0xcd; 128]).unwrap();
    let mut code = Asm::new(CODE);
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    code.push_imm(0x12345678).call(set_error);
    let tick64 = machine
        .resolve_import("kernel32.dll", "GetTickCount64")
        .unwrap();
    code.call(tick64).mov_mem_eax(DATA);
    code.bytes.extend_from_slice(&[0x89, 0x15]); // mov [DATA+4], edx
    code.bytes.extend_from_slice(&(DATA + 4).to_le_bytes());
    code.call(
        machine
            .resolve_import("kernel32.dll", "GetTickCount")
            .unwrap(),
    )
    .mov_mem_eax(DATA + 8);
    let probe = machine
        .resolve_import("kernel32.dll", "IsBadStringPtrW")
        .unwrap();
    let cases = [
        (0, 0, 0),
        (0, 1, 1),
        (DATA + 512, 1, 0),
        (DATA + 512, 10, 0),
        (DATA + 4094, 1, 0),
        (DATA + 4094, 2, 1),
        (u32::MAX, u32::MAX, 1),
    ];
    for (index, (pointer, maximum, _)) in cases.into_iter().enumerate() {
        code.push_imm(maximum)
            .push_imm(pointer)
            .call(probe)
            .mov_mem_eax(DATA + 32 + index as u32 * 4);
    }
    code.call(
        machine
            .resolve_import("kernel32.dll", "FlushProcessWriteBuffers")
            .unwrap(),
    );
    code.call(get_error).mov_mem_eax(DATA + 12);
    let wow = machine
        .resolve_import("kernel32.dll", "GetSystemWow64DirectoryA")
        .unwrap();
    code.push_imm(128)
        .push_imm(DATA + 1024)
        .call(wow)
        .mov_mem_eax(DATA + 64);
    code.call(get_error).mov_mem_eax(DATA + 68);
    let topology = machine
        .resolve_import("kernel32.dll", "GetLogicalProcessorInformation")
        .unwrap();
    code.mov_mem_imm(DATA + 80, 0)
        .push_imm(DATA + 80)
        .push_imm(0)
        .call(topology)
        .mov_mem_eax(DATA + 84)
        .call(get_error)
        .mov_mem_eax(DATA + 88)
        .push_imm(DATA + 80)
        .push_imm(DATA + 1200)
        .call(topology)
        .mov_mem_eax(DATA + 92);
    code.mov_mem_imm(DATA + 96, 0).mov_eax_mem(DATA + 96).ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("core queries", CODE, &[], 65536).unwrap();
    let uptime = u64::from(u32::MAX) + 1234;
    machine.sys.clock.advance_to(uptime * 1000);
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(0)
    ));
    let tick = u64::from(machine.cpu.read_u32(DATA).unwrap())
        | u64::from(machine.cpu.read_u32(DATA + 4).unwrap()) << 32;
    assert_eq!(tick, uptime);
    assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), uptime as u32);
    assert_eq!(machine.cpu.read_u32(DATA + 12).unwrap(), 0x12345678);
    for (index, (_, _, expected)) in cases.into_iter().enumerate() {
        assert_eq!(
            machine.cpu.read_u32(DATA + 32 + index as u32 * 4).unwrap(),
            expected
        );
    }
    assert_eq!(machine.cpu.read_u32(DATA + 64).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 68).unwrap(), 120);
    let mut preserved = [0; 128];
    machine.cpu.read(DATA + 1024, &mut preserved).unwrap();
    assert_eq!(preserved, [0xcd; 128]);
    assert_eq!(machine.cpu.read_u32(DATA + 80).unwrap(), 72);
    assert_eq!(machine.cpu.read_u32(DATA + 84).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 88).unwrap(), 122);
    assert_eq!(machine.cpu.read_u32(DATA + 92).unwrap(), 1);
    for (index, relation) in [0, 1, 3].into_iter().enumerate() {
        let record = DATA + 1200 + index as u32 * 24;
        assert_eq!(machine.cpu.read_u32(record).unwrap(), 1);
        assert_eq!(machine.cpu.read_u32(record + 4).unwrap(), relation);
        let mut payload = [1; 16];
        machine.cpu.read(record + 8, &mut payload).unwrap();
        assert_eq!(payload, [0; 16]);
    }
}

#[test]
// spec: SYS-CORE-1
fn shared_clock_memory_probes_and_x86_topology_run_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        queries(cpu);
    }
    queries(UnicornCpu::new(PARK_ADDR).unwrap());
}
