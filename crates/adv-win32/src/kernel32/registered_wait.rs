//! Legacy registered waits use ordinary guest workers and scheduler wait objects.
use crate::util::set_last_error;
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Object, Outcome, Resume, Wait, WakeReason};

const WORKER: &str = "<registered-wait-worker>";
const ONCE: u32 = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Legacy,
    Timer,
    Wait,
}

struct Registration {
    target: u32,
    cancel: u32,
    callback: u32,
    argument: u32,
    timeout: u32,
    flags: u32,
    worker: u32,
    running: bool,
    canceled: bool,
    fired: bool,
    completion: u32,
    idle: u32,
    kind: Kind,
    public: u32,
    generation: u64,
    due: Option<u64>,
}

fn fail(env: &mut Env<'_>, code: u32) -> Outcome {
    set_last_error(env, code);
    Outcome::Return(0)
}

#[win32(kernel32, name = "RegisterWaitForSingleObject", level = implement)]
fn register_wait(
    env: &mut Env<'_>,
    output: GuestPtr<u32>,
    target: Handle,
    callback: GuestPtr<()>,
    argument: GuestPtr<()>,
    timeout: u32,
    flags: u32,
) -> Outcome {
    if callback.is_null() {
        return fail(env, 87);
    }
    if flags & !(ONCE | 4 | 0x10) != 0 {
        adv_abi::warn_unavailable(
            env,
            "RegisterWaitForSingleObject",
            "requested worker policy is unavailable",
        );
        return fail(env, 50);
    }
    if let Some(Object::Process { access, .. } | Object::RemoteThread { access, .. }) =
        env.sys.objects.get(target.0)
        && access & 0x100000 == 0
    {
        return fail(env, 5);
    }
    match env.sys.objects.get(target.0) {
        Some(
            Object::Event { .. }
            | Object::Timer { .. }
            | Object::Semaphore { .. }
            | Object::Process { .. }
            | Object::Thread { .. }
            | Object::RemoteThread { .. },
        ) => {}
        Some(Object::Mutex { .. }) => {
            adv_abi::warn_unavailable(
                env,
                "RegisterWaitForSingleObject",
                "mutex ownership on registered waits is unavailable",
            );
            return fail(env, 50);
        }
        _ => return fail(env, 6),
    }
    if adv_abi::guest::check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return fail(env, 998);
    }
    let Some(target) = env.sys.objects.duplicate(target.0) else {
        return fail(env, 8);
    };
    let handle = create(
        env,
        target,
        callback.addr(),
        argument.addr(),
        timeout,
        flags,
        Kind::Legacy,
    );
    if handle == 0 {
        return fail(env, 8);
    }
    output
        .write(env.cpu, handle)
        .expect("checked registration output");
    Outcome::Return(1)
}

fn create(
    env: &mut Env<'_>,
    target: u32,
    callback: u32,
    argument: u32,
    timeout: u32,
    flags: u32,
    kind: Kind,
) -> u32 {
    let Ok(entry) = env.sys.thunks.resolve(env.cpu, super::K32, WORKER) else {
        env.sys.objects.close(target);
        return 0;
    };
    let cancel = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    if cancel == 0 {
        env.sys.objects.close(target);
        return 0;
    }
    let idle = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: true,
    });
    if idle == 0 {
        env.sys.objects.close(target);
        env.sys.objects.close(cancel);
        return 0;
    }
    let handle = env.sys.objects.insert_host(Registration {
        target,
        cancel,
        callback,
        argument,
        timeout,
        flags,
        worker: u32::MAX,
        running: false,
        canceled: false,
        fired: false,
        completion: 0,
        idle,
        kind,
        public: 0,
        generation: 0,
        due: None,
    });
    let private = env.sys.objects.duplicate(handle);
    let worker = private.and_then(|private| {
        match super::module::spawn_thread(env, "registered wait", entry, private, 0x100000) {
            Ok(worker) => Some(worker),
            Err(_) => {
                env.sys.objects.close(private);
                None
            }
        }
    });
    let Some(worker) = worker else {
        env.sys.objects.close(handle);
        env.sys.objects.close(cancel);
        env.sys.objects.close(target);
        env.sys.objects.close(idle);
        return 0;
    };
    let registration = env.sys.objects.host_mut::<Registration>(handle).unwrap();
    registration.worker = worker;
    registration.public = handle;
    handle
}

