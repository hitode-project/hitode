//! Explicit failure for SCM connections without a virtual service provider.
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, warn_unavailable, win32};
use adv_machine::Env;

const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
const UNAVAILABLE: &str =
    "virtual service control manager connections and databases are unavailable";

fn unavailable(env: &mut Env<'_>, api: &str) -> GuestPtr<()> {
    warn_unavailable(env, api, UNAVAILABLE);
    crate::util::set_last_error(env, ERROR_CALL_NOT_IMPLEMENTED);
    GuestPtr::new(0)
}

#[win32(advapi32, name = "OpenSCManagerA", level = safe_fail)]
fn open_a(
    env: &mut Env<'_>,
    _machine: AnsiStr,
    _database: AnsiStr,
    _desired_access: u32,
) -> GuestPtr<()> {
    unavailable(env, "advapi32.dll!OpenSCManagerA")
}

#[win32(advapi32, name = "OpenSCManagerW", level = safe_fail)]
fn open_w(
    env: &mut Env<'_>,
    _machine: WideStr,
    _database: WideStr,
    _desired_access: u32,
) -> GuestPtr<()> {
    unavailable(env, "advapi32.dll!OpenSCManagerW")
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(open_a_ABI);
    registry.register(open_w_ABI);
}

#[cfg(test)]
mod tests;
