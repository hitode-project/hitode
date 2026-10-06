use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Reg, asm::Asm},
};
use adv_machine::{Clock, Machine, ObjectLimits, Resume, RunResult, Wait, layout::PARK_ADDR};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

fn fixture(jit: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    super::super::register(&mut registry);
    registry.add("test", "pause", Conv::Stdcall { argc: 0 }, |e| {
        Outcome::block_then_return(Wait::Until(e.sys.clock.now_us() + 1000), 0)
    });
    registry.add("test", "done", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitProcess(0)
    });
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, DATA + 2048);
    m.spawn("ACM caller", CODE, &[], 65536).unwrap();
    m
}

fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: 0,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}

#[test]
fn driver_enumeration_preserves_arguments_stack_and_nested_waits() {
    for jit in [false, true] {
        for stop in [false, true] {
            let mut m = fixture(jit);
            let objects_before = m.sys.objects.object_count();
            let api = m.resolve_import("msacm32.dll", "acmDriverEnum").unwrap();
            let pause = m.resolve_import("test", "pause").unwrap();
            let done = m.resolve_import("test", "done").unwrap();
            let callback = CODE + 256;
            let inner_callback = CODE + 512;
            let mut main = Asm::new(CODE);
            main.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
            main.mov_mem_eax(DATA + 20)
                .push_imm(0)
                .push_imm(0xfedcba98)
                .push_imm(callback)
                .call(api)
                .mov_mem_eax(DATA + 12);
            main.bytes.extend_from_slice(&[0x89, 0xe0]);
            main.mov_mem_eax(DATA + 24).call(done).hlt();
            m.cpu.write(CODE, &main.bytes).unwrap();
            let mut cb = Asm::new(callback);
            cb.mov_eax_esp(4)
                .mov_mem_eax(DATA)
                .mov_eax_esp(8)
                .mov_mem_eax(DATA + 4)
                .mov_eax_esp(12)
                .mov_mem_eax(DATA + 8);
            cb.push_imm(ENUM_FLAGS)
                .push_imm(0)
                .push_imm(inner_callback)
                .call(api)
                .call(pause);
            cb.bytes.push(0xb8);
            cb.bytes.extend_from_slice(&u32::from(!stop).to_le_bytes());
            cb.ret_n(12);
            m.cpu.write(callback, &cb.bytes).unwrap();
            let mut inner = Asm::new(inner_callback);
            inner.mov_eax_esp(4).mov_mem_eax(DATA + 16).call(pause);
            inner.bytes.extend_from_slice(&[0x31, 0xc0]);
            inner.ret_n(12);
            m.cpu.write(inner_callback, &inner.bytes).unwrap();
            loop {
                match m.run() {
                    RunResult::Exited(0) => break,
                    RunResult::Idle {
                        next_deadline: Some(at),
                    } => m.sys.clock.advance_to(at),
                    other => panic!("ACM enumeration: {other:?}"),
                }
            }
            assert_ne!(m.cpu.read_u32(DATA).unwrap(), 0);
            assert_eq!(
                m.cpu.read_u32(DATA).unwrap(),
                m.cpu.read_u32(DATA + 16).unwrap()
            );
            assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 0xfedcba98);
            assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), SUPPORT);
            assert_eq!(m.cpu.read_u32(DATA + 12).unwrap(), 0);
            assert_eq!(
                m.cpu.read_u32(DATA + 20).unwrap(),
                m.cpu.read_u32(DATA + 24).unwrap()
            );
            assert_eq!(m.sys.objects.object_count(), objects_before + 1);
        }
    }
}

#[test]
fn driver_enumeration_validates_flags_callback_and_common_resource_limits() {
    let mut m = fixture(false);
    let objects_before = m.sys.objects.object_count();
    for callback in [0, DATA, u32::MAX] {
        assert!(matches!(
            enumerate(&mut env(&mut m), GuestPtr::new(callback), 0, 0),
            Outcome::Return(INVALID_PARAMETER)
        ));
    }
    assert!(matches!(
        enumerate(&mut env(&mut m), GuestPtr::new(CODE), 0, 1),
        Outcome::Return(INVALID_FLAG)
    ));
    for flags in [0, 0x4000_0000, 0x8000_0000, ENUM_FLAGS] {
        for _ in 0..256 {
            let Outcome::Call { args, then, .. } =
                enumerate(&mut env(&mut m), GuestPtr::new(CODE), u32::MAX, flags)
            else {
                panic!("callback")
            };
            assert_eq!(args[1..], [u32::MAX, SUPPORT]);
            assert!(matches!(
                then(&mut env(&mut m), Resume::Returned(0)),
                Outcome::Return(0)
            ));
        }
    }
    assert_eq!(m.sys.objects.object_count(), objects_before + 1);
    for limits in [
        ObjectLimits {
            objects: 0,
            ..ObjectLimits::default()
        },
        ObjectLimits {
            aliases: 0,
            ..ObjectLimits::default()
        },
        ObjectLimits {
            namespaces: 1,
            ..ObjectLimits::default()
        },
    ] {
        let mut catalog = Catalog::default();
        let mut objects = Objects::with_limits(limits);
        assert_eq!(catalog.id(&mut objects), Err(NO_MEMORY));
        assert_eq!(objects.object_count(), 0);
    }
}

#[cfg(windows)]
#[test]
fn native_driver_enumeration_callback_stop_flags_and_instance() {
    type Callback = unsafe extern "system" fn(usize, usize, u32) -> i32;
    #[link(name = "msacm32")]
    unsafe extern "system" {
        fn acmDriverEnum(callback: Option<Callback>, instance: usize, flags: u32) -> u32;
    }
    struct Probe {
        calls: usize,
        support: u32,
        id: usize,
        continue_enum: bool,
    }
    unsafe extern "system" fn callback(id: usize, instance: usize, support: u32) -> i32 {
        let probe = unsafe { &mut *(instance as *mut Probe) };
        probe.calls += 1;
        probe.support = support;
        probe.id = id;
        i32::from(probe.continue_enum)
    }
    unsafe {
        assert_eq!(acmDriverEnum(None, 0, 0), INVALID_PARAMETER);
        let mut p = Probe {
            calls: 0,
            support: 0,
            id: 0,
            continue_enum: false,
        };
        let instance = (&mut p as *mut Probe) as usize;
        assert_eq!(acmDriverEnum(Some(callback), instance, 1), INVALID_FLAG);
        assert_eq!(p.calls, 0);
        for flags in [0, 0x4000_0000, 0x8000_0000, ENUM_FLAGS] {
            p.calls = 0;
            assert_eq!(acmDriverEnum(Some(callback), instance, flags), 0);
            assert_eq!(p.calls, 1); // This fixture host has installed ACM drivers.
            assert_ne!(p.id, 0);
        }
        p.calls = 0;
        p.continue_enum = true;
        assert_eq!(acmDriverEnum(Some(callback), instance, 0), 0);
        assert!(p.calls >= 1);
    }
}
