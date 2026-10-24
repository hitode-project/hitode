//! WinMM output messages over the device synthesizer. Wine retains stream
//! scheduling, MCI sequencing, handle mapping and DriverCallback delivery.
use super::*;
use adv_device::midi::{MidiEvent, SAMPLE_RATE};

const SOURCE: u32 = 0x4000_0000;
const PREPARED: u32 = 2;
const INQUEUE: u32 = 4;

#[derive(Clone, Copy)]
struct Open {
    device: u32,
    callback: u32,
    instance: u32,
    flags: u32,
}
struct Midi {
    open: Option<Open>,
    volume: u32,
    running: u8,
    synth: Option<adv_device::midi::Synth>,
    generation: u32,
    origin: u64,
    frames: u64,
}
impl Default for Midi {
    fn default() -> Self {
        Self {
            open: None,
            volume: u32::MAX,
            running: 0,
            synth: None,
            generation: 0,
            origin: 0,
            frames: 0,
        }
    }
}

struct Notify {
    open: Open,
    message: u32,
    param: u32,
}

fn source(env: &Env<'_>) -> u64 {
    source_id(env.sys.pid, SOURCE)
}

fn message(
    env: &mut Env<'_>,
    id: u32,
    msg: u32,
    p1: u32,
    p2: u32,
) -> Result<(u32, Option<Notify>), String> {
    if (100..=103).contains(&msg) {
        return Ok((0, None));
    }
    if msg == 1 {
        return Ok((1, None));
    }
    if id != 0 {
        return Ok((2, None));
    }
    let audio = env.ext.get::<AudioControl>().clone();
    audio.sync_clock(env.sys.clock.now_us());
    if msg == 2 {
        if p1 == 0 {
            return Ok((11, None));
        }
        let mut caps = [0u8; 84];
        caps[0..2].copy_from_slice(&0xffu16.to_le_bytes());
        caps[2..4].copy_from_slice(&1u16.to_le_bytes());
        caps[4..8].copy_from_slice(&0x100u32.to_le_bytes());
        for (index, unit) in "Hitode MIDI Synth".encode_utf16().enumerate() {
            caps[8 + index * 2..10 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        for (at, value) in [(72, 7u16), (74, 128), (76, 128), (78, 0xffff)] {
            caps[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        caps[80..84].copy_from_slice(&3u32.to_le_bytes()); // VOLUME | LRVOLUME
        env.cpu
            .write(p1, &caps[..(p2 as usize).min(caps.len())])
            .map_err(|e| e.to_string())?;
        return Ok((0, None));
    }
    if msg == 10 {
        if p1 == 0 {
            return Ok((11, None));
        }
        let volume = env.ext.get_or_default::<Midi>().volume;
        write(env, p1, volume)?;
        return Ok((0, None));
    }
    if msg == 11 {
        if env.ext.get_or_default::<Midi>().open.is_some()
            && !audio.set_stereo_gain(
                source(env),
                [(p1 & 0xffff) as f32 / 65535.0, (p1 >> 16) as f32 / 65535.0],
            )
        {
            return Err("MIDI audio command limit".into());
        }
        env.ext.get_mut::<Midi>().volume = p1;
        return Ok((0, None));
    }
    if msg == 3 {
        if p1 == 0 {
            return Ok((11, None));
        }
        if p2 & !0x7_0000 != 0 {
            return Ok((10, None));
        }
        if env.ext.get_or_default::<Midi>().open.is_some() {
            return Ok((4, None));
        }
        let open = Open {
            device: read(env, p1)?,
            callback: read(env, p1 + 4)?,
            instance: read(env, p1 + 8)?,
            flags: p2 >> 16,
        };
        let midi = env.ext.get_mut::<Midi>();
        if midi.synth.is_none() {
            midi.synth = Some(adv_device::midi::Synth::new()?);
        }
        midi.synth.as_mut().unwrap().event(MidiEvent::Reset)?;
        midi.generation += 1;
        midi.origin = env.sys.clock.now_us() + LEAD_US;
        midi.frames = 0;
        let generation = midi.generation;
        let entry = env
            .sys
            .thunks
            .publish(env.cpu, MODULE, "midi_render")
            .map_err(|e| e.to_string())?;
        env.sys
            .spawn(env.cpu, "MIDI render", entry, &[generation], 64 * 1024)
            .map_err(|e| e.to_string())?;
        let midi = env.ext.get_mut::<Midi>();
        midi.open = Some(open);
        midi.running = 0;
        return Ok((
            0,
            Some(Notify {
                open,
                message: 0x3c7,
                param: 0,
            }),
        ));
    }
    let Some(open) = env.ext.get_or_default::<Midi>().open else {
        return Ok((5, None));
    };
    pump(env)?;
    match msg {
        4 => {
            if !audio.remove_pcm(source(env)) {
                return Err("MIDI audio command limit".into());
            }
            env.ext.get_mut::<Midi>().open = None;
            Ok((
                0,
                Some(Notify {
                    open,
                    message: 0x3c8,
                    param: 0,
                }),
            ))
        }
        5 | 6 | 8 => {
            if p1 == 0 || p2 < 28 {
                return Ok((11, None));
            }
            let data = read(env, p1)?;
            let flags = read(env, p1 + 16)?;
            if msg == 5 {
                if data == 0 {
                    return Ok((11, None));
                }
                if flags & PREPARED == 0 {
                    write(env, p1 + 20, 0)?;
                    write(env, p1 + 16, (flags | PREPARED) & !5)?;
                }
            } else if msg == 6 {
                if data == 0 {
                    return Ok((11, None));
                }
                if flags & INQUEUE != 0 {
                    return Ok((65, None));
                }
                write(env, p1 + 16, flags & !PREPARED)?;
            } else {
                if data == 0 || flags & PREPARED == 0 {
                    return Ok((64, None));
                }
                if flags & INQUEUE != 0 {
                    return Ok((65, None));
                }
                let mut bytes = vec![0; read(env, p1 + 4)? as usize];
                env.cpu.read(data, &mut bytes).map_err(|e| e.to_string())?;
                if let Some(event) = MidiEvent::sysex(&bytes) {
                    env.ext
                        .get_mut::<Midi>()
                        .synth
                        .as_mut()
                        .unwrap()
                        .event(event)?;
                }
                env.ext.get_mut::<Midi>().running = 0;
                write(env, p1 + 16, (flags | 1) & !INQUEUE)?;
                return Ok((
                    0,
                    Some(Notify {
                        open,
                        message: 0x3c9,
                        param: p1,
                    }),
                ));
            }
            Ok((0, None))
        }
        7 => {
            let midi = env.ext.get_mut::<Midi>();
            let first = p1 as u8;
            let packed = if first & 0x80 != 0 {
                if first < 0xf0 {
                    midi.running = first;
                } else if first <= 0xf7 {
                    midi.running = 0;
                }
                p1
            } else if midi.running != 0 {
                (p1 << 8) | u32::from(midi.running)
            } else {
                return Ok((0, None));
            };
            env.ext
                .get_mut::<Midi>()
                .synth
                .as_mut()
                .unwrap()
                .event(MidiEvent::Short(packed))?;
            Ok((0, None))
        }
        9 => {
            env.ext
                .get_mut::<Midi>()
                .synth
                .as_mut()
                .unwrap()
                .event(MidiEvent::Silence)?;
            env.ext.get_mut::<Midi>().running = 0;
            Ok((0, None))
        }
        _ => Ok((8, None)),
    }
}

/// Direct driver exports, used when WinMM loads wineadvaudio.drv itself.
pub(super) fn output(env: &mut Env<'_>) -> Outcome {
    let args = [env.arg(0), env.arg(1), env.arg(3), env.arg(4)];
    match message(env, args[0], args[1], args[2], args[3]) {
        Ok((result, Some(notify))) => {
            let callback = adv_nt::process::module_base(env.cpu, env.sys.peb, "winmm.dll")
                .and_then(|base| adv_nt::process::module_export(env.cpu, base, "DriverCallback"));
            match callback {
                Some(callback) => Outcome::call(
                    callback,
                    [
                        notify.open.callback,
                        notify.open.flags,
                        notify.open.device,
                        notify.message,
                        notify.open.instance,
                        notify.param,
                        0,
                    ],
                    move |_, _| Outcome::Return(result),
                ),
                None => Outcome::Fail("WinMM DriverCallback export missing".into()),
            }
        }
        Ok((result, None)) => Outcome::Return(result),
        Err(error) => Outcome::Fail(error),
    }
}

/// mmdevapi's Unix table uses the same implementation and lets its PE side
/// invoke DriverCallback after the Unix call returns.
pub(super) fn unix_output(env: &mut Env<'_>, args: u32) -> Result<(), String> {
    let (result, notify) = message(
        env,
        read(env, args)?,
        read(env, args + 4)?,
        read(env, args + 12)?,
        read(env, args + 16)?,
    )?;
    write(env, read(env, args + 20)?, result)?;
    let out = read(env, args + 24)?;
    let words = notify.map_or([0; 8], |n| {
        [
            1,
            n.message << 16,
            n.param,
            0,
            n.open.callback,
            n.open.flags,
            n.open.device,
            n.open.instance,
        ]
    });
    crate::write_words(env, out, &words)
}

// Fixed output lead separates guest event times from callback consumption. The
// render context waits on the machine clock, including when the game is idle.
const LEAD_US: u64 = 30_000;
const PERIOD_US: u64 = 10_000;
fn pump(env: &mut Env<'_>) -> Result<(), String> {
    let audio = env.ext.get::<AudioControl>().clone();
    let now = env.sys.clock.now_us();
    audio.sync_clock(now);
    let source = source(env);
    let midi = env.ext.get_mut::<Midi>();
    let end = (now + LEAD_US).saturating_sub(midi.origin) * u64::from(SAMPLE_RATE) / 1_000_000;
    while midi.frames < end {
        let frames = (end - midi.frames).min(441) as usize;
        let samples = midi.synth.as_mut().unwrap().render(frames);
        audio
            .push_pcm(adv_device::mixer::PcmBlock {
                stream_id: source,
                guest_pts_us: midi.origin + midi.frames * 1_000_000 / u64::from(SAMPLE_RATE),
                sample_rate: SAMPLE_RATE,
                channels: 2,
                samples,
            })
            .map_err(|_| "MIDI PCM queue limit")?;
        midi.frames += frames as u64;
    }
    if !audio.set_stereo_gain(
        source,
        [
            (midi.volume & 0xffff) as f32 / 65535.0,
            (midi.volume >> 16) as f32 / 65535.0,
        ],
    ) {
        return Err("MIDI audio command limit".into());
    }
    Ok(())
}

pub(super) fn timer(env: &mut Env<'_>) -> Outcome {
    tick(env, env.arg(0))
}
fn tick(env: &mut Env<'_>, generation: u32) -> Outcome {
    let midi = env.ext.get::<Midi>();
    if midi.open.is_none() || midi.generation != generation {
        return Outcome::Return(0);
    }
    if let Err(error) = pump(env) {
        return Outcome::Fail(error);
    }
    Outcome::block(
        adv_machine::Wait::Until(env.sys.clock.now_us() + PERIOD_US),
        move |env, _| tick(env, generation),
    )
}
