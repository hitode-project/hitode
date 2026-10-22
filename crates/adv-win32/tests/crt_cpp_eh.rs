//! Authored MSVC x86 EH records and funclets exercise Windows dispatch in both
//! execution modes, including object conversion, cleanup and nonlocal catch.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};
const CODE: u32 = 0x400000;
const STUB: u32 = CODE + 0x1000;
const CATCH: u32 = CODE + 0x1400;
const CLEAN: u32 = CODE + 0x1800;
const DESTROY: u32 = CODE + 0x1c00;
const CONT: u32 = CODE + 0x2000;
const INNER: u32 = CODE + 0x2400;
const DATA: u32 = 0x410000;
const FUNC: u32 = DATA + 0x100;
const UNWIND: u32 = DATA + 0x200;
const TRIES: u32 = DATA + 0x300;
const HANDLERS: u32 = DATA + 0x400;
const THROW: u32 = DATA + 0x500;
const ARRAY: u32 = DATA + 0x600;
const TYPE: u32 = DATA + 0x700;
const BASE: u32 = DATA + 0x780;
const CT: u32 = DATA + 0x800;
const OBJECT: u32 = DATA + 0x900;
const INNER_FUNC: u32 = DATA + 0xa00;
const LOG: u32 = DATA + 0xb00;
fn imm(a: &mut Asm, op: &[u8], v: u32) {
    a.bytes.extend_from_slice(op);
    a.bytes.extend_from_slice(&v.to_le_bytes());
}
fn words(m: &mut Machine<JitCpu>, p: u32, values: &[u32]) {
    for (i, v) in values.iter().enumerate() {
        m.cpu.write_u32(p + i as u32 * 4, *v).unwrap();
    }
}

