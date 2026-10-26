//! The POSIX thread locks Wine's Unix libraries use, after glibc's i386
//! layouts.
//!
//! A lock's state is kept here, by its guest address; the guest object
//! holds only what its initializer sets, such as a mutex's type. A thread
//! that finds a lock taken yields and tries again, since the machine's
//! threads are its own contexts.

use std::collections::HashMap;

use adv_cpu::backend::GuestExt;
use adv_machine::{Conv, CtxId, Env, Outcome, Wait};

use crate::Function;

/// `__kind` in glibc's i386 `pthread_mutex_t`.
const MUTEX_KIND: u32 = 12;
const MUTEX_SIZE: usize = 24;
const MUTEX_RECURSIVE: u32 = 1;
const RWLOCK_SIZE: usize = 32;
const EDEADLK: u32 = 35;
const EPERM: u32 = 1;
const ONCE_RUNNING: u32 = 1;
const ONCE_DONE: u32 = 2;

pub fn function(name: &str) -> Option<Function> {
    let cdecl = |argc| Conv::Cdecl { argc };
    Some(match name {
        "pthread_mutexattr_init" => (cdecl(1), mutexattr_init),
        "pthread_mutexattr_destroy" => (cdecl(1), done),
        "pthread_mutexattr_settype" => (cdecl(2), mutexattr_settype),
        "pthread_mutex_init" => (cdecl(2), mutex_init),
        "pthread_mutex_destroy" => (cdecl(1), mutex_destroy),
        "pthread_mutex_lock" => (cdecl(1), mutex_lock),
        "pthread_mutex_unlock" => (cdecl(1), mutex_unlock),
        "pthread_rwlock_init" => (cdecl(2), rwlock_init),
        "pthread_rwlock_destroy" => (cdecl(1), rwlock_destroy),
        "pthread_rwlock_rdlock" => (cdecl(1), rwlock_rdlock),
        "pthread_rwlock_wrlock" => (cdecl(1), rwlock_wrlock),
        "pthread_rwlock_unlock" => (cdecl(1), rwlock_unlock),
        "pthread_once" => (cdecl(2), once),
        _ => return None,
    })
}

#[derive(Default)]
struct Locks {
    /// Each held mutex's owner and depth.
    mutexes: HashMap<u32, (CtxId, u32)>,
    rwlocks: HashMap<u32, Rwlock>,
}

#[derive(Default)]
struct Rwlock {
    writer: Option<CtxId>,
    readers: u32,
}

fn locks<'a>(env: &'a mut Env<'_>) -> &'a mut Locks {
    env.ext.get_or_default::<Locks>()
}

fn done(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(0)
}

