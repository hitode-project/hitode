//! CRT allocation, sizing and in-place resizing share the existing RTL heap owner.
use super::*;
use adv_abi::{GuestPtr, GuestReturn, win32};
use adv_cpu::backend::MemFault;

const HEAP_MAXREQ: u32 = 0xffffffe0;
type Done = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;

pub(super) fn register(registry: &mut Registry) {
    registry.register(calloc_crt_ABI);
    versions::register(
        registry,
        &[
            ("malloc", malloc_ABI.handler),
            ("calloc", calloc_ABI.handler),
            ("realloc", realloc_ABI.handler),
            ("free", free_ABI.handler),
            ("_msize", block_size),
            ("_expand", expand),
        ],
    );
}

pub(super) fn result(env: &mut Env<'_>, address: u32) -> Result<GuestPtr<()>, MemFault> {
    if address == 0 {
        floating::errno_address(env).write(env.cpu, 12)?; // ENOMEM
    }
    Ok(GuestPtr::new(address))
}

#[win32(ucrtbase, name="malloc", level=implement)]
fn malloc(env: &mut Env<'_>, size: u32) -> Outcome {
    if size > HEAP_MAXREQ {
        return result(env, 0).into_outcome();
    }
    crate::native_heap::allocate_then(
        env,
        size.max(1),
        false,
        Box::new(|env, address| result(env, address).into_outcome()),
    )
}

#[win32(ucrtbase, name="calloc", level=implement)]
fn calloc(env: &mut Env<'_>, count: u32, size: u32) -> Outcome {
    let Some(size) = count.checked_mul(size).filter(|&size| size <= HEAP_MAXREQ) else {
        return result(env, 0).into_outcome();
    };
    crate::native_heap::allocate_then(
        env,
        size.max(1),
        true,
        Box::new(|env, address| result(env, address).into_outcome()),
    )
}

#[win32(msvcr90, name = "_calloc_crt", level = implement)]
fn calloc_crt(env: &mut Env<'_>, count: u32, size: u32) -> Outcome {
    // VC90 validates the product before allocation and calls its installed
    // invalid-parameter handler with ENOMEM for an oversized request.
    if count != 0 && size > HEAP_MAXREQ / count {
        return invalid_parameter_result(env, "_calloc_crt", 12, 0);
    }
    let requested = count * size;
    if requested > CRT_ALLOCATION_LIMIT {
        adv_abi::warn_unavailable(
            env,
            "msvcr90.dll!_calloc_crt",
            "valid CRT allocation exceeds the virtual process-heap limit (1 GiB)",
        );
    }
    // The internal VC90 entry passes new-handler mode 0 to its calloc helper.
    // Unlike public calloc, ordinary allocation failure preserves errno. The
    // unconfigured CRT allocation-retry delay has its native default of zero.
    if requested > CRT_ALLOCATION_LIMIT {
        crate::util::set_last_error(env, 8);
        return Outcome::Return(0);
    }
    crate::native_heap::allocate_then(
        env,
        requested,
        true,
        Box::new(|env, address| {
            if address == 0 {
                crate::util::set_last_error(env, 8);
            }
            Outcome::Return(address)
        }),
    )
}

#[win32(ucrtbase, name="free", level=implement)]
fn free(env: &mut Env<'_>, pointer: GuestPtr<()>) -> Outcome {
    crate::native_heap::free(env, pointer.addr())
}

#[win32(ucrtbase, name="realloc", level=implement)]
fn realloc(env: &mut Env<'_>, pointer: GuestPtr<()>, size: u32) -> Outcome {
    if pointer.is_null() {
        return malloc(env, size);
    }
    if size == 0 {
        return crate::native_heap::free(env, pointer.addr());
    }
    if size > HEAP_MAXREQ {
        return result(env, 0).into_outcome();
    }
    crate::native_heap::realloc_then(
        env,
        pointer.addr(),
        size,
        Box::new(|env, address| result(env, address).into_outcome()),
    )
}

fn block_size(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    if pointer == 0 {
        return errors::invalid(env, u32::MAX);
    }
    size_then(env, pointer, Box::new(|_, size| Outcome::Return(size)))
}

pub(super) fn size_then(env: &mut Env<'_>, pointer: u32, then: Done) -> Outcome {
    let heap = env.ext.get::<Heaps>().process_heap;
    match crate::native::public_entry(env, "ntdll.dll", "RtlSizeHeap") {
        Ok(entry) => Outcome::call(entry, [heap, 0, pointer], move |env, value| {
            then(env, value.value())
        }),
        Err(reason) => Outcome::Fail(format!("CRT live RtlSizeHeap resolution: {reason}")),
    }
}

fn expand(env: &mut Env<'_>) -> Outcome {
    let (pointer, size) = (env.arg(0), env.arg(1));
    if pointer == 0 {
        return errors::invalid(env, 0);
    }
    if size > HEAP_MAXREQ {
        return result(env, 0).into_outcome();
    }
    crate::native_heap::realloc_block_then(
        env,
        pointer,
        size.max(1),
        0x10, // HEAP_REALLOC_IN_PLACE_ONLY; a zero request keeps one live byte.
        Box::new(|env, address| result(env, address).into_outcome()),
    )
}

#[cfg(test)]
mod tests;
