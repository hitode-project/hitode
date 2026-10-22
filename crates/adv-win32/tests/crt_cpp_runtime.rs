//! Authored x86 probes for CRT callback identity, isolation and nonreturn paths.
use adv_abi::{Registry, signature};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const CALLBACK: u32 = CODE + 0x3000;
const DATA: u32 = 0x410000;

fn machine(jit: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/crt-cpp",
            "fixture.exe",
            "/nonexistent/crt-cpp-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 16384, Prot::RX).unwrap();
    machine.cpu.map(DATA, 16384, Prot::RW).unwrap();
    machine
        .cpu
        .write(CALLBACK, &[0xff, 0x05, 0, 0, 0x41, 0, 0xc3])
        .unwrap();
    machine
}

fn invoke(
    m: &mut Machine<JitCpu>,
    code: &mut Asm,
    module: &str,
    name: &str,
    args: &[u32],
    output: u32,
) {
    let target = m.resolve_import(module, name).unwrap();
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(target);
    if !args.is_empty() {
        code.add_esp(args.len() as u8 * 4);
    }
    code.mov_mem_eax(output);
}

fn run(m: &mut Machine<JitCpu>, mut code: Asm) -> RunResult {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("CRT C++ runtime", CODE, &[], 65536).unwrap();
    m.run_virtual(1_000_000)
}

fn no_gap(m: &Machine<JitCpu>) {
    if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
        assert_eq!(d.stub_calls, 0);
        assert!(d.warnings.is_empty());
    }
}

// spec: EXC-CRT-1, PERS-ABI-1
#[test]
fn callback_aliases_return_the_previous_function_and_preserve_error_state() {
    let exports: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| {
            (s.name.contains("set_terminate")
                || s.name.contains("set_unexpected")
                || s.name.contains("set_se_translator")
                || s.name == "_set_purecall_handler")
                && s.module != "api-ms-win-crt-private-l1-1-0.dll"
        })
        .collect();
    assert!(exports.len() > 50);
    for jit in [false, true] {
        for export in &exports {
            assert_eq!(export.conv, Some(Conv::Cdecl { argc: 1 }));
            let mut m = machine(jit);
            let mut code = Asm::new(CODE);
            let module = export.module;
            let owner = if module == "vcruntime140.dll" || module.starts_with("api-ms-win-crt-") {
                "ucrtbase.dll"
            } else {
                module
            };
            let errno = m.resolve_import(owner, "_errno").unwrap();
            code.call(errno)
                .mov_mem_eax(DATA + 16)
                .bytes
                .extend_from_slice(&[0xc7, 0x00, 79, 0, 0, 0]);
            let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
            code.push_imm(73).call(set_error);
            // Old C++ H overloads are null-handler overloads, not arbitrary
            // integer-to-function-pointer conversions.
            let value = if export.name.ends_with("H@Z") {
                0
            } else {
                CALLBACK
            };
            invoke(&mut m, &mut code, module, export.name, &[value], DATA + 4);
            invoke(&mut m, &mut code, module, export.name, &[0], DATA + 8);
            code.call(errno).mov_eax_ptr_eax().mov_mem_eax(DATA + 20);
            let error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
            code.call(error).mov_mem_eax(DATA + 24);
            let result = run(&mut m, code);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{}!{}: {result:?}",
                module,
                export.name
            );
            assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), value);
            assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), 79);
            assert_eq!(m.cpu.read_u32(DATA + 24).unwrap(), 73);
            no_gap(&m);
        }
    }
}

// spec: EXC-CRT-2
#[test]
fn returning_terminate_and_unexpected_handlers_finish_through_abort() {
    for jit in [false, true] {
        for (module, set_term, terminate, set_unexpected, unexpected) in [
            (
                "msvcr90.dll",
                "?set_terminate@@YAP6AXXZP6AXXZ@Z",
                "?terminate@@YAXXZ",
                "?set_unexpected@@YAP6AXXZP6AXXZ@Z",
                "?unexpected@@YAXXZ",
            ),
            (
                "ucrtbase.dll",
                "set_terminate",
                "terminate",
                "set_unexpected",
                "unexpected",
            ),
        ] {
            for use_unexpected in [false, true] {
                let mut m = machine(jit);
                let mut code = Asm::new(CODE);
                invoke(&mut m, &mut code, module, set_term, &[CALLBACK], DATA + 4);
                if use_unexpected {
                    invoke(
                        &mut m,
                        &mut code,
                        module,
                        set_unexpected,
                        &[CALLBACK],
                        DATA + 8,
                    );
                }
                invoke(
                    &mut m,
                    &mut code,
                    module,
                    if use_unexpected {
                        unexpected
                    } else {
                        terminate
                    },
                    &[],
                    DATA + 12,
                );
                assert!(matches!(run(&mut m, code), RunResult::Exited(3)));
                assert_eq!(
                    m.cpu.read_u32(DATA).unwrap(),
                    if use_unexpected { 2 } else { 1 }
                );
                no_gap(&m);
            }
        }
    }
}

