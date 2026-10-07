//! Session messaging requires a virtual RDS provider; never send to host sessions.
use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, warn_unavailable, win32};
use adv_machine::Env;

const UNAVAILABLE: &str = "remote desktop session messaging is unavailable";

fn unavailable(env: &mut Env<'_>, api: &str) -> i32 {
    warn_unavailable(env, api, UNAVAILABLE);
    crate::util::set_last_error(env, 120);
    0
}

#[allow(clippy::too_many_arguments)]
#[win32(wtsapi32, name = "WTSSendMessageA", level = safe_fail)]
fn send_a(
    env: &mut Env<'_>,
    _server: Handle,
    _session: u32,
    _title: AnsiStr,
    _title_bytes: u32,
    _message: AnsiStr,
    _message_bytes: u32,
    _style: u32,
    _timeout: u32,
    _response: GuestPtr<u32>,
    _wait: i32,
) -> i32 {
    unavailable(env, "wtsapi32.dll!WTSSendMessageA")
}

#[allow(clippy::too_many_arguments)]
#[win32(wtsapi32, name = "WTSSendMessageW", level = safe_fail)]
fn send_w(
    env: &mut Env<'_>,
    _server: Handle,
    _session: u32,
    _title: WideStr,
    _title_bytes: u32,
    _message: WideStr,
    _message_bytes: u32,
    _style: u32,
    _timeout: u32,
    _response: GuestPtr<u32>,
    _wait: i32,
) -> i32 {
    unavailable(env, "wtsapi32.dll!WTSSendMessageW")
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(send_a_ABI);
    registry.register(send_w_ABI);
}

#[cfg(test)]
mod tests;
