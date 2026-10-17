//! Enumerate only the installed emulated default that DirectSoundCreate(NULL)
//! can open. The front never queries host hardware or invents device GUIDs.
use adv_abi::{GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::{Prot, Reg};
use adv_machine::{Env, Outcome};

use super::{INVALID, OUT_OF_MEMORY, Sound};

const SCRATCH: u32 = 64;
const CALLBACK_STACK: u32 = 20; // Four arguments plus return address.

struct Enumeration {
    callback: u32,
    context: u32,
    scratch: u32,
    wide: bool,
    delivered: bool,
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
            delivered: false,
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
        // The two fixed labels are ASCII, represented identically in every
        // supported guest ACP (including CP932, CP936 and UTF-8).
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        bytes
    }
}

fn next(env: &mut Env<'_>, mut enumeration: Enumeration, keep_going: u32) -> Outcome {
    if keep_going == 0 || enumeration.delivered {
        return Outcome::Return(0); // Callback FALSE and exhaustion both return DS_OK.
    }
    enumeration.delivered = true;
    let mut bytes = string("Primary Sound Driver", enumeration.wide);
    let module = enumeration.scratch + bytes.len() as u32;
    bytes.extend(string("dsound.dll", enumeration.wide));
    if env.cpu.write(enumeration.scratch, &bytes).is_err() {
        return Outcome::Return(OUT_OF_MEMORY);
    }
    // Only the default device has a supported identity: Create rejects any
    // non-NULL GUID. Pointers are valid throughout the synchronous callback,
    // including a wait or a nested API call, but are not retained afterward.
    Outcome::call_on_stack(
        enumeration.callback,
        [0, enumeration.scratch, module, enumeration.context],
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

const DSDEVID_DEFAULT_PLAYBACK: [u8; 16] = [
    0x00, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c, 0x03,
];
const DSDEVID_DEFAULT_CAPTURE: [u8; 16] = [
    0x01, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c, 0x03,
];
const DSDEVID_DEFAULT_VOICE_PLAYBACK: [u8; 16] = [
    0x02, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c, 0x03,
];
const DSDEVID_DEFAULT_VOICE_CAPTURE: [u8; 16] = [
    0x03, 0x00, 0xf0, 0xde, 0x6d, 0x9c, 0xed, 0x47, 0xaa, 0xf1, 0x4d, 0xda, 0x8f, 0x2b, 0x5c, 0x03,
];

#[win32(dsound, name = "GetDeviceID", level = implement)]
fn get_device_id(env: &mut Env<'_>, src: GuestPtr<()>, dest: GuestPtr<()>) -> u32 {
    if dest.is_null() || check_range(env.cpu, dest.addr(), 16, Prot::WRITE).is_err() {
        return INVALID;
    }
    let guid = if src.is_null() {
        DSDEVID_DEFAULT_PLAYBACK
    } else {
        if check_range(env.cpu, src.addr(), 16, Prot::READ).is_err() {
            return INVALID;
        }
        let mut g = [0u8; 16];
        if env.cpu.read(src.addr(), &mut g).is_err() {
            return INVALID;
        }
        g
    };
    let resolved = match guid {
        DSDEVID_DEFAULT_PLAYBACK => DSDEVID_DEFAULT_PLAYBACK,
        DSDEVID_DEFAULT_CAPTURE => DSDEVID_DEFAULT_CAPTURE,
        DSDEVID_DEFAULT_VOICE_PLAYBACK => DSDEVID_DEFAULT_VOICE_PLAYBACK,
        DSDEVID_DEFAULT_VOICE_CAPTURE => DSDEVID_DEFAULT_VOICE_CAPTURE,
        other => other,
    };
    if env.cpu.write(dest.addr(), &resolved).is_err() {
        return INVALID;
    }
    0
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
    registry.register(get_device_id_ABI);
}
