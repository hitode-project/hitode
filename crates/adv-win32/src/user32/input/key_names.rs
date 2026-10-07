//! Shortcut labels borrow the common guest keyboard profile and process codec.
use super::user;
use crate::{Win32, util::set_last_error};
use adv_abi::{AnsiStr, Env, Registry, WideStr, win32};
use adv_cpu::backend::Prot;

pub(super) fn register(registry: &mut Registry) {
    registry.register(get_a_ABI);
    registry.register(get_w_ABI);
}
fn name(env: &mut Env<'_>, parameter: i32, api: &str) -> Option<String> {
    let value = adv_os::keyboard::key_name(user(env).keyboard_layout, parameter as u32);
    if value.is_none() {
        adv_abi::warn_unavailable(
            env,
            api,
            "key names for this keyboard profile are unavailable",
        );
        set_last_error(env, 50);
    }
    value
}

#[win32(user32, name = "GetKeyNameTextA", level = state)]
fn get_a(env: &mut Env<'_>, parameter: i32, output: AnsiStr, capacity: i32) -> i32 {
    if capacity < 0 {
        set_last_error(env, 8);
        return 0;
    }
    let Some(name) = name(env, parameter, "user32.dll!GetKeyNameTextA") else {
        return 0;
    };
    let encoded = env.ext.get::<Win32>().codec.encode(&name);
    let length = encoded.len().min((capacity as usize).saturating_sub(1));
    let mut bytes = if capacity == 0 {
        Vec::new()
    } else {
        encoded[..length].to_vec()
    };
    // The native ANSI wrapper writes an empty string even for zero capacity or
    // an unknown scan. Invalid guest outputs fail safely instead of host faults.
    bytes.push(0);
    if adv_abi::guest::check_range(env.cpu, output.0.addr(), bytes.len() as u32, Prot::WRITE)
        .is_err()
        || env.cpu.write(output.0.addr(), &bytes).is_err()
    {
        set_last_error(env, 998);
        return 0;
    }
    if capacity == 0 { 0 } else { length as i32 }
}

#[win32(user32, name = "GetKeyNameTextW", level = state)]
fn get_w(env: &mut Env<'_>, parameter: i32, output: WideStr, capacity: i32) -> i32 {
    if capacity == 0 {
        return 0;
    }
    if capacity < 0
        || adv_abi::guest::check_range(
            env.cpu,
            output.0.addr(),
            (capacity as u32).saturating_mul(2),
            Prot::WRITE,
        )
        .is_err()
    {
        set_last_error(env, 998);
        return 0;
    }
    let Some(name) = name(env, parameter, "user32.dll!GetKeyNameTextW") else {
        return 0;
    };
    if name.is_empty() {
        return 0;
    }
    let units: Vec<_> = name.encode_utf16().take(capacity as usize - 1).collect();
    let length = units.len();
    let bytes: Vec<_> = units
        .into_iter()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    if env.cpu.write(output.0.addr(), &bytes).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    length as i32
}

#[cfg(test)]
mod tests;