fn finish(env: &mut Env<'_>, private: u32) -> Outcome {
    let registration = env.sys.objects.host::<Registration>(private).unwrap();
    let (target, cancel, completion, idle) = (
        registration.target,
        registration.cancel,
        registration.completion,
        registration.idle,
    );
    if completion != 0 {
        env.sys.set_event(completion);
        env.sys.objects.close(completion);
    }
    env.sys.objects.close(target);
    env.sys.objects.close(cancel);
    env.sys.objects.close(idle);
    env.sys.objects.close(private);
    Outcome::Return(0)
}

fn wait(env: &mut Env<'_>, private: u32) -> Outcome {
    let registration = env.sys.objects.host::<Registration>(private).unwrap();
    if registration.canceled {
        return finish(env, private);
    }
    let (target, cancel, timeout, fired, flags, generation) = (
        registration.target,
        registration.cancel,
        registration.timeout,
        registration.fired,
        registration.flags,
        registration.generation,
    );
    env.sys.reset_event(cancel);
    if target == 0 || (fired && flags & ONCE != 0) {
        return Outcome::block(
            Wait::Object {
                handle: cancel,
                deadline: None,
            },
            move |env, _| wait(env, private),
        );
    }
    let configured_due = env.sys.objects.host::<Registration>(private).unwrap().due;
    let deadline = configured_due.or_else(|| {
        (timeout != u32::MAX).then(|| {
            env.sys
                .clock
                .now_us()
                .saturating_add(u64::from(timeout) * 1000)
        })
    });
    Outcome::block(
        Wait::Objects {
            handles: vec![cancel, target],
            all: false,
            deadline,
            notification: None,
        },
        move |env, reason| {
            let registration = env.sys.objects.host_mut::<Registration>(private).unwrap();
            if registration.canceled {
                return finish(env, private);
            }
            if registration.generation != generation {
                return wait(env, private);
            }
            let timed_out = matches!(reason, Resume::Woke(WakeReason::Timeout));
            let (callback, argument, idle, kind, public) = (
                registration.callback,
                registration.argument,
                registration.idle,
                registration.kind,
                registration.public,
            );
            registration.running = true;
            registration.fired = true;
            env.sys.reset_event(idle);
            let args = match kind {
                Kind::Timer => vec![private, argument, public],
                Kind::Wait => vec![private, argument, public, if timed_out { 258 } else { 0 }],
                Kind::Legacy => vec![argument, u32::from(timed_out)],
            };
            Outcome::call(callback, args, move |env, _| {
                env.sys
                    .objects
                    .host_mut::<Registration>(private)
                    .unwrap()
                    .running = false;
                env.sys.set_event(idle);
                Outcome::block(Wait::Yield, move |env, _| wait(env, private))
            })
        },
    )
}

fn worker(env: &mut Env<'_>) -> Outcome {
    wait(env, env.arg(0))
}

fn unregister(env: &mut Env<'_>, handle: u32, completion: u32) -> Outcome {
    let Some(registration) = env.sys.objects.host::<Registration>(handle) else {
        return fail(env, 6);
    };
    if registration.canceled {
        return fail(env, 6);
    }
    let (worker, running, cancel) = (
        registration.worker,
        registration.running,
        registration.cancel,
    );
    if completion == u32::MAX && worker == env.context {
        return fail(env, 87);
    }
    let completion_alias = if completion != 0 && completion != u32::MAX {
        if !matches!(env.sys.objects.get(completion), Some(Object::Event { .. })) {
            return fail(env, 6);
        }
        let Some(alias) = env.sys.objects.duplicate(completion) else {
            return fail(env, 8);
        };
        alias
    } else {
        0
    };
    let registration = env.sys.objects.host_mut::<Registration>(handle).unwrap();
    registration.canceled = true;
    registration.completion = completion_alias;
    env.sys.set_event(cancel);
    env.sys.objects.close(handle);
    if completion == u32::MAX {
        let thread = env.sys.contexts[worker as usize].thread_handle;
        Outcome::block_then_return(
            Wait::Object {
                handle: thread,
                deadline: None,
            },
            1,
        )
    } else if running {
        fail(env, 997)
    } else {
        Outcome::Return(1)
    }
}

