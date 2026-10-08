//! Bounded user APC queues belong to the target guest thread, never host threads.
use super::*;
use adv_abi::guest::check_range;
use adv_machine::{CtxId, Resume, WakeReason, outcome::DeferredContinuation};
use std::collections::{BTreeMap, VecDeque};

pub(crate) const USER_APC: u32 = 0xc0;
const QUEUE_LIMIT: usize = 64;
struct Apc {
    routine: u32,
    args: [u32; 3],
}
#[derive(Default)]
struct Queue {
    pending: VecDeque<Apc>,
    notification: Option<u32>,
}
#[derive(Default)]
struct Queues(BTreeMap<CtxId, Queue>);

pub(crate) fn retire(env: &mut Env<'_>, id: CtxId) {
    if let Some(queue) = env.ext.get_or_default::<Queues>().0.remove(&id)
        && let Some(handle) = queue.notification
    {
        env.sys.objects.close(handle);
    }
}
pub(crate) fn retire_all(env: &mut Env<'_>) {
    let queues = std::mem::take(&mut env.ext.get_or_default::<Queues>().0);
    for queue in queues.into_values() {
        if let Some(handle) = queue.notification {
            env.sys.objects.close(handle);
        }
    }
}
pub(crate) fn retire_completed(env: &mut Env<'_>) {
    let ids: Vec<_> = env
        .ext
        .get_or_default::<Queues>()
        .0
        .keys()
        .copied()
        .collect();
    for id in ids {
        if env
            .sys
            .contexts
            .get(id as usize)
            .is_none_or(|context| context.is_done())
        {
            retire(env, id);
        }
    }
}
pub(crate) fn pending(env: &mut Env<'_>) -> bool {
    env.ext
        .get_or_default::<Queues>()
        .0
        .get(&env.context)
        .is_some_and(|queue| !queue.pending.is_empty())
}
pub(crate) fn notification(env: &mut Env<'_>) -> Result<u32, u32> {
    let queue = env
        .ext
        .get_or_default::<Queues>()
        .0
        .entry(env.context)
        .or_default();
    if let Some(handle) = queue.notification {
        return Ok(handle);
    }
    let handle = env.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: !queue.pending.is_empty(),
    });
    if handle == 0 {
        return Err(0xc0000017);
    }
    queue.notification = Some(handle);
    Ok(handle)
}
pub(crate) fn drain(env: &mut Env<'_>, then: DeferredContinuation) -> Outcome {
    let (apc, notification) = {
        let queue = env
            .ext
            .get_or_default::<Queues>()
            .0
            .entry(env.context)
            .or_default();
        (queue.pending.pop_front(), queue.notification)
    };
    match apc {
        Some(apc) => Outcome::call(apc.routine, apc.args, move |env, _| drain(env, then)),
        None => {
            if let Some(handle) = notification {
                env.sys.reset_event(handle);
            }
            then(env)
        }
    }
}
pub(crate) fn test_alert(env: &mut Env<'_>) -> Outcome {
    drain(env, Box::new(|_| Outcome::Return(0)))
}
pub(crate) fn dispatch_user(env: &mut Env<'_>) -> Outcome {
    let (routine, parameter, activation) = (env.arg(0), env.arg(1), env.arg(2));
    if activation != 0 && activation != u32::MAX {
        adv_abi::warn_unavailable(
            env,
            "ntdll.dll!RtlDispatchAPC",
            "explicit APC activation contexts are unavailable",
        );
        return Outcome::Fail("APC activation context is unsupported".into());
    }
    Outcome::call(routine, [parameter], |_, _| Outcome::Return(0))
}
pub(crate) fn queue_native(
    env: &mut Env<'_>,
    handle: u32,
    routine: u32,
    args: [u32; 3],
) -> Outcome {
    retire_completed(env);
    let identity = match thread_target(env, handle, 0x10) {
        Ok(identity) => identity,
        Err(error) => {
            return Outcome::Return(match error.code {
                5 => 0xc0000022,
                _ => 0xc0000008,
            });
        }
    };
    if identity.completion.exit_code().is_some() {
        return Outcome::Return(0xc000004b);
    }
    let id = identity.context;
    let pid = identity.process.pid;
    // Synthetic images may have different placement in each process. Translate
    // only the shared system dispatcher; arbitrary guest routine addresses keep
    // their target-process meaning, including a hook's replacement routine.
    let shared_dispatch = if pid != env.sys.pid {
        let modules = adv_os::modules(env.ext);
        modules.by_name("ntdll.dll").is_some_and(|id| {
            let image = modules.get(id);
            matches!(image.exports_by_name.get("RtlDispatchAPC"), Some(adv_pe::ExportTarget::Rva(rva)) if image.base + rva == routine)
        })
    } else {
        false
    };
    processes::visit(
        env,
        pid,
        None,
        move |target| {
            if target
                .sys
                .contexts
                .get(id as usize)
                .is_none_or(|context| context.is_done())
            {
                return Err(processes::Error::new(6, "APC target thread has exited"));
            }
            let routine = if shared_dispatch {
                crate::native::entry(target, "RtlDispatchAPC").map_err(|_| {
                    processes::Error::new(50, "target APC dispatcher is unavailable")
                })?
            } else {
                routine
            };
            let queue = target
                .ext
                .get_or_default::<Queues>()
                .0
                .entry(id)
                .or_default();
            if queue.pending.len() >= QUEUE_LIMIT {
                return Err(processes::Error::new(8, "guest APC queue limit"));
            }
            queue.pending.push_back(Apc { routine, args });
            if let Some(handle) = queue.notification {
                target.sys.set_event(handle);
            }
            Ok(())
        },
        |env, result| {
            let status = match result {
                Ok(()) => 0,
                Err(error) => match error.code {
                    6 => 0xc000004b,
                    8 => 0xc0000017,
                    50 => {
                        adv_abi::warn_unavailable(
                            env,
                            "ntdll.dll!NtQueueApcThread",
                            &error.message,
                        );
                        0xc00000bb
                    }
                    _ => 0xc0000001,
                },
            };
            Outcome::Return(status)
        },
    )
}
pub(super) fn queue_front(env: &mut Env<'_>) -> Outcome {
    let (routine, thread, parameter) = (env.arg(0), env.arg(1), env.arg(2));
    if let Err(error) = super::super::module::prepare_system_env(env) {
        return api_error(env, processes::Error::new(8, error.to_string()), 0);
    }
    let dispatcher = match crate::native::entry(env, "RtlDispatchAPC") {
        Ok(entry) => entry,
        Err(_) => return crate::native::unavailable(env, "RtlDispatchAPC"),
    };
    let args = [thread, dispatcher, routine, parameter, u32::MAX];
    match crate::native::overridden(env, "NtQueueApcThread") {
        Ok(Some(target)) => Outcome::call(target, args, |env, value| {
            native_thread::front_status(env, value.value(), 1, 0)
        }),
        Ok(None) => {
            let outcome = queue_native(env, thread, dispatcher, [routine, parameter, u32::MAX]);
            native_thread::map_status(
                env,
                outcome,
                Box::new(|env, status| native_thread::front_status(env, status, 1, 0)),
            )
        }
        Err(_) => crate::native::unavailable(env, "NtQueueApcThread"),
    }
}
pub(crate) fn timeout(env: &mut Env<'_>, pointer: u32) -> Result<Option<u64>, u32> {
    if pointer == 0 {
        return Ok(None);
    }
    if pointer & 3 != 0 {
        return Err(0x80000002);
    }
    check_range(env.cpu, pointer, 8, Prot::READ).map_err(|_| 0xc0000005u32)?;
    let mut bytes = [0; 8];
    env.cpu
        .read(pointer, &mut bytes)
        .map_err(|_| 0xc0000005u32)?;
    let interval = i64::from_le_bytes(bytes);
    if interval == i64::MIN {
        return Ok(None);
    }
    let remaining = if interval < 0 {
        interval.unsigned_abs()
    } else {
        (interval as u64).saturating_sub(super::super::filetime_now(env))
    };
    Ok(Some(
        env.sys
            .clock
            .now_us()
            .saturating_add(remaining.div_ceil(10)),
    ))
}
pub(crate) fn delay_native(env: &mut Env<'_>, alertable: bool, interval: u32) -> Outcome {
    if interval == 0 {
        return Outcome::Return(0xc0000005);
    }
    let deadline = match timeout(env, interval) {
        Ok(deadline) => deadline,
        Err(status) => return Outcome::Return(status),
    };
    delay_until(env, alertable, deadline)
}
fn delay_until(env: &mut Env<'_>, alertable: bool, deadline: Option<u64>) -> Outcome {
    if alertable && pending(env) {
        return drain(env, Box::new(|_| Outcome::Return(USER_APC)));
    }
    if deadline.is_some_and(|deadline| deadline <= env.sys.clock.now_us()) {
        return Outcome::block_then_return(Wait::Yield, 0);
    }
    if !alertable {
        return match deadline {
            Some(deadline) => Outcome::block_then_return(Wait::Until(deadline), 0),
            None => Outcome::block_then_return(
                Wait::Objects {
                    handles: vec![],
                    all: false,
                    deadline: None,
                    notification: None,
                },
                0,
            ),
        };
    }
    let notify = match notification(env) {
        Ok(handle) => handle,
        Err(status) => return Outcome::Return(status),
    };
    Outcome::block(
        Wait::Objects {
            handles: vec![],
            all: false,
            deadline,
            notification: Some(notify),
        },
        move |env, reason| {
            if matches!(reason, Resume::Woke(WakeReason::Notified)) {
                delay_until(env, true, deadline)
            } else {
                Outcome::Return(0)
            }
        },
    )
}
pub(crate) fn sleep_front(env: &mut Env<'_>, ms: u32, alertable: bool, extended: bool) -> Outcome {
    let done = Box::new(move |env: &mut Env<'_>, status: u32| {
        if status & 0x80000000 != 0 {
            return native_thread::front_status(env, status, 0, u32::MAX);
        }
        Outcome::Return(if extended && status == USER_APC {
            USER_APC
        } else {
            0
        })
    });
    match crate::native::overridden(env, "NtDelayExecution") {
        Ok(Some(target)) => {
            let buffer = match native_thread::scratch(env, 8) {
                Ok(buffer) => buffer,
                Err(error) => return error,
            };
            let interval = if ms == u32::MAX {
                i64::MIN
            } else {
                -(i64::from(ms) * 10000)
            };
            if let Err(error) = env.cpu.write(buffer, &interval.to_le_bytes()) {
                return Outcome::Fail(error.to_string());
            }
            Outcome::call_on_stack(
                target,
                [u32::from(alertable), buffer],
                buffer,
                move |env, value| done(env, value.value()),
            )
        }
        Ok(None) => {
            let deadline = (ms != u32::MAX)
                .then(|| env.sys.clock.now_us().saturating_add(u64::from(ms) * 1000));
            let outcome = delay_until(env, alertable, deadline);
            native_thread::map_status(env, outcome, done)
        }
        Err(_) => crate::native::unavailable(env, "NtDelayExecution"),
    }
}
