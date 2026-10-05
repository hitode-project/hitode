//! System filters, pins and enumerators. One FFmpeg-backed splitter stands in for
//! every demuxer; decoder filters are pass-through stages whose output pins
//! negotiate uncompressed types with downstream (often guest) renderers.
use std::collections::VecDeque;

use adv_cpu::backend::GuestExt;
use adv_device::media::{Decoder, StreamInfo};
use adv_machine::{Env, Outcome};
use adv_os::com::Iid;

use super::*;

pub(crate) const INPUT: u32 = 0;
pub(crate) const OUTPUT: u32 = 1;
const VFW_E_NO_TRANSPORT: u32 = 0x8004_0266;
/// Largest stream read from a guest IAsyncReader into host memory.
const MAX_SOURCE_BYTES: u64 = 512 << 20;
const READ_CHUNK: u32 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    Splitter,
    VideoDecoder,
    AudioDecoder,
    AudioRenderer,
}

/// Demuxed media owned by a splitter; `decoder` is None when FFmpeg is unavailable
/// or rejected the stream, in which case streams end immediately.
pub(crate) struct Source {
    pub decoder: Option<Decoder>,
    pub duration_us: u64,
    pub info: Option<StreamInfo>,
}

pub(crate) struct Filter {
    pub role: Role,
    clsid: Iid,
    pub pins: Vec<u32>,
    pub graph: u32,
    pub name: Vec<u16>,
    pub state: u32,
    pub source: Option<Source>,
}

pub(crate) struct Pin {
    pub filter: u32,
    pub dir: u32,
    name: &'static str,
    pub major: Iid,
    /// Connected IPin (host canonical or guest pointer); 0 when unconnected.
    pub peer: u32,
    pub mt: Option<MediaType>,
    /// Upstream IAsyncReader held by a pull-mode input.
    reader: u32,
    /// Downstream IMemInputPin and the host allocator it was given.
    pub mem: u32,
    pub allocator: u32,
    pub faces: Vec<u32>,
}

pub(crate) fn create_splitter(env: &mut Env<'_>, iid: Iid, outer: u32, output: u32) -> Outcome {
    create_filter(
        env,
        iid,
        outer,
        output,
        Role::Splitter,
        super::CLSID_MPEG1_SPLITTER,
    )
}
pub(crate) fn create_video_decoder(
    env: &mut Env<'_>,
    iid: Iid,
    outer: u32,
    output: u32,
) -> Outcome {
    create_filter(
        env,
        iid,
        outer,
        output,
        Role::VideoDecoder,
        super::CLSID_MPEG_VIDEO_CODEC,
    )
}
pub(crate) fn create_audio_decoder(
    env: &mut Env<'_>,
    iid: Iid,
    outer: u32,
    output: u32,
) -> Outcome {
    create_filter(
        env,
        iid,
        outer,
        output,
        Role::AudioDecoder,
        super::CLSID_MPEG_AUDIO_CODEC,
    )
}
pub(crate) fn create_audio_renderer(
    env: &mut Env<'_>,
    iid: Iid,
    outer: u32,
    output: u32,
) -> Outcome {
    create_filter(
        env,
        iid,
        outer,
        output,
        Role::AudioRenderer,
        super::CLSID_DSOUND_RENDER,
    )
}

