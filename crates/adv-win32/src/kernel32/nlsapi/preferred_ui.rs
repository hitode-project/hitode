//! MUI queries over the existing guest language profile, independent of the
//! host's language settings. No explicit process/thread preference is installed
//! while the MUI setters remain unavailable.
use adv_abi::{GuestPtr, Registry, WideStr, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;

use crate::util::{err, set_last_error};

const LANGUAGE_ID: u32 = 4;
const LANGUAGE_NAME: u32 = 8;
const SYSTEM_FALLBACK: u32 = 0x10;
const USER_FALLBACK: u32 = 0x20;
const THREAD_LANGUAGES: u32 = 0x40;
const MACHINE_SETTINGS: u32 = 0x400;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Thread,
    Process,
    User,
    System,
}
impl Scope {
    fn api(self) -> &'static str {
        match self {
            Self::Thread => "kernel32.dll!GetThreadPreferredUILanguages",
            Self::Process => "kernel32.dll!GetProcessPreferredUILanguages",
            Self::User => "kernel32.dll!GetUserPreferredUILanguages",
            Self::System => "kernel32.dll!GetSystemPreferredUILanguages",
        }
    }
}
fn fail(env: &mut Env<'_>, error: u32) -> i32 {
    set_last_error(env, error);
    0
}

// https://learn.microsoft.com/windows/win32/api/winnls/nf-winnls-getthreadpreferreduilanguages
fn query(
    env: &mut Env<'_>,
    scope: Scope,
    flags: u32,
    count: GuestPtr<u32>,
    buffer: WideStr,
    size: GuestPtr<u32>,
) -> i32 {
    let format = flags & (LANGUAGE_ID | LANGUAGE_NAME);
    if format == LANGUAGE_ID | LANGUAGE_NAME {
        return fail(env, err::INVALID_PARAMETER);
    }
    let allowed = LANGUAGE_ID
        | LANGUAGE_NAME
        | match scope {
            Scope::Thread => SYSTEM_FALLBACK | USER_FALLBACK | THREAD_LANGUAGES,
            Scope::System => MACHINE_SETTINGS,
            _ => 0,
        };
    if flags & !allowed != 0 {
        warn_unavailable(
            env,
            scope.api(),
            "preferred UI language flags are not modeled",
        );
        return fail(env, err::INVALID_PARAMETER);
    }
    if scope == Scope::Thread {
        if flags & THREAD_LANGUAGES != 0 && flags & (USER_FALLBACK | SYSTEM_FALLBACK) != 0
            || flags & SYSTEM_FALLBACK != 0 && flags & USER_FALLBACK == 0
        {
            return fail(env, err::INVALID_PARAMETER);
        }
        if flags & SYSTEM_FALLBACK != 0 {
            warn_unavailable(
                env,
                scope.api(),
                "resource-loader neutral UI language fallbacks are not modeled",
            );
            return fail(env, 50);
        }
    }
    if count.addr() == 0 || size.addr() == 0 {
        return fail(env, err::INVALID_PARAMETER);
    }
    if check_range(env.cpu, count.addr(), 4, Prot::WRITE).is_err()
        || check_range(env.cpu, size.addr(), 4, Prot::RW).is_err()
    {
        return fail(env, 998);
    }
    let capacity = match env.cpu.read_u32(size.addr()) {
        Ok(value) => value,
        Err(_) => return fail(env, 998),
    };
    let buffer = buffer.0.addr();
    // Native MUI rejects NULL/nonzero capacity as an invalid parameter.
    if buffer == 0 && capacity != 0 {
        return fail(env, err::INVALID_PARAMETER);
    }
    let empty = scope == Scope::Process || scope == Scope::Thread && flags & THREAD_LANGUAGES != 0;
    let mut units = if empty {
        vec![0]
    } else {
        let lang = super::lcid(env);
        let name = if format == LANGUAGE_ID {
            format!("{lang:04X}")
        } else {
            match lang {
                0x804 => "zh-CN".into(),
                _ => "ja-JP".into(),
            }
        };
        name.encode_utf16().chain([0]).collect::<Vec<_>>()
    };
    units.push(0);
    let required = units.len() as u32;
    let languages = u32::from(!empty);
    let probe = buffer == 0 && capacity == 0;
    if !probe && capacity < required {
        // Insufficient-buffer failure publishes required UTF-16 capacity.
        // Thread queries also publish their count; user/system and the empty
        // process query leave count alone, as the native no-UI probe confirms.
        if scope == Scope::Thread && env.cpu.write_u32(count.addr(), languages).is_err()
            || env.cpu.write_u32(size.addr(), required).is_err()
        {
            return fail(env, 998);
        }
        if capacity >= 2 {
            if check_range(env.cpu, buffer, 4, Prot::WRITE).is_err()
                || env.cpu.write(buffer, &[0; 4]).is_err()
            {
                return fail(env, 998);
            }
        }
        return fail(env, err::INSUFFICIENT_BUFFER);
    }
    if !probe {
        let bytes: Vec<_> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
        if check_range(env.cpu, buffer, bytes.len() as u32, Prot::WRITE).is_err()
            || env.cpu.write(buffer, &bytes).is_err()
        {
            return fail(env, 998);
        }
    }
    // With no process list installed, Windows writes the empty MULTI_SZ but
    // leaves pulNumLanguages unchanged, including for a size-only query.
    if scope != Scope::Process && env.cpu.write_u32(count.addr(), languages).is_err()
        || env.cpu.write_u32(size.addr(), required).is_err()
    {
        return fail(env, 998);
    }
    1
}

#[win32(kernel32, name = "GetThreadPreferredUILanguages", level = state)]
fn thread(
    env: &mut Env<'_>,
    flags: u32,
    count: GuestPtr<u32>,
    buffer: WideStr,
    size: GuestPtr<u32>,
) -> i32 {
    query(env, Scope::Thread, flags, count, buffer, size)
}
#[win32(kernel32, name = "GetProcessPreferredUILanguages", level = state)]
fn process(
    env: &mut Env<'_>,
    flags: u32,
    count: GuestPtr<u32>,
    buffer: WideStr,
    size: GuestPtr<u32>,
) -> i32 {
    query(env, Scope::Process, flags, count, buffer, size)
}
#[win32(kernel32, name = "GetUserPreferredUILanguages", level = state)]
fn user(
    env: &mut Env<'_>,
    flags: u32,
    count: GuestPtr<u32>,
    buffer: WideStr,
    size: GuestPtr<u32>,
) -> i32 {
    query(env, Scope::User, flags, count, buffer, size)
}
#[win32(kernel32, name = "GetSystemPreferredUILanguages", level = state)]
fn system(
    env: &mut Env<'_>,
    flags: u32,
    count: GuestPtr<u32>,
    buffer: WideStr,
    size: GuestPtr<u32>,
) -> i32 {
    query(env, Scope::System, flags, count, buffer, size)
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(thread_ABI);
    registry.register(process_ABI);
    registry.register(user_ABI);
    registry.register(system_ABI);
}

#[cfg(test)]
mod tests;
