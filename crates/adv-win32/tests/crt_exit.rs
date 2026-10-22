//! Authored CRT callback code exercises the shared exit-array owner and continuations.
use adv_abi::{ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const FIRST: u32 = CODE + 1024;
const SECOND: u32 = CODE + 1280;
const THIRD: u32 = CODE + 1536;
const RUNTIME: &str = "api-ms-win-crt-runtime-l1-1-0.dll";

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    machine_with(
        cpu,
        Win32Config::new(
            "/nonexistent/crt-exit",
            "fixture.exe",
            "/nonexistent/crt-exit-data",
        ),
    )
}
fn machine_with<B: Backend>(cpu: B, config: Win32Config) -> Machine<B> {
    let mut reg = Registry::default();
    adv_win32::register(&mut reg);
    adv_win32::msvcrt::register(&mut reg);
    let mut machine = Machine::new(cpu, reg, Clock::virtual_time()).unwrap();
    Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}
fn append_digit(code: &mut Asm, digit: u32) {
    code.mov_eax_mem(DATA);
    code.bytes.extend_from_slice(&[0x6b, 0xc0, 10]); // imul eax,eax,10
    code.add_eax_imm(digit).mov_mem_eax(DATA);
}
fn callback<B: Backend>(machine: &mut Machine<B>, pointer: u32, digit: u32) {
    let mut code = Asm::new(pointer);
    append_digit(&mut code, digit);
    // The int return of _onexit_t is ignored by the CRT.
    code.bytes
        .extend_from_slice(&[0xb8, 0xff, 0xff, 0xff, 0xff]);
    code.ret();
    machine.cpu.write(pointer, &code.bytes).unwrap();
}
fn run<B: Backend>(machine: &mut Machine<B>, mut code: Asm, expected_exit: u32) {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("CRT exit fixture", CODE, &[], 65536).unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(
        matches!(result, RunResult::Exited(code) if code == expected_exit),
        "{result:?}"
    );
}
fn registration_name(module: &str) -> &str {
    if module == "ucrtbase.dll" || module == RUNTIME {
        "_crt_atexit"
    } else {
        "_onexit"
    }
}

fn lifo<B: Backend>(cpu: B, module: &str) {
    let mut machine = machine(cpu);
    let registration = machine
        .resolve_import(module, registration_name(module))
        .unwrap();
    let atexit = machine
        .resolve_import(
            module,
            if registration_name(module) == "_onexit" {
                "atexit"
            } else {
                "_crt_atexit"
            },
        )
        .unwrap();
    let cleanup_module = if module == RUNTIME {
        "ucrtbase.dll"
    } else {
        module
    };
    let cleanup = machine.resolve_import(cleanup_module, "_cexit").unwrap();
    let sleep = machine.resolve_import("kernel32.dll", "Sleep").unwrap();
    callback(&mut machine, FIRST, 1);
    callback(&mut machine, THIRD, 3);
    let mut second = Asm::new(SECOND);
    append_digit(&mut second, 2);
    second
        .push_imm(THIRD)
        .call(registration)
        .add_esp(4)
        .mov_mem_eax(DATA + 12);
    second.push_imm(1).call(sleep).ret();
    machine.cpu.write(SECOND, &second.bytes).unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(FIRST)
        .call(registration)
        .add_esp(4)
        .mov_mem_eax(DATA + 4);
    code.push_imm(SECOND)
        .call(atexit)
        .add_esp(4)
        .mov_mem_eax(DATA + 8);
    code.call(cleanup).mov_eax_mem(DATA).mov_mem_eax(DATA + 16);
    code.call(cleanup).mov_eax_mem(DATA).mov_mem_eax(DATA + 20);
    code.push_imm(FIRST)
        .call(registration)
        .add_esp(4)
        .call(cleanup);
    run(&mut machine, code, 0);
    let pointer_return = registration_name(module) == "_onexit";
    assert_eq!(
        machine.cpu.read_u32(DATA + 4).unwrap(),
        if pointer_return { FIRST } else { 0 }
    );
    assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), 0);
    assert_eq!(
        machine.cpu.read_u32(DATA + 12).unwrap(),
        if pointer_return { THIRD } else { 0 }
    );
    assert_eq!(machine.cpu.read_u32(DATA + 16).unwrap(), 231);
    assert_eq!(machine.cpu.read_u32(DATA + 20).unwrap(), 231);
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 2311);
}
// spec: CRT-LIB-18
#[test]
fn process_exit_registrations_are_lifo_and_accept_callbacks_added_during_waiting_drain() {
    for module in [
        "msvcrt20.dll",
        "msvcrt40.dll",
        "msvcr70.dll",
        "msvcr71.dll",
        "msvcr100.dll",
        "msvcr110.dll",
        "msvcr120.dll",
        "msvcrt.dll",
        "msvcr80.dll",
        "msvcr90.dll",
        "ucrtbase.dll",
        RUNTIME,
    ] {
        for jit in [false, true] {
            lifo(JitCpu::new(PARK_ADDR, jit).unwrap(), module);
        }
        lifo(UnicornCpu::new(PARK_ADDR).unwrap(), module);
    }
}

