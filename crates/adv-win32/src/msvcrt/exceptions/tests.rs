use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Conv, Machine, RunResult,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const CALLBACK: u32 = CODE + 512;
const ESCAPE: u32 = CODE + 768;
const NAME: &str = "__uncaught_exception";
const MODULE: &str = "msvcr90.dll";

// Inspect the real query from a guest callback, but allow that callback to
// return so the test also observes the phase's normal/nonlocal end.
fn observe(env: &mut Env<'_>) -> Outcome {
    let expected = env.cpu.read_u32(DATA).unwrap();
    let Outcome::Fail(reason) = uncaught(env) else {
        panic!("an active/unknown phase must not return a fabricated bool");
    };
    assert!(reason.contains(match expected {
        1 => "exception dispatch",
        2 => "stack unwind",
        3 => "C++ exception state is unknown",
        _ => panic!("invalid observation selector"),
    }));
    let count = env.cpu.read_u32(DATA + 4).unwrap();
    env.cpu.write_u32(DATA + 4, count + 1).unwrap();
    Outcome::Return(0)
}

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    registry.add("fixture.dll", "observe", Conv::Cdecl { argc: 0 }, observe);
    assert!(registry.signature_conflicts().is_empty());
    assert_eq!(
        registry.get(MODULE, NAME).unwrap().conv,
        Conv::Cdecl { argc: 0 }
    );
    assert!(registry.dynamic_available(MODULE, NAME));
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-exception-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}

fn imm(code: &mut Asm, op: &[u8], value: u32) {
    code.bytes.extend_from_slice(op);
    code.bytes.extend_from_slice(&value.to_le_bytes());
}
fn registration(code: &mut Asm) {
    code.push_imm(CALLBACK);
    code.bytes
        .extend_from_slice(&[0x64, 0xff, 0x35, 0, 0, 0, 0]);
    code.bytes
        .extend_from_slice(&[0x64, 0x89, 0x25, 0, 0, 0, 0]);
}
fn run<B: Backend>(machine: &mut Machine<B>, code: &Asm) -> RunResult {
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("CRT exception query", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}
fn clean_guest<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let query = machine.resolve_import(MODULE, NAME).unwrap();
    let errno = machine.resolve_import(MODULE, "_errno").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.call(errno).mov_mem_eax(DATA);
    code.bytes.extend_from_slice(&[0xc7, 0x00, 79, 0, 0, 0]);
    code.push_imm(73).call(set_error);
    imm(&mut code, &[0x89, 0x25], DATA + 8);
    // Zero-argument cdecl: no caller stack cleanup, result consumed as AL.
    code.call(query);
    code.bytes.extend_from_slice(&[0x0f, 0xb6, 0xc0]);
    code.mov_mem_eax(DATA + 12);
    imm(&mut code, &[0x89, 0x25], DATA + 16);
    code.call(get_error).mov_mem_eax(DATA + 20);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    assert!(matches!(run(&mut machine, &code), RunResult::Exited(0)));
    assert_eq!(
        machine.cpu.read_u32(DATA + 8).unwrap(),
        machine.cpu.read_u32(DATA + 16).unwrap()
    );
    assert_eq!(machine.cpu.read_u32(DATA + 12).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 20).unwrap(), 73);
    let errno = machine.cpu.read_u32(DATA).unwrap();
    assert_eq!(machine.cpu.read_u32(errno).unwrap(), 79);
    assert!(
        machine
            .ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}

#[test]
fn clean_vc90_query_has_cdecl_bool_abi_and_preserves_errno_and_last_error() {
    clean_guest(UnicornCpu::new(PARK_ADDR).unwrap());
    clean_guest(JitCpu::new(PARK_ADDR, false).unwrap());
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Vectored,
    Blocked,
    Frame,
    Unwind,
    Escape,
    Exit,
    CppRaise,
    CppUnwind,
}

