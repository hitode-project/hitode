//! Default IME window state for a personality without an IME window.
use adv_abi::{Handle, Registry, win32};
use adv_machine::Env;

pub fn register_compat(registry: &mut Registry) {
    registry.register(default_window_ABI);
}

#[win32(imm32, name = "ImmGetDefaultIMEWnd", level = state)]
fn default_window(_env: &mut Env<'_>, _window: Handle) -> Handle {
    Handle(0)
}
