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
#[win32(ntdll, name = "NtReadVirtualMemory", level = implement)]
fn read(
    env: &mut Env<'_>,
    process: Handle,
    address: GuestPtr<()>,
    buffer: GuestPtr<()>,
    size: u32,
    copied: GuestPtr<u32>,
) -> Outcome {
    crate::kernel32::native_read_memory(
        env,
        process.0,
        address.addr(),
        buffer.addr(),
        size,
        copied.addr(),
    )
}

// Every guest writer commits through the common code-mutation barrier, so
// translated blocks are already invalidated before execution can resume. The
// native x86 oracle also accepts arbitrary handles/ranges here. The mutation
// fixture executes the old and patched routine on interpreter and JIT to verify
// actual instruction coherence, rather than relying only on this return value.
#[win32(ntdll, name = "NtFlushInstructionCache", level = implement)]
fn flush(_env: &mut Env<'_>, _process: Handle, _address: GuestPtr<()>, _size: u32) -> i32 {
    0
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(write_ABI);
    registry.register(read_ABI);
    registry.register(flush_ABI);
}
