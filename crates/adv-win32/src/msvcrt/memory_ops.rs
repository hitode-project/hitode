//! VCRuntime memory fronts share the established legacy CRT byte operations.
use adv_abi::{
    GuestPtr, Registry,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{MemFault, Prot};
use adv_machine::{Env, Outcome};

pub(super) fn register(registry: &mut Registry) {
    for api in [memcpy_ABI, memmove_ABI, memset_ABI, memicmp_ABI] {
        registry.register(api);
    }
}

/// Bounded chunks and the same unsigned-byte difference for both CRT comparisons.
pub(super) fn compare(
    env: &mut Env<'_>,
    left_pointer: u32,
    right_pointer: u32,
    count: u32,
    lowercase: Option<&[u8; 256]>,
) -> Result<i32, MemFault> {
    let mut left = [0; 4096];
    let mut right = [0; 4096];
    let mut offset = 0;
    while offset < count {
        let length = (count - offset).min(4096) as usize;
        env.cpu
            .read(left_pointer.wrapping_add(offset), &mut left[..length])?;
        env.cpu
            .read(right_pointer.wrapping_add(offset), &mut right[..length])?;
        for (&left, &right) in left[..length].iter().zip(&right[..length]) {
            let (left, right) = lowercase.map_or((left, right), |table| {
                (table[usize::from(left)], table[usize::from(right)])
            });
            let difference = i32::from(left) - i32::from(right);
            if difference != 0 {
                return Ok(difference);
            }
        }
        offset += length as u32;
    }
    Ok(0)
}

#[win32(msvcrt, name = "_memicmp", level = implement)]
fn memicmp(env: &mut Env<'_>, left: GuestPtr<u8>, right: GuestPtr<u8>, count: u32) -> Outcome {
    if count == 0 {
        return Outcome::Return(0);
    }
    if left.is_null() || right.is_null() {
        // Legacy MSVCRT returns _NLSCMPERROR and writes errno alone; no DOS
        // error/LastError or modern invalid-parameter callback is published.
        let errno = super::floating::errno_address(env);
        if errno.is_null() {
            return Outcome::Fail("CRT comparison errno allocation failed".into());
        }
        if let Err(error) = check_range(env.cpu, errno.addr(), 4, Prot::WRITE) {
            return memory_failure(error);
        }
        return match errno.write(env.cpu, 22) {
            Ok(()) => Outcome::Return(i32::MAX as u32),
            Err(error) => memory_failure(error),
        };
    }
    if let Err(error) = check_range(env.cpu, left.addr(), count, Prot::READ)
        .and_then(|_| check_range(env.cpu, right.addr(), count, Prot::READ))
    {
        return memory_failure(error);
    }
    let Some(lowercase) = super::locale::lowercase_bytes(env) else {
        adv_abi::warn_unavailable(
            env,
            "msvcrt.dll!_memicmp",
            "byte comparisons require C or CP932/936/1252 LC_CTYPE",
        );
        return Outcome::Fail("CRT memory comparison locale is unavailable".into());
    };
    match compare(env, left.addr(), right.addr(), count, Some(&lowercase)) {
        Ok(difference) => Outcome::Return(difference as u32),
        Err(error) => memory_failure(error),
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
