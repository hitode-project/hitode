//! Observe the encoding already owned by the live USER window procedure.
use super::user;
use adv_abi::{Env, Handle, Registry, win32};

pub(super) fn register(registry: &mut Registry) {
    registry.register(is_window_unicode_ABI);
}

#[win32(user32, name = "IsWindowUnicode", level = state)]
fn is_window_unicode(env: &mut Env<'_>, hwnd: Handle) -> i32 {
    let Some(unicode) = user(env)
        .windows
        .get(&env.sys.objects, &hwnd.0)
        .map(|window| window.unicode)
    else {
        // Native NULL, invalid and destroyed HWNDs return FALSE with 1400.
        crate::util::set_last_error(env, 1400);
        return 0;
    };
    // A valid ANSI window also returns FALSE, preserving LastError. Destroy
    // callbacks retain the owner through WM_NCDESTROY and may still query it.
    i32::from(unicode)
}
