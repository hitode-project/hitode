//! VCRuntime memory fronts share the established legacy CRT byte operations.
use adv_abi::{GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::{MemFault, Prot};
use adv_machine::Env;

pub(super) fn register(registry: &mut Registry) {
    for api in [memcpy_ABI, memmove_ABI, memset_ABI] {
        registry.register(api);
    }
}

pub(super) fn copy(
    env: &mut Env<'_>,
    destination: u32,
    source: u32,
    count: u32,
) -> Result<u32, MemFault> {
    env.cpu.copy_memory(source, destination, count as usize)?;
    Ok(destination)
}

pub(super) fn fill(
    env: &mut Env<'_>,
    destination: u32,
    value: u8,
    count: u32,
) -> Result<u32, MemFault> {
    let chunk = [value; 4096];
    let mut offset = 0;
    while offset < count {
        let length = (count - offset).min(chunk.len() as u32);
        env.cpu
            .write(destination.wrapping_add(offset), &chunk[..length as usize])?;
        offset += length;
    }
    Ok(destination)
}

fn checked_copy(
    env: &mut Env<'_>,
    destination: GuestPtr<u8>,
    source: GuestPtr<u8>,
    count: u32,
) -> Result<GuestPtr<u8>, MemFault> {
    if count == 0 {
        return Ok(destination);
    }
    check_range(env.cpu, source.addr(), count, Prot::READ)?;
    check_range(env.cpu, destination.addr(), count, Prot::WRITE)?;
    copy(env, destination.addr(), source.addr(), count).map(GuestPtr::new)
}

#[win32(vcruntime140, name = "memcpy", level = implement)]
fn memcpy(
    env: &mut Env<'_>,
    destination: GuestPtr<u8>,
    source: GuestPtr<u8>,
    count: u32,
) -> Result<GuestPtr<u8>, MemFault> {
    checked_copy(env, destination, source, count)
}

#[win32(vcruntime140, name = "memmove", level = implement)]
fn memmove(
    env: &mut Env<'_>,
    destination: GuestPtr<u8>,
    source: GuestPtr<u8>,
    count: u32,
) -> Result<GuestPtr<u8>, MemFault> {
    checked_copy(env, destination, source, count)
}

#[win32(vcruntime140, name = "memset", level = implement)]
fn memset(
    env: &mut Env<'_>,
    destination: GuestPtr<u8>,
    value: i32,
    count: u32,
) -> Result<GuestPtr<u8>, MemFault> {
    check_range(env.cpu, destination.addr(), count, Prot::WRITE)?;
    fill(env, destination.addr(), value as u8, count).map(GuestPtr::new)
}