fn create_filter(
    env: &mut Env<'_>,
    iid: Iid,
    outer: u32,
    output: u32,
    role: Role,
    clsid: Iid,
) -> Outcome {
    if outer != 0 {
        return Outcome::Return(CLASS_E_NOAGGREGATION);
    }
    let faces = match super::create(env, &[Face::BaseFilter], Obj::Filter) {
        Ok(faces) => faces,
        Err(error) => return Outcome::Return(error),
    };
    let base = faces[0];
    state(env).filters.insert(
        base,
        Filter {
            role,
            clsid,
            pins: Vec::new(),
            graph: 0,
            name: Vec::new(),
            state: graph::STOPPED,
            source: None,
        },
    );
    let pins: &[(u32, &'static str, Iid)] = match role {
        Role::Splitter => &[(INPUT, "Input", MEDIATYPE_STREAM)],
        Role::VideoDecoder => &[
            (INPUT, "In", MEDIATYPE_VIDEO),
            (OUTPUT, "Out", MEDIATYPE_VIDEO),
        ],
        Role::AudioDecoder => &[
            (INPUT, "In", MEDIATYPE_AUDIO),
            (OUTPUT, "Out", MEDIATYPE_AUDIO),
        ],
        Role::AudioRenderer => &[(INPUT, "Audio Input pin (rendered)", MEDIATYPE_AUDIO)],
    };
    for &(dir, name, major) in pins {
        if let Err(error) = new_pin(env, base, dir, name, major, None) {
            release(env, base);
            return Outcome::Return(error);
        }
    }
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

/// A pin lives as long as the process: its filter holds an internal reference.
fn new_pin(
    env: &mut Env<'_>,
    filter: u32,
    dir: u32,
    name: &'static str,
    major: Iid,
    mt: Option<MediaType>,
) -> Result<u32, u32> {
    let faces: &[Face] = if dir == INPUT {
        &[Face::Pin, Face::MemInputPin]
    } else {
        &[Face::Pin]
    };
    let faces = super::create(env, faces, Obj::Pin)?;
    let pin = faces[0];
    state(env).pins.insert(
        pin,
        Pin {
            filter,
            dir,
            name,
            major,
            peer: 0,
            mt,
            reader: 0,
            mem: 0,
            allocator: 0,
            faces,
        },
    );
    retain_internal(env, pin);
    release(env, pin);
    state(env).filters.get_mut(&filter).unwrap().pins.push(pin);
    Ok(pin)
}

pub(crate) fn host_pin(env: &Env<'_>, pointer: u32) -> Option<u32> {
    let pin = canonical(env, pointer)?;
    env.ext
        .get::<State>()
        .pins
        .contains_key(&pin)
        .then_some(pin)
}

fn host_filter(env: &Env<'_>, pointer: u32) -> Option<u32> {
    let filter = canonical(env, pointer)?;
    env.ext
        .get::<State>()
        .filters
        .contains_key(&filter)
        .then_some(filter)
}

pub(crate) fn call(env: &mut Env<'_>, face: Face, method: &str, this: u32) -> Option<Outcome> {
    match face {
        Face::BaseFilter => filter_method(env, host_filter(env, this)?, method),
        Face::Pin => pin_method(env, host_pin(env, this)?, method),
        Face::MemInputPin => input_method(env, method),
        Face::EnumPins | Face::EnumMediaTypes | Face::EnumFilters => {
            enumerator(env, face, method, this)
        }
        _ => None,
    }
}

fn filter_method(env: &mut Env<'_>, filter: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let a1 = env.arg(1);
    match method {
        "GetClassID" => {
            let clsid = state(env).filters[&filter].clsid;
            r(if write_guid(env, a1, &clsid).is_ok() {
                S_OK
            } else {
                E_POINTER
            })
        }
        "Stop" | "Pause" | "Run" => {
            let value = match method {
                "Stop" => graph::STOPPED,
                "Pause" => graph::PAUSED,
                _ => graph::RUNNING,
            };
            state(env).filters.get_mut(&filter).unwrap().state = value;
            r(S_OK)
        }
        "GetState" => {
            let value = state(env).filters[&filter].state;
            let output = env.arg(2);
            r(if env.cpu.write_u32(output, value).is_ok() {
                S_OK
            } else {
                E_POINTER
            })
        }
        "SetSyncSource" => r(S_OK),
        "GetSyncSource" => {
            let _ = env.cpu.write_u32(a1, 0);
            r(S_OK)
        }
        "EnumPins" => {
            if a1 == 0 {
                return r(E_POINTER);
            }
            match super::create(
                env,
                &[Face::EnumPins],
                Obj::EnumPins {
                    filter,
                    position: 0,
                },
            ) {
                Ok(faces) => {
                    let _ = env.cpu.write_u32(a1, faces[0]);
                    r(S_OK)
                }
                Err(error) => r(error),
            }
        }
        "FindPin" => {
            let (id, output) = (read_wide(env, a1, 127), env.arg(2));
            let _ = env.cpu.write_u32(output, 0);
            let state = state(env);
            let found = state.filters[&filter]
                .pins
                .iter()
                .copied()
                .find(|pin| String::from_utf16_lossy(&id) == state.pins[pin].name);
            match found {
                Some(pin) => {
                    add_ref(env, pin);
                    let _ = env.cpu.write_u32(output, pin);
                    r(S_OK)
                }
                None => r(VFW_E_NOT_FOUND),
            }
        }
        "QueryFilterInfo" => {
            if a1 == 0 {
                return r(E_POINTER);
            }
            let (name, graph) = {
                let f = &state(env).filters[&filter];
                (f.name.clone(), f.graph)
            };
            let mut raw = vec![0u8; 128 * 2 + 4];
            for (i, c) in name.iter().take(127).enumerate() {
                raw[i * 2..i * 2 + 2].copy_from_slice(&c.to_le_bytes());
            }
            let graph_face = state(env).graphs.get(&graph).map_or(0, |g| g.faces[0]);
            if graph_face != 0 {
                add_ref(env, graph_face);
            }
            raw[256..260].copy_from_slice(&graph_face.to_le_bytes());
            r(if env.cpu.write(a1, &raw).is_ok() {
                S_OK
            } else {
                E_POINTER
            })
        }
        "JoinFilterGraph" => {
            let graph = canonical(env, a1).unwrap_or(0);
            let name = read_wide(env, env.arg(2), 127);
            let f = state(env).filters.get_mut(&filter).unwrap();
            f.graph = graph;
            f.name = name;
            r(S_OK)
        }
        "QueryVendorInfo" => r(E_NOTIMPL),
        _ => None,
    }
}

fn pin_method(env: &mut Env<'_>, pin: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let a1 = env.arg(1);
    match method {
        "Connect" => Some(connect(env, pin, a1, env.arg(2))),
        "ReceiveConnection" => Some(receive_connection(env, pin, a1, env.arg(2))),
        "Disconnect" => r(disconnect_host(env, pin)),
        "ConnectedTo" => {
            let _ = env.cpu.write_u32(a1, 0);
            let peer = state(env).pins[&pin].peer;
            if peer == 0 {
                return r(VFW_E_NOT_CONNECTED);
            }
            let _ = env.cpu.write_u32(a1, peer);
            Some(guest_call(env, peer, 1, vec![], |_, _| {
                Outcome::Return(S_OK)
            }))
        }
        "ConnectionMediaType" => {
            let mt = state(env).pins[&pin].mt.clone();
            match mt {
                Some(mt) if state(env).pins[&pin].peer != 0 => {
                    r(mt.write(env, a1).map_or_else(|e| e, |()| S_OK))
                }
                _ => {
                    let _ = env.cpu.write(a1, &[0u8; MEDIA_TYPE_SIZE as usize]);
                    r(VFW_E_NOT_CONNECTED)
                }
            }
        }
        "QueryPinInfo" => {
            let p = &state(env).pins[&pin];
            let (filter, dir) = (p.filter, p.dir);
            let name: Vec<u16> = p.name.encode_utf16().take(127).collect();
            let mut raw = vec![0u8; 8 + 256];
            raw[0..4].copy_from_slice(&filter.to_le_bytes());
            raw[4..8].copy_from_slice(&dir.to_le_bytes());
            for (i, c) in name.iter().enumerate() {
                raw[8 + i * 2..10 + i * 2].copy_from_slice(&c.to_le_bytes());
            }
            add_ref(env, filter);
            r(if env.cpu.write(a1, &raw).is_ok() {
                S_OK
            } else {
                E_POINTER
            })
        }
        "QueryDirection" => {
            let dir = state(env).pins[&pin].dir;
            r(if env.cpu.write_u32(a1, dir).is_ok() {
                S_OK
            } else {
                E_POINTER
            })
        }
        "QueryId" => {
            let name: Vec<u8> = state(env).pins[&pin]
                .name
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect();
            let text = adv_os::com::task_alloc(env, name.len() as u32);
            if text == 0 || env.cpu.write(text, &name).is_err() {
                return r(E_OUTOFMEMORY);
            }
            let _ = env.cpu.write_u32(a1, text);
            r(S_OK)
        }
        "QueryAccept" => {
            let Ok(mt) = MediaType::read(env, a1) else {
                return r(E_POINTER);
            };
            r(if accepts(env, pin, &mt) {
                S_OK
            } else {
                S_FALSE
            })
        }
        "EnumMediaTypes" => {
            if a1 == 0 {
                return r(E_POINTER);
            }
            let types = offered_types(env, pin);
            match super::create(
                env,
                &[Face::EnumMediaTypes],
                Obj::EnumTypes { types, position: 0 },
            ) {
                Ok(faces) => {
                    let _ = env.cpu.write_u32(a1, faces[0]);
                    r(S_OK)
                }
                Err(error) => r(error),
            }
        }
        "QueryInternalConnections" => r(E_NOTIMPL),
        "EndOfStream" | "BeginFlush" | "EndFlush" | "NewSegment" => r(S_OK),
        _ => None,
    }
}

/// IMemInputPin on host inputs. Pushed samples from guest sources are accepted
/// and dropped; host streams are pulled from IAsyncReader instead.
fn input_method(env: &mut Env<'_>, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    match method {
        "GetAllocator" => {
            let _ = env.cpu.write_u32(env.arg(1), 0);
            r(VFW_E_NO_ALLOCATOR)
        }
        "NotifyAllocator" => r(S_OK),
        "GetAllocatorRequirements" => r(E_NOTIMPL),
        "Receive" => r(S_OK),
        "ReceiveMultiple" => {
            let count = env.arg(2);
            let _ = env.cpu.write_u32(env.arg(3), count);
            r(S_OK)
        }
        "ReceiveCanBlock" => r(S_FALSE),
        _ => None,
    }
}

fn accepts(env: &Env<'_>, pin: u32, mt: &MediaType) -> bool {
    let p = &env.ext.get::<State>().pins[&pin];
    p.major == GUID_NULL || mt.major == p.major
}

/// Types a pin proposes. Decoder outputs depend on their connected input.
fn offered_types(env: &Env<'_>, pin: u32) -> Vec<MediaType> {
    let state = env.ext.get::<State>();
    let p = &state.pins[&pin];
    let role = state.filters.get(&p.filter).map(|f| f.role);
    match (role, p.dir) {
        (Some(Role::Splitter), INPUT) => vec![MediaType {
            major: MEDIATYPE_STREAM,
            sub: MEDIASUBTYPE_MPEG1SYSTEM,
            fixed_size: true,
            temporal: false,
            sample_size: 1,
            format_type: GUID_NULL,
            format: Vec::new(),
        }],
        (Some(Role::Splitter), OUTPUT) => p.mt.iter().cloned().collect(),
        (Some(Role::VideoDecoder), OUTPUT) => decoded_video_types(env, pin),
        (Some(Role::AudioDecoder), OUTPUT) => decoded_audio_type(env, pin).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// The splitter whose stream feeds this decoder output.
pub(crate) fn upstream_source(env: &Env<'_>, pin: u32) -> Option<u32> {
    let state = env.ext.get::<State>();
    let filter = &state.filters[&state.pins[&pin].filter];
    let input = filter.pins.iter().find(|p| state.pins[p].dir == INPUT)?;
    let upstream = state.pins.get(&state.pins[input].peer)?;
    (state.filters.get(&upstream.filter)?.role == Role::Splitter).then_some(upstream.filter)
}

pub(crate) const VIDEO_SUBTYPES: [Iid; 4] = [
    MEDIASUBTYPE_RGB32,
    MEDIASUBTYPE_RGB24,
    MEDIASUBTYPE_RGB565,
    MEDIASUBTYPE_RGB555,
];

fn decoded_video_types(env: &Env<'_>, pin: u32) -> Vec<MediaType> {
    let Some(splitter) = upstream_source(env, pin) else {
        return Vec::new();
    };
    let state = env.ext.get::<State>();
    let (width, height, frame) = state.filters[&splitter]
        .source
        .as_ref()
        .and_then(|s| s.info.as_ref()?.video.as_ref())
        .map_or((320, 240, 333_333), |v| {
            let frame = if v.frame_rate.0 > 0 {
                10_000_000 * i64::from(v.frame_rate.1) / i64::from(v.frame_rate.0)
            } else {
                333_333
            };
            (v.width, v.height, frame)
        });
    VIDEO_SUBTYPES
        .iter()
        .map(|&sub| MediaType::video_rgb(sub, width, height, frame))
        .collect()
}

fn decoded_audio_type(env: &Env<'_>, pin: u32) -> Option<MediaType> {
    let splitter = upstream_source(env, pin)?;
    let audio = env.ext.get::<State>().filters[&splitter]
        .source
        .as_ref()?
        .info
        .as_ref()?
        .audio
        .clone()?;
    Some(MediaType::audio_pcm(
        audio.sample_rate,
        audio.channels.min(2),
    ))
}

/// IGraphBuilder::Connect / ConnectDirect / IPin::Connect for any pair of pins.
pub(crate) fn connect(env: &mut Env<'_>, out: u32, input: u32, pmt: u32) -> Outcome {
    if out == 0 || input == 0 {
        return Outcome::Return(E_POINTER);
    }
    match (host_pin(env, out), host_pin(env, input)) {
        (Some(out), Some(input)) => Outcome::Return(connect_hosts(env, out, input)),
        (Some(out), None) => connect_to_guest(env, out, input, pmt),
        // The guest output drives negotiation and calls back ReceiveConnection.
        (None, _) => guest_call(env, out, 3, vec![input, pmt], |_, hr| Outcome::Return(hr)),
    }
}

fn connect_hosts(env: &mut Env<'_>, out: u32, input: u32) -> u32 {
    let state = state(env);
    let (o, i) = (&state.pins[&out], &state.pins[&input]);
    if o.dir != OUTPUT || i.dir != INPUT {
        return VFW_E_INVALID_DIRECTION;
    }
    if o.peer != 0 || i.peer != 0 {
        return VFW_E_ALREADY_CONNECTED;
    }
    let mt =
        o.mt.clone()
            .or_else(|| offered_types(env, out).into_iter().next());
    let state = self::state(env);
    let Some(mt) = mt else {
        return VFW_E_NO_ACCEPTABLE_TYPES;
    };
    let i = &state.pins[&input];
    if i.major != GUID_NULL && i.major != mt.major {
        return VFW_E_TYPE_NOT_ACCEPTED;
    }
    let o = state.pins.get_mut(&out).unwrap();
    o.peer = input;
    o.mt = Some(mt.clone());
    let i = state.pins.get_mut(&input).unwrap();
    i.peer = out;
    i.mt = Some(mt);
    S_OK
}

/// Propose each candidate type to a guest input until it accepts one, then hand
/// it a host allocator. A fully specified `pmt` is the only candidate.
fn connect_to_guest(env: &mut Env<'_>, out: u32, input: u32, pmt: u32) -> Outcome {
    let p = &state(env).pins[&out];
    if p.dir != OUTPUT {
        return Outcome::Return(VFW_E_INVALID_DIRECTION);
    }
    if p.peer != 0 {
        return Outcome::Return(VFW_E_ALREADY_CONNECTED);
    }
    let mut candidates = offered_types(env, out);
    if pmt != 0
        && let Ok(wanted) = MediaType::read(env, pmt)
    {
        if wanted.major != GUID_NULL && wanted.sub != GUID_NULL && wanted.format_type != GUID_NULL {
            candidates = vec![wanted];
        } else {
            candidates.retain(|c| {
                (wanted.major == GUID_NULL || c.major == wanted.major)
                    && (wanted.sub == GUID_NULL || c.sub == wanted.sub)
            });
        }
    }
    try_types(env, out, input, candidates.into())
}

fn try_types(
    env: &mut Env<'_>,
    out: u32,
    input: u32,
    mut candidates: VecDeque<MediaType>,
) -> Outcome {
    let Some(candidate) = candidates.pop_front() else {
        return Outcome::Return(VFW_E_NO_ACCEPTABLE_TYPES);
    };
    let Some(scratch) = alloc(env, MEDIA_TYPE_SIZE + 20) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    if let Err(error) = candidate.write(env, scratch) {
        free(env, scratch);
        return Outcome::Return(error);
    }
    let face = state(env).pins[&out].faces[0];
    guest_call(env, input, 4, vec![face, scratch], move |env, hr| {
        free_type_block(env, scratch);
        if hr != S_OK {
            free(env, scratch);
            log::debug!("DirectShow: guest pin {input:#x} refused a type ({hr:#x})");
            return try_types(env, out, input, candidates);
        }
        let p = state(env).pins.get_mut(&out).unwrap();
        p.peer = input;
        p.mt = Some(candidate.clone());
        finish_guest_connection(env, out, input, scratch, candidate)
    })
}

fn free_type_block(env: &mut Env<'_>, block: u32) {
    if let Ok(data) = env.cpu.read_u32(block + 68)
        && data != 0
    {
        adv_os::com::task_free(env, data);
    }
}

/// Hold the guest pin, find its IMemInputPin and give it a host allocator.
fn finish_guest_connection(
    env: &mut Env<'_>,
    out: u32,
    input: u32,
    scratch: u32,
    mt: MediaType,
) -> Outcome {
    let _ = write_guid(env, scratch, &IID_IMEMINPUTPIN);
    let _ = env.cpu.write_u32(scratch + 16, 0);
    let calls = VecDeque::from([
        Call(input, 1, vec![]),
        Call(input, 0, vec![scratch, scratch + 16]),
    ]);
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |env, results| {
            let mem = env.cpu.read_u32(scratch + 16).unwrap_or(0);
            free(env, scratch);
            if results[1] != S_OK || mem == 0 {
                log::warn!("DirectShow: guest input {input:#x} has no IMemInputPin");
                state(env).pins.get_mut(&out).unwrap().mem = 0;
                return Outcome::Return(VFW_E_NO_TRANSPORT);
            }
            let size = mt.sample_size.max(1);
            let allocator = match sample::create_allocator(env, 3, size) {
                Ok(allocator) => allocator,
                Err(error) => return Outcome::Return(error),
            };
            let p = state(env).pins.get_mut(&out).unwrap();
            p.mem = mem;
            p.allocator = allocator;
            let face = state(env).allocators[&allocator].face;
            guest_call(env, mem, 4, vec![face, 0], move |env, hr| {
                if hr & 0x8000_0000 != 0 {
                    log::warn!("DirectShow: NotifyAllocator refused ({hr:#x})");
                }
                log::info!(
                    "DirectShow: host pin {out:#x} connected to guest pin {input:#x} ({})",
                    describe(&mt)
                );
                let _ = env;
                Outcome::Return(S_OK)
            })
        }),
    )
}

fn describe(mt: &MediaType) -> String {
    let sub = match mt.sub {
        s if s == MEDIASUBTYPE_RGB32 => "RGB32",
        s if s == MEDIASUBTYPE_RGB24 => "RGB24",
        s if s == MEDIASUBTYPE_RGB565 => "RGB565",
        s if s == MEDIASUBTYPE_RGB555 => "RGB555",
        s if s == MEDIASUBTYPE_PCM => "PCM",
        _ => "other",
    };
    match mt.video_geometry() {
        Some((w, h, _)) => format!("{sub} {w}x{h}"),
        None => sub.to_owned(),
    }
}

pub(crate) fn disconnect(env: &mut Env<'_>, pin: u32) -> Outcome {
    match host_pin(env, pin) {
        Some(pin) => Outcome::Return(disconnect_host(env, pin)),
        None => guest_call(env, pin, 5, vec![], |_, hr| Outcome::Return(hr)),
    }
}

fn disconnect_host(env: &mut Env<'_>, pin: u32) -> u32 {
    let p = state(env).pins.get_mut(&pin).unwrap();
    if p.peer == 0 {
        return S_FALSE;
    }
    p.peer = 0;
    p.mem = 0;
    let allocator = std::mem::take(&mut p.allocator);
    let (dir, filter) = (p.dir, p.filter);
    let state = state(env);
    if dir == OUTPUT
        && state
            .filters
            .get(&filter)
            .is_some_and(|f| f.role != Role::Splitter)
    {
        state.pins.get_mut(&pin).unwrap().mt = None;
    }
    if allocator != 0 {
        release(env, allocator);
    }
    S_OK
}

/// A guest output pin offers a connection to a host input.
fn receive_connection(env: &mut Env<'_>, pin: u32, connector: u32, pmt: u32) -> Outcome {
    let p = &state(env).pins[&pin];
    if p.dir != INPUT {
        return Outcome::Return(VFW_E_INVALID_DIRECTION);
    }
    if p.peer != 0 {
        return Outcome::Return(VFW_E_ALREADY_CONNECTED);
    }
    let Ok(mt) = MediaType::read(env, pmt) else {
        return Outcome::Return(E_POINTER);
    };
    if !accepts(env, pin, &mt) {
        return Outcome::Return(VFW_E_TYPE_NOT_ACCEPTED);
    }
    let filter = state(env).pins[&pin].filter;
    if state(env).filters[&filter].role != Role::Splitter {
        let p = state(env).pins.get_mut(&pin).unwrap();
        p.peer = connector;
        p.mt = Some(mt);
        return guest_call(env, connector, 1, vec![], |_, _| Outcome::Return(S_OK));
    }
    // A pull-mode source: query IAsyncReader during the connection, as the
    // stock splitter does (the async source refuses to complete otherwise).
    let Some(scratch) = alloc(env, 40) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let _ = write_guid(env, scratch, &IID_IASYNCREADER);
    guest_call(
        env,
        connector,
        0,
        vec![scratch, scratch + 16],
        move |env, hr| {
            let reader = env.cpu.read_u32(scratch + 16).unwrap_or(0);
            if hr != S_OK || reader == 0 {
                free(env, scratch);
                return Outcome::Return(VFW_E_NO_TRANSPORT);
            }
            guest_call(
                env,
                reader,
                8,
                vec![scratch + 20, scratch + 28],
                move |env, hr| {
                    let total = read_i64(env, scratch + 20).unwrap_or(0);
                    free(env, scratch);
                    if hr & 0x8000_0000 != 0 || total <= 0 || total as u64 > MAX_SOURCE_BYTES {
                        log::warn!("DirectShow: source length {total} ({hr:#x}) refused");
                        return guest_call(env, reader, 2, vec![], |_, _| {
                            Outcome::Return(VFW_E_TYPE_NOT_ACCEPTED)
                        });
                    }
                    read_source(env, pin, connector, reader, mt, total as u64, Vec::new())
                },
            )
        },
    )
}

fn read_source(
    env: &mut Env<'_>,
    pin: u32,
    connector: u32,
    reader: u32,
    mt: MediaType,
    total: u64,
    mut bytes: Vec<u8>,
) -> Outcome {
    if bytes.len() as u64 >= total {
        return attach_source(env, pin, connector, reader, mt, bytes);
    }
    let length = (total - bytes.len() as u64).min(u64::from(READ_CHUNK)) as u32;
    let Some(buffer) = alloc(env, length) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let position = bytes.len() as u64;
    let args = vec![position as u32, (position >> 32) as u32, length, buffer];
    guest_call(env, reader, 7, args, move |env, hr| {
        let mut chunk = vec![0; length as usize];
        let ok = hr == S_OK && env.cpu.read(buffer, &mut chunk).is_ok();
        free(env, buffer);
        if !ok {
            log::warn!("DirectShow: SyncRead at {position} failed ({hr:#x})");
            return guest_call(env, reader, 2, vec![], |_, _| {
                Outcome::Return(VFW_E_TYPE_NOT_ACCEPTED)
            });
        }
        bytes.extend_from_slice(&chunk);
        read_source(env, pin, connector, reader, mt, total, bytes)
    })
}

/// Probe the stream and expose one output pin per elementary stream.
fn attach_source(
    env: &mut Env<'_>,
    pin: u32,
    connector: u32,
    reader: u32,
    mt: MediaType,
    bytes: Vec<u8>,
) -> Outcome {
    let length = bytes.len();
    let (decoder, info) = match Decoder::open_memory(bytes) {
        Ok(decoder) => {
            let info = decoder.info().clone();
            (Some(decoder), Some(info))
        }
        Err(error) => {
            log::warn!("DirectShow: cannot decode {length}-byte stream: {error}");
            adv_abi::warn_unavailable(env, "MPEG-1 Stream Splitter", "stream not decodable");
            (None, None)
        }
    };
    let filter = state(env).pins[&pin].filter;
    let video = info.as_ref().map_or(true, |i| i.video.is_some());
    let audio = info.as_ref().is_some_and(|i| i.audio.is_some());
    log::info!(
        "DirectShow: splitter {filter:#x} source {length} bytes: {}",
        info.as_ref().map_or("undecodable".to_owned(), |i| format!(
            "{} video={:?} audio={:?} {} us",
            i.container,
            i.video
                .as_ref()
                .map(|v| (v.codec.as_str(), v.width, v.height)),
            i.audio
                .as_ref()
                .map(|a| (a.codec.as_str(), a.sample_rate, a.channels)),
            i.duration_us
        ))
    );
    state(env).filters.get_mut(&filter).unwrap().source = Some(Source {
        duration_us: info.as_ref().map_or(0, |i| i.duration_us),
        decoder,
        info,
    });
    let streams = [
        (video, "Video", MEDIATYPE_VIDEO, MEDIASUBTYPE_MPEG1PAYLOAD),
        (
            audio,
            "Audio",
            MEDIATYPE_AUDIO,
            MEDIASUBTYPE_MPEG1AUDIOPAYLOAD,
        ),
    ];
    for (present, name, major, sub) in streams {
        if !present {
            continue;
        }
        let mt = MediaType {
            major,
            sub,
            fixed_size: false,
            temporal: true,
            sample_size: 0,
            format_type: GUID_NULL,
            format: Vec::new(),
        };
        if let Err(error) = new_pin(env, filter, OUTPUT, name, major, Some(mt)) {
            return Outcome::Return(error);
        }
    }
    let p = state(env).pins.get_mut(&pin).unwrap();
    p.peer = connector;
    p.mt = Some(mt);
    p.reader = reader;
    guest_call(env, connector, 1, vec![], |_, _| Outcome::Return(S_OK))
}

fn enumerator(env: &mut Env<'_>, face: Face, method: &str, this: u32) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    match method {
        "Next" => {
            let (count, output, fetched) = (env.arg(1), env.arg(2), env.arg(3));
            if output == 0 || (count > 1 && fetched == 0) {
                return r(E_POINTER);
            }
            let items = next_items(env, this, count as usize);
            let n = items.len() as u32;
            for (i, &item) in items.iter().enumerate() {
                let _ = env.cpu.write_u32(output + i as u32 * 4, item);
            }
            if fetched != 0 {
                let _ = env.cpu.write_u32(fetched, n);
            }
            let hr = if n == count { S_OK } else { S_FALSE };
            if face == Face::EnumFilters {
                // Filters may be guest objects: AddRef through their vtables.
                let calls = items.into_iter().map(|f| Call(f, 1, vec![])).collect();
                return Some(chain(
                    env,
                    calls,
                    Vec::new(),
                    Box::new(move |_, _| Outcome::Return(hr)),
                ));
            }
            r(hr)
        }
        "Skip" => {
            let count = env.arg(1) as usize;
            let skipped = advance(env, this, count);
            r(if skipped == count { S_OK } else { S_FALSE })
        }
        "Reset" => {
            match payload_mut(env, this) {
                Some(Obj::EnumPins { position, .. })
                | Some(Obj::EnumTypes { position, .. })
                | Some(Obj::EnumFilters { position, .. }) => *position = 0,
                _ => {}
            }
            r(S_OK)
        }
        "Clone" => {
            let output = env.arg(1);
            let copy = match payload(env, this) {
                Some(Obj::EnumPins { filter, position }) => Obj::EnumPins {
                    filter: *filter,
                    position: *position,
                },
                Some(Obj::EnumTypes { types, position }) => Obj::EnumTypes {
                    types: types.clone(),
                    position: *position,
                },
                Some(Obj::EnumFilters { list, position }) => Obj::EnumFilters {
                    list: list.clone(),
                    position: *position,
                },
                _ => return r(E_UNEXPECTED),
            };
            match super::create(env, &[face], copy) {
                Ok(faces) => {
                    let _ = env.cpu.write_u32(output, faces[0]);
                    r(S_OK)
                }
                Err(error) => r(error),
            }
        }
        _ => None,
    }
}

