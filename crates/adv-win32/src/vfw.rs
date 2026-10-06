//! The personality has no Video Compression Manager driver-install backend.
//! Declarations come from the scoped, pinned SDK input in data/abi/sdk-source.json.
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, win32};
use adv_machine::Env;

use crate::util::{MAX_STR, err, set_last_error};

fn unavailable(env: &mut Env<'_>, api: &str) -> i32 {
    adv_abi::warn_unavailable(
        env,
        api,
        "VCM compressor driver installation is unavailable",
    );
    set_last_error(env, 120);
    0
}

#[win32(msvfw32, name = "ICInstall", level = safe_fail)]
fn install(
    env: &mut Env<'_>,
    _kind: u32,
    _handler: u32,
    parameter: i32,
    _description: AnsiStr,
    flags: u32,
) -> i32 {
    let pointer = parameter as u32;
    if pointer == 0 || !matches!(flags, 1 | 2 | 4 | 0x8002) {
        set_last_error(env, err::INVALID_PARAMETER);
        return 0;
    }
    let readable = match flags {
        2 => AnsiStr(GuestPtr::new(pointer))
            .read(env.cpu, MAX_STR)
            .is_ok(),
        0x8002 => WideStr(GuestPtr::new(pointer))
            .read(env.cpu, MAX_STR)
            .is_ok(),
        _ => true,
    };
    if !readable {
        set_last_error(env, 998);
        return 0;
    }
    unavailable(env, "msvfw32.dll!ICInstall")
}

#[win32(msvfw32, name = "ICRemove", level = safe_fail)]
fn remove(env: &mut Env<'_>, _kind: u32, _handler: u32, flags: u32) -> i32 {
    if flags != 0 {
        set_last_error(env, err::INVALID_PARAMETER);
        return 0;
    }
    unavailable(env, "msvfw32.dll!ICRemove")
}

pub fn register(registry: &mut Registry) {
    registry.register(install_ABI);
    registry.register(remove_ABI);
}