/// Runs `attempt` again once the other threads have run.
fn retry(attempt: fn(&mut Env<'_>, u32) -> Outcome, object: u32) -> Outcome {
    Outcome::Block {
        wait: Wait::Yield,
        then: Box::new(move |env, _| attempt(env, object)),
        allow_time_skip: false,
    }
}

fn write_zeros(env: &mut Env<'_>, address: u32, size: usize) -> Result<(), String> {
    env.cpu
        .write(address, &vec![0; size])
        .map_err(|error| format!("lock at {address:#x}: {error}"))
}

fn mutexattr_init(env: &mut Env<'_>) -> Outcome {
    match write_zeros(env, env.arg(0), 4) {
        Ok(()) => Outcome::Return(0),
        Err(error) => Outcome::Fail(error),
    }
}

fn mutexattr_settype(env: &mut Env<'_>) -> Outcome {
    let (attr, kind) = (env.arg(0), env.arg(1));
    match env.cpu.write_u32(attr, kind) {
        Ok(()) => Outcome::Return(0),
        Err(error) => Outcome::Fail(format!("mutex attributes at {attr:#x}: {error}")),
    }
}

fn mutex_init(env: &mut Env<'_>) -> Outcome {
    let (mutex, attr) = (env.arg(0), env.arg(1));
    let kind = if attr == 0 {
        Ok(0)
    } else {
        env.cpu.read_u32(attr)
    };
    let initialized = write_zeros(env, mutex, MUTEX_SIZE).and_then(|()| {
        let kind = kind.map_err(|error| format!("mutex attributes at {attr:#x}: {error}"))?;
        env.cpu
            .write_u32(mutex + MUTEX_KIND, kind)
            .map_err(|error| error.to_string())
    });
    locks(env).mutexes.remove(&mutex);
    match initialized {
        Ok(()) => Outcome::Return(0),
        Err(error) => Outcome::Fail(error),
    }
}

fn mutex_destroy(env: &mut Env<'_>) -> Outcome {
    let mutex = env.arg(0);
    locks(env).mutexes.remove(&mutex);
    Outcome::Return(0)
}

fn mutex_lock(env: &mut Env<'_>) -> Outcome {
    lock_mutex(env, env.arg(0))
}

fn lock_mutex(env: &mut Env<'_>, mutex: u32) -> Outcome {
    let Ok(kind) = env.cpu.read_u32(mutex + MUTEX_KIND) else {
        return Outcome::Fail(format!("mutex at {mutex:#x} is not readable"));
    };
    let context = env.context;
    match locks(env).mutexes.get_mut(&mutex) {
        None => {
            locks(env).mutexes.insert(mutex, (context, 1));
            Outcome::Return(0)
        }
        Some((owner, depth)) if *owner == context => {
            if kind & 3 == MUTEX_RECURSIVE {
                *depth += 1;
                Outcome::Return(0)
            } else {
                Outcome::Return(EDEADLK)
            }
        }
        Some(_) => retry(lock_mutex, mutex),
    }
}

fn mutex_unlock(env: &mut Env<'_>) -> Outcome {
    let (mutex, context) = (env.arg(0), env.context);
    let mutexes = &mut locks(env).mutexes;
    match mutexes.get_mut(&mutex) {
        Some((owner, depth)) if *owner == context => {
            *depth -= 1;
            if *depth == 0 {
                mutexes.remove(&mutex);
            }
            Outcome::Return(0)
        }
        _ => Outcome::Return(EPERM),
    }
}

fn rwlock_init(env: &mut Env<'_>) -> Outcome {
    let rwlock = env.arg(0);
    locks(env).rwlocks.remove(&rwlock);
    match write_zeros(env, rwlock, RWLOCK_SIZE) {
        Ok(()) => Outcome::Return(0),
        Err(error) => Outcome::Fail(error),
    }
}

fn rwlock_destroy(env: &mut Env<'_>) -> Outcome {
    let rwlock = env.arg(0);
    locks(env).rwlocks.remove(&rwlock);
    Outcome::Return(0)
}

fn rwlock_rdlock(env: &mut Env<'_>) -> Outcome {
    read_lock(env, env.arg(0))
}

fn read_lock(env: &mut Env<'_>, rwlock: u32) -> Outcome {
    let context = env.context;
    let lock = locks(env).rwlocks.entry(rwlock).or_default();
    match lock.writer {
        None => {
            lock.readers += 1;
            Outcome::Return(0)
        }
        Some(writer) if writer == context => Outcome::Return(EDEADLK),
        Some(_) => retry(read_lock, rwlock),
    }
}

fn rwlock_wrlock(env: &mut Env<'_>) -> Outcome {
    write_lock(env, env.arg(0))
}

fn write_lock(env: &mut Env<'_>, rwlock: u32) -> Outcome {
    let context = env.context;
    let lock = locks(env).rwlocks.entry(rwlock).or_default();
    match lock.writer {
        None if lock.readers == 0 => {
            lock.writer = Some(context);
            Outcome::Return(0)
        }
        Some(writer) if writer == context => Outcome::Return(EDEADLK),
        _ => retry(write_lock, rwlock),
    }
}

fn rwlock_unlock(env: &mut Env<'_>) -> Outcome {
    let (rwlock, context) = (env.arg(0), env.context);
    let lock = locks(env).rwlocks.entry(rwlock).or_default();
    if lock.writer == Some(context) {
        lock.writer = None;
    } else if lock.readers > 0 {
        lock.readers -= 1;
    } else {
        return Outcome::Return(EPERM);
    }
    Outcome::Return(0)
}

/// pthread_once(once, init): the control word goes from 0 through running
/// to done.
fn once(env: &mut Env<'_>) -> Outcome {
    let (control, init) = (env.arg(0), env.arg(1));
    run_once(env, control, init)
}

fn run_once(env: &mut Env<'_>, control: u32, init: u32) -> Outcome {
    match env.cpu.read_u32(control) {
        Ok(ONCE_DONE) => Outcome::Return(0),
        Ok(ONCE_RUNNING) => Outcome::Block {
            wait: Wait::Yield,
            then: Box::new(move |env, _| run_once(env, control, init)),
            allow_time_skip: false,
        },
        Ok(_) => {
            if env.cpu.write_u32(control, ONCE_RUNNING).is_err() {
                return Outcome::Fail(format!("once control at {control:#x} is not writable"));
            }
            Outcome::call_aligned(env, init, [], move |env, _| {
                match env.cpu.write_u32(control, ONCE_DONE) {
                    Ok(()) => Outcome::Return(0),
                    Err(error) => Outcome::Fail(error.to_string()),
                }
            })
        }
        Err(error) => Outcome::Fail(format!("once control at {control:#x}: {error}")),
    }
}

#[cfg(test)]
// spec: A64-CTL-12
#[test]
fn mutex_leaves_lock_recurse_unlock_and_decline() {
    use adv_cpu::JitCpu;
    use adv_cpu::backend::{Guest, Prot, Reg};
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        crate::Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    let (mutex, stack) = (0x410000, 0x410800);
    m.cpu.map(mutex, 4096, Prot::RW).unwrap();
    m.cpu
        .write_u32(mutex + MUTEX_KIND, MUTEX_RECURSIVE)
        .unwrap();
    m.cpu.write_u32(stack + 4, mutex).unwrap();
    m.cpu.set_reg(Reg::Esp, stack);
    m.cpu.set_reg(Reg::Edx, 0x12345678);
    for (name, context, result, held) in [
        ("pthread_mutex_lock", 0, Some(0), Some((0, 1))),
        ("pthread_mutex_lock", 0, Some(0), Some((0, 2))),
        ("pthread_mutex_unlock", 0, Some(0), Some((0, 1))),
        ("pthread_mutex_lock", 1, None, Some((0, 1))),
        ("pthread_mutex_unlock", 0, Some(0), None),
    ] {
        let entry = m.resolve_import(crate::LIBC, name).unwrap();
        let index = (entry - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
        let binding = m.sys.thunks.get(index).f;
        let mut env = Env {
            context,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: index,
        };
        assert_eq!(
            binding.env_leaf.unwrap()(&mut env),
            result.map(|v| (0x12345678u64 << 32) | v)
        );
        assert_eq!(locks(&mut env).mutexes.get(&mutex).copied(), held);
        assert_eq!(env.cpu.reg(Reg::Esp), stack);
    }
}
