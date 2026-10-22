//! The no-printer profile rejects spool operations without consuming GDI objects.
use adv_abi::{ApiDiagnostics, Registry};
use adv_cpu::{
    JitCpu,
    backend::{GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
#[path = "support/test_cpu.rs"]
mod test_cpu;
use test_cpu::TestCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
fn empty<B: TestCpu>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/gdi-printing-tests");
    Win32::install(&mut m, Win32Config::new(&root, "fixture.exe", &root)).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.write(DATA + 512, b"WINSPOOL\0").unwrap();
    m.cpu.write(DATA + 544, b"absent-printer\0").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0)
        .call(m.resolve_import("gdi32.dll", "CreateCompatibleDC").unwrap())
        .mov_mem_eax(DATA);
    for arg in [0, 0, DATA + 544, DATA + 512] {
        code.push_imm(arg);
    }
    code.call(m.resolve_import("gdi32.dll", "CreateDCA").unwrap())
        .mov_mem_eax(DATA + 4)
        .call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
        .mov_mem_eax(DATA + 8);
    let mut row = DATA + 32;
    for (name, extra) in [
        ("StartDocA", true),
        ("StartPage", false),
        ("EndPage", false),
        ("EndDoc", false),
        ("SetAbortProc", true),
    ] {
        for handle in [None, Some(0), Some(0xdeadbeef)] {
            if extra {
                code.push_imm(0xdead0000);
            }
            if let Some(handle) = handle {
                code.push_imm(handle);
            } else {
                code.mov_eax_mem(DATA).push_eax();
            }
            code.call(m.resolve_import("gdi32.dll", name).unwrap())
                .mov_mem_eax(row)
                .call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
                .mov_mem_eax(row + 4);
            row += 8;
        }
    }
    code.push_imm(12)
        .mov_eax_mem(DATA)
        .push_eax()
        .call(m.resolve_import("gdi32.dll", "GetDeviceCaps").unwrap())
        .mov_mem_eax(DATA + 160);
    code.mov_eax_mem(DATA)
        .push_eax()
        .call(m.resolve_import("gdi32.dll", "DeleteDC").unwrap())
        .mov_mem_eax(DATA + 164)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.cpu.code_region(CODE, code.bytes.len());
    m.spawn("no printer DC", CODE, &[], 65536).unwrap();
    let outcome = m.run_virtual(1_000_000);
    assert!(matches!(outcome, RunResult::Exited(_)), "{outcome:?}");
    assert_ne!(m.cpu.read_u32(DATA).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 1801);
    for at in (DATA + 32..row).step_by(8) {
        assert_eq!(m.cpu.read_u32(at).unwrap(), u32::MAX);
        assert_eq!(m.cpu.read_u32(at + 4).unwrap(), 6);
    }
    assert_eq!(m.cpu.read_u32(DATA + 160).unwrap(), 32);
    assert_eq!(m.cpu.read_u32(DATA + 164).unwrap(), 1);
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}
#[test]
// spec: SYS-PRINT-2
fn printer_dc_creation_and_job_calls_fail_without_spooling_or_warnings() {
    empty(JitCpu::new(PARK_ADDR, false).unwrap());
    empty(JitCpu::new(PARK_ADDR, true).unwrap());
    empty(UnicornCpu::new(PARK_ADDR).unwrap());
}
