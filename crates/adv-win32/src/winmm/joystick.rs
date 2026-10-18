//! The same no-configured-controller personality as DirectInput GAMECTRL.
//! Driver slots are supported IDs, not present devices. No host enumeration,
//! sampling, capture timer, worker or independent device owner is created.
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::Prot;
use adv_machine::Env;

const DRIVER_SLOTS: u32 = 16;
const NODRIVER: u32 = 6;
const INVALPARAM: u32 = 11;
const PARMS: u32 = 165;
const REG_KEY: &[u8] = b"DINPUT.DLL\0";

pub(super) fn register(registry: &mut Registry) {
    registry.register(number_of_devices_ABI);
    registry.register(capabilities_a_ABI);
    registry.register(capabilities_w_ABI);
    registry.register(position_ABI);
    registry.register(position_extended_ABI);
    registry.register(set_capture_ABI);
    registry.register(release_capture_ABI);
}

/// Native no-controller Windows supports sixteen driver slots while every
/// configured-device query fails. This is not a count of attached controllers.
#[win32(winmm, name = "joyGetNumDevs", level = state)]
fn number_of_devices(_env: &mut Env<'_>) -> u32 {
    DRIVER_SLOTS
}

#[win32(winmm, name = "joyGetDevCapsA", level = state)]
fn capabilities_a(env: &mut Env<'_>, id: u32, output: GuestPtr<()>, size: u32) -> u32 {
    capabilities(env, id, output, size, false)
}

#[win32(winmm, name = "joyGetDevCapsW", level = state)]
fn capabilities_w(env: &mut Env<'_>, id: u32, output: GuestPtr<()>, size: u32) -> u32 {
    capabilities(env, id, output, size, true)
}

fn capabilities(env: &mut Env<'_>, id: u32, output: GuestPtr<()>, size: u32, wide: bool) -> u32 {
    // SDK JOYCAPS[A/W] and JOYCAPS2[A/W]. The latter appends three GUIDs.
    let (sizes, key_offset) = if wide {
        ([728, 776], 144)
    } else {
        ([404, 452], 112)
    };
    if output.is_null() {
        return INVALPARAM;
    }
    if !sizes.contains(&size) {
        return PARMS;
    }
    // The ANSI wrapper leaves an invalid ID's output untouched; its Unicode
    // wrapper clears the structure before returning the same driver failure.
    if !wide && id >= DRIVER_SLOTS && id != u32::MAX {
        return NODRIVER;
    }
    if (wide || id != u32::MAX)
        && (!env.cpu.accessible(output.addr(), size, Prot::WRITE)
            || env
                .cpu
                .write(output.addr(), &[0; 776][..size as usize])
                .is_err())
    {
        return INVALPARAM;
    }
    if id == u32::MAX {
        // ID -1 queries only the driver's registry key, even without devices.
        // ANSI writes only this member; Unicode already cleared its full output.
        let Some(address) = output.addr().checked_add(key_offset) else {
            return INVALPARAM;
        };
        let mut key = [0; 22];
        let bytes = if wide {
            for (index, &byte) in REG_KEY.iter().enumerate() {
                key[index * 2] = byte;
            }
            &key[..]
        } else {
            REG_KEY
        };
        if !env.cpu.accessible(address, bytes.len() as u32, Prot::WRITE)
            || env.cpu.write(address, bytes).is_err()
        {
            return INVALPARAM;
        }
        return 0;
    }
    if id < DRIVER_SLOTS { PARMS } else { NODRIVER }
}

/// Unconfigured devices have no position. Unlike configured-but-disconnected
/// devices, this profile returns JOYERR_PARMS, not JOYERR_UNPLUGGED.
#[win32(winmm, name = "joyGetPos", level = safe_fail)]
fn position(_env: &mut Env<'_>, _id: u32, output: GuestPtr<()>) -> u32 {
    if output.is_null() { INVALPARAM } else { PARMS }
}

/// The absent configured-device lookup precedes JOYINFOEX validation on this
/// native profile, including NULL output and arbitrary dwSize/dwFlags.
#[win32(winmm, name = "joyGetPosEx", level = safe_fail)]
fn position_extended(_env: &mut Env<'_>, _id: u32, _output: GuestPtr<()>) -> u32 {
    PARMS
}

#[win32(winmm, name = "joySetCapture", level = safe_fail)]
fn set_capture(env: &mut Env<'_>, window: Handle, id: u32, _period: u32, _changed: i32) -> u32 {
    if id >= DRIVER_SLOTS
        || window.0 == 0
        || !crate::user32::user(env)
            .windows
            .contains_key(&env.sys.objects, &window.0)
    {
        PARMS
    } else {
        NODRIVER
    }
}

/// No joystick is configured or captured. Native returns INVALPARAM for a
/// supported but unconfigured slot and PARMS outside the driver's ID range.
#[win32(winmm, name = "joyReleaseCapture", level = safe_fail)]
fn release_capture(_env: &mut Env<'_>, id: u32) -> u32 {
    if id < DRIVER_SLOTS { INVALPARAM } else { PARMS }
}
