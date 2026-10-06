//! Default IME window state for a personality without an IME window.
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_machine::Env;

pub fn register(registry: &mut Registry) {
    registry.register(get_context_ABI);
    registry.register(get_status_position_ABI);
    registry.register(set_status_position_ABI);
}

pub fn register_compat(registry: &mut Registry) {
    registry.register(default_window_ABI);
}

#[win32(imm32, name = "ImmGetDefaultIMEWnd", level = state)]
fn default_window(_env: &mut Env<'_>, _window: Handle) -> Handle {
    Handle(0)
}

/// This personality creates neither an IME window nor an input context.
#[win32(imm32, name = "ImmGetContext", level = state)]
fn get_context(_env: &mut Env<'_>, _window: Handle) -> Handle {
    Handle(0)
}

/// Native NULL-HIMC GET fails without changing LastError. Other nonexistent
/// contexts fail with ERROR_INVALID_HANDLE. Neither path touches the POINT.
#[win32(imm32, name = "ImmGetStatusWindowPos", level = safe_fail)]
fn get_status_position(env: &mut Env<'_>, context: Handle, _output: GuestPtr<()>) -> i32 {
    if context.0 != 0 {
        crate::util::set_last_error(env, 6);
    }
    0
}

/// SET needs a real context even for a NULL HIMC; failure does not read the
/// supplied POINT or create notifications/status-window state.
#[win32(imm32, name = "ImmSetStatusWindowPos", level = safe_fail)]
fn set_status_position(env: &mut Env<'_>, _context: Handle, _position: GuestPtr<()>) -> i32 {
    crate::util::set_last_error(env, 6);
    0
}
