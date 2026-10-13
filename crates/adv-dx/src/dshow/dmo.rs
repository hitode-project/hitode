//! The DMO wrapper filter around the Windows Media audio and video decoders: a
//! push-mode transform. Compressed samples arrive through IMemInputPin::Receive
//! on the upstream (usually guest) streaming thread. Decoded video is held until
//! the graph position reaches it and delivered to the downstream input on that
//! same thread; decoded audio goes to the DirectSound mixer, which paces the
//! thread through its bounded queue.
use adv_device::media::{AudioFrame, CodecFormat, Decoded, PacketDecoder, VideoFrame};
use adv_device::mixer::PcmBlock;
use adv_machine::{Env, Handle, Object, Outcome, Wait};

use super::filter::{INPUT, OUTPUT, Role};
use super::*;
use crate::dsound::Sound;

pub(crate) const CLSID_DMO_WRAPPER: Iid = guid(
    0x9429_7043,
    0xbd82,
    0x4dfd,
    [0xb0, 0xde, 0x81, 0x77, 0x73, 0x9c, 0x6d, 0x20],
);
pub(crate) const IID_IDMOWRAPPERFILTER: Iid = guid(
    0x52d6_f586,
    0x9f0f,
    0x4824,
    [0x8f, 0xc8, 0xe3, 0x2c, 0xa0, 0x49, 0x30, 0xc2],
);
pub(super) const CLSID_WMV_DECODER: Iid = guid(
    0x82d3_53df,
    0x90bd,
    0x4382,
    [0x8b, 0xc2, 0x3f, 0x61, 0x92, 0xb7, 0x6e, 0x34],
);
const CLSID_WMA_DECODER: Iid = guid(
    0x2eeb_4adf,
    0x4578,
    0x4d10,
    [0xbc, 0xa7, 0xbb, 0x95, 0x5f, 0x56, 0x32, 0x0a],
);
pub(super) const REGDB_E_CLASSNOTREG: u32 = 0x8004_0154;
const EC_COMPLETE: u32 = 1;
const EC_ERRORABORT: u32 = 3;
const E_FAIL: u32 = 0x8000_4005;
const POLL_US: u64 = 10_000;

pub(crate) struct Dmo {
    decoder: Option<PacketDecoder>,
    /// Host allocator handed to the upstream output; one internal reference.
    input_allocator: u32,
    /// Mixer stream of the audio output.
    audio: Option<u64>,
    wake: Handle,
    flushing: bool,
    /// A frame was delivered since the last start or flush (paused preroll).
    prerolled: bool,
    discontinuity: bool,
    /// NewSegment start: media time of stream time zero.
    segment: i64,
    /// The upstream filter's IMediaSeeking (one reference), which owns seeking
    /// and duration for this stream; 0 when it has none.
    upstream_seeking: u32,
    /// The upstream filter, for graph state ordering; not referenced.
    upstream_filter: u32,
}

/// CoCreateInstance(CLSID_DMOWrapperFilter): pins appear at IDMOWrapperFilter::Init.
pub(crate) fn create(env: &mut Env<'_>, iid: Iid, outer: u32, output: u32) -> Outcome {
    if outer != 0 {
        return Outcome::Return(CLASS_E_NOAGGREGATION);
    }
    let base = match filter::new_filter(env, Role::DmoDecoder, CLSID_DMO_WRAPPER) {
        Ok(base) => base,
        Err(error) => return Outcome::Return(error),
    };
    let com = state(env).com;
    let result = match com.query::<Obj>(&mut env.sys.objects, base, iid) {
        Ok(pointer) => {
            let _ = env.cpu.write_u32(output, pointer);
            S_OK
        }
        Err(_) => E_NOINTERFACE,
    };
    release(env, base);
    Outcome::Return(result)
}

fn dmo<'a>(env: &'a mut Env<'_>, filter: u32) -> Option<&'a mut Dmo> {
    state(env).filters.get_mut(&filter)?.dmo.as_mut()
}

fn filter_of(env: &Env<'_>, pin: u32) -> u32 {
    env.ext.get::<State>().pins[&pin].filter
}

fn pin_of(env: &Env<'_>, filter: u32, dir: u32) -> Option<u32> {
    let state = env.ext.get::<State>();
    state.filters[&filter]
        .pins
        .iter()
        .copied()
        .find(|p| state.pins[p].dir == dir)
}

