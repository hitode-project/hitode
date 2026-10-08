//! Heap, Local/Global, and Virtual memory functions.

use adv_abi::Registry;
use adv_machine::{Env, Outcome};

use super::K32;
use crate::stdcall;
use crate::util::{err, set_last_error};
use adv_os::Heaps;
mod dep;
mod global;
pub(crate) use global::{allocate as allocate_local_memory, release as release_local_memory};
mod locks;
mod native;
mod virtual_memory;
pub(crate) use virtual_memory::native_read_memory;
pub(crate) use virtual_memory::native_write_memory;
pub(crate) use virtual_memory::protect_memory;
pub(crate) use virtual_memory::query as query_memory;
pub(crate) use virtual_memory::{
    allocate as allocate_memory, allocation_extent, release as free_memory, span as memory_span,
};

const HEAP_ZERO_MEMORY: u32 = 0x8;
const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 0x10;

/// Highest application address in the existing flat x86 Win32 process profile.
/// GetSystemInfo and VirtualQuery must describe the same address domain.
pub(crate) const MAX_APPLICATION_ADDRESS: u32 = 0x7ffe_ffff;

fn heap_create(env: &mut Env<'_>) -> Outcome {
    let flags = env.arg(0);
    let serialized = flags & 1 == 0; // HEAP_NO_SERIALIZE
    let executable = flags & 0x40000 != 0; // HEAP_CREATE_ENABLE_EXECUTE
    let heaps = env.ext.get_mut::<Heaps>();
    match heaps.create_with_policy(
        env.cpu,
        &mut env.sys.vm,
        serialized,
        executable,
        flags & 4 != 0,
    ) {
        Ok(h) => Outcome::Return(h),
        Err(_) => Outcome::Return(0),
    }
}

fn heap_destroy(env: &mut Env<'_>) -> Outcome {
    let h = env.arg(0);
    crate::heap_lock::destroy(env, h)
}

pub(crate) fn free(env: &mut Env<'_>, heap: u32, p: u32) -> bool {
    if p == 0 {
        return true;
    }
    if let Some(block) = global_allocation_block(env, p) {
        let globals = env.ext.get::<adv_os::global::Globals>();
        let sole = globals
            .identity(&env.sys.objects, block.handle)
            .is_some_and(|id| env.sys.objects.strong_count(id) == Some(1));
        if block.clipboard || !sole || (heap != 0 && heap != env.ext.get::<Heaps>().process_heap) {
            set_last_error(env, if block.clipboard { 5 } else { 87 });
            return false;
        }
        let freed =
            env.ext
                .get_mut::<Heaps>()
                .free(env.cpu, &mut env.sys.vm, heap, block.allocation);
        if freed {
            let globals = env.ext.get_mut::<adv_os::global::Globals>();
            if block.movable {
                globals.set_allocation(&mut env.sys.objects, block.handle, 0, 0, 0);
                globals
                    .get_mut(&mut env.sys.objects, block.handle)
                    .unwrap()
                    .locks = 0;
            } else {
                globals.remove(&mut env.sys.objects, block.handle);
            }
        }
        return freed;
    }
    if global_block(env, p).is_some() {
        // A public data view need not be the raw allocation accepted by RTL.
        set_last_error(env, 87);
        return false;
    }
    let heaps = env.ext.get_mut::<Heaps>();
    let heap = if heap == 0 {
        heaps.owner(p).unwrap_or(0)
    } else {
        heap
    };
    let ok = heaps.free(env.cpu, &mut env.sys.vm, heap, p);
    if !ok {
        log::warn!(
            "free of unknown block {p:#x} (heap {heap:#x}) from {:#010x}",
            env.caller()
        );
    }
    ok
}
fn global_block(env: &Env<'_>, pointer: u32) -> Option<adv_os::global::Block> {
    let globals = env.ext.try_get::<adv_os::global::Globals>()?;
    globals
        .get(
            &env.sys.objects,
            globals.from_pointer(&env.sys.objects, pointer)?,
        )
        .cloned()
}

fn global_allocation_block(env: &Env<'_>, allocation: u32) -> Option<adv_os::global::Block> {
    let globals = env.ext.try_get::<adv_os::global::Globals>()?;
    globals
        .get(
            &env.sys.objects,
            globals.from_allocation(&env.sys.objects, allocation)?,
        )
        .cloned()
}

fn heap_alloc(env: &mut Env<'_>) -> Outcome {
    let (h, flags, size) = (env.arg(0), env.arg(1), env.arg(2));
    crate::native::route(env, "RtlAllocateHeap", [h, flags, size], move |env| {
        crate::ntdll::heap_allocate(env, h, flags, size)
    })
}

