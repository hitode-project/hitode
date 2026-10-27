//! mmdevapi's Wine 11 Unix table over the device mixer. Stream bookkeeping
//! follows winecoreaudio: guest render buffers, held frames and period events.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use adv_abi::dll::{Export, ModuleImage};
use adv_abi::{Conv, Env, Outcome, Registry};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_device::{AudioControl, RingState, ring::RingConfig};
use adv_machine::Machine;

pub mod midi;
mod stream;
#[cfg(test)]
mod tests;

pub const MODULE: &str = "wineadvaudio.drv";
const UNSUPPORTED: u32 = 0x8889_0008;
const NOT_STOPPED: u32 = 0x8889_0005;
const OUT_OF_ORDER: u32 = 0x8889_0007;
const EVENT_CALLBACK: u32 = 0x40000;
/// Default and minimum device periods, in 100 ns units. DirectSound queues
/// three default periods, which must outlast a host callback's burst plus the
/// mixer thread's own wake-up interval and scheduling: 20 ms gives 60 ms.
const PERIODS: [u64; 2] = [200_000, 50_000];
const NAMES: [&str; 37] = [
    "process_attach",
    "process_detach",
    "main_loop",
    "get_endpoint_ids",
    "create_stream",
    "release_stream",
    "start",
    "stop",
    "reset",
    "timer_loop",
    "get_render_buffer",
    "release_render_buffer",
    "get_capture_buffer",
    "release_capture_buffer",
    "is_format_supported",
    "get_loopback_capture_device",
    "get_mix_format",
    "get_device_period",
    "get_buffer_size",
    "get_latency",
    "get_current_padding",
    "get_next_packet_size",
    "get_frequency",
    "get_position",
    "set_volumes",
    "set_event_handle",
    "set_sample_rate",
    "test_connect",
    "is_started",
    "get_prop_value",
    "midi_get_driver",
    "midi_init",
    "midi_release",
    "midi_out_message",
    "midi_in_message",
    "midi_notify_wait",
    "aux_message",
];

#[derive(Clone, Copy)]
struct Format {
    tag: u16,
    channels: u16,
    rate: u32,
    bits: u16,
    align: u32,
}
impl Format {
    fn mixer_bits(self) -> u16 {
        if self.tag == 1 && matches!(self.bits, 8 | 16) {
            self.bits
        } else {
            16
        }
    }
    fn mixer_align(self) -> u32 {
        u32::from(self.channels) * u32::from(self.mixer_bits()) / 8
    }
}

struct Stream {
    format: Format,
    ring: u32,
    mix: u32,
    temporary: u32,
    frames: u32,
    period: u64,
    share: u32,
    flags: u32,
    event: u32,
    playing: bool,
    running: bool,
    generation: u64,
    voice: Arc<RingState>,
    write: u32,
    held: u32,
    written: u64,
    cursor: u32,
    observed: u64,
    pending: i32,
    quit: bool,
}

#[derive(Default)]
struct Streams {
    next: u32,
    streams: HashMap<u32, Stream>,
}

pub(super) fn register(registry: &mut Registry) {
    // One thunk per Unix entry; the ordinary Unix dispatcher calls these.
    macro_rules! entries {
        ($($index:literal),*) => { $(registry.add(MODULE, NAMES[$index],
            Conv::Cdecl { argc: 1 }, |env| dispatch(env, $index));)* };
    }
    entries!(
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36
    );
    for (name, get_num) in [("midMessage", 53), ("auxMessage", 3)] {
        registry.add_with_data(
            MODULE,
            name,
            Conv::Stdcall { argc: 5 },
            |env| {
                let get_num = env.sys.thunks.get(env.thunk).f.data as u32;
                let msg = env.arg(1);
                Outcome::Return(if msg == get_num || (100..=103).contains(&msg) {
                    0
                } else {
                    8
                })
            },
            get_num,
        );
    }
    registry.add(
        MODULE,
        "modMessage",
        Conv::Stdcall { argc: 5 },
        midi::output,
    );
    registry.add(MODULE, "DriverProc", Conv::Stdcall { argc: 5 }, |env| {
        Outcome::Return(match env.arg(2) {
            1 | 3 | 4 | 6 | 7 | 8 => 1,
            _ => 0,
        })
    });
}

