use super::*;
use crate::misc::test_support::{Fixture, env};
use std::time::{Duration, Instant};

#[test]
// spec: WIN32-USER-8
fn physical_double_clicks_use_guest_class_and_mapping_and_keep_key_state() {
    let mut f = Fixture::new("double-click-routing");
    let hwnd = f.m.ext.get::<Win32>().user.desktop;
    let class =
        f.m.ext
            .get::<Win32>()
            .user
            .windows
            .get(&f.m.sys.objects, &hwnd)
            .unwrap()
            .class;
    f.m.ext.get_mut::<Win32>().user.classes[class].style = 8;
    f.m.ext.get_mut::<Win32>().user.top.push(hwnd);
    f.m.ext.get_mut::<Win32>().user.swap_buttons = true;
    let time = Instant::now();
    for (index, (message, expected, down)) in [
        (0x201, 0x204, true),
        (0x202, 0x205, false),
        (0x201, 0x206, true),
        (0x202, 0x205, false),
    ]
    .into_iter()
    .enumerate()
    {
        assert!(post_at(
            &mut env(&mut f.m),
            message,
            if down { 9 } else { 8 },
            make_long(20, 30),
            time + Duration::from_millis(index as u64 * 40)
        ));
        let user = &f.m.ext.get::<Win32>().user;
        let event = user.queues[&0].posted.back().unwrap();
        assert_eq!(
            (event.hwnd, event.msg, event.wparam, event.lparam),
            (hwnd, expected, if down { 10 } else { 8 }, make_long(20, 30))
        );
        assert_eq!(user.keys[1] & 0x80 != 0, down);
        assert_eq!(user.keys[2] & 0x80, 0);
    }
    // Losing native focus ends the click sequence as well as releasing keys.
    post_at(&mut env(&mut f.m), 0x201, 1, 0, time);
    release(&mut env(&mut f.m));
    post_at(&mut env(&mut f.m), 0x201, 1, 0, time);
    assert_eq!(
        f.m.ext.get::<Win32>().user.queues[&0]
            .posted
            .back()
            .unwrap()
            .msg,
        0x204
    );
    f.m.ext.get_mut::<Win32>().user.classes[class].style = 0;
    post_at(&mut env(&mut f.m), 0x202, 0, 0, time);
    post_at(&mut env(&mut f.m), 0x201, 1, 0, time);
    assert_eq!(
        f.m.ext.get::<Win32>().user.queues[&0]
            .posted
            .back()
            .unwrap()
            .msg,
        0x204
    );
    f.cleanup();
}

#[test]
fn paired_time_and_rectangle_apis_share_only_the_guest_profile() {
    let mut f = Fixture::new("double-click-profile");
    assert_eq!(f.call("user32.dll", "GetDoubleClickTime", &[]), 500);
    for (input, expected) in [(1, 1), (900, 900), (u32::MAX, 5000), (0, 500)] {
        assert_eq!(f.call("user32.dll", "SetDoubleClickTime", &[input]), 1);
        assert_eq!(f.call("user32.dll", "GetDoubleClickTime", &[]), expected);
    }
    for api in ["SystemParametersInfoA", "SystemParametersInfoW"] {
        // These scalar setters ignore pvParam; there is no guest/host pointer read.
        for (action, value, metric) in [(0x1d, 9, 36), (0x1e, 7, 37), (0x21, 1, 23)] {
            assert_eq!(f.call("user32.dll", api, &[action, value, u32::MAX, 0]), 1);
            assert_eq!(f.call("user32.dll", "GetSystemMetrics", &[metric]), value);
        }
        assert_eq!(f.call("user32.dll", api, &[0x20, 7000, u32::MAX, 0]), 1);
        assert_eq!(f.call("user32.dll", "GetDoubleClickTime", &[]), 5000);
    }
    f.cleanup();
    let mut fresh = Fixture::new("double-click-isolation");
    assert_eq!(fresh.call("user32.dll", "GetDoubleClickTime", &[]), 500);
    assert_eq!(fresh.call("user32.dll", "GetSystemMetrics", &[36]), 4);
    assert_eq!(fresh.call("user32.dll", "GetSystemMetrics", &[37]), 4);
    assert_eq!(fresh.call("user32.dll", "GetSystemMetrics", &[23]), 0);
    fresh.cleanup();
}

#[test]
// spec: WIN32-USER-12
fn committed_text_reaches_the_focus_as_ime_characters_then_def_window_proc_characters() {
    let mut f = Fixture::new("committed-text");
    crate::set_codepage(&mut env(&mut f.m), 932).unwrap();
    let hwnd = f.m.ext.get::<Win32>().user.desktop;
    f.m.ext.get_mut::<Win32>().user.top.push(hwnd);
    let posted = |f: &mut Fixture| -> Vec<(u32, u32)> {
        let queue = &mut f
            .m
            .ext
            .get_mut::<Win32>()
            .user
            .queues
            .get_mut(&0)
            .unwrap()
            .posted;
        queue.drain(..).map(|m| (m.msg, m.wparam)).collect()
    };
    // An ANSI procedure gets packed Shift-JIS; U+20AC has no CP932 form and is skipped.
    assert_eq!(post_text(&mut env(&mut f.m), "あA\u{20ac}"), 2);
    assert_eq!(posted(&mut f), [(0x286, 0x82a0), (0x286, 0x41)]);
    f.m.ext
        .get_mut::<Win32>()
        .user
        .windows
        .get_mut(&mut f.m.sys.objects, &hwnd)
        .unwrap()
        .unicode = true;
    assert_eq!(post_text(&mut env(&mut f.m), "あ\u{20ac}\u{1f600}"), 3);
    assert_eq!(
        posted(&mut f),
        [
            (0x286, 0x3042),
            (0x286, 0x20ac),
            (0x286, 0xd83d),
            (0x286, 0xde00)
        ]
    );
    // DefWindowProcA splits a DBCS character into lead and trail WM_CHAR.
    assert_eq!(
        f.call("user32.dll", "DefWindowProcA", &[hwnd, 0x286, 0x82a0, 1]),
        0
    );
    assert_eq!(
        f.call("user32.dll", "DefWindowProcA", &[hwnd, 0x286, 0x41, 1]),
        0
    );
    assert_eq!(
        f.call("user32.dll", "DefWindowProcW", &[hwnd, 0x286, 0x3042, 1]),
        0
    );
    assert_eq!(
        posted(&mut f),
        [(0x102, 0x82), (0x102, 0xa0), (0x102, 0x41), (0x102, 0x3042)]
    );
    f.cleanup();
}

