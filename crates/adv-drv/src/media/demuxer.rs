//! winedmo's demuxer table and guest byte-stream callbacks.
use super::*;

pub(super) fn call(env: &mut Env<'_>, code: u32, args: u32) -> Outcome {
    if code == 64 {
        return Outcome::Return(0);
    }
    if code == 65 {
        return finish((|| {
            let mime = bytes(env, args, 256)?;
            let end = mime.iter().position(|b| *b == 0).unwrap_or(mime.len());
            Ok(
                if matches!(
                    &mime[..end],
                    b"video/mp4"
                        | b"audio/mp4"
                        | b"video/x-ms-asf"
                        | b"video/mpeg"
                        | b"audio/mpeg"
                        | b"video/avi"
                        | b"audio/wav"
                ) {
                    0
                } else {
                    0xc000_00bb
                },
            )
        })());
    }
    let handle = match read(env, if code == 66 { args + 8 } else { args }) {
        Ok(h) => h,
        Err(e) => return Outcome::Fail(e),
    };
    let handle = if code == 66 && handle == 0 {
        let result: Result<u32, String> = (|| {
            let mut source = Source::new(env, true);
            source.context = read(env, args + 4)?;
            let wake = env
                .ext
                .get::<State>()
                .wake
                .clone()
                .ok_or("media wake absent")?;
            source.worker = Some(
                Worker::parser(wide(env, source.context + 8)?, wake).map_err(|e| e.to_string())?,
            );
            source.connected = true;
            let handle = id(env);
            put_wide(env, args + 8, u64::from(handle))?;
            env.ext.get_mut::<State>().sources.insert(handle, source);
            Ok(handle)
        })();
        match result {
            Ok(handle) => handle,
            Err(_) => return Outcome::Return(UNSUCCESSFUL),
        }
    } else {
        handle
    };
    if let Some(outcome) = input(env, handle, code, args) {
        return outcome;
    }
    let Some(mut source) = env.ext.get_mut::<State>().sources.remove(&handle) else {
        return Outcome::Return(UNSUCCESSFUL);
    };
    let result = execute(env, code, args, handle, &mut source);
    if code == 67 {
        for track in &source.tracks {
            env.ext.get_mut::<State>().streams.remove(&track.handle);
        }
        env.sys.objects.close(source.read_event);
        env.sys.objects.close(source.work_event);
    } else {
        env.ext.get_mut::<State>().sources.insert(handle, source);
    }
    match result {
        Ok(o) => o,
        Err(e) => Outcome::Fail(format!("{}: {e}", DMO_NAMES[(code - 64) as usize])),
    }
}
fn input(env: &mut Env<'_>, handle: u32, code: u32, args: u32) -> Option<Outcome> {
    let source = env.ext.get::<State>().sources.get(&handle)?;
    let (offset, size) = source.read?;
    let context = source.context;
    let result = (|| {
        let stream = read(env, context)?;
        let seek = read(env, stream)?;
        let read_fn = read(env, stream + 4)?;
        let slot = crate::slot(env)?;
        put_wide(env, slot, offset)?;
        write(env, slot + 8, size)?;
        Ok((stream, seek, read_fn, slot))
    })();
    Some(match result {
        Err(error) => Outcome::Fail(error),
        Ok((stream, seek, read_fn, slot)) => {
            Outcome::call_aligned(env, seek, [stream, slot], move |env, returned| {
                if returned.value() != 0 {
                    crate::free_slot(env, slot);
                    return Outcome::Return(returned.value());
                }
                Outcome::call_aligned(
                    env,
                    read_fn,
                    [stream, context + 32, slot + 8],
                    move |env, returned| {
                        let result = (|| {
                            if returned.value() != 0 {
                                return Err("media stream read failed".into());
                            }
                            let data = bytes(env, context + 32, read(env, slot + 8)? as usize)?;
                            let source =
                                env.ext.get_mut::<State>().sources.get_mut(&handle).unwrap();
                            source.read = None;
                            source
                                .worker
                                .as_ref()
                                .unwrap()
                                .push(data)
                                .map_err(|e| e.to_string())
                        })();
                        crate::free_slot(env, slot);
                        match result {
                            Ok(()) => dispatch(env, code, args),
                            Err(e) => Outcome::Fail(e),
                        }
                    },
                )
            })
        }
    })
}
fn execute(
    env: &mut Env<'_>,
    code: u32,
    args: u32,
    handle: u32,
    source: &mut Source,
) -> Result<Outcome, String> {
    if source.error.is_some() {
        return Ok(Outcome::Return(UNSUCCESSFUL));
    }
    let mut status = 0;
    match code {
        66 => {
            let Some(info) = source.info.clone() else {
                return Ok(wait(env, source.work_event, code, args));
            };
            parser::tracks(env, handle, source);
            let mut mime = info.mime.as_bytes().to_vec();
            mime.push(0);
            env.cpu.write(args + 16, &mime).map_err(|e| e.to_string())?;
            write(env, args + 272, info.tracks.len() as u32)?;
            put_wide(env, args + 280, info.duration)?;
        }
        67 => {
            write(env, args + 8, source.context)?;
            source.worker = None;
        }
        68 => {
            if source.waiting == Some(69) {
                return Ok(wait(env, source.work_event, code, args));
            }
            for index in 0..source.tracks.len() {
                parser::request(source, index)?;
            }
            if source.tracks.iter().any(|track| track.pending) {
                return Ok(wait(env, source.work_event, code, args));
            }
            let selected = source
                .tracks
                .iter()
                .enumerate()
                .filter_map(|(index, track)| {
                    track.samples.front().map(|sample| (index, sample.pts))
                })
                .min_by_key(|(_, pts)| *pts);
            if let Some((index, _)) = selected {
                let track = &mut source.tracks[index];
                let sample = track.samples.front().unwrap();
                let size = sample.size(Output::Compressed).map_err(|e| e.to_string())?;
                let capacity = wide(env, args + 48)? as usize;
                put_wide(env, args + 48, size as u64)?;
                if size > capacity {
                    status = BUFFER_SMALL;
                } else {
                    let mut data = vec![0; size];
                    sample
                        .copy(Output::Compressed, &mut data)
                        .map_err(|e| e.to_string())?;
                    env.cpu
                        .write(read(env, args + 56)?, &data)
                        .map_err(|e| e.to_string())?;
                    write(env, args + 8, index as u32)?;
                    write(env, args + 16, u32::from(sample.key))?;
                    put_wide(env, args + 24, sample.pts as u64)?;
                    put_wide(env, args + 32, sample.pts as u64)?;
                    put_wide(env, args + 40, sample.duration as u64)?;
                    track.samples.pop_front();
                }
            } else {
                status = 0xc000_0011;
            }
        }
        69 => {
            if source.waiting == Some(code) && source.done {
                source.done = false;
                source.waiting = None;
            } else if source.busy != 0 {
                return Ok(wait(env, source.work_event, code, args));
            } else {
                source.send(Command::Seek(wide(env, args + 8)?))?;
                source.busy = 1;
                source.done = false;
                source.waiting = Some(code);
                for track in &mut source.tracks {
                    track.samples.clear();
                    track.pending = false;
                    track.eof = false;
                }
                return Ok(wait(env, source.work_event, code, args));
            }
        }
        70 | 71 => {
            let index = read(env, args + 8)? as usize;
            let track = &source.info.as_ref().unwrap().tracks[index];
            let tag = if code == 70 {
                &track.language
            } else {
                &track.name
            };
            if let Some(tag) = tag {
                let limit = if code == 70 { 31 } else { 255 };
                let mut tag = tag.as_bytes()[..tag.len().min(limit)].to_vec();
                tag.push(0);
                env.cpu.write(args + 12, &tag).map_err(|e| e.to_string())?;
            } else {
                status = NOT_FOUND;
            }
        }
        72 => {
            let index = read(env, args + 8)? as usize;
            let (major, data) = format::media(&source.info.as_ref().unwrap().tracks[index].codec);
            let capacity = read(env, args + 32)? as usize;
            write(env, args + 32, data.len() as u32)?;
            env.cpu
                .write(args + 16, &major)
                .map_err(|e| e.to_string())?;
            if capacity < data.len() {
                status = BUFFER_SMALL;
            } else {
                env.cpu
                    .write(read(env, args + 40)?, &data)
                    .map_err(|e| e.to_string())?;
            }
        }
        _ => unreachable!(),
    }
    Ok(Outcome::Return(status))
}
