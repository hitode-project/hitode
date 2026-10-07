//! Native thread context uses the same stopped process and architecture owner.
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_machine::{Env, Outcome};

#[win32(ntdll, name = "NtGetContextThread", level = implement)]
fn get_context(env: &mut Env<'_>, thread: Handle, context: GuestPtr<()>) -> Outcome {
    crate::kernel32::native_thread_context(env, thread.0, context.addr(), false)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_context_ABI);
}
