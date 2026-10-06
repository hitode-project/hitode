//! Streaming: one guest thread per running splitter. It decodes at the graph's
//! media position, pushes video samples into the downstream (usually guest)
//! renderer and audio into the DirectSound mixer. Receive may block while the
//! graph is paused, which is why delivery happens on a thread of its own.
use std::collections::VecDeque;

use adv_device::media::{AudioFrame, VideoFrame};
use adv_device::mixer::PcmBlock;
use adv_machine::{Env, Handle, Object, Outcome, Wait};

use super::filter::{OUTPUT, Role};
use super::*;
use crate::dsound::Sound;

pub(crate) const THREAD_ENTRY: &str = "<DirectShowStreamThread>";
const POLL_US: u64 = 10_000;
const EC_COMPLETE: u32 = 1;
const EC_ERRORABORT: u32 = 3;
const E_FAIL: u32 = 0x8000_4005;

pub(crate) struct Session {
    pub graph: u32,
    splitter: u32,
    /// Host output pin connected to a guest input.
    video: Option<u32>,
    /// Mixer stream of a host audio renderer.
    audio: Option<u64>,
    wake: Handle,
    generation: u32,
    seen_generation: u32,
    stopping: bool,
    prerolled: bool,
    ended: bool,
    discontinuity: bool,
    segment: i64,
    pending_audio: VecDeque<AudioFrame>,
}

enum End {
    Guest(u32),
    AudioRenderer,
}

/// Follow host-to-host connections from a splitter output to its stream's end.
fn follow(env: &Env<'_>, mut out: u32) -> Option<End> {
    let state = env.ext.get::<State>();
    for _ in 0..8 {
        let pin = state.pins.get(&out)?;
        if pin.peer == 0 {
            return None;
        }
        let Some(peer) = state.pins.get(&pin.peer) else {
            return Some(End::Guest(out));
        };
        let filter = state.filters.get(&peer.filter)?;
        match filter.role {
            Role::AudioRenderer => return Some(End::AudioRenderer),
            Role::VideoDecoder | Role::AudioDecoder => {
                out = *filter.pins.iter().find(|p| state.pins[p].dir == OUTPUT)?;
            }
            Role::Splitter => return None,
        }
    }
    None
}

/// Start streaming a splitter; returns how many renderers will report completion.
pub(crate) fn start(env: &mut Env<'_>, graph: u32, filter: u32) -> u32 {
    let state_ref = state(env);
    let Some(f) = state_ref.filters.get(&filter) else {
        return 0;
    };
    if f.role != Role::Splitter || f.source.is_none() {
        return 0;
    }
    let outputs: Vec<u32> = f
        .pins
        .iter()
        .copied()
        .filter(|p| state_ref.pins[p].dir == OUTPUT)
        .collect();
    let (mut video, mut audio_renderer) = (None, false);
    for out in outputs {
        match follow(env, out) {
            Some(End::Guest(pin)) if video.is_none() => video = Some(pin),
            Some(End::AudioRenderer) => audio_renderer = true,
            _ => {}
        }
    }
    if video.is_none() && !audio_renderer {
        return 0;
    }
    let audio = audio_renderer
        .then(|| env.ext.try_get_mut::<Sound>())
        .flatten()
        .filter(|sound| sound.enabled)
        .map(Sound::allocate_audio_id);
    let wake = env.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    let id = state(env).next_session;
    state(env).next_session += 1;
    state(env).sessions.insert(
        id,
        Session {
            graph,
            splitter: filter,
            video,
            audio,
            wake,
            generation: 1,
            seen_generation: 0,
            stopping: false,
            prerolled: false,
            ended: false,
            discontinuity: true,
            segment: 0,
            pending_audio: VecDeque::new(),
        },
    );
    apply_volume(env, graph);
    let spawned = env
        .sys
        .thunks
        .publish(env.cpu, "quartz.dll", THREAD_ENTRY)
        .map_err(|e| e.to_string())
        .and_then(|entry| {
            env.sys
                .spawn(env.cpu, "dshow-stream", entry, &[id], 0x10000)
                .map_err(|e| format!("{e:?}"))
        });
    if let Err(error) = spawned {
        log::warn!("DirectShow: stream thread: {error}");
        state(env).sessions.remove(&id);
        env.sys.objects.close(wake);
        return 0;
    }
    log::info!("DirectShow: streaming splitter {filter:#x} (video={video:?}, audio={audio:?})");
    u32::from(video.is_some()) + u32::from(audio_renderer)
}

