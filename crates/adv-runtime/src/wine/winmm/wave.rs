use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(super) struct Format {
    pub channels: u16,
    pub rate: u32,
    pub bits: u16,
    pub tag: u16,
    pub align: u16,
    pub bytes_sec: u32,
}
impl Format {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 16 {
            return None;
        }
        let word = |at| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let dword = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let mut tag = word(0);
        if tag == 0xfffe {
            if bytes.len() < 40
                || word(16) < 22
                || bytes[26..40] != [0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]
            {
                return None;
            }
            tag = word(24);
        }
        let f = Self {
            channels: word(2),
            rate: dword(4),
            bytes_sec: dword(8),
            align: word(12),
            bits: word(14),
            tag,
        };
        if !(1..=8).contains(&f.channels)
            || f.rate == 0
            || u32::from(f.align) != u32::from(f.channels) * u32::from(f.bits) / 8
            || u64::from(f.bytes_sec) != u64::from(f.rate) * u64::from(f.align)
        {
            return None;
        }
        ((tag == 1 && matches!(f.bits, 8 | 16 | 24 | 32)) || (tag == 3 && f.bits == 32))
            .then_some(f)
    }
    pub fn samples(self, bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(usize::from(self.bits / 8))
            .map(|b| match (self.tag, self.bits) {
                (3, 32) => f32::from_le_bytes(b.try_into().unwrap()),
                (_, 8) => (f32::from(b[0]) - 128.0) / 128.0,
                (_, 16) => f32::from(i16::from_le_bytes(b.try_into().unwrap())) / 32768.0,
                (_, 24) => {
                    ((i32::from(b[0]) << 8 | i32::from(b[1]) << 16 | i32::from(b[2]) << 24) as f32)
                        / 2147483648.0
                }
                _ => i32::from_le_bytes(b.try_into().unwrap()) as f32 / 2147483648.0,
            })
            .collect()
    }
}
struct Header {
    at: u32,
    samples: Vec<f32>,
    flags: u32,
    loops: u32,
}
struct Span {
    start: u64,
    end: u64,
    frames: u64,
}
struct Wave {
    format: Format,
    device: u32,
    callback: Callback,
    headers: Vec<Header>,
    index: usize,
    offset: usize,
    loop_start: Option<usize>,
    loops: u32,
    tail: u64,
    spans: VecDeque<Span>,
    done: VecDeque<(u64, u32)>,
    played: u64,
    paused: Option<u64>,
    volume: u32,
}
#[derive(Default)]
pub(super) struct Waves {
    next: u32,
    devices: BTreeMap<u32, Wave>,
}

