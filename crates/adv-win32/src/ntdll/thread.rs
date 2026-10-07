//! Native thread context uses the same stopped process and architecture owner.
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_machine::{Env, Outcome};

#[win32(ntdll, name = "NtGetContextThread", level = implement)]
fn get_context(env: &mut Env<'_>, thread: Handle, context: GuestPtr<()>) -> Outcome {
    crate::kernel32::native_thread_context(env, thread.0, context.addr(), false)
}

#[win32(ntdll, name = "NtSetContextThread", level = implement)]
fn set_context(env: &mut Env<'_>, thread: Handle, context: GuestPtr<()>) -> Outcome {
    crate::kernel32::native_thread_context(env, thread.0, context.addr(), true)
}

#[win32(ntdll, name = "NtResumeThread", level = implement)]
fn resume(env: &mut Env<'_>, thread: Handle, previous: GuestPtr<u32>) -> Outcome {
    crate::kernel32::native_resume_thread(env, thread.0, previous.addr())
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_context_ABI);
    registry.register(set_context_ABI);
    registry.register(resume_ABI);
}
