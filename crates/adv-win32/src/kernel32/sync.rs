//! Synchronization. Guest threads are cooperative contexts, so atomics are trivially atomic;
//! contended locks and waits block the context and let others run.

use adv_machine::{Env, Object, Outcome, Registry, Resume, Wait, WakeReason};
use pal_cpu::GuestExt;

use super::K32;
use crate::util::{err, out_u32, set_last_error};
use crate::{Win32, stdcall};

const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 0x102;
const WAIT_FAILED: u32 = u32::MAX;
const INFINITE: u32 = u32::MAX;

// CRITICAL_SECTION (x86): DebugInfo, LockCount, RecursionCount, OwningThread, LockSemaphore, SpinCount.
const CS_LOCK_COUNT: u32 = 4;
const CS_RECURSION: u32 = 8;
const CS_OWNER: u32 = 12;

fn init_cs(env: &mut Env<'_>) -> Outcome {
    let cs = env.arg(0);
    crate::util::zero(env.cpu, cs, 24);
    let _ = env.cpu.write_u32(cs + CS_LOCK_COUNT, u32::MAX);
    Outcome::Return(1)
}

/// Take the lock if free or already ours.
fn try_take(env: &mut Env<'_>, cs: u32) -> bool {
    let tid = env.ctx().tid;
    let owner = env.cpu.read_u32(cs + CS_OWNER).unwrap_or(0);
    if owner != 0 && owner != tid {
        return false;
    }
    let rec = env.cpu.read_u32(cs + CS_RECURSION).unwrap_or(0);
    let lock = env.cpu.read_u32(cs + CS_LOCK_COUNT).unwrap_or(u32::MAX);
    let _ = env.cpu.write_u32(cs + CS_OWNER, tid);
    let _ = env.cpu.write_u32(cs + CS_RECURSION, rec + 1);
    let _ = env.cpu.write_u32(cs + CS_LOCK_COUNT, lock.wrapping_add(1));
    true
}

fn enter_cs(env: &mut Env<'_>, cs: u32) -> Outcome {
    if try_take(env, cs) {
        return Outcome::Return(0);
    }
    // Held by another context: let it run, then retry.
    Outcome::block(Wait::Yield, move |env, _| enter_cs(env, cs))
}

fn leave_cs(env: &mut Env<'_>) -> Outcome {
    let cs = env.arg(0);
    let rec = env.cpu.read_u32(cs + CS_RECURSION).unwrap_or(0);
    if rec == 0 {
        log::warn!(
            "LeaveCriticalSection({cs:#x}) not held, from {:#010x}",
            env.caller()
        );
        return Outcome::Return(0);
    }
    let lock = env.cpu.read_u32(cs + CS_LOCK_COUNT).unwrap_or(0);
    let _ = env.cpu.write_u32(cs + CS_RECURSION, rec - 1);
    let _ = env.cpu.write_u32(cs + CS_LOCK_COUNT, lock.wrapping_sub(1));
    if rec == 1 {
        let _ = env.cpu.write_u32(cs + CS_OWNER, 0);
    }
    Outcome::Return(0)
}

fn interlocked(env: &mut Env<'_>, f: impl Fn(u32, u32, u32) -> (u32, u32)) -> Outcome {
    let (p, a, b) = (env.arg(0), env.arg(1), env.arg(2));
    let old = env.cpu.read_u32(p).unwrap_or(0);
    let (new, ret) = f(old, a, b);
    let _ = env.cpu.write_u32(p, new);
    Outcome::Return(ret)
}

fn wait_result(r: Resume) -> Outcome {
    Outcome::Return(match r {
        Resume::Woke(WakeReason::Signaled) => WAIT_OBJECT_0,
        Resume::Woke(WakeReason::Objects { index, abandoned }) => {
            index + if abandoned { 0x80 } else { WAIT_OBJECT_0 }
        }
        _ => WAIT_TIMEOUT,
    })
}