fn store(m: &mut Machine<JitCpu>, p: u32, a: Asm) {
    m.cpu.write(p, &a.bytes).unwrap();
}
fn log(a: &mut Asm, v: u32) {
    a.mov_eax_mem(DATA)
        .bytes
        .extend_from_slice(&[0x89, 0xc1, 0xc1, 0xe1, 2]);
    imm(a, &[0xc7, 0x81], LOG);
    a.bytes.extend_from_slice(&v.to_le_bytes());
    imm(a, &[0xff, 0x05], DATA);
}
fn machine(jit: bool, module: &str, handler: &str, magic: u32) -> Machine<JitCpu> {
    let mut r = Registry::default();
    adv_win32::register(&mut r);
    adv_win32::msvcrt::register(&mut r);
    assert!(r.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        r,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/crt-cpp-eh",
            "fixture.exe",
            "/nonexistent/crt-cpp-eh-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 0x4000, Prot::RX).unwrap();
    m.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    let frame = m.resolve_import(module, handler).unwrap();
    let mut stub = Asm::new(STUB);
    imm(&mut stub, &[0xb8], FUNC);
    stub.jmp(frame);
    store(&mut m, STUB, stub);
    words(&mut m, FUNC, &[magic, 4, UNWIND, 1, TRIES, 0, 0, 0, 0]);
    words(
        &mut m,
        UNWIND,
        &[u32::MAX, 0, 0, CLEAN + 64, 1, CLEAN + 128, 0, CLEAN + 192],
    );
    words(&mut m, TRIES, &[0, 2, 3, 1, HANDLERS]);
    words(&mut m, HANDLERS, &[0, TYPE, (-32i32) as u32, CATCH]);
    words(&mut m, THROW, &[0, DESTROY, 0, ARRAY]);
    words(&mut m, ARRAY, &[1, CT]);
    words(&mut m, CT, &[0, TYPE, 0, u32::MAX, 0, 8, 0]);
    m.cpu.write(TYPE + 8, b".?AVFixture@@\0").unwrap();
    m.cpu.write(BASE + 8, b".?AVBase@@\0").unwrap();
    words(&mut m, OBJECT, &[0x12345678, 0x76543210]);
    for i in 1..=3 {
        let mut a = Asm::new(CLEAN + i * 64);
        log(&mut a, i);
        a.ret();
        store(&mut m, CLEAN + i * 64, a);
    }
    let mut a = Asm::new(DESTROY);
    imm(&mut a, &[0x89, 0x0d], DATA + 24);
    imm(&mut a, &[0xff, 0x05], DATA + 20);
    a.ret();
    store(&mut m, DESTROY, a);
    let mut a = Asm::new(CATCH);
    a.bytes.extend_from_slice(&[0x8b, 0x45, 0xe0]);
    a.mov_mem_eax(DATA + 8);
    a.bytes.extend_from_slice(&[0x8b, 0x45, 0xe4]);
    a.mov_mem_eax(DATA + 12);
    log(&mut a, 9);
    imm(&mut a, &[0xb8], CONT);
    a.ret();
    store(&mut m, CATCH, a);
    let mut a = Asm::new(CONT);
    a.bytes.extend_from_slice(&[
        0x8b, 0x4d, 0xf4, 0x64, 0x89, 0x0d, 0, 0, 0, 0, 0x89, 0xec, 0x5d, 0x31, 0xc0, 0xc3,
    ]);
    store(&mut m, CONT, a);
    m
}
fn prologue(a: &mut Asm, handler: u32, state: u32) {
    a.bytes.extend_from_slice(&[0x55, 0x89, 0xe5]);
    a.push_imm(state)
        .push_imm(handler)
        .bytes
        .extend_from_slice(&[
            0x64, 0xff, 0x35, 0, 0, 0, 0, 0x83, 0xec, 0x74, 0x8d, 0x45, 0xf4, 0x64, 0xa3, 0, 0, 0,
            0, 0x89, 0x65, 0xf0,
        ]);
}
fn raise(m: &mut Machine<JitCpu>, module: &str, a: &mut Asm, rethrow: bool) {
    let throw = m.resolve_import(module, "_CxxThrowException").unwrap();
    a.push_imm(if rethrow { 0 } else { THROW })
        .push_imm(if rethrow { 0 } else { OBJECT })
        .call(throw);
}
fn run(m: &mut Machine<JitCpu>, a: Asm) -> RunResult {
    store(m, CODE, a);
    m.spawn("C++ exception fixture", CODE, &[], 65536).unwrap();
    m.run_virtual(1_000_000)
}
fn clean(m: &Machine<JitCpu>) {
    if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
        assert_eq!(d.stub_calls, 0);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }
}
fn logs(m: &Machine<JitCpu>) -> Vec<u32> {
    (0..m.cpu.read_u32(DATA).unwrap())
        .map(|i| m.cpu.read_u32(LOG + i * 4).unwrap())
        .collect()
}

// spec: EXC-CPP-1, EXC-CPP-2, EXC-CPP-3
#[test]
fn value_reference_base_and_ellipsis_catches_share_cleanup_and_object_lifetime() {
    for jit in [false, true] {
        for (module, handler, magic) in [
            ("msvcrt.dll", "__CxxFrameHandler", 0x19930520),
            ("msvcr90.dll", "__CxxFrameHandler2", 0x19930521),
            ("vcruntime140.dll", "__CxxFrameHandler3", 0x19930522),
        ] {
            for mode in 0..4 {
                let mut m = machine(jit, module, handler, magic);
                match mode {
                    1 => words(&mut m, HANDLERS, &[8, TYPE, (-32i32) as u32, CATCH]),
                    2 => {
                        words(&mut m, HANDLERS, &[0, BASE, (-32i32) as u32, CATCH]);
                        words(&mut m, ARRAY, &[2, CT, CT + 32]);
                        words(&mut m, CT + 32, &[0, BASE, 4, u32::MAX, 0, 4, 0]);
                    }
                    3 => words(&mut m, HANDLERS, &[0, 0, 0, CATCH]),
                    _ => {}
                }
                let mut a = Asm::new(CODE);
                prologue(&mut a, STUB, 2);
                raise(&mut m, module, &mut a, false);
                a.bytes.extend_from_slice(&[0xcc]);
                let result = run(&mut m, a);
                assert!(
                    matches!(result, RunResult::Exited(0)),
                    "{jit} {module} mode {mode}: {result:?}"
                );
                assert_eq!(logs(&m), vec![2, 1, 9]);
                assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), 1);
                assert_eq!(m.cpu.read_u32(DATA + 24).unwrap(), OBJECT);
                match mode {
                    0 => {
                        assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 0x12345678);
                        assert_eq!(m.cpu.read_u32(DATA + 12).unwrap(), 0x76543210);
                    }
                    1 => assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), OBJECT),
                    2 => assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 0x76543210),
                    _ => {}
                }
                clean(&m);
            }
        }
    }
}

