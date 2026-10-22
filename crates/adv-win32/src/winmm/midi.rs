//! No MIDI synthesizer is installed. Capability queries and stream opening must
//! agree, so applications can take their own no-MIDI path.
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;

fn device_error(device: u32) -> u32 {
    if device == u32::MAX { 6 } else { 2 } // NODRIVER / BADDEVICEID.
}

#[win32(winmm, name = "midiOutGetNumDevs", level = state)]
fn count(_env: &mut Env<'_>) -> u32 {
    0
}

#[win32(winmm, name = "midiOutGetDevCapsA", level = safe_fail)]
fn caps(env: &mut Env<'_>, device: u32, output: GuestPtr<()>, size: u32) -> u32 {
    capabilities(env, device, output, size, 52, true)
}

#[win32(winmm, name = "midiOutGetDevCapsW", level = safe_fail)]
fn caps_w(env: &mut Env<'_>, device: u32, output: GuestPtr<()>, size: u32) -> u32 {
    capabilities(env, device, output, size, 84, true)
}

fn capabilities(
    env: &mut Env<'_>,
    device: u32,
    output: GuestPtr<()>,
    size: u32,
    structure_size: u32,
    mapper: bool,
) -> u32 {
    if size == 0 {
        return 0;
    }
    if check_range(
        env.cpu,
        output.addr(),
        size.min(structure_size),
        Prot::WRITE,
    )
    .is_err()
    {
        return 11;
    }
    if mapper { device_error(device) } else { 2 }
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
    device_error(id)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(count_ABI);
    registry.register(caps_ABI);
    registry.register(caps_w_ABI);
    registry.register(stream_open_ABI);
    register_absent(registry);
}

// spec: MEDIA-AUDIO-5
#[win32(winmm, name = "midiInGetNumDevs", level = state)]
fn input_count(_env: &mut Env<'_>) -> u32 {
    0
}

#[win32(winmm, name = "midiInGetDevCapsA", level = safe_fail)]
fn input_caps_a(env: &mut Env<'_>, device: u32, output: GuestPtr<()>, size: u32) -> u32 {
    capabilities(env, device, output, size, 44, false)
}

#[win32(winmm, name = "midiInGetDevCapsW", level = safe_fail)]
fn input_caps_w(env: &mut Env<'_>, device: u32, output: GuestPtr<()>, size: u32) -> u32 {
    capabilities(env, device, output, size, 76, false)
}

fn open_device(
    env: &mut Env<'_>,
    output: GuestPtr<()>,
    device: u32,
    flags: u32,
    input: bool,
) -> u32 {
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return 11;
    }
    if env.cpu.write_u32(output.addr(), 0).is_err() {
        return 11;
    }
    let allowed = 0x0007_0000 | if input { 0x20 } else { 0 };
    if flags & !allowed != 0
        || !matches!(
            flags & 0x0007_0000,
            0 | 0x10000 | 0x20000 | 0x30000 | 0x50000
        )
    {
        return 10; // MMSYSERR_INVALFLAG.
    }
    // MIDI_MAPPER is an output-device selector, not an input-device ID.
    if input { 2 } else { device_error(device) }
}

#[win32(winmm, name = "midiOutOpen", level = safe_fail)]
fn output_open(
    env: &mut Env<'_>,
    output: GuestPtr<()>,
    device: u32,
    _callback: u32,
    _instance: u32,
    flags: u32,
) -> u32 {
    open_device(env, output, device, flags, false)
}

#[win32(winmm, name = "midiInOpen", level = safe_fail)]
fn input_open(
    env: &mut Env<'_>,
    output: GuestPtr<()>,
    device: u32,
    _callback: u32,
    _instance: u32,
    flags: u32,
) -> u32 {
    open_device(env, output, device, flags, true)
}

// No Open publishes a MIDI handle. Handle-based operations fail before reading
// headers, message arrays, property structures or output pointers. This is an
// ordinary invalid handle, not an unavailable capability.

#[win32(winmm, name = "midiConnect", level = safe_fail)]
fn absent_midi_connect(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: Handle,
    _arg2: GuestPtr<()>,
) -> u32 {
    5
}

#[win32(winmm, name = "midiDisconnect", level = safe_fail)]
fn absent_midi_disconnect(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: Handle,
    _arg2: GuestPtr<()>,
) -> u32 {
    5
}

