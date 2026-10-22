//! In-place byte and UTF-16 edits share checked code-unit access. Secure set
//! operations retain the native incremental writes and terminator validation.
use super::*;
use adv_abi::guest::memory_failure;
use adv_cpu::backend::MemFault;
use strings::{advance, length, read, write};

const NAMES: &[&str] = &[
    "_strrev",
    "_wcsrev",
    "_strset",
    "_wcsset",
    "_strnset",
    "_wcsnset",
    "_strset_s",
    "_wcsset_s",
    "_strnset_s",
    "_wcsnset_s",
    "_o__strset_s",
    "_o__wcsset_s",
    "_o__strnset_s",
    "_o__wcsnset_s",
    "_o__wcsset",
    "_o__wcsnset",
];

pub(super) fn register(registry: &mut Registry) {
    let exports: Vec<_> = NAMES
        .iter()
        .map(|&name| (name, call as adv_machine::Handler))
        .collect();
    versions::register(registry, &exports);
}

fn reverse(env: &mut Env<'_>, destination: u32, width: u32) -> Result<u32, MemFault> {
    let count = length(env, destination, width, None)?;
    let mut left = destination;
    let mut right = destination
        .checked_add(count.saturating_sub(1).checked_mul(width).ok_or(MemFault {
            addr: destination,
            len: u32::MAX,
        })?)
        .ok_or(MemFault {
            addr: destination,
            len: u32::MAX,
        })?;
    for _ in 0..count / 2 {
        let a = read(env, left, width)?;
        let b = read(env, right, width)?;
        write(env, left, b, width)?;
        write(env, right, a, width)?;
        left = advance(left, width)?;
        right -= width;
    }
    Ok(destination)
}

fn set(
    env: &mut Env<'_>,
    destination: u32,
    value: u32,
    mut count: u32,
    width: u32,
) -> Result<u32, MemFault> {
    let mut pointer = destination;
    while count != 0 && read(env, pointer, width)? != 0 {
        write(env, pointer, value, width)?;
        count -= 1;
        if count != 0 {
            pointer = advance(pointer, width)?;
        }
    }
    Ok(destination)
}

fn secure_set(
    env: &mut Env<'_>,
    destination: u32,
    value: u32,
    mut count: u32,
    mut capacity: u32,
    width: u32,
) -> Result<bool, MemFault> {
    let mut pointer = destination;
    // Check the original terminator after each write. Even a NUL replacement
    // or exhausted count must validate the remaining original string.
    while read(env, pointer, width)? != 0 {
        if count == 0 {
            break;
        }
        capacity -= 1;
        if capacity == 0 {
            write(env, destination, 0, width)?;
            return Ok(false);
        }
        write(env, pointer, value, width)?;
        pointer = advance(pointer, width)?;
        count -= 1;
    }
    if count == 0 {
        while read(env, pointer, width)? != 0 {
            capacity -= 1;
            if capacity == 0 {
                write(env, destination, 0, width)?;
                return Ok(false);
            }
            pointer = advance(pointer, width)?;
        }
    }
    Ok(true)
}

fn call(env: &mut Env<'_>) -> Outcome {
    let name = env.sys.thunks.get(env.thunk).name.clone();
    // Pinned UCRT _o_ entries retain the same x86 argument/result contract.
    // Their global-state guard selects the configured single application.
    let name = name.strip_prefix("_o_").unwrap_or(&name);
    let width = if name.starts_with("_wcs") { 2 } else { 1 };
    let destination = env.arg(0);
    if name.ends_with("rev") {
        return match reverse(env, destination, width) {
            Ok(value) => Outcome::Return(value),
            Err(fault) => memory_failure(fault),
        };
    }
    let bounded = name.contains("nset");
    if name.ends_with("_s") {
        let (capacity, value) = (env.arg(1), env.arg(2));
        let count = if bounded { env.arg(3) } else { u32::MAX };
        if bounded && count == 0 && destination == 0 && capacity == 0 {
            return Outcome::Return(0);
        }
        if destination == 0 || capacity == 0 {
            return errors::invalid(env, 22);
        }
        return match secure_set(env, destination, value, count, capacity, width) {
            Ok(true) => Outcome::Return(0),
            Ok(false) => errors::invalid(env, 22),
            Err(fault) => memory_failure(fault),
        };
    }
    let value = env.arg(1);
    let count = if bounded { env.arg(2) } else { u32::MAX };
    match set(env, destination, value, count, width) {
        Ok(value) => Outcome::Return(value),
        Err(fault) => memory_failure(fault),
    }
}
