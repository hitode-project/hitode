//! Generated Global/Local APIs share one handle model and the existing Heaps.
use crate::util::set_last_error;
use adv_abi::{Env, GuestPtr, Handle, Registry, win32};
use adv_machine::Outcome;
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
fn with_handle_heap(
    env: &mut Env<'_>,
    handle: u32,
    body: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    env.ext.get_or_default::<Globals>();
    let globals = env.ext.get::<Globals>();
    let heaps = env.ext.get::<Heaps>();
    let heap = if globals.get(&env.sys.objects, handle).is_some()
        || globals.from_pointer(&env.sys.objects, handle).is_some()
    {
        Some(heaps.process_heap)
    } else {
        heaps.owner(handle)
    };
    match heap {
        Some(heap) => crate::heap_lock::with_heap(env, heap, 0, body),
        None => body(env),
    }
}
fn allocate_front(env: &mut Env<'_>, flags: u32, size: u32) -> Outcome {
    env.ext.get_or_default::<Globals>();
    if flags & !0x7f72 != 0
        || !env.sys.objects.can_insert()
        || rounded(size).is_none()
        || (flags & 2 != 0 && size == 0)
    {
        return Outcome::Return(allocate(env, flags, size));
    }
    match crate::native::overridden(env, "RtlAllocateHeap") {
        Ok(Some(target)) => {
            let heap = env.ext.get::<Heaps>().process_heap;
            let size = rounded(size).unwrap();
            return Outcome::call(
                target,
                [heap, if flags & 0x40 != 0 { 8 } else { 0 }, size],
                move |env, result| {
                    let pointer = result.value();
                    if pointer == 0 {
                        return Outcome::Return(error(env, 8, 0));
                    }
                    let block = Block {
                        handle: 0,
                        allocation: pointer,
                        pointer,
                        size,
                        movable: flags & 2 != 0,
                        locks: 0,
                        attributes: 0,
                        clipboard: false,
                    };
                    if let Some(handle) = env
                        .ext
                        .get_mut::<Globals>()
                        .insert(&mut env.sys.objects, block)
                    {
                        return Outcome::Return(handle);
                    }
                    let target = match crate::native::entry(env, "RtlFreeHeap") {
                        Ok(target) => target,
                        Err(_) => return crate::native::unavailable(env, "RtlFreeHeap"),
                    };
                    Outcome::call(target, [heap, 0, pointer], |env, _| {
                        Outcome::Return(error(env, 8, 0))
                    })
                },
            );
        }
        Err(_) => return crate::native::unavailable(env, "RtlAllocateHeap"),
        Ok(None) => {}
    }
    crate::heap_lock::with_process_heap(env, move |env| Outcome::Return(allocate(env, flags, size)))
}
#[win32(kernel32,name="GlobalAlloc",level=implement)]
fn global_alloc(env: &mut Env<'_>, flags: u32, size: u32) -> Outcome {
    allocate_front(env, flags, size)
}
#[win32(kernel32,name="LocalAlloc",level=implement)]
fn local_alloc(env: &mut Env<'_>, flags: u32, size: u32) -> Outcome {
    allocate_front(env, flags, size)
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
        allocation: pointer,
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
fn global_free(env: &mut Env<'_>, handle: Handle) -> Outcome {
    env.ext.get_or_default::<Globals>();
    if handle.0 != 0 {
        match crate::native::overridden(env, "RtlFreeHeap") {
            Ok(Some(target)) => {
                let block = env
                    .ext
                    .get::<Globals>()
                    .get(&env.sys.objects, handle.0)
                    .cloned();
                let identity = env
                    .ext
                    .get::<Globals>()
                    .identity(&env.sys.objects, handle.0);
                if let Some(block) = &block {
                    if block.clipboard {
                        return Outcome::Return(error(env, 5, handle.0));
                    }
                    if block.pointer == 0 {
                        return Outcome::Return(release(env, handle.0, false));
                    }
                }
                let pointer = block.as_ref().map_or(handle.0, |block| block.allocation);
                let heap = env
                    .ext
                    .get::<Heaps>()
                    .owner(pointer)
                    .unwrap_or(env.ext.get::<Heaps>().process_heap);
                return Outcome::call(target, [heap, 0, pointer], move |env, result| {
                    if result.value() == 0 {
                        return Outcome::Return(error(env, 6, handle.0));
                    }
                    if block.is_some()
                        && env
                            .ext
                            .get::<Globals>()
                            .identity(&env.sys.objects, handle.0)
                            == identity
                    {
                        env.ext
                            .get_mut::<Globals>()
                            .remove(&mut env.sys.objects, handle.0);
                    }
                    Outcome::Return(0)
                });
            }
            Err(_) => return crate::native::unavailable(env, "RtlFreeHeap"),
            Ok(None) => {}
        }
    }
    with_handle_heap(env, handle.0, move |env| {
        Outcome::Return(release(env, handle.0, false))
    })
}
#[win32(kernel32,name="LocalFree",level=implement)]
fn local_free(env: &mut Env<'_>, handle: Handle) -> Outcome {
    global_free(env, handle)
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
fn global_lock(env: &mut Env<'_>, handle: Handle) -> Outcome {
    with_handle_heap(env, handle.0, move |env| {
        Outcome::Return(lock(env, handle.0))
    })
}
#[win32(kernel32,name="LocalLock",level=implement)]
fn local_lock(env: &mut Env<'_>, handle: Handle) -> Outcome {
    global_lock(env, handle)
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
fn global_unlock(env: &mut Env<'_>, handle: Handle) -> Outcome {
    with_handle_heap(env, handle.0, move |env| {
        Outcome::Return(unlock(env, handle.0, false))
    })
}
#[win32(kernel32,name="LocalUnlock",level=state)]
fn local_unlock(env: &mut Env<'_>, handle: Handle) -> Outcome {
    with_handle_heap(env, handle.0, move |env| {
        Outcome::Return(unlock(env, handle.0, true))
    })
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
fn global_size(env: &mut Env<'_>, handle: Handle) -> Outcome {
    size_front(env, handle.0)
}
#[win32(kernel32,name="LocalSize",level=implement)]
fn local_size(env: &mut Env<'_>, handle: Handle) -> Outcome {
    global_size(env, handle)
}
fn size(env: &mut Env<'_>, handle: u32) -> u32 {
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    if let Some(block) = globals.get(&env.sys.objects, handle).cloned() {
        if block.allocation == 0 {
            return 0;
        }
        let result = heaps
            .owner(block.allocation)
            .and_then(|heap| heaps.size(heap, block.allocation))
            .and_then(|size| size.checked_sub(block.pointer.saturating_sub(block.allocation)));
        result.unwrap_or_else(|| error(env, 6, 0))
    } else if let Some(heap) = heaps.owner(handle) {
        heaps.size(heap, handle).unwrap_or(0)
    } else {
        error(env, 6, 0)
    }
}
fn size_front(env: &mut Env<'_>, handle: u32) -> Outcome {
    env.ext.get_or_default::<Globals>();
    let block = env
        .ext
        .get::<Globals>()
        .get(&env.sys.objects, handle)
        .cloned();
    let allocation = block.as_ref().map_or(handle, |block| block.allocation);
    if allocation == 0 {
        return Outcome::Return(size(env, handle));
    }
    let heap = env
        .ext
        .get::<Heaps>()
        .owner(allocation)
        .or_else(|| block.as_ref().map(|_| env.ext.get::<Heaps>().process_heap));
    if let Some(heap) = heap {
        match crate::native::overridden(env, "RtlSizeHeap") {
            Ok(Some(target)) => {
                let bias = block
                    .as_ref()
                    .map_or(0, |block| block.pointer - block.allocation);
                return Outcome::call(target, [heap, 0, allocation], move |env, value| {
                    let size = value.value();
                    Outcome::Return(if size == u32::MAX || size < bias {
                        error(env, 6, 0)
                    } else {
                        size - bias
                    })
                });
            }
            Err(_) => return crate::native::unavailable(env, "RtlSizeHeap"),
            Ok(None) => {}
        }
    }
    with_handle_heap(env, handle, move |env| Outcome::Return(size(env, handle)))
}
#[win32(kernel32,name="GlobalHandle",level=implement)]
fn global_handle(env: &mut Env<'_>, pointer: GuestPtr<()>) -> Outcome {
    with_handle_heap(env, pointer.addr(), move |env| {
        Outcome::Return(handle(env, pointer.addr()))
    })
}
#[win32(kernel32,name="LocalHandle",level=implement)]
fn local_handle(env: &mut Env<'_>, pointer: GuestPtr<()>) -> Outcome {
    global_handle(env, pointer)
}
fn handle(env: &mut Env<'_>, pointer: u32) -> u32 {
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    globals
        .from_pointer(&env.sys.objects, pointer)
        .or_else(|| heaps.owner(pointer).map(|_| pointer))
        .unwrap_or_else(|| error(env, 6, 0))
}
#[win32(kernel32,name="GlobalFlags",level=state)]
fn global_flags(env: &mut Env<'_>, handle: Handle) -> Outcome {
    with_handle_heap(env, handle.0, move |env| {
        Outcome::Return(flags(env, handle.0, false))
    })
}
#[win32(kernel32,name="LocalFlags",level=state)]
fn local_flags(env: &mut Env<'_>, handle: Handle) -> Outcome {
    with_handle_heap(env, handle.0, move |env| {
        Outcome::Return(flags(env, handle.0, true))
    })
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
fn global_realloc(env: &mut Env<'_>, handle: Handle, size: u32, flags: u32) -> Outcome {
    resize_front(env, handle.0, size, flags, false)
}
#[win32(kernel32,name="LocalReAlloc",level=implement)]
fn local_realloc(env: &mut Env<'_>, handle: Handle, size: u32, flags: u32) -> Outcome {
    resize_front(env, handle.0, size, flags, true)
}
fn commit_native_resize(
    env: &mut Env<'_>,
    old: Block,
    identity: Option<adv_machine::Identity<Block>>,
    allocation: u32,
    size: u32,
) -> Outcome {
    if allocation == 0 {
        return Outcome::Return(error(env, 8, 0));
    }
    let bias = old.pointer.saturating_sub(old.allocation);
    let Some(pointer) = allocation.checked_add(bias) else {
        return Outcome::Fail("Global/Local native allocation view overflow".into());
    };
    let globals = env.ext.get_mut::<Globals>();
    if old.movable {
        if globals.identity(&env.sys.objects, old.handle) != identity {
            return Outcome::Fail("Global/Local owner retired during native reallocation".into());
        }
        globals.set_allocation(&mut env.sys.objects, old.handle, allocation, pointer, size);
        return Outcome::Return(old.handle);
    }
    if globals
        .get(&env.sys.objects, pointer)
        .is_some_and(|block| block.allocation == allocation)
    {
        return Outcome::Return(pointer);
    }
    if globals.identity(&env.sys.objects, old.handle) == identity {
        if old.handle == pointer {
            globals.set_allocation(&mut env.sys.objects, old.handle, allocation, pointer, size);
            return Outcome::Return(old.handle);
        }
        if globals.remove(&mut env.sys.objects, old.handle).is_none() {
            return Outcome::Fail("Global/Local reallocation owner is borrowed".into());
        }
    } else {
        return Outcome::Fail("Global/Local owner retired during native reallocation".into());
    }
    let block = Block {
        handle: pointer,
        allocation,
        pointer,
        size,
        ..old
    };
    match globals.insert(&mut env.sys.objects, block) {
        Some(handle) => Outcome::Return(handle),
        None => Outcome::Fail("Global/Local native reallocation could not commit its owner".into()),
    }
}
fn resize_front(
    env: &mut Env<'_>,
    handle: u32,
    requested: u32,
    flags: u32,
    local: bool,
) -> Outcome {
    env.ext.get_or_default::<Globals>();
    if flags & !0x7ff2 != 0 {
        return Outcome::Return(resize(env, handle, requested, flags, local));
    }
    if flags & 0x80 != 0 {
        return with_handle_heap(env, handle, move |env| {
            Outcome::Return(resize(env, handle, requested, flags, local))
        });
    }
    let old = env
        .ext
        .get::<Globals>()
        .get(&env.sys.objects, handle)
        .cloned();
    if let Some(old) = old {
        let identity = env.ext.get::<Globals>().identity(&env.sys.objects, handle);
        if old.clipboard || old.movable && requested == 0 && old.locks != 0 {
            return Outcome::Return(resize(env, handle, requested, flags, local));
        }
        let heap = env
            .ext
            .get::<Heaps>()
            .owner(old.allocation)
            .unwrap_or(env.ext.get::<Heaps>().process_heap);
        if old.movable && requested == 0 {
            if old.allocation == 0 {
                return Outcome::Return(handle);
            }
            match crate::native::overridden(env, "RtlFreeHeap") {
                Ok(Some(target)) => {
                    return Outcome::call(target, [heap, 0, old.allocation], move |env, result| {
                        if result.value() == 0 {
                            return Outcome::Return(error(env, 6, 0));
                        }
                        let globals = env.ext.get_mut::<Globals>();
                        if globals.identity(&env.sys.objects, handle) != identity {
                            return Outcome::Fail(
                                "Global/Local owner retired during native discard".into(),
                            );
                        }
                        globals.set_allocation(&mut env.sys.objects, handle, 0, 0, 0);
                        Outcome::Return(handle)
                    });
                }
                Err(_) => return crate::native::unavailable(env, "RtlFreeHeap"),
                Ok(None) => {}
            }
        } else if let Some(size) = rounded(requested) {
            let bias = old.pointer.saturating_sub(old.allocation);
            let Some(raw_size) = size.checked_add(bias) else {
                return Outcome::Return(error(env, 8, 0));
            };
            let name = if old.allocation == 0 {
                "RtlAllocateHeap"
            } else {
                "RtlReAllocateHeap"
            };
            match crate::native::overridden(env, name) {
                Ok(Some(target)) => {
                    let native_flags = if flags & 0x40 != 0 { 8 } else { 0 }
                        | if old.allocation != 0
                            && (old.movable && old.locks != 0 && flags & 2 == 0
                                || !old.movable && flags & 2 == 0)
                        {
                            0x10
                        } else {
                            0
                        };
                    let args = if old.allocation == 0 {
                        vec![heap, native_flags, raw_size]
                    } else {
                        vec![heap, native_flags, old.allocation, raw_size]
                    };
                    return Outcome::call(target, args, move |env, value| {
                        commit_native_resize(env, old, identity, value.value(), size)
                    });
                }
                Err(_) => return crate::native::unavailable(env, name),
                Ok(None) => {}
            }
        }
    } else if let Some(heap) = env.ext.get::<Heaps>().owner(handle) {
        match crate::native::overridden(env, "RtlReAllocateHeap") {
            Ok(Some(target)) => {
                return Outcome::call(
                    target,
                    [
                        heap,
                        if flags & 0x40 != 0 { 8 } else { 0 }
                            | if flags & 2 == 0 { 0x10 } else { 0 },
                        handle,
                        requested,
                    ],
                    |env, value| {
                        Outcome::Return(if value.value() == 0 {
                            error(env, 8, 0)
                        } else {
                            value.value()
                        })
                    },
                );
            }
            Err(_) => return crate::native::unavailable(env, "RtlReAllocateHeap"),
            Ok(None) => {}
        }
    }
    with_handle_heap(env, handle, move |env| {
        Outcome::Return(resize(env, handle, requested, flags, local))
    })
}
pub(crate) fn resize(env: &mut Env<'_>, handle: u32, size: u32, flags: u32, local: bool) -> u32 {
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
            heaps.free(env.cpu, &mut env.sys.vm, 0, old.allocation);
        }
        globals.set_storage(&mut env.sys.objects, handle, 0, 0);
        return handle;
    }
    let Some(size) = rounded(size) else {
        return error(env, 8, 0);
    };
    if old.movable {
        let bias = old.pointer.saturating_sub(old.allocation);
        let Some(raw_size) = size.checked_add(bias) else {
            return error(env, 8, 0);
        };
        let allocation = if old.allocation == 0 {
            heaps.alloc(env.cpu, &mut env.sys.vm, 0, raw_size, flags & 0x40 != 0)
        } else {
            heaps.realloc(
                env.cpu,
                &mut env.sys.vm,
                0,
                old.allocation,
                raw_size,
                flags & 0x40 != 0,
                old.locks != 0 && flags & 2 == 0,
            )
        };
        let Some(allocation) = allocation else {
            return error(env, 8, 0);
        };
        globals.set_allocation(
            &mut env.sys.objects,
            handle,
            allocation,
            allocation + bias,
            size,
        );
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
        allocation: pointer,
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
    use adv_cpu::{
        JitCpu,
        backend::{Guest, GuestExt},
    };
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

    #[test]
    fn raw_rtl_operations_use_the_allocation_owner_and_preserve_clipboard_ownership() {
        let mut m = fixture();
        let allocation = m
            .ext
            .get_mut::<Heaps>()
            .alloc(&mut m.cpu, &mut m.sys.vm, 0, 40, false)
            .unwrap();
        let heap = m.ext.get::<Heaps>().process_heap;
        let pointer = allocation + 8; // Authored distinct data view; no native layout claim.
        let handle = m
            .ext
            .get_or_default::<Globals>()
            .insert(
                &mut m.sys.objects,
                Block {
                    handle: 0,
                    allocation,
                    pointer,
                    size: 32,
                    movable: true,
                    locks: 0,
                    attributes: 0,
                    clipboard: false,
                },
            )
            .unwrap();
        assert!(!super::super::free(&mut env(&mut m), heap, pointer));
        assert_eq!(
            m.ext
                .get::<Globals>()
                .from_allocation(&m.sys.objects, allocation),
            Some(handle)
        );
        assert!(m.ext.get_mut::<Heaps>().size(heap, allocation).is_some());
        m.ext
            .get_mut::<Globals>()
            .get_mut(&mut m.sys.objects, handle)
            .unwrap()
            .clipboard = true;
        assert!(!super::super::free(&mut env(&mut m), heap, allocation));
        assert!(m.ext.get_mut::<Heaps>().size(heap, allocation).is_some());
        m.ext
            .get_mut::<Globals>()
            .get_mut(&mut m.sys.objects, handle)
            .unwrap()
            .clipboard = false;
        assert!(super::super::free(&mut env(&mut m), heap, allocation));
        assert_eq!(
            m.ext
                .get::<Globals>()
                .from_allocation(&m.sys.objects, allocation),
            None
        );
        assert_eq!(
            m.ext.get::<Globals>().from_pointer(&m.sys.objects, pointer),
            None
        );
        assert_eq!(
            m.ext
                .get::<Globals>()
                .get(&m.sys.objects, handle)
                .unwrap()
                .allocation,
            0
        );
        assert_eq!(release(&mut env(&mut m), handle, false), 0);
    }

    fn contended(
        m: &mut Machine<JitCpu>,
        heap: u32,
        operation: impl FnOnce(&mut Env<'_>) -> Outcome,
        unchanged: impl FnOnce(&mut Machine<JitCpu>),
    ) -> u32 {
        m.sys.current = 0;
        assert!(matches!(
            crate::heap_lock::lock(&mut env(m), heap),
            Outcome::Return(1)
        ));
        m.sys.current = 1;
        let Outcome::Block {
            wait: adv_machine::Wait::Object { handle, .. },
            then,
            ..
        } = operation(&mut env(m))
        else {
            panic!("Global/Local frontend must wait for its actual heap owner")
        };
        unchanged(m);
        m.sys.current = 0;
        assert!(matches!(
            crate::heap_lock::unlock(&mut env(m), heap),
            Outcome::Return(1)
        ));
        m.sys.current = 1;
        assert!(m.sys.try_acquire(handle));
        let Outcome::Return(value) = then(
            &mut env(m),
            adv_machine::Resume::Woke(adv_machine::WakeReason::Signaled),
        ) else {
            panic!("Global/Local frontend must finish after heap handoff")
        };
        value
    }

    #[test]
    fn global_local_fronts_wait_for_process_and_private_heap_owners() {
        let mut m = fixture();
        m.spawn("globals waiter", adv_machine::layout::PARK_ADDR, &[], 65536)
            .unwrap();
        let heap = m.ext.get::<Heaps>().process_heap;
        for local in [false, true] {
            let handle = contended(
                &mut m,
                heap,
                |env| {
                    if local {
                        local_alloc(env, 0x42, 32)
                    } else {
                        global_alloc(env, 0x42, 32)
                    }
                },
                |_| {},
            );
            let pointer = lock(&mut env(&mut m), handle);
            assert_ne!(pointer, 0);
            m.cpu.write_u32(pointer, 0x13572468).unwrap();
            unlock(&mut env(&mut m), handle, local);
            assert_eq!(
                contended(
                    &mut m,
                    heap,
                    |env| {
                        if local {
                            local_realloc(env, Handle(handle), 64, 0x42)
                        } else {
                            global_realloc(env, Handle(handle), 64, 0x42)
                        }
                    },
                    |m| {
                        assert_eq!(size(&mut env(m), handle), 32);
                        assert_eq!(m.cpu.read_u32(pointer).unwrap(), 0x13572468);
                    }
                ),
                handle
            );
            let pointer = lock(&mut env(&mut m), handle);
            unlock(&mut env(&mut m), handle, local);
            assert_eq!(m.cpu.read_u32(pointer).unwrap(), 0x13572468);
            for operation in [0, 1, 2, 3, 4] {
                let value = contended(
                    &mut m,
                    heap,
                    |env| match (local, operation) {
                        (false, 0) => global_size(env, Handle(handle)),
                        (true, 0) => local_size(env, Handle(handle)),
                        (false, 1) => global_handle(env, GuestPtr::new(pointer)),
                        (true, 1) => local_handle(env, GuestPtr::new(pointer)),
                        (false, 2) => global_flags(env, Handle(handle)),
                        (true, 2) => local_flags(env, Handle(handle)),
                        (false, 3) => global_lock(env, Handle(handle)),
                        (true, 3) => local_lock(env, Handle(handle)),
                        (false, _) => global_unlock(env, Handle(handle)),
                        (true, _) => local_unlock(env, Handle(handle)),
                    },
                    |_| {},
                );
                assert_eq!(
                    value,
                    match operation {
                        0 => 64,
                        1 => handle,
                        3 => pointer,
                        _ => 0,
                    }
                );
            }
            assert_eq!(
                contended(
                    &mut m,
                    heap,
                    |env| {
                        if local {
                            local_free(env, Handle(handle))
                        } else {
                            global_free(env, Handle(handle))
                        }
                    },
                    |m| {
                        assert_eq!(m.ext.get::<Heaps>().owner(pointer), Some(heap));
                    }
                ),
                0
            );
            assert!(m.ext.get::<Heaps>().owner(pointer).is_none());
        }

        let private = m
            .ext
            .get_mut::<Heaps>()
            .create(&mut m.cpu, &mut m.sys.vm)
            .unwrap();
        let raw = m
            .ext
            .get_mut::<Heaps>()
            .alloc(&mut m.cpu, &mut m.sys.vm, private, 24, false)
            .unwrap();
        m.cpu.write_u32(raw, 0x24681357).unwrap();
        let resized = contended(
            &mut m,
            private,
            |env| local_realloc(env, Handle(raw), 48, 2),
            |m| {
                assert_eq!(m.ext.get_mut::<Heaps>().size(private, raw), Some(24));
                assert_eq!(m.cpu.read_u32(raw).unwrap(), 0x24681357);
            },
        );
        assert_eq!(m.ext.get::<Heaps>().owner(resized), Some(private));
        assert_eq!(m.cpu.read_u32(resized).unwrap(), 0x24681357);
        assert_eq!(
            contended(
                &mut m,
                private,
                |env| global_free(env, Handle(resized)),
                |m| {
                    assert_eq!(m.ext.get::<Heaps>().owner(resized), Some(private));
                }
            ),
            0
        );
        assert!(m.ext.get::<Heaps>().owner(resized).is_none());
    }
}
