//! DISPLAY_DEVICE A/W projections of the same virtual adapter and monitor.
use super::{PRIMARY_DEVICE_NAME, fail};
use adv_abi::{AnsiStr, Env, GuestPtr, Registry, WideStr, win32};
use adv_cpu::backend::Prot;

pub(super) fn register(registry: &mut Registry) {
    registry.register(devices_a_ABI);
    registry.register(devices_w_ABI);
}

fn field(bytes: &mut [u8], offset: usize, value: &str, wide: bool) {
    if wide {
        for (index, unit) in value.encode_utf16().enumerate() {
            bytes[offset + index * 2..offset + index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
    } else {
        bytes[offset..offset + value.len()].copy_from_slice(value.as_bytes());
    }
}

fn devices(
    env: &mut Env<'_>,
    api: &str,
    parent: Option<String>,
    index: u32,
    output: GuestPtr<()>,
    flags: u32,
    wide: bool,
) -> i32 {
    let Ok(cb) = GuestPtr::<u32>::new(output.addr()).read(env.cpu) else {
        return fail(env, 87);
    };
    let (size, description, state_flags) = if wide { (840, 68, 324) } else { (424, 36, 164) };
    // Windows also accepts shorter structures and copies precisely cb bytes.
    // Oversized cb is outside the SDK contract: never copy beyond our layout.
    if cb > size || adv_abi::guest::check_range(env.cpu, output.addr(), cb, Prot::WRITE).is_err() {
        return fail(env, 87);
    }
    if flags & !1 != 0 {
        adv_abi::warn_unavailable(
            env,
            api,
            "display enumeration flags other than EDD_GET_DEVICE_INTERFACE_NAME are unavailable",
        );
        return fail(env, 50);
    }
    let child = parent.is_some();
    let found = index == 0
        && parent
            .as_deref()
            .is_none_or(|name| name.eq_ignore_ascii_case(PRIMARY_DEVICE_NAME));
    if child && found && flags & 1 != 0 {
        // DeviceID for this flag must be an owned SetupAPI monitor interface.
        // A synthetic hardware path would not identify a registered device.
        adv_abi::warn_unavailable(
            env,
            api,
            "registered monitor device interface paths are unavailable",
        );
        return fail(env, 50);
    }

    let mut bytes = [0u8; 840];
    bytes[..4].copy_from_slice(&cb.to_le_bytes());
    if found {
        let (name, label, state) = if child {
            (r"\\.\DISPLAY1\Monitor0", "adv-host virtual monitor", 3u32)
        } else {
            (
                PRIMARY_DEVICE_NAME,
                "adv-host virtual display adapter",
                5u32,
            )
        };
        field(&mut bytes, 4, name, wide);
        field(&mut bytes, description, label, wide);
        bytes[state_flags..state_flags + 4].copy_from_slice(&state.to_le_bytes());
        // DeviceID is unused without interface-name enumeration; DeviceKey is
        // reserved. Keep both empty rather than exposing host hardware identity.
    }
    // Exhaustion clears the requested output prefix, retains cb and LastError.
    if env.cpu.write(output.addr(), &bytes[..cb as usize]).is_err() {
        return fail(env, 87);
    }
    i32::from(found)
}

#[win32(user32, name = "EnumDisplayDevicesA", level = state)]
fn devices_a(
    env: &mut Env<'_>,
    parent: AnsiStr,
    index: u32,
    output: GuestPtr<()>,
    flags: u32,
) -> i32 {
    let parent = if parent.0.is_null() {
        None
    } else {
        let Ok(bytes) = parent.read(env.cpu, 4096) else {
            return fail(env, 87);
        };
        Some(env.ext.get::<crate::Win32>().codec.decode(&bytes))
    };
    devices(
        env,
        "user32.dll!EnumDisplayDevicesA",
        parent,
        index,
        output,
        flags,
        false,
    )
}

#[win32(user32, name = "EnumDisplayDevicesW", level = state)]
fn devices_w(
    env: &mut Env<'_>,
    parent: WideStr,
    index: u32,
    output: GuestPtr<()>,
    flags: u32,
) -> i32 {
    let parent = if parent.0.is_null() {
        None
    } else {
        let Ok(units) = parent.read(env.cpu, 4096) else {
            return fail(env, 87);
        };
        Some(String::from_utf16_lossy(&units))
    };
    devices(
        env,
        "user32.dll!EnumDisplayDevicesW",
        parent,
        index,
        output,
        flags,
        true,
    )
}

#[cfg(test)]
mod tests;