/// IDMOWrapperFilter::Init: only the Windows Media decoders are provided.
pub(crate) fn wrapper_method(env: &mut Env<'_>, filter: u32, method: &str) -> Option<Outcome> {
    if method != "Init" {
        return None;
    }
    let Ok(clsid) = read_guid(env, env.arg(1)) else {
        return Some(Outcome::Return(E_POINTER));
    };
    if state(env).filters[&filter].dmo.is_some() {
        return Some(Outcome::Return(VFW_E_WRONG_STATE));
    }
    let major = match clsid {
        c if c == CLSID_WMV_DECODER => MEDIATYPE_VIDEO,
        c if c == CLSID_WMA_DECODER => MEDIATYPE_AUDIO,
        _ => {
            adv_abi::warn_unavailable(
                env,
                "IDMOWrapperFilter::Init",
                "only the Windows Media audio and video decoder DMOs are provided",
            );
            return Some(Outcome::Return(REGDB_E_CLASSNOTREG));
        }
    };
    let wake = env.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    for (dir, name) in [(INPUT, "in0"), (OUTPUT, "out0")] {
        if let Err(error) = filter::new_pin(env, filter, dir, name, major, None) {
            env.sys.objects.close(wake);
            return Some(Outcome::Return(error));
        }
    }
    state(env).filters.get_mut(&filter).unwrap().dmo = Some(Dmo {
        decoder: None,
        input_allocator: 0,
        audio: None,
        wake,
        flushing: false,
        prerolled: false,
        discontinuity: true,
        segment: 0,
        upstream_seeking: 0,
        upstream_filter: 0,
    });
    Some(Outcome::Return(S_OK))
}

/// Whether the input pin can decode `mt`.
pub(crate) fn accepts_input(mt: &MediaType) -> Option<CodecFormat> {
    let format = mt.codec_format()?;
    PacketDecoder::open(&format).ok().map(|_| format)
}

/// Bind the decoder for a new input connection.
pub(crate) fn connect_input(env: &mut Env<'_>, pin: u32, mt: &MediaType) -> bool {
    let Some(format) = mt.codec_format() else {
        return false;
    };
    let decoder = match PacketDecoder::open(&format) {
        Ok(decoder) => decoder,
        Err(error) => {
            log::warn!("DirectShow DMO: {error}");
            return false;
        }
    };
    let filter = filter_of(env, pin);
    let Some(dmo) = dmo(env, filter) else {
        return false;
    };
    dmo.decoder = Some(decoder);
    log::info!("DirectShow DMO: decoding {}", summary(&format));
    true
}

fn summary(format: &CodecFormat) -> String {
    match format {
        CodecFormat::Video {
            fourcc,
            width,
            height,
            ..
        } => format!(
            "{} {width}x{height}",
            String::from_utf8_lossy(&fourcc.to_le_bytes())
        ),
        CodecFormat::Audio {
            format_tag,
            rate,
            channels,
            ..
        } => format!("audio tag {format_tag:#x} {rate} Hz x{channels}"),
    }
}

/// After an upstream output connects, find its filter's IMediaSeeking: the
/// graph takes the stream duration from it and passes seeks on to it.
pub(crate) fn learn_upstream(env: &mut Env<'_>, pin: u32, connector: u32) -> Outcome {
    let Some(scratch) = alloc(env, 296) else {
        return Outcome::Return(S_OK);
    };
    let (iid, seeking_out, duration_out) = (scratch + 264, scratch + 280, scratch + 284);
    let _ = env.cpu.write_u32(scratch, 0);
    guest_call(env, connector, 8, vec![scratch], move |env, hr| {
        let upstream = env.cpu.read_u32(scratch).unwrap_or(0);
        if hr != S_OK || upstream == 0 {
            free(env, scratch);
            return Outcome::Return(S_OK);
        }
        let filter = filter_of(env, pin);
        if let Some(d) = dmo(env, filter) {
            d.upstream_filter = upstream;
        }
        let _ = write_guid(env, iid, &IID_IMEDIASEEKING);
        let _ = env.cpu.write_u32(seeking_out, 0);
        guest_call(env, upstream, 0, vec![iid, seeking_out], move |env, hr| {
            let seeking = env.cpu.read_u32(seeking_out).unwrap_or(0);
            guest_call(env, upstream, 2, vec![], move |env, _| {
                if hr != S_OK || seeking == 0 {
                    free(env, scratch);
                    return Outcome::Return(S_OK);
                }
                guest_call(env, seeking, 10, vec![duration_out], move |env, hr| {
                    let duration = read_i64(env, duration_out).unwrap_or(0);
                    free(env, scratch);
                    let filter = filter_of(env, pin);
                    let graph = state(env).filters[&filter].graph;
                    if hr == S_OK
                        && duration > 0
                        && let Some(g) = state(env).graphs.get_mut(&graph)
                    {
                        g.guest_duration = g.guest_duration.max(duration);
                    }
                    match dmo(env, filter) {
                        Some(d) if d.upstream_seeking == 0 => {
                            d.upstream_seeking = seeking;
                            Outcome::Return(S_OK)
                        }
                        _ => guest_call(env, seeking, 2, vec![], |_, _| Outcome::Return(S_OK)),
                    }
                })
            })
        })
    })
}