#[test]
// spec: PERS-INPUT-1
fn wheel_uses_focus_over_capture_and_default_processing_returns_the_parent_result() {
    use crate::user32::window::{Rect, WS_CHILD, WS_POPUP, WS_VISIBLE, Window};
    use adv_machine::{Outcome, Resume};
    let mut f = Fixture::new("wheel-routing");
    let (top, focus, captured, child) = (0x10020, 0x10024, 0x10028, 0x1002c);
    let desktop = f.m.ext.get::<Win32>().user.desktop;
    let class =
        f.m.ext
            .get::<Win32>()
            .user
            .windows
            .get(&f.m.sys.objects, &desktop)
            .unwrap()
            .class;
    for (hwnd, parent, rect) in [
        (top, 0, Rect::new(100, 50, 640, 480)),
        (focus, top, Rect::new(20, 30, 100, 100)),
        (captured, top, Rect::new(40, 50, 100, 100)),
        (child, focus, Rect::new(0, 0, 20, 20)),
    ] {
        let window = Window::new(
            hwnd,
            class,
            0x400100 + hwnd,
            WS_VISIBLE | if parent == 0 { WS_POPUP } else { WS_CHILD },
            0,
            parent,
            rect,
            0,
        );
        assert!(
            f.m.ext
                .get_mut::<Win32>()
                .user
                .windows
                .insert(&mut f.m.sys.objects, hwnd, window)
        );
    }
    let user = &mut f.m.ext.get_mut::<Win32>().user;
    user.top = vec![top];
    user.queue(0).focus = focus;
    user.queue(0).capture = captured;
    let wp = ((-120i32 as u32) << 16) | 8;
    let position = make_long(160, 130);
    for message in [0x20a, 0x20e] {
        assert!(post(&mut env(&mut f.m), message, wp, make_long(60, 80)));
        let event =
            f.m.ext
                .get_mut::<Win32>()
                .user
                .queue(0)
                .posted
                .pop_front()
                .unwrap();
        assert_eq!(
            (event.hwnd, event.msg, event.wparam, event.lparam),
            (focus, message, wp, position)
        );
    }
    for api in ["DefWindowProcA", "DefWindowProcW"] {
        let Outcome::Call {
            target,
            args,
            then: child_return,
            ..
        } = f.invoke("user32.dll", api, &[child, 0x20a, wp, position])
        else {
            panic!("wheel must synchronously call the parent");
        };
        assert_eq!(target, 0x400100 + focus);
        assert_eq!(args, [focus, 0x20a, wp, position]);
        let Outcome::Call {
            target,
            args,
            then: focus_return,
            ..
        } = f.invoke("user32.dll", api, &args)
        else {
            panic!("unhandled wheel must continue up the parent chain");
        };
        assert_eq!(target, 0x400100 + top);
        assert_eq!(args, [top, 0x20a, wp, position]);
        assert_eq!(f.call("user32.dll", api, &args), 0);
        assert!(matches!(
            focus_return(&mut env(&mut f.m), Resume::Returned(73)),
            Outcome::Return(73)
        ));
        assert!(matches!(
            child_return(&mut env(&mut f.m), Resume::Returned(73)),
            Outcome::Return(73)
        ));
    }
    f.cleanup();
}

#[test]
// spec: WIN32-USER-19
fn host_activation_posts_the_windows_activation_order_to_the_active_window() {
    let mut f = Fixture::new("host-activation");
    let hwnd = f.m.ext.get::<Win32>().user.desktop;
    f.m.ext.get_mut::<Win32>().user.top.push(hwnd);
    let time = Instant::now();
    assert!(post_at(&mut env(&mut f.m), 0x1c, 0, 0, time));
    assert!(post_at(&mut env(&mut f.m), 0x1c, 1, 0, time));
    let posted: Vec<_> = f.m.ext.get::<Win32>().user.queues[&0]
        .posted
        .iter()
        .map(|event| (event.hwnd, event.msg, event.wparam, event.lparam))
        .collect();
    assert_eq!(
        posted,
        [
            (hwnd, 0x86, 0, 0),
            (hwnd, 0x06, 0, 0),
            (hwnd, 0x1c, 0, 0),
            (hwnd, 0x1c, 1, 0),
            (hwnd, 0x86, 1, 0),
            (hwnd, 0x06, 1, 0),
        ]
    );
    f.cleanup();
}
