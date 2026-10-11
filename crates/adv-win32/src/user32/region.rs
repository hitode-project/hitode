//! Rectangular windows retain their default region; shaped windows fail explicitly.
use super::*;
use adv_abi::Handle;

#[win32(user32, name = "SetWindowRgn", level = safe_fail)]
fn set(env: &mut Env<'_>, hwnd: Handle, region: Handle, _redraw: i32) -> i32 {
    if !user(env).windows.contains_key(&env.sys.objects, &hwnd.0) {
        crate::util::set_last_error(env, 1400);
        return 0;
    }
    if region.0 != 0 {
        adv_abi::warn_unavailable(env, "SetWindowRgn", "shaped window regions are unavailable");
        crate::util::set_last_error(env, 50);
        return 0;
    }
    // NULL restores the default rectangular window. No owned shape exists yet.
    1
}
#[win32(user32, name = "GetWindowRgn", level = safe_fail)]
fn get(env: &mut Env<'_>, hwnd: Handle, _region: Handle) -> i32 {
    if !user(env).windows.contains_key(&env.sys.objects, &hwnd.0) {
        crate::util::set_last_error(env, 1400);
    }
    // ERROR also describes an ordinary window with no explicit region.
    0
}
pub(super) fn register(reg: &mut Registry) {
    reg.register(set_ABI);
    reg.register(get_ABI);
}