/// Distinct upstream IMediaSeeking interfaces of the graph's DMO decoders.
pub(crate) fn upstream_seekers(env: &Env<'_>, graph: u32) -> Vec<u32> {
    let mut seekers: Vec<u32> = env
        .ext
        .get::<State>()
        .filters
        .values()
        .filter(|f| f.graph == graph)
        .filter_map(|f| f.dmo.as_ref().map(|d| d.upstream_seeking))
        .filter(|&s| s != 0)
        .collect();
    seekers.sort_unstable();
    seekers.dedup();
    seekers
}

/// Guest filters feeding the graph's DMO decoders.
pub(crate) fn upstream_filters(env: &Env<'_>, graph: u32) -> Vec<u32> {
    env.ext
        .get::<State>()
        .filters
        .values()
        .filter(|f| f.graph == graph)
        .filter_map(|f| f.dmo.as_ref().map(|d| d.upstream_filter))
        .filter(|&s| s != 0)
        .collect()
}

/// The upstream reference a disconnecting input gives up.
pub(crate) fn take_upstream(env: &mut Env<'_>, pin: u32) -> u32 {
    if state(env).pins[&pin].dir != INPUT {
        return 0;
    }
    let filter = filter_of(env, pin);
    dmo(env, filter).map_or(0, |d| {
        d.upstream_filter = 0;
        std::mem::take(&mut d.upstream_seeking)
    })
}

/// Uncompressed types the output offers once its input is connected.
pub(crate) fn output_types(env: &Env<'_>, pin: u32) -> Vec<MediaType> {
    let filter = filter_of(env, pin);
    let Some(input) = pin_of(env, filter, INPUT) else {
        return Vec::new();
    };
    let Some(format) = env.ext.get::<State>().pins[&input]
        .mt
        .as_ref()
        .and_then(MediaType::codec_format)
    else {
        return Vec::new();
    };
    match format {
        CodecFormat::Video {
            width,
            height,
            frame_100ns,
            ..
        } => {
            let frame = if frame_100ns > 0 {
                frame_100ns as i64
            } else {
                333_333
            };
            filter::VIDEO_SUBTYPES
                .iter()
                .map(|&sub| MediaType::video_rgb(sub, width, height, frame))
                .collect()
        }
        CodecFormat::Audio { rate, channels, .. } => {
            vec![MediaType::audio_pcm(rate, channels.min(2))]
        }
    }
}

/// Drop decoding state when the input disconnects.
pub(crate) fn disconnected(env: &mut Env<'_>, pin: u32) {
    let filter = filter_of(env, pin);
    if state(env).pins[&pin].dir != INPUT {
        return;
    }
    let allocator = match dmo(env, filter) {
        Some(dmo) => {
            dmo.decoder = None;
            std::mem::take(&mut dmo.input_allocator)
        }
        None => return,
    };
    if allocator != 0 {
        release(env, allocator);
    }
}

/// Graph state changes reach host filters here.
pub(crate) fn state_changed(env: &mut Env<'_>, filter: u32, value: u32) {
    let Some(dmo) = dmo(env, filter) else {
        return;
    };
    let wake = dmo.wake;
    if value == graph::STOPPED {
        dmo.prerolled = false;
        dmo.discontinuity = true;
        if let Some(decoder) = dmo.decoder.as_mut() {
            decoder.reset();
        }
        close_audio(env, filter);
    }
    env.sys.set_event(wake);
}

