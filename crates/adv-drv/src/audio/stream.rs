//! Wine render stream buffers, playback cursors and timer-thread continuations.
use super::*;
use adv_machine::sched::Wait;

/// Refresh from the mixer's cursor at the playback clock, before exposing space.
fn advance(env: &mut Env<'_>, id: u32, stream: &mut Stream) -> Result<(), String> {
    let audio = env.ext.get::<AudioControl>().clone();
    let now = audio.playback_clock().now_us(env.sys.clock.now_us());
    if stream.running {
        let align = stream.format.mixer_align();
        if let Some(cursor) = stream.voice.cursor(now, stream.generation) {
            let cursor = cursor / align;
            let elapsed =
                now.saturating_sub(stream.observed) * u64::from(stream.format.rate) / 1_000_000;
            let frames = if elapsed >= u64::from(stream.frames) {
                stream.held
            } else {
                ((cursor + stream.frames - stream.cursor) % stream.frames).min(stream.held)
            };
            let first = frames.min(stream.frames - stream.cursor);
            let silent = if stream.format.mixer_bits() == 8 {
                128
            } else {
                0
            };
            silence(
                env,
                stream.mix + stream.cursor * align,
                first * align,
                silent,
            )?;
            if first != frames {
                silence(env, stream.mix, (frames - first) * align, silent)?;
            }
            stream.held -= frames;
            stream.cursor = cursor;
            if stream.held == 0 {
                if !audio.stop_ring(source_id(env.sys.pid, id))
                    || !audio.set_position(source_id(env.sys.pid, id), stream.write * align)
                {
                    return Err("audio command limit".into());
                }
                stream.cursor = stream.write;
                stream.running = false;
            }
        }
    }
    stream.observed = now;
    Ok(())
}

fn play(env: &mut Env<'_>, id: u32, stream: &mut Stream) -> Result<(), String> {
    if stream.playing && !stream.running && stream.held != 0 {
        stream.generation += 1;
        if !env.ext.get::<AudioControl>().play_ring(
            source_id(env.sys.pid, id),
            true,
            stream.generation,
        ) {
            return Err("audio command limit".into());
        }
        stream.running = true;
        stream.observed = env
            .ext
            .get::<AudioControl>()
            .playback_clock()
            .now_us(env.sys.clock.now_us());
    }
    Ok(())
}

