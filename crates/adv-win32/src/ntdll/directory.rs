//! RTL byte-counted directory calls share the Win32 process VFS directory.
use adv_abi::{GuestPtr, Registry, WideStr, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;
use adv_os::vfs::DirectoryError;

use crate::Win32;

const ACCESS_VIOLATION: i32 = 0xc000_0005u32 as i32;
const INVALID_PARAMETER: i32 = 0xc000_000du32 as i32;
const NAME_INVALID: i32 = 0xc000_0033u32 as i32;

pub(super) fn register(registry: &mut Registry) {
    registry.register(set_ABI);
    registry.register(get_ABI);
}

#[win32(ntdll, name = "RtlSetCurrentDirectory_U", level = implement)]
fn set(env: &mut Env<'_>, descriptor: GuestPtr<()>) -> i32 {
    let at = descriptor.addr();
    if check_range(env.cpu, at, 8, Prot::READ).is_err() {
        return ACCESS_VIOLATION;
    }
    let Ok(sizes) = env.cpu.read_u32(at) else {
        return ACCESS_VIOLATION;
    };
    let length = sizes & 0xffff;
    if length == 0 {
        return NAME_INVALID;
    }
    if length & 1 != 0 || length > sizes >> 16 {
        return INVALID_PARAMETER;
    }
    let Ok(buffer) = env.cpu.read_u32(at + 4) else {
        return ACCESS_VIOLATION;
    };
    if check_range(env.cpu, buffer, length, Prot::READ).is_err() {
        return ACCESS_VIOLATION;
    }
    let mut bytes = vec![0; length as usize];
    if env.cpu.read(buffer, &mut bytes).is_err() {
        return ACCESS_VIOLATION;
    }
    let units: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect();
    let Ok(path) = String::from_utf16(&units) else {
        warn_unavailable(
            env,
            "ntdll.dll!RtlSetCurrentDirectory_U",
            "unpaired UTF-16 surrogate directory is unavailable",
        );
        return 0xc000_00bbu32 as i32;
    };
    match env.ext.get_mut::<Win32>().vfs.set_current_directory(&path) {
        Ok(()) => 0,
        Err(DirectoryError::NameNotFound) => 0xc000_0034u32 as i32,
        Err(DirectoryError::PathNotFound) => 0xc000_003au32 as i32,
        Err(DirectoryError::NotDirectory) => 0xc000_0103u32 as i32,
        Err(DirectoryError::AccessDenied) => 0xc000_0022u32 as i32,
    }
}

#[win32(ntdll, name = "RtlGetCurrentDirectory_U", level = implement)]
fn get(env: &mut Env<'_>, capacity: u32, output: WideStr) -> u32 {
    let bytes: Vec<_> = env
        .ext
        .get::<Win32>()
        .vfs
        .cwd
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    let required = bytes.len() as u32;
    if capacity < required {
        return required;
    }
    if check_range(env.cpu, output.0.addr(), required, Prot::WRITE).is_err()
        || env.cpu.write(output.0.addr(), &bytes).is_err()
    {
        return 0;
    }
    required - 2
}
