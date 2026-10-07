//! Process identity shares Win32's configured virtual owner, independently of
//! the mutable process environment. No host account or impersonation provider.
use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};
use adv_os::nls::{CP_UTF8, lead_byte_ranges};

use crate::{Win32, util::set_last_error};

const NOACCESS: u32 = 998;
const INSUFFICIENT_BUFFER: u32 = 122;
const NOT_SUPPORTED: u32 = 50;
const UNLEN: usize = 256;

pub(super) fn register(registry: &mut Registry) {
    registry.register(user_name_a_ABI);
    registry.register(user_name_w_ABI);
    registry.register(open_process_token_ABI);
}

#[win32(advapi32, name = "GetUserNameA", level = implement)]
fn user_name_a(env: &mut Env<'_>, buffer: AnsiStr, size: GuestPtr<u32>) -> Outcome {
    query(
        env,
        buffer.0.addr(),
        size,
        false,
        "advapi32.dll!GetUserNameA",
    )
}

#[win32(advapi32, name = "GetUserNameW", level = implement)]
fn user_name_w(env: &mut Env<'_>, buffer: WideStr, size: GuestPtr<u32>) -> Outcome {
    query(
        env,
        buffer.0.addr(),
        size,
        true,
        "advapi32.dll!GetUserNameW",
    )
}

fn failure(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}

fn query(env: &mut Env<'_>, buffer: u32, size: GuestPtr<u32>, wide: bool, api: &str) -> Outcome {
    // Native can raise an access violation for invalid pointers. This frontend
    // follows the repository's checked-pointer contract: false + ERROR_NOACCESS.
    let Ok(capacity) = size.read(env.cpu) else {
        return failure(env, NOACCESS);
    };
    if check_range(env.cpu, size.addr(), 4, Prot::WRITE).is_err() {
        return failure(env, NOACCESS);
    }
    let owner = env.ext.get::<Win32>();
    let units: Vec<_> = owner.cfg.os.user_name.encode_utf16().collect();
    if units.is_empty() || units.len() > UNLEN || units.contains(&0) {
        adv_abi::warn_unavailable(
            env,
            api,
            "virtual user identity must be nonempty, NUL-free, and at most 256 UTF-16 units",
        );
        return failure(env, NOT_SUPPORTED);
    }
    let codepage = owner.codec.acp;
    // Codec only has real encoders for these profiles. Some other NLS queries
    // recognize CP437/ASCII, but their fallback is not a faithful name encoder.
    if !wide && !matches!(codepage, 932 | 936 | 1252 | CP_UTF8) {
        adv_abi::warn_unavailable(
            env,
            api,
            "virtual user identity ANSI code page is unsupported",
        );
        return failure(env, NOT_SUPPORTED);
    }
    let mut bytes: Vec<u8> = if wide {
        units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
    } else {
        owner.codec.from_utf16(0, &units).0
    };
    bytes.push(0);
    if wide {
        bytes.push(0);
    }
    let written = bytes.len() as u32 / if wide { 2 } else { 1 };
    if capacity < written {
        // The x86 ACP936 oracle confirms that ANSI insufficient-buffer queries
        // return (UTF16 length + NUL) * MaxCharSize, while capacity equal to the
        // actual encoded bytes succeeds. Other ACP estimates are derived from
        // the guest GetCPInfo MaxCharSize profile, not host account/codepage data.
        let max_char_size =
            if wide || (lead_byte_ranges(codepage).is_empty() && codepage != CP_UTF8) {
                1
            } else if codepage == CP_UTF8 {
                4
            } else {
                2
            };
        let required = (units.len() as u32 + 1) * max_char_size;
        if size.write(env.cpu, required).is_err() {
            return failure(env, NOACCESS);
        }
        return failure(env, INSUFFICIENT_BUFFER);
    }
    // Validate both outputs before publishing anything. A short-capacity query
    // deliberately never checks or modifies buffer, including a NULL buffer.
    if check_range(env.cpu, buffer, bytes.len() as u32, Prot::WRITE).is_err()
        || env.cpu.write(buffer, &bytes).is_err()
        || size.write(env.cpu, written).is_err()
    {
        return failure(env, NOACCESS);
    }
    // Successful native calls do not promise a particular LastError value.
    // This virtual provider performs no account-service call, so preserves it.
    Outcome::Return(1)
}

// Process tokens are deliberately unavailable in the current identity profile.
// The BOOL failure contract leaves the output untouched and exposes a reason;
// callers can take their non-token fallback without acquiring a host token.
// https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-openprocesstoken
#[win32(advapi32, name = "OpenProcessToken", level = safe_fail)]
fn open_process_token(
    env: &mut Env<'_>,
    _process: Handle,
    _desired_access: u32,
    _token: GuestPtr<Handle>,
) -> i32 {
    adv_abi::warn_unavailable(
        env,
        "advapi32.dll!OpenProcessToken",
        "process security tokens are unavailable in the virtual identity profile",
    );
    set_last_error(env, NOT_SUPPORTED);
    0
}
