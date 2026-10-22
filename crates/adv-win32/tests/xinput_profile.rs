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
const DLLS: [&str; 4] = [
    "xinput1_1.dll",
    "xinput1_2.dll",
    "xinput9_1_0.dll",
    "xinput1_3.dll",
];

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
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    m.cpu.write(BUFFER, &[0xcd; 32]).unwrap();
    m.cpu.write(READ_ONLY, &[0x77; 32]).unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
    code.mov_mem_eax(DATA + 0xe00);
    let mut expected = Vec::new();
    for dll in DLLS {
        let mut names = vec!["XInputGetState", "XInputSetState", "XInputGetCapabilities"];
        if dll == "xinput1_3.dll" {
            names.extend(["#2", "#3"]);
        }
        for name in names {
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
        let mut extra = Vec::new();
        if dll != "xinput9_1_0.dll" {
            let function = m.resolve_import(dll, "XInputEnable").unwrap();
            code.push_imm(0).call(function).push_imm(1).call(function);
            if dll == "xinput1_3.dll" {
                assert_eq!(function, m.resolve_import(dll, "#5").unwrap());
            }
        }
        if dll == "xinput1_3.dll" {
            extra.extend([
                ("XInputGetBatteryInformation", vec![0, 0, BUFFER], 1167),
                ("XInputGetBatteryInformation", vec![3, 2, READ_ONLY], 1167),
                ("XInputGetBatteryInformation", vec![4, 0, BUFFER], 160),
                ("XInputGetBatteryInformation", vec![0, 0, 0], 160),
                ("XInputGetKeystroke", vec![255, 0, BUFFER], 1167),
                ("XInputGetKeystroke", vec![0, 0, UNMAPPED], 1167),
                ("XInputGetKeystroke", vec![4, 0, BUFFER], 160),
                ("XInputGetKeystroke", vec![0, 1, BUFFER], 160),
            ]);
        }
        for (name, args, result) in extra {
            let function = m.resolve_import(dll, name).unwrap();
            let address = DATA + expected.len() as u32 * 8;
            code.push_imm(MARKER).call(set_error);
            for arg in args.into_iter().rev() {
                code.push_imm(arg);
            }
            code.call(function).mov_mem_eax(address);
            code.call(get_error).mov_mem_eax(address + 4);
            expected.push(result);
        }
    }
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0xe04);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    assert!(code.bytes.len() <= 8192);
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("absent controllers", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        m.cpu.read_u32(DATA + 0xe00).unwrap(),
        m.cpu.read_u32(DATA + 0xe04).unwrap(),
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
// spec: WIN32-API-2
fn no_controller_queries_preserve_outputs_and_last_error_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        absent_controllers(cpu);
    }
    absent_controllers(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
// spec: WIN32-API-2
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
    // The xinput1_4 import library still supplies ABI/ordinal metadata, but the
    // DLL shipped with Windows 8 and must not advertise a Win7 implementation.
    for name in [
        "XInputGetState",
        "XInputSetState",
        "XInputGetCapabilities",
        "XInputEnable",
        "XInputGetBatteryInformation",
        "XInputGetKeystroke",
        "XInputGetAudioDeviceIds",
        "#2",
        "#3",
        "#4",
    ] {
        assert!(!registry.dynamic_available("xinput1_4.dll", name));
        assert!(!registry.is_implemented("xinput1_4.dll", name));
    }
    assert!(registry.dynamic_available("xinput1_3.dll", "XInputGetKeystroke"));
    for (ordinal, name) in [
        ("#2", "XInputGetState"),
        ("#3", "XInputSetState"),
        ("#5", "XInputEnable"),
    ] {
        assert_eq!(
            registry.canonical_name("xinput1_3.dll", ordinal),
            Some(name)
        );
        assert!(registry.dynamic_available("xinput1_3.dll", ordinal));
    }
    assert!(registry.signature_conflicts().is_empty());
}
