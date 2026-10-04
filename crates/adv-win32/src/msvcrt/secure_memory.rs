//! Secure CRT byte copies reuse the guest memory and shared invalid-handler owner.
use super::*;
use adv_abi::{
    GuestPtr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::Prot;

pub(super) fn register(registry: &mut Registry) {
    for module in [
        "msvcrt.dll",
        "msvcr80.dll",
        "msvcr90.dll",
        "ucrtbase.dll",
        "api-ms-win-crt-string-l1-1-0.dll",
    ] {
        for api in [memcpy_s_ABI, memmove_s_ABI] {
            registry.register(adv_abi::Api { module, ..api });
        }
    }
}

fn copy(
    env: &mut Env<'_>,
    destination: GuestPtr<u8>,
    capacity: u32,
    source: GuestPtr<u8>,
    count: u32,
    moving: bool,
) -> Outcome {
    if count == 0 {
        return Outcome::Return(0);
    }
    let name = if moving { "memmove_s" } else { "memcpy_s" };
    if destination.is_null() {
        return invalid_parameter(env, name, 22);
    }
    if source.is_null() || capacity < count {
        // Unlike memmove_s, memcpy_s erases the declared destination capacity
        // before invoking the invalid-parameter callback.
        if !moving {
            if let Err(error) = check_range(env.cpu, destination.addr(), capacity, Prot::WRITE) {
                return memory_failure(error);
            }
            let zero = [0; 4096];
            let mut offset = 0;
            while offset < capacity {
                let length = (capacity - offset).min(zero.len() as u32);
                if let Err(error) = env
                    .cpu
                    .write(destination.addr() + offset, &zero[..length as usize])
                {
                    return memory_failure(error);
                }
                offset += length;
            }
        }
        return invalid_parameter(env, name, if source.is_null() { 22 } else { 34 });
    }
    // Only count bytes are touched on success, regardless of excess capacity.
    // The common copy handles overlap for memmove_s; overlap in memcpy_s is UB.
    if let Err(error) = check_range(env.cpu, source.addr(), count, Prot::READ)
        .and_then(|_| check_range(env.cpu, destination.addr(), count, Prot::WRITE))
        .and_then(|_| {
            env.cpu
                .copy_memory(source.addr(), destination.addr(), count as usize)
        })
    {
        return memory_failure(error);
    }
    Outcome::Return(0)
}

#[win32(ucrtbase, name = "memcpy_s", level = implement)]
fn memcpy_s(
    env: &mut Env<'_>,
    destination: GuestPtr<u8>,
    capacity: u32,
    source: GuestPtr<u8>,
    count: u32,
) -> Outcome {
    copy(env, destination, capacity, source, count, false)
}
#[win32(ucrtbase, name = "memmove_s", level = implement)]
fn memmove_s(
    env: &mut Env<'_>,
    destination: GuestPtr<u8>,
    capacity: u32,
    source: GuestPtr<u8>,
    count: u32,
) -> Outcome {
    copy(env, destination, capacity, source, count, true)
}
