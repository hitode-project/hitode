//! Legacy MSVCRT access checks share the existing guest attribute and errno owners.
use super::{floating, shared_file};
use adv_abi::{
    AnsiStr, GuestPtr, Registry, WideStr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};

pub(super) fn register(registry: &mut Registry) {
    registry.register(access_a_ABI);
    registry.register(access_w_ABI);
}

fn fail(env: &mut Env<'_>, errno: GuestPtr<i32>, value: i32, dos: u32) -> Outcome {
    let bytes = [value.to_le_bytes(), dos.to_le_bytes()].concat();
    match env.cpu.write(errno.addr(), &bytes) {
        Ok(()) => Outcome::Return(u32::MAX),
        Err(error) => memory_failure(error),
    }
}

fn unsupported(env: &mut Env<'_>, errno: GuestPtr<i32>, api: &str, reason: &str) -> Outcome {
    adv_abi::warn_unavailable(env, api, reason);
    crate::util::set_last_error(env, 50);
    fail(env, errno, 40, 50)
}

fn access(env: &mut Env<'_>, pointer: u32, mode: i32, wide: bool) -> Outcome {
    let errno = floating::errno_address(env);
    if errno.is_null() {
        return Outcome::Fail("CRT access errno allocation failed".into());
    }
    if let Err(error) = check_range(env.cpu, errno.addr(), 8, Prot::WRITE) {
        return memory_failure(error);
    }
    // Legacy MSVCRT accepts modes 0..15, checks only bit 1, and has no returning
    // invalid-parameter callback here. Modern CRT profiles are not aliased.
    if pointer == 0 || !(0..=15).contains(&mode) {
        return fail(env, errno, 22, 0);
    }
    let api = if wide {
        "msvcrt.dll!_waccess"
    } else {
        "msvcrt.dll!_access"
    };
    let path = if wide {
        let units = match WideStr(GuestPtr::new(pointer)).read(env.cpu, 32768) {
            Ok(units) => units,
            Err(error) => return memory_failure(error),
        };
        match String::from_utf16(&units) {
            Ok(path) => path,
            Err(_) => {
                return unsupported(env, errno, api, "access path contains invalid UTF16");
            }
        }
    } else {
        if !matches!(
            env.ext.get::<crate::Win32>().codec.acp,
            932 | 936 | 1252 | 65001
        ) {
            return unsupported(
                env,
                errno,
                api,
                "ANSI access paths require guest ACP 932/936/1252/65001",
            );
        }
        let bytes = match AnsiStr(GuestPtr::new(pointer)).read(env.cpu, 32768) {
            Ok(bytes) => bytes,
            Err(error) => return memory_failure(error),
        };
        env.ext.get::<crate::Win32>().codec.decode(&bytes)
    };
    if path.is_empty() {
        crate::util::set_last_error(env, 3);
        return fail(env, errno, 2, 3);
    }
    crate::kernel32::file::file_attributes_path_then(
        env,
        &path,
        api,
        Box::new(move |env, result| match result {
            Ok(attributes) if mode & 2 != 0 && attributes & 0x11 == 1 => {
                // Read-only denial sets the CRT error pair, preserving LastError.
                fail(env, errno, 13, 5)
            }
            Ok(_) => Outcome::Return(0),
            Err(dos) => fail(env, errno, shared_file::dos_errno(dos), dos),
        }),
    )
}

#[win32(msvcrt, name = "_access", level = implement)]
fn access_a(env: &mut Env<'_>, path: AnsiStr, mode: i32) -> Outcome {
    access(env, path.0.addr(), mode, false)
}

#[win32(msvcrt, name = "_waccess", level = implement)]
fn access_w(env: &mut Env<'_>, path: WideStr, mode: i32) -> Outcome {
    access(env, path.0.addr(), mode, true)
}

#[cfg(test)]
mod tests;