fn wait_one(env: &mut Env<'_>, h: u32, ms: u32) -> Outcome {
    if !matches!(
        env.sys.objects.get(h),
        Some(
            Object::Event { .. }
                | Object::Mutex { .. }
                | Object::Semaphore { .. }
                | Object::Thread { .. }
        )
    ) {
        log::warn!("wait on unknown handle {h:#x} from {:#010x}", env.caller());
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(WAIT_FAILED);
    }
    if let Some(reason) = env.sys.try_acquire_objects(&[h], false) {
        return wait_result(Resume::Woke(reason));
    }
    if ms == 0 {
        return Outcome::Return(WAIT_TIMEOUT);
    }
    let deadline = (ms != INFINITE).then(|| env.sys.clock.now_us() + u64::from(ms) * 1000);
    Outcome::block(
        Wait::Objects {
            handles: vec![h],
            all: false,
            deadline,
            notification: None,
        },
        |_, r| wait_result(r),
    )
}

fn wait_multiple(env: &mut Env<'_>) -> Outcome {
    let (n, handles, all, ms) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if n == 0 || n > 64 {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(WAIT_FAILED);
    }
    let mut hs = Vec::with_capacity(n as usize);
    for i in 0..n {
        let Ok(h) = env.cpu.read_u32(handles.wrapping_add(4 * i)) else {
            set_last_error(env, err::INVALID_PARAMETER);
            return Outcome::Return(WAIT_FAILED);
        };
        if hs.contains(&h)
            || (all != 0
                && hs
                    .iter()
                    .any(|&other| env.sys.objects.same_object(other, h)))
        {
            set_last_error(env, err::INVALID_PARAMETER);
            return Outcome::Return(WAIT_FAILED);
        }
        if !matches!(
            env.sys.objects.get(h),
            Some(
                Object::Event { .. }
                    | Object::Mutex { .. }
                    | Object::Semaphore { .. }
                    | Object::Thread { .. }
            )
        ) {
            set_last_error(env, err::INVALID_HANDLE);
            return Outcome::Return(WAIT_FAILED);
        }
        hs.push(h);
    }
    if let Some(reason) = env.sys.try_acquire_objects(&hs, all != 0) {
        return wait_result(Resume::Woke(reason));
    }
    if ms == 0 {
        return Outcome::Return(WAIT_TIMEOUT);
    }
    let deadline = (ms != INFINITE).then(|| env.sys.clock.now_us() + u64::from(ms) * 1000);
    Outcome::block(
        Wait::Objects {
            handles: hs,
            all: all != 0,
            deadline,
            notification: None,
        },
        |_, r| wait_result(r),
    )
}

fn created(env: &mut Env<'_>, o: Object) -> Outcome {
    let index = if matches!(o, Object::Mutex { .. }) {
        2
    } else {
        3
    };
    let wide = env.sys.thunks.get(env.thunk).name.ends_with('W');
    super::file_mapping::sync_created(env, o, env.arg(index), wide)
}

fn create_event(env: &mut Env<'_>) -> Outcome {
    let (manual, initial) = (env.arg(1) != 0, env.arg(2) != 0);
    created(
        env,
        Object::Event {
            manual_reset: manual,
            signaled: initial,
        },
    )
}

fn create_semaphore(env: &mut Env<'_>) -> Outcome {
    let (count, max) = (env.arg(1), env.arg(2));
    created(env, Object::Semaphore { count, max })
}

fn release_semaphore(env: &mut Env<'_>) -> Outcome {
    let (h, n, prev) = (env.arg(0), env.arg(1), env.arg(2));
    let Some(Object::Semaphore { count, max }) = env.sys.objects.get_mut(h) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    let old = *count;
    if old + n > *max {
        set_last_error(env, 298); // ERROR_TOO_MANY_POSTS
        return Outcome::Return(0);
    }
    *count += n;
    out_u32(env.cpu, prev, old);
    env.sys.signal_waiters(h);
    Outcome::Return(1)
}

