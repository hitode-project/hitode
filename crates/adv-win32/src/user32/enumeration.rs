//! Thread window traversal uses USER's z-order index and real guest callbacks.
use super::{user, window::WS_CHILD};
use crate::util::set_last_error;
use adv_abi::{Env, GuestPtr, Outcome, Registry, win32};
use adv_cpu::backend::Prot;
use adv_machine::CtxId;

fn next(
    env: &mut Env<'_>,
    mut windows: std::vec::IntoIter<u32>,
    thread: Option<CtxId>,
    callback: u32,
    parameter: u32,
) -> Outcome {
    for hwnd in windows.by_ref() {
        // A callback may destroy or reparent a later window, or yield to its
        // owning thread. Never retain a USER borrow across guest execution.
        if !user(env)
            .windows
            .get(&env.sys.objects, &hwnd)
            .is_some_and(|w| w.style & WS_CHILD == 0 && thread.is_none_or(|id| w.thread == id))
        {
            continue;
        }
        return Outcome::call(callback, [hwnd, parameter], move |env, result| {
            if result.value() == 0 {
                Outcome::Return(0)
            } else {
                next(env, windows, thread, callback, parameter)
            }
        });
    }
    Outcome::Return(1)
}

#[win32(user32, name = "EnumThreadWindows", level = state)]
fn enumerate(env: &mut Env<'_>, thread_id: u32, callback: GuestPtr<()>, parameter: i32) -> Outcome {
    let thread = if thread_id == 0 {
        // Native USER treats zero as all threads on the desktop. The virtual
        // desktop contains only this process's USER windows.
        None
    } else if let Some(context) = env.sys.contexts.iter().find(|ctx| ctx.tid == thread_id) {
        Some(context.id)
    } else {
        // Native succeeds without invoking the callback for an unknown thread,
        // but reports ERROR_INVALID_PARAMETER in LastError.
        set_last_error(env, 87);
        return Outcome::Return(1);
    };
    let u = user(env);
    let windows = u
        .top
        .iter()
        .copied()
        .filter(|hwnd| {
            u.windows
                .get(&env.sys.objects, hwnd)
                .is_some_and(|w| w.style & WS_CHILD == 0 && thread.is_none_or(|id| w.thread == id))
        })
        .collect::<Vec<_>>();
    if windows.is_empty() {
        return Outcome::Return(1);
    }
    if callback.addr() == 0 || !env.cpu.accessible(callback.addr(), 1, Prot::EXEC) {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    next(
        env,
        windows.into_iter(),
        thread,
        callback.addr(),
        parameter as u32,
    )
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(enumerate_ABI);
}

#[cfg(test)]
mod tests;
