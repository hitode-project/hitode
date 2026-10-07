//! Heap, Local/Global, and Virtual memory functions.

use adv_abi::Registry;
use adv_machine::{Env, Outcome};

use super::K32;
use crate::stdcall;
use crate::util::{err, set_last_error};
use adv_os::Heaps;
mod global;
mod locks;
mod virtual_memory;
pub(crate) use virtual_memory::protect_memory;

const HEAP_ZERO_MEMORY: u32 = 0x8;
const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 0x10;

/// Highest application address in the existing flat x86 Win32 process profile.
/// GetSystemInfo and VirtualQuery must describe the same address domain.
pub(super) const MAX_APPLICATION_ADDRESS: u32 = 0x7ffe_ffff;

fn heap_create(env: &mut Env<'_>) -> Outcome {
    let heaps = env.ext.get_mut::<Heaps>();
    match heaps.create(env.cpu, &mut env.sys.vm) {
        Ok(h) => Outcome::Return(h),
        Err(_) => Outcome::Return(0),
    }
}

fn heap_destroy(env: &mut Env<'_>) -> Outcome {
    let h = env.arg(0);
    let heaps = env.ext.get_mut::<Heaps>();
    Outcome::Return(u32::from(heaps.destroy(env.cpu, &mut env.sys.vm, h)))
}

fn alloc(env: &mut Env<'_>, heap: u32, size: u32, zero: bool) -> u32 {
    let heaps = env.ext.get_mut::<Heaps>();
    let p = heaps
        .alloc(env.cpu, &mut env.sys.vm, heap, size, zero)
        .unwrap_or(0);
    if p == 0 {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
    }
    p
}

fn free(env: &mut Env<'_>, heap: u32, p: u32) -> bool {
    if p == 0 {
        return true;
    }
    if let Some(block) = global_block(env, p) {
        if block.movable || (heap != 0 && heap != env.ext.get::<Heaps>().process_heap) {
            set_last_error(env, if block.clipboard { 5 } else { 87 });
            return false;
        }
        return global::release(env, block.handle, false) == 0;
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

fn heap_alloc(env: &mut Env<'_>) -> Outcome {
    let (h, flags, size) = (env.arg(0), env.arg(1), env.arg(2));
    Outcome::Return(alloc(env, h, size, flags & HEAP_ZERO_MEMORY != 0))
}

fn heap_free(env: &mut Env<'_>) -> Outcome {
    let (h, p) = (env.arg(0), env.arg(2));
    Outcome::Return(u32::from(free(env, h, p)))
}

fn heap_realloc(env: &mut Env<'_>) -> Outcome {
    let (h, flags, p, size) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if let Some(block) = global_block(env, p) {
        if block.movable || (h != 0 && h != env.ext.get::<Heaps>().process_heap) {
            set_last_error(env, if block.clipboard { 5 } else { 87 });
            return Outcome::Return(0);
        }
        return Outcome::Return(global::resize(
            env,
            block.handle,
            size,
            if flags & HEAP_REALLOC_IN_PLACE_ONLY != 0 {
                0
            } else {
                2
            } | if flags & HEAP_ZERO_MEMORY != 0 {
                0x40
            } else {
                0
            },
            false,
        ));
    }
    let heaps = env.ext.get_mut::<Heaps>();
    let zero = flags & HEAP_ZERO_MEMORY != 0;
    let in_place = flags & HEAP_REALLOC_IN_PLACE_ONLY != 0;
    let r = heaps
        .realloc(env.cpu, &mut env.sys.vm, h, p, size, zero, in_place)
        .unwrap_or(0);
    if r == 0 {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
    }
    Outcome::Return(r)
}

fn heap_size(env: &mut Env<'_>) -> Outcome {
    let (h, p) = (env.arg(0), env.arg(2));
    Outcome::Return(env.ext.get_mut::<Heaps>().size(h, p).unwrap_or(u32::MAX))
}

pub(super) fn register(reg: &mut Registry) {
    global::register(reg);
    locks::register(reg);
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