/// Remove the mixer stream; a busy mixer keeps it until the next attempt.
fn close_audio(env: &mut Env<'_>, filter: u32) {
    let Some(stream) = dmo(env, filter).and_then(|d| d.audio) else {
        return;
    };
    if env
        .ext
        .try_get_mut::<Sound>()
        .is_none_or(|sound| sound.audio.remove_pcm(stream))
    {
        dmo(env, filter).unwrap().audio = None;
    }
}

pub(crate) fn destroyed(env: &mut Env<'_>, dmo: Dmo) {
    if let (Some(stream), Some(sound)) = (dmo.audio, env.ext.try_get_mut::<Sound>()) {
        let _ = sound.audio.remove_pcm(stream);
    }
    env.sys.objects.close(dmo.wake);
}

pub(crate) fn audio_streams(env: &Env<'_>, graph: u32) -> Vec<u64> {
    env.ext
        .get::<State>()
        .filters
        .values()
        .filter(|f| f.graph == graph)
        .filter_map(|f| f.dmo.as_ref()?.audio)
        .collect()
}

/// Connected outputs whose renderer will report completion.
pub(crate) fn renderers(env: &Env<'_>, graph: u32) -> u32 {
    let state = env.ext.get::<State>();
    state
        .filters
        .values()
        .filter(|f| f.graph == graph && f.dmo.is_some())
        .filter(|f| {
            f.pins
                .iter()
                .any(|p| state.pins[p].dir == OUTPUT && state.pins[p].peer != 0)
        })
        .count() as u32
}

/// IMemInputPin on a DMO input.
pub(crate) fn input_method(env: &mut Env<'_>, pin: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let filter = filter_of(env, pin);
    match method {
        "GetAllocator" => {
            let output = env.arg(1);
            if output == 0 {
                return r(E_POINTER);
            }
            let existing = dmo(env, filter)?.input_allocator;
            let allocator = if existing != 0 {
                existing
            } else {
                // Sized by the upstream's SetProperties.
                match sample::create_allocator(env, 0, 0) {
                    Ok(allocator) => {
                        dmo(env, filter)?.input_allocator = allocator;
                        allocator
                    }
                    Err(error) => return r(error),
                }
            };
            add_ref(env, allocator);
            r(if env.cpu.write_u32(output, allocator).is_ok() {
                S_OK
            } else {
                E_POINTER
            })
        }
        "NotifyAllocator" => r(S_OK),
        "GetAllocatorRequirements" => r(E_NOTIMPL),
        "Receive" => Some(receive(env, filter, env.arg(1))),
        "ReceiveMultiple" => {
            let processed = env.arg(3);
            if processed == 0 || env.cpu.write_u32(processed, 0).is_err() {
                return r(E_POINTER);
            }
            let samples: Vec<u32> = (0..env.arg(2).min(64))
                .map(|i| env.cpu.read_u32(env.arg(1) + i * 4).unwrap_or(0))
                .collect();
            Some(receive_each(env, filter, samples, processed, 0))
        }
        "ReceiveCanBlock" => r(S_OK),
        _ => None,
    }
}

fn receive_each(
    env: &mut Env<'_>,
    filter: u32,
    mut samples: Vec<u32>,
    processed: u32,
    done: u32,
) -> Outcome {
    if samples.is_empty() {
        return Outcome::Return(S_OK);
    }
    let sample = samples.remove(0);
    let outcome_then = move |env: &mut Env<'_>, hr: u32| {
        if hr != S_OK {
            return Outcome::Return(hr);
        }
        let _ = env.cpu.write_u32(processed, done + 1);
        receive_each(env, filter, samples, processed, done + 1)
    };
    receive_then(env, filter, sample, Box::new(outcome_then))
}

fn receive(env: &mut Env<'_>, filter: u32, sample: u32) -> Outcome {
    receive_then(env, filter, sample, Box::new(|_, hr| Outcome::Return(hr)))
}

type Done = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;

