//! Authored guest calls and isolated native probes; no game assets or host UI.
use adv_abi::{ApiDiagnostics, Conv, ImplementationLevel, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const WORKER: u32 = CODE + 0x800;
const DATA: u32 = 0x410000;
const SENTINEL: u32 = 0x13572468;
const MODES: [u32; 10] = [0, 1, 2, 0x8003, 0, 4, 0, 0x8000, u32::MAX, 0];
const PREVIOUS: [u32; 10] = [0, 0, 1, 2, 0x8003, 0, 4, 4, 0x8004, u32::MAX];
const CURRENT: [u32; 10] = [0, 1, 2, 0x8003, 0, 4, 4, 0x8004, u32::MAX, 4];

fn machine(jit: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry.set_mode(RunMode::Strict);
    assert!(registry.signature_conflicts().is_empty());
    for (name, argc) in [("GetErrorMode", 0), ("SetErrorMode", 1)] {
        assert_eq!(
            registry.get("kernel32.dll", name).unwrap().conv,
            Conv::Stdcall { argc }
        );
        assert_eq!(
            registry.level("kernel32.dll", name),
            Some(ImplementationLevel::State)
        );
        assert!(registry.dynamic_available("kernel32.dll", name));
    }
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/error-mode",
            "fixture.exe",
            "/nonexistent/error-mode-data",
        ),
    )
    .unwrap();
    machine.ext.get_or_default::<ApiDiagnostics>();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}

fn call(machine: &mut Machine<JitCpu>, code: &mut Asm, name: &str, args: &[u32], out: u32) {
    for arg in args.iter().rev() {
        code.push_imm(*arg);
    }
    code.call(machine.resolve_import("kernel32.dll", name).unwrap());
    if out != 0 {
        code.mov_mem_eax(out);
    }
}

fn run(machine: &mut Machine<JitCpu>, mut code: Asm) {
    call(machine, &mut code, "ExitProcess", &[0], 0);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("error-mode fixture", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.warnings.is_empty());
}

#[test]
fn exported_modes_return_previous_preserve_last_error_and_are_process_owned() {
    for jit in [false, true] {
        let mut first = machine(jit);
        let mut second = machine(jit);
        let mut code = Asm::new(CODE);
        for (i, mode) in MODES.into_iter().enumerate() {
            let out = DATA + i as u32 * 16;
            call(&mut first, &mut code, "SetLastError", &[SENTINEL], 0);
            call(&mut first, &mut code, "SetErrorMode", &[mode], out);
            call(&mut first, &mut code, "GetLastError", &[], out + 4);
            call(&mut first, &mut code, "GetErrorMode", &[], out + 8);
            call(&mut first, &mut code, "GetLastError", &[], out + 12);
        }
        run(&mut first, code);
        for i in 0..MODES.len() {
            let out = DATA + i as u32 * 16;
            assert_eq!(first.cpu.read_u32(out).unwrap(), PREVIOUS[i]);
            assert_eq!(first.cpu.read_u32(out + 4).unwrap(), SENTINEL);
            assert_eq!(first.cpu.read_u32(out + 8).unwrap(), CURRENT[i]);
            assert_eq!(first.cpu.read_u32(out + 12).unwrap(), SENTINEL);
        }
        let mut code = Asm::new(CODE);
        call(&mut second, &mut code, "GetErrorMode", &[], DATA);
        run(&mut second, code);
        assert_eq!(second.cpu.read_u32(DATA).unwrap(), 0);
    }
}

#[test]
fn guest_threads_observe_and_replace_the_same_process_mode() {
    for jit in [false, true] {
        let mut machine = machine(jit);
        let mut worker = Asm::new(WORKER);
        call(&mut machine, &mut worker, "GetErrorMode", &[], DATA);
        call(&mut machine, &mut worker, "SetErrorMode", &[2], DATA + 4);
        call(&mut machine, &mut worker, "ExitThread", &[0], 0);
        machine.cpu.write(WORKER, &worker.bytes).unwrap();

        let mut code = Asm::new(CODE);
        call(&mut machine, &mut code, "SetErrorMode", &[0x8003], 0);
        call(
            &mut machine,
            &mut code,
            "CreateThread",
            &[0, 0, WORKER, 0, 0, 0],
            DATA + 8,
        );
        code.push_imm(u32::MAX).mov_eax_mem(DATA + 8).push_eax();
        call(&mut machine, &mut code, "WaitForSingleObject", &[], 0);
        call(&mut machine, &mut code, "GetErrorMode", &[], DATA + 12);
        run(&mut machine, code);
        assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0x8003);
        assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 0x8003);
        assert_eq!(machine.cpu.read_u32(DATA + 12).unwrap(), 2);
    }
}

#[cfg(windows)]
#[test]
fn native_error_mode_matches_the_guest_sequence_in_an_isolated_process() {
    const CHILD: &str = "ADV_ERROR_MODE_NATIVE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_error_mode_matches_the_guest_sequence_in_an_isolated_process",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetErrorMode() -> u32;
        fn SetErrorMode(mode: u32) -> u32;
        fn SetLastError(error: u32);
        fn GetLastError() -> u32;
    }
    unsafe {
        SetErrorMode(0);
        // The irreversible flag may already be inherited from the test launcher.
        let inherited = GetErrorMode();
        assert_eq!(inherited & !4, 0);
        for i in 0..MODES.len() {
            SetLastError(SENTINEL);
            assert_eq!(SetErrorMode(MODES[i]), PREVIOUS[i] | inherited);
            assert_eq!(GetLastError(), SENTINEL);
            assert_eq!(GetErrorMode(), CURRENT[i] | inherited);
            assert_eq!(GetLastError(), SENTINEL);
        }
    }
}
