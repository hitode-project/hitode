//! CRT adapters for the shared RTL heap boundary.
use adv_machine::{Env, Outcome};
use adv_os::Heaps;

type Done = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;

pub(crate) fn allocate_then(env: &mut Env<'_>, size: u32, zero: bool, then: Done) -> Outcome {
    let heap = env.ext.get::<Heaps>().process_heap;
    let flags = if zero { 8 } else { 0 };
    match crate::native::overridden(env, "RtlAllocateHeap") {
        Ok(Some(target)) => Outcome::call(target, [heap, flags, size], move |env, value| {
            then(env, value.value())
        }),
        Ok(None) => crate::heap_lock::with_heap(env, heap, flags, move |env| {
            let pointer = env
                .ext
                .get_mut::<Heaps>()
                .alloc(env.cpu, &mut env.sys.vm, heap, size, zero)
                .unwrap_or(0);
            then(env, pointer)
        }),
        Err(_) => crate::native::unavailable(env, "RtlAllocateHeap"),
    }
}

pub(crate) fn allocate(env: &mut Env<'_>, size: u32, zero: bool) -> Outcome {
    allocate_then(env, size, zero, Box::new(|_, value| Outcome::Return(value)))
}

pub(crate) fn free_then(env: &mut Env<'_>, pointer: u32, then: Done) -> Outcome {
    if pointer == 0 {
        return then(env, 1);
    }
    let heap = env.ext.get::<Heaps>().process_heap;
    match crate::native::overridden(env, "RtlFreeHeap") {
        Ok(Some(target)) => Outcome::call(target, [heap, 0, pointer], move |env, value| {
            then(env, value.value())
        }),
        Ok(None) => crate::heap_lock::with_heap(env, heap, 0, move |env| {
            let previous = crate::util::last_error(env);
            let result = crate::kernel32::memory::free(env, heap, pointer);
            crate::util::set_last_error(env, previous);
            then(env, u32::from(result))
        }),
        Err(_) => crate::native::unavailable(env, "RtlFreeHeap"),
    }
}

pub(crate) fn free(env: &mut Env<'_>, pointer: u32) -> Outcome {
    free_then(env, pointer, Box::new(|_, _| Outcome::Return(0)))
}

pub(crate) fn realloc_then(env: &mut Env<'_>, pointer: u32, size: u32, then: Done) -> Outcome {
    if pointer == 0 {
        return allocate_then(env, size, false, then);
    }
    if size == 0 {
        return free_then(env, pointer, Box::new(move |env, _| then(env, 0)));
    }
    let heap = env.ext.get::<Heaps>().process_heap;
    match crate::native::overridden(env, "RtlReAllocateHeap") {
        Ok(Some(target)) => Outcome::call(target, [heap, 0, pointer, size], move |env, value| {
            then(env, value.value())
        }),
        Ok(None) => crate::heap_lock::with_heap(env, heap, 0, move |env| {
            let previous = crate::util::last_error(env);
            match crate::kernel32::memory::reallocate(env, heap, 0, pointer, size) {
                Outcome::Return(pointer) => {
                    crate::util::set_last_error(env, previous);
                    then(env, pointer)
                }
                failure => failure,
            }
        }),
        Err(_) => crate::native::unavailable(env, "RtlReAllocateHeap"),
    }
}

pub(crate) fn realloc(env: &mut Env<'_>, pointer: u32, size: u32) -> Outcome {
    realloc_then(
        env,
        pointer,
        size,
        Box::new(|_, value| Outcome::Return(value)),
    )
}