pub(super) fn call(
    env: &mut Env<'_>,
    code: u32,
    args: u32,
    id: u32,
    s: &mut Stream,
) -> Result<(), String> {
    advance(env, id, s)?;
    let audio = env.ext.get::<AudioControl>().clone();
    let mut hr = 0;
    let mut result = 8;
    match code {
        6 => {
            if s.flags & EVENT_CALLBACK != 0 && s.event == 0 {
                hr = 0x8889_0014;
            } else if s.playing {
                hr = NOT_STOPPED;
            } else {
                s.playing = true;
                play(env, id, s)?;
            }
        }
        7 => {
            if !s.playing {
                hr = 1;
            } else {
                s.playing = false;
                if !audio.stop_ring(source_id(env.sys.pid, id)) {
                    return Err("audio command limit".into());
                }
                s.running = false;
                if !audio.set_position(
                    source_id(env.sys.pid, id),
                    s.cursor * s.format.mixer_align(),
                ) {
                    return Err("audio command limit".into());
                }
            }
        }
        8 => {
            if s.playing {
                hr = NOT_STOPPED;
            } else if s.pending != 0 {
                hr = 0x8889_000b;
            } else {
                s.held = 0;
                s.written = 0;
                s.write = 0;
                s.cursor = 0;
                silence(
                    env,
                    s.mix,
                    s.frames * s.format.mixer_align(),
                    if s.format.mixer_bits() == 8 { 128 } else { 0 },
                )?;
                if !audio.set_position(source_id(env.sys.pid, id), 0) {
                    return Err("audio command limit".into());
                }
            }
        }
        10 => {
            result = 12;
            let frames = read(env, args + 8)?;
            if s.pending != 0 {
                hr = OUT_OF_ORDER;
            } else if frames == 0 {
            } else if frames + s.held > s.frames {
                hr = 0x8889_0006;
            } else {
                let wrap = s.write + frames > s.frames;
                let out = if wrap {
                    s.temporary
                } else {
                    s.ring + s.write * s.format.align
                };
                silence(
                    env,
                    out,
                    frames * s.format.align,
                    if s.format.tag == 1 && s.format.bits == 8 {
                        128
                    } else {
                        0
                    },
                )?;
                write(env, read(env, args + 16)?, out)?;
                s.pending = if wrap {
                    -(frames as i32)
                } else {
                    frames as i32
                };
            }
        }
        11 => {
            result = 16;
            let frames = read(env, args + 8)?;
            if frames == 0 {
                s.pending = 0;
            } else if s.pending == 0 {
                hr = OUT_OF_ORDER;
            } else if frames > s.pending.unsigned_abs() {
                hr = 0x8889_0009;
            } else {
                let at = if s.pending < 0 {
                    s.temporary
                } else {
                    s.ring + s.write * s.format.align
                };
                if read(env, args + 12)? & 2 != 0 {
                    silence(
                        env,
                        at,
                        frames * s.format.align,
                        if s.format.tag == 1 && s.format.bits == 8 {
                            128
                        } else {
                            0
                        },
                    )?;
                }
                let mut input = vec![0; (frames * s.format.align) as usize];
                env.cpu.read(at, &mut input).map_err(|e| e.to_string())?;
                let mut output =
                    Vec::with_capacity(frames as usize * usize::from(s.format.channels) * 2);
                if s.mix == s.ring {
                    output = input;
                } else {
                    for sample in input.chunks_exact(usize::from(s.format.bits / 8)) {
                        let value = match (s.format.tag, s.format.bits) {
                            (1, 8) => (i16::from(sample[0]) - 128) << 8,
                            (1, 16) => i16::from_le_bytes(sample.try_into().unwrap()),
                            (1, 24) => i16::from_le_bytes([sample[1], sample[2]]),
                            (1, 32) => i16::from_le_bytes([sample[2], sample[3]]),
                            (3, 32) => {
                                (f32::from_le_bytes(sample.try_into().unwrap()) * 32768.0) as i16
                            }
                            (3, 64) => {
                                (f64::from_le_bytes(sample.try_into().unwrap()) * 32768.0) as i16
                            }
                            _ => unreachable!(),
                        };
                        output.extend_from_slice(&value.to_le_bytes());
                    }
                }
                let align = s.format.mixer_align();
                let first = frames.min(s.frames - s.write);
                env.cpu
                    .write(s.mix + s.write * align, &output[..(first * align) as usize])
                    .map_err(|e| e.to_string())?;
                if first != frames {
                    env.cpu
                        .write(s.mix, &output[(first * align) as usize..])
                        .map_err(|e| e.to_string())?;
                }
                if !audio.written(source_id(env.sys.pid, id), s.write * align, frames * align) {
                    return Err("audio command limit".into());
                }
                s.write = (s.write + frames) % s.frames;
                s.held += frames;
                s.written += u64::from(frames);
                s.pending = 0;
                play(env, id, s)?;
            }
        }
        18 | 20 => write(
            env,
            read(env, args + 12)?,
            if code == 18 { s.frames } else { s.held },
        )?,
        19 => wide(env, read(env, args + 12)?, s.period)?,
        22 => wide(
            env,
            read(env, args + 12)?,
            u64::from(s.format.rate)
                * if s.share == 0 {
                    u64::from(s.format.align)
                } else {
                    1
                },
        )?,
        23 => {
            result = 12;
            if read(env, args + 8)? != 0 {
                hr = 0x8000_4001;
            } else {
                wide(
                    env,
                    read(env, args + 16)?,
                    (s.written - u64::from(s.held))
                        * if s.share == 0 {
                            u64::from(s.format.align)
                        } else {
                            1
                        },
                )?;
                let stamp = read(env, args + 20)?;
                if stamp != 0 {
                    wide(env, stamp, env.sys.clock.now_us() * 10)?;
                }
            }
        }
        24 => {
            let master = f32::from_bits(read(env, args + 8)?);
            let volumes = read(env, args + 12)?;
            let session = read(env, args + 16)?;
            let mut gain = [0.0; 2];
            for (index, gain) in gain.iter_mut().enumerate() {
                let offset = (index as u32 % u32::from(s.format.channels)) * 4;
                *gain = master
                    * f32::from_bits(read(env, volumes + offset)?)
                    * f32::from_bits(read(env, session + offset)?);
            }
            if !audio.set_stereo_gain(source_id(env.sys.pid, id), gain) {
                return Err("audio command limit".into());
            }
            return Ok(());
        }
        25 => {
            result = 12;
            if s.flags & EVENT_CALLBACK == 0 {
                hr = 0x8889_0011;
            } else if s.event != 0 {
                hr = 0x8007_007b;
            } else {
                s.event = read(env, args + 8)?;
            }
        }
        28 => hr = u32::from(!s.playing),
        _ => return Err(format!("unsupported stream entry {code}")),
    }
    write(env, args + result, hr)
}

