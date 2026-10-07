//! Queries use the configured guest input profile, never host layout/settings.
use super::{User, user, user_mut, window::Rect};
use crate::util::set_last_error;
use adv_abi::{AnsiStr, Env, GuestPtr, Handle, Registry, WideStr, win32};
use adv_cpu::backend::{GuestExt, Prot};

fn display_bounds(user: &User) -> Rect {
    Rect::new(0, 0, user.screen.0.max(1), user.screen.1.max(1))
}

fn clip_bounds(user: &User) -> Rect {
    user.cursor_clip.unwrap_or_else(|| display_bounds(user))
}

fn intersect_display(user: &User, rect: Rect) -> Option<Rect> {
    let screen = display_bounds(user);
    let clipped = Rect {
        left: rect.left.max(screen.left),
        top: rect.top.max(screen.top),
        right: rect.right.min(screen.right),
        bottom: rect.bottom.min(screen.bottom),
    };
    (clipped.left <= clipped.right && clipped.top <= clipped.bottom).then_some(clipped)
}

/// Keep all guest cursor updates in the same desktop constraint, without
/// changing the host cursor or the host's clipping rectangle.
pub(super) fn set_cursor_position(user: &mut User, position: (i32, i32)) -> (i32, i32) {
    let rect = clip_bounds(user);
    let coordinate = |value, lower, upper: i32| {
        if value < lower {
            lower
        } else if value >= upper {
            upper.saturating_sub(1)
        } else {
            value
        }
    };
    let screen = display_bounds(user);
    user.cursor_pos = (
        coordinate(position.0, rect.left, rect.right).clamp(0, screen.right - 1),
        coordinate(position.1, rect.top, rect.bottom).clamp(0, screen.bottom - 1),
    );
    user.cursor_pos
}

/// Guest display policy: intersect a retained constraint with the resized
/// desktop, or release it when disjoint. Never retain an off-desktop position.
pub(super) fn reconcile_cursor_clip(user: &mut User) {
    user.cursor_clip = user
        .cursor_clip
        .and_then(|rect| intersect_display(user, rect));
    set_cursor_position(user, user.cursor_pos);
}

#[win32(user32, name = "GetClipCursor", level = state)]
fn get_clip_cursor(env: &mut Env<'_>, out: GuestPtr<Rect>) -> i32 {
    if adv_abi::guest::check_range(env.cpu, out.addr(), 16, Prot::WRITE).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    let rect = clip_bounds(user(env));
    let fields = [rect.left, rect.top, rect.right, rect.bottom];
    let mut bytes = [0; 16];
    for (coordinate, output) in fields.into_iter().zip(bytes.chunks_exact_mut(4)) {
        output.copy_from_slice(&coordinate.to_le_bytes());
    }
    if env.cpu.write(out.addr(), &bytes).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    1
}

#[win32(user32, name = "ClipCursor", level = state)]
fn clip_cursor(env: &mut Env<'_>, pointer: GuestPtr<Rect>) -> i32 {
    let rect = if pointer.is_null() {
        None
    } else {
        if adv_abi::guest::check_range(env.cpu, pointer.addr(), 16, Prot::READ).is_err() {
            set_last_error(env, 998);
            return 0;
        }
        let Some(rect) = Rect::read(env.cpu, pointer.addr()) else {
            set_last_error(env, 998);
            return 0;
        };
        if rect.left > rect.right || rect.top > rect.bottom {
            // Wine's test_ClipCursor_desktop records this Windows error value.
            set_last_error(env, 5); // ERROR_ACCESS_DENIED, preserving the old constraint.
            return 0;
        }
        let Some(rect) = intersect_display(user(env), rect) else {
            adv_abi::warn_unavailable(
                env,
                "user32.dll!ClipCursor",
                "clip rectangle outside guest desktop is not supported",
            );
            set_last_error(env, 50);
            return 0;
        };
        Some(rect)
    };
    let state = user_mut(env);
    state.cursor_clip = rect;
    reconcile_cursor_clip(state);
    1
}

