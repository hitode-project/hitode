//! Authored guest calls; no game assets or host UI.
use adv_abi::{ApiDiagnostics, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

fn call<B: Backend>(machine: &mut Machine<B>, code: &mut Asm, name: &str, args: &[u32], out: u32) {
    for arg in args.iter().rev() {
        code.push_imm(*arg);
    }
    code.call(machine.resolve_import("kernel32.dll", name).unwrap());
    if out != 0 {
        code.mov_mem_eax(out);
    }
}

fn writes<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry.set_mode(RunMode::Strict);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/console",
            "fixture.exe",
            "/nonexistent/console-data",
        ),
    )
    .unwrap();
    machine.ext.get_or_default::<ApiDiagnostics>();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let (ansi, wide) = (DATA + 0x100, DATA + 0x200);
    machine.cpu.write(ansi, b"PAL.dll").unwrap();
    let units: Vec<u8> = "PAL.dll"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(wide, &units).unwrap();
    machine.cpu.write(DATA + 8, &[0xa5; 8]).unwrap();
    let mut code = Asm::new(CODE);
    call(
        &mut machine,
        &mut code,
        "GetStdHandle",
        &[-11i32 as u32],
        DATA,
    );
    code.push_imm(0)
        .push_imm(DATA + 8)
        .push_imm(7)
        .push_imm(ansi)
        .mov_eax_mem(DATA)
        .push_eax();
    code.call(
        machine
            .resolve_import("kernel32.dll", "WriteConsoleA")
            .unwrap(),
    );
    code.mov_mem_eax(DATA + 16);
    code.push_imm(0)
        .push_imm(DATA + 12)
        .push_imm(7)
        .push_imm(wide)
        .mov_eax_mem(DATA)
        .push_eax();
    code.call(
        machine
            .resolve_import("kernel32.dll", "WriteConsoleW")
            .unwrap(),
    );
    code.mov_mem_eax(DATA + 20);
    call(&mut machine, &mut code, "GetLastError", &[], DATA + 24);
    call(&mut machine, &mut code, "ExitProcess", &[0], 0);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("console fixture", CODE, &[], 65536).unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (out, value) in [
        (DATA + 8, 0),
        (DATA + 12, 0),
        (DATA + 16, 0),
        (DATA + 20, 0),
        (DATA + 24, 6),
    ] {
        assert_eq!(machine.cpu.read_u32(out).unwrap(), value, "{out:#x}");
    }
    assert_eq!(machine.ext.get::<ApiDiagnostics>().stub_calls, 0);
    assert!(
        machine
            .ext
            .get::<ApiDiagnostics>()
            .acceptance_eligible(RunMode::Strict)
    );
}

#[test]
// spec: PERS-CONSOLE-1
fn ansi_and_wide_console_writes_fail_without_an_attached_console() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        writes(cpu);
    }
    writes(adv_unicorn::UnicornCpu::new(PARK_ADDR).unwrap());
}