pub(super) fn call(env: &mut Env<'_>, name: &str, args: Vec<u32>) -> Outcome {
    if name == "waveOutOpen" {
        let callback = args[3];
        let flags = args[5];
        return check_callback(env, callback, flags, Box::new(move |env| open(env, args)));
    }
    if name == "waveOutGetNumDevs" {
        return Outcome::Return(1);
    }
    if name.contains("Pitch") || name.contains("PlaybackRate") {
        return Outcome::Return(8);
    }
    if name.contains("DevCaps") || name == "waveOutMessage" {
        let mut args = args;
        if let Some(wave) = env.ext.get::<Waves>().devices.get(&args[0]) {
            args[0] = if name == "waveOutMessage" {
                0
            } else {
                wave.device
            };
        }
        return forward(env, name, args);
    }
    if !env.ext.get::<Waves>().devices.contains_key(&args[0]) {
        return forward(env, name, args);
    }
    match result(env, name, &args) {
        Ok((value, Some(notice))) => deliver(env, notice, value),
        Ok((value, None)) => Outcome::Return(value),
        Err(error) => Outcome::Fail(error),
    }
}
fn result(env: &mut Env<'_>, name: &str, a: &[u32]) -> Result<(u32, Option<Notice>), String> {
    pump(env)?;
    let audio = env.ext.get::<AudioControl>().clone();
    let time = now(env);
    let id = source(env, a[0]);
    let mut waves = std::mem::take(env.ext.get_mut::<Waves>());
    let wave = waves.devices.get_mut(&a[0]).unwrap();
    let result = match name {
        "waveOutClose" => {
            if wave.headers.iter().any(|h| h.at != 0) {
                33
            } else {
                audio.remove_pcm(id);
                let callback = wave.callback;
                waves.devices.remove(&a[0]);
                *env.ext.get_mut::<Waves>() = waves;
                return Ok((
                    0,
                    Some(Notice {
                        callback,
                        message: 0x3bc,
                        param: 0,
                    }),
                ));
            }
        }
        "waveOutPrepareHeader" | "waveOutUnprepareHeader" => {
            if a[1] == 0 || a[2] < 32 {
                11
            } else {
                let flags = read(env, a[1] + 16)?;
                if name == "waveOutUnprepareHeader" && flags & 16 != 0 {
                    33
                } else {
                    let flags = if name == "waveOutPrepareHeader" {
                        if flags & 2 != 0 {
                            flags
                        } else {
                            (flags | 2) & !17
                        }
                    } else {
                        flags & !2
                    };
                    write(env, a[1] + 16, flags)?;
                    0
                }
            }
        }
        "waveOutWrite" => {
            let flags = read(env, a[1] + 16)?;
            if read(env, a[1])? == 0 || flags & 2 == 0 {
                34
            } else if flags & 16 != 0 {
                33
            } else {
                let length = read(env, a[1] + 4)? / u32::from(wave.format.align)
                    * u32::from(wave.format.align);
                let mut bytes = vec![0; length as usize];
                env.cpu
                    .read(read(env, a[1])?, &mut bytes)
                    .map_err(|e| e.to_string())?;
                if let Some(last) = wave.headers.last() {
                    write(env, last.at + 24, a[1])?;
                }
                write(env, a[1] + 24, 0)?;
                write(env, a[1] + 16, (flags | 16) & !1)?;
                wave.headers.push(Header {
                    at: a[1],
                    samples: wave.format.samples(&bytes),
                    flags,
                    loops: read(env, a[1] + 20)?,
                });
                env.ext.get_mut::<Stats>().wave_headers += 1;
                0
            }
        }
        "waveOutPause" => {
            if wave.paused.is_none() {
                wave.paused = Some(time);
                audio.pause_pcm(id, true, env.sys.clock.now_us());
            }
            0
        }
        "waveOutRestart" => {
            if let Some(at) = wave.paused.take() {
                let delta = time.saturating_sub(at);
                wave.tail += delta;
                for span in &mut wave.spans {
                    span.start += delta;
                    span.end += delta;
                }
                for (due, _) in &mut wave.done {
                    *due += delta;
                }
                audio.pause_pcm(id, false, env.sys.clock.now_us());
            }
            0
        }
        "waveOutReset" => {
            audio.remove_pcm(id);
            for header in &wave.headers {
                if header.at != 0 {
                    complete(env, wave.callback, header.at)?;
                }
            }
            wave.headers.clear();
            wave.spans.clear();
            wave.done.clear();
            wave.index = 0;
            wave.offset = 0;
            wave.loop_start = None;
            wave.loops = 0;
            wave.played = 0;
            wave.tail = time;
            wave.paused = None;
            0
        }
        "waveOutBreakLoop" => {
            wave.loops = 0;
            0
        }
        "waveOutGetPosition" => {
            if a[1] == 0 || a[2] == 0 {
                11
            } else if a[2] < 12 {
                1
            } else {
                let time = wave.paused.unwrap_or(time);
                let frames = wave.played
                    + wave
                        .spans
                        .iter()
                        .map(|s| {
                            if time >= s.end {
                                s.frames
                            } else if time > s.start {
                                (time - s.start) * s.frames / (s.end - s.start).max(1)
                            } else {
                                0
                            }
                        })
                        .sum::<u64>();
                let kind = read(env, a[1])?;
                if kind == 2 {
                    write(env, a[1] + 4, frames as u32)?;
                } else {
                    write(env, a[1], 4)?;
                    write(
                        env,
                        a[1] + 4,
                        ((frames * u64::from(wave.format.bytes_sec)
                            + u64::from(wave.format.rate) / 2)
                            / u64::from(wave.format.rate)) as u32,
                    )?;
                }
                0
            }
        }
        "waveOutGetID" => {
            if a[1] == 0 {
                11
            } else {
                write(env, a[1], wave.device)?;
                0
            }
        }
        "waveOutGetVolume" => {
            if a[1] == 0 {
                11
            } else {
                write(env, a[1], wave.volume)?;
                0
            }
        }
        "waveOutSetVolume" => {
            wave.volume = a[1];
            audio.set_stereo_gain(
                id,
                [
                    (a[1] & 65535) as f32 / 65535.0,
                    (a[1] >> 16) as f32 / 65535.0,
                ],
            );
            0
        }
        _ => 8,
    };
    *env.ext.get_mut::<Waves>() = waves;
    Ok((result, None))
}
fn complete(env: &mut Env<'_>, callback: Callback, at: u32) -> Result<(), String> {
    if at == 0 {
        return Ok(());
    }
    let flags = read(env, at + 16)?;
    write(env, at + 16, (flags | 1) & !16)?;
    env.ext.get_mut::<Stats>().wave_completed += 1;
    if env.sys.contexts.iter().all(|c| !c.is_ready()) {
        env.ext.get_mut::<Stats>().wave_completed_while_blocked += 1;
    }
    notify(
        env,
        Notice {
            callback,
            message: 0x3bd,
            param: at,
        },
    )
}

