use super::*;
use crate::{Win32, Win32Config, util::last_error};
use adv_abi::Conv;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_machine::{Clock, Machine, Outcome, layout::PARK_ADDR};

fn fixture() -> (Machine<JitCpu>, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for name in ["GetClipCursor", "ClipCursor"] {
        assert_eq!(
            registry.get("user32.dll", name).unwrap().conv,
            Conv::Stdcall { argc: 1 }
        );
        assert!(registry.dynamic_available("user32.dll", name));
    }
    assert!(registry.signature_conflicts().is_empty());
    let cpu = JitCpu::new(PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let mut config = Win32Config::new(".", "cursor-clip.exe", "target/cursor-clip-tests");
    config.screen = (320, 240);
    Win32::install(&mut machine, config).unwrap();
    assert_eq!(
        machine
            .spawn("cursor owner", PARK_ADDR, &[], 65536)
            .unwrap(),
        0
    );
    assert_eq!(
        machine
            .spawn("cursor observer", PARK_ADDR, &[], 65536)
            .unwrap(),
        1
    );
    let data = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, data);
    (machine, data)
}

fn env(machine: &mut Machine<JitCpu>, context: u32) -> Env<'_> {
    Env {
        context,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}

fn query(env: &mut Env<'_>, out: u32) -> Rect {
    assert_eq!(get_clip_cursor(env, GuestPtr::new(out)), 1);
    Rect::read(env.cpu, out).unwrap()
}

fn clip(env: &mut Env<'_>, input: u32, rect: Rect) {
    assert!(rect.write(env.cpu, input));
    assert_eq!(clip_cursor(env, GuestPtr::new(input)), 1);
}

fn position(env: &mut Env<'_>, x: i32, y: i32) {
    let stack = env.cpu.reg(Reg::Esp);
    env.cpu.write_u32(stack + 4, x as u32).unwrap();
    env.cpu.write_u32(stack + 8, y as u32).unwrap();
    assert!(matches!(
        super::super::set_cursor_pos(env),
        Outcome::Return(1)
    ));
}

#[test]
fn cursor_clipping_queries_settings_release_and_position_are_shared_by_threads() {
    let (mut machine, data) = fixture();
    let input = data + 32;
    let out = data + 64;
    let mut owner = env(&mut machine, 0);
    set_last_error(&mut owner, 0x12345678);
    assert_eq!(query(&mut owner, out), Rect::new(0, 0, 320, 240));
    assert_eq!(last_error(&owner), 0x12345678);
    let rect = Rect::new(10, 20, 90, 60);
    clip(&mut owner, input, rect);
    assert_eq!(user(&owner).cursor_pos, (99, 79));
    assert_eq!(last_error(&owner), 0x12345678);
    let mut observer = env(&mut machine, 1);
    set_last_error(&mut observer, 0xabcdef01);
    assert_eq!(query(&mut observer, out), rect);
    position(&mut observer, -100, 200);
    assert_eq!(user(&observer).cursor_pos, (10, 79));
    observer.cpu.write_u32(data + 4, out).unwrap();
    assert!(matches!(
        super::super::get_cursor_pos(&mut observer),
        Outcome::Return(1)
    ));
    assert_eq!(observer.cpu.read_u32(out).unwrap(), 10);
    assert_eq!(observer.cpu.read_u32(out + 4).unwrap(), 79);
    assert_eq!(clip_cursor(&mut observer, GuestPtr::new(0)), 1);
    assert_eq!(last_error(&observer), 0xabcdef01);
    let mut owner = env(&mut machine, 0);
    assert_eq!(query(&mut owner, out), Rect::new(0, 0, 320, 240));
    position(&mut owner, -100, 500);
    assert_eq!(user(&owner).cursor_pos, (0, 239));
    clip(&mut owner, input, Rect::new(-100, -100, 1000, 1000));
    assert_eq!(query(&mut owner, out), Rect::new(0, 0, 320, 240));
    // Degenerate bounds follow the Windows expectations in Wine's
    // dlls/user32/tests/input.c test_SetCursorPos empty-rectangle cases:
    // https://github.com/wine-mirror/wine/blob/master/dlls/user32/tests/input.c
    position(&mut owner, 50, 50);
    clip(&mut owner, input, Rect::new(50, 50, 0, 0));
    assert_eq!(query(&mut owner, out), Rect::new(50, 50, 0, 0));
    assert_eq!(user(&owner).cursor_pos, (49, 49));
    position(&mut owner, 48, 48);
    assert_eq!(user(&owner).cursor_pos, (50, 50));
}

