//! Native heap frontends share the process-owned Win32/CRT allocator.
use crate::util::{err, set_last_error};
use adv_abi::{GuestPtr, Handle, Registry, warn_unavailable, win32};
use adv_machine::{Env, Outcome};
use adv_os::Heaps;

#[win32(ntdll, name = "RtlAllocateHeap", level = state)]
fn allocate(env: &mut Env<'_>, heap: GuestPtr<u8>, flags: u32, size: u32) -> Outcome {
    if flags & !(1 | 8) != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!RtlAllocateHeap",
            "native heap allocation flags are unavailable (supported: NO_SERIALIZE and ZERO_MEMORY)",
        );
        return Outcome::Return(0);
    }
    crate::heap_lock::with_heap(env, heap.addr(), flags, move |env| {
        let pointer = env
            .ext
            .get_mut::<Heaps>()
            .alloc(env.cpu, &mut env.sys.vm, heap.addr(), size, flags & 8 != 0)
            .unwrap_or(0);
        if pointer == 0 {
            set_last_error(env, err::NOT_ENOUGH_MEMORY);
        }
        Outcome::Return(pointer)
    })
}

#[win32(ntdll, name = "RtlFreeHeap", level = state)]
fn free(env: &mut Env<'_>, heap: GuestPtr<u8>, flags: u32, pointer: GuestPtr<u8>) -> Outcome {
    if flags & !1 != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!RtlFreeHeap",
            "native heap free flags are unavailable (supported: NO_SERIALIZE)",
        );
        return Outcome::Return(0);
    }
    if pointer.is_null() {
        return Outcome::Return(1);
    }
    // Modern NTDLL returns LOGICAL; the old BOOLEAN caller observes the same AL.
    crate::heap_lock::with_heap(env, heap.addr(), flags, move |env| {
        Outcome::Return(u32::from(crate::kernel32::memory::free(
            env,
            heap.addr(),
            pointer.addr(),
        )))
    })
}

#[win32(ntdll, name = "RtlLockHeap", level = implement)]
fn lock(env: &mut Env<'_>, heap: Handle) -> Outcome {
    crate::heap_lock::lock(env, heap.0)
}

#[win32(ntdll, name = "RtlUnlockHeap", level = implement)]
fn unlock(env: &mut Env<'_>, heap: Handle) -> Outcome {
    crate::heap_lock::unlock(env, heap.0)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(allocate_ABI);
    registry.register(free_ABI);
    registry.register(lock_ABI);
    registry.register(unlock_ABI);
    // SDK x86 ntdll.lib declares _RtlReAllocateHeap@16 and _RtlSizeHeap@12.
    crate::stdcall(registry, "ntdll.dll", "RtlReAllocateHeap", 4, |env| {
        reallocate(env, env.arg(0), env.arg(1), env.arg(2), env.arg(3))
    });
    crate::stdcall(registry, "ntdll.dll", "RtlSizeHeap", 3, |env| {
        size(env, env.arg(0), env.arg(1), env.arg(2))
    });
}

fn reallocate(env: &mut Env<'_>, heap: u32, flags: u32, pointer: u32, size: u32) -> Outcome {
    if flags & !(1 | 8 | 0x10) != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!RtlReAllocateHeap",
            "native heap reallocation flags are unavailable",
        );
        return Outcome::Return(0);
    }
    crate::heap_lock::with_heap(env, heap, flags, move |env| {
        crate::kernel32::memory::reallocate(env, heap, flags, pointer, size)
    })
}

fn size(env: &mut Env<'_>, heap: u32, flags: u32, pointer: u32) -> Outcome {
    if flags & !1 != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!RtlSizeHeap",
            "native heap size flags are unavailable",
        );
        return Outcome::Return(u32::MAX);
    }
    crate::heap_lock::with_heap(env, heap, flags, move |env| {
        Outcome::Return(
            env.ext
                .get_mut::<Heaps>()
                .size(heap, pointer)
                .unwrap_or(u32::MAX),
        )
    })
}
