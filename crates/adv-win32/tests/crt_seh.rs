//! Authored x86 scope tables exercise CRT filters, finally blocks and EH4
//! security callbacks through the real Windows dispatcher and RtlUnwind.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const INNER: u32 = CODE + 0x1000;
const FILTER: u32 = CODE + 0x2000;
const CATCH: u32 = CODE + 0x2400;
const FINALLY: u32 = CODE + 0x2800;
const WRAPPER: u32 = CODE + 0x3000;
const CHECK: u32 = CODE + 0x3400;
const DATA: u32 = 0x410000;
const COUNT: u32 = DATA;
const LOG: u32 = DATA + 16;
const SEEN_EBP: u32 = DATA + 64;
const FILTER_COUNT: u32 = DATA + 128;
const SEEN_CODE: u32 = DATA + 132;
const NORMAL: u32 = DATA + 136;
const OUTER_FRAME: u32 = DATA + 160;
const INNER_FRAME: u32 = DATA + 164;
const COOKIE: u32 = DATA + 192;
const COOKIE_COUNT: u32 = DATA + 196;
const COOKIE_SEEN: u32 = DATA + 200;
const TABLE: u32 = DATA + 0x400;
const INNER_TABLE: u32 = DATA + 0x500;
const LOCAL_FRAME: u32 = DATA + 0x1000;
const KEY: u32 = 0x31415926;

