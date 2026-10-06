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

#[test]
fn driver_details_queries_sizes_encoding_and_protected_outputs() {
    let mut m = fixture(false);
    ensure(&mut env(&mut m));
    let id = m
        .ext
        .get_mut::<Acm>()
        .drivers
        .id(&mut m.sys.objects)
        .unwrap();
    for wide in [false, true] {
        let full: u32 = if wide { 1800 } else { 920 };
        for size in [0, 3, 4, 8, 40, full - 1, full, full + 20] {
            let mut sentinel = vec![0xa5; full as usize + 32];
            sentinel[..4].copy_from_slice(&size.to_le_bytes());
            m.cpu.write(DATA, &sentinel).unwrap();
            assert_eq!(
                details(&mut env(&mut m), id, DATA, 0, wide),
                if size < 4 {
                    Err(INVALID_PARAMETER)
                } else {
                    Ok(())
                }
            );
            let mut actual = sentinel.clone();
            m.cpu.read(DATA, &mut actual).unwrap();
            if size < 4 {
                assert_eq!(actual, sentinel);
            } else {
                let written = size.min(full) as usize;
                assert_eq!(m.cpu.read_u32(DATA).unwrap(), written as u32);
                assert_eq!(actual[written..], sentinel[written..]);
                if written >= 40 {
                    assert_eq!(&actual[4..8], b"audc");
                    assert_eq!(m.cpu.read_u32(DATA + 24).unwrap(), SUPPORT);
                    assert_eq!(m.cpu.read_u32(DATA + 28).unwrap(), 3);
                    assert_eq!(m.cpu.read_u32(DATA + 32).unwrap(), 0);
                }
                if written >= full as usize {
                    let name = if wide {
                        actual[40..104]
                            .chunks_exact(2)
                            .map(|b| u16::from_le_bytes([b[0], b[1]]))
                            .take_while(|u| *u != 0)
                            .map(|u| char::from_u32(u32::from(u)).unwrap())
                            .collect::<String>()
                    } else {
                        String::from_utf8(
                            actual[40..72]
                                .iter()
                                .copied()
                                .take_while(|b| *b != 0)
                                .collect(),
                        )
                        .unwrap()
                    };
                    assert_eq!(name, "Symphonia");
                }
            }
        }
    }
    assert_eq!(
        details(&mut env(&mut m), id, DATA, 1, false),
        Err(INVALID_FLAG)
    );
    assert_eq!(
        details(&mut env(&mut m), id + 4, DATA, 0, false),
        Err(INVALID_HANDLE)
    );
    assert_eq!(
        details(&mut env(&mut m), id, 0, 0, false),
        Err(INVALID_PARAMETER)
    );
    // A trailing inaccessible page is rejected before any partial writes.
    m.cpu.write_u32(DATA + 4096 - 40, 920).unwrap();
    assert_eq!(
        details(&mut env(&mut m), id, DATA + 4096 - 40, 0, false),
        Err(INVALID_PARAMETER)
    );
    assert_eq!(m.cpu.read_u32(DATA + 4096 - 40).unwrap(), 920);
    m.cpu.write_u32(DATA, 920).unwrap();
    m.cpu.protect(DATA, 4096, Prot::READ).unwrap();
    assert_eq!(
        details(&mut env(&mut m), id, DATA, 0, false),
        Err(INVALID_PARAMETER)
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 920);
}

