//! Generated Global/Local APIs share one handle model and the existing Heaps.
use crate::util::set_last_error;
use adv_abi::{Env, GuestPtr, Handle, Registry, win32};
use adv_os::{
    Heaps,
    global::{Block, Globals},
};

pub(super) fn register(registry: &mut Registry) {
    for api in [
        global_alloc_ABI,
        global_free_ABI,
        global_lock_ABI,
        global_unlock_ABI,
        global_size_ABI,
        global_handle_ABI,
        global_flags_ABI,
        global_realloc_ABI,
        local_alloc_ABI,
        local_free_ABI,
        local_lock_ABI,
        local_unlock_ABI,
        local_size_ABI,
        local_handle_ABI,
        local_flags_ABI,
        local_realloc_ABI,
    ] {
        registry.register(api);
    }
}
fn error(env: &mut Env<'_>, code: u32, result: u32) -> u32 {
    set_last_error(env, code);
    result
}
fn rounded(size: u32) -> Option<u32> {
    size.max(1).checked_add(7).map(|n| n & !7)
}
#[win32(kernel32,name="GlobalAlloc",level=implement)]
fn global_alloc(env: &mut Env<'_>, flags: u32, size: u32) -> Handle {
    Handle(allocate(env, flags, size))
}
#[win32(kernel32,name="LocalAlloc",level=implement)]
fn local_alloc(env: &mut Env<'_>, flags: u32, size: u32) -> Handle {
    Handle(allocate(env, flags, size))
}
pub(crate) fn allocate(env: &mut Env<'_>, flags: u32, size: u32) -> u32 {
    if flags & !0x7f72 != 0 {
        return error(env, 87, 0);
    }
    if !env.sys.objects.can_insert() {
        return error(env, 8, 0);
    }
    let movable = flags & 2 != 0;
    let Some(size) = rounded(size)
        .filter(|_| !(movable && size == 0))
        .or((movable && size == 0).then_some(0))
    else {
        return error(env, 8, 0);
    };
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    let pointer = if size == 0 {
        0
    } else {
        match heaps.alloc(env.cpu, &mut env.sys.vm, 0, size, flags & 0x40 != 0) {
            Some(p) => p,
            None => return error(env, 8, 0),
        }
    };
    let block = Block {
        handle: 0,
        pointer,
        size,
        movable,
        locks: 0,
        attributes: 0,
        clipboard: false,
    };
    match globals.insert(&mut env.sys.objects, block) {
        Some(h) => h,
        None => {
            if pointer != 0 {
                heaps.free(env.cpu, &mut env.sys.vm, 0, pointer);
            }
            error(env, 8, 0)
        }
    }
}
#[win32(kernel32,name="GlobalFree",level=implement)]
fn global_free(env: &mut Env<'_>, handle: Handle) -> Handle {
    Handle(release(env, handle.0, false))
}
#[win32(kernel32,name="LocalFree",level=implement)]
fn local_free(env: &mut Env<'_>, handle: Handle) -> Handle {
    Handle(release(env, handle.0, false))
}
/// Used by EmptyClipboard to retire the same handle exactly once.
pub(crate) fn release(env: &mut Env<'_>, handle: u32, clipboard: bool) -> u32 {
    if handle == 0 {
        return 0;
    }
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    if globals.get(&env.sys.objects, handle).is_some() {
        match globals.free(
            &mut env.sys.objects,
            heaps,
            env.cpu,
            &mut env.sys.vm,
            handle,
            clipboard,
        ) {
            Ok(()) => 0,
            Err(adv_os::global::FreeError::Owned) => error(env, 5, handle),
            Err(adv_os::global::FreeError::Invalid) => error(env, 6, handle),
        }
    } else {
        // Preserve the pre-existing fixed HeapAlloc pointer interoperability.
        if let Some(actual) = globals.from_pointer(&env.sys.objects, handle) {
            let block = globals.get(&env.sys.objects, actual).unwrap();
            return error(env, if block.clipboard { 5 } else { 6 }, handle);
        }
        let heap = heaps.owner(handle);
        if !clipboard && heap.is_some_and(|heap| heaps.free(env.cpu, &mut env.sys.vm, heap, handle))
        {
            0
        } else {
            error(env, 6, handle)
        }
    }
}
#[win32(kernel32,name="GlobalLock",level=implement)]
fn global_lock(env: &mut Env<'_>, handle: Handle) -> GuestPtr<()> {
    GuestPtr::new(lock(env, handle.0))
}
#[win32(kernel32,name="LocalLock",level=implement)]
fn local_lock(env: &mut Env<'_>, handle: Handle) -> GuestPtr<()> {
    GuestPtr::new(lock(env, handle.0))
}
fn lock(env: &mut Env<'_>, handle: u32) -> u32 {
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    if let Some(block) = globals.get_mut(&mut env.sys.objects, handle) {
        if block.pointer == 0 {
            return error(env, 157, 0);
        }
        if block.movable {
            let Some(n) = block.locks.checked_add(1) else {
                return error(env, 212, 0);
            };
            block.locks = n;
        }
        block.pointer
    } else if heaps.owner(handle).is_some() {
        handle
    } else {
        error(env, 6, 0)
    }
}
#[win32(kernel32,name="GlobalUnlock",level=state)]
fn global_unlock(env: &mut Env<'_>, handle: Handle) -> u32 {
    unlock(env, handle.0, false)
}
#[win32(kernel32,name="LocalUnlock",level=state)]
fn local_unlock(env: &mut Env<'_>, handle: Handle) -> u32 {
    unlock(env, handle.0, true)
}
fn unlock(env: &mut Env<'_>, handle: u32, local: bool) -> u32 {
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    if let Some(block) = globals.get_mut(&mut env.sys.objects, handle) {
        if !block.movable {
            return if local { error(env, 158, 0) } else { 1 };
        }
        if block.locks == 0 {
            return error(env, 158, 0);
        }
        block.locks -= 1;
        if block.locks == 0 {
            error(env, 0, 0)
        } else {
            1
        }
    } else if heaps.owner(handle).is_some() {
        if local { error(env, 158, 0) } else { 1 }
    } else {
        error(env, 6, 0)
    }
}
#[win32(kernel32,name="GlobalSize",level=implement)]
fn global_size(env: &mut Env<'_>, handle: Handle) -> u32 {
    size(env, handle.0)
}
#[win32(kernel32,name="LocalSize",level=implement)]
fn local_size(env: &mut Env<'_>, handle: Handle) -> u32 {
    size(env, handle.0)
}
fn size(env: &mut Env<'_>, handle: u32) -> u32 {
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    if let Some(block) = globals.get(&env.sys.objects, handle) {
        block.size
    } else if let Some(heap) = heaps.owner(handle) {
        heaps.size(heap, handle).unwrap_or(0)
    } else {
        error(env, 6, 0)
    }
}
#[win32(kernel32,name="GlobalHandle",level=implement)]
fn global_handle(env: &mut Env<'_>, pointer: GuestPtr<()>) -> Handle {
    Handle(handle(env, pointer.addr()))
}
#[win32(kernel32,name="LocalHandle",level=implement)]
fn local_handle(env: &mut Env<'_>, pointer: GuestPtr<()>) -> Handle {
    Handle(handle(env, pointer.addr()))
}
fn handle(env: &mut Env<'_>, pointer: u32) -> u32 {
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    globals
        .from_pointer(&env.sys.objects, pointer)
        .or_else(|| heaps.owner(pointer).map(|_| pointer))
        .unwrap_or_else(|| error(env, 6, 0))
}
#[win32(kernel32,name="GlobalFlags",level=state)]
fn global_flags(env: &mut Env<'_>, handle: Handle) -> u32 {
    flags(env, handle.0, false)
}
#[win32(kernel32,name="LocalFlags",level=state)]
fn local_flags(env: &mut Env<'_>, handle: Handle) -> u32 {
    flags(env, handle.0, true)
}
fn flags(env: &mut Env<'_>, handle: u32, local: bool) -> u32 {
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    if let Some(block) = globals.get(&env.sys.objects, handle) {
        (if local { 0 } else { block.attributes })
            | u32::from(block.locks)
            | if block.pointer == 0 { 0x4000 } else { 0 }
    } else if heaps.owner(handle).is_some() {
        0
    } else {
        error(env, 6, 0x8000)
    }
}
#[win32(kernel32,name="GlobalReAlloc",level=implement)]
fn global_realloc(env: &mut Env<'_>, handle: Handle, size: u32, flags: u32) -> Handle {
    Handle(resize(env, handle.0, size, flags, false))
}
#[win32(kernel32,name="LocalReAlloc",level=implement)]
fn local_realloc(env: &mut Env<'_>, handle: Handle, size: u32, flags: u32) -> Handle {
    Handle(resize(env, handle.0, size, flags, true))
}
pub(super) fn resize(env: &mut Env<'_>, handle: u32, size: u32, flags: u32, local: bool) -> u32 {
    if flags & !0x7ff2 != 0 {
        return error(env, 87, 0);
    }
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    let Some(old) = globals.get(&env.sys.objects, handle).cloned() else {
        if let Some(actual) = globals.from_pointer(&env.sys.objects, handle) {
            let block = globals.get(&env.sys.objects, actual).unwrap();
            return error(env, if block.clipboard { 5 } else { 6 }, 0);
        }
        if let Some(heap) = heaps.owner(handle) {
            return heaps
                .realloc(
                    env.cpu,
                    &mut env.sys.vm,
                    heap,
                    handle,
                    size,
                    flags & 0x40 != 0,
                    flags & 2 == 0,
                )
                .unwrap_or_else(|| error(env, 8, 0));
        }
        return error(env, 6, 0);
    };
    if old.clipboard {
        return error(env, 5, 0);
    }
    if flags & 0x80 != 0 {
        if local || old.movable || flags & 2 == 0 {
            return handle;
        }
        // Conversion requires a new alias but keeps the existing allocation.
        let mut converted = old.clone();
        converted.movable = true;
        converted.locks = 0;
        converted.attributes = 0x2000;
        if let Some(new) = globals.insert(&mut env.sys.objects, converted) {
            globals.remove(&mut env.sys.objects, handle);
            return new;
        }
        return error(env, 8, 0);
    }
    if old.movable && size == 0 {
        if old.locks != 0 {
            return error(env, 212, 0);
        }
        if old.pointer != 0 {
            heaps.free(env.cpu, &mut env.sys.vm, 0, old.pointer);
        }
        globals.set_storage(&mut env.sys.objects, handle, 0, 0);
        return handle;
    }
    let Some(size) = rounded(size) else {
        return error(env, 8, 0);
    };
    if old.movable {
        let pointer = if old.pointer == 0 {
            heaps.alloc(env.cpu, &mut env.sys.vm, 0, size, flags & 0x40 != 0)
        } else {
            heaps.realloc(
                env.cpu,
                &mut env.sys.vm,
                0,
                old.pointer,
                size,
                flags & 0x40 != 0,
                old.locks != 0 && flags & 2 == 0,
            )
        };
        let Some(pointer) = pointer else {
            return error(env, 8, 0);
        };
        globals.set_storage(&mut env.sys.objects, handle, pointer, size);
        return handle;
    }
    if let Some(pointer) = heaps.realloc(
        env.cpu,
        &mut env.sys.vm,
        0,
        old.pointer,
        size,
        flags & 0x40 != 0,
        true,
    ) {
        debug_assert_eq!(pointer, handle);
        globals.set_storage(&mut env.sys.objects, handle, pointer, size);
        return handle;
    }
    if flags & 2 == 0 {
        return error(env, 8, 0);
    }
    // Fixed pointer moves are transactional: publish metadata before retiring
    // the old block; failure leaves its handle, bytes and allocation untouched.
    let Some(pointer) = heaps.alloc(env.cpu, &mut env.sys.vm, 0, size, flags & 0x40 != 0) else {
        return error(env, 8, 0);
    };
    let new = Block {
        handle: pointer,
        pointer,
        size,
        ..old.clone()
    };
    let Some(new) = globals.insert(&mut env.sys.objects, new) else {
        heaps.free(env.cpu, &mut env.sys.vm, 0, pointer);
        return error(env, 8, 0);
    };
    let copy = old.size.min(size);
    if adv_abi::guest::check_range(env.cpu, old.pointer, copy, adv_cpu::backend::Prot::READ)
        .is_err()
        || env
            .cpu
            .copy_memory(old.pointer, pointer, copy as usize)
            .is_err()
    {
        globals.remove(&mut env.sys.objects, new);
        heaps.free(env.cpu, &mut env.sys.vm, 0, pointer);
        return error(env, 87, 0);
    }
    heaps.free(env.cpu, &mut env.sys.vm, 0, old.pointer);
    globals.remove(&mut env.sys.objects, handle);
    new
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{JitCpu, backend::Guest};
    use adv_machine::{Clock, Machine, ObjectLimits, Objects};
    fn fixture() -> Machine<JitCpu> {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
        let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        crate::Win32::install(
            &mut m,
            crate::Win32Config::new(".", "globals-test.exe", "target/global-contract-tests"),
        )
        .unwrap();
        m.spawn("globals", adv_machine::layout::PARK_ADDR, &[], 65536)
            .unwrap();
        m
    }
    fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
        Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        }
    }
    #[test]
    fn namespace_limits_and_clipboard_ownership_preserve_bytes_and_the_live_alias() {
        let mut m = fixture();
        let handle = allocate(&mut env(&mut m), 0x42, 13);
        assert_ne!(handle, 0);
        let pointer = lock(&mut env(&mut m), handle);
        m.cpu.write(pointer, b"owned bytes").unwrap();
        let globals = m.ext.get::<Globals>();
        let id = globals.identity(&m.sys.objects, handle).unwrap();
        assert_eq!(m.sys.objects.strong_count(id), Some(1));
        assert!(m.sys.objects.get(handle).is_none());
        m.ext
            .get::<Globals>()
            .get_mut(&mut m.sys.objects, handle)
            .unwrap()
            .clipboard = true;
        assert_eq!(release(&mut env(&mut m), handle, false), handle);
        assert_eq!(
            release(&mut env(&mut m), pointer, false),
            pointer,
            "a locked data pointer must not bypass clipboard ownership"
        );
        assert_eq!(resize(&mut env(&mut m), handle, 64, 0x42, false), 0);
        let mut bytes = [0; 11];
        m.cpu.read(pointer, &mut bytes).unwrap();
        assert_eq!(&bytes, b"owned bytes");
        assert_eq!(m.sys.objects.strong_count(id), Some(1));
        assert_eq!(release(&mut env(&mut m), handle, true), 0);
        assert!(m.sys.objects.owned(id).is_none());
        assert_eq!(lock(&mut env(&mut m), handle), 0);
        let free = m
            .ext
            .get_mut::<Heaps>()
            .alloc(&mut m.cpu, &mut m.sys.vm, 0, 16, false)
            .unwrap();
        m.ext
            .get_mut::<Heaps>()
            .free(&mut m.cpu, &mut m.sys.vm, 0, free);
        m.ext.insert(Globals::default());
        let saved = std::mem::replace(
            &mut m.sys.objects,
            Objects::with_limits(ObjectLimits {
                objects: 0,
                ..Default::default()
            }),
        );
        for _ in 0..8 {
            assert_eq!(allocate(&mut env(&mut m), 0x42, 16), 0);
        }
        let after = m
            .ext
            .get_mut::<Heaps>()
            .alloc(&mut m.cpu, &mut m.sys.vm, 0, 16, false)
            .unwrap();
        assert_eq!(
            after, free,
            "metadata exhaustion cannot abandon a heap allocation"
        );
        m.ext
            .get_mut::<Heaps>()
            .free(&mut m.cpu, &mut m.sys.vm, 0, after);
        m.sys.objects = saved;
    }
    #[test]
    fn failed_realloc_and_reference_lock_overflow_never_change_the_original() {
        let mut m = fixture();
        let handle = allocate(&mut env(&mut m), 0x42, 32);
        let p = lock(&mut env(&mut m), handle);
        m.cpu.write(p, &[0x7a; 32]).unwrap();
        assert_eq!(resize(&mut env(&mut m), handle, u32::MAX, 0x42, false), 0);
        assert_eq!(
            m.ext.get::<Globals>().from_pointer(&m.sys.objects, p),
            Some(handle)
        );
        assert_eq!(
            m.ext
                .get::<Globals>()
                .get(&m.sys.objects, handle)
                .unwrap()
                .pointer,
            p
        );
        let mut bytes = [0; 32];
        m.cpu.read(p, &mut bytes).unwrap();
        assert_eq!(bytes, [0x7a; 32]);
        m.ext
            .get::<Globals>()
            .get_mut(&mut m.sys.objects, handle)
            .unwrap()
            .locks = u8::MAX;
        assert_eq!(lock(&mut env(&mut m), handle), 0);
        assert_eq!(flags(&mut env(&mut m), handle, false), 255);
        assert_eq!(
            release(&mut env(&mut m), handle, false),
            0,
            "GlobalFree is allowed even while locked"
        );
        // Fixed legacy HeapAlloc interoperability has not acquired a second owner.
        let raw = m
            .ext
            .get_mut::<Heaps>()
            .alloc(&mut m.cpu, &mut m.sys.vm, 0, 24, false)
            .unwrap();
        assert_eq!(lock(&mut env(&mut m), raw), raw);
        assert_eq!(super::handle(&mut env(&mut m), raw), raw);
        assert_eq!(release(&mut env(&mut m), raw, false), 0);
    }
}
