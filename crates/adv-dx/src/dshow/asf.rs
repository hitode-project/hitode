//! WM ASF Reader: defer opening until graph join and expose compressed streams.
use adv_device::media::{Demuxed, PacketStream};
use adv_machine::{Handle, Object, Wait};

use super::*;

pub(super) const IID_ISERVICEPROVIDER: Iid = guid(
    0x6d51_40c1,
    0x7436,
    0x11ce,
    [0x80, 0x34, 0x00, 0xaa, 0x00, 0x60, 0x09, 0xfa],
);
pub(super) const IID_IWMHEADERINFO: Iid = guid(
    0x9640_6bda,
    0x2b2b,
    0x11d3,
    [0xb3, 0x6b, 0x00, 0xc0, 0x4f, 0x61, 0x08, 0xff],
);
pub(super) const IID_IWMREADERADVANCED: Iid = guid(
    0x9640_6bea,
    0x2b2b,
    0x11d3,
    [0xb3, 0x6b, 0x00, 0xc0, 0x4f, 0x61, 0x08, 0xff],
);
pub(super) const IID_IWMREADERADVANCED2: Iid = guid(
    0xae14_a945,
    0xb90c,
    0x4d0d,
    [0x91, 0x27, 0x80, 0xd6, 0x65, 0xf7, 0xd7, 0x3e],
);

pub(crate) struct Asf {
    streams: Vec<(u32, PacketStream, Vec<MediaType>)>,
    handle: Option<u32>,
    threads: u32,
    wake: Handle,
}

impl Asf {
    pub(super) fn new(env: &mut Env<'_>) -> Self {
        Self {
            streams: Vec::new(),
            handle: None,
            threads: 0,
            wake: env.sys.objects.insert(Object::Event {
                manual_reset: true,
                signaled: false,
            }),
        }
    }
}

fn asf<'a>(env: &'a mut Env<'_>, filter: u32) -> &'a mut Asf {
    state(env)
        .filters
        .get_mut(&filter)
        .unwrap()
        .asf
        .as_mut()
        .unwrap()
}

pub(super) fn file_method(env: &mut Env<'_>, filter: u32, method: &str) -> Outcome {
    match method {
        "Load" => {
            if env.arg(1) == 0 {
                return Outcome::Return(E_POINTER);
            }
            if !state(env).filters[&filter].source_path.is_empty() {
                return Outcome::Return(0x8000_4005);
            }
            let path = read_wide(env, env.arg(1), 32768);
            state(env).filters.get_mut(&filter).unwrap().source_path = path;
            Outcome::Return(S_OK)
        }
        "GetCurFile" => {
            let (output, mt) = (env.arg(1), env.arg(2));
            if output == 0 {
                return Outcome::Return(E_POINTER);
            }
            let path = state(env).filters[&filter].source_path.clone();
            let pointer = if path.is_empty() {
                0
            } else {
                let bytes: Vec<u8> = path
                    .iter()
                    .copied()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect();
                let pointer = adv_os::com::task_alloc(env, bytes.len() as u32);
                if pointer == 0 {
                    return Outcome::Return(E_OUTOFMEMORY);
                }
                let _ = env.cpu.write(pointer, &bytes);
                pointer
            };
            let _ = env.cpu.write_u32(output, pointer);
            if mt != 0 {
                let _ = write_guid(env, mt, &GUID_NULL);
                let _ = write_guid(env, mt + 16, &GUID_NULL);
                for at in [40, 60, 64] {
                    let _ = env.cpu.write_u32(mt + at, 0);
                }
            }
            Outcome::Return(S_OK)
        }
        _ => Outcome::Return(E_NOTIMPL),
    }
}

pub(super) fn join(env: &mut Env<'_>, filter: u32, graph: u32, then: dmo::Done) -> Outcome {
    if graph == 0 || !asf(env, filter).streams.is_empty() {
        return then(env, S_OK);
    }
    let path = state(env).filters[&filter].source_path.clone();
    let bytes: Vec<u8> = path
        .iter()
        .copied()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    let Some(filename) = alloc(env, bytes.len() as u32) else {
        return then(env, E_OUTOFMEMORY);
    };
    let _ = env.cpu.write(filename, &bytes);
    let Some(host) = env.ext.try_get::<Host>().copied() else {
        free(env, filename);
        return then(env, E_NOTIMPL);
    };
    (host.read)(
        env,
        filename,
        Box::new(move |env, result| {
            free(env, filename);
            let (bytes, handle) = match result {
                Ok(value) => value,
                Err(hr) => return then(env, hr),
            };
            let media = match adv_device::media::demux(bytes) {
                Ok(media) => media,
                Err(_) => {
                    if let Some(handle) = handle {
                        host::close(env, handle);
                    }
                    return then(env, VFW_E_UNSUPPORTED_STREAM);
                }
            };
            asf(env, filter).handle = handle;
            let hr = attach(env, filter, media).map_or_else(|hr| hr, |()| S_OK);
            then(env, hr)
        }),
    )
}

