use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};

const DATA: u32 = 0x410000;
const SENTINEL: u32 = 0x31415926;
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn query(
    machine: &mut Machine<JitCpu>,
    handle: u32,
    class: i32,
    size: u32,
) -> (i32, u32, u32, u32) {
    machine.cpu.write_u32(DATA + 4, SENTINEL).unwrap();
    machine.cpu.write_u32(DATA + 8, SENTINEL).unwrap();
    crate::util::set_last_error(&mut env(machine), SENTINEL);
    let result = information(
        &mut env(machine),
        Handle(handle),
        class,
        GuestPtr::new(DATA + 4),
        size,
        GuestPtr::new(DATA + 8),
    );
    (
        result,
        crate::util::last_error(&env(machine)),
        machine.cpu.read_u32(DATA + 8).unwrap(),
        machine.cpu.read_u32(DATA + 4).unwrap(),
    )
}

#[test]
fn token_queries_match_native_lengths_errors_and_shared_handle_lifetime() {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let directory = std::env::temp_dir().join(format!("adv-token-{}", std::process::id()));
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&directory, "token.exe", &directory),
    )
    .unwrap();
    machine.spawn("token", PARK_ADDR, &[], 65536).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.set_reg(Reg::Esp, DATA + 3500);
    crate::util::set_last_error(&mut env(&mut machine), SENTINEL);
    assert_eq!(
        open(
            &mut env(&mut machine),
            Handle(u32::MAX),
            QUERY,
            GuestPtr::new(DATA)
        ),
        1
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), SENTINEL);
    let handle = machine.cpu.read_u32(DATA).unwrap();
    for size in [0, 1, 3, 8] {
        assert_eq!(query(&mut machine, handle, 20, size), (0, 24, 4, SENTINEL));
    }
    assert_eq!(query(&mut machine, handle, 20, 4), (1, SENTINEL, 4, 0));
    for class in [8, 18] {
        assert_eq!(query(&mut machine, handle, class, 0), (0, 122, 4, SENTINEL));
        assert_eq!(query(&mut machine, handle, class, 8), (1, SENTINEL, 4, 1));
        assert_eq!(query(&mut machine, 0, class, 4), (0, 6, SENTINEL, SENTINEL));
    }
    assert_eq!(query(&mut machine, handle, 1, 0), (0, 122, 36, SENTINEL));
    // TOKEN_USER owns a self-contained SID relative to the caller's buffer.
    assert_eq!(
        information(
            &mut env(&mut machine),
            Handle(handle),
            1,
            GuestPtr::new(DATA + 64),
            36,
            GuestPtr::new(DATA + 8)
        ),
        1
    );
    assert_eq!(machine.cpu.read_u32(DATA + 64).unwrap(), DATA + 72);
    assert_eq!(machine.cpu.read_u32(DATA + 68).unwrap(), 0);
    let mut sid = [0; 28];
    machine.cpu.read(DATA + 72, &mut sid).unwrap();
    assert_eq!(sid, adv_os::identity::current_user_sid());
    assert_eq!(
        adv_os::identity::current_user_sid_string(),
        "S-1-5-21-0-0-0-1000"
    );
    assert_eq!(
        sid_text_w(
            &mut env(&mut machine),
            GuestPtr::new(DATA + 72),
            GuestPtr::new(DATA + 128)
        ),
        1
    );
    let text_pointer = machine.cpu.read_u32(DATA + 128).unwrap();
    let expected = adv_os::identity::current_user_sid_string();
    let units: Vec<_> = (0..expected.len() + 1)
        .map(|i| machine.cpu.read_u16(text_pointer + i as u32 * 2).unwrap())
        .collect();
    assert_eq!(
        String::from_utf16(&units[..units.len() - 1]).unwrap(),
        expected
    );
    assert_eq!(units.last(), Some(&0));
    assert!(
        machine
            .ext
            .get::<adv_os::global::Globals>()
            .get(&machine.sys.objects, text_pointer)
            .is_some()
    );
    assert_eq!(
        crate::kernel32::release_local_memory(&mut env(&mut machine), text_pointer, false),
        0
    );
    assert!(
        machine
            .ext
            .get::<adv_os::global::Globals>()
            .get(&machine.sys.objects, text_pointer)
            .is_none()
    );
    machine.cpu.write(DATA + 72, &[2]).unwrap();
    machine.cpu.write_u32(DATA + 128, SENTINEL).unwrap();
    assert_eq!(
        sid_text_a(
            &mut env(&mut machine),
            GuestPtr::new(DATA + 72),
            GuestPtr::new(DATA + 128)
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 1337);
    assert_eq!(machine.cpu.read_u32(DATA + 128).unwrap(), SENTINEL);
    assert_eq!(
        adv_os::identity::sid_string(&[1, 1, 1, 2, 0xab, 0xcd, 0xef, 0x12, 255, 255, 255, 255])
            .as_deref(),
        Some("S-1-0x102ABCDEF12-4294967295")
    );
    let alias = machine.sys.objects.duplicate(handle).unwrap();
    let restricted = duplicate(&mut env(&mut machine), handle, 0)
        .unwrap()
        .unwrap();
    assert!(machine.sys.objects.close(handle).is_some());
    assert_eq!(query(&mut machine, alias, 20, 4), (1, SENTINEL, 4, 0));
    assert_eq!(query(&mut machine, handle, 20, 4), (0, 6, 4, SENTINEL));
    assert_eq!(query(&mut machine, restricted, 20, 8), (0, 24, 4, SENTINEL));
    assert_eq!(query(&mut machine, restricted, 20, 4), (0, 5, 4, SENTINEL));
    assert_eq!(
        query(&mut machine, restricted, 8, 4),
        (0, 5, SENTINEL, SENTINEL)
    );
    assert!(machine.sys.objects.close(alias).is_some());
    assert!(machine.sys.objects.close(restricted).is_some());
    assert_eq!(machine.sys.objects.hosts::<Token>().count(), 0);
}