#[win32(user32, name = "SetKeyboardState", level = state)]
pub(super) fn set_keyboard_state(env: &mut Env<'_>, state: GuestPtr<u8>) -> i32 {
    let mut keys = [0; 256];
    if adv_abi::guest::check_range(env.cpu, state.addr(), 256, Prot::READ).is_err()
        || env.cpu.read(state.addr(), &mut keys).is_err()
    {
        set_last_error(env, 998);
        return 0;
    }
    // The queue owns logical down/toggle bits. This does not change physical
    // input, other threads, or messages already waiting in the queue.
    for key in &mut keys {
        *key &= 0x81;
    }
    let context = env.context;
    user_mut(env).queue(context).keyboard.0 = keys;
    1
}

#[win32(user32, name = "GetCursorInfo", level = state)]
fn get_cursor_info(env: &mut Env<'_>, out: GuestPtr<u8>) -> i32 {
    // Win32 CURSORINFO is 20 bytes: DWORD, DWORD, HCURSOR, POINT. Keep
    // the ABI independent of the host pointer width and validate before writing.
    let pointer = out.addr();
    if adv_abi::guest::check_range(env.cpu, pointer, 4, Prot::READ).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    let size = env.cpu.read_u32(pointer).unwrap_or(0);
    if size != 20 {
        set_last_error(env, 87);
        return 0;
    }
    if adv_abi::guest::check_range(env.cpu, pointer, 20, Prot::WRITE).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    let state = user(env);
    let words = [
        20,
        u32::from(state.cursor_handle != 0 && state.cursor_count >= 0),
        state.cursor_handle,
        state.cursor_pos.0 as u32,
        state.cursor_pos.1 as u32,
    ];
    let mut bytes = [0; 20];
    for (word, output) in words.into_iter().zip(bytes.chunks_exact_mut(4)) {
        output.copy_from_slice(&word.to_le_bytes());
    }
    if env.cpu.write(pointer, &bytes).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    1
}

#[win32(user32, name = "GetCursor", level = state)]
fn get_cursor(env: &mut Env<'_>) -> Handle {
    Handle(user(env).cursor_handle)
}

pub(super) fn layout_for_codepage(codepage: u32) -> u32 {
    match codepage {
        932 => 0x0411_0411,
        936 => 0x0804_0804,
        _ => 0x0409_0409,
    }
}
#[win32(user32,name="GetKeyboardLayout",level=state)]
fn get_keyboard_layout(env: &mut Env<'_>, thread: u32) -> Handle {
    if thread != 0
        && !env
            .sys
            .contexts
            .iter()
            .any(|c| c.tid == thread && !c.is_done())
    {
        set_last_error(env, 87);
        return Handle(0);
    }
    Handle(user(env).keyboard_layout)
}
#[win32(user32,name="GetKeyboardLayoutList",level=state)]
fn get_keyboard_layout_list(env: &mut Env<'_>, count: i32, list: GuestPtr<Handle>) -> i32 {
    if count < 0 {
        set_last_error(env, 87);
        return 0;
    }
    if count == 0 {
        return 1;
    }
    let layout = user(env).keyboard_layout;
    if GuestPtr::<u32>::new(list.addr())
        .write(env.cpu, layout)
        .is_err()
    {
        set_last_error(env, 87);
        return 0;
    }
    1
}
fn layout_name(env: &mut Env<'_>, pointer: u32, wide: bool) -> u32 {
    let name = format!("{:08X}", user(env).keyboard_layout & 0xffff);
    let bytes: Vec<u8> = if wide {
        name.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        name.bytes().chain([0]).collect()
    };
    if adv_abi::guest::check_range(env.cpu, pointer, bytes.len() as u32, Prot::WRITE).is_err()
        || env.cpu.write(pointer, &bytes).is_err()
    {
        set_last_error(env, 87);
        return 0;
    }
    1
}
#[win32(user32,name="GetKeyboardLayoutNameA",level=state)]
fn get_keyboard_layout_name_a(env: &mut Env<'_>, out: AnsiStr) -> u32 {
    layout_name(env, out.0.addr(), false)
}
#[win32(user32,name="GetKeyboardLayoutNameW",level=state)]
fn get_keyboard_layout_name_w(env: &mut Env<'_>, out: WideStr) -> u32 {
    layout_name(env, out.0.addr(), true)
}
fn map_virtual_key(env: &mut Env<'_>, code: u32, kind: u32, api: &str) -> u32 {
    match adv_os::keyboard::map_virtual_key(user(env).keyboard_layout, code, kind) {
        Some(value) => value,
        None => {
            adv_abi::warn_unavailable(
                env,
                api,
                "the keyboard profile or mapping type is unavailable",
            );
            set_last_error(env, 50);
            0
        }
    }
}
#[win32(user32, name = "MapVirtualKeyA", level = state)]
fn map_virtual_key_a(env: &mut Env<'_>, code: u32, kind: u32) -> u32 {
    map_virtual_key(env, code, kind, "user32.dll!MapVirtualKeyA")
}
#[win32(user32, name = "MapVirtualKeyW", level = state)]
fn map_virtual_key_w(env: &mut Env<'_>, code: u32, kind: u32) -> u32 {
    map_virtual_key(env, code, kind, "user32.dll!MapVirtualKeyW")
}
#[win32(user32,name="SwapMouseButton",level=state)]
fn swap_mouse_button(env: &mut Env<'_>, swap: i32) -> u32 {
    u32::from(std::mem::replace(
        &mut user_mut(env).swap_buttons,
        swap != 0,
    ))
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        set_keyboard_state_ABI,
        get_cursor_info_ABI,
        get_cursor_ABI,
        get_clip_cursor_ABI,
        clip_cursor_ABI,
        get_keyboard_layout_ABI,
        get_keyboard_layout_list_ABI,
        get_keyboard_layout_name_a_ABI,
        get_keyboard_layout_name_w_ABI,
        map_virtual_key_a_ABI,
        map_virtual_key_w_ABI,
        swap_mouse_button_ABI,
    ] {
        registry.register(api);
    }
}

