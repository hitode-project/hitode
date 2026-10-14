//! Native heap frontends share the process-owned Win32/CRT allocator.
use crate::util::{err, last_error, set_last_error};
use adv_abi::{GuestPtr, Handle, Registry, warn_unavailable, win32};
use adv_cpu::backend::Reg;
use adv_machine::{Env, Outcome};
use adv_os::Heaps;

const HEAP_GENERATE_EXCEPTIONS: u32 = 4;
const STATUS_NO_MEMORY: u32 = 0xc000_0017;

fn effective_flags(env: &mut Env<'_>, heap: u32, flags: u32, api: &str) -> Option<u32> {
    match env.ext.get::<Heaps>().generates_exceptions(heap) {
        Some(true) => Some(flags | HEAP_GENERATE_EXCEPTIONS),
        Some(false) => Some(flags),
        None => {
            // Unowned storage is an invalid/corrupt heap, not an OOM result.
            // Keep its unsupported exception behavior visible instead of raising
            // a misleading STATUS_NO_MEMORY or inheriting the process heap policy.
            warn_unavailable(
                env,
                api,
                "heap handle is not owned by this process; invalid/corrupt heap exception behavior is unavailable",
            );
            None
        }
    }
}

/// Publish failure only after `with_heap` has released its temporary recursion.
/// A waiter repeats this check after it obtains and releases the same heap lock.
fn allocation_result(
    env: &mut Env<'_>,
    result: Outcome,
    flags: u32,
    size: u32,
    resume_eip: u32,
) -> Outcome {
    match result {
        Outcome::Return(0)
            if flags & HEAP_GENERATE_EXCEPTIONS != 0
                && last_error(env) == err::NOT_ENOUGH_MEMORY =>
        {
            // A vectored handler may continue this API's captured epilogue.
            // Allocation still failed; do not expose its previous EAX as success.
            env.cpu.set_reg(Reg::Eax, 0);
            crate::kernel32::raise_status(env, STATUS_NO_MEMORY, 0, &[size], resume_eip)
        }
        Outcome::Block {
            wait,
            then,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            then: Box::new(move |env, resume| {
                let result = then(env, resume);
                allocation_result(env, result, flags, size, resume_eip)
            }),
            allow_time_skip,
        },
        other => other,
    }
}

#[win32(ntdll, name = "RtlAllocateHeap", level = state)]
fn allocate(env: &mut Env<'_>, heap: GuestPtr<u8>, flags: u32, size: u32) -> Outcome {
    allocate_shared(env, heap.addr(), flags, size)
}

pub(crate) fn allocate_shared(env: &mut Env<'_>, heap: u32, flags: u32, size: u32) -> Outcome {
    if flags & !(1 | HEAP_GENERATE_EXCEPTIONS | 8) != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!RtlAllocateHeap",
            &format!(
                "native heap allocation flags {flags:#x} are unavailable (supported: NO_SERIALIZE, GENERATE_EXCEPTIONS and ZERO_MEMORY)"
            ),
        );
        return Outcome::Return(0);
    }
    let Some(flags) = effective_flags(env, heap, flags, "ntdll.dll!RtlAllocateHeap") else {
        return Outcome::Return(0);
    };
    // A blocked backend parks EIP before the wake continuation runs. Retain the
    // actual entry's epilogue, including a guest trampoline or relocated thunk.
    let resume_eip = env.cpu.reg(Reg::Eip);
    let result = crate::heap_lock::with_heap(env, heap, flags, move |env| {
        let pointer = env
            .ext
            .get_mut::<Heaps>()
            .alloc(env.cpu, &mut env.sys.vm, heap, size, flags & 8 != 0)
            .unwrap_or(0);
        if pointer == 0 {
            set_last_error(env, err::NOT_ENOUGH_MEMORY);
        }
        Outcome::Return(pointer)
    });
    allocation_result(env, result, flags, size, resume_eip)
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
        reallocate_shared(env, env.arg(0), env.arg(1), env.arg(2), env.arg(3))
    });
    crate::stdcall(registry, "ntdll.dll", "RtlSizeHeap", 3, |env| {
        size(env, env.arg(0), env.arg(1), env.arg(2))
    });
}

pub(crate) fn reallocate_shared(
    env: &mut Env<'_>,
    heap: u32,
    flags: u32,
    pointer: u32,
    size: u32,
) -> Outcome {
    if flags & !(1 | HEAP_GENERATE_EXCEPTIONS | 8 | 0x10) != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!RtlReAllocateHeap",
            &format!(
                "native heap reallocation flags {flags:#x} are unavailable (supported: NO_SERIALIZE, GENERATE_EXCEPTIONS, ZERO_MEMORY and REALLOC_IN_PLACE_ONLY)"
            ),
        );
        return Outcome::Return(0);
    }
    let Some(flags) = effective_flags(env, heap, flags, "ntdll.dll!RtlReAllocateHeap") else {
        return Outcome::Return(0);
    };
    let resume_eip = env.cpu.reg(Reg::Eip);
    let result = crate::heap_lock::with_heap(env, heap, flags, move |env| {
        crate::kernel32::memory::reallocate(env, heap, flags, pointer, size)
    });
    allocation_result(env, result, flags, size, resume_eip)
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
                .size(env.cpu, heap, pointer)
                .unwrap_or(u32::MAX),
        )
    })
}