#[win32(kernel32, name = "UnregisterWait", level = implement)]
fn unregister_wait(env: &mut Env<'_>, handle: Handle) -> Outcome {
    unregister(env, handle.0, 0)
}
#[win32(kernel32, name = "UnregisterWaitEx", level = implement)]
fn unregister_wait_ex(env: &mut Env<'_>, handle: Handle, completion: Handle) -> Outcome {
    unregister(env, handle.0, completion.0)
}

#[win32(kernel32, name = "CreateThreadpoolTimer", level = implement)]
fn create_timer(
    env: &mut Env<'_>,
    callback: GuestPtr<()>,
    argument: GuestPtr<()>,
    environment: GuestPtr<()>,
) -> u32 {
    if callback.is_null() {
        set_last_error(env, 87);
        return 0;
    }
    if !environment.is_null() {
        adv_abi::warn_unavailable(
            env,
            "CreateThreadpoolTimer",
            "custom callback environments are unavailable",
        );
        set_last_error(env, 50);
        return 0;
    }
    let target = env.sys.objects.insert(Object::Timer {
        manual_reset: false,
        signaled: false,
        deadline: None,
        period_us: 0,
    });
    if target == 0 {
        set_last_error(env, 8);
        return 0;
    }
    let handle = create(
        env,
        target,
        callback.addr(),
        argument.addr(),
        u32::MAX,
        0,
        Kind::Timer,
    );
    if handle == 0 {
        set_last_error(env, 8);
    }
    handle
}

fn timer_target(env: &Env<'_>, handle: u32) -> Option<u32> {
    env.sys
        .objects
        .host::<Registration>(handle)
        .filter(|r| r.kind == Kind::Timer && !r.canceled)
        .map(|r| r.target)
}

#[win32(kernel32, name = "SetThreadpoolTimer", level = implement)]
fn set_timer(
    env: &mut Env<'_>,
    handle: u32,
    due: GuestPtr<i64>,
    period: u32,
    _window: u32,
) -> Outcome {
    let Some(target) = timer_target(env, handle) else {
        return fail(env, 6);
    };
    let due = if due.is_null() {
        None
    } else {
        let Ok(value) = due.read(env.cpu) else {
            return fail(env, 998);
        };
        Some(value)
    };
    let Some(due) = due else {
        env.sys.cancel_timer(target);
        return Outcome::Return(0);
    };
    let delay = if due < 0 {
        due.unsigned_abs().div_ceil(10)
    } else {
        (due as u64)
            .saturating_sub(super::time::filetime_now(env))
            .div_ceil(10)
    };
    let deadline = env.sys.clock.now_us().saturating_add(delay);
    env.sys
        .arm_timer(target, deadline, u64::from(period) * 1000);
    Outcome::Return(0)
}

#[win32(kernel32, name = "WaitForThreadpoolTimerCallbacks", level = implement)]
fn wait_timer(env: &mut Env<'_>, handle: u32, cancel: i32) -> Outcome {
    let Some(target) = timer_target(env, handle) else {
        return fail(env, 6);
    };
    if cancel != 0 {
        // Native cancellation also retires an expiration which has not yet
        // started delivery, including the currently scheduled timer deadline.
        env.sys.cancel_timer(target);
        if let Some(Object::Timer { signaled, .. }) = env.sys.objects.get_mut(target) {
            *signaled = false;
        }
        let registration = env.sys.objects.host_mut::<Registration>(handle).unwrap();
        registration.generation = registration.generation.wrapping_add(1);
        let refresh = registration.cancel;
        env.sys.set_event(refresh);
    }
    let registration = env.sys.objects.host::<Registration>(handle).unwrap();
    if !registration.running {
        return Outcome::Return(0);
    }
    let (idle, worker) = (registration.idle, registration.worker);
    if worker == env.context {
        return fail(env, 87);
    }
    Outcome::block_then_return(
        Wait::Object {
            handle: idle,
            deadline: None,
        },
        0,
    )
}

