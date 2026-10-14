//! Heap frontends share the real recursive lock owned by each process heap.
use adv_abi::{Handle, Registry, win32};
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome, Wait};
use adv_os::Heaps;

use crate::kernel32::{enter_cs, initialize_cs, leave_cs, release_cs_wait};

const HEAP_NO_SERIALIZE: u32 = 1;
// Heaps reserve their first page for infrastructure, outside caller-owned blocks.
const LOCK_OFFSET: u32 = 0x20;

fn section(env: &mut Env<'_>, heap: u32) -> Result<u32, String> {
    let Some(heap) = env.ext.get::<Heaps>().normalize(heap) else {
        return Err("heap lock handle is not owned by this process".into());
    };
    let synchronization = env.ext.get::<Heaps>().synchronization(heap).unwrap();
    if let Some(lock) = synchronization.lock {
        return Ok(lock);
    }
    let lock = heap + LOCK_OFFSET;
    initialize_cs(env.cpu, lock)
        .map_err(|error| format!("cannot initialize heap lock: {error:?}"))?;
    env.ext
        .get::<Heaps>()
        .gate_small(env.cpu, heap, true)
        .map_err(|e| format!("cannot disable guest heap operations: {e:?}"))?;
    env.ext.get_mut::<Heaps>().set_lock(heap, lock);
    Ok(lock)
}

fn drained(
    env: &mut Env<'_>,
    heap: u32,
    body: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    if env.ext.get::<Heaps>().small_active(env.cpu, heap) != 0 {
        return Outcome::block(Wait::Yield, move |env, _| drained(env, heap, body));
    }
    body(env)
}

fn registry_drained(
    env: &mut Env<'_>,
    body: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    if env.ext.get::<Heaps>().small_registry_active(env.cpu) != 0 {
        return Outcome::block(Wait::Yield, move |env, _| registry_drained(env, body));
    }
    let result = body(env);
    let _ = env.ext.get::<Heaps>().gate_small_registry(env.cpu, false);
    result
}

fn after_enter(
    env: &mut Env<'_>,
    result: Outcome,
    body: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    match result {
        Outcome::Return(_) => body(env),
        Outcome::Block {
            wait,
            then,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            then: Box::new(move |env, resume| {
                let result = then(env, resume);
                after_enter(env, result, body)
            }),
            allow_time_skip,
        },
        other => other,
    }
}

/// Hold only for synchronous heap work. A guest callback or wait returned by
/// `body` runs after this temporary recursion level has been released.
pub(crate) fn with_heap(
    env: &mut Env<'_>,
    heap: u32,
    flags: u32,
    body: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let Some(synchronization) = env.ext.get::<Heaps>().synchronization(heap) else {
        return body(env);
    };
    if flags & HEAP_NO_SERIALIZE != 0 || !synchronization.serialized {
        return body(env);
    }
    let Some(lock) = synchronization.lock else {
        // Controller-owned heap methods finish atomically without yielding.
        // Only a persistent explicit lock can make an ordinary call contend.
        return body(env);
    };
    drained(env, heap, move |env| {
        let result = enter_cs(env, lock);
        after_enter(env, result, move |env| {
            let result = body(env);
            match leave_cs(env, lock) {
                Outcome::Return(_) => result,
                failure => failure,
            }
        })
    })
}

pub(crate) fn inspect(
    env: &mut Env<'_>,
    heap: u32,
    flags: u32,
    body: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    if env
        .ext
        .get::<Heaps>()
        .gate_small(env.cpu, heap, true)
        .is_err()
    {
        return Outcome::Return(0);
    }
    drained(env, heap, move |env| {
        with_heap(env, heap, flags, move |env| {
            let result = body(env);
            let _ = env.ext.get::<Heaps>().gate_small(env.cpu, heap, false);
            result
        })
    })
}