fn receive_then(env: &mut Env<'_>, filter: u32, sample: u32, done: Done) -> Outcome {
    let filter_state = state(env).filters[&filter].state;
    let Some(d) = dmo(env, filter) else {
        return done(env, E_UNEXPECTED);
    };
    if d.flushing {
        return done(env, S_FALSE);
    }
    if filter_state == graph::STOPPED {
        return done(env, VFW_E_WRONG_STATE);
    }
    let Some(s) = canonical(env, sample).and_then(|c| state(env).samples.get(&c).cloned()) else {
        adv_abi::warn_unavailable(
            env,
            "IMemInputPin::Receive",
            "DMO decoders accept only samples from the allocator they provide",
        );
        return done(env, E_NOTIMPL);
    };
    let mut data = vec![0u8; s.actual as usize];
    if env.cpu.read(s.buffer, &mut data).is_err() {
        return done(env, E_POINTER);
    }
    let d = dmo(env, filter).unwrap();
    if s.discontinuity {
        d.discontinuity = true;
    }
    let Some(decoder) = d.decoder.as_mut() else {
        return done(env, VFW_E_NOT_CONNECTED);
    };
    let frames = match decoder.decode(&data, s.time.map(|(start, _)| start.max(0) as u64)) {
        Ok(frames) => frames,
        Err(error) => {
            log::warn!("DirectShow DMO: {error}");
            return abort(env, filter, done);
        }
    };
    emit(env, filter, frames.into(), done)
}

/// A decoder failure stops the stream with EC_ERRORABORT, as the stock filter does.
fn abort(env: &mut Env<'_>, filter: u32, done: Done) -> Outcome {
    let graph = state(env).filters[&filter].graph;
    adv_abi::warn_unavailable(
        env,
        "DirectShow DMO decoder",
        "a compressed sample failed to decode",
    );
    if state(env).graphs.contains_key(&graph) {
        graph::notify(env, graph, EC_ERRORABORT, E_FAIL, 0);
        return graph::flush_posts(env, Box::new(move |env, _| done(env, E_FAIL)));
    }
    done(env, E_FAIL)
}

/// Deliver decoded frames in order, each when it is due.
fn emit(env: &mut Env<'_>, filter: u32, mut frames: VecDeque<Decoded>, done: Done) -> Outcome {
    let Some(frame) = frames.pop_front() else {
        return done(env, S_OK);
    };
    match frame {
        Decoded::Video(frame) => {
            let segment = dmo(env, filter).map_or(0, |d| d.segment);
            let due = frame.pts_us as i64 * 10 + segment;
            when_due(env, filter, due, move |env, ready| match ready {
                Ready::Now => deliver_video(env, filter, frame, move |env, hr| {
                    if hr & 0x8000_0000 != 0 || hr == S_FALSE {
                        return done(env, hr);
                    }
                    emit(env, filter, frames, done)
                }),
                Ready::Stopped => done(env, VFW_E_WRONG_STATE),
                Ready::Flushing => done(env, S_FALSE),
            })
        }
        Decoded::Audio(frame) => push_audio(env, filter, frame, move |env, hr| {
            if hr != S_OK {
                return done(env, hr);
            }
            emit(env, filter, frames, done)
        }),
    }
}

enum Ready {
    Now,
    Stopped,
    Flushing,
}

