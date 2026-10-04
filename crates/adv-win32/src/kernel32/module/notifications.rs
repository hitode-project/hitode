//! Win32 module notification policy, composed exclusively with Machine outcomes.
use super::*;
use adv_cpu::backend::Backend;
use adv_cpu::backend::MemFault;
use adv_machine::CtxId;
use adv_machine::Machine;
use adv_machine::outcome::DeferredContinuation;

pub(super) const INTERNAL: &str = "<win32>";
pub(super) const THREAD_START: &str = "thread_start";

#[cfg(test)]
mod tests;

pub(crate) fn install_startup<B: Backend>(machine: &mut Machine<B>) -> Result<(), MemFault> {
    machine.sys.process_return = Some(exit_process);
    if machine
        .sys
        .thunks
        .resolver()
        .host_fn(INTERNAL, "startup_enter")
        .is_none()
    {
        return Ok(());
    }
    let enter = machine
        .sys
        .thunks
        .resolve(&mut machine.cpu, INTERNAL, "startup_enter")?;
    let leave = machine
        .sys
        .thunks
        .resolve(&mut machine.cpu, INTERNAL, "startup_leave")?;
    machine.sys.process_init.push_front(InitCall {
        target: enter,
        args: vec![],
        what: "Win32 startup gate acquire".into(),
        failure_exit: Some(0xc0000017),
        begin: None,
    });
    machine.sys.process_ready.push(InitCall {
        target: leave,
        args: vec![],
        what: "Win32 startup gate release".into(),
        failure_exit: Some(0xc0000142),
        begin: None,
    });
    Ok(())
}

pub(super) fn startup_enter(env: &mut Env<'_>) -> Outcome {
    let Some(handle) = loader_mutex(env) else {
        return Outcome::Return(0);
    };
    if env.sys.try_acquire_objects(&[handle], false).is_some() {
        return Outcome::Return(1);
    }
    Outcome::block(
        Wait::Objects {
            handles: vec![handle],
            all: false,
            deadline: None,
            notification: None,
        },
        |_, _| Outcome::Return(1),
    )
}

pub(super) fn startup_leave(env: &mut Env<'_>) -> Outcome {
    let Some(handle) = env.ext.get::<Win32>().loader_lock else {
        return Outcome::Fail("missing startup loader lock".into());
    };
    release_lock(env, handle, Outcome::Return(1))
}

struct Notification {
    module: ModuleId,
    target: u32,
    reason: u32,
    reserved: u32,
    dllmain: bool,
}

fn notifications(env: &Env<'_>, reason: u32) -> VecDeque<Notification> {
    let modules = adv_os::modules(env.ext);
    let mut order = modules.initialization_order();
    if reason != 2 {
        order.reverse();
    }
    let mut calls = VecDeque::new();
    for id in order {
        let module = modules.get(id);
        if !module.lifetime.attach_started || module.lifetime.unloading {
            continue;
        }
        for &target in &module.lifetime.callbacks {
            calls.push_back(Notification {
                module: id,
                target,
                reason,
                reserved: 0,
                dllmain: false,
            });
        }
        if module.is_dll && module.entry != 0 && (reason == 0 || module.lifetime.thread_calls) {
            calls.push_back(Notification {
                module: id,
                target: module.entry,
                reason,
                reserved: u32::from(reason == 0),
                dllmain: true,
            });
        }
    }
    calls
}

fn run(
    env: &mut Env<'_>,
    mut calls: VecDeque<Notification>,
    then: DeferredContinuation,
) -> Outcome {
    while let Some(call) = calls.pop_front() {
        // A prior callback can unload another module or disable notifications.
        // Never reinterpret a retired identity as a later image at the same base.
        let Some(module) = adv_os::modules(env.ext).try_get(call.module) else {
            continue;
        };
        if call.reason != 0 && call.dllmain && !module.lifetime.thread_calls {
            continue;
        }
        return Outcome::call(
            call.target,
            vec![module.base, call.reason, call.reserved],
            move |env, _| run(env, calls, then),
        );
    }
    then(env)
}

