//! Authored calls for the same absent-controller profile as DirectInput/WinMM.
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
const BUFFER: u32 = DATA + 0x800;
const MARKER: u32 = 0x12345678;
const DLLS: [&str; 3] = ["xinput9_1_0.dll", "xinput1_3.dll", "xinput1_4.dll"];

fn registry() -> Registry {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry
}

fn absent_controllers<B: Backend>(cpu: B) {
    let mut m = Machine::new(cpu, registry(), Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/xinput",
            "fixture.exe",
            "/nonexistent/xinput-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    m.cpu.write(BUFFER, &[0xcd; 32]).unwrap();
    m.cpu.write(READ_ONLY, &[0x77; 32]).unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
    code.mov_mem_eax(DATA + 0x700);
    let mut expected = Vec::new();
    for dll in DLLS {
        for name in ["XInputGetState", "XInputSetState", "XInputGetCapabilities"] {
            let function = m.resolve_import(dll, name).unwrap();
            let capabilities = name == "XInputGetCapabilities";
            let mut cases = vec![
                (0, 0, BUFFER + 4, 1167),
                (3, 1, READ_ONLY + 4, 1167),
                (4, 0, BUFFER + 4, 160),
                (u32::MAX, 0, UNMAPPED, 160),
                (0, 0, 0, 160),
                (1, 1, UNMAPPED, 1167),
                (2, 0, 1, 1167),
                (2, 0, u32::MAX, 1167),
            ];
            if capabilities {
                cases.extend([(0, 2, BUFFER + 4, 160), (0, u32::MAX, UNMAPPED, 160)]);
            }
            for (index, flags, pointer, result) in cases {
                let address = DATA + expected.len() as u32 * 8;
                code.push_imm(MARKER).call(set_error);
                code.push_imm(pointer);
                if capabilities {
                    code.push_imm(flags);
                }
                code.push_imm(index).call(function).mov_mem_eax(address);
                code.call(get_error).mov_mem_eax(address + 4);
                expected.push(result);
            }
        }
    }
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x704);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    assert!(code.bytes.len() <= 4096);
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("absent controllers", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        m.cpu.read_u32(DATA + 0x700).unwrap(),
        m.cpu.read_u32(DATA + 0x704).unwrap(),
        "typed x86 stdcall adapters balance the stack"
    );
    for (index, result) in expected.into_iter().enumerate() {
        assert_eq!(m.cpu.read_u32(DATA + index as u32 * 8).unwrap(), result);
        assert_eq!(m.cpu.read_u32(DATA + index as u32 * 8 + 4).unwrap(), MARKER);
    }
    let mut actual = [0; 32];
    m.cpu.read(BUFFER, &mut actual).unwrap();
    assert_eq!(
        actual, [0xcd; 32],
        "failed queries preserve every output byte"
    );
    m.cpu.read(READ_ONLY, &mut actual).unwrap();
    assert_eq!(actual, [0x77; 32]);
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|diag| diag.acceptance_eligible(RunMode::Strict))
    );
}

#[test]
fn no_controller_queries_preserve_outputs_and_last_error_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        absent_controllers(cpu);
    }
    absent_controllers(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn selected_legacy_versions_are_typed_and_do_not_advertise_attached_devices() {
    let registry = registry();
    for dll in DLLS {
        for (name, argc) in [
            ("XInputGetState", 2),
            ("XInputSetState", 2),
            ("XInputGetCapabilities", 3),
        ] {
            assert_eq!(
                registry.level(dll, name),
                Some(ImplementationLevel::SafeFail)
            );
            assert_eq!(
                registry.get(dll, name).unwrap().conv,
                Conv::Stdcall { argc }
            );
            assert!(registry.dynamic_available(dll, name));
        }
    }
    for (ordinal, name) in [
        ("#2", "XInputGetState"),
        ("#3", "XInputSetState"),
        ("#4", "XInputGetCapabilities"),
    ] {
        assert_eq!(
            registry.canonical_name("xinput1_4.dll", ordinal),
            Some(name)
        );
        assert!(registry.dynamic_available("xinput1_4.dll", ordinal));
    }
    assert!(!registry.dynamic_available("xinput1_3.dll", "XInputGetKeystroke"));
    assert!(registry.signature_conflicts().is_empty());
}