fn exit_mode<B: Backend>(cpu: B, module: &str, mode: &str) {
    let mut machine = machine(cpu);
    let registration = machine
        .resolve_import(module, registration_name(module))
        .unwrap();
    callback(&mut machine, FIRST, 1);
    let mut code = Asm::new(CODE);
    code.push_imm(FIRST).call(registration).add_esp(4);
    if mode != "raw" {
        let target = machine
            .resolve_import(
                if mode == "ExitProcess" {
                    "kernel32.dll"
                } else {
                    module
                },
                mode,
            )
            .unwrap();
        code.push_imm(73).call(target);
    }
    run(&mut machine, code, if mode == "raw" { 0 } else { 73 });
    assert_eq!(
        machine.cpu.read_u32(DATA).unwrap(),
        u32::from(mode == "exit"),
        "{module}!{mode}"
    );
}
// spec: CRT-LIB-18
#[test]
fn crt_exit_drains_but_fast_exit_kernel_exit_and_raw_entry_return_do_not() {
    for module in ["msvcr90.dll", RUNTIME] {
        for mode in ["exit", "_exit", "ExitProcess", "raw"] {
            for jit in [false, true] {
                exit_mode(JitCpu::new(PARK_ADDR, jit).unwrap(), module, mode);
            }
            exit_mode(UnicornCpu::new(PARK_ADDR).unwrap(), module, mode);
        }
    }
}

fn independent<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    callback(&mut machine, FIRST, 1);
    callback(&mut machine, SECOND, 2);
    let mut code = Asm::new(CODE);
    for (module, name, target) in [
        ("msvcr90.dll", "_onexit", FIRST),
        (RUNTIME, "_crt_atexit", SECOND),
    ] {
        let registration = machine.resolve_import(module, name).unwrap();
        code.push_imm(target).call(registration).add_esp(4);
    }
    let cleanup_90 = machine.resolve_import("msvcr90.dll", "_cexit").unwrap();
    let cleanup_ucrt = machine.resolve_import("ucrtbase.dll", "_cexit").unwrap();
    code.call(cleanup_90)
        .mov_eax_mem(DATA)
        .mov_mem_eax(DATA + 4);
    code.call(cleanup_ucrt);
    run(&mut machine, code, 0);
    assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 12);
}
// spec: CRT-LIB-18
#[test]
fn canonical_crt_instances_keep_independent_tables_while_ucrt_apisets_share_one() {
    for jit in [false, true] {
        independent(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    independent(UnicornCpu::new(PARK_ADDR).unwrap());
    let mut reg = Registry::default();
    adv_win32::msvcrt::register(&mut reg);
    assert!(reg.signature_conflicts().is_empty());
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        assert_eq!(
            reg.level(module, "_onexit"),
            Some(ImplementationLevel::Implement)
        );
        assert!(reg.dynamic_available(module, "_onexit"));
    }
    assert!(reg.dynamic_available(RUNTIME, "_crt_atexit"));
    assert!(!reg.dynamic_available("ucrtbase.dll", "_onexit"));
}

fn invalid_parameter_noinfo<B: Backend>(cpu: B, module: &str, handler: bool) {
    let mut machine = machine(cpu);
    let report = machine
        .resolve_import(module, "_invalid_parameter_noinfo")
        .unwrap();
    callback(&mut machine, FIRST, 1);
    let mut code = Asm::new(CODE);
    if handler {
        let setter = machine
            .resolve_import(module, "_set_invalid_parameter_handler")
            .unwrap();
        code.push_imm(FIRST).call(setter).add_esp(4);
    }
    code.call(report);
    append_digit(&mut code, 2);
    run(&mut machine, code, if handler { 0 } else { 0xc000_0417 });
    assert_eq!(
        machine.cpu.read_u32(DATA).unwrap(),
        if handler { 12 } else { 0 },
        "{module} handler={handler}"
    );
}
#[test]
fn invalid_parameter_noinfo_returns_through_handler_or_ends_process() {
    for module in ["msvcr80.dll", "msvcr90.dll"] {
        for handler in [false, true] {
            for jit in [false, true] {
                invalid_parameter_noinfo(JitCpu::new(PARK_ADDR, jit).unwrap(), module, handler);
            }
            invalid_parameter_noinfo(UnicornCpu::new(PARK_ADDR).unwrap(), module, handler);
        }
    }
}

#[path = "crt_exit/versions.rs"]
mod versions;