pub(super) fn install<B: Backend>(machine: &mut Machine<B>, prefix: &Path) -> Result<(), String> {
    let table = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .map_err(|e| e.to_string())?;
    for (index, name) in NAMES.into_iter().enumerate() {
        let entry = machine
            .sys
            .thunks
            .publish(&mut machine.cpu, MODULE, name)
            .map_err(|e| e.to_string())?;
        machine
            .cpu
            .write_u32(table + index as u32 * 4, entry)
            .map_err(|e| e.to_string())?;
    }
    machine
        .ext
        .get_or_default::<adv_nt::unixlib::HostTables>()
        .0
        .insert(MODULE.into(), table);
    let exports: Vec<_> = ["DriverProc", "auxMessage", "midMessage", "modMessage"]
        .into_iter()
        .zip(1..)
        .map(|(name, ordinal)| Export { name, ordinal })
        .collect();
    let image = ModuleImage::build(machine, MODULE, &exports, &[])?;
    adv_unix::builtin::install(prefix, &image)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn read(env: &Env<'_>, at: u32) -> Result<u32, String> {
    env.cpu.read_u32(at).map_err(|e| e.to_string())
}
fn write(env: &mut Env<'_>, at: u32, value: u32) -> Result<(), String> {
    env.cpu.write_u32(at, value).map_err(|e| e.to_string())
}
fn wide(env: &mut Env<'_>, at: u32, value: u64) -> Result<(), String> {
    env.cpu
        .write(at, &value.to_le_bytes())
        .map_err(|e| e.to_string())
}
fn format(env: &Env<'_>, at: u32) -> Result<Option<Format>, String> {
    let head = read(env, at)?;
    let mut tag = head as u16;
    if tag == 0xfffe {
        let mut guid = [0; 16];
        env.cpu
            .read(at + 24, &mut guid)
            .map_err(|e| e.to_string())?;
        if guid[4..] != [0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71] {
            return Ok(None);
        }
        tag = u32::from_le_bytes(guid[..4].try_into().unwrap()) as u16;
    }
    let format = Format {
        tag,
        channels: (head >> 16) as u16,
        rate: read(env, at + 4)?,
        align: read(env, at + 12)? & 0xffff,
        bits: (read(env, at + 12)? >> 16) as u16,
    };
    Ok((matches!(format.channels, 1 | 2)
        && format.rate != 0
        && format.align == u32::from(format.channels) * u32::from(format.bits) / 8
        && ((tag == 1 && matches!(format.bits, 8 | 16 | 24 | 32))
            || (tag == 3 && matches!(format.bits, 32 | 64))))
    .then_some(format))
}

fn allocate(env: &mut Env<'_>, bytes: u32) -> Result<u32, String> {
    env.sys
        .vm
        .alloc(env.cpu, bytes.div_ceil(4096) * 4096, Prot::RW)
        .map_err(|e| e.to_string())
}
fn silence(env: &mut Env<'_>, at: u32, bytes: u32, value: u8) -> Result<(), String> {
    env.cpu
        .write(at, &vec![value; bytes as usize])
        .map_err(|e| e.to_string())
}

fn dispatch(env: &mut Env<'_>, code: u32) -> Outcome {
    let args = env.arg(0);
    if code == 9 {
        return match read(env, args) {
            Ok(id) => stream::timer(env, id, None),
            Err(error) => Outcome::Fail(error),
        };
    }
    if code == 5 {
        return stream::release(env, args);
    }
    match execute(env, code, args) {
        Ok(()) => Outcome::Return(0),
        Err(error) => Outcome::Fail(format!("{MODULE}: {}: {error}", NAMES[code as usize])),
    }
}

fn execute(env: &mut Env<'_>, code: u32, args: u32) -> Result<(), String> {
    if let Some(audio) = env.ext.try_get::<AudioControl>() {
        audio.sync_clock(env.sys.clock.now_us());
    }
    match code {
        // coreaudio leaves the caller's initialized result for optional entries.
        0 | 1 | 15 | 26 | 30 | 32 => {}
        2 => {
            return match adv_nt::sync::set_event(env, &[read(env, args)?, 0]) {
                Outcome::Return(0) => Ok(()),
                _ => Err("main loop event failed".into()),
            };
        }
        3 => {
            let render = read(env, args)? == 0;
            write(env, args + 16, u32::from(render))?;
            write(env, args + 20, 0)?;
            let name: Vec<u8> = "Hitode audio\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            let needed = 8 + name.len() as u32 + 8;
            let size = read(env, args + 8)?;
            let hr = if render && size < needed {
                write(env, args + 8, needed)?;
                0x8007_007a
            } else {
                if render {
                    let out = read(env, args + 4)?;
                    write(env, out, 8)?;
                    write(env, out + 4, 8 + name.len() as u32)?;
                    env.cpu.write(out + 8, &name).map_err(|e| e.to_string())?;
                    env.cpu
                        .write(out + 8 + name.len() as u32, b"advaudio\0")
                        .map_err(|e| e.to_string())?;
                }
                0
            };
            write(env, args + 12, hr)?;
        }
        4 => {
            let fmt = format(env, read(env, args + 40)?)?;
            if read(env, args + 8)? != 0 || fmt.is_none() {
                return write(env, args + 44, UNSUPPORTED);
            }
            let fmt = fmt.unwrap();
            let duration =
                u64::from(read(env, args + 24)?) | (u64::from(read(env, args + 28)?) << 32);
            let period = u64::from(read(env, args + 32)?);
            let share = read(env, args + 12)?;
            let period_frames = (period * u64::from(fmt.rate) + 5_000_000) / 10_000_000;
            let mut frames = ((duration * u64::from(fmt.rate) + 5_000_000) / 10_000_000) as u32;
            if share == 1 {
                frames -= frames % period_frames as u32;
            }
            let native = fmt.tag == 1 && matches!(fmt.bits, 8 | 16);
            let mix_bytes = (frames * fmt.mixer_align()).div_ceil(4096) * 4096;
            let mix = env
                .sys
                .vm
                .reserve(mix_bytes)
                .ok_or("audio ring allocation failed")?;
            let region = env
                .cpu
                .map_shared_audio(mix, mix_bytes)
                .map_err(|e| e.to_string())?;
            let ring = if native {
                mix
            } else {
                allocate(env, frames * fmt.align)?
            };
            silence(
                env,
                ring,
                frames * fmt.align,
                if fmt.tag == 1 && fmt.bits == 8 {
                    128
                } else {
                    0
                },
            )?;
            let temporary = allocate(env, frames * fmt.align)?;
            let id = {
                let streams = env.ext.get_or_default::<Streams>();
                streams.next += 1;
                streams.next
            };
            let audio = env.ext.get::<AudioControl>();
            let voice = audio.add_ring(
                source_id(env.sys.pid, id),
                RingConfig {
                    view: region.read_alias,
                    bytes: frames * fmt.mixer_align(),
                    channels: fmt.channels,
                    bits: fmt.mixer_bits(),
                    frequency: fmt.rate,
                    output_rate: 44100,
                    notifications: Vec::new(),
                },
            )?;
            let flags = read(env, args + 16)?;
            env.ext.get_mut::<Streams>().streams.insert(
                id,
                Stream {
                    format: fmt,
                    ring,
                    mix,
                    temporary,
                    frames,
                    period,
                    share,
                    flags,
                    event: 0,
                    playing: false,
                    running: false,
                    generation: 0,
                    voice,
                    write: 0,
                    held: 0,
                    written: 0,
                    cursor: 0,
                    observed: 0,
                    pending: 0,
                    quit: false,
                },
            );
            write(env, read(env, args + 48)?, u32::from(fmt.channels))?;
            wide(env, read(env, args + 52)?, u64::from(id))?;
            write(env, args + 44, 0)?;
        }
        14 => {
            let supported =
                read(env, args + 4)? == 0 && format(env, read(env, args + 12)?)?.is_some();
            write(env, args + 16, if supported { 0 } else { UNSUPPORTED })?;
        }
        16 => {
            let out = read(env, args + 8)?;
            // Stereo PCM16 at the mixer's native rate, WAVEFORMATEXTENSIBLE.
            let mut fmt = vec![0u8; 40];
            fmt[0..2].copy_from_slice(&0xfffeu16.to_le_bytes());
            fmt[2..4].copy_from_slice(&2u16.to_le_bytes());
            fmt[4..8].copy_from_slice(&44100u32.to_le_bytes());
            fmt[8..12].copy_from_slice(&176400u32.to_le_bytes());
            fmt[12..14].copy_from_slice(&4u16.to_le_bytes());
            fmt[14..16].copy_from_slice(&16u16.to_le_bytes());
            fmt[16..18].copy_from_slice(&22u16.to_le_bytes());
            fmt[18..20].copy_from_slice(&16u16.to_le_bytes());
            fmt[20..24].copy_from_slice(&3u32.to_le_bytes());
            fmt[24..40].copy_from_slice(&[
                1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
            ]);
            env.cpu.write(out, &fmt).map_err(|e| e.to_string())?;
            write(env, args + 12, 0)?;
        }
        17 => {
            for (offset, period) in [12, 16].into_iter().zip(PERIODS) {
                let out = read(env, args + offset)?;
                if out != 0 {
                    wide(env, out, period)?;
                }
            }
            write(env, args + 8, 0)?;
        }
        27 => write(env, args + 4, 3)?,
        29 => write(env, args + 16, 0x8000_4001)?,
        31 => write(env, read(env, args)?, 1)?,
        33 => midi::unix_output(env, args)?,
        34 | 36 => {
            let msg = read(env, args + 4)?;
            let get_num = if code == 34 { 53 } else { 3 };
            write(
                env,
                read(env, args + 20)?,
                if msg == get_num || (100..=103).contains(&msg) {
                    0
                } else {
                    8
                },
            )?;
            if code != 36 {
                write(env, read(env, args + 24)?, 0)?;
            }
        }
        35 => write(env, read(env, args)?, 1)?,
        12 | 13 | 21 => return Err(format!("unsupported Unix entry {code}")),
        _ => {
            let id = read(env, args)?;
            let mut stream = env
                .ext
                .get_mut::<Streams>()
                .streams
                .remove(&id)
                .ok_or("unknown stream")?;
            let result = stream::call(env, code, args, id, &mut stream);
            env.ext.get_mut::<Streams>().streams.insert(id, stream);
            result?;
        }
    }
    Ok(())
}

fn source_id(pid: u32, id: u32) -> u64 {
    (u64::from(pid) << 32) | u64::from(id)
}