#[win32(kernel32, name = "CloseThreadpoolTimer", level = implement)]
fn close_timer(env: &mut Env<'_>, handle: u32) -> Outcome {
    if timer_target(env, handle).is_none() {
        return fail(env, 6);
    }
    let _ = unregister(env, handle, 0);
    Outcome::Return(0)
}

#[win32(kernel32, name = "CreateThreadpoolWait", level = implement)]
fn create_wait(
    env: &mut Env<'_>,
    callback: GuestPtr<()>,
    argument: GuestPtr<()>,
    environment: GuestPtr<()>,
) -> u32 {
    if callback.is_null() {
        set_last_error(env, 87);
        return 0;
    }
    if !environment.is_null() {
        adv_abi::warn_unavailable(
            env,
            "CreateThreadpoolWait",
            "custom callback environments are unavailable",
        );
        set_last_error(env, 50);
        return 0;
    }
    let handle = create(
        env,
        0,
        callback.addr(),
        argument.addr(),
        u32::MAX,
        ONCE,
        Kind::Wait,
    );
    if handle == 0 {
        set_last_error(env, 8);
    }
    handle
}

#[win32(kernel32, name = "SetThreadpoolWait", level = implement)]
fn set_wait(env: &mut Env<'_>, handle: u32, target: Handle, timeout: GuestPtr<i64>) -> Outcome {
    if !env
        .sys
        .objects
        .host::<Registration>(handle)
        .is_some_and(|r| r.kind == Kind::Wait && !r.canceled)
    {
        return fail(env, 6);
    }
    let due = if target.0 == 0 || timeout.is_null() {
        None
    } else {
        let Ok(value) = timeout.read(env.cpu) else {
            return fail(env, 998);
        };
        let delay = if value < 0 {
            value.unsigned_abs().div_ceil(10)
        } else {
            (value as u64)
                .saturating_sub(super::time::filetime_now(env))
                .div_ceil(10)
        };
        Some(env.sys.clock.now_us().saturating_add(delay))
    };
    let owned = if target.0 == 0 {
        0
    } else {
        if let Some(Object::Process { access, .. } | Object::RemoteThread { access, .. }) =
            env.sys.objects.get(target.0)
            && access & 0x100000 == 0
        {
            return fail(env, 5);
        }
        if !matches!(
            env.sys.objects.get(target.0),
            Some(
                Object::Event { .. }
                    | Object::Timer { .. }
                    | Object::Semaphore { .. }
                    | Object::Process { .. }
                    | Object::Thread { .. }
                    | Object::RemoteThread { .. }
            )
        ) {
            return fail(env, 6);
        }
        let Some(owned) = env.sys.objects.duplicate(target.0) else {
            return fail(env, 8);
        };
        owned
    };
    let registration = env.sys.objects.host_mut::<Registration>(handle).unwrap();
    let old = std::mem::replace(&mut registration.target, owned);
    registration.due = due;
    registration.fired = false;
    registration.generation = registration.generation.wrapping_add(1);
    let refresh = registration.cancel;
    env.sys.set_event(refresh);
    env.sys.objects.close(old);
    Outcome::Return(0)
}

#[win32(kernel32, name = "WaitForThreadpoolWaitCallbacks", level = implement)]
fn wait_wait(env: &mut Env<'_>, handle: u32, cancel: i32) -> Outcome {
    let Some(registration) = env
        .sys
        .objects
        .host_mut::<Registration>(handle)
        .filter(|r| r.kind == Kind::Wait && !r.canceled)
    else {
        return fail(env, 6);
    };
    let (idle, worker, refresh, running) = (
        registration.idle,
        registration.worker,
        registration.cancel,
        registration.running,
    );
    if cancel != 0 {
        registration.generation = registration.generation.wrapping_add(1);
        env.sys.set_event(refresh);
    }
    if !running {
        return Outcome::Return(0);
    }
    if worker == env.context {
        return fail(env, 87);
    }
    Outcome::block_then_return(
        Wait::Object {
            handle: idle,
            deadline: None,
        },
        0,
    )
}

