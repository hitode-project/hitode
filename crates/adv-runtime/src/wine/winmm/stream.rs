use super::*;
use adv_device::midi::MidiEvent;
use std::collections::BTreeMap;

struct Header {
    at: u32,
    data: u32,
    length: u32,
    offset: u32,
    scheduled: bool,
}
struct Stream {
    callback: Callback,
    id: u32,
    tempo: u32,
    division: u32,
    paused: Option<u64>,
    origin: u64,
    due: u64,
    pulses: u64,
    position_us: u64,
    headers: VecDeque<Header>,
}
#[derive(Default)]
pub(super) struct Streams {
    devices: BTreeMap<u32, Stream>,
}

pub(super) fn call(env: &mut Env<'_>, name: &str, a: Vec<u32>) -> Outcome {
    if name == "midiStreamOpen" {
        if a[0] == 0 || a[1] == 0 || a[2] != 1 {
            return Outcome::Return(11);
        }
        let device = match read(env, a[1]) {
            Ok(id) => id,
            Err(e) => return Outcome::Fail(e),
        };
        let open = match imported(env, ORIGINAL, "midiOutOpen") {
            Ok(i) => i,
            Err(e) => return Outcome::Fail(e),
        };
        return check_callback(
            env,
            a[3],
            a[5],
            Box::new(move |env| {
                open.call(env, [a[0], device, 0, 0, 0], move |env, result| {
                    if result.value() != 0 {
                        return Outcome::Return(result.value());
                    }
                    let handle = match read(env, a[0]) {
                        Ok(h) => h,
                        Err(e) => return Outcome::Fail(e),
                    };
                    let callback = Callback {
                        target: a[3],
                        instance: a[4],
                        flags: a[5] & 0x70000,
                        handle,
                    };
                    let now = env.sys.clock.now_us();
                    let id = (1..u32::MAX)
                        .find(|id| {
                            env.ext
                                .get::<Streams>()
                                .devices
                                .values()
                                .all(|s| s.id != *id)
                        })
                        .unwrap();
                    env.ext.get_mut::<Streams>().devices.insert(
                        handle,
                        Stream {
                            callback,
                            id,
                            tempo: 500000,
                            division: 24,
                            paused: Some(now),
                            origin: now,
                            due: now,
                            pulses: 0,
                            position_us: 0,
                            headers: VecDeque::new(),
                        },
                    );
                    start_worker(
                        env,
                        Notice {
                            callback,
                            message: 0x3c7,
                            param: 0,
                        },
                    )
                })
            }),
        );
    }
    if !env.ext.get::<Streams>().devices.contains_key(&a[0]) {
        return Outcome::Return(5);
    }
    match result(env, name, &a) {
        Ok(value) => {
            if name == "midiStreamClose" && value == 0 {
                let stream = env.ext.get_mut::<Streams>().devices.remove(&a[0]).unwrap();
                match imported(env, ORIGINAL, "midiOutClose") {
                    Ok(close) => close.call(env, [a[0]], move |env, result| {
                        deliver(
                            env,
                            Notice {
                                callback: stream.callback,
                                message: 0x3c8,
                                param: 0,
                            },
                            result.value(),
                        )
                    }),
                    Err(e) => Outcome::Fail(e),
                }
            } else {
                Outcome::Return(value)
            }
        }
        Err(e) => Outcome::Fail(e),
    }
}
fn finish(env: &mut Env<'_>, callback: Callback, header: u32) -> Result<(), String> {
    write(env, header + 16, (read(env, header + 16)? | 1) & !4)?;
    env.ext.get_mut::<Stats>().stream_headers += 1;
    notify(
        env,
        Notice {
            callback,
            message: 0x3c9,
            param: header,
        },
    )
}
fn result(env: &mut Env<'_>, name: &str, a: &[u32]) -> Result<u32, String> {
    pump(env)?;
    let time = env.sys.clock.now_us();
    let mut streams = std::mem::take(env.ext.get_mut::<Streams>());
    let s = streams.devices.get_mut(&a[0]).unwrap();
    let value = match name {
        "midiStreamOut" => {
            if a[1] == 0
                || a[2] < 28
                || read(env, a[1])? == 0
                || read(env, a[1] + 4)? < read(env, a[1] + 8)?
                || !read(env, a[1] + 8)?.is_multiple_of(4)
            {
                11
            } else {
                let flags = read(env, a[1] + 16)?;
                if flags & 2 == 0 {
                    64
                } else if flags & 4 != 0 {
                    65
                } else {
                    write(env, a[1] + 16, (flags | 12) & !1)?;
                    write(env, a[1] + 24, 0)?;
                    if let Some(last) = s.headers.back() {
                        write(env, last.at + 24, a[1])?;
                    }
                    s.headers.push_back(Header {
                        at: a[1],
                        data: read(env, a[1])?,
                        length: read(env, a[1] + 8)?,
                        offset: 0,
                        scheduled: false,
                    });
                    0
                }
            }
        }
        "midiStreamPause" => {
            s.paused.get_or_insert(time);
            0
        }
        "midiStreamRestart" => {
            if let Some(at) = s.paused.take() {
                let delta = time - at;
                s.origin += delta;
                s.due += delta;
            }
            0
        }
        "midiStreamStop" | "midiStreamClose" => {
            for header in s.headers.drain(..) {
                finish(env, s.callback, header.at)?;
            }
            adv_drv::audio::midi::native_event(env, time, MidiEvent::Silence)?;
            s.paused = Some(time);
            s.origin = time;
            s.due = time;
            s.position_us = 0;
            s.pulses = 0;
            0
        }
        "midiStreamProperty" => {
            if a[1] == 0 || read(env, a[1])? != 8 || a[2] & 0xc000_0000 == 0 || a[2] & 3 == 0 {
                11
            } else if a[2] & 0x8000_0000 != 0 {
                let value = read(env, a[1] + 4)?;
                if a[2] & 2 != 0 {
                    s.tempo = value;
                    0
                } else if s.paused.is_some() {
                    s.division = value;
                    0
                } else {
                    11
                }
            } else {
                write(
                    env,
                    a[1] + 4,
                    if a[2] & 2 != 0 { s.tempo } else { s.division },
                )?;
                0
            }
        }
        "midiStreamPosition" => {
            if a[1] == 0 || a[2] != 12 {
                11
            } else {
                let elapsed = s.paused.unwrap_or(time).saturating_sub(s.origin);
                let pulses = s.pulses
                    + if elapsed > s.position_us {
                        if s.division <= 0x8000 {
                            (elapsed - s.position_us) * u64::from(s.division)
                                / u64::from(s.tempo).max(1)
                        } else {
                            (elapsed - s.position_us)
                                * u64::from(s.division & 255)
                                * (-(s.division as u16 as i16 >> 8)) as u64
                                / 1_000_000
                        }
                    } else {
                        0
                    };
                let kind = read(env, a[1])?;
                let value = match kind {
                    32 => pulses,
                    16 if s.division <= 0x8000 => {
                        let div = u64::from(s.division.max(24));
                        let pos = (pulses + div / 8) / (div / 4);
                        if pulses != 0 { pos.max(1) } else { 0 }
                    }
                    _ => {
                        write(env, a[1], 1)?;
                        elapsed / 1000
                    }
                };
                write(env, a[1] + 4, value as u32)?;
                0
            }
        }
        _ => 8,
    };
    *env.ext.get_mut::<Streams>() = streams;
    Ok(value)
}
pub(super) fn pump(env: &mut Env<'_>) -> Result<Option<u64>, String> {
    let time = env.sys.clock.now_us();
    let mut streams = std::mem::take(env.ext.get_mut::<Streams>());
    let mut next = None;
    for stream in streams.devices.values_mut() {
        if stream.paused.is_some() {
            continue;
        }
        while let Some(header) = stream.headers.front_mut() {
            if header.offset + 12 > header.length {
                let header = stream.headers.pop_front().unwrap();
                finish(env, stream.callback, header.at)?;
                continue;
            }
            let at = header.data + header.offset;
            if header.offset == 0 {
                let id = read(env, at + 4)?;
                if id != 0 && id != u32::MAX && id != stream.id {
                    let header = stream.headers.pop_front().unwrap();
                    finish(env, stream.callback, header.at)?;
                    continue;
                }
            }
            let delta = read(env, at)?;
            if !header.scheduled {
                stream.due += convert_values(stream.tempo, stream.division, delta);
                header.scheduled = true;
            }
            if stream.due > time {
                next = Some(next.map_or(stream.due, |n: u64| n.min(stream.due)));
                break;
            }
            let event = read(env, at + 8)?;
            let param = event & 0xffffff;
            match (event & !0x4000_0000) >> 24 {
                0 => adv_drv::audio::midi::native_event(env, stream.due, MidiEvent::Short(param))?,
                1 => stream.tempo = param,
                0x80 => {
                    let mut bytes = vec![0; param as usize];
                    env.cpu
                        .read(at + 12, &mut bytes)
                        .map_err(|e| e.to_string())?;
                    if let Some(event) = MidiEvent::sysex(&bytes) {
                        adv_drv::audio::midi::native_event(env, stream.due, event)?;
                    }
                }
                _ => {}
            }
            env.ext.get_mut::<Stats>().stream_events += 1;
            stream.pulses += u64::from(delta);
            stream.position_us = stream.due.saturating_sub(stream.origin);
            env.ext.get_mut::<Stats>().stream_time_us = stream.position_us;
            if event & 0x4000_0000 != 0 {
                write(env, header.at + 28, header.offset)?;
                notify(
                    env,
                    Notice {
                        callback: stream.callback,
                        message: 0x3ca,
                        param: header.at,
                    },
                )?;
            }
            header.offset += 12;
            if event & 0x8000_0000 != 0 {
                header.offset += (param + 3) & !3;
            }
            header.scheduled = false;
        }
    }
    *env.ext.get_mut::<Streams>() = streams;
    Ok(next)
}
fn convert_values(tempo: u32, division: u32, pulses: u32) -> u64 {
    if division == 0 {
        0
    } else if division > 0x8000 {
        let frames = 256 - ((division >> 8) & 255);
        u64::from(pulses.wrapping_mul(1_000_000) / (frames * (division & 255)))
    } else {
        (f64::from(pulses) * f64::from(tempo) / f64::from(division)) as u32 as u64
    }
}
