//! Layered presentation is unavailable until it has a shared scene owner.
use crate::{user32::user, util::set_last_error};
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_machine::Env;
fn unavailable(env: &mut Env<'_>, hwnd: u32, api: &str) -> i32 {
    let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        set_last_error(env, 1400);
        return 0;
    };
    if w.ex_style & 0x80000 == 0 {
        set_last_error(env, 87);
        return 0;
    }
    adv_abi::warn_unavailable(
        env,
        api,
        "layered window composition has no shared scene owner",
    );
    set_last_error(env, 50);
    0
}
#[win32(user32,name="UpdateLayeredWindow",level=safe_fail)]
fn update(
    env: &mut Env<'_>,
    hwnd: Handle,
    _dst: Handle,
    _position: GuestPtr<()>,
    _size: GuestPtr<()>,
    _src: Handle,
    _origin: GuestPtr<()>,
    _key: u32,
    _blend: GuestPtr<()>,
    _flags: u32,
) -> i32 {
    unavailable(env, hwnd.0, "user32.dll!UpdateLayeredWindow")
}
#[win32(user32,name="UpdateLayeredWindowIndirect",level=safe_fail)]
fn indirect(env: &mut Env<'_>, hwnd: Handle, _info: GuestPtr<()>) -> i32 {
    unavailable(env, hwnd.0, "user32.dll!UpdateLayeredWindowIndirect")
}
#[win32(user32,name="SetLayeredWindowAttributes",level=safe_fail)]
fn set(env: &mut Env<'_>, hwnd: Handle, _key: u32, _alpha: u32, _flags: u32) -> i32 {
    unavailable(env, hwnd.0, "user32.dll!SetLayeredWindowAttributes")
}
#[win32(user32,name="GetLayeredWindowAttributes",level=safe_fail)]
fn get(
    env: &mut Env<'_>,
    hwnd: Handle,
    _key: GuestPtr<u32>,
    _alpha: GuestPtr<u8>,
    _flags: GuestPtr<u32>,
) -> i32 {
    // No attribute state was accepted by SetLayeredWindowAttributes.
    set_last_error(
        env,
        if user(env).is_window(&env.sys.objects, hwnd.0) {
            87
        } else {
            1400
        },
    );
    0
}
pub(super) fn register(reg: &mut Registry) {
    for api in [update_ABI, indirect_ABI, set_ABI, get_ABI] {
        reg.register(api);
    }
}
