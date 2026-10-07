//! Native remote transfers marshal owned bytes through the process supervisor.
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_machine::{Env, Outcome};
#[win32(ntdll, name = "NtWriteVirtualMemory", level = implement)]
fn write(
    env: &mut Env<'_>,
    process: Handle,
    address: GuestPtr<()>,
    buffer: GuestPtr<()>,
    size: u32,
    copied: GuestPtr<u32>,
) -> Outcome {
    crate::kernel32::native_write_memory(
        env,
        process.0,
        address.addr(),
        buffer.addr(),
        size,
        copied.addr(),
    )
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(write_ABI);
}