pub(crate) fn wake(env: &mut Env<'_>, id: u32) {
    if let Some(session) = state(env).sessions.get(&id) {
        let wake = session.wake;
        env.sys.set_event(wake);
    }
}

pub(crate) fn stop(env: &mut Env<'_>, id: u32) {
    if let Some(session) = state(env).sessions.get_mut(&id) {
        session.stopping = true;
    }
    wake(env, id);
}

pub(crate) fn seek(env: &mut Env<'_>, id: u32, _position: i64) {
    if let Some(session) = state(env).sessions.get_mut(&id) {
        session.generation += 1;
    }
    wake(env, id);
}

/// IBasicAudio volume (hundredths of a dB) and balance onto the mixer streams.
pub(crate) fn apply_volume(env: &mut Env<'_>, graph: u32) {
    let Some(g) = state(env).graphs.get(&graph) else {
        return;
    };
    let gain = |db100: i32| 10f32.powf(db100 as f32 / 2000.0);
    let base = gain(g.volume);
    let (left, right) = if g.balance > 0 {
        (base * gain(-g.balance), base)
    } else {
        (base, base * gain(g.balance))
    };
    let streams: Vec<u64> = state(env)
        .sessions
        .values()
        .filter(|s| s.graph == graph)
        .filter_map(|s| s.audio)
        .collect();
    if let Some(sound) = env.ext.try_get_mut::<Sound>() {
        for stream in streams {
            sound.audio.set_stereo_gain(stream, [left, right]);
        }
    }
}

pub(crate) fn thread_entry(env: &mut Env<'_>) -> Outcome {
    let id = env.arg(0);
    pump(env, id)
}

fn session<'a>(env: &'a mut Env<'_>, id: u32) -> &'a mut Session {
    state(env).sessions.get_mut(&id).unwrap()
}

fn pump(env: &mut Env<'_>, id: u32) -> Outcome {
    let Some(s) = state(env).sessions.get(&id) else {
        return Outcome::Return(0);
    };
    let (graph, stopping, generation, seen, ended, prerolled, splitter) = (
        s.graph,
        s.stopping,
        s.generation,
        s.seen_generation,
        s.ended,
        s.prerolled,
        s.splitter,
    );
    let graph_state = state(env).graphs.get(&graph).map(|g| g.state);
    if stopping || graph_state.is_none_or(|g| g == graph::STOPPED) {
        return finish(env, id);
    }
    if generation != seen {
        return begin_segment(env, id);
    }
    let paused = graph_state == Some(graph::PAUSED);
    if ended || (paused && prerolled) {
        return wait(env, id, None);
    }
    let position = graph::current(env, graph);
    let media_us = (position.max(0) / 10) as u64;
    let update = state(env)
        .filters
        .get_mut(&splitter)
        .and_then(|f| f.source.as_mut()?.decoder.as_mut())
        .map(|decoder| decoder.frame_at(media_us));
    process_update(env, id, update, position, paused)
}

fn process_update(
    env: &mut Env<'_>,
    id: u32,
    update: Option<Result<adv_device::media::Update, adv_device::media::Error>>,
    position: i64,
    paused: bool,
) -> Outcome {
    let graph = session(env, id).graph;
    let media_us = (position.max(0) / 10) as u64;
    let update = match update {
        Some(Ok(update)) => update,
        Some(Err(error)) => {
            return abort_stream(
                env,
                id,
                E_FAIL,
                &format!("decode at {media_us} us: {error}"),
            );
        }
        None => {
            return abort_stream(
                env,
                id,
                VFW_E_UNSUPPORTED_STREAM,
                "stream decoder unavailable",
            );
        }
    };
    let s = session(env, id);
    if s.audio.is_some() {
        s.pending_audio.extend(update.audio);
    }
    if !paused {
        push_audio(env, id, media_us);
    }
    let past_stop = state(env).graphs[&graph]
        .stop
        .is_some_and(|stop| position >= stop);
    let ended = update.ended || past_stop;
    let s = session(env, id);
    if paused {
        s.prerolled = true;
    }
    let video = s.video;
    match (update.video, video) {
        (Some(frame), Some(pin)) if !past_stop => deliver(env, id, pin, frame, move |env| {
            if ended {
                end_of_stream(env, id)
            } else {
                pump(env, id)
            }
        }),
        _ if ended => end_of_stream(env, id),
        _ => {
            let deadline = env.sys.clock.now_us() + POLL_US;
            wait(env, id, Some(deadline))
        }
    }
}

