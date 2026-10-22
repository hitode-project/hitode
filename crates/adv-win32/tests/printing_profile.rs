//! No printer device is invented; the actual local server has an empty catalog.
use adv_abi::{ApiDiagnostics, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
#[derive(Clone, Copy)]
enum Arg {
    I(u32),
    L(u32),
}
use Arg::{I, L};
fn call<B: Backend>(m: &mut Machine<B>, code: &mut Asm, name: &str, args: &[Arg], row: u32) {
    let target = m.resolve_import("winspool.drv", name).unwrap();
    let error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    for arg in args.iter().rev() {
        match arg {
            I(value) => {
                code.push_imm(*value);
            }
            L(address) => {
                code.mov_eax_mem(*address).push_eax();
            }
        }
    }
    code.call(target)
        .mov_mem_eax(row)
        .call(error)
        .mov_mem_eax(row + 4);
}
fn empty<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/printer-profile",
            "fixture.exe",
            "/nonexistent/printer-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    const SERVER: u32 = DATA + 0x800;
    const ALIAS: u32 = SERVER + 4;
    const NEEDED: u32 = DATA + 0x810;
    const RETURNED: u32 = NEEDED + 4;
    const BUFFER: u32 = DATA + 0x900;
    const NAME: u32 = DATA + 0xa00;
    const LOCAL: u32 = DATA + 0xa40;
    const DEFAULTS: u32 = DATA + 0xb00;
    m.cpu.write(NAME, b"absent-printer\0").unwrap();
    m.cpu.write(LOCAL, b"\\\\PAL-HLE\0").unwrap();
    m.cpu.write(BUFFER, &[0xcd; 128]).unwrap();
    m.cpu.write_u32(DEFAULTS + 8, 1).unwrap();
    let mut code = Asm::new(CODE);
    code.mov_mem_imm(NEEDED, 0xcdcdcdcd)
        .mov_mem_imm(RETURNED, 0xcdcdcdcd);
    call(
        &mut m,
        &mut code,
        "EnumPrintersA",
        &[I(6), I(0), I(4), I(0), I(0), I(NEEDED), I(RETURNED)],
        DATA,
    );
    code.mov_eax_mem(NEEDED)
        .mov_mem_eax(DATA + 8)
        .mov_eax_mem(RETURNED)
        .mov_mem_eax(DATA + 12);
    code.mov_mem_imm(NEEDED, 0xcdcdcdcd)
        .mov_mem_imm(RETURNED, 0xcdcdcdcd);
    call(
        &mut m,
        &mut code,
        "EnumPrintersA",
        &[I(2), I(0), I(3), I(BUFFER), I(128), I(NEEDED), I(RETURNED)],
        DATA + 16,
    );
    code.mov_eax_mem(NEEDED)
        .mov_mem_eax(DATA + 24)
        .mov_eax_mem(RETURNED)
        .mov_mem_eax(DATA + 28);
    call(
        &mut m,
        &mut code,
        "OpenPrinterA",
        &[I(0), I(SERVER), I(0)],
        DATA + 32,
    );
    call(
        &mut m,
        &mut code,
        "OpenPrinterA",
        &[I(LOCAL), I(ALIAS), I(0)],
        DATA + 40,
    );
    code.mov_mem_imm(DATA + 0x820, 0xcdcdcdcd);
    call(
        &mut m,
        &mut code,
        "OpenPrinterA",
        &[I(NAME), I(DATA + 0x820), I(0)],
        DATA + 48,
    );
    call(
        &mut m,
        &mut code,
        "OpenPrinterA",
        &[I(0), I(DATA + 0x820), I(DEFAULTS)],
        DATA + 56,
    );
    call(
        &mut m,
        &mut code,
        "DocumentPropertiesA",
        &[I(0), L(SERVER), I(0), I(BUFFER), I(BUFFER), I(2)],
        DATA + 64,
    );
    call(
        &mut m,
        &mut code,
        "DocumentPropertiesA",
        &[I(0), I(0), I(NAME), I(BUFFER), I(0), I(0)],
        DATA + 72,
    );
    call(&mut m, &mut code, "ClosePrinter", &[L(SERVER)], DATA + 80);
    call(&mut m, &mut code, "ClosePrinter", &[L(SERVER)], DATA + 88);
    call(&mut m, &mut code, "ClosePrinter", &[L(ALIAS)], DATA + 96);
    call(
        &mut m,
        &mut code,
        "ClosePrinter",
        &[I(u32::MAX)],
        DATA + 104,
    );
    code.mov_mem_imm(DATA + 112, 0)
        .mov_eax_mem(DATA + 112)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("no printers", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 12).unwrap(), 0);
    for (offset, result, error) in [
        (16, 0, Some(124)),
        (32, 1, None),
        (40, 1, None),
        (48, 0, Some(1801)),
        (56, 0, Some(5)),
        (64, u32::MAX, Some(6)),
        (72, u32::MAX, Some(1801)),
        (80, 1, None),
        (88, 0, Some(6)),
        (96, 1, None),
        (104, 0, Some(6)),
    ] {
        assert_eq!(
            m.cpu.read_u32(DATA + offset).unwrap(),
            result,
            "offset {offset}"
        );
        if let Some(error) = error {
            assert_eq!(
                m.cpu.read_u32(DATA + offset + 4).unwrap(),
                error,
                "offset {offset}"
            );
        }
    }
    assert_ne!(m.cpu.read_u32(SERVER).unwrap(), 0);
    assert_ne!(m.cpu.read_u32(ALIAS).unwrap(), 0);
    assert_ne!(
        m.cpu.read_u32(SERVER).unwrap(),
        m.cpu.read_u32(ALIAS).unwrap()
    );
    for at in [DATA + 24, DATA + 28] {
        assert_eq!(m.cpu.read_u32(at).unwrap(), 0xcdcdcdcd);
    }
    assert_eq!(m.cpu.read_u32(DATA + 0x820).unwrap(), 0);
    let mut buffer = [0; 128];
    m.cpu.read(BUFFER, &mut buffer).unwrap();
    assert_eq!(buffer, [0xcd; 128]);
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.acceptance_eligible(RunMode::Strict))
    );
}
#[test]
// spec: SYS-PRINT-1
fn empty_printer_catalog_and_local_server_lifetimes_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        empty(cpu);
    }
    empty(UnicornCpu::new(PARK_ADDR).unwrap());
}
