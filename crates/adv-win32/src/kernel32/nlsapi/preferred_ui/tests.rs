use super::*;
use crate::{Win32, Win32Config};
use adv_abi::Conv;
use adv_cpu::{JitCpu, backend::Guest};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};

const DATA: u32 = 0x410000;
const COUNT: u32 = DATA;
const SIZE: u32 = DATA + 4;
const BUFFER: u32 = DATA + 32;
const SENTINEL: u32 = 0xcccccccc;
const ERROR: u32 = 0x12345678;

fn machine(cp: u32) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for scope in [Scope::Thread, Scope::Process, Scope::User, Scope::System] {
        let name = scope.api().split_once('!').unwrap().1;
        assert_eq!(
            registry.get("kernel32.dll", name).unwrap().conv,
            Conv::Stdcall { argc: 4 }
        );
        assert!(registry.dynamic_available("kernel32.dll", name));
    }
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let mut cfg = Win32Config::new("/nonexistent/mui", "fixture.exe", "/nonexistent/mui-data");
    cfg.os.codepage = cp;
    Win32::install(&mut machine, cfg).unwrap();
    machine
        .spawn("MUI contract", PARK_ADDR, &[], 65536)
        .unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn request(
    machine: &mut Machine<JitCpu>,
    scope: Scope,
    flags: u32,
    buffer: u32,
    capacity: u32,
) -> i32 {
    machine.cpu.write_u32(COUNT, SENTINEL).unwrap();
    machine.cpu.write_u32(SIZE, capacity).unwrap();
    machine.cpu.write(BUFFER, &[0xcc; 32]).unwrap();
    crate::util::set_last_error(&mut env(machine), ERROR);
    query(
        &mut env(machine),
        scope,
        flags,
        GuestPtr::new(COUNT),
        WideStr(GuestPtr::new(buffer)),
        GuestPtr::new(SIZE),
    )
}
fn words(machine: &Machine<JitCpu>) -> (u32, u32) {
    (
        machine.cpu.read_u32(COUNT).unwrap(),
        machine.cpu.read_u32(SIZE).unwrap(),
    )
}

#[test]
fn profile_queries_use_utf16_multisz_and_native_empty_list_outputs() {
    for (cp, name, id) in [(932, "ja-JP", "0411"), (936, "zh-CN", "0804")] {
        let mut m = machine(cp);
        for scope in [Scope::Thread, Scope::User, Scope::System, Scope::Process] {
            for (flags, value) in [(0, name), (LANGUAGE_NAME, name), (LANGUAGE_ID, id)] {
                let empty = scope == Scope::Process;
                let expected: Vec<u16> = if empty {
                    vec![0, 0]
                } else {
                    value.encode_utf16().chain([0, 0]).collect()
                };
                let count = if empty { SENTINEL } else { 1 };
                assert_eq!(request(&mut m, scope, flags, 0, 0), 1);
                assert_eq!(words(&m), (count, expected.len() as u32));
                assert_eq!(crate::util::last_error(&env(&mut m)), ERROR);
                assert_eq!(request(&mut m, scope, flags, BUFFER, 16), 1);
                assert_eq!(words(&m), (count, expected.len() as u32));
                let mut actual = vec![0; expected.len() * 2];
                m.cpu.read(BUFFER, &mut actual).unwrap();
                assert_eq!(
                    actual,
                    expected
                        .iter()
                        .flat_map(|u| u.to_le_bytes())
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    m.cpu.read_u32(BUFFER + actual.len() as u32).unwrap(),
                    SENTINEL
                );
                assert_eq!(crate::util::last_error(&env(&mut m)), ERROR);
            }
        }
        assert_eq!(
            request(
                &mut m,
                Scope::Thread,
                LANGUAGE_NAME | THREAD_LANGUAGES,
                BUFFER,
                16
            ),
            1
        );
        assert_eq!(words(&m), (0, 2));
        assert_eq!(m.cpu.read_u32(BUFFER).unwrap(), 0);
    }
}

#[test]
fn insufficient_capacity_flags_and_bad_memory_have_bounded_outputs() {
    let mut m = machine(932);
    for scope in [Scope::Thread, Scope::User, Scope::System, Scope::Process] {
        let needed = if scope == Scope::Process { 2 } else { 7 };
        assert_eq!(request(&mut m, scope, LANGUAGE_NAME, BUFFER, needed - 1), 0);
        assert_eq!(
            words(&m),
            (if scope == Scope::Thread { 1 } else { SENTINEL }, needed)
        );
        assert_eq!(
            crate::util::last_error(&env(&mut m)),
            err::INSUFFICIENT_BUFFER
        );
        assert_eq!(request(&mut m, scope, LANGUAGE_NAME, 0, 1), 0);
        assert_eq!(words(&m), (SENTINEL, 1));
        assert_eq!(
            crate::util::last_error(&env(&mut m)),
            err::INVALID_PARAMETER
        );
        assert_eq!(
            request(&mut m, scope, LANGUAGE_NAME | LANGUAGE_ID, BUFFER, 16),
            0
        );
        assert_eq!(words(&m), (SENTINEL, 16));
        assert_eq!(
            crate::util::last_error(&env(&mut m)),
            err::INVALID_PARAMETER
        );
        assert_eq!(request(&mut m, scope, 0x80000000, BUFFER, 16), 0);
        assert_eq!(words(&m), (SENTINEL, 16));
        assert_eq!(
            crate::util::last_error(&env(&mut m)),
            err::INVALID_PARAMETER
        );
    }
    assert_eq!(
        request(
            &mut m,
            Scope::Thread,
            LANGUAGE_NAME | SYSTEM_FALLBACK | USER_FALLBACK,
            BUFFER,
            16
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    m.cpu.write_u32(COUNT, SENTINEL).unwrap();
    m.cpu.write_u32(SIZE, 16).unwrap();
    assert_eq!(
        query(
            &mut env(&mut m),
            Scope::Thread,
            LANGUAGE_NAME,
            GuestPtr::new(COUNT),
            WideStr(GuestPtr::new(0x600000)),
            GuestPtr::new(SIZE)
        ),
        0
    );
    assert_eq!(words(&m), (SENTINEL, 16));
    assert_eq!(crate::util::last_error(&env(&mut m)), 998);
}
