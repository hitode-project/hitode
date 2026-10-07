//! Authored x86 FLS calls and native no-UI callback lifetime comparison.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const CALLBACK: u32 = CODE + 0x1000;
const WORKER: u32 = CODE + 0x2000;
const DATA: u32 = 0x410000;
const INDEX: u32 = DATA;
const TLS: u32 = DATA + 4;
const READY: u32 = DATA + 8;
const GATE: u32 = DATA + 12;
const THREAD: u32 = DATA + 16;
const COUNT: u32 = DATA + 20;
const SUM: u32 = DATA + 24;
const READ_SUM: u32 = DATA + 28;
const CALLBACK_THREAD: u32 = DATA + 32;
const MAIN_THREAD: u32 = DATA + 36;
const WORKER_INITIAL: u32 = DATA + 40;
const NULL_ERROR: u32 = DATA + 44;
const SET_ERROR: u32 = DATA + 48;
const TLS_VALUE: u32 = DATA + 52;
const INVALID_RESULT: u32 = DATA + 56;
const INVALID_ERROR: u32 = DATA + 60;
const FREE_RESULT: u32 = DATA + 64;
const REUSED: u32 = DATA + 68;
const REUSED_VALUE: u32 = DATA + 72;
const EXIT_CODE: u32 = DATA + 76;
const VALUE: u32 = DATA + 80;
const ERROR_SENTINEL: u32 = 0x13572468;

#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as M, Value as I};

fn call(machine: &mut Machine<JitCpu>, code: &mut Asm, name: &str, args: &[Arg], out: u32) {
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
    code.call(machine.resolve_import("kernel32.dll", name).unwrap());
    if out != 0 {
        code.mov_mem_eax(out);
    }
}

fn machine(jit: bool, rearm: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    for (name, argc) in [
        ("FlsAlloc", 1),
        ("FlsFree", 1),
        ("FlsGetValue", 1),
        ("FlsSetValue", 2),
    ] {
        assert_eq!(
            registry.get("kernel32.dll", name).unwrap().conv,
            Conv::Stdcall { argc }
        );
        assert!(registry.dynamic_available("kernel32.dll", name));
    }
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut m,
        Win32Config::new("/nonexistent/fls", "fixture.exe", "/nonexistent/fls-data"),
    )
    .unwrap();
    m.cpu.map(CODE, 0x3000, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let mut cb = Asm::new(CALLBACK);
    cb.bytes.extend_from_slice(&[0x8b, 0x44, 0x24, 4]); // mov eax,[esp+4]
    cb.mov_mem_eax(VALUE);
    cb.bytes.extend_from_slice(&[0x03, 0x05]); // add eax,[SUM]
    cb.bytes.extend_from_slice(&SUM.to_le_bytes());
    cb.mov_mem_eax(SUM)
        .mov_eax_mem(COUNT)
        .add_eax_imm(1)
        .mov_mem_eax(COUNT);
    call(&mut m, &mut cb, "FlsGetValue", &[M(INDEX)], 0);
    cb.bytes.extend_from_slice(&[0x03, 0x05]);
    cb.bytes.extend_from_slice(&READ_SUM.to_le_bytes());
    cb.mov_mem_eax(READ_SUM);
    call(&mut m, &mut cb, "GetCurrentThreadId", &[], CALLBACK_THREAD);
    if rearm {
        call(&mut m, &mut cb, "FlsSetValue", &[M(INDEX), I(10)], 0);
    }
    // Cleanup can yield without losing the pending callback list.
    call(&mut m, &mut cb, "Sleep", &[I(1)], 0);
    cb.ret_n(4);
    m.cpu.write(CALLBACK, &cb.bytes).unwrap();
    m
}

fn event(m: &mut Machine<JitCpu>, code: &mut Asm, out: u32) {
    call(m, code, "CreateEventA", &[I(0), I(1), I(0), I(0)], out);
}

