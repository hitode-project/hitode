//! Native heap frontends share the process-owned Win32/CRT allocator.
use crate::util::{err, set_last_error};
use adv_abi::{GuestPtr, Registry, warn_unavailable, win32};
use adv_machine::Env;
use adv_os::Heaps;

#[win32(ntdll, name = "RtlAllocateHeap", level = state)]
fn allocate(env: &mut Env<'_>, heap: GuestPtr<u8>, flags: u32, size: u32) -> GuestPtr<u8> {
    if flags & !(1 | 8) != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!RtlAllocateHeap",
            "native heap allocation flags are unavailable (supported: NO_SERIALIZE and ZERO_MEMORY)",
        );
        return GuestPtr::new(0);
    }
    let pointer = env
        .ext
        .get_mut::<Heaps>()
        .alloc(env.cpu, &mut env.sys.vm, heap.addr(), size, flags & 8 != 0)
        .unwrap_or(0);
    if pointer == 0 {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
    }
    GuestPtr::new(pointer)
}

#[win32(ntdll, name = "RtlFreeHeap", level = state)]
fn free(env: &mut Env<'_>, heap: GuestPtr<u8>, flags: u32, pointer: GuestPtr<u8>) -> u32 {
    if flags & !1 != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!RtlFreeHeap",
            "native heap free flags are unavailable (supported: NO_SERIALIZE)",
        );
        return 0;
    }
    // Modern NTDLL returns LOGICAL; the old BOOLEAN caller observes the same AL.
    u32::from(
        pointer.is_null()
            || env.ext.get_mut::<Heaps>().free(
                env.cpu,
                &mut env.sys.vm,
                heap.addr(),
                pointer.addr(),
            ),
    )
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(allocate_ABI);
    registry.register(free_ABI);
}