fn wait(env: &mut Env<'_>, id: u32, deadline: Option<u64>) -> Outcome {
    let handle = session(env, id).wake;
    Outcome::block(Wait::Object { handle, deadline }, move |env, _| {
        pump(env, id)
    })
}

fn finish(env: &mut Env<'_>, id: u32) -> Outcome {
    if let Some(s) = state(env).sessions.remove(&id) {
        env.sys.objects.close(s.wake);
        if let (Some(stream), Some(sound)) = (s.audio, env.ext.try_get_mut::<Sound>()) {
            sound.audio.remove_pcm(stream);
        }
    }
    Outcome::Return(0)
}

/// A failed delivery does not notify renderers of successful end-of-stream.
/// Report the error to the graph, discard queued PCM and end this stream thread.
fn abort_stream(env: &mut Env<'_>, id: u32, hresult: u32, reason: &str) -> Outcome {
    let graph = session(env, id).graph;
    log::warn!("DirectShow: {reason} ({hresult:#x})");
    adv_abi::warn_unavailable(env, "DirectShowStream", reason);
    graph::notify(env, graph, EC_ERRORABORT, hresult, 0);
    graph::flush_posts(env, Box::new(move |env, _| finish(env, id)))
}

/// Start (or, after a seek, flush and restart) a segment at the graph position.
fn begin_segment(env: &mut Env<'_>, id: u32) -> Outcome {
    let s = session(env, id);
    let flush = s.seen_generation != 0;
    s.seen_generation = s.generation;
    s.ended = false;
    s.prerolled = false;
    s.discontinuity = true;
    s.pending_audio.clear();
    let (graph, splitter, video, audio) = (s.graph, s.splitter, s.video, s.audio);
    let position = graph::current(env, graph).max(0);
    session(env, id).segment = position;
    let stop = state(env).graphs[&graph]
        .stop
        .unwrap_or_else(|| graph::duration(env, graph));
    if let Some(decoder) = state(env)
        .filters
        .get_mut(&splitter)
        .and_then(|f| f.source.as_mut()?.decoder.as_mut())
        && flush
        && let Err(error) = decoder.seek((position / 10) as u64)
    {
        return abort_stream(env, id, E_FAIL, &format!("seek to {position}: {error}"));
    }
    if let (Some(stream), Some(sound)) = (audio, env.ext.try_get_mut::<Sound>())
        && flush
    {
        sound.audio.flush_pcm(stream);
    }
    let mut calls = VecDeque::new();
    if let Some(pin) = video {
        let peer = state(env).pins[&pin].peer;
        if flush {
            calls.push_back(Call(peer, 15, vec![]));
            calls.push_back(Call(peer, 16, vec![]));
        }
        let rate = 1.0f64.to_bits();
        calls.push_back(Call(
            peer,
            17,
            vec![
                position as u32,
                (position >> 32) as u32,
                stop as u32,
                (stop >> 32) as u32,
                rate as u32,
                (rate >> 32) as u32,
            ],
        ));
    }
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |env, _| pump(env, id)),
    )
}