#[test]
fn cursor_clipping_failures_preserve_rectangles_positions_and_output_buffers() {
    let (mut machine, data) = fixture();
    let input = data + 32;
    let out = data + 64;
    let mut env = env(&mut machine, 0);
    let valid = Rect::new(10, 20, 90, 60);
    clip(&mut env, input, valid);
    let position = user(&env).cursor_pos;
    // Wine's test_ClipCursor_desktop records ERROR_ACCESS_DENIED on Windows.
    for invalid in [
        Rect {
            left: 20,
            right: 19,
            ..valid
        },
        Rect {
            top: 40,
            bottom: 39,
            ..valid
        },
    ] {
        assert!(invalid.write(env.cpu, input));
        assert_eq!(clip_cursor(&mut env, GuestPtr::new(input)), 0);
        assert_eq!(last_error(&env), 5);
        assert_eq!(query(&mut env, out), valid);
        assert_eq!(user(&env).cursor_pos, position);
    }
    assert!(Rect::new(400, 0, 10, 10).write(env.cpu, input));
    assert_eq!(clip_cursor(&mut env, GuestPtr::new(input)), 0);
    assert_eq!(last_error(&env), 50);
    assert_eq!(query(&mut env, out), valid);
    assert_eq!(user(&env).cursor_pos, position);
    let diagnostics = env.ext.get::<adv_abi::ApiDiagnostics>();
    assert!(diagnostics.warnings.iter().any(|((api, reason), count)| {
        api == "user32.dll!ClipCursor" && reason.contains("outside guest desktop") && *count == 1
    }));
    for pointer in [0x700000, u32::MAX - 7] {
        assert_eq!(clip_cursor(&mut env, GuestPtr::new(pointer)), 0);
        assert_eq!(last_error(&env), 998);
        assert_eq!(query(&mut env, out), valid);
        assert_eq!(user(&env).cursor_pos, position);
    }
    let edge = data + 4096 - 8;
    env.cpu.write(edge, &[0x5a; 16]).unwrap();
    env.cpu.protect(data + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(get_clip_cursor(&mut env, GuestPtr::new(edge)), 0);
    assert_eq!(last_error(&env), 998);
    let mut bytes = [0; 16];
    env.cpu.read(edge, &mut bytes).unwrap();
    assert_eq!(bytes, [0x5a; 16]);
    for pointer in [0, data + 4096, u32::MAX - 7] {
        assert_eq!(get_clip_cursor(&mut env, GuestPtr::new(pointer)), 0);
        assert_eq!(last_error(&env), 998);
    }
    env.cpu.protect(data + 4096, 4096, Prot::WRITE).unwrap();
    assert_eq!(clip_cursor(&mut env, GuestPtr::new(edge)), 0);
    assert_eq!(last_error(&env), 998);
    assert_eq!(query(&mut env, out), valid);
    assert_eq!(user(&env).cursor_pos, position);
}

fn add_window(machine: &mut Machine<JitCpu>, hwnd: u32, parent: u32, rect: Rect) {
    use super::super::window::{ControlState, WS_CHILD, WS_POPUP, WS_VISIBLE, Window};
    let window = Window {
        dc_style: 0,
        hwnd,
        class: 0,
        proc: 0,
        unicode: false,
        style: WS_VISIBLE | if parent == 0 { WS_POPUP } else { WS_CHILD },
        ex_style: 0,
        parent,
        owner: 0,
        id: 0,
        instance: 0,
        rect,
        placement: super::super::PlacementState::new(rect),
        text: String::new(),
        extra: Vec::new(),
        user_data: 0,
        thread: 0,
        children: Vec::new(),
        font: 0,
        needs_paint: false,
        created_us: 0,
        initial_geometry_reported: true,
        destroying: false,
        control: ControlState::default(),
        auto_clicks: 0,
        hidden_popup: false,
    };
    let state = &mut machine.ext.get_mut::<Win32>().user;
    assert!(state.windows.insert(&mut machine.sys.objects, hwnd, window));
    if parent == 0 {
        state.top.push(hwnd);
    } else {
        state
            .windows
            .get_mut(&mut machine.sys.objects, &parent)
            .unwrap()
            .children
            .push(hwnd);
    }
}

#[test]
fn cursor_clipping_frontend_messages_use_clipped_screen_and_target_client_coordinates() {
    use super::super::{host_input::post, window::make_long};
    let (mut machine, data) = fixture();
    let top = 0x10020;
    let child = 0x10030;
    add_window(&mut machine, top, 0, Rect::new(100, 200, 200, 100));
    add_window(&mut machine, child, top, Rect::new(10, 20, 30, 40));
    let mut env = env(&mut machine, 0);
    clip(&mut env, data + 32, Rect::new(120, 230, 30, 10));
    user_mut(&mut env).queue(0).capture = child;
    for (message, point, screen, lp) in [
        (0x200, (0, 0), (120, 230), (10, 10)),
        (0x201, (300, 200), (149, 239), (39, 19)),
        (0x20a, (300, 200), (149, 239), (149, 239)),
        (0x20e, (0, 0), (120, 230), (120, 230)),
    ] {
        assert!(post(&mut env, message, 0, make_long(point.0, point.1)));
        let state = user(&env);
        assert_eq!(state.cursor_pos, screen);
        let msg = state.queues[&0].posted.back().unwrap();
        assert_eq!(
            (msg.hwnd, msg.msg, msg.lparam, msg.pt),
            (child, message, make_long(lp.0, lp.1), screen)
        );
    }
    user_mut(&mut env).queue(0).capture = 0;
    // Without capture, choose the child under the clipped physical cursor,
    // then express the position in that target's client coordinates.
    for (point, target, local) in [((0, 0), child, (10, 10)), ((300, 200), top, (49, 39))] {
        assert!(post(&mut env, 0x200, 0, make_long(point.0, point.1)));
        let msg = user(&env).queues[&0].posted.back().unwrap();
        assert_eq!(
            (msg.hwnd, msg.lparam),
            (target, make_long(local.0, local.1))
        );
    }
}
