//! Owned process and impersonation tokens for the ordinary, non-elevated guest user.
//! Token handles use Objects ownership; no host account/token is acquired.
use std::{cell::RefCell, collections::BTreeMap, rc::Rc, sync::Arc};
mod permissions;

use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, process::ProcessIdentity};

use crate::{kernel32::child_process::process_target, util::set_last_error};

const QUERY: u32 = 8;
const READ_CONTROL: u32 = 0x20000;
const SUPPORTED_ACCESS: u32 = 0xf01ff;
#[derive(Clone)]
struct TokenData {
    kind: u32,
    level: u32,
    privileges: BTreeMap<u64, u32>,
}
impl Default for TokenData {
    fn default() -> Self {
        Self {
            kind: 1,
            level: 2,
            privileges: BTreeMap::from([(19, 0), (23, 3), (25, 0), (33, 0), (34, 0)]),
        }
    }
}
#[derive(Default)]
struct TokenOwner {
    primary: Rc<RefCell<TokenData>>,
    threads: BTreeMap<
        adv_machine::CtxId,
        (
            std::sync::Weak<adv_machine::process::Completion>,
            Rc<RefCell<TokenData>>,
        ),
    >,
}

struct Token {
    _process: Arc<ProcessIdentity>,
    access: u32,
    data: Rc<RefCell<TokenData>>,
}
fn fail(env: &mut Env<'_>, error: u32) -> i32 {
    set_last_error(env, error);
    0
}
fn access(mut requested: u32) -> Option<u32> {
    for (generic, specific) in [
        (0x80000000, READ_CONTROL | QUERY),
        (0x40000000, READ_CONTROL | 0xe0),
        (0x20000000, READ_CONTROL),
        (0x10000000, SUPPORTED_ACCESS),
        (0x02000000, SUPPORTED_ACCESS),
    ] {
        if requested & generic != 0 {
            requested = (requested & !generic) | specific;
        }
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
        return fail(env, if desired & 0x01000000 != 0 { 1314 } else { 5 });
    };
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return fail(env, 998);
    }
    let data = env.ext.get_or_default::<TokenOwner>().primary.clone();
    let handle = env.sys.objects.insert_host(Token {
        _process: identity,
        access,
        data,
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
    if class == 29 {
        return fail(env, 87);
    }
    if !matches!(class, 1 | 3 | 8 | 9 | 18 | 20 | 21 | 23 | 24 | 26) {
        warn_unavailable(
            env,
            "advapi32.dll!GetTokenInformation",
            &format!("token information class {class} is unavailable"),
        );
        return fail(env, 50);
    }
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
    let data = token.data.borrow().clone();
    let value = match class {
        8 => data.kind,
        9 if data.kind == 1 => return fail(env, 1349),
        9 => data.level,
        18 => 1,
        _ => 0,
    }; // ERROR_BAD_TOKEN_TYPE
    if check_range(env.cpu, required.addr(), 4, Prot::WRITE).is_err() {
        return fail(env, 998);
    }
    let length = match class {
        1 => 8 + adv_os::identity::current_user_sid().len() as u32,
        3 => 4 + 12 * data.privileges.len() as u32,
        _ => 4,
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
    } else if class == 3 {
        bytes[..4].copy_from_slice(&(data.privileges.len() as u32).to_le_bytes());
        for (row, (&luid, &attrs)) in bytes[4..]
            .as_chunks_mut::<12>()
            .0
            .iter_mut()
            .zip(data.privileges.iter())
        {
            row[..8].copy_from_slice(&luid.to_le_bytes());
            row[8..].copy_from_slice(&attrs.to_le_bytes());
        }
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
        data: token.data.clone(),
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
    permissions::register(registry);
    registry.register(open_ABI);
    registry.register(information_ABI);
    registry.register(sid_text_a_ABI);
    registry.register(sid_text_w_ABI);
}

#[cfg(test)]
mod tests;
