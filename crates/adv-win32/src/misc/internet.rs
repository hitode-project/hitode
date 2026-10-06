//! Explicit unsupported-session boundary; never fabricate Internet handles.
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, win32};
use adv_machine::Env;

pub(super) fn register(reg: &mut Registry) {
    reg.register(open_a_ABI);
    reg.register(open_w_ABI);
    reg.register(url_a_ABI);
    reg.register(url_w_ABI);
    reg.register(close_ABI);
}
fn unavailable(env: &mut Env<'_>, api: &str) -> GuestPtr<()> {
    adv_abi::warn_unavailable(env, api, "WinINet session backend is unavailable");
    crate::util::set_last_error(env, 120); // ERROR_CALL_NOT_IMPLEMENTED
    GuestPtr::new(0)
}
#[win32(wininet, name="InternetOpenA", level=safe_fail)]
fn open_a(
    env: &mut Env<'_>,
    _agent: AnsiStr,
    _access: u32,
    _proxy: AnsiStr,
    _bypass: AnsiStr,
    _flags: u32,
) -> GuestPtr<()> {
    unavailable(env, "wininet.dll!InternetOpenA")
}
#[win32(wininet, name="InternetOpenW", level=safe_fail)]
fn open_w(
    env: &mut Env<'_>,
    _agent: WideStr,
    _access: u32,
    _proxy: WideStr,
    _bypass: WideStr,
    _flags: u32,
) -> GuestPtr<()> {
    unavailable(env, "wininet.dll!InternetOpenW")
}

fn not_initialized(env: &mut Env<'_>, api: &str) -> GuestPtr<()> {
    // No InternetOpen call can create a session in this personality. Native
    // WinINet rejects URL opens before accessing URL/header buffers when its
    // session subsystem has not initialized (ERROR_INTERNET_NOT_INITIALIZED).
    adv_abi::warn_unavailable(
        env,
        api,
        "WinINet URL retrieval is unavailable: no initialized session backend",
    );
    crate::util::set_last_error(env, 12172);
    GuestPtr::default()
}

#[win32(wininet, name = "InternetOpenUrlA", level = safe_fail)]
fn url_a(
    env: &mut Env<'_>,
    _session: GuestPtr<()>,
    _url: AnsiStr,
    _headers: AnsiStr,
    _header_length: u32,
    _flags: u32,
    _context: u32,
) -> GuestPtr<()> {
    not_initialized(env, "wininet.dll!InternetOpenUrlA")
}

#[win32(wininet, name = "InternetOpenUrlW", level = safe_fail)]
fn url_w(
    env: &mut Env<'_>,
    _session: GuestPtr<()>,
    _url: WideStr,
    _headers: WideStr,
    _header_length: u32,
    _flags: u32,
    _context: u32,
) -> GuestPtr<()> {
    not_initialized(env, "wininet.dll!InternetOpenUrlW")
}

#[win32(wininet, name = "InternetCloseHandle", level = safe_fail)]
fn close(env: &mut Env<'_>, _handle: GuestPtr<()>) -> i32 {
    // No session or request owner is ever issued by this profile.
    crate::util::set_last_error(env, 6);
    0
}

#[cfg(test)]
mod tests;
