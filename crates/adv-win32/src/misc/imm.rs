//! The guest has no IME provider, HIMC owner or host composition bridge.
//! Query/set fronts preserve native absent-context errors; disabling an already
//! absent association needs no fabricated handle or per-window context cache.
use crate::{Win32, util::set_last_error};
use adv_abi::{Env, GuestPtr, Handle, Registry, win32};

const UNAVAILABLE: &str = "guest IME/input-context provider and host composition are unavailable";

fn warn(env: &mut Env<'_>, api: &str) {
    adv_abi::warn_unavailable(env, api, UNAVAILABLE);
}

pub(crate) fn get_context(env: &mut Env<'_>, window: Handle) -> Handle {
    // No context exists for an invalid HWND either. Native NULL HWND fails
    // without changing LastError, rather than inventing a context association.
    if env
        .ext
        .get::<Win32>()
        .user
        .is_window(&env.sys.objects, window.0)
    {
        warn(env, "imm32.dll!ImmGetContext");
    }
    Handle(0)
}

fn getter_failure(env: &mut Env<'_>, context: Handle, api: &str) -> i32 {
    if context.0 == 0 {
        warn(env, api);
        // Both native NULL-HIMC getters leave the caller's LastError intact.
    } else {
        set_last_error(env, 6);
    }
    0
}

#[win32(imm32, name = "ImmGetConversionStatus", level = safe_fail)]
fn get_conversion(
    env: &mut Env<'_>,
    context: Handle,
    _conversion: GuestPtr<u32>,
    _sentence: GuestPtr<u32>,
) -> i32 {
    // Validate HIMC before either output. No valid context can exist in this
    // profile, so even NULL/readonly/wrapping outputs are never accessed.
    getter_failure(env, context, "imm32.dll!ImmGetConversionStatus")
}

#[win32(imm32, name = "ImmGetOpenStatus", level = state)]
fn get_open(env: &mut Env<'_>, context: Handle) -> i32 {
    getter_failure(env, context, "imm32.dll!ImmGetOpenStatus")
}

fn setter_failure(env: &mut Env<'_>, api: &str) -> i32 {
    warn(env, api);
    // Native setters reject NULL HIMC with ERROR_INVALID_HANDLE, including
    // attempts to close/clear the modes. Invalid context precedes mode flags.
    set_last_error(env, 6);
    0
}

#[win32(imm32, name = "ImmSetConversionStatus", level = safe_fail)]
fn set_conversion(env: &mut Env<'_>, _context: Handle, _conversion: u32, _sentence: u32) -> i32 {
    setter_failure(env, "imm32.dll!ImmSetConversionStatus")
}

#[win32(imm32, name = "ImmSetOpenStatus", level = safe_fail)]
fn set_open(env: &mut Env<'_>, _context: Handle, _open: i32) -> i32 {
    setter_failure(env, "imm32.dll!ImmSetOpenStatus")
}

#[win32(imm32, name = "ImmReleaseContext", level = state)]
fn release(_env: &mut Env<'_>, _window: Handle, _context: Handle) -> i32 {
    // Native ReleaseContext is a no-op TRUE even for invalid HWND/HIMC and
    // preserves LastError. There are no borrowed HIMC references to release.
    // In particular, an unrelated Objects handle is never closed here.
    1
}

#[win32(imm32, name = "ImmAssociateContext", level = state)]
fn associate(env: &mut Env<'_>, window: Handle, context: Handle) -> Handle {
    if context.0 != 0 {
        warn(env, "imm32.dll!ImmAssociateContext");
        set_last_error(env, 6);
        return Handle(0);
    }
    if env
        .ext
        .get::<Win32>()
        .user
        .is_window(&env.sys.objects, window.0)
    {
        // This is a real, already-disabled guest association with previous
        // context NULL. No host IME, handle or input route is created.
        set_last_error(env, 0);
    }
    Handle(0)
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        get_conversion_ABI,
        get_open_ABI,
        set_conversion_ABI,
        set_open_ABI,
        release_ABI,
        associate_ABI,
    ] {
        registry.register(api);
    }
}