fn heap_free(env: &mut Env<'_>) -> Outcome {
    let (h, flags, p) = (env.arg(0), env.arg(1), env.arg(2));
    if p == 0 {
        return Outcome::Return(1);
    }
    let owner = if h == 0 {
        env.ext.get::<Heaps>().owner(p).unwrap_or(0)
    } else {
        h
    };
    crate::native::route(env, "RtlFreeHeap", [h, flags, p], move |env| {
        crate::heap_lock::with_heap(env, owner, flags, move |env| {
            Outcome::Return(u32::from(free(env, h, p)))
        })
    })
}

fn heap_realloc(env: &mut Env<'_>) -> Outcome {
    let (h, flags, p, size) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    crate::native::route(env, "RtlReAllocateHeap", [h, flags, p, size], move |env| {
        crate::ntdll::heap_reallocate(env, h, flags, p, size)
    })
}

pub(crate) fn reallocate(env: &mut Env<'_>, h: u32, flags: u32, p: u32, size: u32) -> Outcome {
    let block = global_allocation_block(env, p);
    if let Some(block) = &block {
        let sole = env
            .ext
            .get::<adv_os::global::Globals>()
            .identity(&env.sys.objects, block.handle)
            .is_some_and(|id| env.sys.objects.strong_count(id) == Some(1));
        if block.clipboard
            || !sole
            || (h != 0 && h != env.ext.get::<Heaps>().process_heap)
            || size < block.pointer - block.allocation
        {
            set_last_error(env, if block.clipboard { 5 } else { 87 });
            return Outcome::Return(0);
        }
    } else if global_block(env, p).is_some() {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let heaps = env.ext.get_mut::<Heaps>();
    let zero = flags & HEAP_ZERO_MEMORY != 0;
    let in_place = flags & HEAP_REALLOC_IN_PLACE_ONLY != 0;
    let r = heaps
        .realloc(env.cpu, &mut env.sys.vm, h, p, size, zero, in_place)
        .unwrap_or(0);
    if r == 0 {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
    } else if let Some(mut block) = block {
        let bias = block.pointer - block.allocation;
        let globals = env.ext.get_mut::<adv_os::global::Globals>();
        if block.movable || r == block.allocation {
            globals.set_allocation(&mut env.sys.objects, block.handle, r, r + bias, size - bias);
        } else {
            globals
                .remove(&mut env.sys.objects, block.handle)
                .expect("validated sole global owner");
            block.handle = r;
            block.allocation = r;
            block.pointer = r;
            block.size = size;
            if globals.insert(&mut env.sys.objects, block).is_none() {
                return Outcome::Fail(
                    "heap reallocation could not commit its Global/Local owner".into(),
                );
            }
        }
    }
    Outcome::Return(r)
}

fn heap_size(env: &mut Env<'_>) -> Outcome {
    let (h, flags, p) = (env.arg(0), env.arg(1), env.arg(2));
    crate::native::route(env, "RtlSizeHeap", [h, flags, p], move |env| {
        crate::heap_lock::with_heap(env, h, flags, move |env| {
            Outcome::Return(env.ext.get_mut::<Heaps>().size(h, p).unwrap_or(u32::MAX))
        })
    })
}

pub(super) fn register(reg: &mut Registry) {
    dep::register(reg);
    global::register(reg);
    locks::register(reg);
    crate::heap_lock::register(reg);
    stdcall(reg, K32, "HeapCreate", 3, heap_create);
    stdcall(reg, K32, "HeapDestroy", 1, heap_destroy);
    stdcall(reg, K32, "HeapAlloc", 3, heap_alloc);
    stdcall(reg, K32, "HeapFree", 3, heap_free);
    stdcall(reg, K32, "HeapReAlloc", 4, heap_realloc);
    stdcall(reg, K32, "HeapSize", 3, heap_size);
    stdcall(reg, K32, "HeapSetInformation", 4, |_| Outcome::Return(1));
    stdcall(reg, K32, "HeapValidate", 3, |_| Outcome::Return(1));
    stdcall(reg, K32, "HeapCompact", 2, |_| Outcome::Return(0x10_0000));
    stdcall(reg, K32, "GetProcessHeap", 0, |e| {
        Outcome::Return(e.ext.get::<Heaps>().process_heap)
    });
    virtual_memory::register(reg);
    stdcall(reg, K32, "IsBadReadPtr", 2, |_| Outcome::Return(0));
    stdcall(reg, K32, "IsBadWritePtr", 2, |_| Outcome::Return(0));
    stdcall(reg, K32, "IsBadCodePtr", 1, |_| Outcome::Return(0));
}
