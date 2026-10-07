//! Native isolated-desktop geometry and host routing share these authored windows.
use super::super::{
    Win32, host_input,
    window::{Rect, WS_CHILD, WS_DISABLED, WS_POPUP, WS_VISIBLE, make_long},
};
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, Reg},
};
use adv_machine::{Clock, Machine, Outcome, layout::PARK_ADDR};

const ROOT: u32 = 0x10020;
const A: u32 = 0x10040;
const B: u32 = 0x10060;
const GRANDCHILD: u32 = 0x10080;
const STATIC: u32 = 0x100a0;

fn fixture() -> (Machine<JitCpu>, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    crate::Win32::install(
        &mut m,
        crate::Win32Config::new(".", "hit-test.exe", "target/hit-test-tests"),
    )
    .unwrap();
    m.spawn("hit-test", PARK_ADDR, &[], 65536).unwrap();
    let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, data);
    let user = &mut m.ext.get_mut::<Win32>().user;
    let button = user
        .classes
        .iter()
        .position(|c| c.name == "Button")
        .unwrap();
    let static_ = user
        .classes
        .iter()
        .position(|c| c.name == "Static")
        .unwrap();
    for (hwnd, parent, class, rect, children) in [
        (
            ROOT,
            0,
            button,
            Rect::new(1500, 800, 180, 180),
            vec![A, B, STATIC],
        ),
        (A, ROOT, button, Rect::new(10, 10, 100, 100), vec![]),
        (
            B,
            ROOT,
            button,
            Rect::new(20, 20, 100, 100),
            vec![GRANDCHILD],
        ),
        (GRANDCHILD, B, button, Rect::new(5, 5, 30, 30), vec![]),
        (STATIC, ROOT, static_, Rect::new(20, 20, 100, 100), vec![]),
    ] {
        let mut window = Window::new(
            hwnd,
            class,
            0,
            WS_VISIBLE | if parent == 0 { WS_POPUP } else { WS_CHILD },
            0,
            parent,
            rect,
            0,
        );
        window.children = children;
        assert!(user.windows.insert(&mut m.sys.objects, hwnd, window));
    }
    user.top = vec![ROOT];
    user.queue(0).active = ROOT;
    user.queue(0).focus = A;
    (m, data)
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

fn style(m: &mut Machine<JitCpu>, hwnd: u32, set: u32, clear: u32) {
    let window = m
        .ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut m.sys.objects, &hwnd)
        .unwrap();
    window.style = (window.style | set) & !clear;
}

fn query(m: &mut Machine<JitCpu>, data: u32, point: (i32, i32), expected: u32) {
    let mut registry = Registry::default();
    super::register(&mut registry);
    let api = registry.get("user32.dll", "WindowFromPoint").unwrap();
    assert_eq!(api.conv, adv_abi::Conv::Stdcall { argc: 2 });
    m.cpu.write_u32(data + 4, point.0 as u32).unwrap();
    m.cpu.write_u32(data + 8, point.1 as u32).unwrap();
    crate::util::set_last_error(&mut env(m), 0x12345678);
    assert!(matches!((api.handler)(&mut env(m)), Outcome::Return(v) if v == expected));
    assert_eq!(crate::util::last_error(&env(m)), 0x12345678);
}

#[test]
fn window_from_point_matches_native_visibility_disabled_static_hierarchy_and_order() {
    let (mut m, data) = fixture();
    query(&mut m, data, (1650, 950), ROOT);
    query(&mut m, data, (1530, 830), A);
    query(&mut m, data, (1520, 820), A);
    query(&mut m, data, (1620, 820), ROOT); // right edge is excluded.
    style(&mut m, A, 0, WS_VISIBLE);
    query(&mut m, data, (1530, 830), GRANDCHILD);
    style(&mut m, A, WS_VISIBLE | WS_DISABLED, 0);
    query(&mut m, data, (1530, 830), GRANDCHILD);
    style(&mut m, A, 0, WS_DISABLED | WS_VISIBLE);
    style(&mut m, B, WS_DISABLED, 0);
    query(&mut m, data, (1530, 830), ROOT);
    style(&mut m, B, 0, WS_DISABLED);
    m.ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut m.sys.objects, &ROOT)
        .unwrap()
        .children = vec![STATIC, A, B];
    query(&mut m, data, (1530, 830), GRANDCHILD);
    style(&mut m, STATIC, 4, 0);
    query(&mut m, data, (1530, 830), GRANDCHILD);
    style(&mut m, STATIC, 0x100, 4);
    query(&mut m, data, (1530, 830), STATIC);
    style(&mut m, ROOT, 0, WS_VISIBLE);
    query(&mut m, data, (1530, 830), 0);
    style(&mut m, ROOT, WS_VISIBLE | WS_DISABLED, 0);
    query(&mut m, data, (1530, 830), ROOT);
    query(&mut m, data, (1650, 950), ROOT);
    style(&mut m, ROOT, 0, WS_DISABLED);
    query(&mut m, data, (-10, -10), 0);
    query(&mut m, data, (i32::MAX, i32::MIN), 0);
}

