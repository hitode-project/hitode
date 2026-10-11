//! Ideal processor hints over the same flat, one-CPU guest profile as affinity.
//! All owned threads already run on processor zero; validation and queries do
//! not pin host threads or introduce a second scheduling owner.
use super::*;
use adv_cpu::backend::Prot;

fn target(env: &mut Env<'_>, handle: Handle, access: u32) -> bool {
    match super::super::thread_target(env, handle.0, access) {
        Ok(_) => true,
        Err(error) => {
            set_last_error(env, error.code);
            false
        }
    }
}

fn memory(env: &mut Env<'_>, pointer: u32, access: Prot) -> bool {
    if adv_abi::guest::check_range(env.cpu, pointer, 4, access).is_err() {
        set_last_error(env, 998); // ERROR_NOACCESS, without partial output.
        false
    } else {
        true
    }
}

#[win32(kernel32, name = "SetThreadIdealProcessor", level = state)]
fn set_ideal(env: &mut Env<'_>, handle: Handle, number: u32) -> u32 {
    if !target(env, handle, 0x20) {
        return u32::MAX;
    }
    let profile = *env.ext.get_or_default::<Processors>();
    // MAXIMUM_PROCESSORS is 32 for the native x86 guest ABI: query only.
    if number != 32 && number >= profile.count() {
        set_last_error(env, err::INVALID_PARAMETER);
        return u32::MAX;
    }
    profile.current()
}

#[win32(kernel32, name = "GetThreadIdealProcessorEx", level = state)]
fn get_ideal_ex(env: &mut Env<'_>, handle: Handle, output: GuestPtr<u32>) -> i32 {
    if !target(env, handle, 0x800) || !memory(env, output.addr(), Prot::WRITE) {
        return 0;
    }
    let number = env.ext.get_or_default::<Processors>().current();
    if output.write(env.cpu, number << 16).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    1
}

#[win32(kernel32, name = "SetThreadIdealProcessorEx", level = state)]
fn set_ideal_ex(
    env: &mut Env<'_>,
    handle: Handle,
    input: GuestPtr<u32>,
    previous: GuestPtr<u32>,
) -> Result<i32, MemFault> {
    // Native captures this input in user mode before checking the thread.
    // Bad setter pointers use the common typed-ABI memory-fault boundary;
    // the getter instead returns ERROR_NOACCESS from its query service.
    let packed = input.read(env.cpu)?;
    if !target(env, handle, 0x20) {
        return Ok(0);
    }
    // PROCESSOR_NUMBER is WORD Group, BYTE Number, BYTE Reserved.
    let profile = *env.ext.get_or_default::<Processors>();
    if packed & 0xff00ffff != 0 || (packed >> 16) >= profile.count() {
        set_last_error(env, err::INVALID_PARAMETER);
        return Ok(0);
    }
    if !previous.is_null() {
        previous.write(env.cpu, profile.current() << 16)?;
    }
    // Snapshot input before output so callers can use the same structure.
    Ok(1)
}

pub(super) fn register(registry: &mut Registry) {
    for api in [set_ideal_ABI, get_ideal_ex_ABI, set_ideal_ex_ABI] {
        registry.register(api);
    }
}

#[cfg(test)]
mod tests;
