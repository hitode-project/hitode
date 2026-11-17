//! System filters, pins and enumerators. The splitter uses the common media
//! demuxer; MPEG decoders also accept pushed samples through the media worker.
//! Output pins negotiate compressed or decoded types with guest renderers.
use std::collections::VecDeque;

use adv_cpu::backend::GuestExt;
use adv_device::media::wine::{Command, Event, Output as DecodedOutput, Worker};
use adv_device::media::{Decoder, StreamInfo};
use adv_machine::{Env, Outcome, Wait};
use adv_os::com::Iid;

use super::*;

pub(crate) const INPUT: u32 = 0;
pub(crate) const OUTPUT: u32 = 1;
const VFW_E_NO_TRANSPORT: u32 = 0x8004_0266;
/// Largest stream read from a guest IAsyncReader into host memory.
const MAX_SOURCE_BYTES: u64 = 512 << 20;
const READ_CHUNK: u32 = 1 << 20;

type Done = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    FileSource,
    Splitter,
    VideoDecoder,
    AudioDecoder,
    AudioRenderer,
    VideoRenderer,
    /// IDMOWrapperFilter around a Windows Media decoder; push-mode input.
    DmoDecoder,
}

/// Demuxed media owned by a splitter. Failed probes never attach a source;
/// a missing decoder during delivery is a stream error, not successful EOF.
pub(crate) struct Source {
    pub file_handle: Option<u32>,
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
    pub source_path: Vec<u16>,
    pub dmo: Option<super::dmo::Dmo>,
    pub vmr: Option<super::vmr::Vmr>,
    /// The controlling outer object of an aggregated filter, and the filter's
    /// own non-delegating IUnknown; both zero otherwise.
    pub outer: u32,
    pub inner: u32,
    pub position: i64,
    pub stop: Option<i64>,
    rate: f64,
    pub packets: Option<adv_device::media::Demuxed>,
    decoder: Option<Worker>,
    pushed_bytes: Vec<u8>,
    pushed_time: Option<i64>,
    flushing: bool,
}

