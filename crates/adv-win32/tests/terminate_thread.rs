//! Authored guest calls: terminating another context retires execution and wakes
//! thread waiters without clearing process diagnostics. No games or native UI.
use adv_abi::{ApiDiagnostics, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const WORKER: u32 = CODE + 0x1000;
const WAITER: u32 = CODE + 0x2000;
const DATA: u32 = 0x410000;
const GATE: u32 = DATA;
const WORKER_ACK: u32 = DATA + 4;
const WAITER_ACK: u32 = DATA + 8;
const THREAD: u32 = DATA + 12;
const WAITER_THREAD: u32 = DATA + 16;
const STARTED: u32 = DATA + 20;
const AFTER_WAIT: u32 = DATA + 24;
const WAITER_RESULT: u32 = DATA + 28;
const WAITER_EXIT_CODE: u32 = DATA + 32;
const WAITER_EXIT_QUERY: u32 = DATA + 36;
const TERMINATED: u32 = DATA + 40;
const SUCCESS_ERROR: u32 = DATA + 44;
const MAIN_WAIT: u32 = DATA + 48;
const EXIT_CODE: u32 = DATA + 52;
const EXIT_QUERY: u32 = DATA + 56;
const INVALID_RESULT: u32 = DATA + 60;
const INVALID_ERROR: u32 = DATA + 64;
const MESSAGE_RESULT: u32 = DATA + 68;
const WORKER_MESSAGE_RESULT: u32 = DATA + 72;
const LOOKUP_RESULT: u32 = DATA + 76;
const TEXT: u32 = DATA + 0x800;
const CAPTION: u32 = DATA + 0x900;
const WORKER_TEXT: u32 = DATA + 0xa00;
const SYMBOL: u32 = DATA + 0xb00;
const TERMINATION_CODE: u32 = 0x8765_4321;
const ERROR_SENTINEL: u32 = 0x2468_abcd;

#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as M, Value as I};

fn invoke(
    machine: &mut Machine<JitCpu>,
    code: &mut Asm,
    api: &str,
    args: &[Arg],
    output: u32,
) -> u32 {
    let (module, name) = api.split_once('!').unwrap_or(("kernel32.dll", api));
    let entry = machine.resolve_import(module, name).unwrap();
    for arg in args.iter().rev() {
        match arg {
            I(value) => {
                code.push_imm(*value);
            }
            M(pointer) => {
                code.mov_eax_mem(*pointer).push_eax();
            }
        }
    }
    code.call(entry);
    let caller = code.here();
    if output != 0 {
        code.mov_mem_eax(output);
    }
    caller
}

fn case(jit: bool, blocked: bool) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/terminate-thread-probe",
            "terminate-thread-probe.exe",
            "/nonexistent/terminate-thread-probe-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 0x3000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    for (pointer, bytes) in [
        (TEXT, b"diagnostic before termination\0".as_slice()),
        (CAPTION, b"TerminateThread regression\0"),
        (WORKER_TEXT, b"blocked worker diagnostic\0"),
        (SYMBOL, b"AuthoredTerminateThreadMissingExport\0"),
    ] {
        machine.cpu.write(pointer, bytes).unwrap();
    }

    let mut worker = Asm::new(WORKER);
    invoke(
        &mut machine,
        &mut worker,
        "user32.dll!MessageBoxA",
        &[I(0), I(WORKER_TEXT), I(CAPTION), I(0)],
        WORKER_MESSAGE_RESULT,
    );
    worker.mov_mem_imm(STARTED, 1);
    invoke(&mut machine, &mut worker, "SetEvent", &[M(WORKER_ACK)], 0);
    invoke(
        &mut machine,
        &mut worker,
        "WaitForSingleObject",
        &[M(GATE), I(u32::MAX)],
        0,
    );
    worker.mov_mem_imm(AFTER_WAIT, 1);
    worker.bytes.extend_from_slice(&[0x31, 0xc0]);
    worker.ret_n(4);
    machine.cpu.write(WORKER, &worker.bytes).unwrap();

    let mut waiter = Asm::new(WAITER);
    invoke(&mut machine, &mut waiter, "SetEvent", &[M(WAITER_ACK)], 0);
    invoke(
        &mut machine,
        &mut waiter,
        "WaitForSingleObject",
        &[M(THREAD), I(u32::MAX)],
        WAITER_RESULT,
    );
    invoke(
        &mut machine,
        &mut waiter,
        "GetExitCodeThread",
        &[M(THREAD), I(WAITER_EXIT_CODE)],
        WAITER_EXIT_QUERY,
    );
    waiter.bytes.extend_from_slice(&[0x31, 0xc0]);
    waiter.ret_n(4);
    machine.cpu.write(WAITER, &waiter.bytes).unwrap();

    let mut main = Asm::new(CODE);
    // Invalid HMODULE creates an actual guest lookup diagnostic before MessageBoxA.
    // Neither diagnostic may disappear when a different guest context is retired.
    let lookup_caller = invoke(
        &mut machine,
        &mut main,
        "GetProcAddress",
        &[I(0xdead_0000), I(SYMBOL)],
        LOOKUP_RESULT,
    );
    let message_caller = invoke(
        &mut machine,
        &mut main,
        "user32.dll!MessageBoxA",
        &[I(0), I(TEXT), I(CAPTION), I(0)],
        MESSAGE_RESULT,
    );
    for output in [GATE, WORKER_ACK, WAITER_ACK] {
        invoke(
            &mut machine,
            &mut main,
            "CreateEventA",
            &[I(0), I(1), I(0), I(0)],
            output,
        );
    }
    invoke(
        &mut machine,
        &mut main,
        "CreateThread",
        &[I(0), I(0), I(WORKER), I(0), I(0), I(0)],
        THREAD,
    );
    invoke(
        &mut machine,
        &mut main,
        "CreateThread",
        &[I(0), I(0), I(WAITER), I(0), I(0), I(0)],
        WAITER_THREAD,
    );
    if blocked {
        // In the serial scheduler each signaller proceeds into its infinite wait
        // before the main context resumes: target waits on GATE, observer on target.
        invoke(
            &mut machine,
            &mut main,
            "WaitForSingleObject",
            &[M(WORKER_ACK), I(u32::MAX)],
            0,
        );
        invoke(
            &mut machine,
            &mut main,
            "WaitForSingleObject",
            &[M(WAITER_ACK), I(u32::MAX)],
            0,
        );
    }
    // The ready case has no yield between creation and termination.
    invoke(
        &mut machine,
        &mut main,
        "SetLastError",
        &[I(ERROR_SENTINEL)],
        0,
    );
    invoke(
        &mut machine,
        &mut main,
        "TerminateThread",
        &[M(THREAD), I(TERMINATION_CODE)],
        TERMINATED,
    );
    invoke(&mut machine, &mut main, "GetLastError", &[], SUCCESS_ERROR);
    // A released former wait must not resurrect the retired target continuation.
    invoke(&mut machine, &mut main, "SetEvent", &[M(GATE)], 0);
    invoke(
        &mut machine,
        &mut main,
        "WaitForSingleObject",
        &[M(WAITER_THREAD), I(u32::MAX)],
        MAIN_WAIT,
    );
    invoke(
        &mut machine,
        &mut main,
        "TerminateThread",
        &[I(0xdead_beef), I(99)],
        INVALID_RESULT,
    );
    invoke(&mut machine, &mut main, "GetLastError", &[], INVALID_ERROR);
    invoke(
        &mut machine,
        &mut main,
        "GetExitCodeThread",
        &[M(THREAD), I(EXIT_CODE)],
        EXIT_QUERY,
    );
    for handle in [THREAD, WAITER_THREAD, GATE, WORKER_ACK, WAITER_ACK] {
        invoke(&mut machine, &mut main, "CloseHandle", &[M(handle)], 0);
    }
    main.bytes.extend_from_slice(&[0x31, 0xc0]);
    main.ret();
    machine.cpu.write(CODE, &main.bytes).unwrap();
    machine
        .start_main(CODE, CODE, 65536, "terminate-thread-probe.exe")
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(
        matches!(result, RunResult::Exited(0)),
        "jit={jit}, blocked={blocked}: {result:?}"
    );

    let word = |pointer| machine.cpu.read_u32(pointer).unwrap();
    assert_eq!(word(TERMINATED), 1);
    assert_eq!(word(SUCCESS_ERROR), ERROR_SENTINEL);
    assert_eq!(word(STARTED), u32::from(blocked));
    assert_eq!(word(AFTER_WAIT), 0, "retired target must never resume");
    assert_eq!(word(WAITER_RESULT), 0); // WAIT_OBJECT_0
    assert_eq!(word(MAIN_WAIT), 0);
    assert_eq!(word(WAITER_EXIT_QUERY), 1);
    assert_eq!(word(WAITER_EXIT_CODE), TERMINATION_CODE);
    assert_eq!(word(EXIT_QUERY), 1);
    assert_eq!(word(EXIT_CODE), TERMINATION_CODE);
    assert_eq!(word(INVALID_RESULT), 0);
    assert_eq!(word(INVALID_ERROR), 6); // ERROR_INVALID_HANDLE
    assert_eq!(machine.sys.contexts[1].exit_code(), Some(TERMINATION_CODE));
    assert!(machine.sys.contexts[2].is_done());
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert_eq!(diagnostics.dynamic_lookup_failure_count, 1);
    assert_eq!(diagnostics.message_box_count, 1 + u64::from(blocked));
    let failure = diagnostics.dynamic_lookup_failures.front().unwrap();
    assert_eq!(failure.caller, lookup_caller);
    let message = diagnostics.message_boxes.front().unwrap();
    assert_eq!(message.caller, message_caller);
    assert_eq!(message.text, "diagnostic before termination");
    assert_eq!(message.preceding_lookup_failure.as_ref(), Some(failure));
    assert_eq!(word(MESSAGE_RESULT), 1);
    if blocked {
        let message = diagnostics.message_boxes.back().unwrap();
        assert_eq!(message.context, 1);
        assert_eq!(message.text, "blocked worker diagnostic");
        assert_eq!(word(WORKER_MESSAGE_RESULT), 1);
    }
}

#[test]
fn terminating_ready_and_blocked_guest_workers_signals_thread_waiters_and_retains_diagnostics() {
    for jit in [false, true] {
        for blocked in [false, true] {
            case(jit, blocked);
        }
    }
}