fn imm(code: &mut Asm, op: &[u8], value: u32) {
    code.bytes.extend_from_slice(op);
    code.bytes.extend_from_slice(&value.to_le_bytes());
}
fn log(code: &mut Asm, id: u32) {
    code.mov_eax_mem(COUNT)
        .bytes
        .extend_from_slice(&[0x89, 0xc1, 0xc1, 0xe1, 2]);
    imm(code, &[0xc7, 0x81], LOG);
    code.bytes.extend_from_slice(&id.to_le_bytes());
    imm(code, &[0xff, 0x05], COUNT);
    imm(code, &[0x89, 0x2d], SEEN_EBP + id * 4);
}
fn epilogue(code: &mut Asm) {
    code.bytes.extend_from_slice(&[
        0x8b, 0x4d, 0xf0, 0x64, 0x89, 0x0d, 0, 0, 0, 0, 0x89, 0xec, 0x5d, 0x31, 0xc0, 0xc3,
    ]);
}
fn prologue(
    code: &mut Asm,
    handler: u32,
    table: u32,
    level: u32,
    saved: u32,
    four: bool,
    bad: bool,
) {
    code.bytes.extend_from_slice(&[0x55, 0x89, 0xe5]);
    code.push_imm(level)
        .push_imm(if four { table ^ KEY } else { table })
        .push_imm(handler);
    code.bytes.extend_from_slice(&[
        0x64,
        0xff,
        0x35,
        0,
        0,
        0,
        0,
        0x83,
        0xec,
        if four { 16 } else { 8 },
        0x8d,
        0x45,
        0xf0,
    ]);
    code.mov_mem_eax(saved);
    code.bytes
        .extend_from_slice(&[0x64, 0xa3, 0, 0, 0, 0, 0x89, 0x65, 0xe8]);
    if four {
        imm(code, &[0xb8], KEY);
        code.bytes
            .extend_from_slice(&[0x31, 0xe8, 0x89, 0x45, 0xe4, 0x89, 0x45, 0xe0]);
        if bad {
            code.bytes.extend_from_slice(&[0x83, 0x75, 0xe4, 1]);
        }
    }
}
fn machine(jit: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    let unwind = registry.get("kernel32.dll", "RtlUnwind").unwrap();
    registry.add(
        "fixture.dll",
        "original_unwind",
        unwind.conv,
        unwind.handler,
    );
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/crt-seh",
            "fixture.exe",
            "/nonexistent/crt-seh-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 16384, Prot::RW).unwrap();
    m.cpu.write_u32(COOKIE, KEY).unwrap();
    for id in 1..=4 {
        let mut code = Asm::new(FINALLY + id * 64);
        log(&mut code, id);
        code.ret();
        m.cpu.write(FINALLY + id * 64, &code.bytes).unwrap();
    }
    let mut catch = Asm::new(CATCH);
    log(&mut catch, 9);
    epilogue(&mut catch);
    m.cpu.write(CATCH, &catch.bytes).unwrap();
    m
}
fn scope(m: &mut Machine<JitCpu>, base: u32, index: u32, previous: i32, filter: u32, handler: u32) {
    for (offset, value) in [previous as u32, filter, handler].into_iter().enumerate() {
        m.cpu
            .write_u32(base + index * 12 + offset as u32 * 4, value)
            .unwrap();
    }
}
fn filter(m: &mut Machine<JitCpu>, result: u32) {
    let mut code = Asm::new(FILTER);
    imm(&mut code, &[0xff, 0x05], FILTER_COUNT);
    code.bytes
        .extend_from_slice(&[0x8b, 0x45, 0xec, 0x8b, 0, 0x8b, 0]);
    code.mov_mem_eax(SEEN_CODE);
    imm(&mut code, &[0xb8], result);
    code.ret();
    m.cpu.write(FILTER, &code.bytes).unwrap();
}
fn run(m: &mut Machine<JitCpu>, code: Asm) -> RunResult {
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("CRT structured exception", CODE, &[], 65536)
        .unwrap();
    m.run_virtual(1_000_000)
}
fn no_gap(m: &Machine<JitCpu>) {
    if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
        assert_eq!(d.stub_calls, 0);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }
}
fn raise(m: &mut Machine<JitCpu>, code: &mut Asm) {
    let api = m.resolve_import("kernel32.dll", "RaiseException").unwrap();
    code.push_imm(0)
        .push_imm(0)
        .push_imm(0)
        .push_imm(0x23456789)
        .call(api);
}
fn handler4(m: &mut Machine<JitCpu>, module: &str) -> u32 {
    let api = m.resolve_import(module, "_except_handler4_common").unwrap();
    let mut code = Asm::new(WRAPPER);
    for _ in 0..4 {
        code.bytes.extend_from_slice(&[0xff, 0x74, 0x24, 16]);
    }
    code.push_imm(CHECK)
        .push_imm(COOKIE)
        .call(api)
        .add_esp(24)
        .ret();
    m.cpu.write(WRAPPER, &code.bytes).unwrap();
    let exit = m.resolve_import("kernel32.dll", "ExitProcess").unwrap();
    let mut check = Asm::new(CHECK);
    imm(&mut check, &[0x89, 0x0d], COOKIE_SEEN);
    imm(&mut check, &[0xff, 0x05], COOKIE_COUNT);
    imm(&mut check, &[0x3b, 0x0d], COOKIE);
    check.bytes.extend_from_slice(&[0x74, 10]);
    check.push_imm(0xc0000409).call(exit).ret();
    m.cpu.write(CHECK, &check.bytes).unwrap();
    WRAPPER
}

