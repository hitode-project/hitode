//! Native wait status and Win32 wait results share the scheduler object owner.
use super::*;
use crate::kernel32::apc;
use crate::kernel32::process::native_thread;

fn valid(env: &Env<'_>, handle: u32) -> Result<(), u32> {
    if handle == u32::MAX || handle == u32::MAX - 1 {
        return Ok(());
    }
    match env.sys.objects.get(handle) {
        Some(Object::Process { access, .. } | Object::RemoteThread { access, .. })
            if access & 0x100000 == 0 =>
        {
            Err(0xc0000022)
        }
        Some(
            Object::Event { .. }
            | Object::Timer { .. }
            | Object::Mutex { .. }
            | Object::Semaphore { .. }
            | Object::Thread { .. }
            | Object::Process { .. }
            | Object::RemoteThread { .. },
        ) => Ok(()),
        Some(_) => Err(0xc0000024),
        None => Err(0xc0000008),
    }
}
pub(crate) fn wait_native(
    env: &mut Env<'_>,
    handle: u32,
    alertable: bool,
    timeout: u32,
) -> Outcome {
    if let Err(status) = valid(env, handle) {
        return Outcome::Return(status);
    }
    let deadline = match apc::timeout(env, timeout) {
        Ok(deadline) => deadline,
        Err(status) => return Outcome::Return(status),
    };
    wait_until(env, handle, alertable, deadline)
}
fn wait_until(env: &mut Env<'_>, handle: u32, alertable: bool, deadline: Option<u64>) -> Outcome {
    if alertable && apc::pending(env) {
        return apc::drain(env, Box::new(|_| Outcome::Return(apc::USER_APC)));
    }
    if handle != u32::MAX
        && handle != u32::MAX - 1
        && let Some(reason) = env.sys.try_acquire_objects(&[handle], false)
    {
        return wait_result(Resume::Woke(reason));
    }
    if deadline.is_some_and(|deadline| deadline <= env.sys.clock.now_us()) {
        return Outcome::Return(WAIT_TIMEOUT);
    }
    let notification = if alertable {
        match apc::notification(env) {
            Ok(handle) => Some(handle),
            Err(status) => return Outcome::Return(status),
        }
    } else {
        None
    };
    Outcome::block(
        Wait::Objects {
            handles: vec![handle],
            all: false,
            deadline,
            notification,
        },
        move |env, reason| {
            if matches!(reason, Resume::Woke(WakeReason::Notified)) {
                wait_until(env, handle, alertable, deadline)
            } else {
                wait_result(reason)
            }
        },
    )
}
pub(super) fn wait_front(env: &mut Env<'_>, handle: u32, ms: u32, alertable: bool) -> Outcome {
    match crate::native::overridden(env, "NtWaitForSingleObject") {
        Ok(Some(target)) => {
            let (timeout, stack) = if ms == u32::MAX {
                (0, None)
            } else {
                let buffer = match native_thread::scratch(env, 8) {
                    Ok(buffer) => buffer,
                    Err(error) => return error,
                };
                if let Err(error) = env
                    .cpu
                    .write(buffer, &(-(i64::from(ms) * 10000)).to_le_bytes())
                {
                    return Outcome::Fail(error.to_string());
                }
                (buffer, Some(buffer))
            };
            let done = |env: &mut Env<'_>, value: Resume| {
                let status = value.value();
                native_thread::front_status(env, status, status, WAIT_FAILED)
            };
            if let Some(stack) = stack {
                Outcome::call_on_stack(target, [handle, u32::from(alertable), timeout], stack, done)
            } else {
                Outcome::call(target, [handle, u32::from(alertable), timeout], done)
            }
        }
        Ok(None) if !alertable => wait_one(env, handle, ms),
        Ok(None) => {
            if let Err(status) = valid(env, handle) {
                return native_thread::front_status(env, status, status, WAIT_FAILED);
            }
            let deadline = (ms != u32::MAX)
                .then(|| env.sys.clock.now_us().saturating_add(u64::from(ms) * 1000));
            let outcome = wait_until(env, handle, true, deadline);
            native_thread::map_status(
                env,
                outcome,
                Box::new(|env, status| {
                    native_thread::front_status(env, status, status, WAIT_FAILED)
                }),
            )
        }
        Err(_) => crate::native::unavailable(env, "NtWaitForSingleObject"),
    }
}

pub(crate) fn register(registry: &mut Registry) {
    crate::stdcall(registry, "ntdll.dll", "NtWaitForSingleObject", 3, |env| {
        let args = [env.arg(0), env.arg(1), env.arg(2)];
        wait_native(env, args[0], args[1] as u8 != 0, args[2])
    });
}