// spec: EXC-CPP-1, EXC-CPP-2
#[test]
fn cross_frame_throw_runs_inner_destructors_before_outer_catch() {
    for jit in [false, true] {
        let module = "msvcr90.dll";
        let mut m = machine(jit, module, "__CxxFrameHandler3", 0x19930522);
        words(
            &mut m,
            INNER_FUNC,
            &[0x19930522, 4, UNWIND, 0, 0, 0, 0, 0, 0],
        );
        let frame = m.resolve_import(module, "__CxxFrameHandler3").unwrap();
        let mut stub = Asm::new(STUB + 64);
        imm(&mut stub, &[0xb8], INNER_FUNC);
        stub.jmp(frame);
        store(&mut m, STUB + 64, stub);
        let mut inner = Asm::new(INNER);
        prologue(&mut inner, STUB + 64, 2);
        raise(&mut m, module, &mut inner, false);
        inner.bytes.push(0xcc);
        store(&mut m, INNER, inner);
        let mut a = Asm::new(CODE);
        prologue(&mut a, STUB, 0);
        a.call(INNER).bytes.push(0xcc);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_eq!(logs(&m), vec![2, 1, 9]);
        clean(&m);
    }
}

// spec: EXC-CPP-1, EXC-CPP-3
#[test]
fn rethrow_enters_outer_nested_try_without_destroying_object_twice() {
    for jit in [false, true] {
        let module = "msvcr90.dll";
        let mut m = machine(jit, module, "__CxxFrameHandler3", 0x19930522);
        words(&mut m, FUNC, &[0x19930522, 5, UNWIND, 2, TRIES, 0, 0, 0, 0]);
        words(
            &mut m,
            TRIES,
            &[1, 2, 3, 1, HANDLERS, 0, 3, 4, 1, HANDLERS + 16],
        );
        words(&mut m, HANDLERS + 16, &[0, TYPE, (-32i32) as u32, CATCH]);
        let mut inner = Asm::new(CATCH + 128);
        log(&mut inner, 8);
        raise(&mut m, module, &mut inner, true);
        inner.bytes.push(0xcc);
        store(&mut m, CATCH + 128, inner);
        m.cpu.write_u32(HANDLERS + 12, CATCH + 128).unwrap();
        let mut a = Asm::new(CODE);
        prologue(&mut a, STUB, 2);
        raise(&mut m, module, &mut a, false);
        a.bytes.push(0xcc);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_eq!(logs(&m), vec![2, 8, 3, 9]);
        assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), 1);
        clean(&m);
    }
}

// spec: EXC-CPP-4
#[test]
fn uncaught_cpp_throw_enters_installed_terminate_handler() {
    for jit in [false, true] {
        let module = "msvcr90.dll";
        let mut m = machine(jit, module, "__CxxFrameHandler3", 0x19930522);
        let exit = m.resolve_import("kernel32.dll", "ExitProcess").unwrap();
        let set = m
            .resolve_import(module, "?set_terminate@@YAP6AXXZP6AXXZ@Z")
            .unwrap();
        let mut t = Asm::new(INNER);
        t.push_imm(42).call(exit);
        store(&mut m, INNER, t);
        let mut a = Asm::new(CODE);
        a.push_imm(INNER).call(set).add_esp(4);
        raise(&mut m, module, &mut a, false);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Exited(42)), "{result:?}");
        clean(&m);
    }
}