#[win32(winmm, name = "midiInAddBuffer", level = safe_fail)]
fn absent_midi_in_add_buffer(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: GuestPtr<()>,
    _arg2: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiInClose", level = safe_fail)]
fn absent_midi_in_close(_env: &mut Env<'_>, _arg0: Handle) -> u32 {
    5
}

#[win32(winmm, name = "midiInGetID", level = safe_fail)]
fn absent_midi_in_get_id(_env: &mut Env<'_>, _arg0: Handle, _arg1: GuestPtr<()>) -> u32 {
    5
}

#[win32(winmm, name = "midiInMessage", level = safe_fail)]
fn absent_midi_in_message(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: u32,
    _arg2: u32,
    _arg3: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiInPrepareHeader", level = safe_fail)]
fn absent_midi_in_prepare_header(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: GuestPtr<()>,
    _arg2: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiInReset", level = safe_fail)]
fn absent_midi_in_reset(_env: &mut Env<'_>, _arg0: Handle) -> u32 {
    5
}

#[win32(winmm, name = "midiInStart", level = safe_fail)]
fn absent_midi_in_start(_env: &mut Env<'_>, _arg0: Handle) -> u32 {
    5
}

#[win32(winmm, name = "midiInStop", level = safe_fail)]
fn absent_midi_in_stop(_env: &mut Env<'_>, _arg0: Handle) -> u32 {
    5
}

#[win32(winmm, name = "midiInUnprepareHeader", level = safe_fail)]
fn absent_midi_in_unprepare_header(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: GuestPtr<()>,
    _arg2: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiOutCacheDrumPatches", level = safe_fail)]
fn absent_midi_out_cache_drum_patches(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: u32,
    _arg2: GuestPtr<()>,
    _arg3: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiOutCachePatches", level = safe_fail)]
fn absent_midi_out_cache_patches(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: u32,
    _arg2: GuestPtr<()>,
    _arg3: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiOutClose", level = safe_fail)]
fn absent_midi_out_close(_env: &mut Env<'_>, _arg0: Handle) -> u32 {
    5
}

#[win32(winmm, name = "midiOutGetID", level = safe_fail)]
fn absent_midi_out_get_id(_env: &mut Env<'_>, _arg0: Handle, _arg1: GuestPtr<()>) -> u32 {
    5
}

#[win32(winmm, name = "midiOutGetVolume", level = safe_fail)]
fn absent_midi_out_get_volume(_env: &mut Env<'_>, device: Handle, _arg1: GuestPtr<()>) -> u32 {
    device_error(device.0)
}

#[win32(winmm, name = "midiOutLongMsg", level = safe_fail)]
fn absent_midi_out_long_msg(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: GuestPtr<()>,
    _arg2: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiOutMessage", level = safe_fail)]
fn absent_midi_out_message(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: u32,
    _arg2: u32,
    _arg3: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiOutPrepareHeader", level = safe_fail)]
fn absent_midi_out_prepare_header(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: GuestPtr<()>,
    _arg2: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiOutReset", level = safe_fail)]
fn absent_midi_out_reset(_env: &mut Env<'_>, _arg0: Handle) -> u32 {
    5
}

#[win32(winmm, name = "midiOutSetVolume", level = safe_fail)]
fn absent_midi_out_set_volume(_env: &mut Env<'_>, device: Handle, _arg1: u32) -> u32 {
    device_error(device.0)
}

#[win32(winmm, name = "midiOutShortMsg", level = safe_fail)]
fn absent_midi_out_short_msg(_env: &mut Env<'_>, _arg0: Handle, _arg1: u32) -> u32 {
    5
}

#[win32(winmm, name = "midiOutUnprepareHeader", level = safe_fail)]
fn absent_midi_out_unprepare_header(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: GuestPtr<()>,
    _arg2: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiStreamClose", level = safe_fail)]
fn absent_midi_stream_close(_env: &mut Env<'_>, _arg0: Handle) -> u32 {
    5
}

#[win32(winmm, name = "midiStreamOut", level = safe_fail)]
fn absent_midi_stream_out(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: GuestPtr<()>,
    _arg2: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiStreamPause", level = safe_fail)]
fn absent_midi_stream_pause(_env: &mut Env<'_>, _arg0: Handle) -> u32 {
    5
}

#[win32(winmm, name = "midiStreamPosition", level = safe_fail)]
fn absent_midi_stream_position(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: GuestPtr<()>,
    _arg2: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiStreamProperty", level = safe_fail)]
fn absent_midi_stream_property(
    _env: &mut Env<'_>,
    _arg0: Handle,
    _arg1: GuestPtr<()>,
    _arg2: u32,
) -> u32 {
    5
}

#[win32(winmm, name = "midiStreamRestart", level = safe_fail)]
fn absent_midi_stream_restart(_env: &mut Env<'_>, _arg0: Handle) -> u32 {
    5
}

#[win32(winmm, name = "midiStreamStop", level = safe_fail)]
fn absent_midi_stream_stop(_env: &mut Env<'_>, _arg0: Handle) -> u32 {
    5
}

fn register_absent(registry: &mut Registry) {
    registry.register(input_count_ABI);
    registry.register(input_caps_a_ABI);
    registry.register(input_caps_w_ABI);
    registry.register(output_open_ABI);
    registry.register(input_open_ABI);
    registry.register(absent_midi_connect_ABI);
    registry.register(absent_midi_disconnect_ABI);
    registry.register(absent_midi_in_add_buffer_ABI);
    registry.register(absent_midi_in_close_ABI);
    registry.register(absent_midi_in_get_id_ABI);
    registry.register(absent_midi_in_message_ABI);
    registry.register(absent_midi_in_prepare_header_ABI);
    registry.register(absent_midi_in_reset_ABI);
    registry.register(absent_midi_in_start_ABI);
    registry.register(absent_midi_in_stop_ABI);
    registry.register(absent_midi_in_unprepare_header_ABI);
    registry.register(absent_midi_out_cache_drum_patches_ABI);
    registry.register(absent_midi_out_cache_patches_ABI);
    registry.register(absent_midi_out_close_ABI);
    registry.register(absent_midi_out_get_id_ABI);
    registry.register(absent_midi_out_get_volume_ABI);
    registry.register(absent_midi_out_long_msg_ABI);
    registry.register(absent_midi_out_message_ABI);
    registry.register(absent_midi_out_prepare_header_ABI);
    registry.register(absent_midi_out_reset_ABI);
    registry.register(absent_midi_out_set_volume_ABI);
    registry.register(absent_midi_out_short_msg_ABI);
    registry.register(absent_midi_out_unprepare_header_ABI);
    registry.register(absent_midi_stream_close_ABI);
    registry.register(absent_midi_stream_out_ABI);
    registry.register(absent_midi_stream_pause_ABI);
    registry.register(absent_midi_stream_position_ABI);
    registry.register(absent_midi_stream_property_ABI);
    registry.register(absent_midi_stream_restart_ABI);
    registry.register(absent_midi_stream_stop_ABI);
}
