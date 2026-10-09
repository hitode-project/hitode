//! Native remote transfers marshal owned bytes through the process supervisor.
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_machine::{Env, GuestCode, Outcome};

/// Authored x86 implementation of the documented matching-prefix count.
/// Real CPU reads preserve access faults and handler-directed continuation.
/// ```text
/// push esi; push edi
/// mov esi,[esp+12]; mov edi,[esp+16]; mov ecx,[esp+20]
/// mov eax,ecx; test ecx,ecx; jz done
/// cld; repe cmpsb; je matched; dec eax
/// matched: sub eax,ecx
/// done: pop edi; pop esi; ret 12
/// ```
static COMPARE: GuestCode = GuestCode {
    code: &[
        0x56, 0x57, 0x8b, 0x74, 0x24, 0x0c, 0x8b, 0x7c, 0x24, 0x10, 0x8b, 0x4c, 0x24, 0x14, 0x89,
        0xc8, 0x85, 0xc9, 0x74, 0x08, 0xfc, 0xf3, 0xa6, 0x74, 0x01, 0x48, 0x29, 0xc8, 0x5f, 0x5e,
        0xc2, 0x0c, 0x00,
    ],
    host: &[],
    links: &[],
};

fn compare(env: &mut Env<'_>) -> Outcome {
    let Some(body) = env.sys.thunks.get(env.thunk).guest_body() else {
        return Outcome::Fail("RtlCompareMemory body is missing".into());
    };
    match env.checkpoint() {
        Ok(mut checkpoint) => {
            checkpoint.state.eip = body;
            Outcome::ResumeExecution(checkpoint)
        }
        Err(error) => Outcome::Fail(error.to_string()),
    }
}
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
    for module in ["ntdll.dll", "kernel32.dll"] {
        // Both user-mode exports share the WDK's SIZE_T(void*, void*, SIZE_T)
        // declaration; the SDK metadata lists only the kernel32 alias.
        crate::stdcall(registry, module, "RtlCompareMemory", 3, compare);
        registry.set_guest(module, "RtlCompareMemory", &COMPARE);
    }
    registry.register(write_ABI);
    registry.register(read_ABI);
    registry.register(flush_ABI);
}