/// Wait on the upstream thread until media time `due` is reached on the graph
/// clock. A paused graph lets one preroll frame through.
fn when_due(
    env: &mut Env<'_>,
    filter: u32,
    due: i64,
    then: impl FnOnce(&mut Env<'_>, Ready) -> Outcome + 'static,
) -> Outcome {
    let (filter_state, graph) = {
        let f = &state(env).filters[&filter];
        (f.state, f.graph)
    };
    let Some(d) = dmo(env, filter) else {
        return then(env, Ready::Stopped);
    };
    if d.flushing {
        return then(env, Ready::Flushing);
    }
    let (prerolled, wake) = (d.prerolled, d.wake);
    if filter_state == graph::STOPPED || !state(env).graphs.contains_key(&graph) {
        return then(env, Ready::Stopped);
    }
    let now = env.sys.clock.now_us();
    let position = state(env).graphs[&graph].position(now);
    let wait_us = if filter_state == graph::RUNNING {
        if position >= due {
            return then(env, Ready::Now);
        }
        (((due - position) / 10) as u64).min(POLL_US * 10)
    } else if !prerolled {
        return then(env, Ready::Now);
    } else {
        POLL_US
    };
    Outcome::block(
        Wait::Object {
            handle: wake,
            deadline: Some(now + wait_us.max(1)),
        },
        move |env, _| when_due(env, filter, due, then),
    )
}

fn deliver_video(
    env: &mut Env<'_>,
    filter: u32,
    frame: VideoFrame,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let Some(out) = pin_of(env, filter, OUTPUT) else {
        return then(env, VFW_E_NOT_CONNECTED);
    };
    let p = &state(env).pins[&out];
    let (mem, allocator, mt) = (p.mem, p.allocator, p.mt.clone());
    let Some(mt) = mt.filter(|_| mem != 0 && allocator != 0) else {
        // An unconnected output discards its frames, as an unrendered stream does.
        return then(env, S_OK);
    };
    let Some(sample) = sample::take(env, allocator) else {
        log::debug!("DirectShow DMO: no free output sample; dropping a frame");
        return then(env, S_OK);
    };
    let bytes = stream::convert(&frame, &mt);
    let s = &state(env).samples[&sample];
    let (buffer, length) = (s.buffer, bytes.len().min(mt.sample_size as usize));
    if env.cpu.write(buffer, &bytes[..length]).is_err() {
        release(env, sample);
        return then(env, E_POINTER);
    }
    let frame_100ns = mt
        .format
        .get(40..48)
        .map_or(0, |b| i64::from_le_bytes(b.try_into().unwrap()))
        .max(1);
    let d = dmo(env, filter).unwrap();
    d.prerolled = true;
    let discontinuity = std::mem::take(&mut d.discontinuity);
    let start = frame.pts_us as i64 * 10;
    let s = state(env).samples.get_mut(&sample).unwrap();
    s.actual = length as u32;
    s.time = Some((start, start + frame_100ns));
    s.sync_point = true;
    s.discontinuity = discontinuity;
    guest_call(env, mem, 6, vec![sample], move |env, hr| {
        release(env, sample);
        if hr & 0x8000_0000 != 0 {
            log::debug!("DirectShow DMO: downstream Receive returned {hr:#x}");
        }
        then(env, hr)
    })
}

/// Queue PCM at its media time; a full mixer queue holds the upstream thread.
fn push_audio(
    env: &mut Env<'_>,
    filter: u32,
    frame: AudioFrame,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let (filter_state, graph) = {
        let f = &state(env).filters[&filter];
        (f.state, f.graph)
    };
    let Some(d) = dmo(env, filter) else {
        return then(env, VFW_E_WRONG_STATE);
    };
    if d.flushing {
        return then(env, S_FALSE);
    }
    let (wake, segment) = (d.wake, d.segment);
    if filter_state == graph::STOPPED || !state(env).graphs.contains_key(&graph) {
        return then(env, VFW_E_WRONG_STATE);
    }
    let connected = pin_of(env, filter, OUTPUT).is_some_and(|out| state(env).pins[&out].peer != 0);
    let enabled = env
        .ext
        .try_get::<Sound>()
        .is_some_and(|sound| sound.enabled);
    if !connected || !enabled {
        return then(env, S_OK);
    }
    let now = env.sys.clock.now_us();
    if filter_state != graph::RUNNING {
        // Paused: hold PCM until the clock runs.
        return Outcome::block(
            Wait::Object {
                handle: wake,
                deadline: Some(now + POLL_US),
            },
            move |env, _| push_audio(env, filter, frame, then),
        );
    }
    let stream = match dmo(env, filter).unwrap().audio {
        Some(stream) => stream,
        None => {
            let Some(stream) = env
                .ext
                .try_get_mut::<Sound>()
                .and_then(Sound::allocate_audio_id)
            else {
                return then(env, S_OK);
            };
            dmo(env, filter).unwrap().audio = Some(stream);
            stream::apply_volume(env, graph);
            stream
        }
    };
    let position = state(env).graphs[&graph].position(now);
    let media_us = ((frame.pts_us as i64 * 10 + segment).max(0) / 10) as u64;
    let block = PcmBlock {
        stream_id: stream,
        guest_pts_us: (now as i64 + media_us as i64 - position.max(0) / 10).max(0) as u64,
        sample_rate: frame.sample_rate,
        channels: frame.channels,
        samples: frame.samples,
    };
    let Some(sound) = env.ext.try_get_mut::<Sound>() else {
        return then(env, S_OK);
    };
    match sound.audio.push_pcm(block) {
        Ok(()) => then(env, S_OK),
        Err(block) => {
            let frame = AudioFrame {
                pts_us: frame.pts_us,
                sample_rate: block.sample_rate,
                channels: block.channels,
                samples: block.samples,
            };
            Outcome::block(
                Wait::Object {
                    handle: wake,
                    deadline: Some(now + POLL_US),
                },
                move |env, _| push_audio(env, filter, frame, then),
            )
        }
    }
}

/// IPin streaming calls on DMO pins.
pub(crate) fn pin_method(env: &mut Env<'_>, pin: u32, method: &str) -> Option<Outcome> {
    let filter = filter_of(env, pin);
    if state(env).pins[&pin].dir != INPUT {
        return None;
    }
    let downstream = pin_of(env, filter, OUTPUT)
        .map(|out| state(env).pins[&out].peer)
        .filter(|&peer| peer != 0 && filter::host_pin(env, peer).is_none());
    let forward = move |env: &mut Env<'_>, slot: u32, args: Vec<u32>| match downstream {
        Some(peer) => guest_call(env, peer, slot, args, |_, _| Outcome::Return(S_OK)),
        None => Outcome::Return(S_OK),
    };
    match method {
        "EndOfStream" => Some(end_of_stream(env, filter, downstream)),
        "BeginFlush" => {
            let d = dmo(env, filter)?;
            d.flushing = true;
            if let Some(decoder) = d.decoder.as_mut() {
                decoder.reset();
            }
            let (wake, audio) = (d.wake, d.audio);
            if let (Some(stream), Some(sound)) = (audio, env.ext.try_get_mut::<Sound>()) {
                let _ = sound.audio.flush_pcm(stream);
            }
            env.sys.set_event(wake);
            Some(forward(env, 15, vec![]))
        }
        "EndFlush" => {
            let d = dmo(env, filter)?;
            d.flushing = false;
            d.prerolled = false;
            d.discontinuity = true;
            Some(forward(env, 16, vec![]))
        }
        "NewSegment" => {
            let start = arg_i64(env, 1);
            dmo(env, filter)?.segment = start;
            let args = (1..7).map(|i| env.arg(i)).collect();
            Some(forward(env, 17, args))
        }
        _ => None,
    }
}

