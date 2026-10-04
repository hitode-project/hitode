//! Secure UTF-16 concatenation; CRT counts code units, not Unicode characters.
use super::*;
use adv_abi::{GuestPtr, WideStr, guest::memory_failure, win32};

pub(super) fn register(reg: &mut Registry) {
    reg.register(wcscat_s_ABI);
    reg.register(adv_abi::Api {
        module: "api-ms-win-crt-string-l1-1-0.dll",
        ..wcscat_s_ABI
    });
}

#[win32(ucrtbase, name = "wcscat_s", level = implement)]
fn wcscat_s(
    env: &mut Env<'_>,
    destination: GuestPtr<u16>,
    capacity: u32,
    source: WideStr,
) -> Outcome {
    if destination.is_null() || capacity == 0 {
        return invalid_parameter(env, "wcscat_s", 22);
    }
    if source.0.is_null() {
        if let Err(error) = destination.write(env.cpu, 0) {
            return memory_failure(error);
        }
        return invalid_parameter(env, "wcscat_s", 22);
    }
    let mut end = 0;
    while end < capacity {
        match destination.offset(end).and_then(|p| p.read(env.cpu)) {
            Ok(0) => break,
            Ok(_) => end += 1,
            Err(error) => return memory_failure(error),
        }
    }
    if end == capacity {
        if let Err(error) = destination.write(env.cpu, 0) {
            return memory_failure(error);
        }
        return invalid_parameter(env, "wcscat_s", 22);
    }
    for index in 0..capacity - end {
        let unit = match source.0.offset(index).and_then(|p| p.read(env.cpu)) {
            Ok(unit) => unit,
            Err(error) => return memory_failure(error),
        };
        if let Err(error) = destination
            .offset(end + index)
            .and_then(|p| p.write(env.cpu, unit))
        {
            return memory_failure(error);
        }
        if unit == 0 {
            return Outcome::Return(0);
        }
    }
    // Match release UCRT: failed concatenation leaves the copied prefix beyond
    // the first code unit, empties the string, then reports ERANGE.
    if let Err(error) = destination.write(env.cpu, 0) {
        return memory_failure(error);
    }
    invalid_parameter(env, "wcscat_s", 34)
}