/// Terminal APIs must never turn a loader-lock allocation failure into a return.
/// With no callbacks, cleanup itself needs no loader mutex. If we must wait for
/// the lock, collect again after acquisition so intervening module changes are
/// observed; `run` also revalidates each identity before invoking guest code.
fn terminal_notifications(env: &mut Env<'_>, reason: u32, then: DeferredContinuation) -> Outcome {
    if notifications(env, reason).is_empty() {
        return then(env);
    }
    if loader_mutex(env).is_none() {
        return Outcome::Fail("terminal DLL/TLS notification loader lock allocation failed".into());
    }
    locked(env, move |env| {
        let calls = notifications(env, reason);
        run(env, calls, then)
    })
}

pub(crate) fn spawn_thread(
    env: &mut Env<'_>,
    name: &str,
    start: u32,
    param: u32,
    stack: u32,
) -> Result<CtxId, MemFault> {
    let wrapper = env.sys.thunks.resolve(env.cpu, INTERNAL, THREAD_START)?;
    env.sys
        .spawn(env.cpu, name, wrapper, &[start, param], stack)
}

pub(super) fn thread_start(env: &mut Env<'_>) -> Outcome {
    let (start, param) = (env.arg(0), env.arg(1));
    // Acquire only around notifications; the thread procedure itself must run
    // after releasing the loader lock or thread joins would deadlock.
    let locked_start = locked(env, |env| {
        let calls = notifications(env, 2);
        run(env, calls, Box::new(|_| Outcome::Return(0)))
    });
    after(
        env,
        locked_start,
        Box::new(move |_| {
            Outcome::call(start, vec![param], |env, returned| {
                exit_thread(env, returned.value())
            })
        }),
    )
}

pub(super) fn after(env: &mut Env<'_>, outcome: Outcome, then: DeferredContinuation) -> Outcome {
    match outcome {
        Outcome::Call {
            target,
            args,
            then: resume,
            stack,
        } => Outcome::Call {
            target,
            args,
            stack,
            then: Box::new(move |env, value| {
                let next = resume(env, value);
                after(env, next, then)
            }),
        },
        Outcome::Block {
            wait,
            then: resume,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            allow_time_skip,
            then: Box::new(move |env, value| {
                let next = resume(env, value);
                after(env, next, then)
            }),
        },
        Outcome::Defer { then: resume } => Outcome::defer(move |env| {
            let next = resume(env);
            after(env, next, then)
        }),
        Outcome::Return(_) => then(env),
        terminal => terminal,
    }
}

pub(crate) fn exit_thread(env: &mut Env<'_>, code: u32) -> Outcome {
    if !env
        .ext
        .get_mut::<Win32>()
        .exiting_threads
        .insert(env.context)
    {
        return Outcome::ExitThread(code);
    }
    terminal_notifications(
        env,
        3,
        Box::new(move |env| {
            crate::user32::clipboard::exit_thread(env, move |env| {
                if let Err(error) = crate::msvcrt::detach_thread(env) {
                    return Outcome::Fail(error);
                }
                if env
                    .sys
                    .contexts
                    .iter()
                    .all(|context| context.id == env.context || context.is_done())
                {
                    exit_process(env, code)
                } else {
                    Outcome::ExitThread(code)
                }
            })
        }),
    )
}

pub(crate) fn exit_process(env: &mut Env<'_>, code: u32) -> Outcome {
    if std::mem::replace(&mut env.ext.get_mut::<Win32>().process_exiting, true) {
        return Outcome::ExitProcess(code);
    }
    env.sys.terminate_other_contexts(code);
    terminal_notifications(
        env,
        0,
        Box::new(move |env| {
            match crate::msvcrt::detach_process(env)
                .and_then(|()| crate::gdi32::font::detach_process(env))
            {
                Ok(()) => Outcome::ExitProcess(code),
                Err(error) => Outcome::Fail(error),
            }
        }),
    )
}