// spec: EXC-CRT-3, PROC-MACH-11
#[test]
fn filters_continue_execution_or_enter_catch_after_ordered_local_finally_blocks() {
    for jit in [false, true] {
        for module in ["msvcrt.dll", "msvcr90.dll", "vcruntime140.dll"] {
            for name in ["_except_handler2", "_except_handler3"] {
                for accepted in [false, true] {
                    let mut m = machine(jit);
                    let handler = m.resolve_import(module, name).unwrap();
                    filter(&mut m, if accepted { 1 } else { u32::MAX });
                    scope(&mut m, TABLE, 0, -1, FILTER, CATCH);
                    scope(&mut m, TABLE, 1, 0, 0, FINALLY + 64);
                    scope(&mut m, TABLE, 2, 1, 0, FINALLY + 128);
                    let mut code = Asm::new(CODE);
                    prologue(&mut code, handler, TABLE, 2, OUTER_FRAME, false, false);
                    raise(&mut m, &mut code);
                    imm(&mut code, &[0xc7, 0x05], NORMAL);
                    code.bytes.extend_from_slice(&1u32.to_le_bytes());
                    epilogue(&mut code);
                    let result = run(&mut m, code);
                    assert!(
                        matches!(result, RunResult::Exited(0)),
                        "{module}!{name}, accepted={accepted}, jit={jit}: {result:?}"
                    );
                    assert_eq!(m.cpu.read_u32(FILTER_COUNT).unwrap(), 1);
                    assert_eq!(m.cpu.read_u32(SEEN_CODE).unwrap(), 0x23456789);
                    assert_eq!(m.cpu.read_u32(NORMAL).unwrap(), u32::from(!accepted));
                    assert_eq!(m.cpu.read_u32(COUNT).unwrap(), if accepted { 3 } else { 0 });
                    if accepted {
                        for (i, id) in [2, 1, 9].into_iter().enumerate() {
                            assert_eq!(m.cpu.read_u32(LOG + i as u32 * 4).unwrap(), id);
                        }
                        let frame = m.cpu.read_u32(OUTER_FRAME).unwrap();
                        for id in [1, 2, 9] {
                            assert_eq!(m.cpu.read_u32(SEEN_EBP + id * 4).unwrap(), frame + 16);
                        }
                        assert_eq!(m.cpu.read_u32(frame + 12).unwrap(), u32::MAX);
                    }
                    no_gap(&m);
                }
            }
        }
    }
}

// spec: EXC-CRT-3, PROC-MACH-11
#[test]
fn catching_across_frames_uses_global_unwind_before_the_outer_finally_and_catch() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let handler = m.resolve_import("msvcr90.dll", "_except_handler3").unwrap();
        filter(&mut m, 1);
        scope(&mut m, TABLE, 0, -1, FILTER, CATCH);
        scope(&mut m, TABLE, 1, 0, 0, FINALLY + 64);
        scope(&mut m, INNER_TABLE, 0, -1, 0, FINALLY + 192);
        m.cpu.write(FILTER + 256, &[0x31, 0xc0, 0xc3]).unwrap();
        scope(&mut m, INNER_TABLE, 1, 0, FILTER + 256, CATCH);
        scope(&mut m, INNER_TABLE, 2, 1, 0, FINALLY + 256);
        let mut inner = Asm::new(INNER);
        prologue(
            &mut inner,
            handler,
            INNER_TABLE,
            2,
            INNER_FRAME,
            false,
            false,
        );
        raise(&mut m, &mut inner);
        epilogue(&mut inner);
        m.cpu.write(INNER, &inner.bytes).unwrap();
        let mut outer = Asm::new(CODE);
        prologue(&mut outer, handler, TABLE, 1, OUTER_FRAME, false, false);
        outer.call(INNER);
        epilogue(&mut outer);
        let result = run(&mut m, outer);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "jit={jit}: {result:?}"
        );
        assert_eq!(m.cpu.read_u32(COUNT).unwrap(), 4);
        for (i, id) in [4, 3, 1, 9].into_iter().enumerate() {
            assert_eq!(m.cpu.read_u32(LOG + i as u32 * 4).unwrap(), id);
        }
        let inner = m.cpu.read_u32(INNER_FRAME).unwrap();
        let outer = m.cpu.read_u32(OUTER_FRAME).unwrap();
        for (id, frame) in [(4, inner), (3, inner), (1, outer), (9, outer)] {
            assert_eq!(m.cpu.read_u32(SEEN_EBP + id * 4).unwrap(), frame + 16);
        }
        no_gap(&m);
    }
}

