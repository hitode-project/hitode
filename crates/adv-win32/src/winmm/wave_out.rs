//! WaveOut capability boundary until a guest waveform-buffer front exists.
//! DirectSound's mixer does not imply a second, implemented WaveOut device.
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;

const BAD_DEVICE_ID: u32 = 2;
const INVALID_HANDLE: u32 = 5;
const NO_DRIVER: u32 = 6;
const INVALID_PARAMETER: u32 = 11;
const WAVE_FORMAT_QUERY: u32 = 1;
const CALLBACK_MASK: u32 = 0x0007_0000;

fn unavailable(env: &mut Env<'_>, api: &str) {
    adv_abi::warn_unavailable(
        env,
        api,
        "WaveOut frontend is unavailable; no waveform output devices are exposed",
    );
}

#[win32(winmm, name = "waveOutGetNumDevs", level = state)]
fn count(env: &mut Env<'_>) -> u32 {
    unavailable(env, "waveOutGetNumDevs");
    0
}

#[win32(winmm, name = "waveOutGetDevCapsA", level = safe_fail)]
fn caps(env: &mut Env<'_>, device: u32, output: GuestPtr<()>, size: u32) -> u32 {
    capabilities(env, device, output, size, false)
}

#[win32(winmm, name = "waveOutGetDevCapsW", level = safe_fail)]
fn caps_w(env: &mut Env<'_>, device: u32, output: GuestPtr<()>, size: u32) -> u32 {
    capabilities(env, device, output, size, true)
}

fn capabilities(
    env: &mut Env<'_>,
    device: u32,
    output: GuestPtr<()>,
    size: u32,
    wide: bool,
) -> u32 {
    // SDK explicitly defines a zero-byte capability copy as a successful no-op.
    if size == 0 {
        return 0;
    }
    if check_range(
        env.cpu,
        output.addr(),
        size.min(if wide { 84 } else { 52 }),
        Prot::WRITE,
    )
    .is_err()
    {
        return INVALID_PARAMETER;
    }
    unavailable(
        env,
        if wide {
            "waveOutGetDevCapsW"
        } else {
            "waveOutGetDevCapsA"
        },
    );
    if device == u32::MAX {
        NO_DRIVER
    } else {
        BAD_DEVICE_ID
    }
}

fn valid_format(env: &Env<'_>, pointer: u32) -> bool {
    if check_range(env.cpu, pointer, 16, Prot::READ).is_err() {
        return false;
    }
    if env.cpu.read_u16(pointer).ok() == Some(1) {
        // PCM accepts the legacy 16-byte PCMWAVEFORMAT; no cbSize is required.
        return true;
    }
    if check_range(env.cpu, pointer, 18, Prot::READ).is_err() {
        return false;
    }
    let Ok(extra) = env.cpu.read_u16(pointer + 16) else {
        return false;
    };
    check_range(env.cpu, pointer, 18 + u32::from(extra), Prot::READ).is_ok()
}

#[win32(winmm, name = "waveOutOpen", level = safe_fail)]
fn open(
    env: &mut Env<'_>,
    output: GuestPtr<()>,
    device: u32,
    format: GuestPtr<()>,
    _callback: u32,
    _instance: u32,
    flags: u32,
) -> u32 {
    let query = flags & WAVE_FORMAT_QUERY != 0;
    if flags & !(CALLBACK_MASK | 0x1f) != 0
        || !matches!(
            flags & CALLBACK_MASK,
            0 | 0x10000 | 0x20000 | 0x30000 | 0x50000
        )
        || (!query && check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err())
        || !valid_format(env, format.addr())
    {
        return INVALID_PARAMETER;
    }
    if !query && env.cpu.write_u32(output.addr(), 0).is_err() {
        return INVALID_PARAMETER;
    }
    unavailable(env, "waveOutOpen");
    if device == u32::MAX {
        NO_DRIVER
    } else {
        BAD_DEVICE_ID
    }
}

// No successful Open can create a waveform handle. Reject the handle before
// reading a WAVEHDR or changing flags/buffers; never send WOM_OPEN/DONE/CLOSE.
fn invalid_handle(env: &mut Env<'_>, api: &str) -> u32 {
    unavailable(env, api);
    INVALID_HANDLE
}

#[win32(winmm, name = "waveOutPrepareHeader", level = safe_fail)]
fn prepare(env: &mut Env<'_>, _wave: Handle, _header: GuestPtr<()>, _size: u32) -> u32 {
    invalid_handle(env, "waveOutPrepareHeader")
}
#[win32(winmm, name = "waveOutUnprepareHeader", level = safe_fail)]
fn unprepare(env: &mut Env<'_>, _wave: Handle, _header: GuestPtr<()>, _size: u32) -> u32 {
    invalid_handle(env, "waveOutUnprepareHeader")
}
#[win32(winmm, name = "waveOutWrite", level = safe_fail)]
fn write(env: &mut Env<'_>, _wave: Handle, _header: GuestPtr<()>, _size: u32) -> u32 {
    invalid_handle(env, "waveOutWrite")
}
#[win32(winmm, name = "waveOutPause", level = safe_fail)]
fn pause(env: &mut Env<'_>, _wave: Handle) -> u32 {
    invalid_handle(env, "waveOutPause")
}
#[win32(winmm, name = "waveOutRestart", level = safe_fail)]
fn restart(env: &mut Env<'_>, _wave: Handle) -> u32 {
    invalid_handle(env, "waveOutRestart")
}
#[win32(winmm, name = "waveOutReset", level = safe_fail)]
fn reset(env: &mut Env<'_>, _wave: Handle) -> u32 {
    invalid_handle(env, "waveOutReset")
}
#[win32(winmm, name = "waveOutClose", level = safe_fail)]
fn close(env: &mut Env<'_>, _wave: Handle) -> u32 {
    invalid_handle(env, "waveOutClose")
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(count_ABI);
    registry.register(caps_ABI);
    registry.register(caps_w_ABI);
    registry.register(open_ABI);
    registry.register(prepare_ABI);
    registry.register(unprepare_ABI);
    registry.register(write_ABI);
    registry.register(pause_ABI);
    registry.register(restart_ABI);
    registry.register(reset_ABI);
    registry.register(close_ABI);
}
