//! Touch subscriptions belong to USER windows; this display has no touch packets.
use super::user;
use crate::util::set_last_error;
use adv_abi::{
    GuestPtr, Handle, Registry,
    types::{GESTUREINFO, TOUCHINPUT},
    win32,
};
use adv_cpu::backend::GuestExt;
use adv_machine::Env;
fn fail(env: &mut Env<'_>, error: u32) -> i32 {
    set_last_error(env, error);
    0
}
fn subscription(env: &mut Env<'_>, hwnd: u32, flags: Option<u32>) -> i32 {
    let Some(window) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        return fail(env, 1400);
    };
    if window.thread != env.context {
        return fail(env, 5);
    }
    if flags.is_some_and(|flags| flags & !3 != 0) {
        return fail(env, 87);
    }
    env.ext
        .get_mut::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
        .unwrap()
        .touch_flags = flags;
    1
}
#[win32(user32,name="RegisterTouchWindow",level=implement)]
fn register_window(env: &mut Env<'_>, hwnd: Handle, flags: u32) -> i32 {
    subscription(env, hwnd.0, Some(flags))
}
#[win32(user32,name="UnregisterTouchWindow",level=implement)]
fn unregister_window(env: &mut Env<'_>, hwnd: Handle) -> i32 {
    subscription(env, hwnd.0, None)
}
#[win32(user32,name="GetTouchInputInfo",level=implement)]
fn touch_info(
    env: &mut Env<'_>,
    _input: Handle,
    _count: u32,
    _out: GuestPtr<TOUCHINPUT>,
    size: i32,
) -> i32 {
    if size != std::mem::size_of::<TOUCHINPUT>() as i32 {
        return fail(env, 87);
    }
    fail(env, 6)
}
#[win32(user32,name="GetGestureInfo",level=implement)]
fn gesture_info(env: &mut Env<'_>, _input: Handle, out: GuestPtr<GESTUREINFO>) -> i32 {
    if out.is_null()
        || env.cpu.read_u32(out.addr()).ok() != Some(std::mem::size_of::<GESTUREINFO>() as u32)
    {
        return fail(env, 87);
    }
    fail(env, 6)
}
#[win32(user32,name="CloseTouchInputHandle",level=implement)]
fn close_touch(env: &mut Env<'_>, _input: Handle) -> i32 {
    fail(env, 6)
}
#[win32(user32,name="CloseGestureInfoHandle",level=implement)]
fn close_gesture(env: &mut Env<'_>, _input: Handle) -> i32 {
    fail(env, 6)
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        register_window_ABI,
        unregister_window_ABI,
        touch_info_ABI,
        gesture_info_ABI,
        close_touch_ABI,
        close_gesture_ABI,
    ] {
        registry.register(api);
    }
}