// spec: EXC-CRT-3
#[test]
fn local_unwind_helpers_stop_at_the_requested_state_and_do_not_repeat_finally() {
    for jit in [false, true] {
        for four in [false, true] {
            let mut m = machine(jit);
            let base = TABLE + if four { 16 } else { 0 };
            scope(&mut m, base, 0, if four { -2 } else { -1 }, 0, FINALLY + 64);
            scope(&mut m, base, 1, 0, FILTER, CATCH);
            scope(&mut m, base, 2, 1, 0, FINALLY + 128);
            m.cpu
                .write_u32(LOCAL_FRAME + 8, if four { TABLE ^ KEY } else { TABLE })
                .unwrap();
            m.cpu.write_u32(LOCAL_FRAME + 12, 2).unwrap();
            let api = m
                .resolve_import(
                    "msvcr90.dll",
                    if four {
                        "_local_unwind4"
                    } else {
                        "_local_unwind2"
                    },
                )
                .unwrap();
            let mut code = Asm::new(CODE);
            for stop in [
                0,
                if four { u32::MAX - 1 } else { u32::MAX },
                if four { u32::MAX - 1 } else { u32::MAX },
            ] {
                code.push_imm(stop).push_imm(LOCAL_FRAME);
                if four {
                    code.push_imm(COOKIE);
                }
                code.call(api).add_esp(if four { 12 } else { 8 });
            }
            code.bytes.extend_from_slice(&[0x31, 0xc0]);
            code.ret();
            assert!(matches!(run(&mut m, code), RunResult::Exited(0)));
            assert_eq!(m.cpu.read_u32(COUNT).unwrap(), 2);
            assert_eq!(m.cpu.read_u32(LOG).unwrap(), 2);
            assert_eq!(m.cpu.read_u32(LOG + 4).unwrap(), 1);
            assert_eq!(
                m.cpu.read_u32(LOCAL_FRAME + 12).unwrap(),
                if four { u32::MAX - 1 } else { u32::MAX }
            );
            no_gap(&m);
        }
    }
}

// spec: EXC-CRT-3, PERS-BRIDGE-1
#[test]
fn global_unwind_helper_observes_the_live_public_entry_and_leaves_the_target_registered() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let public = m.resolve_import("kernel32.dll", "RtlUnwind").unwrap();
        let original = m.resolve_import("fixture.dll", "original_unwind").unwrap();
        let mut hook = Asm::new(public);
        imm(&mut hook, &[0xff, 0x05], COOKIE_COUNT);
        hook.jmp(original);
        m.cpu.write(public, &hook.bytes).unwrap();
        let handler = m.resolve_import("msvcr90.dll", "_except_handler3").unwrap();
        let global = m.resolve_import("msvcr90.dll", "_global_unwind2").unwrap();
        scope(&mut m, TABLE, 0, -1, 0, FINALLY + 64);
        scope(&mut m, INNER_TABLE, 0, -1, 0, FINALLY + 192);
        let mut inner = Asm::new(INNER);
        prologue(
            &mut inner,
            handler,
            INNER_TABLE,
            0,
            INNER_FRAME,
            false,
            false,
        );
        imm(&mut inner, &[0xff, 0x35], OUTER_FRAME);
        inner.call(global).add_esp(4);
        inner.mov_eax_fs(0).mov_mem_eax(DATA + 220);
        log(&mut inner, 4);
        epilogue(&mut inner);
        m.cpu.write(INNER, &inner.bytes).unwrap();
        let mut code = Asm::new(CODE);
        prologue(&mut code, handler, TABLE, 0, OUTER_FRAME, false, false);
        code.call(INNER);
        log(&mut code, 9);
        epilogue(&mut code);
        let result = run(&mut m, code);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "jit={jit}: {result:?}"
        );
        assert_eq!(m.cpu.read_u32(COOKIE_COUNT).unwrap(), 1);
        assert_eq!(
            m.cpu.read_u32(DATA + 220).unwrap(),
            m.cpu.read_u32(OUTER_FRAME).unwrap()
        );
        assert_eq!(m.cpu.read_u32(COUNT).unwrap(), 3);
        for (i, id) in [3, 4, 9].into_iter().enumerate() {
            assert_eq!(m.cpu.read_u32(LOG + i as u32 * 4).unwrap(), id);
        }
        no_gap(&m);
    }
}