// spec: EXC-CPP-5
#[test]
fn seh_translator_receives_original_exception_and_throws_into_cpp_catch() {
    for jit in [false, true] {
        let module = "msvcr90.dll";
        let mut m = machine(jit, module, "__CxxFrameHandler3", 0x19930522);
        let set = m
            .resolve_import(
                module,
                "?_set_se_translator@@YAP6AXIPAU_EXCEPTION_POINTERS@@@ZP6AXI0@Z@Z",
            )
            .unwrap();
        let raise_api = m.resolve_import("kernel32.dll", "RaiseException").unwrap();
        let mut t = Asm::new(INNER);
        t.bytes.extend_from_slice(&[0x8b, 0x44, 0x24, 4]);
        t.mov_mem_eax(DATA + 32);
        t.bytes
            .extend_from_slice(&[0x8b, 0x44, 0x24, 8, 0x8b, 0, 0x8b, 0]);
        t.mov_mem_eax(DATA + 36);
        raise(&mut m, module, &mut t, false);
        t.bytes.push(0xcc);
        store(&mut m, INNER, t);
        let mut a = Asm::new(CODE);
        a.push_imm(INNER).call(set).add_esp(4);
        prologue(&mut a, STUB, 2);
        a.push_imm(0)
            .push_imm(0)
            .push_imm(0)
            .push_imm(0x23456789)
            .call(raise_api)
            .bytes
            .push(0xcc);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_eq!(m.cpu.read_u32(DATA + 32).unwrap(), 0x23456789);
        assert_eq!(m.cpu.read_u32(DATA + 36).unwrap(), 0x23456789);
        assert_eq!(logs(&m), vec![2, 1, 9]);
        clean(&m);
    }
}

// spec: EXC-CPP-6
#[test]
fn processing_throw_is_true_in_cleanup_and_false_in_catch_and_independent_thread() {
    for jit in [false, true] {
        let module = "msvcr90.dll";
        let mut m = machine(jit, module, "__CxxFrameHandler3", 0x19930522);
        let query = m.resolve_import(module, "__uncaught_exception").unwrap();
        let sleep = m.resolve_import("kernel32.dll", "Sleep").unwrap();
        let mut c = Asm::new(CLEAN + 128);
        c.call(query).mov_mem_eax(DATA + 40);
        c.push_imm(0).call(sleep);
        log(&mut c, 2);
        c.ret();
        store(&mut m, CLEAN + 128, c);
        let mut c = Asm::new(CATCH);
        c.call(query).mov_mem_eax(DATA + 44);
        imm(&mut c, &[0xb8], CONT);
        c.ret();
        store(&mut m, CATCH, c);
        let mut a = Asm::new(CODE);
        prologue(&mut a, STUB, 2);
        raise(&mut m, module, &mut a, false);
        a.bytes.push(0xcc);
        let mut worker = Asm::new(INNER);
        worker
            .call(query)
            .mov_mem_eax(DATA + 48)
            .bytes
            .extend_from_slice(&[0x31, 0xc0]);
        worker.ret();
        store(&mut m, INNER, worker);
        store(&mut m, CODE, a);
        m.spawn("C++ throw owner", CODE, &[], 65536).unwrap();
        m.spawn("Independent CRT thread", INNER, &[], 65536)
            .unwrap();
        let result = m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_eq!(m.cpu.read_u32(DATA + 40).unwrap() & 255, 1);
        assert_eq!(m.cpu.read_u32(DATA + 44).unwrap() & 255, 0);
        assert_eq!(m.cpu.read_u32(DATA + 48).unwrap() & 255, 0);
        clean(&m);
    }
}