fn end_of_stream(env: &mut Env<'_>, id: u32) -> Outcome {
    let s = session(env, id);
    s.ended = true;
    let (graph, video, audio) = (s.graph, s.video, s.audio);
    if let (Some(stream), Some(sound)) = (audio, env.ext.try_get_mut::<Sound>()) {
        sound.audio.end_pcm(stream);
    }
    let renderer_audio = follow_audio(env, id);
    if renderer_audio {
        graph::notify(env, graph, EC_COMPLETE, 0, 0);
    }
    let then =
        move |env: &mut Env<'_>| graph::flush_posts(env, Box::new(move |env, _| pump(env, id)));
    match video {
        Some(pin) => {
            let peer = state(env).pins[&pin].peer;
            guest_call(env, peer, 14, vec![], move |env, _| then(env))
        }
        None => then(env),
    }
}

/// Whether this session feeds a host audio renderer (counted in `expected`).
fn follow_audio(env: &mut Env<'_>, id: u32) -> bool {
    let splitter = session(env, id).splitter;
    let state = state(env);
    let outputs: Vec<u32> = state.filters[&splitter]
        .pins
        .iter()
        .copied()
        .filter(|p| state.pins[p].dir == OUTPUT)
        .collect();
    outputs
        .into_iter()
        .any(|out| matches!(follow(env, out), Some(End::AudioRenderer)))
}

fn push_audio(env: &mut Env<'_>, id: u32, media_us: u64) {
    let now = env.sys.clock.now_us();
    let Some(stream) = session(env, id).audio else {
        return;
    };
    while let Some(frame) = session(env, id).pending_audio.pop_front() {
        let block = PcmBlock {
            stream_id: stream,
            guest_pts_us: now + frame.pts_us.saturating_sub(media_us),
            sample_rate: frame.sample_rate,
            channels: frame.channels,
            samples: frame.samples,
        };
        let Some(sound) = env.ext.try_get_mut::<Sound>() else {
            return;
        };
        if let Err(block) = sound.audio.push_pcm(block) {
            // The mixer queue is full; retry on the next pass.
            session(env, id).pending_audio.push_front(AudioFrame {
                pts_us: frame.pts_us,
                sample_rate: block.sample_rate,
                channels: block.channels,
                samples: block.samples,
            });
            return;
        }
    }
}

fn deliver(
    env: &mut Env<'_>,
    id: u32,
    pin: u32,
    frame: VideoFrame,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let p = &state(env).pins[&pin];
    let (mem, allocator) = (p.mem, p.allocator);
    let Some(mt) = p.mt.clone() else {
        return then(env);
    };
    if mem == 0 || allocator == 0 {
        return then(env);
    }
    let Some(sample) = sample::take(env, allocator) else {
        log::debug!("DirectShow: no free sample; dropping a frame");
        return then(env);
    };
    let bytes = convert(&frame, &mt);
    let s = &state(env).samples[&sample];
    let (buffer, length) = (s.buffer, bytes.len().min(mt.sample_size as usize));
    if env.cpu.write(buffer, &bytes[..length]).is_err() {
        release(env, sample);
        return then(env);
    }
    let frame_100ns = mt
        .format
        .get(40..48)
        .map_or(0, |b| i64::from_le_bytes(b.try_into().unwrap()))
        .max(1);
    let session = session(env, id);
    let start = frame.pts_us as i64 * 10 - session.segment;
    let discontinuity = std::mem::take(&mut session.discontinuity);
    let s = state(env).samples.get_mut(&sample).unwrap();
    s.actual = length as u32;
    s.time = Some((start, start + frame_100ns));
    s.sync_point = true;
    s.discontinuity = discontinuity;
    guest_call(env, mem, 6, vec![sample], move |env, hr| {
        release(env, sample);
        if hr & 0x8000_0000 != 0 {
            log::debug!("DirectShow: Receive returned {hr:#x}");
        }
        then(env)
    })
}

