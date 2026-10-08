//! Win32 virtual-memory argument adapters over the live NT entry boundary.
use crate::util::set_last_error;
use adv_cpu::backend::{GuestExt, Prot, Reg};
use adv_machine::{Env, Outcome};

fn target(env: &mut Env<'_>, name: &str) -> Result<Option<u32>, Outcome> {
    crate::native::overridden(env, name).map_err(|_| crate::native::unavailable(env, name))
}
fn error(env: &mut Env<'_>, status: u32) -> Outcome {
    let code = match status {
        0x8000000d => 299,
        0xc0000018 | 0xc0000019 | 0xc00000a0 => 487,
        0xc0000045 => 87,
        _ => crate::kernel32::file::native::win32_error(status),
    };
    set_last_error(env, code);
    Outcome::Return(0)
}
fn scratch(env: &mut Env<'_>, words: &[u32]) -> Option<u32> {
    let size = words.len() as u32 * 4;
    let p = env.cpu.reg(Reg::Esp).checked_sub(size + 15)? & !15;
    if !env.cpu.accessible(p, size, Prot::RW) {
        return None;
    }
    let bytes: Vec<_> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    if env.cpu.write(p, &bytes).is_err() {
        return None;
    }
    Some(p)
}
pub(super) fn allocate(
    env: &mut Env<'_>,
    process: u32,
    address: u32,
    size: u32,
    flags: u32,
    protection: u32,
) -> Option<Outcome> {
    let target = match target(env, "NtAllocateVirtualMemory") {
        Ok(target) => target?,
        Err(outcome) => return Some(outcome),
    };
    let Some(p) = scratch(env, &[address, size]) else {
        return Some(error(env, 0xc0000017));
    };
    Some(Outcome::call_on_stack(
        target,
        [process, p, 0, p + 4, flags, protection],
        p,
        move |env, result| {
            let address = env.cpu.read_u32(p);
            if (result.value() as i32) < 0 {
                return error(env, result.value());
            }
            match address {
                Ok(address) => Outcome::Return(address),
                Err(_) => error(env, 0xc0000005),
            }
        },
    ))
}
pub(super) fn free(
    env: &mut Env<'_>,
    process: u32,
    address: u32,
    size: u32,
    flags: u32,
) -> Option<Outcome> {
    let target = match target(env, "NtFreeVirtualMemory") {
        Ok(target) => target?,
        Err(outcome) => return Some(outcome),
    };
    let Some(p) = scratch(env, &[address, size]) else {
        return Some(error(env, 0xc0000017));
    };
    Some(Outcome::call_on_stack(
        target,
        [process, p, p + 4, flags],
        p,
        move |env, result| {
            if (result.value() as i32) < 0 {
                error(env, result.value())
            } else {
                Outcome::Return(1)
            }
        },
    ))
}
pub(super) fn protect(
    env: &mut Env<'_>,
    process: u32,
    address: u32,
    size: u32,
    protection: u32,
    output: u32,
) -> Option<Outcome> {
    let target = match target(env, "NtProtectVirtualMemory") {
        Ok(target) => target?,
        Err(outcome) => return Some(outcome),
    };
    let Some(p) = scratch(env, &[address, size]) else {
        return Some(error(env, 0xc0000017));
    };
    Some(Outcome::call_on_stack(
        target,
        [process, p, p + 4, protection, output],
        p,
        move |env, result| {
            if (result.value() as i32) < 0 {
                error(env, result.value())
            } else {
                Outcome::Return(1)
            }
        },
    ))
}
pub(super) fn query(
    env: &mut Env<'_>,
    process: u32,
    address: u32,
    output: u32,
    length: u32,
) -> Option<Outcome> {
    let target = match target(env, "NtQueryVirtualMemory") {
        Ok(target) => target?,
        Err(outcome) => return Some(outcome),
    };
    let Some(p) = scratch(env, &[0]) else {
        return Some(error(env, 0xc0000017));
    };
    Some(Outcome::call_on_stack(
        target,
        [process, address, 0, output, length, p],
        p,
        move |env, result| {
            let length = env.cpu.read_u32(p);
            if (result.value() as i32) < 0 {
                return error(env, result.value());
            }
            match length {
                Ok(length) => Outcome::Return(length),
                Err(_) => error(env, 0xc0000005),
            }
        },
    ))
}
pub(super) fn transfer(env: &mut Env<'_>, name: &'static str, args: [u32; 5]) -> Option<Outcome> {
    let target = match target(env, name) {
        Ok(target) => target?,
        Err(outcome) => return Some(outcome),
    };
    Some(Outcome::call(target, args, move |env, result| {
        if (result.value() as i32) < 0 {
            error(env, result.value())
        } else {
            Outcome::Return(1)
        }
    }))
}
