//! Authored USER objects and callback continuations; native Windows order is
//! ENTER/INIT/INITPOPUP, SELECT, UNINIT, SELECT(close), EXIT, COMMAND.
use super::*;
use adv_cpu::{JitCpu, backend::Guest};
use adv_machine::{Clock, Machine, Resume, WakeReason, layout::PARK_ADDR};

const CODE: u32 = 0x400000;
fn fixture() -> (Machine<JitCpu>, u32, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    crate::Win32::install(
        &mut m,
        crate::Win32Config::new(
            "/nonexistent/authored-menu",
            "fixture.exe",
            "/nonexistent/authored-menu-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.write(CODE, &[0xc3]).unwrap();
    m.spawn("authored menu owner", CODE, &[], 65536).unwrap();
    let u = &mut m.ext.get_mut::<crate::Win32>().user;
    let owner = u.desktop;
    let w = u.windows.get_mut(&mut m.sys.objects, &owner).unwrap();
    w.proc = CODE;
    w.thread = m.sys.current;
    w.needs_paint = false;
    let root = u.menus.new_menu(&mut m.sys.objects).unwrap();
    *u.menus.get_mut(&mut m.sys.objects, root).unwrap() = vec![
        Item {
            id: 7,
            text: "Save".encode_utf16().collect(),
            ..Item::default()
        },
        Item {
            id: 9,
            flags: 3,
            text: "Disabled".encode_utf16().collect(),
            ..Item::default()
        },
    ];
    (m, owner, root)
}
fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn calls(m: &mut Machine<JitCpu>, mut outcome: Outcome, log: &mut Vec<[u32; 4]>) -> Outcome {
    for _ in 0..64 {
        match outcome {
            Outcome::Call {
                target, args, then, ..
            } => {
                assert_eq!(target, CODE);
                log.push(args.try_into().unwrap());
                outcome = then(&mut env(m), Resume::Returned(0));
            }
            _ => return outcome,
        }
    }
    panic!("unbounded popup callbacks");
}
fn returned(outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("menu must return"),
    }
}

#[test]
fn popup_notifications_return_command_and_cancel_follow_native_order() {
    for flags in [0, 0x100, 0x80, 0x180] {
        for selected in [false, true] {
            let (mut m, owner, root) = fixture();
            assert!(super::super::super::message::post(
                &mut env(&mut m),
                owner,
                0x100,
                if selected { 13 } else { 27 },
                0
            ));
            let outcome = track(&mut env(&mut m), root, flags, 20, 30, owner, None);
            let mut log = Vec::new();
            let outcome = calls(&mut m, outcome, &mut log);
            assert_eq!(
                returned(outcome),
                if flags & 0x100 != 0 {
                    if selected { 7 } else { 0 }
                } else {
                    1
                }
            );
            let mut expected = Vec::new();
            if flags & 0x80 == 0 {
                expected.extend([0x211, 0x116, 0x117]);
            }
            if selected {
                expected.push(0x11f);
            }
            if flags & 0x80 == 0 {
                expected.push(0x125);
            }
            expected.push(0x11f);
            if flags & 0x80 == 0 {
                expected.push(0x212);
            }
            if selected && flags & 0x100 == 0 {
                expected.push(0x111);
            }
            assert_eq!(log.iter().map(|a| a[1]).collect::<Vec<_>>(), expected);
            assert_eq!(
                log.iter().rev().find(|a| a[1] == 0x11f).unwrap()[2..],
                [0xffff0000, 0]
            );
            assert!(user(&env(&mut m)).menus.tracking.is_none());
        }
    }
}

#[test]
fn popup_wait_accepts_only_live_choices_and_retires_frontend_token() {
    let (mut m, owner, root) = fixture();
    let outcome = track(
        &mut env(&mut m),
        root,
        0x100,
        20,
        30,
        owner,
        Some([1, 2, 3, 4]),
    );
    assert_eq!(
        presentation_key(&m.ext.get::<crate::Win32>().user, &m.sys.objects).menu,
        0,
        "no projection before init completes"
    );
    let mut log = Vec::new();
    let outcome = calls(&mut m, outcome, &mut log);
    let Outcome::Block { then, .. } = outcome else {
        panic!("no automatic menu choice")
    };
    let bar = presentation(&m.ext.get::<crate::Win32>().user, &m.sys.objects).unwrap();
    let token = bar.popup.as_ref().unwrap().active.clone();
    assert!(token.load(Ordering::Acquire));
    assert_eq!(bar.popup.as_ref().unwrap().exclude, Some([1, 2, 3, 4]));
    assert!(!post_host_event(
        &mut env(&mut m),
        MenuEvent::Command(bar.items[1].command)
    ));
    assert_eq!(
        returned(track(&mut env(&mut m), root, 0, 0, 0, owner, None)),
        0,
        "nested tracking fails without replacing state"
    );
    assert!(post_host_event(
        &mut env(&mut m),
        MenuEvent::Command(bar.items[0].command)
    ));
    let outcome = then(&mut env(&mut m), Resume::Woke(WakeReason::Notified));
    assert_eq!(returned(calls(&mut m, outcome, &mut log)), 7);
    assert!(!token.load(Ordering::Acquire));
    assert!(
        !post_host_event(&mut env(&mut m), MenuEvent::Command(bar.items[0].command)),
        "retired projection cannot send another command"
    );
    assert_eq!(
        m.ext.get::<crate::Win32>().user.menus.count(&m.sys.objects),
        1,
        "caller still owns menu"
    );
    assert!(
        m.ext
            .get_mut::<crate::Win32>()
            .user
            .menus
            .destroy(root, &mut m.sys.objects)
    );
    assert_eq!(
        m.ext.get::<crate::Win32>().user.menus.count(&m.sys.objects),
        0
    );
}

#[test]
fn popup_end_and_destroy_cancel_without_command_or_ownership_leak() {
    for destroy in [false, true] {
        let (mut m, owner, root) = fixture();
        set_last_error(&mut env(&mut m), 4660);
        assert_eq!(end_menu(&mut env(&mut m)), 1);
        assert_eq!(crate::util::last_error(&env(&mut m)), 4660);
        let mut log = Vec::new();
        let outcome = track(&mut env(&mut m), root, 0x100, 0, 0, owner, None);
        let Outcome::Block { then, .. } = calls(&mut m, outcome, &mut log) else {
            panic!("tracking must wait")
        };
        let bar = presentation(&m.ext.get::<crate::Win32>().user, &m.sys.objects).unwrap();
        if destroy {
            assert!(
                m.ext
                    .get_mut::<crate::Win32>()
                    .user
                    .menus
                    .destroy(root, &mut m.sys.objects)
            );
        } else {
            assert!(post_host_event(
                &mut env(&mut m),
                MenuEvent::Cancel {
                    window: owner,
                    root
                }
            ));
        }
        assert!(!bar.popup.as_ref().unwrap().active.load(Ordering::Acquire));
        let outcome = then(&mut env(&mut m), Resume::Woke(WakeReason::Notified));
        assert_eq!(returned(calls(&mut m, outcome, &mut log)), 0);
        assert!(!log.iter().any(|a| a[1] == 0x111));
    }
}

#[test]
fn popup_validation_and_unsupported_presentation_fail_without_false_success() {
    let (mut m, owner, root) = fixture();
    for (menu, flags, hwnd, error) in [
        (root, 0, 0, 1400),
        (0, 0, owner, 1401),
        (root, 1, owner, 50),
        (root, 0xc, owner, 87),
    ] {
        assert_eq!(
            returned(track(&mut env(&mut m), menu, flags, 0, 0, hwnd, None)),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut m)), error);
        assert!(m.ext.get::<crate::Win32>().user.menus.tracking.is_none());
    }
    let ctx = m.sys.current;
    m.ext
        .get_mut::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut m.sys.objects, &owner)
        .unwrap()
        .thread = ctx + 1;
    assert_eq!(
        returned(track(&mut env(&mut m), root, 0, 0, 0, owner, None)),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    m.ext
        .get_mut::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut m.sys.objects, &owner)
        .unwrap()
        .thread = ctx;
    m.ext
        .get_mut::<crate::Win32>()
        .user
        .menus
        .get_mut(&mut m.sys.objects, root)
        .unwrap()[0]
        .flags = 0x100;
    let mut log = Vec::new();
    let outcome = track(&mut env(&mut m), root, 0, 0, 0, owner, None);
    assert_eq!(returned(calls(&mut m, outcome, &mut log)), 0);
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    assert!(m.ext.get::<crate::Win32>().user.menus.tracking.is_none());
    assert!(!log.iter().any(|a| a[1] == 0x111));
}