// spec: EXC-CPP-1, EXC-CPP-3
#[test]
fn virtual_base_conversion_and_nontrivial_copy_constructor_use_guest_layout() {
    for jit in [false, true] {
        for reference in [false, true] {
            let module = "msvcr90.dll";
            let mut m = machine(jit, module, "__CxxFrameHandler3", 0x19930522);
            words(
                &mut m,
                HANDLERS,
                &[if reference { 8 } else { 0 }, BASE, (-32i32) as u32, CATCH],
            );
            words(
                &mut m,
                CT,
                &[0, BASE, 0, 4, 8, 4, if reference { 0 } else { INNER }],
            );
            m.cpu.write_u32(OBJECT + 4, DATA + 0xc00).unwrap();
            m.cpu.write_u32(DATA + 0xc08, 8).unwrap();
            m.cpu.write_u32(OBJECT + 12, 0x91827364).unwrap();
            let mut copy = Asm::new(INNER);
            imm(&mut copy, &[0x89, 0x0d], DATA + 52);
            copy.bytes.extend_from_slice(&[0x8b, 0x44, 0x24, 4]);
            copy.mov_mem_eax(DATA + 56);
            copy.bytes
                .extend_from_slice(&[0x8b, 0, 0x89, 1, 0x89, 0xc8, 0xc2, 4, 0]);
            store(&mut m, INNER, copy);
            let mut a = Asm::new(CODE);
            prologue(&mut a, STUB, 2);
            raise(&mut m, module, &mut a, false);
            a.bytes.push(0xcc);
            let result = run(&mut m, a);
            assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
            assert_eq!(
                m.cpu.read_u32(DATA + 8).unwrap(),
                if reference { OBJECT + 12 } else { 0x91827364 }
            );
            if !reference {
                assert_ne!(m.cpu.read_u32(DATA + 52).unwrap(), 0);
                assert_eq!(m.cpu.read_u32(DATA + 56).unwrap(), OBJECT + 12);
            }
            clean(&m);
        }
    }
}

// spec: EXC-CPP-3
#[test]
fn new_exception_from_catch_releases_displaced_exception_before_outer_handler() {
    for jit in [false, true] {
        let module = "msvcr90.dll";
        let mut m = machine(jit, module, "__CxxFrameHandler3", 0x19930522);
        words(&mut m, FUNC, &[0x19930522, 5, UNWIND, 2, TRIES, 0, 0, 0, 0]);
        words(
            &mut m,
            TRIES,
            &[1, 2, 3, 1, HANDLERS, 0, 3, 4, 1, HANDLERS + 16],
        );
        words(&mut m, HANDLERS + 16, &[0, TYPE, (-32i32) as u32, CATCH]);
        words(&mut m, OBJECT + 32, &[0xabcdef01, 0x1234]);
        let throw = m.resolve_import(module, "_CxxThrowException").unwrap();
        let mut inner = Asm::new(CATCH + 128);
        log(&mut inner, 8);
        inner
            .push_imm(THROW)
            .push_imm(OBJECT + 32)
            .call(throw)
            .bytes
            .push(0xcc);
        store(&mut m, CATCH + 128, inner);
        m.cpu.write_u32(HANDLERS + 12, CATCH + 128).unwrap();
        let mut d = Asm::new(DESTROY);
        log(&mut d, 6);
        imm(&mut d, &[0xff, 0x05], DATA + 20);
        d.ret();
        store(&mut m, DESTROY, d);
        let mut a = Asm::new(CODE);
        prologue(&mut a, STUB, 2);
        raise(&mut m, module, &mut a, false);
        a.bytes.push(0xcc);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_eq!(logs(&m), vec![2, 8, 3, 6, 9, 6]);
        assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), 2);
        assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 0xabcdef01);
        clean(&m);
    }
}