/// Decode what the decoder still holds, then pass end-of-stream downstream.
fn end_of_stream(env: &mut Env<'_>, filter: u32, downstream: Option<u32>) -> Outcome {
    let frames = match dmo(env, filter)
        .and_then(|d| d.decoder.as_mut())
        .map(PacketDecoder::drain)
    {
        Some(Ok(frames)) => frames,
        Some(Err(error)) => {
            log::warn!("DirectShow DMO: drain: {error}");
            Vec::new()
        }
        None => Vec::new(),
    };
    emit(
        env,
        filter,
        frames.into(),
        Box::new(move |env, _| finish_stream(env, filter, downstream)),
    )
}

fn finish_stream(env: &mut Env<'_>, filter: u32, downstream: Option<u32>) -> Outcome {
    if let Some(peer) = downstream {
        // A guest renderer reports EC_COMPLETE itself.
        return guest_call(env, peer, 14, vec![], |_, _| Outcome::Return(S_OK));
    }
    let graph = state(env).filters[&filter].graph;
    let audio = dmo(env, filter).and_then(|d| d.audio);
    if let (Some(stream), Some(sound)) = (audio, env.ext.try_get_mut::<Sound>())
        && !sound.audio.end_pcm(stream)
    {
        let wake = dmo(env, filter).map(|d| d.wake);
        let Some(wake) = wake else {
            return Outcome::Return(S_OK);
        };
        let deadline = env.sys.clock.now_us() + POLL_US;
        return Outcome::block(
            Wait::Object {
                handle: wake,
                deadline: Some(deadline),
            },
            move |env, _| finish_stream(env, filter, downstream),
        );
    }
    let connected = pin_of(env, filter, OUTPUT).is_some_and(|out| state(env).pins[&out].peer != 0);
    if connected && state(env).graphs.contains_key(&graph) {
        graph::notify(env, graph, EC_COMPLETE, 0, 0);
        return graph::flush_posts(env, Box::new(|_, _| Outcome::Return(S_OK)));
    }
    Outcome::Return(S_OK)
}