/// Top-down BGRA into the negotiated RGB layout (bottom-up for positive heights).
pub(crate) fn convert(frame: &VideoFrame, mt: &MediaType) -> Vec<u8> {
    let Some((width, height, bits)) = mt.video_geometry() else {
        return Vec::new();
    };
    let rows = height.unsigned_abs();
    let bytes_per = usize::from(bits / 8);
    let stride = ((width as usize * bytes_per) + 3) & !3;
    let mut out = vec![0u8; stride * rows as usize];
    let (w, h) = (
        width.min(frame.width) as usize,
        rows.min(frame.height) as usize,
    );
    let rgb565 = mt.sub == MEDIASUBTYPE_RGB565;
    for y in 0..h {
        let src = &frame.bgra[y * frame.width as usize * 4..];
        let row = if height > 0 { rows as usize - 1 - y } else { y };
        let dst = &mut out[row * stride..];
        for x in 0..w {
            let (b, g, r) = (src[x * 4], src[x * 4 + 1], src[x * 4 + 2]);
            let d = &mut dst[x * bytes_per..];
            match bits {
                32 => d[..4].copy_from_slice(&[b, g, r, 0]),
                24 => d[..3].copy_from_slice(&[b, g, r]),
                _ => {
                    let value = if rgb565 {
                        (u16::from(r >> 3) << 11) | (u16::from(g >> 2) << 5) | u16::from(b >> 3)
                    } else {
                        (u16::from(r >> 3) << 10) | (u16::from(g >> 3) << 5) | u16::from(b >> 3)
                    };
                    d[..2].copy_from_slice(&value.to_le_bytes());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use adv_cpu::JitCpu;
    use adv_cpu::backend::GuestExt;
    use adv_machine::Machine;

    use super::*;
    use crate::dshow::tests::{activate, env, machine, method, query};

    const OUTPUT: u32 = 0x500000;

    fn failure_session() -> (Machine<JitCpu>, u32, u32, Handle) {
        let mut m = machine();
        let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
        let events = query(&mut m, graph, IID_IMEDIAEVENTEX);
        let wake = m.sys.objects.insert(Object::Event {
            manual_reset: false,
            signaled: false,
        });
        m.ext.get_mut::<State>().sessions.insert(
            1,
            Session {
                graph,
                splitter: 0,
                // Error handling must not call this renderer's EndOfStream.
                video: Some(0xdead_beef),
                audio: None,
                wake,
                generation: 1,
                seen_generation: 1,
                stopping: false,
                prerolled: false,
                ended: false,
                discontinuity: true,
                segment: 0,
                pending_audio: VecDeque::new(),
            },
        );
        (m, graph, events, wake)
    }

    fn assert_error_only(m: &mut Machine<JitCpu>, events: u32, wake: Handle, hresult: u32) {
        assert!(!m.ext.get::<State>().sessions.contains_key(&1));
        assert!(m.sys.objects.get(wake).is_none());
        let diagnostics = m.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostics.stub_calls, 0);
        assert!(
            diagnostics
                .warnings
                .keys()
                .any(|(api, _)| api == "DirectShowStream")
        );
        assert_eq!(
            method(
                m,
                "IMediaEventEx::GetEvent",
                &[events, OUTPUT, OUTPUT + 4, OUTPUT + 8, 0]
            ),
            S_OK
        );
        assert_eq!(m.cpu.read_u32(OUTPUT).unwrap(), EC_ERRORABORT);
        assert_eq!(m.cpu.read_u32(OUTPUT + 4).unwrap(), hresult);
        assert_eq!(m.cpu.read_u32(OUTPUT + 8).unwrap(), 0);
        assert_eq!(
            method(
                m,
                "IMediaEventEx::GetEvent",
                &[events, OUTPUT, OUTPUT + 4, OUTPUT + 8, 0]
            ),
            0x8000_4004
        );
    }

    #[test]
    fn decode_failure_reports_error_without_successful_eos() {
        let (mut m, _, events, wake) = failure_session();
        let error = adv_device::media::Error::Decode("authored decoder failure".into());
        assert!(matches!(
            process_update(&mut env(&mut m), 1, Some(Err(error)), 0, false),
            Outcome::Return(0)
        ));
        assert_error_only(&mut m, events, wake, E_FAIL);
    }

    #[test]
    fn missing_decoder_reports_error_without_successful_eos() {
        let (mut m, _, events, wake) = failure_session();
        assert!(matches!(
            process_update(&mut env(&mut m), 1, None, 0, false),
            Outcome::Return(0)
        ));
        assert_error_only(&mut m, events, wake, VFW_E_UNSUPPORTED_STREAM);
    }
}