fn attach(env: &mut Env<'_>, filter: u32, media: Demuxed) -> Result<(), u32> {
    for (index, stream) in media.streams.into_iter().enumerate() {
        let video = matches!(stream.format, CodecFormat::Video { .. });
        let name = format!("Raw {} {index}", if video { "Video" } else { "Audio" });
        let types = media_types(&stream);
        let pin = filter::new_pin(env, filter, filter::OUTPUT, &name, types[0].major, None)?;
        asf(env, filter).streams.push((pin, stream, types));
    }
    Ok(())
}

fn media_types(stream: &PacketStream) -> Vec<MediaType> {
    let mut mt = MediaType::compressed(&stream.format, stream.bit_rate);
    if let CodecFormat::Video { width, height, .. } = stream.format {
        mt.format[40..48].fill(0);
        let mut info2 = mt.clone();
        info2.format_type = vmr::FORMAT_VIDEOINFO2;
        info2.format.splice(48..48, [0; 24]);
        info2.format[56..60].copy_from_slice(&width.to_le_bytes());
        info2.format[60..64].copy_from_slice(&height.to_le_bytes());
        vec![info2, mt]
    } else {
        vec![mt]
    }
}

pub(super) fn types(env: &Env<'_>, pin: u32) -> Vec<MediaType> {
    let s = env.ext.get::<State>();
    s.filters[&s.pins[&pin].filter]
        .asf
        .as_ref()
        .unwrap()
        .streams
        .iter()
        .find(|(p, _, _)| *p == pin)
        .map_or_else(Vec::new, |(_, _, types)| types.clone())
}

pub(super) fn transition(env: &mut Env<'_>, filter: u32, value: u32, then: dmo::Done) -> Outcome {
    if asf(env, filter).streams.is_empty() {
        return then(env, 0x8000_4005);
    }
    if value == graph::STOPPED {
        state(env).filters.get_mut(&filter).unwrap().state = value;
        let a = asf(env, filter);
        let (wake, threads) = (a.wake, a.threads);
        env.sys.set_event(wake);
        if threads != 0 {
            return Outcome::block(Wait::Until(env.sys.clock.now_us() + 1000), move |env, _| {
                transition(env, filter, value, then)
            });
        }
        filter::set_state(env, filter, value);
        return then(env, S_OK);
    }
    let previous = state(env).filters[&filter].state;
    filter::set_state(env, filter, value);
    if previous != graph::STOPPED {
        return then(env, S_OK);
    }
    let pins: Vec<u32> = asf(env, filter)
        .streams
        .iter()
        .map(|(p, _, _)| *p)
        .collect();
    let pins = pins
        .into_iter()
        .filter(|p| state(env).pins[p].mem != 0)
        .collect();
    can_block(env, filter, pins, Vec::new(), false, then)
}

fn can_block(
    env: &mut Env<'_>,
    filter: u32,
    mut pins: Vec<u32>,
    mut connected: Vec<u32>,
    blocking: bool,
    then: dmo::Done,
) -> Outcome {
    let Some(pin) = pins.pop() else {
        let groups = if blocking {
            connected.into_iter().map(|p| vec![p]).collect()
        } else {
            vec![connected]
        };
        return start_threads(env, filter, groups, then);
    };
    let mem = state(env).pins[&pin].mem;
    guest_call(env, mem, 8, vec![], move |env, hr| {
        connected.push(pin);
        can_block(env, filter, pins, connected, blocking || hr == S_OK, then)
    })
}

fn start_threads(
    env: &mut Env<'_>,
    filter: u32,
    mut groups: Vec<Vec<u32>>,
    then: dmo::Done,
) -> Outcome {
    let Some(pins) = groups.pop() else {
        return then(env, S_OK);
    };
    if pins.is_empty() {
        return then(env, S_OK);
    }
    let id = state(env).next_session;
    state(env).next_session += 1;
    state(env)
        .asf_threads
        .insert(id, (filter, pins.iter().map(|&p| (p, 0)).collect()));
    asf(env, filter).threads += 1;
    let thread = env.ext.get::<Host>().thread;
    let outcome = thread(env, id);
    after(
        env,
        outcome,
        Box::new(move |env, _| start_threads(env, filter, groups, then)),
    )
}

