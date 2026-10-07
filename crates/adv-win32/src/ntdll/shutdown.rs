//! Native loader shutdown reuses the real DLL/TLS/FLS notification owner.
use adv_abi::{Registry, win32};
use adv_machine::{Env, Outcome};

#[win32(ntdll, name = "LdrShutdownProcess", level = implement)]
fn shutdown_process(env: &mut Env<'_>) -> Outcome {
    crate::kernel32::shutdown_process(env)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(shutdown_process_ABI);
}
