//! The parser read handshake and per-stream demand, following wg_parser.c.
use super::*;

pub(super) fn tracks(env: &mut Env<'_>, source_id: u32, source: &mut Source) {
    let Some(info) = &source.info else {
        return;
    };
    if !source.tracks.is_empty() {
        return;
    }
    for (index, track) in info.tracks.iter().enumerate() {
        let handle = id(env);
        env.ext
            .get_mut::<State>()
            .streams
            .insert(handle, (source_id, index));
        let format = format::wg(&track.codec, source.compressed);
        source.tracks.push(Track {
            handle,
            format,
            output: format::output(&format, 0),
            enabled: true,
            pending: false,
            eof: false,
            samples: VecDeque::new(),
            offset: 0,
            discontinuity: true,
        });
    }
}
pub(super) fn request(source: &mut Source, index: usize) -> Result<(), String> {
    let track = &mut source.tracks[index];
    if track.enabled && !track.pending && !track.eof && track.samples.is_empty() {
        track.pending = true;
        let compressed = matches!(track.output, Output::Compressed);
        source.send(Command::Next(index, compressed))?;
    }
    Ok(())
}

pub(super) fn call(env: &mut Env<'_>, code: u32, args: u32) -> Outcome {
    if code == 1 {
        let result = (|| {
            let compressed = read(env, args + 8)? & 0xff != 0;
            let source = Source::new(env, compressed);
            let id = id(env);
            env.ext.get_mut::<State>().sources.insert(id, source);
            put_wide(env, args, u64::from(id))?;
            Ok(0)
        })();
        return finish(result);
    }
    let handle = match read(env, args) {
        Ok(h) => h,
        Err(e) => return Outcome::Fail(e),
    };
    let (source_id, index) = if matches!(code, 2..=8 | 13) {
        (handle, 0)
    } else {
        match env.ext.get::<State>().streams.get(&handle) {
            Some(pair) => *pair,
            None => return Outcome::Return(WRONG_STATE),
        }
    };
    let Some(mut source) = env.ext.get_mut::<State>().sources.remove(&source_id) else {
        return Outcome::Return(WRONG_STATE);
    };
    let result = execute(env, code, args, source_id, index, &mut source);
    if code == 2 {
        for track in &source.tracks {
            env.ext.get_mut::<State>().streams.remove(&track.handle);
        }
        env.sys.objects.close(source.read_event);
        env.sys.objects.close(source.work_event);
    } else {
        env.ext.get_mut::<State>().sources.insert(source_id, source);
    }
    match result {
        Ok(outcome) => outcome,
        Err(e) => Outcome::Fail(format!("{}: {e}", NAMES[code as usize])),
    }
}
fn execute(
    env: &mut Env<'_>,
    code: u32,
    args: u32,
    source_id: u32,
    index: usize,
    source: &mut Source,
) -> Result<Outcome, String> {
    let mut status = 0;
    match code {
        2 => {
            source.worker = None;
        }
        3 => {
            if source.worker.is_none() {
                let wake = env
                    .ext
                    .get::<State>()
                    .wake
                    .clone()
                    .ok_or("media wake absent")?;
                source.connected = true;
                match Worker::parser(wide(env, args + 16)?, wake) {
                    Ok(worker) => source.worker = Some(worker),
                    Err(_) => return Ok(Outcome::Return(FAIL)),
                }
            }
            if source.error.is_some() {
                return Ok(Outcome::Return(FAIL));
            }
            if source.info.is_none() {
                return Ok(wait(env, source.work_event, code, args));
            }
            tracks(env, source_id, source);
        }
        4 => {
            source.connected = false;
            source.worker = None;
            source.tracks.iter_mut().for_each(|track| {
                track.samples.clear();
                track.eof = true;
            });
            env.sys.set_event(source.read_event);
            env.sys.set_event(source.work_event);
        }
        5 => {
            if !source.connected {
                status = WRONG_STATE;
            } else if let Some((offset, size)) = source.read {
                write(env, args + 8, size)?;
                put_wide(env, args + 16, offset)?;
            } else {
                return Ok(wait(env, source.read_event, code, args));
            }
        }
        6 => {
            let at = read(env, args + 8)?;
            let size = read(env, args + 12)?;
            let data = if at == 0 {
                Vec::new()
            } else {
                bytes(env, at, size as usize)?
            };
            source.read = None;
            if let Some(worker) = &source.worker {
                worker.push(data).map_err(|e| e.to_string())?;
            }
        }
        7 => write(env, args + 8, source.tracks.len() as u32)?,
        8 => {
            let index = read(env, args + 8)? as usize;
            put_wide(env, args + 16, u64::from(source.tracks[index].handle))?;
        }
        9 | 10 => {
            let fmt = if code == 10 {
                format::wg(&source.info.as_ref().unwrap().tracks[index].codec, true)
            } else {
                source.tracks[index].format
            };
            env.cpu
                .write(read(env, args + 8)?, &fmt)
                .map_err(|e| e.to_string())?;
        }
        11 => {
            let format = bytes(env, read(env, args + 8)?, 120)?;
            let track = &mut source.tracks[index];
            track.output = format::output(&format, 0);
            track.enabled = true;
            track.format.copy_from_slice(&format);
        }
        12 => {
            source.tracks[index].enabled = false;
            source.tracks[index].samples.clear();
        }
        13 => {
            if source.waiting == Some(19) {
                return Ok(wait(env, source.work_event, code, args));
            }
            if source.error.is_some() {
                return Ok(Outcome::Return(FAIL));
            }
            let handle = read(env, args + 8)?;
            let selected = if handle != 0 {
                let index = env.ext.get::<State>().streams[&handle].1;
                request(source, index)?;
                if source.tracks[index].pending {
                    return Ok(wait(env, source.work_event, code, args));
                }
                source.tracks[index]
                    .enabled
                    .then(|| source.tracks[index].samples.front().map(|_| index))
                    .flatten()
            } else {
                for index in 0..source.tracks.len() {
                    request(source, index)?;
                }
                if source
                    .tracks
                    .iter()
                    .any(|track| track.enabled && track.pending)
                {
                    return Ok(wait(env, source.work_event, code, args));
                }
                source
                    .tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, track)| track.enabled)
                    .filter_map(|(i, track)| track.samples.front().map(|sample| (i, sample.pts)))
                    .min_by_key(|(_, pts)| *pts)
                    .map(|(index, _)| index)
            };
            if let Some(index) = selected {
                if source.stop.is_some_and(|stop| {
                    source.tracks[index].samples.front().unwrap().pts >= stop as i64
                }) {
                    source.tracks[index].samples.clear();
                    source.tracks[index].eof = true;
                    return Ok(Outcome::Return(1));
                }
                let track = &source.tracks[index];
                let sample = track.samples.front().unwrap();
                let out = read(env, args + 16)?;
                put_wide(env, out, sample.pts.max(0) as u64)?;
                let duration = if sample.duration > 0 {
                    sample.duration as u64
                } else {
                    match &source.info.as_ref().unwrap().tracks[index].codec {
                        adv_device::media::CodecFormat::Video { frame_100ns, .. } => *frame_100ns,
                        _ => 0,
                    }
                };
                put_wide(env, out + 8, duration)?;
                write(
                    env,
                    out + 16,
                    sample.size(track.output).map_err(|e| e.to_string())? as u32,
                )?;
                write(env, out + 20, index as u32)?;
                env.cpu
                    .write(
                        out + 24,
                        &[
                            u8::from(track.discontinuity),
                            0,
                            u8::from(!sample.key),
                            1,
                            u8::from(duration > 0),
                            0,
                            0,
                            0,
                        ],
                    )
                    .map_err(|e| e.to_string())?;
            } else {
                status = 1;
            }
        }
        14 => {
            let track = &source.tracks[index];
            if let Some(sample) = track.samples.front() {
                let size = sample.size(track.output).map_err(|e| e.to_string())?;
                let mut data = vec![0; size];
                sample
                    .copy(track.output, &mut data)
                    .map_err(|e| e.to_string())?;
                let offset = read(env, args + 12)? as usize;
                let size = read(env, args + 16)? as usize;
                env.cpu
                    .write(read(env, args + 8)?, &data[offset..offset + size])
                    .map_err(|e| e.to_string())?;
            } else {
                status = WRONG_STATE;
            }
        }
        15 => {
            let track = &mut source.tracks[index];
            track.samples.pop_front();
            track.discontinuity = false;
        }
        16 => {}
        17 => put_wide(env, args + 8, source.info.as_ref().unwrap().duration)?,
        18 => {
            let tag = wide(env, args + 8)?;
            if tag > 1 {
                status = 0xc000_000d;
            } else {
                let track = &source.info.as_ref().unwrap().tracks[index];
                if let Some(value) = if tag == 0 {
                    &track.language
                } else {
                    &track.name
                } {
                    let needed = value.len() as u32 + 1;
                    let size = read(env, args + 20)?;
                    if read(env, size)? < needed {
                        write(env, size, needed)?;
                        status = BUFFER_SMALL;
                    } else {
                        let mut value = value.as_bytes().to_vec();
                        value.push(0);
                        env.cpu
                            .write(read(env, args + 16)?, &value)
                            .map_err(|e| e.to_string())?;
                    }
                } else {
                    status = NOT_FOUND;
                }
            }
        }
        19 => {
            if source.waiting == Some(code) && source.done {
                source.done = false;
                source.waiting = None;
            } else if source.busy != 0 {
                return Ok(wait(env, source.work_event, code, args));
            } else {
                if read(env, args + 32)? & 3 != 0 {
                    source.start = wide(env, args + 16)?;
                }
                if read(env, args + 36)? & 3 != 0 {
                    source.stop = Some(wide(env, args + 24)?);
                }
                source.send(Command::Seek(source.start))?;
                source.busy = 1;
                source.done = false;
                source.waiting = Some(code);
                for track in &mut source.tracks {
                    track.samples.clear();
                    track.pending = false;
                    track.eof = false;
                    track.discontinuity = true;
                }
                return Ok(wait(env, source.work_event, code, args));
            }
        }
        _ => unreachable!(),
    }
    Ok(Outcome::Return(status))
}
