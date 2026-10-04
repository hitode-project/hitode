//! Legacy scalar C++ allocation uses the existing guest process heap.
use super::*;
use adv_abi::{GuestPtr, win32};

pub(super) fn register(registry: &mut Registry) {
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        for api in [scalar_new_ABI, scalar_delete_ABI] {
            registry.register(adv_abi::Api { module, ..api });
        }
    }
}

#[win32(msvcr90, name = "??2@YAPAXI@Z", level = implement)]
fn scalar_new(env: &mut Env<'_>, size: u32) -> Outcome {
    // The shared heap rounds a zero-byte request to a distinct live block.
    let address = allocate(env, size, false);
    if address == 0 {
        // The throwing overload must not return NULL. Guest C++ exception
        // objects and new-handler dispatch have not been implemented yet.
        return Outcome::Fail(format!(
            "operator new({size}): allocation failed; std::bad_alloc/new-handler dispatch is not implemented"
        ));
    }
    Outcome::Return(address)
}

#[win32(msvcr90, name = "??3@YAXPAX@Z", level = implement)]
fn scalar_delete(env: &mut Env<'_>, pointer: GuestPtr<()>) -> Outcome {
    if pointer.is_null() {
        return Outcome::Return(0);
    }
    let heaps = env.ext.get_mut::<Heaps>();
    if !heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer.addr()) {
        return Outcome::Fail(format!(
            "operator delete: {:#x} is not a live process-heap allocation",
            pointer.addr()
        ));
    }
    Outcome::Return(0)
}