pub(crate) struct Pin {
    pub filter: u32,
    pub dir: u32,
    name: &'static str,
    pub major: Iid,
    /// Connected IPin (host canonical or guest pointer); 0 when unconnected.
    pub peer: u32,
    pub mt: Option<MediaType>,
    types: Vec<MediaType>,
    /// Upstream IAsyncReader held by a pull-mode input.
    reader: u32,
    /// Downstream IMemInputPin and the host allocator it was given.
    pub mem: u32,
    pub allocator: u32,
    pub faces: Vec<u32>,
    quality_sink: u32,
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
pub(crate) fn create_asf_reader(env: &mut Env<'_>, iid: Iid, outer: u32, output: u32) -> Outcome {
    create_filter(
        env,
        iid,
        outer,
        output,
        Role::FileSource,
        super::CLSID_WMASF_READER,
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

pub(crate) fn create_vmr_renderer(env: &mut Env<'_>, iid: Iid, outer: u32, output: u32) -> Outcome {
    create_filter(
        env,
        iid,
        outer,
        output,
        Role::VideoRenderer,
        super::vmr::CLSID_VIDEO_MIXING_RENDERER,
    )
}

/// CoCreateInstance of a host filter. Like Windows, an aggregated filter hands
/// out only its inner IUnknown.
pub(super) fn create_filter(
    env: &mut Env<'_>,
    iid: Iid,
    outer: u32,
    output: u32,
    role: Role,
    clsid: Iid,
) -> Outcome {
    if outer != 0 && iid != adv_os::com::IUNKNOWN {
        let _ = env.cpu.write_u32(output, 0);
        return Outcome::Return(E_NOINTERFACE);
    }
    let base = match new_filter(env, role, clsid, outer) {
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

pub(super) fn new_filter(
    env: &mut Env<'_>,
    role: Role,
    clsid: Iid,
    outer: u32,
) -> Result<u32, u32> {
    let is_vmr = role == Role::VideoRenderer && clsid == super::vmr::CLSID_VIDEO_MIXING_RENDERER;
    let faces: &[Face] = match role {
        Role::FileSource => &[Face::BaseFilter, Face::FileSource],
        Role::DmoDecoder => &[Face::BaseFilter, Face::DmoWrapper],
        Role::AudioDecoder => &[Face::BaseFilter, Face::MpegAudioDecoder],
        Role::Splitter => &[Face::BaseFilter, Face::StreamSelect],
        Role::VideoRenderer if is_vmr => &[
            Face::BaseFilter,
            Face::VmrFilterConfig,
            Face::VmrWindowlessControl,
        ],
        _ => &[Face::BaseFilter],
    };
    let faces = if outer == 0 {
        super::create(env, faces, Obj::Filter)?
    } else {
        let mut faces = faces.to_vec();
        faces.push(Face::Inner);
        super::create_aggregated(env, &faces, Obj::Filter)?
    };
    let (base, inner) = (
        faces[0],
        if outer == 0 {
            0
        } else {
            *faces.last().unwrap()
        },
    );
    let vmr = (role == Role::VideoRenderer).then(|| super::vmr::Vmr::new(env));
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
            source_path: Vec::new(),
            dmo: None,
            vmr,
            outer,
            inner,
            position: 0,
            stop: None,
            rate: 1.0,
            packets: None,
            decoder: None,
            pushed_bytes: Vec::new(),
            pushed_time: None,
            flushing: false,
        },
    );
    let pins: &[(u32, &'static str, Iid)] = match role {
        Role::FileSource => &[(OUTPUT, "Output", MEDIATYPE_STREAM)],
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
        Role::VideoRenderer if is_vmr => &[(INPUT, "VMR Input0", MEDIATYPE_VIDEO)],
        Role::VideoRenderer => &[(INPUT, "Video Input pin (rendered)", MEDIATYPE_VIDEO)],
        // Pins follow the decoder chosen by IDMOWrapperFilter::Init.
        Role::DmoDecoder => &[],
    };
    for &(dir, name, major) in pins {
        if let Err(error) = new_pin(env, base, dir, name, major, None) {
            release(env, base);
            return Err(error);
        }
    }
    Ok(base)
}

/// A pin lives as long as the process: its filter holds an internal reference.
pub(super) fn new_pin(
    env: &mut Env<'_>,
    filter: u32,
    dir: u32,
    name: &'static str,
    major: Iid,
    mt: Option<MediaType>,
) -> Result<u32, u32> {
    let role = state(env).filters[&filter].role;
    let mpeg = matches!(
        role,
        Role::Splitter | Role::VideoDecoder | Role::AudioDecoder
    );
    let faces: &[Face] = match (mpeg, dir) {
        (true, INPUT) => &[Face::Pin, Face::MemInputPin, Face::QualityControl],
        (true, OUTPUT) if role != Role::Splitter => &[
            Face::Pin,
            Face::QualityControl,
            Face::MediaSeeking,
            Face::MediaPosition,
        ],
        (true, OUTPUT) => &[Face::Pin, Face::QualityControl, Face::MediaSeeking],
        (false, INPUT) => &[Face::Pin, Face::MemInputPin],
        _ => &[Face::Pin],
    };
    let faces = super::create(env, faces, Obj::Pin)?;
    let pin = faces[0];
    retain_internal(env, pin);
    release(env, pin);
    for &face in &faces {
        let _ = env.cpu.write_u32(face + 4, 0);
    }
    state(env).pins.insert(
        pin,
        Pin {
            filter,
            dir,
            name,
            major,
            peer: 0,
            types: mt.iter().cloned().collect(),
            mt,
            reader: 0,
            mem: 0,
            allocator: 0,
            faces,
            quality_sink: 0,
        },
    );
    state(env).filters.get_mut(&filter).unwrap().pins.push(pin);
    Ok(pin)
}

fn role_of(env: &Env<'_>, pin: u32) -> Role {
    let state = env.ext.get::<State>();
    state.filters[&state.pins[&pin].filter].role
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
        Face::StreamSelect => Some(stream_select(env, host_filter(env, this)?, method)),
        Face::QualityControl => Some(quality_control(env, host_pin(env, this)?, method)),
        Face::MpegAudioDecoder => Some(Outcome::Return(E_NOTIMPL)),
        Face::FileSource => Some(super::graph::source::file_method(
            env,
            host_filter(env, this)?,
            method,
        )),
        Face::BaseFilter => filter_method(env, host_filter(env, this)?, method),
        Face::Pin => pin_method(env, host_pin(env, this)?, method),
        Face::MemInputPin => {
            let pin = host_pin(env, this)?;
            input_method(env, pin, method)
        }
        Face::DmoWrapper => super::dmo::wrapper_method(env, host_filter(env, this)?, method),
        Face::EnumPins | Face::EnumMediaTypes | Face::EnumFilters => {
            enumerator(env, face, method, this)
        }
        _ => None,
    }
}

pub(super) fn interface_methods(interface: &str) -> Option<&'static [(&'static str, Conv, u16)]> {
    match interface {
        "IQualityControl" => Some(&[
            ("QueryInterface", Conv::Stdcall { argc: 3 }, 0),
            ("AddRef", Conv::Stdcall { argc: 1 }, 1),
            ("Release", Conv::Stdcall { argc: 1 }, 2),
            ("Notify", Conv::Stdcall { argc: 8 }, 3),
            ("SetSink", Conv::Stdcall { argc: 2 }, 4),
        ]),
        "IMpegAudioDecoder" => Some(&[
            ("QueryInterface", Conv::Stdcall { argc: 3 }, 0),
            ("AddRef", Conv::Stdcall { argc: 1 }, 1),
            ("Release", Conv::Stdcall { argc: 1 }, 2),
            ("get_FrequencyDivider", Conv::Stdcall { argc: 2 }, 3),
            ("put_FrequencyDivider", Conv::Stdcall { argc: 2 }, 4),
            ("get_DecoderAccuracy", Conv::Stdcall { argc: 2 }, 5),
            ("put_DecoderAccuracy", Conv::Stdcall { argc: 2 }, 6),
            ("get_Stereo", Conv::Stdcall { argc: 2 }, 7),
            ("put_Stereo", Conv::Stdcall { argc: 2 }, 8),
            ("get_DecoderWordSize", Conv::Stdcall { argc: 2 }, 9),
            ("put_DecoderWordSize", Conv::Stdcall { argc: 2 }, 10),
            ("get_IntegerDecode", Conv::Stdcall { argc: 2 }, 11),
            ("put_IntegerDecode", Conv::Stdcall { argc: 2 }, 12),
            ("get_DualMode", Conv::Stdcall { argc: 2 }, 13),
            ("put_DualMode", Conv::Stdcall { argc: 2 }, 14),
            ("get_AudioFormat", Conv::Stdcall { argc: 2 }, 15),
        ]),
        "IAMStreamSelect" => Some(&[
            ("QueryInterface", Conv::Stdcall { argc: 3 }, 0),
            ("AddRef", Conv::Stdcall { argc: 1 }, 1),
            ("Release", Conv::Stdcall { argc: 1 }, 2),
            ("Count", Conv::Stdcall { argc: 2 }, 3),
            ("Info", Conv::Stdcall { argc: 9 }, 4),
            ("Enable", Conv::Stdcall { argc: 3 }, 5),
        ]),
        _ => None,
    }
}

fn stream_select(env: &mut Env<'_>, filter: u32, method: &str) -> Outcome {
    let pins: Vec<_> = env.ext.get::<State>().filters[&filter]
        .pins
        .iter()
        .copied()
        .filter(|p| env.ext.get::<State>().pins[p].dir == OUTPUT)
        .collect();
    if method == "Count" {
        let _ = env.cpu.write_u32(env.arg(1), pins.len() as u32);
        return Outcome::Return(S_OK);
    }
    let index = env.arg(1) as usize;
    let Some(&pin) = pins.get(index) else {
        return Outcome::Return(S_FALSE);
    };
    if method == "Enable" {
        return Outcome::Return(S_OK);
    }
    let args: Vec<_> = (2..=8).map(|i| env.arg(i)).collect();
    if args[0] != 0 {
        let mt = state(env).pins[&pin].mt.clone().unwrap();
        let pointer = match mt.create(env) {
            Ok(p) => p,
            Err(hr) => return Outcome::Return(hr),
        };
        let _ = env.cpu.write_u32(args[0], pointer);
    }
    for (output, value) in [
        (args[1], 1),
        (args[2], 0),
        (args[3], index as u32),
        (args[6], 0),
    ] {
        if output != 0 {
            let _ = env.cpu.write_u32(output, value);
        }
    }
    if args[4] != 0 {
        let name = if state(env).pins[&pin].major == MEDIATYPE_VIDEO {
            "Stream(E0)"
        } else {
            "Stream(C0)"
        };
        let text: Vec<_> = name
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        let pointer = adv_os::com::task_alloc(env, text.len() as u32);
        let _ = env.cpu.write(pointer, &text);
        let _ = env.cpu.write_u32(args[4], pointer);
    }
    if args[5] != 0 {
        add_ref(env, pin);
        let _ = env.cpu.write_u32(args[5], pin);
    }
    Outcome::Return(S_OK)
}

fn quality_control(env: &mut Env<'_>, pin: u32, method: &str) -> Outcome {
    if method == "SetSink" {
        let sink = env.arg(1);
        state(env).pins.get_mut(&pin).unwrap().quality_sink = sink;
        return Outcome::Return(S_OK);
    }
    let p = &state(env).pins[&pin];
    let filter = p.filter;
    if p.dir == INPUT || state(env).filters[&filter].role != Role::AudioDecoder {
        return Outcome::Return(S_OK);
    }
    let input = state(env).filters[&filter].pins[0];
    let (sink, peer) = {
        let p = &state(env).pins[&input];
        (p.quality_sink, p.peer)
    };
    let mut args = vec![filter];
    args.extend((2..=7).map(|i| env.arg(i)));
    if sink != 0 {
        return guest_call(env, sink, 3, args, |_, hr| Outcome::Return(hr));
    }
    if peer == 0 {
        return Outcome::Return(VFW_E_NOT_FOUND);
    }
    let Some(scratch) = alloc(env, 20) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let _ = write_guid(env, scratch, &quartz(0x56a8_68a5));
    guest_call(env, peer, 0, vec![scratch, scratch + 16], move |env, hr| {
        let sink = env.cpu.read_u32(scratch + 16).unwrap_or(0);
        free(env, scratch);
        if hr != S_OK {
            return Outcome::Return(VFW_E_NOT_FOUND);
        }
        guest_call(env, sink, 3, args, move |env, hr| {
            guest_call(env, sink, 2, vec![], move |_, _| Outcome::Return(hr))
        })
    })
}

pub(super) fn seeking(env: &mut Env<'_>, pin: u32, method: &str) -> Outcome {
    let filter = state(env).pins[&pin].filter;
    let f = &state(env).filters[&filter];
    let duration = f.source.as_ref().map_or(0, |s| s.duration_us as i64 * 10);
    let (position, stop, rate) = (f.position, f.stop.unwrap_or(duration), f.rate);
    let (a1, a2) = (env.arg(1), env.arg(2));
    let media_time = guid(
        0x7b78_5574,
        0x8c82,
        0x11cf,
        [0xbc, 0x0c, 0, 0xaa, 0, 0xac, 0x74, 0xf6],
    );
    let hr = match method {
        "GetCapabilities" => {
            let _ = env.cpu.write_u32(a1, 0x37);
            S_OK
        }
        "CheckCapabilities" => {
            let caps = env.cpu.read_u32(a1).unwrap_or(0);
            let supported = caps & 0x37;
            let _ = env.cpu.write_u32(a1, supported);
            if supported == 0 {
                0x8000_4005
            } else if supported == caps {
                S_OK
            } else {
                S_FALSE
            }
        }
        "IsFormatSupported" | "IsUsingTimeFormat" | "SetTimeFormat" => {
            if read_guid(env, a1) == Ok(media_time) {
                S_OK
            } else if method == "SetTimeFormat" {
                E_INVALIDARG
            } else {
                S_FALSE
            }
        }
        "QueryPreferredFormat" | "GetTimeFormat" => {
            let _ = write_guid(env, a1, &media_time);
            S_OK
        }
        "GetDuration" => {
            write_i64(env, a1, duration);
            S_OK
        }
        "GetCurrentPosition" => {
            write_i64(env, a1, position);
            S_OK
        }
        "GetStopPosition" => {
            write_i64(env, a1, stop);
            S_OK
        }
        "GetPositions" | "GetAvailable" => {
            write_i64(
                env,
                a1,
                if method == "GetAvailable" {
                    0
                } else {
                    position
                },
            );
            write_i64(
                env,
                a2,
                if method == "GetAvailable" {
                    duration
                } else {
                    stop
                },
            );
            S_OK
        }
        "ConvertTimeFormat" => {
            write_i64(env, a1, arg_i64(env, 3));
            S_OK
        }
        "GetRate" => {
            let _ = env.cpu.write(a1, &rate.to_le_bytes());
            S_OK
        }
        "SetRate" => {
            let rate = arg_f64(env, 1);
            if rate <= 0.0 {
                E_INVALIDARG
            } else {
                state(env).filters.get_mut(&filter).unwrap().rate = rate;
                S_OK
            }
        }
        "SetPositions" => {
            let flags = env.arg(4);
            let start = if a2 & 3 == 0 {
                position
            } else {
                read_i64(env, a1).unwrap_or(0) + if a2 & 3 == 1 { 0 } else { position }
            };
            let end = if flags & 3 == 0 {
                stop
            } else {
                read_i64(env, env.arg(3)).unwrap_or(0)
                    + match flags & 3 {
                        1 => 0,
                        2 => stop,
                        _ => start,
                    }
            };
            let f = state(env).filters.get_mut(&filter).unwrap();
            f.position = start;
            f.stop = Some(end);
            if a2 & 8 != 0 {
                write_i64(env, a1, start);
            }
            if flags & 8 != 0 {
                write_i64(env, env.arg(3), end);
            }
            let sessions: Vec<_> = state(env)
                .sessions
                .iter()
                .filter(|(_, s)| s.splitter == filter)
                .map(|(&id, _)| id)
                .collect();
            for id in sessions {
                stream::seek(env, id, start);
            }
            S_OK
        }
        _ => E_NOTIMPL,
    };
    Outcome::Return(hr)
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
            Some(transition(
                env,
                filter,
                value,
                if method == "Run" { arg_i64(env, 1) } else { 0 },
                Box::new(|_, hr| Outcome::Return(hr)),
            ))
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
            let list = state(env).filters[&filter].pins.clone();
            match super::create(
                env,
                &[Face::EnumPins],
                Obj::EnumPins {
                    filter,
                    list,
                    position: 0,
                },
            ) {
                Ok(faces) => {
                    add_ref(env, filter);
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

/// Move host filter `filter` to `state` (State_Stopped/Paused/Running; `start` is the Run time, 0 otherwise),
/// running any guest calls it needs, then call `then` with the IBaseFilter result.
pub(crate) fn transition(
    env: &mut Env<'_>,
    filter: u32,
    state: u32,
    _start: i64,
    then: Done,
) -> Outcome {
    set_state(env, filter, state);
    let calls = env.ext.get::<State>().filters[&filter]
        .pins
        .iter()
        .filter_map(|p| {
            let pin = &env.ext.get::<State>().pins[p];
            (pin.dir == OUTPUT && pin.allocator != 0 && canonical(env, pin.allocator).is_none())
                .then_some(Call(
                    pin.allocator,
                    if state == graph::STOPPED { 6 } else { 5 },
                    vec![],
                ))
        })
        .collect();
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |env, results| {
            then(
                env,
                results
                    .into_iter()
                    .find(|hr| hr & 0x8000_0000 != 0)
                    .unwrap_or(S_OK),
            )
        }),
    )
}

pub(crate) fn set_state(env: &mut Env<'_>, filter: u32, value: u32) {
    let outputs: Vec<u32> = {
        let s = state(env);
        let Some(f) = s.filters.get_mut(&filter) else {
            return;
        };
        f.state = value;
        if value == graph::STOPPED {
            f.decoder = None;
            f.pushed_bytes.clear();
            f.pushed_time = None;
            f.flushing = false;
        }
        f.pins
            .iter()
            .filter_map(|p| {
                let pin = &s.pins[p];
                (pin.dir == OUTPUT && pin.allocator != 0).then_some(pin.allocator)
            })
            .collect()
    };
    for allocator in outputs {
        if value == graph::STOPPED {
            sample::decommit(env, allocator);
        } else {
            sample::commit(env, allocator);
        }
    }
    super::dmo::state_changed(env, filter, value);
    super::vmr::state_changed(env, filter, value);
}

fn pin_method(env: &mut Env<'_>, pin: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    if matches!(
        method,
        "EndOfStream" | "BeginFlush" | "EndFlush" | "NewSegment"
    ) {
        match role_of(env, pin) {
            Role::DmoDecoder => {
                if let Some(outcome) = super::dmo::pin_method(env, pin, method) {
                    return Some(outcome);
                }
            }
            Role::VideoRenderer => {
                if let Some(outcome) = super::vmr::pin_method(env, pin, method) {
                    return Some(outcome);
                }
            }
            _ => {}
        }
    }
    if matches!(role_of(env, pin), Role::VideoDecoder | Role::AudioDecoder)
        && state(env).pins[&pin].dir == INPUT
        && matches!(
            method,
            "EndOfStream" | "BeginFlush" | "EndFlush" | "NewSegment"
        )
    {
        return Some(decoder_event(env, pin, method));
    }
    let a1 = env.arg(1);
    match method {
        "Connect" => Some(connect(env, pin, a1, env.arg(2))),
        "ReceiveConnection" => Some(receive_connection(env, pin, a1, env.arg(2))),
        "Disconnect" => Some(disconnect(env, pin)),
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
            let name = if matches!(
                state(env).filters[&filter].role,
                Role::VideoDecoder | Role::AudioDecoder
            ) {
                if state(env).filters[&filter].role == Role::VideoDecoder {
                    if dir == INPUT { "Input" } else { "Output" }
                } else if dir == INPUT {
                    "XForm In"
                } else {
                    "XForm Out"
                }
            } else {
                state(env).pins[&pin].name
            };
            let name: Vec<u16> = name.encode_utf16().take(127).collect();
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
            if a1 == 0 || !env.cpu.accessible(a1, 4, Prot::WRITE) {
                return r(E_POINTER);
            }
            let name: Vec<u8> = state(env).pins[&pin]
                .name
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect();
            let text = adv_os::com::task_alloc(env, name.len() as u32);
            if text == 0 {
                return r(E_OUTOFMEMORY);
            }
            if env.cpu.write(text, &name).is_err() || env.cpu.write_u32(a1, text).is_err() {
                adv_os::com::task_free(env, text);
                return r(E_POINTER);
            }
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

/// Host input allocators and delivery. MPEG decoders accept compressed samples;
/// splitter inputs pull from IAsyncReader and reject pushed samples.
fn input_method(env: &mut Env<'_>, pin: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    match role_of(env, pin) {
        Role::DmoDecoder => return super::dmo::input_method(env, pin, method),
        Role::VideoRenderer => return super::vmr::input_method(env, pin, method),
        _ => {}
    }
    let role = role_of(env, pin);
    if matches!(role, Role::VideoDecoder | Role::AudioDecoder) {
        match method {
            "GetAllocator" => {
                let output = env.arg(1);
                if output == 0 {
                    return r(E_POINTER);
                }
                let mut allocator = state(env).pins[&pin].allocator;
                if allocator == 0 {
                    allocator = match sample::create_allocator(env, 0, 0) {
                        Ok(a) => a,
                        Err(hr) => return r(hr),
                    };
                    sample::decommit(env, allocator);
                    state(env).pins.get_mut(&pin).unwrap().allocator = allocator;
                }
                let _ = env.cpu.write_u32(output, allocator);
                return Some(guest_call(env, allocator, 1, vec![], |_, _| {
                    Outcome::Return(S_OK)
                }));
            }
            "NotifyAllocator" => {
                let allocator = env.arg(1);
                if allocator == 0 {
                    return r(E_POINTER);
                }
                let previous = std::mem::replace(
                    &mut state(env).pins.get_mut(&pin).unwrap().allocator,
                    allocator,
                );
                let mut calls = VecDeque::from([Call(allocator, 1, vec![])]);
                if previous != 0 {
                    calls.push_back(Call(previous, 2, vec![]));
                }
                return Some(chain(
                    env,
                    calls,
                    Vec::new(),
                    Box::new(|_, _| Outcome::Return(S_OK)),
                ));
            }
            "ReceiveCanBlock" => return r(S_OK),
            "Receive" => return Some(receive_compressed(env, pin, env.arg(1))),
            _ => {}
        }
    }
    match method {
        "GetAllocator" => {
            let _ = env.cpu.write_u32(env.arg(1), 0);
            r(VFW_E_NO_ALLOCATOR)
        }
        "NotifyAllocator" => r(S_OK),
        "GetAllocatorRequirements" => r(E_NOTIMPL),
        "Receive" => {
            adv_abi::warn_unavailable(
                env,
                "IMemInputPin::Receive",
                "pushed media samples are not supported",
            );
            r(E_NOTIMPL)
        }
        "ReceiveMultiple" => {
            let processed = env.arg(3);
            if processed == 0
                || !env.cpu.accessible(processed, 4, Prot::WRITE)
                || env.cpu.write_u32(processed, 0).is_err()
            {
                return r(E_POINTER);
            }
            adv_abi::warn_unavailable(
                env,
                "IMemInputPin::ReceiveMultiple",
                "pushed media samples are not supported",
            );
            r(E_NOTIMPL)
        }
        "ReceiveCanBlock" => r(S_FALSE),
        _ => None,
    }
}

fn decoder_event(env: &mut Env<'_>, pin: u32, method: &str) -> Outcome {
    let filter = state(env).pins[&pin].filter;
    let out = state(env).filters[&filter].pins[1];
    let peer = state(env).pins[&out].peer;
    if method == "EndOfStream" {
        if state(env).filters[&filter].flushing {
            return Outcome::Return(S_FALSE);
        }
        if role_of(env, pin) == Role::VideoDecoder
            && !state(env).filters[&filter].pushed_bytes.is_empty()
        {
            let bytes =
                std::mem::take(&mut state(env).filters.get_mut(&filter).unwrap().pushed_bytes);
            return decode_push(env, pin, bytes, true);
        }
    }
    if method == "BeginFlush" || method == "EndFlush" {
        let f = state(env).filters.get_mut(&filter).unwrap();
        f.flushing = method == "BeginFlush";
        f.decoder = None;
        f.pushed_bytes.clear();
        f.pushed_time = None;
    }
    let (slot, args) = match method {
        "EndOfStream" => (14, vec![]),
        "BeginFlush" => (15, vec![]),
        "EndFlush" => (16, vec![]),
        _ => (17, (1..=6).map(|i| env.arg(i)).collect()),
    };
    if peer == 0 {
        Outcome::Return(S_OK)
    } else {
        guest_call(env, peer, slot, args, |_, hr| Outcome::Return(hr))
    }
}

fn receive_compressed(env: &mut Env<'_>, pin: u32, sample: u32) -> Outcome {
    let filter = state(env).pins[&pin].filter;
    let f = &state(env).filters[&filter];
    if f.state == graph::STOPPED {
        return Outcome::Return(VFW_E_WRONG_STATE);
    }
    if f.flushing {
        return Outcome::Return(S_FALSE);
    }
    let Some(scratch) = alloc(env, 24) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    chain(
        env,
        VecDeque::from([
            Call(sample, 3, vec![scratch]),
            Call(sample, 11, vec![]),
            Call(sample, 5, vec![scratch + 8, scratch + 16]),
        ]),
        Vec::new(),
        Box::new(move |env, results| {
            let pointer = env.cpu.read_u32(scratch).unwrap_or(0);
            let time = (results[2] & 0x8000_0000 == 0)
                .then(|| read_i64(env, scratch + 8))
                .flatten();
            let mut bytes = vec![0; results[1] as usize];
            let read = env.cpu.read(pointer, &mut bytes);
            free(env, scratch);
            if results[0] != S_OK || read.is_err() {
                return Outcome::Return(E_POINTER);
            }
            state(env).filters.get_mut(&filter).unwrap().pushed_time = time;
            if role_of(env, pin) == Role::VideoDecoder {
                state(env)
                    .filters
                    .get_mut(&filter)
                    .unwrap()
                    .pushed_bytes
                    .extend(bytes);
                Outcome::Return(S_OK)
            } else {
                decode_push(env, pin, bytes, false)
            }
        }),
    )
}

fn decode_push(env: &mut Env<'_>, pin: u32, bytes: Vec<u8>, eos: bool) -> Outcome {
    let filter = state(env).pins[&pin].filter;
    if state(env).filters[&filter].decoder.is_none() {
        let mt = state(env).pins[&pin].mt.as_ref().unwrap();
        let format = if mt.major == MEDIATYPE_VIDEO {
            CodecFormat::Video {
                fourcc: u32::from_le_bytes(*b"MPG1"),
                width: u32::from_le_bytes(mt.format[52..56].try_into().unwrap()),
                height: u32::from_le_bytes(mt.format[56..60].try_into().unwrap()),
                bit_count: 0,
                frame_100ns: u64::from_le_bytes(mt.format[40..48].try_into().unwrap()),
                extradata: mt.format[96..108.min(mt.format.len())].to_vec(),
            }
        } else {
            mt.codec_format().unwrap()
        };
        let decoder = adv_sync::WakeEvent::new()
            .map_err(|_| E_OUTOFMEMORY)
            .and_then(|wake| Worker::transform(format, wake).map_err(|_| VFW_E_TYPE_NOT_ACCEPTED));
        match decoder {
            Ok(decoder) => state(env).filters.get_mut(&filter).unwrap().decoder = Some(decoder),
            Err(hr) => return Outcome::Return(hr),
        }
    }
    let f = &state(env).filters[&filter];
    if f.decoder
        .as_ref()
        .unwrap()
        .send(Command::Decode(bytes, f.pushed_time, 0))
        .is_err()
    {
        return Outcome::Return(0x8000_4005);
    }
    decoder_results(env, pin, eos, false)
}

fn decoder_results(env: &mut Env<'_>, pin: u32, eos: bool, draining: bool) -> Outcome {
    let filter = state(env).pins[&pin].filter;
    let event = state(env).filters[&filter].decoder.as_ref().unwrap().poll();
    match event {
        None => Outcome::block(Wait::Until(env.sys.clock.now_us() + 1000), move |env, _| {
            decoder_results(env, pin, eos, draining)
        }),
        Some(Event::Sample(_, Ok(Some(frame)))) => deliver_decoded(
            env,
            pin,
            frame,
            Box::new(move |env, hr| {
                if hr & 0x8000_0000 != 0 {
                    Outcome::Return(hr)
                } else {
                    decoder_results(env, pin, eos, draining)
                }
            }),
        ),
        Some(Event::Sample(_, Err(_)) | Event::Done(Err(_)) | Event::Ready(Err(_))) => {
            Outcome::Return(0x8000_4005)
        }
        Some(Event::Done(Ok(()))) if eos && !draining => {
            let _ = state(env).filters[&filter]
                .decoder
                .as_ref()
                .unwrap()
                .send(Command::Drain);
            decoder_results(env, pin, eos, true)
        }
        Some(Event::Done(Ok(()))) => {
            if eos {
                state(env).filters.get_mut(&filter).unwrap().decoder = None;
                let out = state(env).filters[&filter].pins[1];
                let peer = state(env).pins[&out].peer;
                return guest_call(env, peer, 14, vec![], |_, hr| Outcome::Return(hr));
            }
            Outcome::Return(S_OK)
        }
        _ => decoder_results(env, pin, eos, draining),
    }
}

fn deliver_decoded(
    env: &mut Env<'_>,
    pin: u32,
    frame: adv_device::media::wine::Sample,
    then: Done,
) -> Outcome {
    let filter = state(env).pins[&pin].filter;
    let out = state(env).filters[&filter].pins[1];
    let p = &state(env).pins[&out];
    let (allocator, mem) = (p.allocator, p.mem);
    let mt = p.mt.as_ref().unwrap().clone();
    let Some(scratch) = alloc(env, 40) else {
        return then(env, E_OUTOFMEMORY);
    };
    write_i64(env, scratch + 16, frame.pts);
    write_i64(env, scratch + 24, frame.pts + frame.duration);
    guest_call(
        env,
        allocator,
        7,
        vec![scratch, scratch + 16, scratch + 24, 0],
        move |env, hr| {
            if hr != S_OK {
                free(env, scratch);
                return then(env, hr);
            }
            let sample = env.cpu.read_u32(scratch).unwrap_or(0);
            chain(
                env,
                VecDeque::from([
                    Call(sample, 3, vec![scratch + 4]),
                    Call(sample, 4, vec![]),
                    Call(sample, 13, vec![scratch + 8]),
                ]),
                Vec::new(),
                Box::new(move |env, results| {
                    let dynamic = env.cpu.read_u32(scratch + 8).unwrap_or(0);
                    let mt = if results[2] == S_OK {
                        MediaType::read(env, dynamic).unwrap()
                    } else {
                        mt
                    };
                    if results[2] == S_OK {
                        free_type_block(env, dynamic);
                        adv_os::com::task_free(env, dynamic);
                    }
                    let output = if mt.major == MEDIATYPE_AUDIO {
                        DecodedOutput::Audio {
                            sample: if mt.format[14] == 8 { 1 } else { 2 },
                            channels: u32::from(u16::from_le_bytes(
                                mt.format[2..4].try_into().unwrap(),
                            )),
                            rate: u32::from_le_bytes(mt.format[4..8].try_into().unwrap()),
                        }
                    } else {
                        let (width, height, _) = mt.video_geometry().unwrap();
                        let pixel = if mt.sub == MEDIASUBTYPE_RGB32 {
                            2
                        } else if mt.sub == MEDIASUBTYPE_RGB24 {
                            3
                        } else if mt.sub == MEDIASUBTYPE_RGB565 {
                            5
                        } else if mt.sub == MEDIASUBTYPE_RGB555 {
                            4
                        } else if mt.sub == fourcc_type(u32::from_le_bytes(*b"UYVY")) {
                            11
                        } else if mt.sub == fourcc_type(u32::from_le_bytes(*b"YUY2")) {
                            12
                        } else if mt.sub == amovie(0xe436_eb7a) {
                            2
                        } else {
                            11
                        };
                        let height = if [
                            MEDIASUBTYPE_RGB32,
                            MEDIASUBTYPE_RGB24,
                            MEDIASUBTYPE_RGB565,
                            MEDIASUBTYPE_RGB555,
                            amovie(0xe436_eb7a),
                        ]
                        .contains(&mt.sub)
                        {
                            -height
                        } else {
                            height
                        };
                        DecodedOutput::Video {
                            pixel,
                            width,
                            height,
                            align: 1,
                        }
                    };
                    let decoded = frame.size(output).and_then(|size| {
                        let mut bytes = vec![0; size];
                        let written = frame.copy(output, &mut bytes)?;
                        bytes.truncate(written);
                        Ok(bytes)
                    });
                    let mut bytes = match decoded {
                        Ok(bytes) => bytes,
                        Err(_) => {
                            return guest_call(env, sample, 2, vec![], move |env, _| {
                                free(env, scratch);
                                then(env, 0x8000_4005)
                            });
                        }
                    };
                    if mt.sub == amovie(0xe436_eb7a) {
                        bytes = bytes
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|p| (p[2] & 0xe0) | ((p[1] >> 3) & 0x1c) | (p[0] >> 6))
                            .collect();
                    } else if mt.sub == fourcc_type(u32::from_le_bytes(*b"Y41P")) {
                        bytes = bytes
                            .as_chunks::<16>()
                            .0
                            .iter()
                            .flat_map(|p| {
                                [
                                    ((u16::from(p[0]) + u16::from(p[4])) / 2) as u8,
                                    p[1],
                                    ((u16::from(p[2]) + u16::from(p[6])) / 2) as u8,
                                    p[3],
                                    ((u16::from(p[8]) + u16::from(p[12])) / 2) as u8,
                                    p[5],
                                    ((u16::from(p[10]) + u16::from(p[14])) / 2) as u8,
                                    p[7],
                                    p[9],
                                    p[11],
                                    p[13],
                                    p[15],
                                ]
                            })
                            .collect();
                    }
                    let length = bytes.len().min(results[1] as usize);
                    let pointer = env.cpu.read_u32(scratch + 4).unwrap_or(0);
                    let _ = env.cpu.write(pointer, &bytes[..length]);
                    let start = if mt.major == MEDIATYPE_VIDEO {
                        state(env).filters[&filter].pushed_time.unwrap_or(0)
                    } else {
                        frame.pts
                    };
                    write_i64(env, scratch + 16, start);
                    write_i64(
                        env,
                        scratch + 24,
                        start
                            + if mt.major == MEDIATYPE_VIDEO {
                                0
                            } else {
                                frame.duration
                            },
                    );
                    chain(
                        env,
                        VecDeque::from([
                            Call(sample, 12, vec![length as u32]),
                            Call(sample, 6, vec![scratch + 16, scratch + 24]),
                            Call(mem, 6, vec![sample]),
                            Call(sample, 2, vec![]),
                        ]),
                        Vec::new(),
                        Box::new(move |env, results| {
                            free(env, scratch);
                            then(env, results[2])
                        }),
                    )
                }),
            )
        },
    )
}

fn accepts(env: &Env<'_>, pin: u32, mt: &MediaType) -> bool {
    let state = env.ext.get::<State>();
    let p = &state.pins[&pin];
    if p.major != GUID_NULL && mt.major != p.major {
        return false;
    }
    match (state.filters[&p.filter].role, p.dir) {
        (Role::Splitter, INPUT) => {
            return [0xe436_eb84, 0xe436_eb85, 0xe436_eb86, 0xe436_eb87]
                .into_iter()
                .any(|id| mt.sub == amovie(id))
                && [GUID_NULL, FORMAT_WAVEFORMATEX].contains(&mt.format_type);
        }
        (Role::Splitter, OUTPUT) if p.major == MEDIATYPE_AUDIO => {
            return offered_types(env, pin).iter().any(|offered| {
                offered.sub == mt.sub
                    && offered.format_type == mt.format_type
                    && offered.format == mt.format
            });
        }
        (Role::VideoDecoder, INPUT) => {
            return [MEDIASUBTYPE_MPEG1PAYLOAD, amovie(0xe436_eb80)].contains(&mt.sub)
                && mt.format.len() >= 96
                && mt.format_type
                    == guid(
                        0x0558_9f82,
                        0xc356,
                        0x11ce,
                        [0xbf, 1, 0, 0xaa, 0, 0x55, 0x59, 0x5a],
                    );
        }
        (Role::AudioDecoder, INPUT) => {
            return [
                MEDIASUBTYPE_MPEG1AUDIOPAYLOAD,
                MEDIASUBTYPE_MPEG1PAYLOAD,
                amovie(0xe436_eb80),
                GUID_NULL,
            ]
            .contains(&mt.sub)
                && mt.format_type == FORMAT_WAVEFORMATEX
                && mt.format.get(18..20).is_some_and(|layer| {
                    layer == 1u16.to_le_bytes() || layer == 2u16.to_le_bytes()
                });
        }
        (Role::VideoDecoder | Role::AudioDecoder, OUTPUT) => {
            return offered_types(env, pin).iter().any(|offered| {
                offered.sub == mt.sub
                    && offered.format_type == mt.format_type
                    && offered.format == mt.format
            });
        }
        _ => {}
    }
    if p.dir == INPUT && state.filters[&p.filter].role == Role::DmoDecoder {
        return super::dmo::accepts_input(mt).is_some();
    }
    if p.dir == INPUT && state.filters[&p.filter].role == Role::VideoRenderer {
        return super::vmr::accepts_input(mt);
    }
    true
}

/// Types a pin proposes. Decoder outputs depend on their connected input.
fn offered_types(env: &Env<'_>, pin: u32) -> Vec<MediaType> {
    let state = env.ext.get::<State>();
    let p = &state.pins[&pin];
    let role = state.filters.get(&p.filter).map(|f| f.role);
    match (role, p.dir) {
        (Some(Role::Splitter), INPUT) => [
            MEDIASUBTYPE_MPEG1SYSTEM,
            amovie(0xe436_eb85),
            amovie(0xe436_eb86),
            amovie(0xe436_eb87),
        ]
        .into_iter()
        .map(|sub| MediaType {
            major: MEDIATYPE_STREAM,
            sub,
            fixed_size: true,
            temporal: true,
            sample_size: 1,
            format_type: GUID_NULL,
            format: Vec::new(),
        })
        .collect(),
        (Some(Role::Splitter), OUTPUT) if p.major == MEDIATYPE_AUDIO => {
            let Some(mt) = p.types.first().cloned() else {
                return Vec::new();
            };
            let mut types = vec![mt.clone()];
            let mut payload = mt.clone();
            payload.sub = MEDIASUBTYPE_MPEG1PAYLOAD;
            types.push(payload);
            if mt.format.get(18..20) == Some(&4u16.to_le_bytes()) {
                let mut mp3 = mt;
                mp3.sub = fourcc_type(0x55);
                mp3.format[0..2].copy_from_slice(&0x55u16.to_le_bytes());
                mp3.format[12..14].copy_from_slice(&1u16.to_le_bytes());
                mp3.format[16..18].copy_from_slice(&12u16.to_le_bytes());
                let block = types[0].format[12..14].to_vec();
                mp3.format.truncate(18);
                mp3.format.extend_from_slice(&1u16.to_le_bytes());
                mp3.format.extend_from_slice(&0u32.to_le_bytes());
                mp3.format.extend_from_slice(&block);
                mp3.format.extend_from_slice(&1u16.to_le_bytes());
                mp3.format.extend_from_slice(&0u16.to_le_bytes());
                types.push(mp3);
            }
            types
        }
        (Some(Role::Splitter), OUTPUT) => p.types.clone(),
        (Some(Role::FileSource), OUTPUT) => p.mt.iter().cloned().collect(),
        (Some(Role::VideoDecoder), OUTPUT) => decoded_video_types(env, pin),
        (Some(Role::AudioDecoder), OUTPUT) => decoded_audio_types(env, pin),
        (Some(Role::DmoDecoder), OUTPUT) => super::dmo::output_types(env, pin),
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
        let Some(input) = decoder_input(env, pin) else {
            return Vec::new();
        };
        return [
            (
                fourcc_type(u32::from_le_bytes(*b"YV12")),
                12u16,
                u32::from_le_bytes(*b"YV12"),
            ),
            (
                fourcc_type(u32::from_le_bytes(*b"Y41P")),
                12,
                u32::from_le_bytes(*b"Y41P"),
            ),
            (
                fourcc_type(u32::from_le_bytes(*b"YUY2")),
                16,
                u32::from_le_bytes(*b"YUY2"),
            ),
            (
                fourcc_type(u32::from_le_bytes(*b"UYVY")),
                16,
                u32::from_le_bytes(*b"UYVY"),
            ),
            (MEDIASUBTYPE_RGB24, 24, 0),
            (MEDIASUBTYPE_RGB32, 32, 0),
            (MEDIASUBTYPE_RGB565, 16, 3),
            (MEDIASUBTYPE_RGB555, 16, 0),
            (amovie(0xe436_eb7a), 8, 0),
        ]
        .into_iter()
        .map(|(sub, bits, compression)| {
            let mut format = input.format[..88].to_vec();
            let width = u32::from_le_bytes(format[52..56].try_into().unwrap());
            let height = u32::from_le_bytes(format[56..60].try_into().unwrap());
            let size = width * height * u32::from(bits) / 8;
            let frame = i64::from_le_bytes(format[40..48].try_into().unwrap());
            format[32..36].copy_from_slice(
                &((u64::from(size) * 8 * 10_000_000 / frame as u64) as u32).to_le_bytes(),
            );
            format[60..62].copy_from_slice(&1u16.to_le_bytes());
            format[62..64].copy_from_slice(&bits.to_le_bytes());
            format[64..68].copy_from_slice(&compression.to_le_bytes());
            format[68..72].copy_from_slice(&size.to_le_bytes());
            if bits == 8 {
                format[80..84].copy_from_slice(&256u32.to_le_bytes());
            }
            MediaType {
                major: MEDIATYPE_VIDEO,
                sub,
                fixed_size: true,
                temporal: false,
                sample_size: size,
                format_type: FORMAT_VIDEOINFO,
                format,
            }
        })
        .collect();
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

fn decoder_input<'a>(env: &'a Env<'_>, pin: u32) -> Option<&'a MediaType> {
    let state = env.ext.get::<State>();
    let f = &state.filters[&state.pins[&pin].filter];
    let input = &state.pins[&f.pins[0]];
    (input.peer != 0).then_some(input.mt.as_ref()).flatten()
}

fn decoded_audio_types(env: &Env<'_>, pin: u32) -> Vec<MediaType> {
    if let Some(splitter) = upstream_source(env, pin) {
        return env.ext.get::<State>().filters[&splitter]
            .source
            .as_ref()
            .and_then(|s| s.info.as_ref()?.audio.as_ref())
            .map(|audio| vec![MediaType::audio_pcm(audio.sample_rate, audio.channels)])
            .unwrap_or_default();
    }
    let Some(input) = decoder_input(env, pin) else {
        return Vec::new();
    };
    let rate = u32::from_le_bytes(input.format[4..8].try_into().unwrap());
    let channels = u16::from_le_bytes(input.format[2..4].try_into().unwrap());
    [16u16, 8]
        .into_iter()
        .map(|bits| {
            let mut mt = MediaType::audio_pcm(rate, channels);
            let align = channels * bits / 8;
            mt.sample_size = u32::from(align);
            mt.format[8..12].copy_from_slice(&(rate * u32::from(align)).to_le_bytes());
            mt.format[12..14].copy_from_slice(&align.to_le_bytes());
            mt.format[14..16].copy_from_slice(&bits.to_le_bytes());
            mt
        })
        .collect()
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
    let (out_role, in_role) = (
        state.filters[&state.pins[&out].filter].role,
        state.filters[&i.filter].role,
    );
    // DMO video is pushed only to input pins that receive samples.
    if out_role == Role::DmoDecoder && in_role == Role::VideoRenderer {
        return VFW_E_NO_ACCEPTABLE_TYPES;
    }
    if in_role == Role::DmoDecoder && !super::dmo::connect_input(env, input, &mt) {
        return VFW_E_TYPE_NOT_ACCEPTED;
    }
    let state = self::state(env);
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
    let filter = state(env).pins[&out].filter;
    if state(env).filters[&filter].state != graph::STOPPED {
        return Outcome::Return(0x8004_0224);
    }
    let mut candidates = offered_types(env, out);
    let wanted = (pmt != 0).then(|| MediaType::read(env, pmt).ok()).flatten();
    if let Some(wanted) = &wanted {
        if role_of(env, out) == Role::VideoDecoder
            && wanted.major == GUID_NULL
            && wanted.format.get(32..36) == Some(&[0; 4])
        {
            return Outcome::Return(VFW_E_NO_ACCEPTABLE_TYPES);
        }
        if wanted.major != GUID_NULL && wanted.format_type != GUID_NULL {
            if wanted.sub == fourcc_type(u32::from_le_bytes(*b"YV12")) || !accepts(env, out, wanted)
            {
                return Outcome::Return(VFW_E_TYPE_NOT_ACCEPTED);
            }
            return try_types(env, out, input, VecDeque::from([wanted.clone()]));
        } else {
            candidates.retain(|c| {
                (wanted.major == GUID_NULL || c.major == wanted.major)
                    && (wanted.sub == GUID_NULL || c.sub == wanted.sub)
                    && (wanted.format_type == GUID_NULL || c.format_type == wanted.format_type)
            });
        }
    }
    let Some(scratch) = alloc(env, 8) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    guest_call(env, input, 12, vec![scratch], move |env, hr| {
        if hr != S_OK {
            free(env, scratch);
            return try_types(env, out, input, candidates.into());
        }
        peer_types(env, out, input, scratch, wanted, Vec::new(), candidates)
    })
}

fn peer_types(
    env: &mut Env<'_>,
    out: u32,
    input: u32,
    scratch: u32,
    wanted: Option<MediaType>,
    mut preferred: Vec<MediaType>,
    candidates: Vec<MediaType>,
) -> Outcome {
    let enumerator = env.cpu.read_u32(scratch).unwrap_or(0);
    guest_call(
        env,
        enumerator,
        3,
        vec![1, scratch + 4, 0],
        move |env, hr| {
            if hr != S_OK {
                free(env, scratch);
                preferred.extend(candidates);
                return guest_call(env, enumerator, 2, vec![], move |env, _| {
                    try_types(env, out, input, preferred.into())
                });
            }
            let pointer = env.cpu.read_u32(scratch + 4).unwrap_or(0);
            if let Ok(mt) = MediaType::read(env, pointer)
                && accepts(env, out, &mt)
                && wanted.as_ref().is_none_or(|w| {
                    (w.major == GUID_NULL || w.major == mt.major)
                        && (w.sub == GUID_NULL || w.sub == mt.sub)
                        && (w.format_type == GUID_NULL || w.format_type == mt.format_type)
                })
            {
                preferred.push(mt);
            }
            free_type_block(env, pointer);
            adv_os::com::task_free(env, pointer);
            peer_types(env, out, input, scratch, wanted, preferred, candidates)
        },
    )
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
    if role_of(env, out) == Role::VideoDecoder
        && candidate.sub == fourcc_type(u32::from_le_bytes(*b"YV12"))
    {
        return try_types(env, out, input, candidates);
    }
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

/// Hold the guest pin, find its IMemInputPin and negotiate its allocator.
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
                return abort_guest_connection(env, out, input, VFW_E_NO_TRANSPORT);
            }
            state(env).pins.get_mut(&out).unwrap().mem = mem;
            if matches!(role_of(env, out), Role::VideoDecoder | Role::AudioDecoder)
                && upstream_source(env, out).is_none()
            {
                return decoder_allocator(env, out, input, mem, mt);
            }
            let size = if role_of(env, out) == Role::Splitter {
                let filter = state(env).pins[&out].filter;
                state(env).filters[&filter]
                    .packets
                    .as_ref()
                    .map_or(1, |media| {
                        media
                            .streams
                            .iter()
                            .filter(|s| {
                                matches!(s.format, adv_device::media::CodecFormat::Audio { .. })
                                    == (mt.major == MEDIATYPE_AUDIO)
                            })
                            .map(|s| s.max_packet)
                            .max()
                            .unwrap_or(1)
                    })
                    .max(1)
            } else if mt.major == MEDIATYPE_AUDIO {
                // Keep a second of PCM, as the DirectSound renderer does.
                u32::from_le_bytes(mt.format[8..12].try_into().unwrap())
            } else {
                mt.sample_size.max(1)
            };
            let allocator = match sample::create_allocator(env, 3, size) {
                Ok(allocator) => allocator,
                Err(error) => return abort_guest_connection(env, out, input, error),
            };
            let p = state(env).pins.get_mut(&out).unwrap();
            p.mem = mem;
            p.allocator = allocator;
            let face = state(env).allocators[&allocator].face;
            guest_call(env, mem, 4, vec![face, 0], move |env, hr| {
                if hr & 0x8000_0000 != 0 {
                    log::warn!("DirectShow: NotifyAllocator refused ({hr:#x})");
                    return abort_guest_connection(env, out, input, hr);
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

fn decoder_allocator(env: &mut Env<'_>, out: u32, input: u32, mem: u32, mt: MediaType) -> Outcome {
    let Some(scratch) = alloc(env, 36) else {
        return abort_guest_connection(env, out, input, E_OUTOFMEMORY);
    };
    let (count, size) = if role_of(env, out) == Role::AudioDecoder {
        let source = decoder_input(env, out).unwrap();
        let layer = u16::from_le_bytes(source.format[18..20].try_into().unwrap());
        let align = u16::from_le_bytes(mt.format[12..14].try_into().unwrap());
        (8, u32::from(align) * if layer == 1 { 1536 } else { 4608 })
    } else {
        (1, mt.sample_size)
    };
    guest_call(env, mem, 3, vec![scratch], move |env, hr| {
        let allocator = env.cpu.read_u32(scratch).unwrap_or(0);
        if hr != S_OK || allocator == 0 {
            let allocator = match sample::create_allocator(env, count, size) {
                Ok(a) => a,
                Err(hr) => {
                    free(env, scratch);
                    return abort_guest_connection(env, out, input, hr);
                }
            };
            sample::decommit(env, allocator);
            free(env, scratch);
            return notify_decoder_allocator(env, out, input, mem, allocator);
        }
        let props: Vec<_> = [count, size, 1, 0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        let _ = env.cpu.write(scratch + 4, &props);
        guest_call(
            env,
            allocator,
            3,
            vec![scratch + 4, scratch + 20],
            move |env, hr| {
                free(env, scratch);
                if hr & 0x8000_0000 != 0 {
                    return guest_call(env, allocator, 2, vec![], move |env, _| {
                        abort_guest_connection(env, out, input, hr)
                    });
                }
                notify_decoder_allocator(env, out, input, mem, allocator)
            },
        )
    })
}

fn notify_decoder_allocator(
    env: &mut Env<'_>,
    out: u32,
    input: u32,
    mem: u32,
    allocator: u32,
) -> Outcome {
    state(env).pins.get_mut(&out).unwrap().allocator = allocator;
    guest_call(env, mem, 4, vec![allocator, 0], move |env, hr| {
        if hr & 0x8000_0000 != 0 {
            abort_guest_connection(env, out, input, hr)
        } else {
            Outcome::Return(S_OK)
        }
    })
}

fn abort_guest_connection(env: &mut Env<'_>, out: u32, input: u32, error: u32) -> Outcome {
    let mem = state(env).pins[&out].mem;
    let allocator = state(env).pins[&out].allocator;
    let foreign_allocator = allocator != 0 && canonical(env, allocator).is_none();
    disconnect_host(env, out);
    let mut calls = VecDeque::new();
    if foreign_allocator {
        calls.push_back(Call(allocator, 2, vec![]));
    }
    if mem != 0 {
        calls.push_back(Call(mem, 2, vec![]));
    }
    calls.push_back(Call(input, 5, vec![]));
    calls.push_back(Call(input, 2, vec![]));
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |_, _| Outcome::Return(error)),
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
        Some(pin) => {
            let filter = state(env).pins[&pin].filter;
            if state(env).filters[&filter].state != graph::STOPPED {
                return Outcome::Return(0x8004_0224);
            }
            let hr = if state(env).pins[&pin].peer == 0 {
                S_FALSE
            } else {
                S_OK
            };
            let calls = disconnect_owned(env, pin);
            chain(
                env,
                calls,
                Vec::new(),
                Box::new(move |_, _| Outcome::Return(hr)),
            )
        }
        None => guest_call(env, pin, 5, vec![], |_, hr| Outcome::Return(hr)),
    }
}

/// Detach a host pin and return the guest releases owned by its connection.
/// Host-to-host connections borrow their peer; guest peers, reader interfaces
/// and memory input interfaces each hold a separate COM reference.
pub(super) fn disconnect_owned(env: &mut Env<'_>, pin: u32) -> VecDeque<Call> {
    let p = &state(env).pins[&pin];
    let (peer, mem, reader, allocator) = (p.peer, p.mem, p.reader, p.allocator);
    let foreign_allocator = allocator != 0 && canonical(env, allocator).is_none();
    let guest_peer = peer != 0 && host_pin(env, peer).is_none();
    let seeking = if role_of(env, pin) == Role::DmoDecoder {
        super::dmo::take_upstream(env, pin)
    } else {
        0
    };
    disconnect_host(env, pin);
    state(env).pins.get_mut(&pin).unwrap().reader = 0;
    let mut calls = VecDeque::new();
    for pointer in [
        mem,
        reader,
        seeking,
        if foreign_allocator { allocator } else { 0 },
    ] {
        if pointer != 0 {
            calls.push_back(Call(pointer, 2, vec![]));
        }
    }
    if guest_peer {
        calls.push_back(Call(peer, 2, vec![]));
    }
    calls
}

/// Graph removal disconnects both ends; individual IPin::Disconnect only owns
/// its own end. Keep the host filter alive until these guest callbacks finish.
pub(super) fn disconnect_filter(env: &mut Env<'_>, filter: u32) -> VecDeque<Call> {
    let pins = state(env)
        .filters
        .get(&filter)
        .map(|f| f.pins.clone())
        .unwrap_or_default();
    let mut calls = VecDeque::new();
    for pin in pins {
        let peer = state(env).pins[&pin].peer;
        if peer != 0 {
            if let Some(peer) = host_pin(env, peer) {
                calls.extend(disconnect_owned(env, peer));
            } else {
                calls.push_back(Call(peer, 5, vec![]));
            }
        }
        calls.extend(disconnect_owned(env, pin));
    }
    calls
}

fn disconnect_host(env: &mut Env<'_>, pin: u32) -> u32 {
    let p = state(env).pins.get_mut(&pin).unwrap();
    if p.peer == 0 {
        return S_FALSE;
    }
    super::dmo::disconnected(env, pin);
    let p = state(env).pins.get_mut(&pin).unwrap();
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
    let p = &env.ext.get::<State>().pins[&pin];
    if env.ext.get::<State>().filters[&p.filter].state != graph::STOPPED {
        return Outcome::Return(0x8004_0224);
    }
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
    if state(env).filters[&filter].role == Role::DmoDecoder
        && !super::dmo::connect_input(env, pin, &mt)
    {
        return Outcome::Return(VFW_E_TYPE_NOT_ACCEPTED);
    }
    if state(env).filters[&filter].role != Role::Splitter {
        let p = state(env).pins.get_mut(&pin).unwrap();
        p.peer = connector;
        p.mt = Some(mt);
        let dmo = state(env).filters[&filter].role == Role::DmoDecoder;
        return guest_call(env, connector, 1, vec![], move |env, _| {
            if dmo {
                super::dmo::learn_upstream(env, pin, connector)
            } else {
                Outcome::Return(S_OK)
            }
        });
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
        return guest_call(env, reader, 2, vec![], |_, _| {
            Outcome::Return(E_OUTOFMEMORY)
        });
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

pub(super) fn mpeg_video_type(bytes: &[u8]) -> Option<MediaType> {
    let at = bytes.windows(4).position(|b| b == [0, 0, 1, 0xb3])?;
    let header = bytes.get(at..at + 12)?;
    let width = (u32::from(header[4]) << 4) | u32::from(header[5] >> 4);
    let height = (u32::from(header[5] & 15) << 8) | u32::from(header[6]);
    let rate = [
        (0, 1),
        (24000, 1001),
        (24, 1),
        (25, 1),
        (30000, 1001),
        (30, 1),
        (50, 1),
        (60000, 1001),
        (60, 1),
    ]
    .get(usize::from(header[7] & 15))
    .copied()?;
    let frame = 10_000_000 * rate.1 / rate.0.max(1);
    let mut mt = MediaType::video_rgb(MEDIASUBTYPE_MPEG1PAYLOAD, width, height, frame);
    mt.fixed_size = true;
    mt.temporal = false;
    mt.sample_size = 1;
    mt.format_type = guid(
        0x0558_9f82,
        0xc356,
        0x11ce,
        [0xbf, 1, 0, 0xaa, 0, 0x55, 0x59, 0x5a],
    );
    mt.format[60..72].fill(0);
    mt.format.resize(112, 0);
    mt.format[92..96].copy_from_slice(&12u32.to_le_bytes());
    mt.format[96..108].copy_from_slice(header);
    Some(mt)
}

pub(super) fn mpeg_audio_type(bytes: &[u8]) -> Option<MediaType> {
    let h = bytes.windows(4).find(|h| {
        h[0] == 0xff
            && h[1] & 0xf8 == 0xf8
            && h[1] & 6 != 0
            && h[2] >> 4 != 15
            && h[2] >> 4 != 0
            && h[2] & 12 != 12
    })?;
    let layer = 4 - ((h[1] >> 1) & 3);
    let rates = [44100u32, 48000, 32000];
    let rate = rates[usize::from((h[2] >> 2) & 3)];
    let bitrates = [
        [
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
        ],
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
        ],
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ],
    ];
    let bitrate = bitrates[usize::from(layer - 1)][usize::from(h[2] >> 4)] * 1000u32;
    let block = if layer == 1 {
        (12 * bitrate / rate + u32::from((h[2] >> 1) & 1)) * 4
    } else {
        144 * bitrate / rate + u32::from((h[2] >> 1) & 1)
    };
    let mut format = Vec::new();
    format.extend_from_slice(&0x50u16.to_le_bytes());
    format.extend_from_slice(&(if h[3] >> 6 == 3 { 1u16 } else { 2 }).to_le_bytes());
    format.extend_from_slice(&rate.to_le_bytes());
    format.extend_from_slice(&(bitrate / 8).to_le_bytes());
    format.extend_from_slice(&(block as u16).to_le_bytes());
    format.extend_from_slice(&0u16.to_le_bytes());
    format.extend_from_slice(&22u16.to_le_bytes());
    format.extend_from_slice(&(1u16 << (layer - 1)).to_le_bytes());
    format.extend_from_slice(&bitrate.to_le_bytes());
    format.extend_from_slice(&(1u16 << (h[3] >> 6)).to_le_bytes());
    format.extend_from_slice(&(u16::from((h[3] >> 4) & 3) << 12).to_le_bytes());
    format.extend_from_slice(&(u16::from(h[3] & 3) + 1).to_le_bytes());
    let flags = u16::from(h[2] & 1)
        | (u16::from(h[3] & 8) >> 2)
        | u16::from(h[3] & 4)
        | u16::from(h[1] & 1) << 3
        | 16;
    format.extend_from_slice(&flags.to_le_bytes());
    format.extend_from_slice(&[0; 8]);
    Some(MediaType {
        major: MEDIATYPE_AUDIO,
        sub: MEDIASUBTYPE_MPEG1AUDIOPAYLOAD,
        fixed_size: true,
        temporal: false,
        sample_size: 1,
        format_type: FORMAT_WAVEFORMATEX,
        format,
    })
}

/// Probe the stream and expose one output pin per elementary stream.
pub(super) fn attach_source(
    env: &mut Env<'_>,
    pin: u32,
    connector: u32,
    reader: u32,
    mt: MediaType,
    bytes: Vec<u8>,
) -> Outcome {
    let length = bytes.len();
    let audio_type = mpeg_audio_type(&bytes);
    let video_type = mpeg_video_type(&bytes);
    let packets = adv_device::media::demux(bytes.clone()).ok();
    let (decoder, info) = match Decoder::open_memory(bytes) {
        Ok(decoder) => {
            let info = decoder.info().clone();
            if !["mpeg", "mpegvideo", "mp3"].contains(&info.container.as_str()) {
                return guest_call(env, reader, 2, vec![], |_, _| {
                    Outcome::Return(VFW_E_TYPE_NOT_ACCEPTED)
                });
            }
            (Some(decoder), Some(info))
        }
        Err(error) => {
            log::warn!("DirectShow: cannot decode {length}-byte stream: {error}");
            adv_abi::warn_unavailable(env, "MPEG-1 Stream Splitter", &error.to_string());
            // QueryInterface acquired this reader before the probe. Reject the
            // connection without publishing a source or invented output types.
            return guest_call(env, reader, 2, vec![], |_, _| {
                Outcome::Return(VFW_E_UNSUPPORTED_STREAM)
            });
        }
    };
    let filter = state(env).pins[&pin].filter;
    let video = info.as_ref().is_none_or(|i| i.video.is_some());
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
    state(env).filters.get_mut(&filter).unwrap().packets = packets;
    state(env).filters.get_mut(&filter).unwrap().source = Some(Source {
        file_handle: None,
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
        let mt = if major == MEDIATYPE_AUDIO {
            audio_type.clone().unwrap_or(MediaType {
                major,
                sub,
                fixed_size: false,
                temporal: true,
                sample_size: 0,
                format_type: GUID_NULL,
                format: Vec::new(),
            })
        } else {
            video_type.clone().unwrap_or(MediaType {
                major,
                sub,
                fixed_size: false,
                temporal: true,
                sample_size: 0,
                format_type: GUID_NULL,
                format: Vec::new(),
            })
        };
        if let Err(error) = new_pin(env, filter, OUTPUT, name, major, Some(mt)) {
            state(env).filters.get_mut(&filter).unwrap().source = None;
            return guest_call(env, reader, 2, vec![], move |_, _| Outcome::Return(error));
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
            if output == 0 {
                return r(E_POINTER);
            }
            if count > 1 && fetched == 0 {
                return r(if face == Face::EnumPins {
                    E_INVALIDARG
                } else {
                    E_POINTER
                });
            }
            if out_of_sync(env, this) {
                return r(VFW_E_ENUM_OUT_OF_SYNC);
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
            if out_of_sync(env, this) {
                return r(VFW_E_ENUM_OUT_OF_SYNC);
            }
            // quartz's filter enumerator refuses to skip past its end; the
            // pin and media type enumerators skip what remains.
            if let Some(
                Obj::EnumFilters { list, position, .. } | Obj::EnumPins { list, position, .. },
            ) = payload(env, this)
                && count > list.len().saturating_sub(*position)
            {
                return r(if face == Face::EnumPins {
                    S_FALSE
                } else {
                    E_INVALIDARG
                });
            }
            let skipped = advance(env, this, count);
            r(if skipped == count { S_OK } else { S_FALSE })
        }
        "Reset" => {
            let current = filter_list(env, this);
            let pins = match payload(env, this) {
                Some(Obj::EnumPins { filter, .. }) => {
                    Some(env.ext.get::<State>().filters[filter].pins.clone())
                }
                _ => None,
            };
            match payload_mut(env, this) {
                Some(Obj::EnumFilters { list, position, .. }) => {
                    if let Some(current) = current {
                        *list = current;
                    }
                    *position = 0;
                }
                Some(Obj::EnumPins { list, position, .. }) => {
                    *list = pins.unwrap();
                    *position = 0;
                }
                Some(Obj::EnumTypes { position, .. }) => *position = 0,
                _ => {}
            }
            r(S_OK)
        }
        "Clone" => {
            let output = env.arg(1);
            let copy = match payload(env, this) {
                Some(Obj::EnumPins {
                    filter,
                    list,
                    position,
                }) => Obj::EnumPins {
                    filter: *filter,
                    list: list.clone(),
                    position: *position,
                },
                Some(Obj::EnumTypes { types, position }) => Obj::EnumTypes {
                    types: types.clone(),
                    position: *position,
                },
                Some(Obj::EnumFilters {
                    graph,
                    list,
                    position,
                }) => Obj::EnumFilters {
                    graph: *graph,
                    list: list.clone(),
                    position: *position,
                },
                _ => return r(E_UNEXPECTED),
            };
            let owner = match &copy {
                Obj::EnumPins { filter, .. } => Some(*filter),
                _ => None,
            };
            match super::create(env, &[face], copy) {
                Ok(faces) => {
                    if let Some(filter) = owner {
                        add_ref(env, filter);
                    }
                    let _ = env.cpu.write_u32(output, faces[0]);
                    r(S_OK)
                }
                Err(error) => r(error),
            }
        }
        _ => None,
    }
}

/// The filters now in an EnumFilters object's graph, if it is still alive.
fn filter_list(env: &Env<'_>, this: u32) -> Option<Vec<u32>> {
    let Some(Obj::EnumFilters { graph, .. }) = payload(env, this) else {
        return None;
    };
    let graph = env.ext.get::<State>().graphs.get(graph)?;
    Some(graph.members.iter().map(|m| m.filter).collect())
}
fn out_of_sync(env: &Env<'_>, this: u32) -> bool {
    match (payload(env, this), filter_list(env, this)) {
        (Some(Obj::EnumFilters { list, .. }), Some(current)) => *list != current,
        _ => false,
    }
}

fn advance(env: &mut Env<'_>, this: u32, count: usize) -> usize {
    let length = match payload(env, this) {
        Some(Obj::EnumPins { list, .. }) => list.len(),
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
        Some(Obj::EnumPins { list, position, .. }) => {
            (*position, list.iter().map(|&p| Item::Pin(p)).collect())
        }
        Some(Obj::EnumTypes { types, position }) => {
            (*position, types.iter().cloned().map(Item::Type).collect())
        }
        Some(Obj::EnumFilters { list, position, .. }) => {
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
