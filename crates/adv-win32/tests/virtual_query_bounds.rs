//! The Win32 profile's terminal address must stop an authored VirtualQuery walk.
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

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut reg = Registry::default();
    adv_win32::register(&mut reg);
    let mut machine = Machine::new(cpu, reg, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/query",
            "fixture.exe",
            "/nonexistent/query-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    // A regressed nonterminating scan fails within a few calls, without a CPU
    // timing dependency or a large trace.
    machine.sys.call_limit = Some(24);
    machine
}

fn boundaries<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let info = machine
        .resolve_import("kernel32.dll", "GetSystemInfo")
        .unwrap();
    let query = machine
        .resolve_import("kernel32.dll", "VirtualQuery")
        .unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let addresses = [0x7ffe_ffff, 0x7fff_0000, 0x8000_0000, 0xffff_0000, u32::MAX];
    let mut code = Asm::new(CODE);
    code.push_imm(DATA).call(info);
    for (i, address) in addresses.into_iter().enumerate() {
        let output = DATA + 256 + i as u32 * 64;
        machine.cpu.write(output, &[0xa5; 28]).unwrap();
        code.push_imm(0x1234).call(set_error);
        code.push_imm(28)
            .push_imm(output)
            .push_imm(address)
            .call(query)
            .mov_mem_eax(output + 32);
        code.call(get_error).mov_mem_eax(output + 36);
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("VirtualQuery terminal address", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(machine.cpu.read_u32(DATA + 12).unwrap(), addresses[0]);
    for (i, _) in addresses.into_iter().enumerate() {
        let output = DATA + 256 + i as u32 * 64;
        assert_eq!(
            machine.cpu.read_u32(output + 32).unwrap(),
            if i == 0 { 28 } else { 0 }
        );
        assert_eq!(
            machine.cpu.read_u32(output + 36).unwrap(),
            if i == 0 { 0x1234 } else { 87 }
        );
        if i == 0 {
            assert_eq!(machine.cpu.read_u32(output).unwrap(), 0x7ffe_f000);
            assert_eq!(machine.cpu.read_u32(output + 12).unwrap(), 4096);
        } else {
            // Our preflight rejects the domain before touching the output.
            // Native error outputs are undefined, so only 0/87 is differential.
            let mut bytes = [0; 28];
            machine.cpu.read(output, &mut bytes).unwrap();
            assert_eq!(bytes, [0xa5; 28]);
        }
    }
}

#[test]
fn virtual_query_and_system_info_share_the_same_terminal_address() {
    for jit in [false, true] {
        boundaries(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    boundaries(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn walk<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let query = machine
        .resolve_import("kernel32.dll", "VirtualQuery")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    let again = code.here();
    code.bytes.extend_from_slice(&[0xff, 0x05]); // inc dword [call count]
    code.bytes.extend_from_slice(&(DATA + 64).to_le_bytes());
    code.push_imm(28)
        .push_imm(DATA)
        .push_imm(0xffff_0000)
        .call(query);
    code.bytes.extend_from_slice(&[0x85, 0xc0]); // test eax,eax
    code.jnz(again);
    code.call(get_error).mov_mem_eax(DATA + 68);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("bounded address scan", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(machine.cpu.read_u32(DATA + 64).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 68).unwrap(), 87);
}

#[test]
fn repeated_top_address_query_stops_after_one_call() {
    for jit in [false, true] {
        walk(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    walk(UnicornCpu::new(PARK_ADDR).unwrap());
}
