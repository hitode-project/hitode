//! Win32 timer units and naming over the neutral Machine timer object.
use crate::util::set_last_error;
use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, win32};
use adv_machine::{Env, Object, Outcome};

fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
fn create(
    env: &mut Env<'_>,
    attributes: GuestPtr<[u32; 3]>,
    manual: i32,
    name: u32,
    wide: bool,
) -> Outcome {
    let inherit = match super::handle::security_inherit(env, attributes.addr()) {
        Ok(inherit) => inherit,
        Err(error) => {
            if error == 50 {
                adv_abi::warn_unavailable(
                    env,
                    if wide {
                        "CreateWaitableTimerW"
                    } else {
                        "CreateWaitableTimerA"
                    },
                    "custom timer security descriptors are not modeled",
                );
            }
            return fail(env, error);
        }
    };
    let result = super::file_mapping::sync_created(
        env,
        Object::Timer {
            manual_reset: manual != 0,
            signaled: false,
            deadline: None,
            period_us: 0,
        },
        name,
        wide,
    );
    if let Outcome::Return(handle) = &result {
        super::handle::set_inherit(env, *handle, inherit);
    }
    result
}
#[win32(kernel32, name = "CreateWaitableTimerA", level = implement)]
fn create_a(
    env: &mut Env<'_>,
    attributes: GuestPtr<[u32; 3]>,
    manual: i32,
    name: AnsiStr,
) -> Outcome {
    create(env, attributes, manual, name.0.addr(), false)
}
#[win32(kernel32, name = "CreateWaitableTimerW", level = implement)]
fn create_w(
    env: &mut Env<'_>,
    attributes: GuestPtr<[u32; 3]>,
    manual: i32,
    name: WideStr,
) -> Outcome {
    create(env, attributes, manual, name.0.addr(), true)
}
#[win32(kernel32, name = "SetWaitableTimer", level = implement)]
fn set(
    env: &mut Env<'_>,
    timer: Handle,
    due: GuestPtr<i64>,
    period: i32,
    completion: GuestPtr<()>,
    _argument: GuestPtr<()>,
    resume: i32,
) -> Outcome {
    if !matches!(env.sys.objects.get(timer.0), Some(Object::Timer { .. })) {
        return fail(env, 6);
    }
    let Ok(due) = due.read(env.cpu) else {
        return fail(env, 998);
    };
    if period < 0 {
        return fail(env, 87);
    }
    if !completion.is_null() {
        adv_abi::warn_unavailable(
            env,
            "SetWaitableTimer",
            "timer APC completion routines are not modeled",
        );
        return fail(env, 50);
    }
    // Absolute values share GetSystemTimeAsFileTime's UTC basis. Thereafter
    // the one Machine clock owns scheduling; relative ticks round up to avoid
    // signaling before their requested 100-ns interval has elapsed.
    let delay_us = if due < 0 {
        due.unsigned_abs().div_ceil(10)
    } else {
        (due as u64)
            .saturating_sub(super::time::filetime_now(env))
            .div_ceil(10)
    };
    let deadline = env.sys.clock.now_us().saturating_add(delay_us);
    if !env.sys.arm_timer(timer.0, deadline, period as u64 * 1000) {
        return fail(env, 6);
    }
    if resume != 0 {
        // The SDK explicitly allows success with ERROR_NOT_SUPPORTED for a
        // system that cannot resume from a power-conservation state.
        adv_abi::warn_unavailable(
            env,
            "SetWaitableTimer",
            "power-state resume is unavailable; the waitable timer is armed",
        );
        set_last_error(env, 50);
    }
    Outcome::Return(1)
}
#[win32(kernel32, name = "CancelWaitableTimer", level = implement)]
fn cancel(env: &mut Env<'_>, timer: Handle) -> Outcome {
    if env.sys.cancel_timer(timer.0) {
        Outcome::Return(1)
    } else {
        fail(env, 6)
    }
}
#[win32(kernel32, name = "OpenWaitableTimerA", level = implement)]
fn open_a(env: &mut Env<'_>, _access: u32, _inherit: i32, name: AnsiStr) -> Outcome {
    super::file_mapping::open_sync(env, name.0.addr(), false, 4, 0x001f0003)
}
#[win32(kernel32, name = "OpenWaitableTimerW", level = implement)]
fn open_w(env: &mut Env<'_>, _access: u32, _inherit: i32, name: WideStr) -> Outcome {
    super::file_mapping::open_sync(env, name.0.addr(), true, 4, 0x001f0003)
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        create_a_ABI,
        create_w_ABI,
        set_ABI,
        cancel_ABI,
        open_a_ABI,
        open_w_ABI,
    ] {
        registry.register(api);
    }
}

#[cfg(test)]
mod tests;
