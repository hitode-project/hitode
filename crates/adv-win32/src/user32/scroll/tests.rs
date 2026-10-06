use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_machine::{Clock, Machine, Resume, layout::PARK_ADDR};

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
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/scroll-tests");
    crate::Win32::install(
        &mut m,
        crate::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    m.spawn("scroll owner", PARK_ADDR, &[], 65536).unwrap();
    let data = m.sys.vm.alloc(&mut m.cpu, 8192, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, data + 4096);
    let hwnd = m.ext.get::<Win32>().user.desktop;
    let procedure = m.resolve_import("user32.dll", "DefWindowProcA").unwrap();
    let window = m
        .ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut m.sys.objects, &hwnd)
        .unwrap();
    window.thread = m.sys.current;
    window.proc = procedure;
    (m, data, hwnd)
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
fn returned(outcome: Outcome) -> i32 {
    match outcome {
        Outcome::Return(value) => value as i32,
        _ => panic!("synchronous scroll state result expected"),
    }
}
fn put(m: &mut Machine<JitCpu>, data: u32, values: [u32; 7]) {
    for (index, value) in values.into_iter().enumerate() {
        m.cpu.write_u32(data + index as u32 * 4, value).unwrap();
    }
}
fn read(m: &Machine<JitCpu>, data: u32) -> [u32; 7] {
    std::array::from_fn(|index| m.cpu.read_u32(data + index as u32 * 4).unwrap())
}