pub(super) fn thread(env: &mut Env<'_>, id: u32) -> Outcome {
    let Some((filter, cursors)) = state(env).asf_threads.get(&id).cloned() else {
        return Outcome::Return(0);
    };
    if state(env).filters[&filter].state == graph::STOPPED {
        state(env).asf_threads.remove(&id);
        let a = asf(env, filter);
        a.threads -= 1;
        let wake = a.wake;
        env.sys.set_event(wake);
        return Outcome::Return(0);
    }
    let next = cursors
        .iter()
        .enumerate()
        .filter_map(|(index, &(pin, cursor))| {
            let a = asf(env, filter);
            let stream = &a.streams.iter().find(|(p, _, _)| *p == pin)?.1;
            stream
                .packets
                .get(cursor)
                .map(|packet| (index, pin, packet.clone()))
        })
        .min_by_key(|(_, _, packet)| packet.time_100ns);
    let Some((index, pin, packet)) = next else {
        let wake = asf(env, filter).wake;
        env.sys.reset_event(wake);
        return Outcome::block(
            Wait::Object {
                handle: wake,
                deadline: None,
            },
            move |env, _| thread(env, id),
        );
    };
    let allocator = state(env).pins[&pin].allocator;
    let Some(sample) = sample::take(env, allocator) else {
        return Outcome::Return(0);
    };
    let s = state(env).samples.get_mut(&sample).unwrap();
    s.actual = packet.data.len() as u32;
    s.time = Some((
        packet.time_100ns as i64,
        (packet.time_100ns + packet.duration_100ns) as i64,
    ));
    s.sync_point = packet.key;
    let buffer = s.buffer;
    let _ = env.cpu.write(buffer, &packet.data);
    let Some(scratch) = alloc(env, 8) else {
        release(env, sample);
        return Outcome::Return(0);
    };
    let _ = env.cpu.write_u32(scratch, sample);
    let mem = state(env).pins[&pin].mem;
    guest_call(env, mem, 7, vec![scratch, 1, scratch + 4], move |env, _| {
        release(env, sample);
        free(env, scratch);
        state(env).asf_threads.get_mut(&id).unwrap().1[index].1 += 1;
        thread(env, id)
    })
}

pub(super) fn destroyed(env: &mut Env<'_>, asf: Asf) {
    if let Some(handle) = asf.handle {
        host::close(env, handle);
    }
    env.sys.objects.close(asf.wake);
}

pub(super) fn sample_size(env: &Env<'_>, pin: u32) -> u32 {
    let s = env.ext.get::<State>();
    s.filters[&s.pins[&pin].filter]
        .asf
        .as_ref()
        .unwrap()
        .streams
        .iter()
        .find(|(p, _, _)| *p == pin)
        .unwrap()
        .1
        .max_packet
        .max(1)
}

pub(super) fn interface_methods(interface: &str) -> Option<Vec<(&'static str, Conv, u16)>> {
    let methods: &[(&str, u8)] = match interface {
        "IServiceProvider" => &[("QueryService", 4)],
        "IWMReaderAdvanced2" => &[
            ("SetUserProvidedClock", 2),
            ("GetUserProvidedClock", 2),
            ("DeliverTime", 3),
            ("SetManualStreamSelection", 2),
            ("GetManualStreamSelection", 2),
            ("SetStreamsSelected", 4),
            ("GetStreamSelected", 3),
            ("SetReceiveSelectionCallbacks", 2),
            ("GetReceiveSelectionCallbacks", 2),
            ("SetReceiveStreamSamples", 3),
            ("GetReceiveStreamSamples", 3),
            ("SetAllocateForOutput", 3),
            ("GetAllocateForOutput", 3),
            ("SetAllocateForStream", 3),
            ("GetAllocateForStream", 3),
            ("GetStatistics", 2),
            ("SetClientInfo", 2),
            ("GetMaxOutputSampleSize", 3),
            ("GetMaxStreamSampleSize", 3),
            ("NotifyLateDelivery", 3),
            ("SetPlayMode", 2),
            ("GetPlayMode", 2),
            ("GetBufferProgress", 3),
            ("GetDownloadProgress", 4),
            ("GetSaveAsProgress", 2),
            ("SaveFileAs", 2),
            ("GetProtocolName", 3),
            ("StartAtMarker", 6),
            ("GetOutputSetting", 6),
            ("SetOutputSetting", 6),
            ("Preroll", 6),
            ("SetLogClientID", 2),
            ("GetLogClientID", 2),
            ("StopBuffering", 1),
            ("OpenStream", 4),
        ],
        _ => return None,
    };
    Some(
        [("QueryInterface", 3), ("AddRef", 1), ("Release", 1)]
            .iter()
            .chain(methods)
            .enumerate()
            .map(|(slot, &(name, argc))| (name, Conv::Stdcall { argc }, slot as u16))
            .collect(),
    )
}
