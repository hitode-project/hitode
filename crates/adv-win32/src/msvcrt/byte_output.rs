//! Single-byte I/O through the existing process-owned CRT FILE/VFS owner.
//! Only explicit binary streams are supported; no CRLF/orientation state,
//! pseudo standard FILE or independently owned output handle is introduced.
use super::{CrtState, Env, Outcome, Registry, floating, invalid_parameter_result};
use adv_abi::{
    Api, GuestPtr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::Prot;
use std::io::{Read, Write};

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

fn unsupported(env: &mut Env<'_>, reason: &'static str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    Outcome::Fail(format!("{api}: {reason}"))
}

fn io_failure(env: &mut Env<'_>, error: std::io::Error) -> Outcome {
    // Native rb fputc fails with EBADF, preserving DOS errno/LastError and
    // invoking no invalid handler. The owned File rejects that byte write
    // with access denied (Windows) or EBADF (portable hosts).
    let access = error.kind() == std::io::ErrorKind::PermissionDenied;
    #[cfg(not(windows))]
    let access = access || error.raw_os_error() == Some(9);
    let cell = floating::errno_address(env);
    if access {
        return match cell.write(env.cpu, 9) {
            Ok(()) => Outcome::Return(u32::MAX),
            Err(error) => memory_failure(error),
        };
    }
    #[cfg(windows)]
    let dos = error.raw_os_error().map(|code| code as u32);
    #[cfg(not(windows))]
    let dos: Option<u32> = None;
    let dos = dos.unwrap_or(match error.kind() {
        std::io::ErrorKind::OutOfMemory => 8,
        std::io::ErrorKind::StorageFull => 112,
        std::io::ErrorKind::BrokenPipe => 109,
        std::io::ErrorKind::InvalidInput => 87,
        _ => 1117,
    });
    let errno: i32 = match dos {
        6 => 9,
        8 | 14 => 12,
        112 => 28,
        109 => 32,
        87 => 22,
        _ => 5,
    };
    if let Err(error) = check_range(env.cpu, cell.addr(), 8, Prot::WRITE) {
        return memory_failure(error);
    }
    let mut pair = [0; 8];
    pair[..4].copy_from_slice(&errno.to_le_bytes());
    pair[4..].copy_from_slice(&dos.to_le_bytes());
    match env.cpu.write(cell.addr(), &pair) {
        Ok(()) => {
            crate::util::set_last_error(env, dos);
            Outcome::Return(u32::MAX)
        }
        Err(error) => memory_failure(error),
    }
}

fn output(env: &mut Env<'_>, character: i32, stream: GuestPtr<u8>) -> Outcome {
    if stream.is_null() {
        if env.sys.thunks.get(env.thunk).module == "msvcrt.dll" {
            return unsupported(
                env,
                "legacy NULL FILE invalid-parameter handling is unavailable",
            );
        }
        crate::util::set_last_error(env, 0);
        let name = env.sys.thunks.get(env.thunk).name.clone();
        return invalid_parameter_result(env, &name, 22, u32::MAX);
    }
    let binary = env
        .ext
        .try_get::<CrtState>()
        .and_then(|state| state.files.get(&stream.addr()))
        .map(|file| file.binary);
    match binary {
        None => {
            return unsupported(
                env,
                "unknown, closed or pseudo standard FILE byte output is unavailable",
            );
        }
        Some(false) => {
            return unsupported(
                env,
                "text or Unicode FILE conversion/orientation is unavailable; explicit binary mode is required",
            );
        }
        Some(true) => {}
    }
    // The shared CRT open owner publishes a 32-byte guest FILE allocation.
    // Check the complete mutable object before advancing the underlying file.
    if let Err(error) = check_range(env.cpu, stream.addr(), 32, Prot::RW) {
        return memory_failure(error);
    }
    let byte = character as u8;
    let file = env
        .ext
        .get_mut::<CrtState>()
        .files
        .get_mut(&stream.addr())
        .expect("validated existing FILE owner, without an intervening guest call");
    match file.write_all(&[byte]) {
        Ok(()) => Outcome::Return(u32::from(byte)),
        Err(error) => io_failure(env, error),
    }
}

#[win32(msvcrt, name = "fputc", level = implement)]
fn put_character(env: &mut Env<'_>, character: i32, stream: GuestPtr<u8>) -> Outcome {
    output(env, character, stream)
}

#[win32(msvcrt, name = "putc", level = implement)]
fn put_character_alias(env: &mut Env<'_>, character: i32, stream: GuestPtr<u8>) -> Outcome {
    output(env, character, stream)
}

fn input(env: &mut Env<'_>) -> Outcome {
    let stream = env.arg(0);
    if stream == 0 {
        if env.sys.thunks.get(env.thunk).module == "msvcrt.dll" {
            return unsupported(
                env,
                "legacy NULL FILE invalid-parameter handling is unavailable",
            );
        }
        crate::util::set_last_error(env, 0);
        let name = env.sys.thunks.get(env.thunk).name.clone();
        return invalid_parameter_result(env, &name, 22, u32::MAX);
    }
    let binary = env
        .ext
        .try_get::<CrtState>()
        .and_then(|s| s.files.get(&stream))
        .map(|f| f.binary);
    if binary != Some(true) {
        return unsupported(env, "byte input requires an owned explicit binary FILE");
    }
    if let Err(error) = check_range(env.cpu, stream, 32, Prot::RW) {
        return memory_failure(error);
    }
    let mut byte = [0u8];
    let result = env
        .ext
        .get_mut::<CrtState>()
        .files
        .get_mut(&stream)
        .unwrap()
        .read(&mut byte);
    match result {
        Ok(0) => Outcome::Return(u32::MAX),
        Ok(_) => Outcome::Return(u32::from(byte[0])),
        Err(error) => io_failure(env, error),
    }
}

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        // These legacy CRT exports are outside the pinned SDK projection.
        for name in ["fgetc", "getc"] {
            registry.register(Api {
                module,
                name,
                conv: adv_abi::Conv::Cdecl { argc: 1 },
                handler: input,
                level: adv_abi::ImplementationLevel::Implement,
            });
        }
        for api in [put_character_ABI, put_character_alias_ABI] {
            registry.register(Api { module, ..api });
        }
    }
}

#[cfg(test)]
mod tests;
