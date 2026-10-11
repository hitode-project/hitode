use super::*;
use crate::misc::test_support::{Fixture, env};
use std::time::{Duration, Instant};

#[test]
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