#[test]
fn host_mouse_routes_to_hit_child_preserves_capture_and_keeps_keyboard_focus() {
    let (mut m, _) = fixture();
    style(&mut m, A, 0, WS_VISIBLE);
    let position = make_long(30, 30);
    for (msg, wp) in [(0x200, 0), (0x201, 1), (0x202, 0)] {
        assert!(host_input::post(&mut env(&mut m), msg, wp, position));
        let user = &mut m.ext.get_mut::<Win32>().user;
        let message = user.queue(0).posted.pop_front().unwrap();
        assert_eq!(
            (message.hwnd, message.msg, message.wparam, message.lparam),
            (GRANDCHILD, msg, wp, make_long(5, 5))
        );
        assert_eq!(message.pt, (1530, 830));
    }
    style(&mut m, A, WS_VISIBLE, 0);
    m.ext.get_mut::<Win32>().user.queue(0).capture = B;
    assert!(host_input::post(&mut env(&mut m), 0x200, 0, position));
    let message = m
        .ext
        .get_mut::<Win32>()
        .user
        .queue(0)
        .posted
        .pop_front()
        .unwrap();
    assert_eq!((message.hwnd, message.lparam), (B, make_long(10, 10)));
    m.ext.get_mut::<Win32>().user.queue(0).capture = 0xdeadbeef;
    assert!(host_input::post(&mut env(&mut m), 0x200, 0, position));
    assert_eq!(
        m.ext
            .get_mut::<Win32>()
            .user
            .queue(0)
            .posted
            .pop_front()
            .unwrap()
            .hwnd,
        A
    );
    assert!(host_input::post(&mut env(&mut m), 0x100, 13, 1));
    assert_eq!(
        m.ext
            .get_mut::<Win32>()
            .user
            .queue(0)
            .posted
            .pop_front()
            .unwrap()
            .hwnd,
        A
    );
    // Destroying children never receive new mouse messages; the shared traversal
    // falls back to their eligible sibling/parent without retaining a HWND.
    m.ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut m.sys.objects, &A)
        .unwrap()
        .destroying = true;
    assert!(host_input::post(&mut env(&mut m), 0x200, 0, position));
    assert_eq!(
        m.ext
            .get_mut::<Win32>()
            .user
            .queue(0)
            .posted
            .pop_front()
            .unwrap()
            .hwnd,
        GRANDCHILD
    );
}

#[test]
fn host_full_client_child_receives_frame_relative_move_down_up() {
    use super::super::super::window::{WS_CAPTION, WS_THICKFRAME};
    let (mut m, data) = fixture();
    let user = &mut m.ext.get_mut::<Win32>().user;
    let root = user.windows.get_mut(&mut m.sys.objects, &ROOT).unwrap();
    root.style = WS_VISIBLE | WS_CAPTION | WS_THICKFRAME;
    root.rect = Rect::new(120, 67, 1296, 759);
    root.children = vec![A];
    let child = user.windows.get_mut(&mut m.sys.objects, &A).unwrap();
    child.rect = Rect::new(0, 0, 1280, 720);
    child.children.clear();
    assert_eq!(user.client_origin(&m.sys.objects, ROOT), (128, 98));
    assert_eq!(user.client_origin(&m.sys.objects, A), (128, 98));
    query(&mut m, data, (408, 753), A);
    for (msg, wp) in [(0x200, 0), (0x201, 1), (0x202, 0)] {
        assert!(host_input::post(
            &mut env(&mut m),
            msg,
            wp,
            make_long(280, 655)
        ));
        let message = m
            .ext
            .get_mut::<Win32>()
            .user
            .queue(0)
            .posted
            .pop_front()
            .unwrap();
        assert_eq!(
            (message.hwnd, message.msg, message.wparam, message.lparam),
            (A, msg, wp, make_long(280, 655))
        );
        assert_eq!(message.pt, (408, 753));
    }
}
