//! Native wait status and Win32 wait results share the scheduler object owner.
use super::*;
use crate::kernel32::apc;
use crate::kernel32::process::native_thread;
use adv_abi::guest::check_range;
use adv_cpu::backend::Prot;

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
    wait_set_until(env, vec![handle], false, alertable, deadline)
}
fn wait_set_until(
    env: &mut Env<'_>,
    handles: Vec<u32>,
    all: bool,
    alertable: bool,
    deadline: Option<u64>,
) -> Outcome {
    if alertable && apc::pending(env) {
        return apc::drain(env, Box::new(|_| Outcome::Return(apc::USER_APC)));
    }
    if let Some(reason) = env.sys.try_acquire_objects(&handles, all) {
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
    let retry = handles.clone();
    Outcome::block(
        Wait::Objects {
            handles,
            all,
            deadline,
            notification,
        },
        move |env, reason| {
            if matches!(reason, Resume::Woke(WakeReason::Notified)) {
                wait_set_until(env, retry, all, alertable, deadline)
            } else {
                wait_result(reason)
            }
        },
    )
}
fn front_result(env: &mut Env<'_>, status: u32) -> Outcome {
    match status {
        0xc0000024 => {
            set_last_error(env, err::INVALID_HANDLE);
            Outcome::Return(WAIT_FAILED)
        }
        0x80000002 => {
            set_last_error(env, 998);
            Outcome::Return(WAIT_FAILED)
        }
        _ => native_thread::front_status(env, status, status, WAIT_FAILED),
    }
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
                front_result(env, status)
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
                return front_result(env, status);
            }
            let deadline = (ms != u32::MAX)
                .then(|| env.sys.clock.now_us().saturating_add(u64::from(ms) * 1000));
            let outcome = wait_until(env, handle, true, deadline);
            native_thread::map_status(env, outcome, Box::new(front_result))
        }
        Err(_) => crate::native::unavailable(env, "NtWaitForSingleObject"),
    }
}

/// Signaling and waiting share the existing scheduler owners. Windows does not
/// guarantee an atomic transition: already blocked waiters can receive the signal
/// before this caller starts waiting, including when both handles alias an object.
pub(super) fn signal_wait_front(env: &mut Env<'_>) -> Outcome {
    let (signal, wait, milliseconds, alertable) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3) != 0);
    if let Err(status) = valid(env, wait) {
        return front_result(env, status);
    }
    let signaled = match env.sys.objects.get(signal) {
        Some(Object::Event { .. }) => event::state_front(env, true),
        Some(Object::Mutex { .. }) => owned::release_at(env, owned::Kind::Mutant, signal, 0, 0),
        Some(Object::Semaphore { .. }) => {
            owned::release_at(env, owned::Kind::Semaphore, signal, 1, 0)
        }
        _ => {
            set_last_error(env, err::INVALID_HANDLE);
            return Outcome::Return(WAIT_FAILED);
        }
    };
    native_thread::map_status(
        env,
        signaled,
        Box::new(move |env, succeeded| {
            if succeeded == 0 {
                Outcome::Return(WAIT_FAILED)
            } else {
                wait_front(env, wait, milliseconds, alertable)
            }
        }),
    )
}

fn handles(env: &Env<'_>, count: u32, pointer: u32, all: bool) -> Result<Vec<u32>, u32> {
    if count == 0 || count > 64 {
        return Err(0xc000000d);
    }
    if pointer & 3 != 0 {
        return Err(0x80000002);
    }
    check_range(env.cpu, pointer, count * 4, Prot::READ).map_err(|_| 0xc0000005u32)?;
    let mut result = Vec::with_capacity(count as usize);
    for i in 0..count {
        let handle = env
            .cpu
            .read_u32(pointer + i * 4)
            .map_err(|_| 0xc0000005u32)?;
        if result.contains(&handle)
            || all
                && result
                    .iter()
                    .any(|&other| env.sys.objects.same_object(other, handle))
        {
            return Err(0xc000000d);
        }
        valid(env, handle)?;
        result.push(handle);
    }
    Ok(result)
}
pub(crate) fn multiple_native(env: &mut Env<'_>, args: [u32; 5]) -> Outcome {
    let [count, pointer, kind, alertable, timeout] = args;
    if kind > 1 {
        return Outcome::Return(0xc000000d);
    }
    let handles = match handles(env, count, pointer, kind == 0) {
        Ok(handles) => handles,
        Err(status) => return Outcome::Return(status),
    };
    let deadline = match apc::timeout(env, timeout) {
        Ok(deadline) => deadline,
        Err(status) => return Outcome::Return(status),
    };
    wait_set_until(env, handles, kind == 0, alertable as u8 != 0, deadline)
}
pub(super) fn multiple_front(env: &mut Env<'_>, alertable: bool) -> Outcome {
    let (count, pointer, all, ms) = (env.arg(0), env.arg(1), env.arg(2) != 0, env.arg(3));
    match crate::native::overridden(env, "NtWaitForMultipleObjects") {
        Ok(Some(target)) => {
            let (timeout, stack) = if ms == u32::MAX {
                (0, None)
            } else {
                let buffer = match native_thread::scratch(env, 8) {
                    Ok(buffer) => buffer,
                    Err(outcome) => return outcome,
                };
                if let Err(error) = env
                    .cpu
                    .write(buffer, &(-(i64::from(ms) * 10000)).to_le_bytes())
                {
                    return Outcome::Fail(error.to_string());
                }
                (buffer, Some(buffer))
            };
            let args = [
                count,
                pointer,
                u32::from(!all),
                u32::from(alertable),
                timeout,
            ];
            let done = |env: &mut Env<'_>, result: Resume| front_result(env, result.value());
            if let Some(stack) = stack {
                Outcome::call_on_stack(target, args, stack, done)
            } else {
                Outcome::call(target, args, done)
            }
        }
        Ok(None) => {
            let handles = match handles(env, count, pointer, all) {
                Ok(handles) => handles,
                Err(status) => return front_result(env, status),
            };
            let deadline = (ms != u32::MAX)
                .then(|| env.sys.clock.now_us().saturating_add(u64::from(ms) * 1000));
            let outcome = wait_set_until(env, handles, all, alertable, deadline);
            native_thread::map_status(env, outcome, Box::new(front_result))
        }
        Err(_) => crate::native::unavailable(env, "NtWaitForMultipleObjects"),
    }
}

pub(crate) fn register(registry: &mut Registry) {
    crate::stdcall(registry, "ntdll.dll", "NtWaitForSingleObject", 3, |env| {
        let args = [env.arg(0), env.arg(1), env.arg(2)];
        wait_native(env, args[0], args[1] as u8 != 0, args[2])
    });
    crate::stdcall(
        registry,
        "ntdll.dll",
        "NtWaitForMultipleObjects",
        5,
        |env| {
            let args = std::array::from_fn(|i| env.arg(i as u32));
            multiple_native(env, args)
        },
    );
}
