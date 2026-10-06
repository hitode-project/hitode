use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, RunResult, Wait, layout::PARK_ADDR};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

#[test]
fn primary_enumeration_calls_guest_preserves_stack_and_survives_nested_waits() {
    for jit in [false, true] {
        for wide in [false, true] {
            for stop in [0, 1] {
                let mut registry = Registry::default();
                register(&mut registry);
                registry.add("test", "probe", Conv::Stdcall { argc: 4 }, |e| {
                    assert_eq!(e.arg(0), 0);
                    assert_eq!(e.arg(3), DATA);
                    let read = |p| {
                        if *e.ext.get::<bool>() {
                            String::from_utf16_lossy(
                                &adv_abi::WideStr(GuestPtr::new(p)).read(e.cpu, 256).unwrap(),
                            )
                        } else {
                            String::from_utf8(e.cpu.read_cstr(p, 256).unwrap()).unwrap()
                        }
                    };
                    assert_eq!(read(e.arg(1)), DESCRIPTION);
                    assert_eq!(read(e.arg(2)), DRIVER);
                    e.cpu.write_u32(DATA + 4, e.arg(1)).unwrap();
                    e.cpu.write_u32(DATA + 8, e.arg(2)).unwrap();
                    e.cpu
                        .write_u32(DATA + 28, e.cpu.read_u32(DATA + 28).unwrap() + 1)
                        .unwrap();
                    Outcome::Return(0)
                });
                registry.add("test", "wait", Conv::Stdcall { argc: 0 }, |e| {
                    Outcome::block_then_return(Wait::Until(e.sys.clock.now_us() + 1000), 0)
                });
                registry.add("test", "done", Conv::Stdcall { argc: 0 }, |_| {
                    Outcome::ExitThread(0)
                });
                assert!(registry.signature_conflicts().is_empty());
                let mut m = Machine::new(
                    JitCpu::new(PARK_ADDR, jit).unwrap(),
                    registry,
                    Clock::virtual_time(),
                )
                .unwrap();
                m.ext.insert(wide);
                m.cpu.map(CODE, 4096, Prot::RX).unwrap();
                m.cpu.map(DATA, 4096, Prot::RW).unwrap();
                let api = m
                    .resolve_import(
                        "ddraw.dll",
                        if wide {
                            "DirectDrawEnumerateW"
                        } else {
                            "DirectDrawEnumerateA"
                        },
                    )
                    .unwrap();
                let probe = m.resolve_import("test", "probe").unwrap();
                let wait = m.resolve_import("test", "wait").unwrap();
                let done = m.resolve_import("test", "done").unwrap();
                let callback = CODE + 256;
                let inner_callback = CODE + 512;
                let mut main = Asm::new(CODE);
                main.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
                main.mov_mem_eax(DATA + 20)
                    .push_imm(DATA)
                    .push_imm(callback)
                    .call(api)
                    .mov_mem_eax(DATA + 12);
                main.bytes.extend_from_slice(&[0x89, 0xe0]);
                main.mov_mem_eax(DATA + 24).call(done).hlt();
                m.cpu.write(CODE, &main.bytes).unwrap();
                let mut cb = Asm::new(callback);
                // Forward the caller's arguments to the inspecting host thunk.
                cb.mov_eax_esp(16)
                    .push_eax()
                    .mov_eax_esp(16)
                    .push_eax()
                    .mov_eax_esp(16)
                    .push_eax()
                    .mov_eax_esp(16)
                    .push_eax()
                    .call(probe)
                    .push_imm(0)
                    .push_imm(inner_callback)
                    .call(api)
                    .call(wait);
                // Read strings again after nested enumeration and a guest wait.
                cb.mov_eax_esp(16)
                    .push_eax()
                    .mov_eax_esp(16)
                    .push_eax()
                    .mov_eax_esp(16)
                    .push_eax()
                    .mov_eax_esp(16)
                    .push_eax()
                    .call(probe)
                    .mov_eax_mem(DATA + 16)
                    .ret_n(16);
                m.cpu.write(callback, &cb.bytes).unwrap();
                let mut inner = Asm::new(inner_callback);
                inner.call(wait).ret_n(16);
                m.cpu.write(inner_callback, &inner.bytes).unwrap();
                m.cpu.write_u32(DATA + 16, stop).unwrap();
                m.spawn("enumeration", CODE, &[], 65536).unwrap();
                loop {
                    match m.run() {
                        RunResult::Exited(_) => break,
                        RunResult::Idle {
                            next_deadline: Some(t),
                        } => m.sys.clock.advance_to(t),
                        other => panic!("unexpected enumeration result: {other:?}"),
                    }
                }
                assert_eq!(m.cpu.read_u32(DATA + 12).unwrap(), 0);
                assert_eq!(
                    m.cpu.read_u32(DATA + 20).unwrap(),
                    m.cpu.read_u32(DATA + 24).unwrap()
                );
                assert_eq!(m.cpu.read_u32(DATA + 28).unwrap(), 2);
            }
        }
    }
}

#[test]
fn null_or_non_executable_callbacks_and_short_stack_reject() {
    let mut registry = Registry::default();
    register(&mut registry);
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, DATA + 128);
    for callback in [0, DATA, 0xdeadbeef] {
        let mut e = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        assert!(matches!(
            enumerate(&mut e, callback, 0, false),
            Outcome::Return(INVALID)
        ));
    }
    m.cpu.set_reg(Reg::Esp, 8);
    let mut e = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    assert!(matches!(
        enumerate(&mut e, PARK_ADDR, 0, false),
        Outcome::Return(INVALID)
    ));
}
