use super::*;
use crate::{Win32, Win32Config};
use adv_abi::{ApiDiagnostics, Registry, RunMode, signature::AbiKind};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const BUFFER: u32 = DATA + 0x1000;
const COUNT: u32 = DATA + 0x1100;
const READ_ONLY: u32 = 0x430000;
const MARKER: u32 = 0x12345678;
fn profile<B: Backend>(cpu: B, allocation: bool) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(".", "fixture.exe", "target/console-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    m.cpu.write(READ_ONLY, &[0x5a; 4]).unwrap();
    m.cpu.write(BUFFER, &[0xa5; 512]).unwrap();
    let set_error = m.resolve_import(K32, "SetLastError").unwrap();
    let get_error = m.resolve_import(K32, "GetLastError").unwrap();
    let pid = m.sys.pid;
    let mut code = Asm::new(CODE);
    let mut expected = Vec::new();
    let mut call = |name: &str, args: &[u32], value, error| {
        code.push_imm(MARKER).call(set_error);
        for arg in args.iter().rev() {
            code.push_imm(*arg);
        }
        let function = m.resolve_import(K32, name).unwrap();
        let row = DATA + expected.len() as u32 * 8;
        code.call(function)
            .mov_mem_eax(row)
            .call(get_error)
            .mov_mem_eax(row + 4);
        expected.push((name.to_owned(), value, error));
    };
    for &name in ABSENT {
        let declaration = signature::lookup(K32, name).unwrap();
        assert!(declaration.callable(), "{name}");
        let args = declaration
            .parameters
            .iter()
            .map(|parameter| match parameter.kind {
                AbiKind::Pointer | AbiKind::AnsiString | AbiKind::WideString => BUFFER,
                AbiKind::Handle => 0x1234,
                _ => 1,
            })
            .collect::<Vec<_>>();
        call(name, &args, 0, 6);
    }
    for name in [
        "ReadConsoleA",
        "ReadConsoleW",
        "WriteConsoleA",
        "WriteConsoleW",
    ] {
        call(name, &[0x1234, BUFFER, 1, COUNT, 0], 0, 6);
        call(name, &[0x1234, BUFFER, 1, READ_ONLY, 0], 0, 6);
    }
    call(
        "CreateConsoleScreenBuffer",
        &[0xc0000000, 3, 0, 1, 0],
        u32::MAX,
        6,
    );
    call("GetConsoleWindow", &[], 0, MARKER);
    call("AttachConsole", &[pid], 0, 6);
    call("AttachConsole", &[0], 0, 87);
    call("GenerateConsoleCtrlEvent", &[0, 0], 0, 6);
    call("GenerateConsoleCtrlEvent", &[2, 0], 0, 87);
    call("SetConsoleCtrlHandler", &[CODE + 0x3f00, 1], 1, MARKER);
    call("SetConsoleCtrlHandler", &[CODE + 0x3f00, 1], 1, MARKER);
    call("SetConsoleCtrlHandler", &[CODE + 0x3f00, 0], 1, MARKER);
    call("SetConsoleCtrlHandler", &[CODE + 0x3f00, 0], 1, MARKER);
    call("SetConsoleCtrlHandler", &[CODE + 0x3f00, 0], 0, 87);
    call("SetConsoleCtrlHandler", &[CODE + 0x3f00, 1], 1, MARKER);
    call("SetConsoleCtrlHandler", &[0, 1], 1, MARKER);
    call("FreeConsole", &[], 1, MARKER);
    call("SetConsoleCtrlHandler", &[CODE + 0x3f00, 0], 0, 87);
    if allocation {
        call("AllocConsole", &[], 0, 50);
    }
    code.mov_mem_imm(DATA + 0xe00, 0)
        .mov_eax_mem(DATA + 0xe00)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("detached console", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (i, (name, value, error)) in expected.into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + i as u32 * 8).unwrap(),
            value,
            "{name}"
        );
        assert_eq!(
            m.cpu.read_u32(DATA + i as u32 * 8 + 4).unwrap(),
            error,
            "{name} error"
        );
    }
    let mut actual = [0; 512];
    m.cpu.read(BUFFER, &mut actual).unwrap();
    let mut expected = [0xa5; 512];
    expected[256..260].fill(0);
    assert_eq!(actual, expected);
    assert_eq!(m.cpu.read_u32(READ_ONLY).unwrap(), 0x5a5a5a5a);
    let control = m.ext.get::<Control>();
    assert!(control.handlers.is_empty());
    assert!(!control.ignore_ctrl_c);
    let diagnostics = m.ext.get_or_default::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.acceptance_eligible(RunMode::Strict));
    assert_eq!(
        diagnostics.warnings.values().sum::<u64>(),
        u64::from(allocation)
    );
}
#[test]
// spec: SYS-CONSOLE-1, SYS-CONSOLE-2
fn detached_console_queries_and_control_state_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        profile(cpu, false);
    }
    profile(UnicornCpu::new(PARK_ADDR).unwrap(), false);
}
#[test]
// spec: SYS-CONSOLE-3
fn console_allocation_reports_the_missing_transport() {
    profile(JitCpu::new(PARK_ADDR, false).unwrap(), true);
}
