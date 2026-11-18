//! Enumerate the installed emulated playback device as Windows does: first
//! the primary driver entry, then the device under its fixed identity, which
//! is what GetDeviceID gives the default playback aliases. The front never
//! queries host hardware.
use adv_abi::{GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::{Prot, Reg};
use adv_machine::{Env, Outcome};

use super::{INVALID, NO_DRIVER, OUT_OF_MEMORY, Sound};

const SCRATCH: u32 = 160;
const CALLBACK_STACK: u32 = 20; // Four arguments plus return address.

struct Enumeration {
    callback: u32,
    context: u32,
    scratch: u32,
    wide: bool,
    /// Entries already handed to the callback.
    delivered: usize,
}

fn start(env: &mut Env<'_>, callback: u32, context: u32, wide: bool, installed: bool) -> Outcome {
    if check_range(env.cpu, callback, 1, Prot::EXEC).is_err() {
        return Outcome::Return(INVALID);
    }
    if !installed
        || env
            .ext
            .try_get::<Sound>()
            .is_none_or(|sound| !sound.enabled)
    {
        adv_abi::warn_unavailable(
            env,
            if wide {
                "DirectSoundEnumerateW"
            } else {
                "DirectSoundEnumerateA"
            },
            "DirectSound audio is unavailable; no playback devices are enumerated",
        );
        return Outcome::Return(0);
    }
    let Some(scratch) = env
        .cpu
        .reg(Reg::Esp)
        .checked_sub(SCRATCH + 15)
        .map(|sp| sp & !15)
    else {
        return Outcome::Return(OUT_OF_MEMORY);
    };
    let Some(bottom) = scratch.checked_sub(CALLBACK_STACK) else {
        return Outcome::Return(OUT_OF_MEMORY);
    };
    if bottom < env.ctx().stack_lo
        || check_range(env.cpu, bottom, SCRATCH + CALLBACK_STACK, Prot::RW).is_err()
    {
        return Outcome::Return(OUT_OF_MEMORY);
    }
    // Caller-stack scratch needs no heap ownership or deferred cleanup if a
    // callback exits its thread. Reentrant callbacks reserve space below it.
    next(
        env,
        Enumeration {
            callback,
            context,
            scratch,
            wide,
            delivered: 0,
        },
        1,
    )
}

fn string(text: &str, wide: bool) -> Vec<u8> {
    if wide {
        text.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        // The fixed labels are ASCII, represented identically in every
        // supported guest ACP (including CP932, CP936 and UTF-8).
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        bytes
    }
}

/// The device entry's module: Windows names an endpoint by its id.
const MODULE: &str = "{0.0.0.00000000}.{2270eaf5-ed98-4c2d-ae2b-97007ff345c1}";

fn next(env: &mut Env<'_>, mut enumeration: Enumeration, keep_going: u32) -> Outcome {
    if keep_going == 0 || enumeration.delivered == 2 {
        return Outcome::Return(0); // Callback FALSE and exhaustion both return DS_OK.
    }
    // The primary driver entry has no GUID and an empty module.
    let (guid, description, module) = match enumeration.delivered {
        0 => (None, "Primary Sound Driver", ""),
        _ => (Some(PLAYBACK_DEVICE), "Speakers", MODULE),
    };
    enumeration.delivered += 1;
    let mut bytes = guid.map_or_else(Vec::new, |guid| guid.to_vec());
    let at = |bytes: &Vec<u8>| enumeration.scratch + bytes.len() as u32;
    let text = at(&bytes);
    bytes.extend(string(description, enumeration.wide));
    let module_at = at(&bytes);
    bytes.extend(string(module, enumeration.wide));
    if env.cpu.write(enumeration.scratch, &bytes).is_err() {
        return Outcome::Return(OUT_OF_MEMORY);
    }
    let guid = guid.map_or(0, |_| enumeration.scratch);
    // Pointers are valid throughout the synchronous callback, including a
    // wait or a nested API call, but are not retained afterward.
    Outcome::call_on_stack(
        enumeration.callback,
        [guid, text, module_at, enumeration.context],
        enumeration.scratch,
        move |env, resume| next(env, enumeration, resume.value()),
    )
}

#[win32(dsound, name = "DirectSoundEnumerateA", level = implement)]
fn enumerate_a(env: &mut Env<'_>, callback: GuestPtr<()>, context: GuestPtr<()>) -> Outcome {
    start(env, callback.addr(), context.addr(), false, true)
}
#[win32(dsound, name = "DirectSoundEnumerateW", level = implement)]
fn enumerate_w(env: &mut Env<'_>, callback: GuestPtr<()>, context: GuestPtr<()>) -> Outcome {
    start(env, callback.addr(), context.addr(), true, true)
}
#[win32(dsound, name = "DirectSoundEnumerateA", level = state)]
fn unavailable_a(env: &mut Env<'_>, callback: GuestPtr<()>, context: GuestPtr<()>) -> Outcome {
    start(env, callback.addr(), context.addr(), false, false)
}
#[win32(dsound, name = "DirectSoundEnumerateW", level = state)]
fn unavailable_w(env: &mut Env<'_>, callback: GuestPtr<()>, context: GuestPtr<()>) -> Outcome {
    start(env, callback.addr(), context.addr(), true, false)
}

/// `DSDEVID_DefaultPlayback` and `DSDEVID_DefaultVoicePlayback`.
pub(super) const DEFAULT_PLAYBACK: [[u8; 16]; 2] = [
    [
        0x00, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c,
        0x03,
    ],
    [
        0x02, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c,
        0x03,
    ],
];
/// {2270EAF5-ED98-4C2D-AE2B-97007FF345C1}, the emulated default playback
/// device.
pub(super) const PLAYBACK_DEVICE: [u8; 16] = [
    0xf5, 0xea, 0x70, 0x22, 0x98, 0xed, 0x2d, 0x4c, 0xae, 0x2b, 0x97, 0x00, 0x7f, 0xf3, 0x45, 0xc1,
];
const GUID_NULL: [u8; 16] = [0; 16];

/// Whether `guid` names the default playback device, by alias or identity.
pub(super) fn playback(guid: &[u8; 16]) -> bool {
    *guid == PLAYBACK_DEVICE || DEFAULT_PLAYBACK.contains(guid)
}

/// Checks whether a device creation or initialization GUID pointer names the
/// installed playback device. Null pointers and `GUID_NULL` select the default
/// playback device, whereas `GetDeviceID` still rejects `GUID_NULL`.
pub(super) fn check_playback_device(env: &mut Env<'_>, address: u32) -> Result<(), u32> {
    if !env.ext.get::<Sound>().enabled {
        return Err(NO_DRIVER);
    }
    if address == 0 {
        return Ok(());
    }
    let Some(guid) = read_guid(env, address) else {
        return Err(INVALID);
    };
    if guid == GUID_NULL || playback(&guid) {
        Ok(())
    } else {
        Err(NO_DRIVER)
    }
}

/// Reads the GUID at `address`, or `None` when it is unreadable.
pub(super) fn read_guid(env: &mut Env<'_>, address: u32) -> Option<[u8; 16]> {
    let mut guid = [0; 16];
    check_range(env.cpu, address, 16, Prot::READ).ok()?;
    env.cpu.read(address, &mut guid).ok()?;
    Some(guid)
}

#[win32(dsound, name = "GetDeviceID", level = implement)]
fn get_device_id(env: &mut Env<'_>, src: GuestPtr<()>, dest: GuestPtr<()>) -> u32 {
    if src.is_null() || dest.is_null() {
        return INVALID;
    }
    let Some(guid) = read_guid(env, src.addr()) else {
        return INVALID;
    };
    if check_range(env.cpu, dest.addr(), 16, Prot::WRITE).is_err() {
        return INVALID;
    }
    // Capture and other devices do not exist.
    if !env.ext.get::<Sound>().enabled || !playback(&guid) {
        return NO_DRIVER;
    }
    if env.cpu.write(dest.addr(), &PLAYBACK_DEVICE).is_err() {
        return INVALID;
    }
    0
}

#[win32(dsound, name = "GetDeviceID", level = safe_fail)]
fn unavailable_device_id(_env: &mut Env<'_>, _src: GuestPtr<()>, _dest: GuestPtr<()>) -> u32 {
    NO_DRIVER
}

pub(super) fn register(registry: &mut Registry, installed: bool) {
    registry.register(if installed {
        enumerate_a_ABI
    } else {
        unavailable_a_ABI
    });
    registry.register(if installed {
        enumerate_w_ABI
    } else {
        unavailable_w_ABI
    });
    registry.register(if installed {
        get_device_id_ABI
    } else {
        unavailable_device_id_ABI
    });
}
