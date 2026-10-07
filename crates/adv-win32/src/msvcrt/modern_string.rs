//! Needed modern string fronts validate inputs before the existing CRT kernels.
use super::*;
use adv_abi::{
    AnsiStr, GuestPtr, WideStr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{MemFault, Prot};

pub(super) fn register(registry: &mut Registry) {
    registry.register(reverse_a_ABI);
    registry.register(reverse_w_ABI);
    for module in ["ucrtbase.dll", "api-ms-win-crt-string-l1-1-0.dll"] {
        for api in [copy_a_ABI, copy_w_ABI, copy_n_ABI] {
            registry.register(adv_abi::Api { module, ..api });
        }
    }
}

fn destination(env: &Env<'_>, address: u32, capacity: u32, width: u32) -> Result<(), MemFault> {
    let length = capacity.checked_mul(width).ok_or(MemFault {
        addr: address,
        len: u32::MAX,
    })?;
    check_range(env.cpu, address, length, Prot::WRITE)
}

#[win32(vcruntime140, name = "strrchr", level = implement)]
fn reverse_a(env: &mut Env<'_>, source: AnsiStr, _value: i32) -> Outcome {
    if let Err(error) = source.read(env.cpu, 16 * 1024 * 1024) {
        return memory_failure(error);
    }
    call(env)
}

#[win32(vcruntime140, name = "wcsrchr", level = implement)]
fn reverse_w(env: &mut Env<'_>, source: WideStr, _value: u32) -> Outcome {
    if let Err(error) = source.read(env.cpu, 32768) {
        return memory_failure(error);
    }
    call(env)
}

#[win32(ucrtbase, name = "strcpy_s", level = implement)]
fn copy_a(env: &mut Env<'_>, output: GuestPtr<u8>, capacity: u32, source: AnsiStr) -> Outcome {
    if output.is_null() || capacity == 0 {
        return invalid_parameter(env, "strcpy_s", 22);
    }
    if let Err(error) = destination(env, output.addr(), capacity, 1) {
        return memory_failure(error);
    }
    if source.0.is_null() {
        if let Err(error) = output.write(env.cpu, 0) {
            return memory_failure(error);
        }
        return invalid_parameter(env, "strcpy_s", 22);
    }
    let bytes = match source.read(env.cpu, 16 * 1024 * 1024) {
        Ok(bytes) => bytes,
        Err(error) => return memory_failure(error),
    };
    if bytes.len() >= capacity as usize {
        if let Err(error) = output.write(env.cpu, 0) {
            return memory_failure(error);
        }
        return invalid_parameter(env, "strcpy_s", 34);
    }
    call(env)
}

#[win32(ucrtbase, name = "wcscpy_s", level = implement)]
fn copy_w(env: &mut Env<'_>, output: GuestPtr<u16>, capacity: u32, source: WideStr) -> Outcome {
    if output.is_null() || capacity == 0 {
        return invalid_parameter(env, "wcscpy_s", 22);
    }
    if let Err(error) = destination(env, output.addr(), capacity, 2) {
        return memory_failure(error);
    }
    if source.0.is_null() {
        if let Err(error) = output.write(env.cpu, 0) {
            return memory_failure(error);
        }
        return invalid_parameter(env, "wcscpy_s", 22);
    }
    let units = match source.read(env.cpu, 32768) {
        Ok(units) => units,
        Err(error) => return memory_failure(error),
    };
    if units.len() >= capacity as usize {
        if let Err(error) = output.write(env.cpu, 0) {
            return memory_failure(error);
        }
        return invalid_parameter(env, "wcscpy_s", 34);
    }
    call(env)
}

#[win32(ucrtbase, name = "strncpy_s", level = implement)]
fn copy_n(
    env: &mut Env<'_>,
    output: GuestPtr<u8>,
    capacity: u32,
    source: AnsiStr,
    count: u32,
) -> Outcome {
    if output.is_null() || capacity == 0 {
        return invalid_parameter(env, "strncpy_s", 22);
    }
    if let Err(error) = destination(env, output.addr(), capacity, 1) {
        return memory_failure(error);
    }
    if count == 0 {
        return match output.write(env.cpu, 0) {
            Ok(()) => Outcome::Return(0),
            Err(error) => memory_failure(error),
        };
    }
    if source.0.is_null() {
        if let Err(error) = output.write(env.cpu, 0) {
            return memory_failure(error);
        }
        return invalid_parameter(env, "strncpy_s", 22);
    }
    if count == u32::MAX {
        let thunk = env.sys.thunks.get(env.thunk);
        let api = format!("{}!{}", thunk.module, thunk.name);
        let reason = "_TRUNCATE is outside this finite-count string-copy profile";
        adv_abi::warn_unavailable(env, &api, reason);
        return Outcome::Fail(format!("{api}: {reason}"));
    }
    let bytes = match source.read(env.cpu, 16 * 1024 * 1024) {
        Ok(bytes) => bytes,
        Err(error) => return memory_failure(error),
    };
    if bytes.len().min(count as usize) >= capacity as usize {
        if let Err(error) = output.write(env.cpu, 0) {
            return memory_failure(error);
        }
        return invalid_parameter(env, "strncpy_s", 34);
    }
    call(env)
}
