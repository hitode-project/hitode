//! Single-byte I/O through the existing process-owned CRT FILE/VFS owner.
//! Only explicit binary streams are supported; no CRLF/orientation state,
//! pseudo standard FILE or independently owned output handle is introduced.
use super::{CrtState, Env, Outcome, Registry, file, floating, invalid_parameter_result};
use adv_abi::{
    Api, GuestPtr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{GuestExt, Prot};

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

fn unsupported(env: &mut Env<'_>, reason: &'static str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    Outcome::Fail(format!("{api}: {reason}"))
}

fn io_failure(env: &mut Env<'_>, status: u32) -> Outcome {
    if status == 0xc000_0022 {
        return match floating::errno_address(env).write(env.cpu, 9) {
            Ok(()) => Outcome::Return(u32::MAX),
            Err(fault) => memory_failure(fault),
        };
    }
    file::error(env, status, u32::MAX)
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
    // Use the existing guest FILE storage for the output byte.
    if let Err(fault) = env.cpu.write(stream.addr(), &[byte]) {
        return memory_failure(fault);
    }
    file::transfer(
        env,
        stream.addr(),
        stream.addr(),
        1,
        true,
        Box::new(move |env, result| match result {
            Ok(1) => Outcome::Return(u32::from(byte)),
            Ok(_) => Outcome::Return(u32::MAX),
            Err(status) => io_failure(env, status),
        }),
    )
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
    file::transfer(
        env,
        stream,
        stream,
        1,
        false,
        Box::new(move |env, result| match result {
            Ok(0) => Outcome::Return(u32::MAX),
            Ok(_) => match env.cpu.read_u8(stream) {
                Ok(byte) => Outcome::Return(u32::from(byte)),
                Err(fault) => memory_failure(fault),
            },
            Err(status) => io_failure(env, status),
        }),
    )
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