fn create_mutex(env: &mut Env<'_>) -> Outcome {
    let owned = env.arg(1) != 0;
    let cur = env.sys.current;
    let owner = owned.then_some(cur);
    created(
        env,
        Object::Mutex {
            owner,
            recursion: u32::from(owned),
        },
    )
}

fn release_mutex(env: &mut Env<'_>) -> Outcome {
    let h = env.arg(0);
    let cur = env.sys.current;
    let result = match env.sys.objects.get_mut(h) {
        Some(Object::Mutex { owner, recursion }) if *owner == Some(cur) => {
            *recursion -= 1;
            if *recursion == 0 {
                *owner = None;
            }
            Outcome::Return(1)
        }
        _ => {
            set_last_error(env, 288); // ERROR_NOT_OWNER
            Outcome::Return(0)
        }
    };
    if matches!(result, Outcome::Return(1)) {
        env.sys.signal_waiters(h);
    }
    result
}

/// SLIST_HEADER (x86): `Next` pointer, then `Depth` and `Sequence` u16s. Guest threads never
/// run concurrently, so the lock-free list is plain pushes and pops.
fn slist(
    env: &mut Env<'_>,
    head: u32,
    f: impl FnOnce(&mut Env<'_>, u32, u16) -> (u32, u16, u32),
) -> Outcome {
    let next = env.cpu.read_u32(head).unwrap_or(0);
    let ds = env.cpu.read_u32(head + 4).unwrap_or(0);
    let (next2, depth2, ret) = f(env, next, ds as u16);
    let seq2 = (ds >> 16).wrapping_add(1) & 0xffff;
    let _ = env.cpu.write_u32(head, next2);
    let _ = env
        .cpu
        .write_u32(head + 4, u32::from(depth2) | (seq2 << 16));
    Outcome::Return(ret)
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "InitializeSListHead", 1, |e| {
        let _ = e.cpu.write(e.arg(0), &[0; 8]);
        Outcome::Return(0)
    });
    stdcall(reg, K32, "InterlockedPushEntrySList", 2, |e| {
        let (head, entry) = (e.arg(0), e.arg(1));
        slist(e, head, |e, next, depth| {
            let _ = e.cpu.write_u32(entry, next);
            (entry, depth.wrapping_add(1), next)
        })
    });
    stdcall(reg, K32, "InterlockedPopEntrySList", 1, |e| {
        let head = e.arg(0);
        slist(e, head, |e, next, depth| match next {
            0 => (0, 0, 0),
            _ => (
                e.cpu.read_u32(next).unwrap_or(0),
                depth.saturating_sub(1),
                next,
            ),
        })
    });
    stdcall(reg, K32, "InterlockedFlushSList", 1, |e| {
        let head = e.arg(0);
        slist(e, head, |_, next, _| (0, 0, next))
    });
    stdcall(reg, K32, "QueryDepthSList", 1, |e| {
        Outcome::Return(u32::from(e.cpu.read_u16(e.arg(0) + 4).unwrap_or(0)))
    });
    stdcall(reg, K32, "InitializeCriticalSection", 1, init_cs);
    stdcall(
        reg,
        K32,
        "InitializeCriticalSectionAndSpinCount",
        2,
        init_cs,
    );
    stdcall(reg, K32, "InitializeCriticalSectionEx", 3, init_cs);
    stdcall(reg, K32, "DeleteCriticalSection", 1, |_| Outcome::Return(0));
    stdcall(reg, K32, "EnterCriticalSection", 1, |e| {
        let cs = e.arg(0);
        enter_cs(e, cs)
    });
    stdcall(reg, K32, "TryEnterCriticalSection", 1, |e| {
        let cs = e.arg(0);
        Outcome::Return(u32::from(try_take(e, cs)))
    });
    stdcall(reg, K32, "LeaveCriticalSection", 1, leave_cs);
    stdcall(reg, K32, "InterlockedIncrement", 1, |e| {
        interlocked(e, |o, _, _| (o.wrapping_add(1), o.wrapping_add(1)))
    });
    stdcall(reg, K32, "InterlockedDecrement", 1, |e| {
        interlocked(e, |o, _, _| (o.wrapping_sub(1), o.wrapping_sub(1)))
    });
    stdcall(reg, K32, "InterlockedExchange", 2, |e| {
        interlocked(e, |o, v, _| (v, o))
    });
    stdcall(reg, K32, "InterlockedExchangeAdd", 2, |e| {
        interlocked(e, |o, v, _| (o.wrapping_add(v), o))
    });
    stdcall(reg, K32, "InterlockedCompareExchange", 3, |e| {
        interlocked(e, |o, x, c| (if o == c { x } else { o }, o))
    });
    stdcall(reg, K32, "CreateEventA", 4, create_event);
    stdcall(reg, K32, "CreateEventW", 4, create_event);
    stdcall(reg, K32, "SetEvent", 1, |e| {
        let h = e.arg(0);
        Outcome::Return(u32::from(e.sys.set_event(h)))
    });
    stdcall(reg, K32, "ResetEvent", 1, |e| {
        let h = e.arg(0);
        Outcome::Return(u32::from(e.sys.reset_event(h)))
    });
    stdcall(reg, K32, "CreateSemaphoreA", 4, create_semaphore);
    stdcall(reg, K32, "CreateSemaphoreW", 4, create_semaphore);
    stdcall(reg, K32, "ReleaseSemaphore", 3, release_semaphore);
    stdcall(reg, K32, "CreateMutexA", 3, create_mutex);
    stdcall(reg, K32, "CreateMutexW", 3, create_mutex);
    stdcall(reg, K32, "ReleaseMutex", 1, release_mutex);
    stdcall(reg, K32, "WaitForSingleObject", 2, |e| {
        let (h, ms) = (e.arg(0), e.arg(1));
        wait_one(e, h, ms)
    });
    stdcall(reg, K32, "WaitForSingleObjectEx", 3, |e| {
        let (h, ms) = (e.arg(0), e.arg(1));
        wait_one(e, h, ms)
    });
    stdcall(reg, K32, "WaitForMultipleObjects", 4, wait_multiple);
    stdcall(reg, K32, "TlsAlloc", 0, |e| {
        let w = e.ext.get_mut::<Win32>();
        match w.tls_used.iter().position(|u| !u) {
            Some(i) => {
                w.tls_used[i] = true;
                Outcome::Return(i as u32)
            }
            None => Outcome::Return(u32::MAX),
        }
    });
    stdcall(reg, K32, "TlsFree", 1, |e| {
        let i = e.arg(0) as usize;
        let w = e.ext.get_mut::<Win32>();
        Outcome::Return(u32::from(
            i < 64 && std::mem::replace(&mut w.tls_used[i], false),
        ))
    });
    stdcall(reg, K32, "TlsGetValue", 1, |e| {
        let i = e.arg(0);
        let teb = e.ctx().teb;
        let v = if i < 64 {
            e.cpu
                .read_u32(teb + adv_machine::layout::teb::TLS_SLOTS + 4 * i)
                .unwrap_or(0)
        } else {
            0
        };
        set_last_error(e, 0);
        Outcome::Return(v)
    });
    stdcall(reg, K32, "TlsSetValue", 2, |e| {
        let (i, v) = (e.arg(0), e.arg(1));
        let teb = e.ctx().teb;
        if i >= 64 {
            set_last_error(e, err::INVALID_PARAMETER);
            return Outcome::Return(0);
        }
        let _ = e
            .cpu
            .write_u32(teb + adv_machine::layout::teb::TLS_SLOTS + 4 * i, v);
        Outcome::Return(1)
    });
}
