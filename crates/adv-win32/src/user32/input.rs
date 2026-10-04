//! Queries use the configured guest input profile, never host layout/settings.
use super::{user, user_mut};
use crate::util::set_last_error;
use adv_abi::{AnsiStr, Env, GuestPtr, Handle, Registry, WideStr, win32};
use adv_cpu::backend::Prot;

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
#[win32(user32,name="SwapMouseButton",level=state)]
fn swap_mouse_button(env: &mut Env<'_>, swap: i32) -> u32 {
    u32::from(std::mem::replace(
        &mut user_mut(env).swap_buttons,
        swap != 0,
    ))
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        get_keyboard_layout_ABI,
        get_keyboard_layout_list_ABI,
        get_keyboard_layout_name_a_ABI,
        get_keyboard_layout_name_w_ABI,
        swap_mouse_button_ABI,
    ] {
        registry.register(api);
    }
}

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