pub(crate) fn with_process_heap(
    env: &mut Env<'_>,
    body: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let heap = env.ext.get::<Heaps>().process_heap;
    with_heap(env, heap, 0, body)
}

pub(crate) fn lock(env: &mut Env<'_>, heap: u32) -> Outcome {
    if heap == 0 {
        return Outcome::Return(0);
    }
    let lock = match section(env, heap) {
        Ok(lock) => lock,
        Err(_) => return Outcome::Return(0),
    };
    drained(env, heap, move |env| {
        let result = enter_cs(env, lock);
        after_enter(env, result, |_| Outcome::Return(1))
    })
}

pub(crate) fn unlock(env: &mut Env<'_>, heap: u32) -> Outcome {
    if heap == 0 {
        return Outcome::Return(0);
    }
    let Some(lock) = env
        .ext
        .get::<Heaps>()
        .synchronization(heap)
        .and_then(|sync| sync.lock)
    else {
        return Outcome::Return(0);
    };
    let tid = env.ctx().tid;
    if env.cpu.read_u32(lock + 12).unwrap_or(0) != tid {
        return Outcome::Return(0);
    }
    match leave_cs(env, lock) {
        Outcome::Return(_) => Outcome::Return(1),
        failure => failure,
    }
}

/// Destroy cannot discard a live continuation or free the storage it resumes on.
pub(crate) fn destroy(env: &mut Env<'_>, heap: u32) -> Outcome {
    if heap == 0 {
        return Outcome::Return(0);
    }
    let Some(sync) = env.ext.get::<Heaps>().synchronization(heap) else {
        return Outcome::Return(0);
    };
    let body = move |env: &mut Env<'_>| {
        if let Some(lock) = sync.lock
            && let Err(error) = release_cs_wait(env, lock)
        {
            if sync.serialized {
                let _ = leave_cs(env, lock);
            }
            return Outcome::Fail(error);
        }
        let destroyed = env
            .ext
            .get_mut::<Heaps>()
            .destroy(env.cpu, &mut env.sys.vm, heap);
        if destroyed && heap == env.ext.get::<Heaps>().process_heap {
            let _ = env.cpu.write_u32(
                adv_machine::layout::PEB_ADDR + adv_machine::layout::peb::PROCESS_HEAP,
                0,
            );
        }
        if !destroyed && let Some(lock) = sync.lock.filter(|_| sync.serialized) {
            let _ = leave_cs(env, lock);
        }
        Outcome::Return(u32::from(destroyed))
    };
    if env
        .ext
        .get::<Heaps>()
        .gate_small(env.cpu, heap, true)
        .is_err()
    {
        return Outcome::Return(0);
    }
    let retire = move |env: &mut Env<'_>| {
        if env
            .ext
            .get::<Heaps>()
            .gate_small_registry(env.cpu, true)
            .is_err()
        {
            return Outcome::Return(0);
        }
        registry_drained(env, body)
    };
    drained(env, heap, move |env| match sync.lock {
        Some(lock) if sync.serialized => {
            let result = enter_cs(env, lock);
            after_enter(env, result, retire)
        }
        _ => retire(env),
    })
}

#[win32(kernel32, name = "HeapLock", level = implement)]
fn heap_lock(env: &mut Env<'_>, heap: Handle) -> Outcome {
    let result = lock(env, heap.0);
    if matches!(result, Outcome::Return(0)) {
        crate::util::set_last_error(env, 6);
    }
    result
}

#[win32(kernel32, name = "HeapUnlock", level = implement)]
fn heap_unlock(env: &mut Env<'_>, heap: Handle) -> Outcome {
    let result = unlock(env, heap.0);
    if matches!(result, Outcome::Return(0)) {
        crate::util::set_last_error(env, 288);
    }
    result
}

pub(crate) fn register(registry: &mut Registry) {
    registry.register(heap_lock_ABI);
    registry.register(heap_unlock_ABI);
}

#[cfg(test)]
mod tests;
