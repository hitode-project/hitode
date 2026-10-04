//! Explicit unsupported-session boundary; never fabricate Internet handles.
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, win32};
use adv_machine::Env;

pub(super) fn register(reg: &mut Registry) {
    reg.register(open_a_ABI);
    reg.register(open_w_ABI);
}
fn unavailable(env: &mut Env<'_>) -> GuestPtr<()> {
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
    unavailable(env)
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
    unavailable(env)
}