fn worker(m: &mut Machine<JitCpu>, blocked: bool) {
    let mut code = Asm::new(WORKER);
    call(m, &mut code, "FlsGetValue", &[M(INDEX)], WORKER_INITIAL);
    call(m, &mut code, "FlsSetValue", &[M(INDEX), I(2)], 0);
    call(m, &mut code, "SetEvent", &[M(READY)], 0);
    if blocked {
        call(
            m,
            &mut code,
            "WaitForSingleObject",
            &[M(GATE), I(u32::MAX)],
            0,
        );
    }
    code.bytes.extend_from_slice(&[0xb8, 42, 0, 0, 0]);
    code.ret_n(4);
    m.cpu.write(WORKER, &code.bytes).unwrap();
}

fn spawn(m: &mut Machine<JitCpu>, code: &mut Asm) {
    call(
        m,
        code,
        "CreateThread",
        &[I(0), I(0), I(WORKER), I(0), I(0), I(0)],
        THREAD,
    );
    call(m, code, "WaitForSingleObject", &[M(READY), I(u32::MAX)], 0);
}

fn run(m: &mut Machine<JitCpu>, mut code: Asm) {
    call(m, &mut code, "ExitProcess", &[I(0)], 0);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("FLS fixture", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}

#[test]
fn fls_free_clears_all_threads_before_callbacks_and_keeps_tls_independent() {
    for jit in [false, true] {
        let mut m = machine(jit, false);
        worker(&mut m, true);
        let mut code = Asm::new(CODE);
        call(&mut m, &mut code, "GetCurrentThreadId", &[], MAIN_THREAD);
        call(&mut m, &mut code, "FlsAlloc", &[I(CALLBACK)], INDEX);
        call(&mut m, &mut code, "SetLastError", &[I(ERROR_SENTINEL)], 0);
        call(&mut m, &mut code, "FlsGetValue", &[M(INDEX)], 0);
        call(&mut m, &mut code, "GetLastError", &[], NULL_ERROR);
        call(&mut m, &mut code, "FlsSetValue", &[M(INDEX), I(1)], 0);
        call(&mut m, &mut code, "TlsAlloc", &[], TLS);
        call(&mut m, &mut code, "TlsSetValue", &[M(TLS), I(0x55)], 0);
        call(&mut m, &mut code, "TlsGetValue", &[M(TLS)], TLS_VALUE);
        event(&mut m, &mut code, READY);
        event(&mut m, &mut code, GATE);
        spawn(&mut m, &mut code);
        call(&mut m, &mut code, "FlsFree", &[M(INDEX)], FREE_RESULT);
        call(&mut m, &mut code, "FlsFree", &[M(INDEX)], INVALID_RESULT);
        call(&mut m, &mut code, "GetLastError", &[], INVALID_ERROR);
        call(&mut m, &mut code, "FlsAlloc", &[I(0)], REUSED);
        call(&mut m, &mut code, "FlsGetValue", &[M(REUSED)], REUSED_VALUE);
        call(&mut m, &mut code, "SetLastError", &[I(ERROR_SENTINEL)], 0);
        call(&mut m, &mut code, "FlsSetValue", &[M(REUSED), I(0)], 0);
        call(&mut m, &mut code, "GetLastError", &[], SET_ERROR);
        call(&mut m, &mut code, "SetEvent", &[M(GATE)], 0);
        call(
            &mut m,
            &mut code,
            "WaitForSingleObject",
            &[M(THREAD), I(u32::MAX)],
            0,
        );
        run(&mut m, code);
        let word = |p| m.cpu.read_u32(p).unwrap();
        assert_eq!((word(COUNT), word(SUM), word(READ_SUM)), (2, 3, 0));
        assert_eq!(word(CALLBACK_THREAD), word(MAIN_THREAD));
        assert_eq!(
            (word(WORKER_INITIAL), word(NULL_ERROR), word(REUSED_VALUE)),
            (0, 0, 0)
        );
        assert_eq!(
            (word(FREE_RESULT), word(INVALID_RESULT), word(INVALID_ERROR)),
            (1, 0, 87)
        );
        assert_eq!(word(INDEX), word(REUSED));
        assert_eq!((word(TLS_VALUE), word(SET_ERROR)), (0x55, ERROR_SENTINEL));
    }
}

#[test]
fn fls_thread_return_and_process_exit_keep_value_visible_then_cleanup_once() {
    for jit in [false, true] {
        let mut m = machine(jit, true);
        worker(&mut m, false);
        let mut code = Asm::new(CODE);
        call(&mut m, &mut code, "FlsAlloc", &[I(CALLBACK)], INDEX);
        call(&mut m, &mut code, "FlsSetValue", &[M(INDEX), I(9)], 0);
        event(&mut m, &mut code, READY);
        spawn(&mut m, &mut code);
        call(
            &mut m,
            &mut code,
            "WaitForSingleObject",
            &[M(THREAD), I(u32::MAX)],
            0,
        );
        call(
            &mut m,
            &mut code,
            "GetExitCodeThread",
            &[M(THREAD), I(EXIT_CODE)],
            0,
        );
        run(&mut m, code);
        let word = |p| m.cpu.read_u32(p).unwrap();
        assert_eq!((word(COUNT), word(SUM), word(READ_SUM)), (2, 11, 11));
        assert_eq!(word(WORKER_INITIAL), 0);
        assert_eq!(word(EXIT_CODE), 42);
    }
}

#[cfg(windows)]
#[test]
fn native_fls_confirms_error_and_callback_contracts() {
    use std::{
        ffi::c_void,
        sync::atomic::{AtomicU32, Ordering::SeqCst},
    };
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn FlsAlloc(callback: Option<unsafe extern "system" fn(*mut c_void)>) -> u32;
        fn FlsGetValue(index: u32) -> *mut c_void;
        fn FlsSetValue(index: u32, value: *mut c_void) -> i32;
        fn FlsFree(index: u32) -> i32;
        fn SetLastError(value: u32);
        fn GetLastError() -> u32;
    }
    static SLOT: AtomicU32 = AtomicU32::new(0);
    static CALLS: AtomicU32 = AtomicU32::new(0);
    static VALUES: AtomicU32 = AtomicU32::new(0);
    static READS: AtomicU32 = AtomicU32::new(0);
    unsafe extern "system" fn cleanup(value: *mut c_void) {
        CALLS.fetch_add(1, SeqCst);
        VALUES.fetch_add(value as usize as u32, SeqCst);
        // SAFETY: SLOT is held until every thread has joined and FlsFree ends.
        unsafe {
            let read = FlsGetValue(SLOT.load(SeqCst)) as usize as u32;
            READS.fetch_add(read, SeqCst);
            if read != 0 {
                FlsSetValue(SLOT.load(SeqCst), 10usize as *mut c_void);
            }
        }
    }
    // SAFETY: No native storage is dereferenced; these are opaque integer data.
    // The allocated index is freed only after its worker has completed cleanup.
    unsafe {
        let index = FlsAlloc(Some(cleanup));
        assert_ne!(index, u32::MAX);
        SLOT.store(index, SeqCst);
        SetLastError(ERROR_SENTINEL);
        assert!(FlsGetValue(index).is_null());
        assert_eq!(GetLastError(), 0);
        SetLastError(ERROR_SENTINEL);
        assert_ne!(FlsSetValue(index, 1usize as _), 0);
        assert_eq!(GetLastError(), ERROR_SENTINEL);
        std::thread::spawn(move || {
            assert_ne!(FlsSetValue(index, 2usize as _), 0);
        })
        .join()
        .unwrap();
        assert_eq!(
            (CALLS.load(SeqCst), VALUES.load(SeqCst), READS.load(SeqCst)),
            (1, 2, 2)
        );
        assert_ne!(FlsFree(index), 0);
        assert_eq!(
            (CALLS.load(SeqCst), VALUES.load(SeqCst), READS.load(SeqCst)),
            (2, 3, 2)
        );
        assert_eq!(FlsFree(index), 0);
        assert_eq!(GetLastError(), 87);
        assert!(FlsGetValue(u32::MAX).is_null());
        assert_eq!(GetLastError(), 87);
        assert_eq!(FlsSetValue(u32::MAX, std::ptr::null_mut()), 0);
        assert_eq!(GetLastError(), 87);
    }
}