#[test]
fn scroll_queries_share_window_state_and_publish_only_requested_writable_fields() {
    let (mut m, data, hwnd) = fixture();
    assert_eq!(returned(get_pos(&mut env(&mut m), Handle(hwnd), 1)), 0);
    assert_eq!(crate::util::last_error(&env(&mut m)), 1447);
    put(
        &mut m,
        data,
        [28, 7, (-10i32) as u32, 100, 20, 95, 0xcccccccc],
    );
    assert_eq!(
        returned(set_info(
            &mut env(&mut m),
            Handle(hwnd),
            1,
            GuestPtr::new(data),
            1
        )),
        81
    );
    assert_eq!(returned(get_pos(&mut env(&mut m), Handle(hwnd), 1)), 81);
    put(&mut m, data, [24, 23, 0, 0, 0, 0, 0]);
    assert_eq!(
        returned(get_info(
            &mut env(&mut m),
            Handle(hwnd),
            1,
            GuestPtr::new(data)
        )),
        1
    );
    assert_eq!(read(&m, data), [24, 23, (-10i32) as u32, 100, 20, 81, 81]);
    assert_eq!(
        returned(set_pos(&mut env(&mut m), Handle(hwnd), 1, -100, 0)),
        81
    );
    assert_eq!(returned(get_pos(&mut env(&mut m), Handle(hwnd), 1)), -10);
    assert_eq!(returned(get_pos(&mut env(&mut m), Handle(hwnd), 0)), 0);
    put(
        &mut m,
        data,
        [
            28, 4, 0xcccccccc, 0xcccccccc, 0xcccccccc, 0xcccccccc, 0xcccccccc,
        ],
    );
    assert_eq!(
        returned(get_info(
            &mut env(&mut m),
            Handle(hwnd),
            1,
            GuestPtr::new(data)
        )),
        1
    );
    assert_eq!(
        read(&m, data),
        [
            28,
            4,
            0xcccccccc,
            0xcccccccc,
            0xcccccccc,
            (-10i32) as u32,
            0xcccccccc
        ]
    );
    // A requested field on a protected page prevents earlier fields publishing.
    let crossing = data + 4096 - 24;
    put(
        &mut m,
        crossing,
        [
            28, 23, 0xa5a5a5a5, 0xa5a5a5a5, 0xa5a5a5a5, 0xa5a5a5a5, 0xa5a5a5a5,
        ],
    );
    m.cpu.protect(data + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(
        returned(get_info(
            &mut env(&mut m),
            Handle(hwnd),
            1,
            GuestPtr::new(crossing)
        )),
        0
    );
    assert_eq!(
        read(&m, crossing),
        [
            28, 23, 0xa5a5a5a5, 0xa5a5a5a5, 0xa5a5a5a5, 0xa5a5a5a5, 0xa5a5a5a5
        ]
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 998);
    assert_eq!(returned(get_pos(&mut env(&mut m), Handle(hwnd), 3)), 0);
    assert_eq!(crate::util::last_error(&env(&mut m)), 87);
    let mut destroyed =
        super::super::window::destroy_window(&mut env(&mut m), hwnd, |_, success| {
            Outcome::Return(u32::from(success))
        });
    loop {
        destroyed = match destroyed {
            Outcome::Call { then, .. } => then(&mut env(&mut m), Resume::Returned(0)),
            Outcome::Return(value) => {
                assert_eq!(value, 1);
                break;
            }
            _ => panic!("unexpected window destruction continuation"),
        };
    }
    assert_eq!(returned(get_pos(&mut env(&mut m), Handle(hwnd), 1)), 0);
    assert_eq!(crate::util::last_error(&env(&mut m)), 1400);
}

#[test]
fn scrollbar_controls_dispatch_guest_messages_instead_of_using_standard_bar_state() {
    let (mut m, data, hwnd) = fixture();
    let proc = m
        .ext
        .get::<Win32>()
        .user
        .windows
        .get(&m.sys.objects, &hwnd)
        .unwrap()
        .proc;
    for (outcome, message, wparam, lparam, result) in [
        (get_pos(&mut env(&mut m), Handle(hwnd), 2), 0xe1, 0, 0, 99),
        (
            set_pos(&mut env(&mut m), Handle(hwnd), 2, 75, 1),
            0xe0,
            75,
            1,
            42,
        ),
        (
            set_info(&mut env(&mut m), Handle(hwnd), 2, GuestPtr::new(data), 1),
            0xe9,
            1,
            data,
            66,
        ),
    ] {
        match outcome {
            Outcome::Call {
                target, args, then, ..
            } => {
                assert_eq!(target, proc);
                assert_eq!(args, [hwnd, message, wparam, lparam]);
                assert_eq!(
                    returned(then(&mut env(&mut m), Resume::Returned(result))),
                    result as i32
                );
            }
            _ => panic!("SB_CTL must call the window procedure"),
        }
    }
}

#[cfg(windows)]
#[test]
fn scrollbar_range_page_and_position_updates_match_native_windows() {
    use core::ffi::c_void;
    #[repr(C)]
    struct Info {
        size: u32,
        mask: u32,
        minimum: i32,
        maximum: i32,
        page: u32,
        position: i32,
        track: i32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn CreateWindowExW(
            ex: u32,
            class: *const u16,
            name: *const u16,
            style: u32,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            parent: *mut c_void,
            menu: *mut c_void,
            instance: *mut c_void,
            param: *mut c_void,
        ) -> *mut c_void;
        fn DestroyWindow(hwnd: *mut c_void) -> i32;
        fn SetScrollInfo(hwnd: *mut c_void, bar: i32, info: *const Info, redraw: i32) -> i32;
        fn GetScrollInfo(hwnd: *mut c_void, bar: i32, info: *mut Info) -> i32;
        fn SetScrollPos(hwnd: *mut c_void, bar: i32, position: i32, redraw: i32) -> i32;
        fn SetScrollRange(
            hwnd: *mut c_void,
            bar: i32,
            minimum: i32,
            maximum: i32,
            redraw: i32,
        ) -> i32;
        fn GetScrollRange(hwnd: *mut c_void, bar: i32, minimum: *mut i32, maximum: *mut i32)
        -> i32;
    }
    let class: Vec<u16> = "STATIC".encode_utf16().chain([0]).collect();
    let empty = [0u16];
    // SAFETY: initialized strings and NULL optional parameters create one hidden
    // native control. Every Info is a full writable SDK structure.
    let native = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            empty.as_ptr(),
            0,
            0,
            0,
            100,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert!(!native.is_null());
    let (mut m, data, hwnd) = fixture();
    for (mask, minimum, maximum, page, position) in [
        (7, -10, 100, 20, 95),
        (4, 0, 0, 0, 500),
        (2, 0, 0, 999, 0),
        (1, 20, 10, 0, 0),
        (1, i32::MIN, i32::MAX, 0, 0),
        (7, 0, 100000, 1, 80000),
    ] {
        let input = Info {
            size: 28,
            mask,
            minimum,
            maximum,
            page,
            position,
            track: 0,
        };
        put(
            &mut m,
            data,
            [
                28,
                mask,
                minimum as u32,
                maximum as u32,
                page,
                position as u32,
                0,
            ],
        );
        let expected = unsafe { SetScrollInfo(native, 1, &input, 0) };
        assert_eq!(
            returned(set_info(
                &mut env(&mut m),
                Handle(hwnd),
                1,
                GuestPtr::new(data),
                0
            )),
            expected
        );
        let mut output = Info {
            size: 28,
            mask: 23,
            minimum: 0,
            maximum: 0,
            page: 0,
            position: 0,
            track: 0,
        };
        assert_eq!(unsafe { GetScrollInfo(native, 1, &mut output) }, 1);
        put(&mut m, data, [28, 23, 0, 0, 0, 0, 0]);
        assert_eq!(
            returned(get_info(
                &mut env(&mut m),
                Handle(hwnd),
                1,
                GuestPtr::new(data)
            )),
            1
        );
        assert_eq!(
            read(&m, data),
            [
                28,
                23,
                output.minimum as u32,
                output.maximum as u32,
                output.page,
                output.position as u32,
                output.track as u32
            ]
        );
    }
    assert_eq!(
        returned(set_pos(&mut env(&mut m), Handle(hwnd), 1, 90000, 0)),
        unsafe { SetScrollPos(native, 1, 90000, 0) }
    );
    for (minimum, maximum) in [(-20, 10), (20, 10), (i32::MIN, i32::MAX), (0, 0)] {
        assert_eq!(
            returned(set_range(
                &mut env(&mut m),
                Handle(hwnd),
                1,
                minimum,
                maximum,
                0
            )),
            unsafe { SetScrollRange(native, 1, minimum, maximum, 0) }
        );
        let (mut native_minimum, mut native_maximum) = (0, 0);
        assert_eq!(
            unsafe { GetScrollRange(native, 1, &mut native_minimum, &mut native_maximum) },
            1
        );
        assert_eq!(
            returned(get_range(
                &mut env(&mut m),
                Handle(hwnd),
                1,
                GuestPtr::new(data + 32),
                GuestPtr::new(data + 36)
            )),
            1
        );
        assert_eq!(m.cpu.read_u32(data + 32).unwrap() as i32, native_minimum);
        assert_eq!(m.cpu.read_u32(data + 36).unwrap() as i32, native_maximum);
    }
    assert_eq!(unsafe { DestroyWindow(native) }, 1);
}
