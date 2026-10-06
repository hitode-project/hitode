//! Status-position calls cannot create an IME context or invent coordinates.
use adv_abi::{ApiDiagnostics, Conv, ImplementationLevel, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;
const UNMAPPED: u32 = 0x600000;
const POINT: u32 = DATA + 0x800;
const MARKER: u32 = 0x12345678;

fn registry() -> Registry {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry
}
fn no_context<B: Backend>(cpu: B) {
    let mut m = Machine::new(cpu, registry(), Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new("/nonexistent/ime", "fixture.exe", "/nonexistent/ime-data"),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    m.cpu.write(POINT, &[0xcd; 16]).unwrap();
    m.cpu.write(READ_ONLY, &[0x77; 16]).unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let get = m
        .resolve_import("imm32.dll", "ImmGetStatusWindowPos")
        .unwrap();
    let set = m
        .resolve_import("imm32.dll", "ImmSetStatusWindowPos")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0)
        .call(m.resolve_import("imm32.dll", "ImmGetContext").unwrap())
        .mov_mem_eax(DATA + 0x700);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
    code.mov_mem_eax(DATA + 0x704);
    let mut expected = Vec::new();
    for context in [0, 0x1234, 0x10001] {
        for pointer in [POINT + 4, READ_ONLY + 4, UNMAPPED, 0, u32::MAX - 3] {
            for (function, error) in [(get, if context == 0 { MARKER } else { 6 }), (set, 6)] {
                let result = DATA + expected.len() as u32 * 8;
                code.push_imm(MARKER).call(set_error);
                code.push_imm(pointer)
                    .push_imm(context)
                    .call(function)
                    .mov_mem_eax(result);
                code.call(get_error).mov_mem_eax(result + 4);
                expected.push(error);
            }
        }
    }
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
    code.mov_mem_eax(DATA + 0x708);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("no IME", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        m.cpu.read_u32(DATA + 0x700).unwrap(),
        0,
        "no fabricated HIMC"
    );
    assert_eq!(
        m.cpu.read_u32(DATA + 0x704).unwrap(),
        m.cpu.read_u32(DATA + 0x708).unwrap()
    );
    for (index, error) in expected.into_iter().enumerate() {
        assert_eq!(m.cpu.read_u32(DATA + index as u32 * 8).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(DATA + index as u32 * 8 + 4).unwrap(), error);
    }
    let mut actual = [0; 16];
    m.cpu.read(POINT, &mut actual).unwrap();
    assert_eq!(actual, [0xcd; 16]);
    m.cpu.read(READ_ONLY, &mut actual).unwrap();
    assert_eq!(actual, [0x77; 16]);
    assert_eq!(m.sys.contexts.len(), 1);
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|diag| diag.acceptance_eligible(RunMode::Strict))
    );
}

#[test]
fn absent_ime_contexts_fail_without_reading_or_writing_points_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        no_context(cpu);
    }
    no_context(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn status_exports_are_typed_safe_failures_and_dynamic_capabilities() {
    let registry = registry();
    for (name, argc, level) in [
        ("ImmGetContext", 1, ImplementationLevel::State),
        ("ImmGetStatusWindowPos", 2, ImplementationLevel::SafeFail),
        ("ImmSetStatusWindowPos", 2, ImplementationLevel::SafeFail),
    ] {
        assert_eq!(registry.level("imm32.dll", name), Some(level));
        assert_eq!(
            registry.get("imm32.dll", name).unwrap().conv,
            Conv::Stdcall { argc }
        );
        assert!(registry.dynamic_available("imm32.dll", name));
    }
    assert!(registry.signature_conflicts().is_empty());
}