// spec: EXC-CRT-4, PROC-MACH-11
#[test]
fn eh4_decodes_scope_tables_and_calls_real_security_checks_before_filtering() {
    for jit in [false, true] {
        for bad in [false, true] {
            let mut m = machine(jit);
            let handler = handler4(&mut m, "vcruntime140.dll");
            filter(&mut m, 1);
            for (i, value) in [-28i32, 0, -32, 0].into_iter().enumerate() {
                m.cpu.write_u32(TABLE + i as u32 * 4, value as u32).unwrap();
            }
            scope(&mut m, TABLE + 16, 0, -2, FILTER, CATCH);
            let mut code = Asm::new(CODE);
            prologue(&mut code, handler, TABLE, 0, OUTER_FRAME, true, bad);
            raise(&mut m, &mut code);
            epilogue(&mut code);
            let result = run(&mut m, code);
            assert!(
                matches!(result, RunResult::Exited(code) if code == if bad { 0xc0000409 } else { 0 }),
                "bad={bad}, jit={jit}: {result:?}"
            );
            assert_eq!(
                m.cpu.read_u32(COOKIE_COUNT).unwrap(),
                if bad { 1 } else { 2 }
            );
            assert_eq!(m.cpu.read_u32(COOKIE_SEEN).unwrap(), KEY ^ u32::from(bad));
            assert_eq!(m.cpu.read_u32(FILTER_COUNT).unwrap(), u32::from(!bad));
            no_gap(&m);
        }
    }
}

// spec: EXC-CPP-7, EXC-CRT-3
#[test]
fn longjmp_uses_live_global_unwind_then_default_local_seh_cleanup() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let module = "msvcr90.dll";
        let handler = m.resolve_import(module, "_except_handler3").unwrap();
        let setjmp = m.resolve_import(module, "_setjmp3").unwrap();
        let longjmp = m.resolve_import(module, "longjmp").unwrap();
        let buffer = DATA + 0x1200;
        scope(&mut m, TABLE, 0, -1, FILTER, CATCH);
        scope(&mut m, TABLE, 1, 0, 0, FINALLY + 64);
        scope(&mut m, TABLE, 2, 1, 0, FINALLY + 128);
        scope(&mut m, INNER_TABLE, 0, -1, 0, FINALLY + 192);
        scope(&mut m, INNER_TABLE, 1, 0, 0, FINALLY + 256);
        let mut inner = Asm::new(INNER);
        prologue(
            &mut inner,
            handler,
            INNER_TABLE,
            1,
            INNER_FRAME,
            false,
            false,
        );
        inner
            .push_imm(0)
            .push_imm(buffer)
            .call(longjmp)
            .bytes
            .push(0xcc);
        m.cpu.write(INNER, &inner.bytes).unwrap();
        let mut a = Asm::new(CODE);
        prologue(&mut a, handler, TABLE, 0, OUTER_FRAME, false, false);
        a.push_imm(0)
            .push_imm(buffer)
            .call(setjmp)
            .add_esp(8)
            .mov_mem_eax(NORMAL);
        a.bytes.extend_from_slice(&[0x85, 0xc0, 0x0f, 0x85]);
        let relative = CATCH.wrapping_sub(CODE + a.bytes.len() as u32 + 4);
        a.bytes.extend_from_slice(&relative.to_le_bytes());
        a.bytes.extend_from_slice(&[0xc7, 0x45, 0xfc, 2, 0, 0, 0]);
        a.call(INNER).bytes.push(0xcc);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_eq!(m.cpu.read_u32(NORMAL).unwrap(), 1);
        let events: Vec<_> = (0..m.cpu.read_u32(COUNT).unwrap())
            .map(|i| m.cpu.read_u32(LOG + i * 4).unwrap())
            .collect();
        assert_eq!(events, vec![4, 3, 2, 1, 9]);
        no_gap(&m);
    }
}