#[win32(kernel32, name = "CloseThreadpoolWait", level = implement)]
fn close_wait(env: &mut Env<'_>, handle: u32) -> Outcome {
    if !env
        .sys
        .objects
        .host::<Registration>(handle)
        .is_some_and(|r| r.kind == Kind::Wait && !r.canceled)
    {
        return fail(env, 6);
    }
    let _ = unregister(env, handle, 0);
    Outcome::Return(0)
}

pub(super) fn register(reg: &mut Registry) {
    reg.register(register_wait_ABI);
    reg.register(unregister_wait_ABI);
    reg.register(unregister_wait_ex_ABI);
    reg.register(create_timer_ABI);
    reg.register(set_timer_ABI);
    reg.register(wait_timer_ABI);
    reg.register(close_timer_ABI);
    reg.register(create_wait_ABI);
    reg.register(set_wait_ABI);
    reg.register(wait_wait_ABI);
    reg.register(close_wait_ABI);
    crate::stdcall(reg, super::K32, WORKER, 1, worker);
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, GuestExt},
    };
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};

    #[test]
    fn timer_cancellation_retires_pending_expirations_and_discards_stale_delivery() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.spawn("test", PARK_ADDR, &[], 65536).unwrap();
        let env = &mut Env {
            context: 0,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        let handle = create_timer(
            env,
            GuestPtr::new(PARK_ADDR),
            GuestPtr::new(0),
            GuestPtr::new(0),
        );
        assert_ne!(handle, 0);
        let target = timer_target(env, handle).unwrap();
        let idle = env.sys.objects.host::<Registration>(handle).unwrap().idle;
        let deadline = env.sys.clock.now_us() + 50_000;
        env.sys.arm_timer(target, deadline, 5_000);
        let pending = wait(env, handle);
        assert!(matches!(wait_timer(env, handle, 1), Outcome::Return(0)));
        assert!(matches!(
            env.sys.objects.get(target),
            Some(Object::Timer {
                deadline: None,
                period_us: 5_000,
                ..
            })
        ));
        let Outcome::Block { then, .. } = pending else {
            panic!("expected parked worker")
        };
        assert!(matches!(
            then(
                env,
                Resume::Woke(WakeReason::Objects {
                    index: 1,
                    abandoned: false
                })
            ),
            Outcome::Block { .. }
        ));
        assert!(matches!(
            env.sys.objects.get(idle),
            Some(Object::Event { signaled: true, .. })
        ));
        let generation = env
            .sys
            .objects
            .host::<Registration>(handle)
            .unwrap()
            .generation;
        assert!(matches!(
            set_timer(env, handle, GuestPtr::new(1), 0, 0),
            Outcome::Return(0)
        ));
        assert_eq!(
            env.sys
                .objects
                .host::<Registration>(handle)
                .unwrap()
                .generation,
            generation
        );
        assert_eq!(crate::util::last_error(env), 998);
    }

    #[test]
    fn invalid_wait_requests_do_not_publish_handles_or_spawn_workers() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.spawn("test", PARK_ADDR, &[], 65536).unwrap();
        machine.cpu.map(0x410000, 4096, Prot::RW).unwrap();
        machine.cpu.write_u32(0x410000, 0xa5a5a5a5).unwrap();
        let event = machine.sys.objects.insert(Object::Event {
            manual_reset: false,
            signaled: false,
        });
        let env = &mut Env {
            context: 0,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        for (handle, flags, error) in [(0, 0, 6), (event, 0x100, 50)] {
            assert!(matches!(
                register_wait(
                    env,
                    GuestPtr::new(0x410000),
                    Handle(handle),
                    GuestPtr::new(PARK_ADDR),
                    GuestPtr::new(0),
                    u32::MAX,
                    flags
                ),
                Outcome::Return(0)
            ));
            assert_eq!(crate::util::last_error(env), error);
            assert_eq!(env.cpu.read_u32(0x410000).unwrap(), 0xa5a5a5a5);
            assert_eq!(env.sys.contexts.len(), 1);
        }
    }
}