pub(super) fn timer(env: &mut Env<'_>, id: u32, next: Option<u64>) -> Outcome {
    let result = (|| {
        let mut stream = env
            .ext
            .get_mut::<Streams>()
            .streams
            .remove(&id)
            .ok_or("unknown timer stream")?;
        if stream.quit {
            env.ext.get_mut::<Streams>().streams.insert(id, stream);
            return Ok(None);
        }
        advance(env, id, &mut stream)?;
        let period = stream.period / 10;
        let event = stream.event;
        env.ext.get_mut::<Streams>().streams.insert(id, stream);
        if event != 0 {
            match adv_nt::sync::set_event(env, &[event, 0]) {
                Outcome::Return(0) => {}
                _ => return Err("timer event failed".to_string()),
            }
        }
        let now = env.sys.clock.now_us();
        let target = next.unwrap_or(now);
        let adjust = (i128::from(target) - i128::from(now))
            .clamp(-i128::from(period / 2), i128::from(period / 2));
        Ok(Some((
            now + (i128::from(period) + adjust) as u64,
            target + period,
        )))
    })();
    match result {
        Ok(Some((deadline, next))) => Outcome::block(Wait::Until(deadline), move |env, _| {
            timer(env, id, Some(next))
        }),
        Ok(None) => Outcome::Return(0),
        Err(error) => Outcome::Fail(error),
    }
}

pub(super) fn release(env: &mut Env<'_>, args: u32) -> Outcome {
    let result = (|| {
        let id = read(env, args)?;
        let thread = read(env, args + 8)?;
        env.ext
            .get_mut::<Streams>()
            .streams
            .get_mut(&id)
            .ok_or("unknown released stream")?
            .quit = true;
        Ok((id, thread))
    })();
    let finish = move |env: &mut Env<'_>, id: u32, thread: u32| -> Outcome {
        let result = (|| {
            if thread != 0 {
                adv_nt::server::close(env, thread).map_err(|e| format!("{e:?}"))?;
            }
            let stream = env
                .ext
                .get_mut::<Streams>()
                .streams
                .remove(&id)
                .ok_or("unknown released stream")?;
            if !env
                .ext
                .get::<AudioControl>()
                .remove_pcm(source_id(env.sys.pid, id))
            {
                return Err("audio command limit".into());
            }
            for at in [stream.ring, stream.temporary] {
                env.sys.vm.free(env.cpu, at).map_err(|e| e.to_string())?;
            }
            if stream.mix != stream.ring {
                env.sys
                    .vm
                    .free(env.cpu, stream.mix)
                    .map_err(|e| e.to_string())?;
            }
            write(env, args + 12, 0)
        })();
        match result {
            Ok(()) => Outcome::Return(0),
            Err(error) => Outcome::Fail(error),
        }
    };
    match result {
        Ok((id, 0)) => finish(env, id, 0),
        Ok((id, thread)) => {
            let wait =
                adv_nt::process::module_base(env.cpu, env.sys.peb, "ntdll.dll").and_then(|base| {
                    adv_nt::process::module_export(env.cpu, base, "NtWaitForSingleObject")
                });
            match wait {
                Some(wait) => {
                    Outcome::call(wait, [thread, 0, 0], move |env, _| finish(env, id, thread))
                }
                None => Outcome::Fail("ntdll wait export missing".into()),
            }
        }
        Err(error) => Outcome::Fail(error),
    }
}