fn phase_guest<B: Backend>(cpu: B, mode: Mode) {
    let mut machine = machine(cpu);
    let query = machine.resolve_import(MODULE, NAME).unwrap();
    let spy = machine.resolve_import("fixture.dll", "observe").unwrap();
    let vectored = machine
        .resolve_import("kernel32.dll", "AddVectoredExceptionHandler")
        .unwrap();
    let raise = machine
        .resolve_import("kernel32.dll", "RaiseException")
        .unwrap();
    let unwind = machine.resolve_import("kernel32.dll", "RtlUnwind").unwrap();
    let exit = machine
        .resolve_import("kernel32.dll", "ExitThread")
        .unwrap();
    let cpp = matches!(mode, Mode::CppRaise | Mode::CppUnwind);
    let is_vectored = matches!(
        mode,
        Mode::Vectored | Mode::Blocked | Mode::CppRaise | Mode::Exit
    );
    let is_unwind = matches!(mode, Mode::Unwind | Mode::CppUnwind);
    machine
        .cpu
        .write_u32(
            DATA,
            if cpp {
                3
            } else if is_unwind {
                2
            } else {
                1
            },
        )
        .unwrap();

    let mut handler = Asm::new(CALLBACK);
    handler.call(spy);
    if mode == Mode::Blocked {
        let sleep = machine.resolve_import("kernel32.dll", "Sleep").unwrap();
        handler.push_imm(0).call(sleep).call(spy);
    }
    if mode == Mode::Escape {
        // Abandon the marked handler continuation on the establisher's stack.
        handler
            .bytes
            .extend_from_slice(&[0x8b, 0x44, 0x24, 0x08, 0x8b, 0x10]);
        handler
            .bytes
            .extend_from_slice(&[0x64, 0x89, 0x15, 0, 0, 0, 0]);
        imm(&mut handler, &[0x8b, 0x25], DATA + 8);
        handler.jmp(ESCAPE);
    } else if mode == Mode::Exit {
        handler.push_imm(42).call(exit);
        handler.bytes.extend_from_slice(&[0x0f, 0x0b]);
    } else {
        imm(
            &mut handler,
            &[0xb8],
            if is_vectored {
                u32::MAX
            } else if is_unwind {
                1
            } else {
                0
            },
        );
        handler.ret();
    }
    machine.cpu.write(CALLBACK, &handler.bytes).unwrap();

    let mut finish = Asm::new(ESCAPE);
    finish.call(query).mov_mem_eax(DATA + 12);
    finish.bytes.extend_from_slice(&[0x31, 0xc0]);
    finish.ret();
    machine.cpu.write(ESCAPE, &finish.bytes).unwrap();
    let mut code = Asm::new(CODE);
    imm(&mut code, &[0x89, 0x25], DATA + 8);
    if is_vectored {
        code.push_imm(CALLBACK).push_imm(1).call(vectored);
    } else {
        registration(&mut code);
    }
    if is_unwind {
        let record = if cpp { DATA + 512 } else { 0 };
        if cpp {
            machine.cpu.write_u32(record, 0xe06d_7363).unwrap();
        }
        code.push_imm(77)
            .push_imm(record)
            .push_imm(0)
            .push_imm(0)
            .call(unwind);
        code.add_esp(8); // Registrations removed by RtlUnwind; discard their stack cells.
    } else {
        code.push_imm(0)
            .push_imm(0)
            .push_imm(0)
            .push_imm(if cpp { 0xe06d_7363 } else { 0x1234_5678 })
            .call(raise);
        if !is_vectored {
            code.bytes
                .extend_from_slice(&[0x58, 0x64, 0xa3, 0, 0, 0, 0]);
            code.add_esp(4);
        }
    }
    code.call(query).mov_mem_eax(DATA + 12);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    let result = if mode == Mode::Blocked {
        let mut worker = Asm::new(CODE + 1024);
        worker.call(query).mov_mem_eax(DATA + 16);
        worker.bytes.extend_from_slice(&[0x31, 0xc0]);
        worker.ret();
        machine.cpu.write(CODE + 1024, &worker.bytes).unwrap();
        machine.cpu.write_u32(DATA + 16, u32::MAX).unwrap();
        machine.cpu.write(CODE, &code.bytes).unwrap();
        machine
            .spawn("active exception owner", CODE, &[], 65536)
            .unwrap();
        machine
            .spawn("independent ready CRT worker", CODE + 1024, &[], 65536)
            .unwrap();
        let result = machine.run_virtual(1_000_000);
        assert_eq!(machine.cpu.read_u32(DATA + 16).unwrap(), 0);
        result
    } else {
        run(&mut machine, &code)
    };
    if cpp {
        assert!(
            matches!(result, RunResult::Failed(ref reason) if reason.message.contains("C++ exception state is unknown")),
            "{result:?}"
        );
    } else if mode == Mode::Exit {
        assert!(matches!(result, RunResult::Exited(42)), "{result:?}");
    } else {
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_eq!(machine.cpu.read_u32(DATA + 12).unwrap(), 0);
    }
    assert_eq!(
        machine.cpu.read_u32(DATA + 4).unwrap(),
        if mode == Mode::Blocked { 2 } else { 1 }
    );
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 1);
    assert_eq!(
        *diagnostics.warnings.values().next().unwrap(),
        if cpp || mode == Mode::Blocked { 2 } else { 1 }
    );
    assert_eq!(diagnostics.stub_calls, 0);

    // The phase has ended even when the callback never returned. C++ taint
    // survives that end, but never belongs to a different guest thread.
    let owner = machine.sys.current;
    let thunk = (query - THUNK_BASE) / THUNK_STRIDE;
    let env = Env {
        context: owner,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk,
    };
    if cpp {
        assert!(
            crate::kernel32::uncaught_query_blocker(&env)
                .unwrap()
                .contains("C++ exception state is unknown")
        );
    } else {
        assert_eq!(crate::kernel32::uncaught_query_blocker(&env), None);
    }
    let other = machine
        .spawn("independent clean CRT thread", PARK_ADDR, &[], 65536)
        .unwrap();
    let mut env = Env {
        context: other,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk,
    };
    assert!(matches!(uncaught(&mut env), Outcome::Return(0)));
}

