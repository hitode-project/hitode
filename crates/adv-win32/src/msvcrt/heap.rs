//! UCRT/API-set heap fronts share the existing process heap.
use super::*;
use adv_abi::{GuestPtr, GuestReturn, win32};
use adv_cpu::backend::MemFault;

const HEAP_MAXREQ: u32 = 0xffffffe0;

pub(super) fn register(registry: &mut Registry) {
    registry.register(calloc_crt_ABI);
    for api in [malloc_ABI, calloc_ABI, realloc_ABI, free_ABI] {
        registry.register(api);
        registry.register(adv_abi::Api {
            module: "api-ms-win-crt-heap-l1-1-0.dll",
            ..api
        });
    }
}

fn result(env: &mut Env<'_>, address: u32) -> Result<GuestPtr<()>, MemFault> {
    if address == 0 {
        floating::errno_address(env).write(env.cpu, 12)?; // ENOMEM
    }
    Ok(GuestPtr::new(address))
}

#[win32(ucrtbase, name="malloc", level=implement)]
fn malloc(env: &mut Env<'_>, size: u32) -> Outcome {
    crate::heap_lock::with_process_heap(env, move |env| malloc_owned(env, size).into_outcome())
}

fn malloc_owned(env: &mut Env<'_>, size: u32) -> Result<GuestPtr<()>, MemFault> {
    let address = if size <= 0xffffffe0 {
        allocate(env, size, false)
    } else {
        0
    };
    result(env, address)
}

#[win32(ucrtbase, name="calloc", level=implement)]
fn calloc(env: &mut Env<'_>, count: u32, size: u32) -> Outcome {
    crate::heap_lock::with_process_heap(env, move |env| {
        let address = calloc_address(env, count, size);
        result(env, address).into_outcome()
    })
}

fn calloc_address(env: &mut Env<'_>, count: u32, size: u32) -> u32 {
    count
        .checked_mul(size)
        .filter(|&size| size <= HEAP_MAXREQ)
        .map_or(0, |size| allocate(env, size, true))
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
    crate::heap_lock::with_process_heap(env, move |env| {
        let address = calloc_address(env, count, size);
        if address == 0 {
            crate::util::set_last_error(env, 8); // HeapAlloc failure: NOT_ENOUGH_MEMORY.
        }
        Outcome::Return(address)
    })
}

#[win32(ucrtbase, name="free", level=implement)]
fn free(env: &mut Env<'_>, pointer: GuestPtr<()>) -> Outcome {
    if pointer.is_null() {
        return Outcome::Return(0);
    }
    crate::heap_lock::with_process_heap(env, move |env| {
        free_owned(env, pointer);
        Outcome::Return(0)
    })
}

fn free_owned(env: &mut Env<'_>, pointer: GuestPtr<()>) {
    let heaps = env.ext.get_mut::<Heaps>();
    heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer.addr());
}

#[win32(ucrtbase, name="realloc", level=implement)]
fn realloc(env: &mut Env<'_>, pointer: GuestPtr<()>, size: u32) -> Outcome {
    crate::heap_lock::with_process_heap(env, move |env| {
        realloc_owned(env, pointer, size).into_outcome()
    })
}

fn realloc_owned(
    env: &mut Env<'_>,
    pointer: GuestPtr<()>,
    size: u32,
) -> Result<GuestPtr<()>, MemFault> {
    if pointer.is_null() {
        return malloc_owned(env, size);
    }
    if size == 0 {
        free_owned(env, pointer);
        return Ok(GuestPtr::new(0));
    }
    let address = if size <= 0xffffffe0 {
        let heaps = env.ext.get_mut::<Heaps>();
        heaps
            .realloc(
                env.cpu,
                &mut env.sys.vm,
                heaps.process_heap,
                pointer.addr(),
                size,
                false,
                false,
            )
            .unwrap_or(0)
    } else {
        0
    };
    result(env, address)
}

#[cfg(test)]
mod tests;
