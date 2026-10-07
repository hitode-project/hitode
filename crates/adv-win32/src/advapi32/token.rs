//! Read-only primary-token queries for the ordinary, non-elevated guest user.
//! Token handles use Objects ownership; no host account/token is acquired.
use std::sync::Arc;

use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, process::ProcessIdentity};

use crate::{kernel32::child_process::process_target, util::set_last_error};

const QUERY: u32 = 8;
const READ_CONTROL: u32 = 0x20000;
const SUPPORTED_ACCESS: u32 = QUERY | READ_CONTROL;

struct Token {
    _process: Arc<ProcessIdentity>,
    access: u32,
}
fn fail(env: &mut Env<'_>, error: u32) -> i32 {
    set_last_error(env, error);
    0
}
fn access(mut requested: u32) -> Option<u32> {
    if requested & 0x80000000 != 0 {
        requested = (requested & !0x80000000) | SUPPORTED_ACCESS;
    }
    (requested & !SUPPORTED_ACCESS == 0).then_some(requested)
}

#[win32(advapi32, name = "OpenProcessToken", level = state)]
fn open(env: &mut Env<'_>, process: Handle, desired: u32, output: GuestPtr<Handle>) -> i32 {
    let identity = match process_target(env, process.0, 0x1000) {
        Ok(value) => value,
        Err(error) => return fail(env, error.code),
    };
    if identity.pid != env.sys.pid {
        warn_unavailable(
            env,
            "advapi32.dll!OpenProcessToken",
            "remote process tokens are unavailable",
        );
        return fail(env, 50);
    }
    let Some(access) = access(desired) else {
        warn_unavailable(
            env,
            "advapi32.dll!OpenProcessToken",
            "token access outside read-only queries is unavailable",
        );
        return fail(env, 50);
    };
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return fail(env, 998);
    }
    let handle = env.sys.objects.insert_host(Token {
        _process: identity,
        access,
    });
    if handle == 0 {
        return fail(env, 8);
    }
    if env.cpu.write_u32(output.addr(), handle).is_err() {
        env.sys.objects.close(handle);
        return fail(env, 998);
    }
    1
}

// https://learn.microsoft.com/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation
#[win32(advapi32, name = "GetTokenInformation", level = state)]
fn information(
    env: &mut Env<'_>,
    token: Handle,
    class: i32,
    output: GuestPtr<()>,
    capacity: u32,
    required: GuestPtr<u32>,
) -> i32 {
    let value: u32 = match class {
        1 => 0,  // TOKEN_USER is serialized below using the shared guest account SID.
        8 => 1,  // TokenPrimary
        18 => 1, // TokenElevationTypeDefault: ordinary user, no linked token.
        20 | 21 | 23 | 24 | 26 | 29 => 0, // Not elevated/filtered/virtualized/UIAccess/AppContainer.
        9 => 0, // Primary-token type is rejected after validating the handle.
        _ => {
            warn_unavailable(
                env,
                "advapi32.dll!GetTokenInformation",
                &format!("token information class {class} is unavailable"),
            );
            return fail(env, 50);
        }
    };
    // TokenElevation is a Win32 wrapper with exact-size validation before
    // token validation. Other scalar classes validate the token first.
    if class == 20 {
        if check_range(env.cpu, required.addr(), 4, Prot::WRITE).is_err()
            || required.write(env.cpu, 4).is_err()
        {
            return fail(env, 998);
        }
        if capacity != 4 {
            return fail(env, 24);
        } // ERROR_BAD_LENGTH
    }
    let Some(token) = env.sys.objects.host::<Token>(token.0) else {
        return fail(env, 6);
    };
    if token.access & QUERY == 0 {
        return fail(env, 5);
    }
    if class == 9 {
        return fail(env, 1349);
    } // ERROR_BAD_TOKEN_TYPE
    if check_range(env.cpu, required.addr(), 4, Prot::WRITE).is_err() {
        return fail(env, 998);
    }
    let length = if class == 1 {
        8 + adv_os::identity::current_user_sid().len() as u32
    } else {
        4
    };
    if capacity < length {
        let _ = required.write(env.cpu, length);
        return fail(env, 122);
    }
    if check_range(env.cpu, output.addr(), length, Prot::WRITE).is_err() {
        return fail(env, 998);
    }
    let mut bytes = vec![0; length as usize];
    if class == 1 {
        bytes[..4].copy_from_slice(&(output.addr() + 8).to_le_bytes());
        bytes[8..].copy_from_slice(&adv_os::identity::current_user_sid());
    } else {
        bytes[..4].copy_from_slice(&value.to_le_bytes());
    }
    if env.cpu.write(output.addr(), &bytes).is_err() || required.write(env.cpu, length).is_err() {
        return fail(env, 998);
    }
    1
}

/// Local DuplicateHandle with reduced rights must not silently preserve QUERY.
/// SAME_ACCESS uses the existing Objects alias path, as every other kernel object.
pub(crate) fn duplicate(env: &mut Env<'_>, source: u32, desired: u32) -> Option<Result<u32, u32>> {
    let token = env.sys.objects.host::<Token>(source)?;
    let Some(granted) = access(desired) else {
        return Some(Err(5));
    };
    if granted & !token.access != 0 {
        return Some(Err(5));
    }
    let value = Token {
        _process: token._process.clone(),
        access: granted,
    };
    let handle = env.sys.objects.insert_host(value);
    Some(if handle == 0 { Err(8) } else { Ok(handle) })
}

fn sid_text(env: &mut Env<'_>, sid: u32, output: u32, wide: bool) -> i32 {
    if check_range(env.cpu, output, 4, Prot::WRITE).is_err() {
        return fail(env, 87);
    }
    let mut header = [0u8; 8];
    if check_range(env.cpu, sid, 8, Prot::READ).is_err() || env.cpu.read(sid, &mut header).is_err()
    {
        return fail(env, 87);
    }
    if header[0] != 1 || header[1] > 15 {
        return fail(env, 1337);
    }
    let mut bytes = vec![0; 8 + 4 * header[1] as usize];
    if check_range(env.cpu, sid, bytes.len() as u32, Prot::READ).is_err()
        || env.cpu.read(sid, &mut bytes).is_err()
    {
        return fail(env, 87);
    }
    let Some(text) = adv_os::identity::sid_string(&bytes) else {
        return fail(env, 1337);
    };
    let encoded: Vec<u8> = if wide {
        text.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        text.bytes().chain([0]).collect()
    };
    let allocation = crate::kernel32::allocate_local_memory(env, 0, encoded.len() as u32);
    if allocation == 0 {
        return fail(env, 8);
    }
    if env.cpu.write(allocation, &encoded).is_err()
        || env.cpu.write_u32(output, allocation).is_err()
    {
        crate::kernel32::release_local_memory(env, allocation, false);
        return fail(env, 87);
    }
    1
}
#[win32(advapi32, name = "ConvertSidToStringSidA", level = implement)]
fn sid_text_a(env: &mut Env<'_>, sid: GuestPtr<()>, output: GuestPtr<GuestPtr<u8>>) -> i32 {
    sid_text(env, sid.addr(), output.addr(), false)
}
#[win32(advapi32, name = "ConvertSidToStringSidW", level = implement)]
fn sid_text_w(env: &mut Env<'_>, sid: GuestPtr<()>, output: GuestPtr<GuestPtr<u16>>) -> i32 {
    sid_text(env, sid.addr(), output.addr(), true)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(open_ABI);
    registry.register(information_ABI);
    registry.register(sid_text_a_ABI);
    registry.register(sid_text_w_ABI);
}

#[cfg(test)]
mod tests;