fn advance(env: &mut Env<'_>, this: u32, count: usize) -> usize {
    let length = match payload(env, this) {
        Some(Obj::EnumPins { filter, .. }) => env
            .ext
            .get::<State>()
            .filters
            .get(filter)
            .map_or(0, |f| f.pins.len()),
        Some(Obj::EnumTypes { types, .. }) => types.len(),
        Some(Obj::EnumFilters { list, .. }) => list.len(),
        _ => 0,
    };
    match payload_mut(env, this) {
        Some(Obj::EnumPins { position, .. })
        | Some(Obj::EnumTypes { position, .. })
        | Some(Obj::EnumFilters { position, .. }) => {
            let start = (*position).min(length);
            *position = (start + count).min(length);
            *position - start
        }
        _ => 0,
    }
}

/// Up to `count` items: AddRef'd host pins, new task-memory media types, or
/// (unreferenced) filter pointers.
fn next_items(env: &mut Env<'_>, this: u32, count: usize) -> Vec<u32> {
    let (start, items): (usize, Vec<Item>) = match payload(env, this) {
        Some(Obj::EnumPins { filter, position }) => (
            *position,
            env.ext
                .get::<State>()
                .filters
                .get(filter)
                .map_or(Vec::new(), |f| {
                    f.pins.iter().map(|&p| Item::Pin(p)).collect()
                }),
        ),
        Some(Obj::EnumTypes { types, position }) => {
            (*position, types.iter().cloned().map(Item::Type).collect())
        }
        Some(Obj::EnumFilters { list, position }) => {
            (*position, list.iter().map(|&f| Item::Filter(f)).collect())
        }
        _ => return Vec::new(),
    };
    let taken: Vec<Item> = items.into_iter().skip(start).take(count).collect();
    advance(env, this, taken.len());
    let mut out = Vec::new();
    for item in taken {
        match item {
            Item::Pin(pin) => {
                add_ref(env, pin);
                out.push(pin);
            }
            Item::Type(mt) => match mt.create(env) {
                Ok(pointer) => out.push(pointer),
                Err(_) => break,
            },
            Item::Filter(filter) => out.push(filter),
        }
    }
    out
}

enum Item {
    Pin(u32),
    Type(MediaType),
    Filter(u32),
}