#[test]
fn popup_ex_checks_entire_params_and_preserves_exclusion_and_caller_storage() {
    const DATA: u32 = 0x410000;
    let (mut m, owner, root) = fixture();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let args = [20u32, 1, 2, 31, 42, 0xcccccccc];
    for (i, value) in args.iter().enumerate() {
        m.cpu.write_u32(DATA + i as u32 * 4, *value).unwrap();
    }
    let bad = track_menu_ex(
        &mut env(&mut m),
        Handle(root),
        0,
        0,
        0,
        Handle(owner),
        GuestPtr::new(DATA + 4090),
    );
    assert_eq!(returned(bad), 0);
    assert!(m.ext.get::<crate::Win32>().user.menus.tracking.is_none());
    m.cpu.write_u32(DATA, 16).unwrap();
    assert_eq!(
        returned(track_menu_ex(
            &mut env(&mut m),
            Handle(root),
            0,
            0,
            0,
            Handle(owner),
            GuestPtr::new(DATA)
        )),
        0
    );
    m.cpu.write_u32(DATA, 20).unwrap();
    let mut log = Vec::new();
    let outcome = track_menu_ex(
        &mut env(&mut m),
        Handle(root),
        0x100,
        20,
        30,
        Handle(owner),
        GuestPtr::new(DATA),
    );
    let Outcome::Block { then, .. } = calls(&mut m, outcome, &mut log) else {
        panic!("menu must wait")
    };
    assert_eq!(
        presentation(&m.ext.get::<crate::Win32>().user, &m.sys.objects)
            .unwrap()
            .popup
            .unwrap()
            .exclude,
        Some([1, 2, 31, 42])
    );
    for (i, value) in args.iter().enumerate() {
        assert_eq!(m.cpu.read_u32(DATA + i as u32 * 4).unwrap(), *value);
    }
    assert_eq!(end_menu(&mut env(&mut m)), 1);
    let outcome = then(&mut env(&mut m), Resume::Woke(WakeReason::Notified));
    assert_eq!(returned(calls(&mut m, outcome, &mut log)), 0);
}

#[test]
fn popup_cyclic_projection_fails_instead_of_waiting_without_a_presentable_menu() {
    let (mut m, owner, root) = fixture();
    m.ext
        .get_mut::<crate::Win32>()
        .user
        .menus
        .get_mut(&mut m.sys.objects, root)
        .unwrap()[0]
        .submenu = root;
    let mut log = Vec::new();
    let outcome = track(&mut env(&mut m), root, 0, 0, 0, owner, None);
    assert_eq!(returned(calls(&mut m, outcome, &mut log)), 0);
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    assert!(m.ext.get::<crate::Win32>().user.menus.tracking.is_none());
    assert!(!log.iter().any(|a| a[1] == 0x111));
}
