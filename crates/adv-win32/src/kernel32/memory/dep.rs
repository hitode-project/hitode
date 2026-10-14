//! Windows' default OptIn policy: an image with NX_COMPAT gets permanent DEP,
//! and any other process runs readable pages. Never acknowledge a DEP toggle
//! we cannot perform.
use crate::util::set_last_error;
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::Prot;
use adv_machine::Env;

#[win32(kernel32, name = "GetSystemDEPPolicy", level = state)]
fn system(_env: &mut Env<'_>) -> i32 {
    2
} // OptIn

#[win32(kernel32, name = "GetProcessDEPPolicy", level = state)]
fn process(
    env: &mut Env<'_>,
    handle: Handle,
    flags: GuestPtr<u32>,
    permanent: GuestPtr<i32>,
) -> i32 {
    if let Err(error) = super::super::child_process::process_target(env, handle.0, 0x400) {
        set_last_error(env, error.code);
        return 0;
    }
    if check_range(env.cpu, flags.addr(), 4, Prot::WRITE).is_err()
        || check_range(env.cpu, permanent.addr(), 4, Prot::WRITE).is_err()
    {
        set_last_error(env, 998);
        return 0;
    }
    // NX_COMPAT: DEP enabled, ATL NX-thunk emulation disabled, permanent.
    let enabled = !adv_os::try_modules(env.ext).is_some_and(|m| m.dep_disabled());
    let (value, fixed) = if enabled { (3, 1) } else { (0, 0) };
    if flags.write(env.cpu, value).is_err() || permanent.write(env.cpu, fixed).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    1
}

// https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-setprocessdeppolicy
#[win32(kernel32, name = "SetProcessDEPPolicy", level = safe_fail)]
fn set(env: &mut Env<'_>, flags: u32) -> i32 {
    if !matches!(flags, 0 | 1 | 3) {
        set_last_error(env, 87);
        return 0;
    }
    warn_unavailable(
        env,
        "kernel32.dll!SetProcessDEPPolicy",
        "process DEP follows the image's NX_COMPAT flag; dynamic DEP and ATL thunk emulation are unavailable",
    );
    set_last_error(env, 5);
    0
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(system_ABI);
    registry.register(process_ABI);
    registry.register(set_ABI);
}
