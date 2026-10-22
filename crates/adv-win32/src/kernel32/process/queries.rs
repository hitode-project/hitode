//! Local memory probes and x86-only process capabilities, without host identity.
use adv_abi::{AnsiStr, Registry, WideStr, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;

#[win32(kernel32, name = "IsBadStringPtrW", level = implement)]
fn bad_string(env: &mut Env<'_>, string: WideStr, maximum: u32) -> i32 {
    for index in 0..maximum {
        let Some(pointer) = index
            .checked_mul(2)
            .and_then(|offset| string.0.addr().checked_add(offset))
        else {
            return 1;
        };
        if !env.cpu.accessible(pointer, 2, Prot::READ) {
            return 1;
        }
        match env.cpu.read_u16(pointer) {
            Ok(0) => return 0,
            Ok(_) => (),
            Err(_) => return 1,
        }
    }
    0
}

#[win32(kernel32, name = "GetSystemWow64DirectoryA", level = implement)]
fn wow64_directory(env: &mut Env<'_>, _output: AnsiStr, _capacity: u32) -> u32 {
    // PERS-PROFILE-1 is native x86, so there is no WOW64 subsystem directory.
    crate::util::set_last_error(env, 120);
    0
}

#[win32(kernel32, name = "FlushProcessWriteBuffers", level = implement)]
fn flush_write_buffers(_env: &mut Env<'_>) {
    // Guest contexts share a single virtual processor and coherent memory.
    // A full host fence also orders writes crossing native service boundaries.
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
}

pub(super) fn register(registry: &mut Registry) {
    for api in [bad_string_ABI, wow64_directory_ABI, flush_write_buffers_ABI] {
        registry.register(api);
    }
}