pub(super) fn pump(env: &mut Env<'_>) -> Result<Option<u64>, String> {
    let audio = env.ext.get::<AudioControl>().clone();
    let now = now(env);
    let guest = env.sys.clock.now_us();
    let mut waves = std::mem::take(env.ext.get_mut::<Waves>());
    let mut next = None;
    for (&handle, wave) in &mut waves.devices {
        if wave.paused.is_some() {
            continue;
        }
        while wave.spans.front().is_some_and(|s| s.end <= now) {
            wave.played += wave.spans.pop_front().unwrap().frames;
        }
        while wave.done.front().is_some_and(|&(due, _)| due <= now) {
            let (_, at) = wave.done.pop_front().unwrap();
            complete(env, wave.callback, at)?;
            if let Some(header) = wave.headers.iter_mut().find(|h| h.at == at) {
                header.at = 0;
            }
        }
        if wave.spans.is_empty() && wave.done.is_empty() && wave.index == wave.headers.len() {
            wave.headers.clear();
            wave.index = 0;
        }
        wave.tail = wave.tail.max(now);
        let channels = usize::from(wave.format.channels);
        while wave.tail < now + LEAD && wave.index < wave.headers.len() {
            let header = &wave.headers[wave.index];
            if wave.offset == 0 && header.flags & 4 != 0 && wave.loop_start.is_none() {
                wave.loop_start = Some(wave.index);
                wave.loops = header.loops;
            }
            let remaining = (header.samples.len() - wave.offset) / channels;
            let frames = remaining.min(
                ((now + LEAD - wave.tail) * u64::from(wave.format.rate) / 1_000_000).max(1)
                    as usize,
            );
            if frames != 0 {
                let end = wave.offset + frames * channels;
                audio
                    .push_pcm(adv_device::mixer::PcmBlock {
                        stream_id: source(env, handle),
                        guest_pts_us: guest.saturating_add(wave.tail.saturating_sub(now)),
                        sample_rate: wave.format.rate,
                        channels: wave.format.channels,
                        samples: header.samples[wave.offset..end].to_vec(),
                    })
                    .map_err(|_| "waveOut PCM queue limit")?;
                audio.set_stereo_gain(
                    source(env, handle),
                    [
                        (wave.volume & 65535) as f32 / 65535.0,
                        (wave.volume >> 16) as f32 / 65535.0,
                    ],
                );
                let duration = frames as u64 * 1_000_000 / u64::from(wave.format.rate);
                wave.spans.push_back(Span {
                    start: wave.tail,
                    end: wave.tail + duration,
                    frames: frames as u64,
                });
                wave.tail += duration;
                wave.offset = end;
            }
            if wave.offset < header.samples.len() {
                break;
            }
            if header.flags & 8 != 0
                && let Some(start) = wave.loop_start
            {
                if wave.loops > 1
                    && wave.headers[start..=wave.index]
                        .iter()
                        .any(|h| !h.samples.is_empty())
                {
                    wave.loops -= 1;
                    wave.index = start;
                    wave.offset = 0;
                    continue;
                }
                for h in &wave.headers[start..=wave.index] {
                    wave.done.push_back((wave.tail, h.at));
                }
                wave.loop_start = None;
            } else if wave.loop_start.is_none() {
                wave.done.push_back((wave.tail, header.at));
            }
            wave.index += 1;
            wave.offset = 0;
        }
        if wave.index < wave.headers.len() || !wave.done.is_empty() {
            let due = wave
                .done
                .front()
                .map_or(now + 10_000, |&(due, _)| due.min(now + 10_000));
            let deadline = guest + due.saturating_sub(now);
            next = Some(next.map_or(deadline, |n: u64| n.min(deadline)));
        }
    }
    *env.ext.get_mut::<Waves>() = waves;
    Ok(next)
}