#[test]
fn dispatch_and_unwind_phases_end_on_return_nonlocal_escape_and_thread_exit() {
    for mode in [
        Mode::Vectored,
        Mode::Blocked,
        Mode::Frame,
        Mode::Unwind,
        Mode::Escape,
        Mode::Exit,
    ] {
        phase_guest(UnicornCpu::new(PARK_ADDR).unwrap(), mode);
        phase_guest(JitCpu::new(PARK_ADDR, false).unwrap(), mode);
    }
}

#[test]
fn cpp_raise_and_external_unwind_record_remain_explicitly_unknown_per_thread() {
    for mode in [Mode::CppRaise, Mode::CppUnwind] {
        phase_guest(UnicornCpu::new(PARK_ADDR).unwrap(), mode);
        phase_guest(JitCpu::new(PARK_ADDR, false).unwrap(), mode);
    }
}

#[test]
fn empty_type_info_cleanup_is_idempotent_and_nonempty_cache_is_not_discarded() {
    use adv_cpu::backend::{Guest, Reg};
    let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    m.spawn("empty cleanup", PARK_ADDR, &[], 65536).unwrap();
    let thunk = (m
        .resolve_import("msvcr80.dll", "__clean_type_info_names_internal")
        .unwrap()
        - THUNK_BASE)
        / THUNK_STRIDE;
    m.cpu.set_reg(Reg::Esp, DATA);
    m.cpu.write_u32(DATA + 4, DATA + 128).unwrap();
    let mut env = Env {
        context: 0,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk,
    };
    crate::util::set_last_error(&mut env, 0x1357);
    for _ in 0..2 {
        assert!(matches!(clean_names(&mut env), Outcome::Return(0)));
    }
    assert_eq!(crate::util::last_error(&env), 0x1357);
    env.cpu.write_u32(DATA + 128, 0x1234).unwrap();
    assert!(matches!(clean_names(&mut env), Outcome::Fail(_)));
    assert_eq!(env.cpu.read_u32(DATA + 128).unwrap(), 0x1234);
}