#[test]
fn driver_metrics_match_catalog_and_distinguish_null_from_stale_ids() {
    let mut m = fixture(false);
    ensure(&mut env(&mut m));
    let id = m
        .ext
        .get_mut::<Acm>()
        .drivers
        .id(&mut m.sys.objects)
        .unwrap();
    for index in (1..=6).chain(20..=24) {
        assert_eq!(metric(&mut env(&mut m), 0, index, DATA), Ok(()));
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), u32::from(index <= 3));
        assert_eq!(
            metric(&mut env(&mut m), id, index, DATA),
            Err(INVALID_HANDLE)
        );
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0);
    }
    for (object, index, expected) in [(0, 50, 50), (id, 50, 50), (id, 100, SUPPORT), (id, 101, 1)] {
        assert_eq!(metric(&mut env(&mut m), object, index, DATA), Ok(()));
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), expected);
    }
    for (object, index, expected, output) in [
        (0, 100, INVALID_HANDLE, 0),
        (id + 4, 101, INVALID_HANDLE, 0xfedcba98),
        (id + 4, 50, INVALID_HANDLE, 0xfedcba98),
        (id, 30, INVALID_HANDLE, u32::MAX),
        (id, 31, INVALID_HANDLE, u32::MAX),
        (id, 51, NOT_POSSIBLE, 0),
    ] {
        m.cpu.write_u32(DATA, 0xfedcba98).unwrap();
        assert_eq!(metric(&mut env(&mut m), object, index, DATA), Err(expected));
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), output);
    }
    assert_eq!(metric(&mut env(&mut m), id, 101, 0), Err(INVALID_PARAMETER));
    m.cpu.write_u32(DATA, 0xfedcba98).unwrap();
    m.cpu.protect(DATA, 4096, Prot::READ).unwrap();
    assert_eq!(
        metric(&mut env(&mut m), id, 101, DATA),
        Err(INVALID_PARAMETER)
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0xfedcba98);
}

#[cfg(windows)]
#[test]
fn native_driver_metrics_distinguish_null_from_unknown_ids() {
    #[link(name = "msacm32")]
    unsafe extern "system" {
        fn acmMetrics(object: usize, index: u32, output: *mut u32) -> u32;
    }
    unsafe {
        for (object, index) in [(0, 101), (0x1234, 101), (0x1234, 50)] {
            let mut value = 0xfedcba98;
            assert_eq!(acmMetrics(object, index, &mut value), INVALID_HANDLE);
            assert_eq!(value, if object == 0 { 0 } else { 0xfedcba98 });
        }
    }
}

#[cfg(windows)]
#[test]
fn native_driver_details_allow_partial_and_clamp_extended_structures() {
    type Callback = unsafe extern "system" fn(usize, usize, u32) -> i32;
    #[link(name = "msacm32")]
    unsafe extern "system" {
        fn acmDriverEnum(callback: Option<Callback>, instance: usize, flags: u32) -> u32;
        fn acmDriverDetailsA(id: usize, output: *mut u8, flags: u32) -> u32;
        fn acmDriverDetailsW(id: usize, output: *mut u8, flags: u32) -> u32;
    }
    unsafe extern "system" fn callback(id: usize, instance: usize, _: u32) -> i32 {
        unsafe {
            *(instance as *mut usize) = id;
        }
        0
    }
    unsafe {
        let mut id = 0;
        assert_eq!(
            acmDriverEnum(Some(callback), (&mut id as *mut usize) as usize, 0),
            0
        );
        assert_ne!(id, 0);
        for (query, full) in [
            (
                acmDriverDetailsA as unsafe extern "system" fn(usize, *mut u8, u32) -> u32,
                916 + std::mem::size_of::<usize>(),
            ),
            (acmDriverDetailsW, 1796 + std::mem::size_of::<usize>()),
        ] {
            for size in [0, 3, 4, 8, 40, full - 1, full, full + 20] {
                // Native W's ANSI adapter inspects fields beyond a short
                // cbStruct. Initialize the supplied structure as normal callers
                // do; A has no such dependency on untouched fields.
                let mut bytes = vec![if full > 1000 { 0 } else { 0xa5 }; full + 32];
                bytes[..4].copy_from_slice(&(size as u32).to_le_bytes());
                let before = bytes.clone();
                assert_eq!(
                    query(id, bytes.as_mut_ptr(), 0),
                    if size < 4 { INVALID_PARAMETER } else { 0 }
                );
                if size < 4 {
                    assert_eq!(bytes, before);
                } else {
                    let written = size.min(full);
                    assert_eq!(
                        u32::from_le_bytes(bytes[..4].try_into().unwrap()),
                        written as u32
                    );
                    assert_eq!(bytes[written..], before[written..]);
                    if written >= 8 {
                        assert_eq!(&bytes[4..8], b"audc");
                    }
                }
            }
            let mut bytes = vec![0xa5; full];
            bytes[..4].copy_from_slice(&(full as u32).to_le_bytes());
            assert_eq!(query(id, bytes.as_mut_ptr(), 1), INVALID_FLAG);
            assert_eq!(query(0, bytes.as_mut_ptr(), 0), INVALID_HANDLE);
            assert_eq!(query(id, std::ptr::null_mut(), 0), INVALID_PARAMETER);
        }
    }
}