#[cfg(test)]
mod cursor_tests;

#[cfg(test)]
mod keymap_tests;

#[cfg(test)]
mod tests {
    use super::super::host_input::{mouse_mapping, post, release};
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, GuestExt, Reg},
    };
    use adv_machine::{Clock, Machine, Outcome};
    #[test]
    fn cursor_info_tracks_shape_visibility_position_and_rejects_bad_outputs() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
        let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        crate::Win32::install(
            &mut m,
            crate::Win32Config::new(".", "cursor.exe", "target/cursor-tests"),
        )
        .unwrap();
        m.spawn("cursor", adv_machine::layout::PARK_ADDR, &[], 65536)
            .unwrap();
        let page = m.sys.vm.alloc(&mut m.cpu, 8192, Prot::RW).unwrap();
        m.cpu.set_reg(Reg::Esp, page);
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        env.cpu.write_u32(page + 4, 0).unwrap();
        env.cpu.write_u32(page + 8, 32512).unwrap();
        let Outcome::Return(cursor) = super::super::load_cursor_a(&mut env) else {
            panic!()
        };
        assert_ne!(cursor, 0);
        env.cpu.write_u32(page + 4, cursor).unwrap();
        assert!(matches!(
            super::super::set_cursor(&mut env),
            Outcome::Return(0)
        ));
        assert_eq!(get_cursor(&mut env), Handle(cursor));
        user_mut(&mut env).cursor_pos = (-20, 100);
        let output = page + 64;
        env.cpu.write_u32(output, 20).unwrap();
        assert_eq!(get_cursor_info(&mut env, GuestPtr::new(output)), 1);
        let read = |env: &Env<'_>| {
            (0..5)
                .map(|i| env.cpu.read_u32(output + i * 4).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(read(&env), [20, 1, cursor, (-20i32) as u32, 100]);
        env.cpu.write_u32(page + 4, 0).unwrap();
        assert!(matches!(
            super::super::show_cursor(&mut env),
            Outcome::Return(u32::MAX)
        ));
        assert_eq!(get_cursor_info(&mut env, GuestPtr::new(output)), 1);
        assert_eq!(read(&env)[1], 0);
        assert!(
            matches!(super::super::set_cursor(&mut env), Outcome::Return(value) if value == cursor)
        );
        assert_eq!(get_cursor(&mut env), Handle(0));
        env.cpu.write_u32(output, 19).unwrap();
        let unchanged = read(&env);
        assert_eq!(get_cursor_info(&mut env, GuestPtr::new(output)), 0);
        assert_eq!(read(&env), unchanged);
        // The size is readable, but the output would cross a read-only page.
        let edge = page + 4092;
        env.cpu.write(edge, &[0x5a; 20]).unwrap();
        env.cpu.write_u32(edge, 20).unwrap();
        env.cpu.protect(page + 4096, 4096, Prot::READ).unwrap();
        assert_eq!(get_cursor_info(&mut env, GuestPtr::new(edge)), 0);
        assert_eq!(env.cpu.read_u32(edge + 4).unwrap(), 0x5a5a5a5a);
        assert_eq!(get_cursor_info(&mut env, GuestPtr::new(0)), 0);
    }
    #[test]
    fn host_inverse_then_guest_mapping_is_once_and_preserves_ctrl_wheel_and_extra_buttons() {
        for host in [false, true] {
            for guest in [false, true] {
                for physical in [
                    0x200, 0x201, 0x202, 0x203, 0x204, 0x205, 0x206, 0x20a, 0x20b, 0x20e, 0x100,
                ] {
                    for bits in [0, 1, 2, 3, 0x780028, 0xffff003b] {
                        let native = mouse_mapping(physical, bits, host);
                        let normalized = mouse_mapping(native.0, native.1, host);
                        assert_eq!(normalized, (physical, bits));
                        let logical = mouse_mapping(normalized.0, normalized.1, guest);
                        assert_eq!(logical, mouse_mapping(physical, bits, guest));
                        assert_eq!(logical.1 & !3, bits & !3);
                    }
                }
            }
        }
    }
    #[test]
    fn swapped_mouse_events_keep_async_keys_physical_and_release_the_logical_button() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
        let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        crate::Win32::install(
            &mut m,
            crate::Win32Config::new(".", "input-tests.exe", "target/input-contract-tests"),
        )
        .unwrap();
        m.spawn("input", adv_machine::layout::PARK_ADDR, &[], 65536)
            .unwrap();
        let stack = m
            .sys
            .vm
            .alloc(&mut m.cpu, 4096, Prot::READ | Prot::WRITE)
            .unwrap();
        m.cpu.set_reg(Reg::Esp, stack);
        let hwnd = m.ext.get::<crate::Win32>().user.desktop;
        m.ext.get_mut::<crate::Win32>().user.top.push(hwnd);
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        assert_eq!(swap_mouse_button(&mut env, 1), 0);
        assert!(post(&mut env, 0x201, 1 | 8, 0));
        let state = user(&env);
        let msg = state.queues[&0].posted.back().unwrap();
        assert_eq!((msg.msg, msg.wparam), (0x204, 2 | 8));
        assert_eq!((state.keys[1] & 0x80, state.keys[2] & 0x80), (0x80, 0));
        env.cpu.write_u32(stack + 4, 1).unwrap();
        assert!(matches!(
            super::super::get_async_key_state(&mut env),
            Outcome::Return(0x8000)
        ));
        env.cpu.write_u32(stack + 4, 2).unwrap();
        assert!(matches!(
            super::super::get_async_key_state(&mut env),
            Outcome::Return(0)
        ));
        assert_eq!(release(&mut env), 1);
        let msg = user(&env).queues[&0].posted.back().unwrap();
        assert_eq!(msg.msg, 0x205);
        assert_eq!(swap_mouse_button(&mut env, 0), 1);
        assert_eq!(get_keyboard_layout(&mut env, 0), Handle(0x04110411));
        assert_eq!(get_keyboard_layout_list(&mut env, 0, GuestPtr::new(0)), 1);
        assert_eq!(
            get_keyboard_layout_list(&mut env, 1, GuestPtr::new(stack + 32)),
            1
        );
        assert_eq!(env.cpu.read_u32(stack + 32).unwrap(), 0x04110411);
        assert_eq!(layout_name(&mut env, stack + 64, true), 1);
        let mut bytes = [0; 18];
        env.cpu.read(stack + 64, &mut bytes).unwrap();
        let expected: Vec<u8> = "00000411"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(&bytes[..], expected);
        assert_eq!(get_keyboard_layout(&mut env, u32::MAX), Handle(0));
        assert_eq!(layout_name(&mut env, u32::MAX - 1, true), 0);
    }
}
