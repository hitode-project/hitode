//! No MIDI synthesizer is installed. Capability queries and stream opening must
//! agree, so applications can take their own no-MIDI path.
use adv_abi::{GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;

fn unavailable(env: &mut Env<'_>, api: &str) {
    adv_abi::warn_unavailable(
        env,
        api,
        "MIDI synthesizer is unavailable; no MIDI output devices are exposed",
    );
}

fn device_error(device: u32) -> u32 {
    if device == u32::MAX { 6 } else { 2 } // NODRIVER / BADDEVICEID.
}

#[win32(winmm, name = "midiOutGetNumDevs", level = state)]
fn count(env: &mut Env<'_>) -> u32 {
    unavailable(env, "midiOutGetNumDevs");
    0
}

#[win32(winmm, name = "midiOutGetDevCapsA", level = safe_fail)]
fn caps(env: &mut Env<'_>, device: u32, output: GuestPtr<()>, size: u32) -> u32 {
    if size == 0 {
        return 0;
    }
    if check_range(env.cpu, output.addr(), size.min(52), Prot::WRITE).is_err() {
        return 11;
    }
    unavailable(env, "midiOutGetDevCapsA");
    device_error(device)
}

#[win32(winmm, name = "midiStreamOpen", level = safe_fail)]
fn stream_open(
    env: &mut Env<'_>,
    output: GuestPtr<()>,
    device: GuestPtr<u32>,
    count: u32,
    _callback: u32,
    _instance: u32,
    _flags: u32,
) -> u32 {
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return 11;
    }
    if env.cpu.write_u32(output.addr(), 0).is_err() {
        return 11;
    }
    if count != 1 || check_range(env.cpu, device.addr(), 4, Prot::RW).is_err() {
        return 11;
    }
    let Ok(id) = env.cpu.read_u32(device.addr()) else {
        return 11;
    };
    // Native tags the selected device as a stream even if opening fails. Flags
    // and callback targets are not consumed when the device itself is absent.
    if env.cpu.write_u32(device.addr(), id | 0x8000_0000).is_err() {
        return 11;
    }
    unavailable(env, "midiStreamOpen");
    device_error(id)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(count_ABI);
    registry.register(caps_ABI);
    registry.register(stream_open_ABI);
}
