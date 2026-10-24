//! DMO and MF sample transforms. Queue limits follow wg_transform.c.
use super::*;
const NEED_INPUT: u32 = 0xc00d_6d72;
const NOT_ACCEPTING: u32 = 0xc00d_36b0;

pub(super) fn call(env: &mut Env<'_>, code: u32, args: u32) -> Outcome {
    if code == 20 {
        return create(env, args);
    }
    let handle = match read(env, args) {
        Ok(h) => h,
        Err(e) => return Outcome::Fail(e),
    };
    let Some(mut source) = env.ext.get_mut::<State>().sources.remove(&handle) else {
        return Outcome::Return(UNSUCCESSFUL);
    };
    let result = execute(env, code, args, &mut source);
    if code == 21 {
        env.sys.objects.close(source.read_event);
        env.sys.objects.close(source.work_event);
    } else {
        env.ext.get_mut::<State>().sources.insert(handle, source);
    }
    match result {
        Ok(o) => o,
        Err(e) => Outcome::Fail(format!("{}: {e}", NAMES[code as usize])),
    }
}
fn create(env: &mut Env<'_>, args: u32) -> Outcome {
    let existing = read(env, args).unwrap_or(0);
    if let Some(source) = env.ext.get::<State>().sources.get(&existing) {
        if source.error.is_some() {
            return Outcome::Return(UNSUCCESSFUL);
        }
        if source.info.is_some() {
            return Outcome::Return(0);
        }
        let event = source.work_event;
        return wait(env, event, 20, args);
    }
    let result = (|| {
        let (codec, _, _, _) = format::parse_media(env, args + 8)?;
        let (_, mut output, major, data) = format::parse_media(env, args + 32)?;
        if let Output::Video { align, .. } = &mut output {
            *align = read(env, args + 56)? + 1;
        }
        let mut source = Source::new(env, false);
        let wake = env
            .ext
            .get::<State>()
            .wake
            .clone()
            .ok_or("media wake absent")?;
        let worker = match Worker::transform(codec, wake) {
            Ok(worker) => worker,
            Err(_) => return Ok(UNSUCCESSFUL),
        };
        source.worker = Some(worker);
        source.tracks.push(Track {
            handle: 0,
            format: [0; 120],
            output,
            enabled: true,
            pending: false,
            eof: false,
            samples: VecDeque::new(),
            offset: 0,
            discontinuity: true,
        });
        source.output_type = data;
        source.output_major = major;
        let handle = id(env);
        env.ext.get_mut::<State>().sources.insert(handle, source);
        put_wide(env, args, u64::from(handle))?;
        Ok(handle)
    })();
    match result {
        Ok(handle) if handle != UNSUCCESSFUL => {
            let event = env.ext.get::<State>().sources[&handle].work_event;
            wait(env, event, 20, args)
        }
        _ => finish(result),
    }
}
fn execute(
    env: &mut Env<'_>,
    code: u32,
    args: u32,
    source: &mut Source,
) -> Result<Outcome, String> {
    let mut status = 0;
    match code {
        21 => source.worker = None,
        22 => {
            let size = source.output_type.len() as u32;
            let capacity = read(env, args + 24)?;
            write(env, args + 24, size)?;
            env.cpu
                .write(args + 8, &source.output_major)
                .map_err(|e| e.to_string())?;
            if capacity < size {
                status = BUFFER_SMALL;
            } else {
                env.cpu
                    .write(read(env, args + 28)?, &source.output_type)
                    .map_err(|e| e.to_string())?;
            }
        }
        23 => {
            let (_, output, major, data) = format::parse_media(env, args + 8)?;
            source.tracks[0].output = output;
            source.output_major = major;
            source.output_type = data;
        }
        24 => {
            let sample = read(env, args + 8)?;
            if source.busy != 0 || source.tracks[0].samples.len() >= 3 {
                write(env, args + 12, NOT_ACCEPTING)?;
            } else {
                let flags = read(env, sample + 20)?;
                let pts = (flags & 2 != 0)
                    .then(|| wide(env, sample))
                    .transpose()?
                    .map(|pts| pts as i64);
                let duration = if flags & 4 != 0 {
                    wide(env, sample + 8)? as i64
                } else {
                    0
                };
                let data = bytes(
                    env,
                    read(env, sample + 32)?,
                    read(env, sample + 28)? as usize,
                )?;
                source.send(Command::Decode(data, pts, duration))?;
                source.busy += 1;
                write(env, args + 12, 0)?;
            }
        }
        25 => {
            if source.busy != 0 {
                return Ok(wait(env, source.work_event, code, args));
            }
            let at = read(env, args + 8)?;
            if source.error.is_some() {
                return Ok(Outcome::Return(UNSUCCESSFUL));
            }
            let track = &mut source.tracks[0];
            if let Some(sample) = track.samples.front() {
                let size = sample.size(track.output).map_err(|e| e.to_string())?;
                let mut data = vec![0; size];
                let size = sample
                    .copy(track.output, &mut data)
                    .map_err(|e| e.to_string())?;
                let max = read(env, at + 24)? as usize;
                let count = max.min(size - track.offset);
                env.cpu
                    .write(
                        read(env, at + 32)?,
                        &data[track.offset..track.offset + count],
                    )
                    .map_err(|e| e.to_string())?;
                put_wide(env, at, sample.pts as u64)?;
                put_wide(env, at + 8, sample.duration.max(0) as u64)?;
                track.offset += count;
                let incomplete = track.offset != size;
                write(
                    env,
                    at + 20,
                    u32::from(incomplete) | 2 | 4 | if sample.key { 8 } else { 0 },
                )?;
                write(env, at + 28, count as u32)?;
                if !incomplete {
                    track.samples.pop_front();
                    track.offset = 0;
                }
                write(env, args + 12, 0)?;
            } else {
                write(env, at + 28, 0)?;
                write(env, args + 12, NEED_INPUT)?;
            }
        }
        26 => write(
            env,
            args + 8,
            u32::from(source.busy == 0 && source.tracks[0].samples.len() < 3),
        )?,
        27 | 28 => {
            if source.waiting == Some(code) && source.done {
                source.done = false;
                source.waiting = None;
            } else if source.busy != 0 {
                return Ok(wait(env, source.work_event, code, args));
            } else {
                source.send(if code == 27 {
                    Command::Drain
                } else {
                    Command::Flush
                })?;
                if code == 28 {
                    source.tracks[0].samples.clear();
                    source.tracks[0].offset = 0;
                }
                source.busy += 1;
                source.done = false;
                source.waiting = Some(code);
                return Ok(wait(env, source.work_event, code, args));
            }
        }
        29 => {}
        _ => unreachable!(),
    }
    Ok(Outcome::Return(status))
}
