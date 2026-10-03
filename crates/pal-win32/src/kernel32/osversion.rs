//! One virtual OS identity, shared by GetVersion[Ex] and VerifyVersionInfo.
//! This is the existing Windows 7 SP1 x86 personality, not the host OS version.
use pal_cpu::{GuestExt, Prot};
use pal_machine::{Env, Outcome, Registry};

use crate::{stdcall, util::set_last_error};

// Indexed by the bit position of VER_*.
const VERSION: [u32; 8] = [1, 6, 7601, 2, 0, 1, 0, 1];

fn fail(env: &mut Env<'_>, code: u32) -> Outcome {
    set_last_error(env, code);
    Outcome::Return(0)
}
fn get_version(_: &mut Env<'_>) -> Outcome {
    Outcome::Return((VERSION[2] << 16) | (VERSION[0] << 8) | VERSION[1])
}
fn get_version_ex(env: &mut Env<'_>, wide: bool) -> Outcome {
    let pointer = env.arg(0);
    let base = if wide { 276 } else { 148 };
    let size = env.cpu.read_u32(pointer).unwrap_or(0);
    if (size != base && size != base + 8) || !env.cpu.accessible(pointer, size, Prot::WRITE) {
        return fail(env, 87);
    }
    let mut bytes = [0u8; 284];
    bytes[..4].copy_from_slice(&size.to_le_bytes());
    for (index, value) in [VERSION[1], VERSION[0], VERSION[2], VERSION[3]].into_iter().enumerate() {
        bytes[4 + index * 4..8 + index * 4].copy_from_slice(&value.to_le_bytes());
    }
    for (index, byte) in b"Service Pack 1".iter().enumerate() {
        bytes[20 + index * if wide { 2 } else { 1 }] = *byte;
    }
    if size == base + 8 {
        let at = base as usize;
        bytes[at..at + 2].copy_from_slice(&(VERSION[5] as u16).to_le_bytes());
        bytes[at + 2..at + 4].copy_from_slice(&(VERSION[4] as u16).to_le_bytes());
        bytes[at + 4..at + 6].copy_from_slice(&(VERSION[6] as u16).to_le_bytes());
        bytes[at + 6] = VERSION[7] as u8;
    }
    match env.cpu.write(pointer, &bytes[..size as usize]) {
        Ok(()) => Outcome::Return(1),
        Err(_) => fail(env, 998),
    }
}

fn condition_mask(env: &mut Env<'_>) -> Outcome {
    let incoming = u64::from(env.arg(0)) | (u64::from(env.arg(1)) << 32);
    let types = env.arg(2) & 0xff;
    if types == 0 {
        return Outcome::Return64(0);
    }
    let bit = 31 - types.leading_zeros();
    let shift = bit * 3;
    let condition = u64::from(env.arg(3) & 7);
    // Bit 63 is the native condition-mask marker; comparisons use only the
    // selected three-bit fields. Replacing an operator clears its old bits.
    Outcome::Return64((incoming & !(7u64 << shift)) | (condition << shift) | (1 << 63))
}
fn compare(actual: u32, requested: u32, condition: u64) -> bool {
    match condition {
        1 => actual == requested,
        2 => actual > requested,
        3 => actual >= requested,
        4 => actual < requested,
        5 => actual <= requested,
        _ => false,
    }
}
fn matches(requested: [u32; 8], types: u32, condition: u64) -> Result<bool, u32> {
    if types == 0 || types & !0xff != 0 {
        return Err(160);
    }
    let op = |bit: usize| (condition >> (3 * bit)) & 7;
    // Independent attributes must match even if an earlier version tuple
    // component already satisfies a greater/less comparison.
    for bit in [2, 3, 7] {
        if types & (1 << bit) != 0 && !compare(VERSION[bit], requested[bit], op(bit)) {
            return Ok(false);
        }
    }
    if types & 0x40 != 0 {
        let good = match op(6) {
            6 => VERSION[6] & requested[6] == requested[6],
            7 => requested[6] == 0 || VERSION[6] & requested[6] != 0,
            _ => return Err(160),
        };
        if !good {
            return Ok(false);
        }
    }
    let mut operation = 0;
    for bit in [1, 0, 5, 4] {
        if types & (1 << bit) == 0 {
            continue;
        }
        if operation == 0 || operation == 1 {
            operation = op(bit);
        }
        if operation == 0 {
            return Ok(false);
        }
        if VERSION[bit] != requested[bit] || bit == 4 {
            return Ok(compare(VERSION[bit], requested[bit], operation));
        }
    }
    Ok(true)
}
fn verify(env: &mut Env<'_>, wide: bool) -> Outcome {
    let pointer = env.arg(0);
    let base = if wide { 276 } else { 148 };
    if !env.cpu.accessible(pointer, base + 8, Prot::READ) {
        return fail(env, 998);
    }
    let requested = [
        env.cpu.read_u32(pointer + 8).unwrap(),
        env.cpu.read_u32(pointer + 4).unwrap(),
        env.cpu.read_u32(pointer + 12).unwrap(),
        env.cpu.read_u32(pointer + 16).unwrap(),
        u32::from(env.cpu.read_u16(pointer + base + 2).unwrap()),
        u32::from(env.cpu.read_u16(pointer + base).unwrap()),
        u32::from(env.cpu.read_u16(pointer + base + 4).unwrap()),
        u32::from(env.cpu.read_u8(pointer + base + 6).unwrap()),
    ];
    let condition = u64::from(env.arg(2)) | (u64::from(env.arg(3)) << 32);
    match matches(requested, env.arg(1), condition) {
        Ok(true) => Outcome::Return(1),
        Ok(false) => fail(env, 1150),
        Err(code) => fail(env, code),
    }
}
pub(super) fn register(registry: &mut Registry) {
    stdcall(registry, super::K32, "GetVersion", 0, get_version);
    stdcall(registry, super::K32, "GetVersionExA", 1, |e| get_version_ex(e, false));
    stdcall(registry, super::K32, "GetVersionExW", 1, |e| get_version_ex(e, true));
    stdcall(registry, super::K32, "VerSetConditionMask", 4, condition_mask);
    stdcall(registry, super::K32, "VerifyVersionInfoA", 4, |e| verify(e, false));
    stdcall(registry, super::K32, "VerifyVersionInfoW", 4, |e| verify(e, true));
}