// spec: EXC-CRT-2
#[test]
fn split_runtime_terminate_uses_the_installed_ucrt_handler_and_respects_exit() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let exit = m.resolve_import("kernel32.dll", "ExitProcess").unwrap();
        let mut callback = Asm::new(CALLBACK);
        callback.push_imm(42).call(exit).ret();
        m.cpu.write(CALLBACK, &callback.bytes).unwrap();
        let mut code = Asm::new(CODE);
        invoke(
            &mut m,
            &mut code,
            "api-ms-win-crt-runtime-l1-1-0.dll",
            "set_terminate",
            &[CALLBACK],
            DATA + 4,
        );
        invoke(
            &mut m,
            &mut code,
            "vcruntime140.dll",
            "__std_terminate",
            &[],
            DATA + 8,
        );
        assert!(matches!(run(&mut m, code), RunResult::Exited(42)));
        no_gap(&m);
    }
}

// spec: EXC-CRT-2
#[test]
fn recursive_terminate_aborts_instead_of_calling_the_same_handler_again() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let term = m.resolve_import("ucrtbase.dll", "terminate").unwrap();
        let mut callback = Asm::new(CALLBACK);
        callback.call(term).ret();
        m.cpu.write(CALLBACK, &callback.bytes).unwrap();
        let mut code = Asm::new(CODE);
        invoke(
            &mut m,
            &mut code,
            "ucrtbase.dll",
            "set_terminate",
            &[CALLBACK],
            DATA + 4,
        );
        code.call(term);
        assert!(matches!(run(&mut m, code), RunResult::Exited(3)));
        no_gap(&m);
    }
}

// spec: EXC-CRT-1
#[test]
fn purecall_handler_is_shared_between_threads_and_runs_before_termination() {
    for jit in [false, true] {
        for (module, expected) in [
            ("msvcr90.dll", 255),
            ("ucrtbase.dll", 3),
            ("vcruntime140.dll", 3),
        ] {
            let mut m = machine(jit);
            let mut primary = Asm::new(CODE);
            let sleep = m.resolve_import("kernel32.dll", "Sleep").unwrap();
            invoke(
                &mut m,
                &mut primary,
                module,
                "_set_purecall_handler",
                &[CALLBACK],
                DATA + 4,
            );
            primary.push_imm(10).call(sleep).ret();
            let mut worker = Asm::new(CODE + 0x1000);
            invoke(
                &mut m,
                &mut worker,
                module,
                "_get_purecall_handler",
                &[],
                DATA + 8,
            );
            invoke(&mut m, &mut worker, module, "_purecall", &[], DATA + 12);
            m.cpu.write(CODE, &primary.bytes).unwrap();
            m.cpu.write(CODE + 0x1000, &worker.bytes).unwrap();
            m.spawn("CRT purecall setter", CODE, &[], 65536).unwrap();
            m.spawn("CRT purecall caller", CODE + 0x1000, &[], 65536)
                .unwrap();
            assert!(
                matches!(m.run_virtual(1_000_000), RunResult::Exited(code) if code == expected)
            );
            assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), CALLBACK);
            assert_eq!(m.cpu.read_u32(DATA).unwrap(), 1);
            no_gap(&m);
        }
    }
}

// spec: EXC-CRT-1
#[test]
fn terminate_unexpected_and_translator_slots_are_thread_local() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let sleep = m.resolve_import("kernel32.dll", "Sleep").unwrap();
        let mut primary = Asm::new(CODE);
        let mut worker = Asm::new(CODE + 0x1000);
        for (index, code) in [&mut primary, &mut worker].into_iter().enumerate() {
            let at = DATA + 32 + index as u32 * 32;
            let value = CALLBACK + index as u32 * 16;
            for (i, name) in ["set_terminate", "set_unexpected", "_set_se_translator"]
                .iter()
                .enumerate()
            {
                invoke(
                    &mut m,
                    code,
                    "ucrtbase.dll",
                    name,
                    &[value],
                    at + i as u32 * 4,
                );
            }
            code.push_imm(0).call(sleep);
            invoke(&mut m, code, "ucrtbase.dll", "_get_terminate", &[], at + 12);
            invoke(
                &mut m,
                code,
                "ucrtbase.dll",
                "_get_unexpected",
                &[],
                at + 16,
            );
            invoke(
                &mut m,
                code,
                "ucrtbase.dll",
                "_set_se_translator",
                &[0],
                at + 20,
            );
            code.bytes.extend_from_slice(&[0x31, 0xc0]);
            code.ret();
        }
        m.cpu.write(CODE, &primary.bytes).unwrap();
        m.cpu.write(CODE + 0x1000, &worker.bytes).unwrap();
        m.spawn("CRT runtime first thread", CODE, &[], 65536)
            .unwrap();
        m.spawn("CRT runtime second thread", CODE + 0x1000, &[], 65536)
            .unwrap();
        assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
        for index in 0..2 {
            let at = DATA + 32 + index * 32;
            for offset in [0, 4, 8] {
                assert_eq!(m.cpu.read_u32(at + offset).unwrap(), 0);
            }
            for offset in [12, 16, 20] {
                assert_eq!(m.cpu.read_u32(at + offset).unwrap(), CALLBACK + index * 16);
            }
        }
        no_gap(&m);
    }
}