pub(super) fn play_sound(env: &mut Env<'_>, format: Format, bytes: &[u8], looping: bool) -> u32 {
    let time = now(env);
    let waves = env.ext.get_mut::<Waves>();
    waves.next += 1;
    let handle = 0x6000_0000 | waves.next;
    let callback = Callback {
        handle,
        target: 0,
        instance: 0,
        flags: 0,
    };
    waves.devices.insert(
        handle,
        Wave {
            format,
            device: u32::MAX,
            callback,
            headers: vec![Header {
                at: 0,
                samples: format.samples(bytes),
                flags: if looping { 12 } else { 0 },
                loops: if looping { u32::MAX } else { 0 },
            }],
            index: 0,
            offset: 0,
            loop_start: None,
            loops: 0,
            tail: time,
            spans: VecDeque::new(),
            done: VecDeque::new(),
            played: 0,
            paused: None,
            volume: u32::MAX,
        },
    );
    env.ext.get_mut::<Stats>().wave_headers += 1;
    handle
}
pub(super) fn sound_finished(env: &Env<'_>, handle: u32) -> bool {
    env.ext
        .get::<Waves>()
        .devices
        .get(&handle)
        .is_none_or(|w| w.index == w.headers.len() && w.done.is_empty())
}
pub(super) fn discard_sound(env: &mut Env<'_>, handle: u32) {
    env.ext.get_mut::<Waves>().devices.remove(&handle);
    env.ext
        .get::<AudioControl>()
        .remove_pcm(source(env, handle));
}

fn open(env: &mut Env<'_>, args: Vec<u32>) -> Outcome {
    let mut bytes = [0; 40];
    if env.cpu.read(args[2], &mut bytes[..16]).is_err() {
        return Outcome::Return(11);
    }
    if u16::from_le_bytes([bytes[0], bytes[1]]) == 0xfffe
        && env.cpu.read(args[2], &mut bytes).is_err()
    {
        return Outcome::Return(11);
    }
    let Some(format) = Format::parse(&bytes) else {
        return forward(env, "waveOutOpen", args);
    };
    if !matches!(args[1], 0 | 0xffff | u32::MAX) {
        return Outcome::Return(2);
    }
    if args[5] & 1 != 0 {
        return Outcome::Return(0);
    }
    if args[0] == 0 {
        return Outcome::Return(11);
    }
    let now = now(env);
    let waves = env.ext.get_mut::<Waves>();
    waves.next += 1;
    let handle = 0x6000_0000 | waves.next;
    let callback = Callback {
        handle,
        target: args[3],
        instance: args[4],
        flags: args[5] & 0x70000,
    };
    waves.devices.insert(
        handle,
        Wave {
            format,
            device: if args[1] == 0 { 0 } else { u32::MAX },
            callback,
            headers: Vec::new(),
            index: 0,
            offset: 0,
            loop_start: None,
            loops: 0,
            tail: now,
            spans: VecDeque::new(),
            done: VecDeque::new(),
            played: 0,
            paused: None,
            volume: u32::MAX,
        },
    );
    if let Err(error) = write(env, args[0], handle) {
        return Outcome::Fail(error);
    }
    start_worker(
        env,
        Notice {
            callback,
            message: 0x3bb,
            param: 0,
        },
    )
}

pub(super) fn is_native(env: &Env<'_>, handle: u32) -> bool {
    env.ext.get::<Waves>().devices.contains_key(&handle)
}