// spec: EXC-CPP-4
#[test]
fn throws_escaping_cleanup_enter_terminate_and_throwing_terminate_aborts() {
    for jit in [false, true] {
        for in_terminate in [false, true] {
            let module = "msvcr90.dll";
            let mut m = machine(jit, module, "__CxxFrameHandler3", 0x19930522);
            let set = m
                .resolve_import(module, "?set_terminate@@YAP6AXXZP6AXXZ@Z")
                .unwrap();
            let exit = m.resolve_import("kernel32.dll", "ExitProcess").unwrap();
            let mut t = Asm::new(INNER);
            if in_terminate {
                raise(&mut m, module, &mut t, false);
            } else {
                t.push_imm(42).call(exit);
            }
            t.bytes.push(0xcc);
            store(&mut m, INNER, t);
            let mut c = Asm::new(CLEAN + 128);
            raise(&mut m, module, &mut c, false);
            c.bytes.push(0xcc);
            store(&mut m, CLEAN + 128, c);
            let mut a = Asm::new(CODE);
            a.push_imm(INNER).call(set).add_esp(4);
            if !in_terminate {
                prologue(&mut a, STUB, 2);
            }
            raise(&mut m, module, &mut a, false);
            a.bytes.push(0xcc);
            let result = run(&mut m, a);
            assert!(
                matches!(result,RunResult::Exited(code) if code==if in_terminate{3}else{42}),
                "{result:?}"
            );
            clean(&m);
        }
    }
}

// spec: EXC-CPP-1, EXC-CPP-2, EXC-CPP-3, EXC-CPP-6
#[test]
fn cleanup_can_catch_its_own_exception_while_original_throw_remains_pending() {
    for jit in [false, true] {
        let module = "msvcr90.dll";
        let mut m = machine(jit, module, "__CxxFrameHandler3", 0x19930522);
        let frame = m.resolve_import(module, "__CxxFrameHandler3").unwrap();
        let throw = m.resolve_import(module, "_CxxThrowException").unwrap();
        let query = m.resolve_import(module, "__uncaught_exception").unwrap();
        words(
            &mut m,
            INNER_FUNC,
            &[0x19930522, 2, UNWIND + 128, 1, TRIES + 64, 0, 0, 0, 0],
        );
        words(&mut m, UNWIND + 128, &[u32::MAX, 0]);
        words(&mut m, TRIES + 64, &[0, 0, 1, 1, HANDLERS + 64]);
        words(
            &mut m,
            HANDLERS + 64,
            &[0, TYPE, (-32i32) as u32, CATCH + 128],
        );
        words(&mut m, OBJECT + 32, &[0xabcdef01, 0x2345]);
        let mut s = Asm::new(STUB + 64);
        imm(&mut s, &[0xb8], INNER_FUNC);
        s.jmp(frame);
        store(&mut m, STUB + 64, s);
        let mut c = Asm::new(CLEAN + 128);
        prologue(&mut c, STUB + 64, 0);
        c.push_imm(THROW)
            .push_imm(OBJECT + 32)
            .call(throw)
            .bytes
            .push(0xcc);
        store(&mut m, CLEAN + 128, c);
        let mut c = Asm::new(CATCH + 128);
        c.call(query).mov_mem_eax(DATA + 60);
        log(&mut c, 7);
        imm(&mut c, &[0xb8], CONT + 128);
        c.ret();
        store(&mut m, CATCH + 128, c);
        let mut c = Asm::new(CONT + 128);
        c.bytes.extend_from_slice(&[
            0x8b, 0x4d, 0xf4, 0x64, 0x89, 0x0d, 0, 0, 0, 0, 0x89, 0xec, 0x5d, 0xc3,
        ]);
        store(&mut m, CONT + 128, c);
        let mut d = Asm::new(DESTROY);
        log(&mut d, 6);
        imm(&mut d, &[0xff, 0x05], DATA + 20);
        d.ret();
        store(&mut m, DESTROY, d);
        let mut a = Asm::new(CODE);
        prologue(&mut a, STUB, 2);
        raise(&mut m, module, &mut a, false);
        a.bytes.push(0xcc);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_eq!(logs(&m), vec![7, 6, 1, 9, 6]);
        assert_eq!(m.cpu.read_u32(DATA + 60).unwrap() & 255, 1);
        assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), 2);
        clean(&m);
    }
}
