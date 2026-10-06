//! Authored WinMM calls against the virtual no-controller profile. No capture,
//! host device polling, window, audio, PE loader or game data is involved.
use adv_abi::{ApiDiagnostics, ImplementationLevel, Registry, RunMode};
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
const OUTPUT: u32 = DATA + 0x800;
const MARKER: u32 = 0x12345678;

fn registry() -> Registry {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry
}

fn no_controllers<B: Backend>(cpu: B) {
    let mut machine = Machine::new(cpu, registry(), Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/joystick",
            "fixture.exe",
            "/nonexistent/joystick-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    machine.cpu.write(OUTPUT, &[0xcd; 24]).unwrap();
    machine.cpu.write(READ_ONLY, &[0x77; 24]).unwrap();
    let query = machine.resolve_import("winmm.dll", "joyGetPos").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let cases: Vec<_> = (0..=16)
        .chain([u32::MAX])
        .flat_map(|id| [OUTPUT + 4, READ_ONLY + 4, UNMAPPED, 0].map(|pointer| (id, pointer)))
        .collect();
    let mut code = Asm::new(CODE);
    for (i, &(id, pointer)) in cases.iter().enumerate() {
        let record = DATA + i as u32 * 8;
        code.push_imm(MARKER).call(set_error);
        code.push_imm(pointer)
            .push_imm(id)
            .call(query)
            .mov_mem_eax(record);
        code.call(get_error).mov_mem_eax(record + 4);
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("no controllers", CODE, &[], 65536).unwrap();
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(0)
    ));
    for (i, &(id, pointer)) in cases.iter().enumerate() {
        let record = DATA + i as u32 * 8;
        assert_eq!(
            machine.cpu.read_u32(record).unwrap(),
            if pointer == 0 { 11 } else { 165 },
            "id={id:#x} output={pointer:#x}"
        );
        assert_eq!(machine.cpu.read_u32(record + 4).unwrap(), MARKER);
    }
    let mut bytes = [0; 24];
    machine.cpu.read(OUTPUT, &mut bytes).unwrap();
    assert_eq!(
        bytes, [0xcd; 24],
        "JOYINFO and its guards must remain unchanged"
    );
    machine.cpu.read(READ_ONLY, &mut bytes).unwrap();
    assert_eq!(bytes, [0x77; 24]);
    let diagnostics = machine.ext.try_get::<ApiDiagnostics>();
    assert!(diagnostics.is_none_or(|value| value.acceptance_eligible(RunMode::Strict)));
}

#[test]
fn no_controller_queries_preserve_outputs_and_last_error_in_strict_mode() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        no_controllers(cpu);
    }
    no_controllers(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn joystick_failure_is_a_typed_implementation_and_dynamic_capability() {
    let registry = registry();
    assert_eq!(
        registry.level("winmm.dll", "joyGetPos"),
        Some(ImplementationLevel::SafeFail)
    );
    assert!(registry.dynamic_available("winmm.dll", "joyGetPos"));
    assert!(registry.signature_conflicts().is_empty());
    for name in [
        "joyGetNumDevs",
        "joyGetDevCapsA",
        "joyGetDevCapsW",
        "joyGetPosEx",
        "joySetCapture",
        "joyReleaseCapture",
    ] {
        assert!(registry.is_implemented("winmm.dll", name));
        assert!(registry.dynamic_available("winmm.dll", name));
    }
}
