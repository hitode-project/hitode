//! Windows Media Format SDK synchronous readers. The common demuxer supplies
//! compressed samples from guest IStreams. Wine assembly also opens filenames
//! and decodes uncompressed outputs on demand, using the same sample objects.
use adv_abi::{GuestPtr, Registry, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_device::media::{self, CodecFormat, Demuxed};
use adv_machine::{Env, Outcome};
use adv_os::com::{Com, Iid, QueryError};
use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};

use crate::dshow::{
    self, E_INVALIDARG, E_NOTIMPL, E_OUTOFMEMORY, E_POINTER, E_UNEXPECTED, MEDIA_TYPE_SIZE,
    MediaType, S_OK, guest_call, guid, read_guid, read_wide,
};

const IID_IWMSYNCREADER: Iid = guid(
    0x9397_f121,
    0x7705,
    0x4dc9,
    [0xb0, 0x49, 0x98, 0xb6, 0x98, 0x18, 0x84, 0x14],
);
const IID_IWMSYNCREADER2: Iid = guid(
    0xfaed_3d21,
    0x1b6b,
    0x4af7,
    [0x8c, 0xb6, 0x3e, 0x18, 0x9b, 0xbc, 0x18, 0x7b],
);
const fn wm(d1: u32) -> Iid {
    guid(
        d1,
        0x2b2b,
        0x11d3,
        [0xb3, 0x6b, 0x00, 0xc0, 0x4f, 0x61, 0x08, 0xff],
    )
}
const IID_IWMPROFILE: Iid = wm(0x9640_6bdb);
const IID_IWMHEADERINFO: Iid = wm(0x9640_6bda);
const IID_IWMSTREAMCONFIG: Iid = wm(0x9640_6bdc);
const IID_IWMOUTPUTMEDIAPROPS: Iid = wm(0x9640_6bd7);
const IID_IWMMEDIAPROPS: Iid = wm(0x9640_6bce);
const IID_INSSBUFFER: Iid = guid(
    0xe1cd_3524,
    0x03d7,
    0x11d2,
    [0x9e, 0xed, 0x00, 0x60, 0x97, 0xd2, 0xd7, 0xcf],
);

const IID_IWMPROFILE2: Iid = guid(
    0x07e72d33,
    0xd94e,
    0x4be7,
    [0x88, 0x43, 0x60, 0xae, 0x5f, 0xf7, 0xe5, 0xf5],
);
const IID_IWMPROFILE3: Iid = guid(
    0x00ef96cc,
    0xa461,
    0x4546,
    [0x8b, 0xcd, 0xc9, 0xa2, 0x8f, 0x0e, 0x06, 0xf5],
);
const IID_IWMHEADERINFO2: Iid = guid(
    0x15cf9781,
    0x454e,
    0x482e,
    [0xb3, 0x93, 0x85, 0xfa, 0xe4, 0x87, 0xa8, 0x10],
);
const IID_IWMHEADERINFO3: Iid = guid(
    0x15cc68e3,
    0x27cc,
    0x4ecd,
    [0xb2, 0x22, 0x3f, 0x5d, 0x02, 0xd8, 0x0b, 0xd5],
);
const IID_IWMLANGUAGELIST: Iid = guid(
    0xdf683f00,
    0x2d49,
    0x4d8e,
    [0x92, 0xb7, 0xfb, 0x19, 0xf6, 0xa0, 0xdc, 0x57],
);
const IID_IWMPACKETSIZE: Iid = guid(
    0xcdfb97ab,
    0x188f,
    0x40b3,
    [0xb6, 0x43, 0x5b, 0x79, 0x03, 0x97, 0x5c, 0x59],
);
const IID_IWMPACKETSIZE2: Iid = guid(
    0x8bfc2b9e,
    0xb646,
    0x4233,
    [0xa8, 0x77, 0x1c, 0x6a, 0x07, 0x96, 0x69, 0xdc],
);
const IID_IWMREADERPLAYLISTBURN: Iid = guid(
    0xf28c0300,
    0x9baa,
    0x4477,
    [0xa8, 0x46, 0x17, 0x44, 0xd9, 0xcb, 0xf5, 0x33],
);
const IID_IWMREADERTIMECODE: Iid = guid(
    0xf369e2f0,
    0xe081,
    0x4fe6,
    [0x84, 0x50, 0xb8, 0x10, 0xb2, 0xf4, 0x10, 0xd1],
);

const NS_E_NO_MORE_SAMPLES: u32 = 0xc00d_0bcf;
const NS_E_INVALID_REQUEST: u32 = 0xc00d_002b;
const NS_E_INVALID_INPUT_FORMAT: u32 = 0xc00d_0bb8;
const NS_E_INVALID_OUTPUT_FORMAT: u32 = 0xc00d_0bbb;
const NS_E_NO_STREAM: u32 = 0xc00d_0033;
const NS_E_AUDIO_CODEC_NOT_INSTALLED: u32 = 0xc00d_0bc2;
const NS_E_INCOMPATIBLE_FORMAT: u32 = 0xc00d_0041;
const ASF_E_BADMEDIATYPE: u32 = 0xc00d_0807;
const ASF_E_NOTFOUND: u32 = 0xc00d_07f0;
const ASF_E_BUFFERTOOSMALL: u32 = 0xc00d_07d1;
const NS_E_INVALID_STREAM: u32 = 0xc00d_0b9d;

const WM_SF_CLEANPOINT: u32 = 1;
const WM_SF_DISCONTINUITY: u32 = 2;
const WMT_TYPE_DWORD: u32 = 0;
const WMT_TYPE_BOOL: u32 = 3;
const WMT_TYPE_QWORD: u32 = 4;

/// Largest container read from a guest IStream into host memory.
const MAX_SOURCE_BYTES: usize = 512 << 20;
const READ_CHUNK: u32 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Face {
    SyncReader,
    Profile,
    HeaderInfo,
    StreamConfig,
    MediaProps,
    Buffer,
    OutputProps,
    ReaderAux,
}

const FACES: [(Face, &str); 8] = [
    (Face::SyncReader, "IWMSyncReader2"),
    (Face::Profile, "IWMProfile"),
    (Face::HeaderInfo, "IWMHeaderInfo"),
    (Face::StreamConfig, "IWMStreamConfig"),
    (Face::MediaProps, "IWMMediaProps"),
    (Face::Buffer, "INSSBuffer"),
    (Face::OutputProps, "IWMOutputMediaProps"),
    (Face::ReaderAux, "IUnknown"),
];

impl Face {
    fn interface(self) -> &'static str {
        FACES[self as usize].1
    }
    fn signatures(self) -> &'static str {
        if self == Face::OutputProps {
            "IWMMediaProps"
        } else {
            self.interface()
        }
    }
    fn iids(self) -> &'static [Iid] {
        match self {
            Face::SyncReader => &[IID_IWMSYNCREADER, IID_IWMSYNCREADER2],
            Face::Profile => &[IID_IWMPROFILE, IID_IWMPROFILE2, IID_IWMPROFILE3],
            Face::HeaderInfo => &[IID_IWMHEADERINFO, IID_IWMHEADERINFO2, IID_IWMHEADERINFO3],
            Face::StreamConfig => &[IID_IWMSTREAMCONFIG],
            Face::MediaProps => &[IID_IWMMEDIAPROPS],
            Face::Buffer => &[IID_INSSBUFFER],
            Face::ReaderAux => &[
                IID_IWMLANGUAGELIST,
                IID_IWMPACKETSIZE,
                IID_IWMPACKETSIZE2,
                IID_IWMREADERPLAYLISTBURN,
                IID_IWMREADERTIMECODE,
            ],
            Face::OutputProps => &[IID_IWMOUTPUTMEDIAPROPS, IID_IWMMEDIAPROPS],
        }
    }
}

const DECODE_AHEAD: usize = 4;

struct DecodedStream {
    rx: mpsc::Receiver<Result<media::Packet, u32>>,
    head: Option<Result<media::Packet, u32>>,
    eof: bool,
    delivered: u64,
}

struct Stream {
    number: u16,
    selected: bool,
    compressed: bool,
    /// Next compressed packet to deliver and the active requested range.
    cursor: usize,
    start_100ns: u64,
    end_100ns: u64,
    discontinuity: bool,
    /// Guest IWMReaderAllocatorEx with one reference held, or 0.
    allocator: u32,
    output_allocator: u32,
    allocator_used: bool,
    output_type: MediaType,
    settings: BTreeMap<String, u32>,
    epoch: u64,
    decoded: Option<DecodedStream>,
}

#[derive(Default)]
struct Reader {
    /// The opened IStream with one reference held, or 0.
    source: u32,
    file: Option<u32>,
    media: Option<Arc<Demuxed>>,
    can_decode: bool,
    streams: Vec<Stream>,
    /// Eight guest bytes for output pointers of nested guest calls.
    scratch: u32,
}

impl Reader {
    /// Select every stream of `media`, positioned at its start.
    fn open(&mut self, media: Demuxed, can_decode: bool) {
        self.streams = media
            .streams
            .iter()
            .map(|s| Stream {
                number: s.number,
                selected: true,
                compressed: false,
                cursor: 0,
                start_100ns: 0,
                end_100ns: u64::MAX,
                discontinuity: true,
                allocator: 0,
                output_allocator: 0,
                allocator_used: false,
                output_type: output_type(&s.format),
                settings: BTreeMap::new(),
                epoch: 0,
                decoded: None,
            })
            .collect();
        self.media = Some(Arc::new(media));
        self.can_decode = can_decode;
    }
}

enum Obj {
    Reader(Box<Reader>),
    /// A snapshot of one stream of an opened reader.
    Config {
        number: u16,
        bit_rate: u32,
        media_type: MediaType,
    },
    Output(MediaType),
    Buffer {
        data: u32,
        length: u32,
        max: u32,
    },
}

struct State {
    com: Com,
    tables: BTreeMap<Face, u32>,
}

fn data(face: Face, slot: u16) -> u64 {
    ((face as u64) << 16) | u64::from(slot)
}

pub fn register(registry: &mut Registry) {
    registry.register(create_sync_reader_ABI);
    for (face, interface) in FACES {
        for signature in adv_abi::signature::com_signatures()
            .iter()
            .filter(|signature| signature.module == face.signatures())
        {
            let (Some(conv), Some(slot)) = (signature.conv, signature.com_slot) else {
                continue;
            };
            registry.add_with_data(
                "wmvcore.dll",
                &format!("{interface}::{}", signature.name),
                conv,
                dispatch,
                data(face, slot),
            );
        }
    }
}

fn initialize(env: &mut Env<'_>) -> Result<(), String> {
    if env.ext.try_get::<State>().is_some() {
        return Ok(());
    }
    let mut tables = BTreeMap::new();
    for (face, interface) in FACES {
        let methods: Vec<_> = adv_abi::signature::com_signatures()
            .iter()
            .filter(|signature| signature.module == face.signatures())
            .collect();
        let table =
            dshow::alloc(env, methods.len() as u32 * 4).ok_or("Windows Media vtable allocation")?;
        for signature in methods {
            let slot = u32::from(signature.com_slot.ok_or("COM slot")?);
            let name = format!("{interface}::{}", signature.name);
            let method = env
                .sys
                .thunks
                .publish(env.cpu, "wmvcore.dll", &name)
                .map_err(|e| e.to_string())?;
            env.cpu
                .write_u32(table + slot * 4, method)
                .map_err(|e| e.to_string())?;
        }
        tables.insert(face, table);
    }
    let com = Com::new(&mut env.sys.objects).ok_or("COM namespace exhausted")?;
    env.ext.insert(State { com, tables });
    Ok(())
}

fn com(env: &Env<'_>) -> Com {
    env.ext.get::<State>().com
}

/// Publish a host object with one vtable per face; the first is canonical.
fn create(env: &mut Env<'_>, faces: &[Face], value: Obj) -> Result<u32, u32> {
    initialize(env).map_err(|error| {
        log::warn!("Windows Media: {error}");
        E_OUTOFMEMORY
    })?;
    let block = dshow::alloc(env, faces.len() as u32 * 4).ok_or(E_OUTOFMEMORY)?;
    for (i, face) in faces.iter().enumerate() {
        let table = env.ext.get::<State>().tables[face];
        if env.cpu.write_u32(block + i as u32 * 4, table).is_err() {
            dshow::free(env, block);
            return Err(E_OUTOFMEMORY);
        }
    }
    let com = com(env);
    let Some(id) = com.create(&mut env.sys.objects, block, faces[0].iids()[0], value) else {
        dshow::free(env, block);
        return Err(E_OUTOFMEMORY);
    };
    for (i, face) in faces.iter().enumerate() {
        for &iid in face.iids() {
            if !com.register_interface(&mut env.sys.objects, id, iid, block + i as u32 * 4) {
                let _ = release(env, block);
                return Err(E_OUTOFMEMORY);
            }
        }
    }
    Ok(block)
}

fn payload<'a>(env: &'a mut Env<'_>, pointer: u32) -> Option<&'a mut Obj> {
    let com = env.ext.try_get::<State>()?.com;
    com.get_mut::<Obj>(&mut env.sys.objects, pointer)
}

fn reader<'a>(env: &'a mut Env<'_>, pointer: u32) -> Option<&'a mut Reader> {
    match payload(env, pointer)? {
        Obj::Reader(reader) => Some(reader),
        _ => None,
    }
}

/// Release one reference; a destroyed object hands back the guest references it
/// held, which the caller releases through their own vtables.
fn release(env: &mut Env<'_>, pointer: u32) -> (u32, Vec<u32>) {
    let com = com(env);
    let Some(released) = com.release::<Obj>(&mut env.sys.objects, pointer) else {
        return (0, Vec::new());
    };
    let mut held = Vec::new();
    if let Some(destroyed) = released.destroyed {
        match destroyed.value {
            Obj::Reader(reader) => {
                held = close(env, *reader);
            }
            Obj::Buffer { data, .. } => dshow::free(env, data),
            Obj::Config { .. } | Obj::Output(_) => {}
        }
        if let Some(&first) = destroyed.interfaces.iter().min() {
            dshow::free(env, first);
        }
    }
    (released.references, held)
}

/// Free a reader's host state; returns the guest objects it still referenced.
fn close(env: &mut Env<'_>, reader: Reader) -> Vec<u32> {
    if let Some(file) = reader.file {
        dshow::host::close(env, file);
    }
    if reader.scratch != 0 {
        dshow::free(env, reader.scratch);
    }
    let mut held: Vec<u32> = reader
        .streams
        .iter()
        .flat_map(|s| [s.allocator, s.output_allocator])
        .collect();
    held.push(reader.source);
    held.retain(|&p| p != 0);
    held
}

/// Release guest references in order, then return `value`.
fn release_guests(env: &mut Env<'_>, mut held: Vec<u32>, value: u32) -> Outcome {
    let Some(pointer) = held.pop() else {
        return Outcome::Return(value);
    };
    guest_call(env, pointer, 2, vec![], move |env, _| {
        release_guests(env, held, value)
    })
}

#[win32(wmvcore, name = "WMCreateSyncReader", level = implement)]
fn create_sync_reader(
    env: &mut Env<'_>,
    _certificate: GuestPtr<()>,
    _rights: u32,
    output: GuestPtr<u32>,
) -> Outcome {
    let output = output.addr();
    env.ext.get_or_default::<NativeSamples>().readers += 1;
    if output == 0 || !env.cpu.accessible(output, 4, Prot::WRITE) {
        return Outcome::Return(E_POINTER);
    }
    let _ = env.cpu.write_u32(output, 0);
    if !media::Decoder::available() {
        adv_abi::warn_unavailable(
            env,
            "wmvcore.dll!WMCreateSyncReader",
            "Windows Media decoding needs a build with the media feature",
        );
        return Outcome::Return(E_NOTIMPL);
    }
    let Some(scratch) = dshow::alloc(env, 8) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let reader = Reader {
        scratch,
        ..Reader::default()
    };
    match create(
        env,
        &[
            Face::SyncReader,
            Face::Profile,
            Face::HeaderInfo,
            Face::ReaderAux,
        ],
        Obj::Reader(Box::new(reader)),
    ) {
        Ok(pointer) => {
            let _ = env.cpu.write_u32(output, pointer);
            Outcome::Return(S_OK)
        }
        Err(error) => {
            dshow::free(env, scratch);
            Outcome::Return(error)
        }
    }
}

fn dispatch(env: &mut Env<'_>) -> Outcome {
    let data = env.sys.thunks.get(env.thunk).f.data;
    let face = FACES[(data >> 16) as usize].0;
    let slot = data as u16;
    let this = env.arg(0);
    let method = env.sys.thunks.get(env.thunk).name.clone();
    let method = method.rsplit("::").next().unwrap_or("").to_owned();
    if env.ext.try_get::<State>().is_none() {
        return Outcome::Return(E_UNEXPECTED);
    }
    if payload(env, this).is_none() {
        log::warn!(
            "Windows Media {}::{method}: unknown this {this:#x}",
            face.interface()
        );
        return Outcome::Return(E_POINTER);
    }
    match slot {
        0 => return Outcome::Return(query_interface(env, this, env.arg(1), env.arg(2))),
        1 => {
            let com = com(env);
            return Outcome::Return(com.add_ref::<Obj>(&mut env.sys.objects, this).unwrap_or(0));
        }
        2 => {
            let (references, held) = release(env, this);
            return release_guests(env, held, references);
        }
        _ => {}
    }
    let outcome = match face {
        Face::SyncReader => reader_method(env, this, &method),
        Face::Profile => profile_method(env, this, &method),
        Face::HeaderInfo => header_method(env, this, &method),
        Face::StreamConfig | Face::MediaProps | Face::OutputProps => {
            config_method(env, this, &method)
        }
        Face::ReaderAux => None,
        Face::Buffer => buffer_method(env, this, &method),
    };
    outcome.unwrap_or_else(|| {
        adv_abi::warn_unavailable(
            env,
            &format!("wmvcore.dll!{}::{method}", face.interface()),
            "this Windows Media method is not implemented",
        );
        Outcome::Return(E_NOTIMPL)
    })
}

fn query_interface(env: &mut Env<'_>, this: u32, iid: u32, output: u32) -> u32 {
    if output == 0 || iid == 0 {
        return E_POINTER;
    }
    let _ = env.cpu.write_u32(output, 0);
    let Ok(requested) = read_guid(env, iid) else {
        return E_POINTER;
    };
    let com = com(env);
    match com.query::<Obj>(&mut env.sys.objects, this, requested) {
        Ok(pointer) => {
            let _ = env.cpu.write_u32(output, pointer);
            S_OK
        }
        Err(QueryError::NoInterface) => dshow::E_NOINTERFACE,
        Err(QueryError::ReferenceLimit) => E_OUTOFMEMORY,
        Err(QueryError::InvalidObject) => E_POINTER,
    }
}

fn write_u16(env: &mut Env<'_>, pointer: u32, value: u16) -> bool {
    pointer != 0 && env.cpu.write(pointer, &value.to_le_bytes()).is_ok()
}

fn write_u32(env: &mut Env<'_>, pointer: u32, value: u32) -> bool {
    pointer != 0 && env.cpu.write_u32(pointer, value).is_ok()
}

fn write_u64(env: &mut Env<'_>, pointer: u32, value: u64) -> bool {
    pointer != 0 && env.cpu.write(pointer, &value.to_le_bytes()).is_ok()
}

fn arg_u64(env: &Env<'_>, index: u32) -> u64 {
    dshow::arg_i64(env, index) as u64
}

fn stream_index(env: &mut Env<'_>, this: u32, number: u16) -> Option<usize> {
    reader(env, this)?
        .streams
        .iter()
        .position(|s| s.number == number)
}

fn reader_method(env: &mut Env<'_>, this: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let (a1, a2, a3) = (env.arg(1), env.arg(2), env.arg(3));
    let opened = reader(env, this)?.media.is_some();
    match method {
        "Open" if env.ext.try_get::<dshow::Host>().is_some() => {
            if opened {
                return r(NS_E_INVALID_REQUEST);
            }
            let read = env.ext.get::<dshow::Host>().read;
            Some(read(
                env,
                a1,
                Box::new(move |env, result| match result {
                    Ok((bytes, file)) => {
                        let outcome = prepare_native(env, this, bytes);
                        dshow::after(
                            env,
                            outcome,
                            Box::new(move |env, result| {
                                if result[0] == S_OK {
                                    reader(env, this).unwrap().file = file;
                                } else if let Some(file) = file {
                                    dshow::host::close(env, file);
                                }
                                Outcome::Return(result[0])
                            }),
                        )
                    }
                    Err(hr) => Outcome::Return(hr),
                }),
            ))
        }
        "OpenStream" => {
            if a1 == 0 {
                return r(E_INVALIDARG);
            }
            if opened || reader(env, this)?.source != 0 {
                return r(NS_E_INVALID_REQUEST);
            }
            Some(open_stream(env, this, a1))
        }
        "Close" => {
            if !opened {
                return r(NS_E_INVALID_REQUEST);
            }
            let file = reader(env, this)?.file.take();
            if let Some(file) = file {
                dshow::host::close(env, file);
            }
            let reader = reader(env, this)?;
            let held: Vec<u32> = reader
                .streams
                .drain(..)
                .flat_map(|s| [s.allocator, s.output_allocator])
                .chain([std::mem::take(&mut reader.source)])
                .filter(|&p| p != 0)
                .collect();
            reader.media = None;
            reader.can_decode = false;
            Some(release_guests(env, held, S_OK))
        }
        "GetOutputSetting" | "SetOutputSetting" => {
            Some(output_setting(env, this, method == "SetOutputSetting"))
        }
        _ if !opened => r(NS_E_INVALID_REQUEST),
        "SetRange" => {
            let (start, duration) = (arg_u64(env, 1), dshow::arg_i64(env, 3));
            set_range(env, this, start, duration);
            r(S_OK)
        }
        "GetNextSample" => Some(next_sample(env, this)),
        "SetStreamsSelected" => {
            let (count, numbers, selections) = (a1 as u16, a2, a3);
            if count == 0 || numbers == 0 || selections == 0 {
                return r(E_INVALIDARG);
            }
            let mut chosen = Vec::new();
            for i in 0..u32::from(count) {
                let (Ok(number), Ok(selection)) = (
                    env.cpu.read_u16(numbers + i * 2),
                    env.cpu.read_u32(selections + i * 4),
                ) else {
                    return r(E_POINTER);
                };
                let Some(index) = stream_index(env, this, number) else {
                    return r(NS_E_INVALID_REQUEST);
                };
                chosen.push((index, selection != 0));
            }
            let reader = reader(env, this)?;
            for (index, selected) in chosen {
                reader.streams[index].selected = selected;
            }
            r(S_OK)
        }
        "GetStreamSelected" => {
            let Some(index) = stream_index(env, this, a1 as u16) else {
                return r(E_INVALIDARG);
            };
            let selected = reader(env, this)?.streams[index].selected;
            r(if write_u32(env, a2, if selected { 2 } else { 0 }) {
                S_OK
            } else {
                E_INVALIDARG
            })
        }
        "SetReadStreamSamples" => {
            let Some(index) = stream_index(env, this, a1 as u16) else {
                return r(E_INVALIDARG);
            };
            let stream = &mut reader(env, this)?.streams[index];
            let compressed = a2 != 0;
            if stream.compressed != compressed {
                stream.compressed = compressed;
                stream.epoch = stream.epoch.wrapping_add(1);
                if compressed {
                    stream.decoded = None;
                }
            }
            r(S_OK)
        }
        "GetReadStreamSamples" => {
            let Some(index) = stream_index(env, this, a1 as u16) else {
                return r(E_INVALIDARG);
            };
            let compressed = reader(env, this)?.streams[index].compressed;
            r(if write_u32(env, a2, u32::from(compressed)) {
                S_OK
            } else {
                E_INVALIDARG
            })
        }
        "GetOutputProps" | "GetOutputFormat" | "GetOutputFormatCount" | "SetOutputProps" => {
            let Some(index) = output_index(reader(env, this)?, a1) else {
                return r(E_INVALIDARG);
            };
            if method == "GetOutputFormatCount" {
                return r(if write_u32(env, a2, 1) {
                    S_OK
                } else {
                    E_INVALIDARG
                });
            }
            if method == "SetOutputProps" {
                let Some(Obj::Output(mt)) = payload(env, a2) else {
                    return r(E_POINTER);
                };
                let mt = mt.clone();
                let source = &reader(env, this)?.media.as_ref()?.streams[index].format;
                match source {
                    CodecFormat::Audio { channels, .. } => {
                        if mt.major != dshow::MEDIATYPE_AUDIO {
                            return r(ASF_E_BADMEDIATYPE);
                        }
                        if mt.format.len() >= 4
                            && u16::from_le_bytes(mt.format[2..4].try_into().unwrap()) > *channels
                        {
                            return r(NS_E_AUDIO_CODEC_NOT_INSTALLED);
                        }
                    }
                    CodecFormat::Video { width, height, .. } => {
                        if mt.major != dshow::MEDIATYPE_VIDEO {
                            return r(NS_E_INCOMPATIBLE_FORMAT);
                        }
                        if ![dshow::MEDIASUBTYPE_RGB24, dshow::MEDIASUBTYPE_RGB32].contains(&mt.sub)
                            || mt.format.len() < 60
                            || u32::from_le_bytes(mt.format[52..56].try_into().unwrap()) != *width
                            || u32::from_le_bytes(mt.format[56..60].try_into().unwrap()) != *height
                        {
                            return r(NS_E_INVALID_OUTPUT_FORMAT);
                        }
                    }
                }
                reader(env, this)?.streams[index].output_type = mt;
                return r(S_OK);
            }
            if method == "GetOutputFormat" && a2 != 0 {
                return r(NS_E_INVALID_OUTPUT_FORMAT);
            }
            let output = if method == "GetOutputProps" { a2 } else { a3 };
            let mt = if method == "GetOutputProps" {
                reader(env, this)?.streams[index].output_type.clone()
            } else {
                output_type(&reader(env, this)?.media.as_ref()?.streams[index].format)
            };
            r(match create(env, &[Face::OutputProps], Obj::Output(mt)) {
                Ok(pointer) => {
                    if write_u32(env, output, pointer) {
                        S_OK
                    } else {
                        E_POINTER
                    }
                }
                Err(hr) => hr,
            })
        }
        "GetOutputCount" => {
            let count = reader(env, this)?.streams.len() as u32;
            r(if write_u32(env, a1, count) {
                S_OK
            } else {
                E_POINTER
            })
        }
        "GetOutputNumberForStream" => {
            let Some(index) = stream_index(env, this, a1 as u16) else {
                return r(NS_E_INVALID_STREAM);
            };
            let output = output_number(reader(env, this)?, index);
            r(if write_u32(env, a2, output) {
                S_OK
            } else {
                E_POINTER
            })
        }
        "GetStreamNumberForOutput" => {
            let index = output_index(reader(env, this)?, a1).unwrap_or(usize::MAX);
            let Some(number) = reader(env, this)?.streams.get(index).map(|s| s.number) else {
                return r(E_INVALIDARG);
            };
            r(if write_u16(env, a2, number) {
                S_OK
            } else {
                E_POINTER
            })
        }
        "GetMaxOutputSampleSize" => {
            let Some(index) = output_index(reader(env, this)?, a1) else {
                return r(E_INVALIDARG);
            };
            let mt = &reader(env, this)?.streams[index].output_type;
            let size = if mt.major == dshow::MEDIATYPE_VIDEO {
                mt.sample_size
            } else {
                u32::from_le_bytes(mt.format[8..12].try_into().unwrap())
            };
            r(if write_u32(env, a2, size) {
                S_OK
            } else {
                E_INVALIDARG
            })
        }
        "GetMaxStreamSampleSize" => {
            let Some(index) = stream_index(env, this, a1 as u16) else {
                return r(NS_E_INVALID_STREAM);
            };
            let max = reader(env, this)?.media.as_ref()?.streams[index].max_packet;
            r(if write_u32(env, a2, max) {
                S_OK
            } else {
                E_POINTER
            })
        }
        "SetAllocateForStream" | "SetAllocateForOutput" => {
            let index = if method == "SetAllocateForStream" {
                stream_index(env, this, a1 as u16)
            } else {
                output_index(reader(env, this)?, a1)
            };
            let Some(index) = index else {
                return r(E_INVALIDARG);
            };
            let stream = &mut reader(env, this)?.streams[index];
            if stream.allocator_used {
                return r(NS_E_INVALID_REQUEST);
            }
            let allocator = if method == "SetAllocateForStream" {
                &mut stream.allocator
            } else {
                &mut stream.output_allocator
            };
            let old = std::mem::replace(allocator, a2);
            let mut calls = Vec::new();
            if a2 != 0 {
                calls.push((a2, 1));
            }
            if old != 0 {
                calls.push((old, 2));
            }
            Some(guest_calls(env, calls, S_OK))
        }
        "GetAllocateForStream" | "GetAllocateForOutput" => {
            let index = if method == "GetAllocateForStream" {
                stream_index(env, this, a1 as u16)
            } else {
                output_index(reader(env, this)?, a1)
            };
            let Some(index) = index else {
                return r(E_INVALIDARG);
            };
            let stream = &reader(env, this)?.streams[index];
            let allocator = if method == "GetAllocateForStream" {
                stream.allocator
            } else {
                stream.output_allocator
            };
            if !write_u32(env, a2, allocator) {
                return r(E_INVALIDARG);
            }
            Some(guest_calls(
                env,
                (allocator != 0)
                    .then_some((allocator, 1))
                    .into_iter()
                    .collect(),
                S_OK,
            ))
        }
        _ => None,
    }
}

fn output_setting(env: &mut Env<'_>, this: u32, set: bool) -> Outcome {
    let [output, name, kind, value, size] = std::array::from_fn(|i| env.arg(i as u32 + 1));
    let Some(index) = reader(env, this).and_then(|r| output_index(r, output)) else {
        return Outcome::Return(if reader(env, this).unwrap().media.is_none() {
            E_UNEXPECTED
        } else {
            E_INVALIDARG
        });
    };
    let name = String::from_utf16_lossy(&read_wide(env, name, 256));
    let audio =
        reader(env, this).unwrap().streams[index].output_type.major == dshow::MEDIATYPE_AUDIO;
    let (expected_kind, default) = match name.as_str() {
        "DedicatedDeliveryThread" | "JustInTimeDecode" | "SingleOutputBuffer" => {
            return Outcome::Return(NS_E_INVALID_REQUEST);
        }
        "StreamLanguage" if !set => return Outcome::Return(NS_E_INVALID_REQUEST),
        "NeedsPreviousSample" if !audio && !set => return Outcome::Return(NS_E_INVALID_REQUEST),
        "DeliverOnReceive" => (WMT_TYPE_BOOL, 0),
        "EnableDiscreteOutput" if audio => (WMT_TYPE_BOOL, 0),
        "EnableWMAProSPDIFOutput" if audio && set => (WMT_TYPE_BOOL, 0),
        "AllowInterlacedOutput" | "EnableFrameInterpolation" | "VideoSampleDurations" if !audio => {
            (WMT_TYPE_BOOL, 0)
        }
        "SoftwareScaling" if !audio => (WMT_TYPE_BOOL, 1),
        "DynamicRangeControl" | "SpeakerConfig" if audio => (WMT_TYPE_DWORD, u32::MAX),
        "EarlyDataDelivery" => (WMT_TYPE_DWORD, 0),
        "StreamLanguage" => (5, 0),
        _ => return Outcome::Return(E_INVALIDARG),
    };
    let length = if expected_kind == 5 { 2 } else { 4 };
    if set {
        if kind != expected_kind || size != length {
            return Outcome::Return(E_INVALIDARG);
        }
        let data = if length == 2 {
            env.cpu.read_u16(value).map(u32::from)
        } else {
            env.cpu.read_u32(value)
        };
        let Ok(data) = data else {
            return Outcome::Return(E_INVALIDARG);
        };
        reader(env, this).unwrap().streams[index]
            .settings
            .insert(name, data);
    } else {
        let capacity = env.cpu.read_u16(size).unwrap_or(0);
        write_u16(env, size, length as u16);
        if u32::from(capacity) < length {
            return Outcome::Return(ASF_E_BUFFERTOOSMALL);
        }
        let data = reader(env, this).unwrap().streams[index]
            .settings
            .get(&name)
            .copied()
            .unwrap_or(default);
        write_u32(env, kind, expected_kind);
        if length == 2 {
            write_u16(env, value, data as u16);
        } else {
            write_u32(env, value, data);
        }
    }
    Outcome::Return(S_OK)
}

/// Call slot `.1` (AddRef or Release) of each pointer in order, then return `value`.
fn guest_calls(env: &mut Env<'_>, mut calls: Vec<(u32, u32)>, value: u32) -> Outcome {
    if calls.is_empty() {
        return Outcome::Return(value);
    }
    let (pointer, slot) = calls.remove(0);
    guest_call(env, pointer, slot, vec![], move |env, _| {
        guest_calls(env, calls, value)
    })
}

/// Read the whole IStream, from its start, into host memory and demux it.
fn open_stream(env: &mut Env<'_>, this: u32, stream: u32) -> Outcome {
    let Some(buffer) = dshow::alloc(env, READ_CHUNK + 4) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    // IStream::Seek(0, STREAM_SEEK_SET, NULL)
    guest_call(env, stream, 5, vec![0, 0, 0, 0], move |env, hr| {
        if hr & 0x8000_0000 != 0 {
            dshow::free(env, buffer);
            return Outcome::Return(hr);
        }
        read_chunks(env, this, stream, buffer, Vec::new())
    })
}

fn read_chunks(
    env: &mut Env<'_>,
    this: u32,
    stream: u32,
    buffer: u32,
    mut bytes: Vec<u8>,
) -> Outcome {
    let count = buffer + READ_CHUNK;
    let _ = env.cpu.write_u32(count, 0);
    guest_call(
        env,
        stream,
        3,
        vec![buffer, READ_CHUNK, count],
        move |env, hr| {
            let read = env.cpu.read_u32(count).unwrap_or(0).min(READ_CHUNK);
            if hr & 0x8000_0000 != 0 {
                dshow::free(env, buffer);
                return Outcome::Return(hr);
            }
            let start = bytes.len();
            bytes.resize(start + read as usize, 0);
            if env.cpu.read(buffer, &mut bytes[start..]).is_err() {
                dshow::free(env, buffer);
                return Outcome::Return(E_POINTER);
            }
            if bytes.len() > MAX_SOURCE_BYTES {
                dshow::free(env, buffer);
                adv_abi::warn_unavailable(
                    env,
                    "IWMSyncReader::OpenStream",
                    "Windows Media streams over 512 MiB are not supported",
                );
                return Outcome::Return(E_OUTOFMEMORY);
            }
            if read == READ_CHUNK && hr == S_OK {
                return read_chunks(env, this, stream, buffer, bytes);
            }
            dshow::free(env, buffer);
            finish_open(env, this, stream, bytes)
        },
    )
}

fn finish_open(env: &mut Env<'_>, this: u32, stream: u32, bytes: Vec<u8>) -> Outcome {
    if env.ext.try_get::<dshow::Host>().is_some() {
        let outcome = prepare_native(env, this, bytes);
        return dshow::after(
            env,
            outcome,
            Box::new(move |env, result| {
                if result[0] != S_OK {
                    return Outcome::Return(result[0]);
                }
                reader(env, this).unwrap().source = stream;
                guest_call(env, stream, 1, vec![], |_, _| Outcome::Return(S_OK))
            }),
        );
    }
    let size = bytes.len();
    let demuxed = match demux(bytes) {
        Ok(demuxed) => demuxed,
        Err(error) => {
            log::warn!("IWMSyncReader::OpenStream: {size} bytes: {error}");
            return Outcome::Return(NS_E_INVALID_INPUT_FORMAT);
        }
    };
    log::info!(
        "IWMSyncReader::OpenStream: {} with {} streams, {} ms",
        demuxed.container,
        demuxed.streams.len(),
        demuxed.duration_100ns / 10_000
    );
    let Some(reader) = reader(env, this) else {
        return Outcome::Return(E_UNEXPECTED);
    };
    reader.open(demuxed, false);
    reader.source = stream;
    guest_call(env, stream, 1, vec![], |_, _| Outcome::Return(S_OK))
}

fn clean_point(packets: &[media::Packet], start: u64) -> usize {
    let first = packets.partition_point(|p| p.time_100ns < start);
    let mut cursor = packets[..first.min(packets.len())]
        .iter()
        .rposition(|p| p.key)
        .unwrap_or(first.min(packets.len()));
    if first < packets.len() && packets[first].time_100ns == start && packets[first].key {
        cursor = first;
    }
    cursor
}

/// Position every stream at the last clean point at or before `start`.
fn set_range(env: &mut Env<'_>, this: u32, start: u64, duration: i64) {
    let Some(reader) = reader(env, this) else {
        return;
    };
    let Some(media) = reader.media.as_ref() else {
        return;
    };
    let end = if duration > 0 {
        start.saturating_add(duration as u64)
    } else {
        u64::MAX
    };
    for (index, stream) in reader.streams.iter_mut().enumerate() {
        stream.cursor = clean_point(&media.streams[index].packets, start);
        stream.start_100ns = start;
        stream.end_100ns = end;
        stream.discontinuity = true;
        stream.epoch = stream.epoch.wrapping_add(1);
        stream.decoded = None;
    }
}

/// Native reader activity retained after COM objects have been released.
#[derive(Default)]
pub struct NativeSamples {
    pub readers: u64,
    pub samples: u64,
}

fn demux(bytes: Vec<u8>) -> Result<Demuxed, media::Error> {
    let file_properties = guid(
        0x8cab_dca1,
        0xa947,
        0x11cf,
        [0x8e, 0xe4, 0, 0xc0, 0x0c, 0x20, 0x53, 0x65],
    );
    let mut duration = None;
    if bytes.starts_with(&guid(
        0x75b2_2630,
        0x668e,
        0x11cf,
        [0xa6, 0xd9, 0, 0xaa, 0, 0x62, 0xce, 0x6c],
    )) {
        let mut offset = 30;
        while let Some(header) = bytes.get(offset..offset + 24) {
            let size = u64::from_le_bytes(header[16..24].try_into().unwrap()) as usize;
            if size < 24 {
                break;
            }
            let Some(object) = bytes.get(offset..).and_then(|tail| tail.get(..size)) else {
                break;
            };
            if object[..16] == file_properties && object.len() >= 88 {
                let play = u64::from_le_bytes(object[64..72].try_into().unwrap());
                let preroll = u64::from_le_bytes(object[80..88].try_into().unwrap());
                duration = Some(play.saturating_sub(preroll.saturating_mul(10_000)));
                break;
            }
            offset += size;
        }
    }
    let mut media = media::demux(bytes)?;
    if let Some(duration) = duration {
        media.duration_100ns = duration;
    }
    Ok(media)
}

fn prepare_native(env: &mut Env<'_>, this: u32, bytes: Vec<u8>) -> Outcome {
    let size = bytes.len();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let result = demux(bytes).map_err(|error| {
            log::warn!("IWMSyncReader::Open: {size} bytes: {error}");
            NS_E_INVALID_INPUT_FORMAT
        });
        let _ = send.send(result);
    });
    prepared_native(env, this, receive)
}

fn spawn_decoded(media: Arc<Demuxed>, index: usize, start_100ns: u64) -> DecodedStream {
    let (tx, rx) = mpsc::sync_channel(DECODE_AHEAD);
    std::thread::spawn(move || {
        let input = &media.streams[index];
        let mut decoder = match media::PacketDecoder::open(&input.format) {
            Ok(decoder) => decoder,
            Err(error) => {
                log::warn!("IWMSyncReader::GetNextSample: {error}");
                let _ = tx.send(Err(NS_E_INVALID_INPUT_FORMAT));
                return;
            }
        };
        let start_index = clean_point(&input.packets, start_100ns);
        for packet in &input.packets[start_index..] {
            let frames = match decoder.decode(&packet.data, Some(packet.time_100ns)) {
                Ok(frames) => frames,
                Err(error) => {
                    log::warn!("IWMSyncReader::GetNextSample: {error}");
                    let _ = tx.send(Err(NS_E_INVALID_INPUT_FORMAT));
                    return;
                }
            };
            for frame in frames {
                let item = decoded_packet(frame, &input.format);
                if item.time_100ns >= start_100ns && tx.send(Ok(item)).is_err() {
                    return;
                }
            }
        }
        match decoder.drain() {
            Ok(frames) => {
                for frame in frames {
                    let item = decoded_packet(frame, &input.format);
                    if item.time_100ns >= start_100ns && tx.send(Ok(item)).is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                log::warn!("IWMSyncReader::GetNextSample: {error}");
                let _ = tx.send(Err(NS_E_INVALID_INPUT_FORMAT));
            }
        }
    });
    DecodedStream {
        rx,
        head: None,
        eof: false,
        delivered: 0,
    }
}

fn decoded_packet(frame: media::Decoded, format: &CodecFormat) -> media::Packet {
    match frame {
        media::Decoded::Video(frame) => media::Packet {
            time_100ns: frame.pts_us * 10,
            duration_100ns: match format {
                CodecFormat::Video { frame_100ns, .. } => *frame_100ns,
                _ => 0,
            },
            key: true,
            data: frame.into_bgra().into(),
        },
        media::Decoded::Audio(frame) => media::Packet {
            time_100ns: frame.pts_us * 10,
            duration_100ns: frame.samples.len() as u64 * 10_000_000
                / (u64::from(frame.channels) * u64::from(frame.sample_rate)),
            key: true,
            data: frame
                .samples
                .into_iter()
                .flat_map(|sample| {
                    ((sample * 32768.0).clamp(-32768.0, 32767.0) as i16).to_le_bytes()
                })
                .collect(),
        },
    }
}

type Prepared = mpsc::Receiver<Result<Demuxed, u32>>;
fn prepared_native(env: &mut Env<'_>, this: u32, receive: Prepared) -> Outcome {
    match receive.try_recv() {
        Ok(Ok(demuxed)) => {
            log::info!(
                "IWMSyncReader::Open: {} with {} streams, {} ms",
                demuxed.container,
                demuxed.streams.len(),
                demuxed.duration_100ns / 10_000
            );
            let Some(reader) = reader(env, this) else {
                return Outcome::Return(E_UNEXPECTED);
            };
            reader.open(demuxed, true);
            Outcome::Return(S_OK)
        }
        Ok(Err(hr)) => Outcome::Return(hr),
        Err(mpsc::TryRecvError::Empty) => Outcome::block(
            adv_machine::Wait::Until(env.sys.clock.now_us() + 1000),
            move |env, _| prepared_native(env, this, receive),
        ),
        Err(_) => Outcome::Return(E_UNEXPECTED),
    }
}

fn next_sample(env: &mut Env<'_>, this: u32) -> Outcome {
    let number = env.arg(1) as u16;
    let outputs: [u32; 6] = std::array::from_fn(|i| env.arg(2 + i as u32));
    let [sample_out, time_out, duration_out, flags_out, _, _] = outputs;
    if number == 0 && outputs[4] == 0 && outputs[5] == 0 {
        return Outcome::Return(E_INVALIDARG);
    }
    if sample_out == 0 || time_out == 0 || duration_out == 0 || flags_out == 0 {
        return Outcome::Return(E_INVALIDARG);
    }
    let _ = env.cpu.write_u32(sample_out, 0);
    if number != 0 && outputs[5] != 0 {
        write_u16(env, outputs[5], number);
    }
    poll_next_sample(env, this, number, outputs)
}

fn poll_next_sample(env: &mut Env<'_>, this: u32, number: u16, outputs: [u32; 6]) -> Outcome {
    let [
        sample_out,
        time_out,
        duration_out,
        flags_out,
        output_out,
        number_out,
    ] = outputs;
    let Some(state) = reader(env, this) else {
        return Outcome::Return(E_UNEXPECTED);
    };
    let Reader {
        media: Some(media),
        can_decode,
        streams,
        scratch,
        ..
    } = state
    else {
        return Outcome::Return(NS_E_INVALID_REQUEST);
    };
    let media = Arc::clone(media);
    let can_decode = *can_decode;
    let scratch = *scratch;
    let mut any = false;
    let mut waiting = false;
    for (index, stream) in streams.iter_mut().enumerate() {
        if !stream.selected || (number != 0 && stream.number != number) {
            continue;
        }
        any = true;
        if !stream.compressed && can_decode {
            if stream.decoded.is_none() {
                stream.decoded = Some(spawn_decoded(Arc::clone(&media), index, stream.start_100ns));
            }
            let decoded = stream.decoded.as_mut().unwrap();
            if decoded.head.is_none() && !decoded.eof {
                match decoded.rx.try_recv() {
                    Ok(item) => decoded.head = Some(item),
                    Err(mpsc::TryRecvError::Disconnected) => decoded.eof = true,
                    Err(mpsc::TryRecvError::Empty) => waiting = true,
                }
            }
        }
    }
    if !any {
        return Outcome::Return(if number == 0 {
            NS_E_NO_MORE_SAMPLES
        } else {
            NS_E_INVALID_REQUEST
        });
    }
    if waiting {
        return Outcome::block(
            adv_machine::Wait::Until(env.sys.clock.now_us() + 500),
            move |env, _| poll_next_sample(env, this, number, outputs),
        );
    }
    let mut chosen = None;
    for (index, stream) in streams.iter().enumerate() {
        if !stream.selected || (number != 0 && stream.number != number) {
            continue;
        }
        let packet = if stream.compressed || !can_decode {
            media.streams[index].packets.get(stream.cursor)
        } else {
            match stream.decoded.as_ref().unwrap().head.as_ref() {
                Some(Ok(packet)) => Some(packet),
                Some(Err(hr)) => return Outcome::Return(*hr),
                None => None,
            }
        };
        let Some(packet) = packet else {
            continue;
        };
        if packet.time_100ns >= stream.end_100ns {
            continue;
        }
        if chosen.is_none_or(|(_, time)| packet.time_100ns < time) {
            chosen = Some((index, packet.time_100ns));
        }
    }
    let Some((index, _)) = chosen else {
        return Outcome::Return(NS_E_NO_MORE_SAMPLES);
    };
    let stream = &streams[index];
    if !stream.compressed && !can_decode {
        adv_abi::warn_unavailable(
            env,
            "IWMSyncReader::GetNextSample",
            "decompressed Windows Media outputs are not supported; read stream samples",
        );
        return Outcome::Return(E_NOTIMPL);
    }
    let (compressed, cursor, epoch, delivered, allocator, discontinuity, stream_number) = (
        stream.compressed,
        stream.cursor,
        stream.epoch,
        stream.decoded.as_ref().map_or(0, |d| d.delivered),
        if stream.compressed {
            stream.allocator
        } else {
            stream.output_allocator
        },
        stream.discontinuity,
        stream.number,
    );
    let packet = if compressed {
        &media.streams[index].packets[cursor]
    } else {
        stream
            .decoded
            .as_ref()
            .unwrap()
            .head
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
    };
    let (time, duration, key, data) = (
        packet.time_100ns,
        if compressed
            && media.container == "asf"
            && matches!(media.streams[index].format, CodecFormat::Video { .. })
        {
            10_000
        } else {
            packet.duration_100ns
        },
        packet.key,
        if !compressed && let CodecFormat::Video { width, height, .. } = media.streams[index].format
        {
            dshow::convert_bgra(width, height, &packet.data, &stream.output_type).into()
        } else {
            packet.data.clone()
        },
    );
    let flags = if key { WM_SF_CLEANPOINT } else { 0 }
        | if discontinuity {
            WM_SF_DISCONTINUITY
        } else {
            0
        };
    let finish = move |env: &mut Env<'_>, buffer: u32| {
        // Another thread may have moved this stream meanwhile; only advance it from
        // the sample just delivered.
        if let Some(stream) = reader(env, this).and_then(|r| r.streams.get_mut(index))
            && stream.epoch == epoch
        {
            if compressed {
                if stream.cursor == cursor {
                    stream.cursor += 1;
                    stream.discontinuity = false;
                }
            } else if let Some(decoded) = stream.decoded.as_mut()
                && decoded.delivered == delivered
            {
                decoded.head = None;
                decoded.delivered += 1;
                stream.discontinuity = false;
            }
        }
        env.ext.get_or_default::<NativeSamples>().samples += 1;
        let _ = env.cpu.write_u32(sample_out, buffer);
        write_u64(env, time_out, time);
        write_u64(env, duration_out, duration);
        write_u32(env, flags_out, flags);
        if output_out != 0 {
            let output = output_number(reader(env, this).unwrap(), index);
            write_u32(env, output_out, output);
        }
        if number_out != 0 {
            write_u16(env, number_out, stream_number);
        }
        Outcome::Return(S_OK)
    };
    let length = data.len() as u32;
    if allocator == 0 {
        let Some(memory) = dshow::alloc(env, length.max(1)) else {
            return Outcome::Return(E_OUTOFMEMORY);
        };
        if env.cpu.write(memory, &data).is_err() {
            dshow::free(env, memory);
            return Outcome::Return(E_OUTOFMEMORY);
        }
        return match create(
            env,
            &[Face::Buffer],
            Obj::Buffer {
                data: memory,
                length,
                max: length,
            },
        ) {
            Ok(buffer) => finish(env, buffer),
            Err(error) => {
                dshow::free(env, memory);
                Outcome::Return(error)
            }
        };
    }
    let output = output_number(reader(env, this).unwrap(), index);
    let stream = &mut reader(env, this).unwrap().streams[index];
    stream.allocator_used = true;
    let _ = env.cpu.write_u32(scratch, 0);
    let args = vec![
        if compressed {
            u32::from(stream_number)
        } else {
            output
        },
        length,
        scratch,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    // IWMReaderAllocatorEx::AllocateForStreamEx, then INSSBuffer::GetBuffer and SetLength.
    guest_call(
        env,
        allocator,
        if compressed { 3 } else { 4 },
        args,
        move |env, hr| {
            let buffer = env.cpu.read_u32(scratch).unwrap_or(0);
            if hr & 0x8000_0000 != 0 || buffer == 0 {
                return Outcome::Return(if hr & 0x8000_0000 != 0 {
                    NS_E_NO_MORE_SAMPLES
                } else {
                    E_OUTOFMEMORY
                });
            }
            guest_call(env, buffer, 5, vec![scratch], move |env, hr| {
                let max = env.cpu.read_u32(scratch).unwrap_or(0);
                if hr & 0x8000_0000 != 0 || max < length {
                    log::warn!(
                        "IWMSyncReader: allocator buffer of {max} bytes for a {length}-byte sample"
                    );
                    return guest_call(env, buffer, 2, vec![], move |_, _| {
                        Outcome::Return(if hr & 0x8000_0000 != 0 {
                            hr
                        } else {
                            E_OUTOFMEMORY
                        })
                    });
                }
                guest_call(env, buffer, 6, vec![scratch], move |env, hr| {
                    let pointer = env.cpu.read_u32(scratch).unwrap_or(0);
                    if hr & 0x8000_0000 != 0
                        || pointer == 0
                        || env.cpu.write(pointer, &data).is_err()
                    {
                        return guest_call(env, buffer, 2, vec![], move |_, _| {
                            Outcome::Return(E_POINTER)
                        });
                    }
                    guest_call(env, buffer, 4, vec![length], move |env, hr| {
                        if hr & 0x8000_0000 != 0 {
                            return guest_call(env, buffer, 2, vec![], move |_, _| {
                                Outcome::Return(hr)
                            });
                        }
                        finish(env, buffer)
                    })
                })
            })
        },
    )
}

fn output_type(format: &CodecFormat) -> MediaType {
    match *format {
        CodecFormat::Audio { rate, channels, .. } => MediaType::audio_pcm(rate, channels),
        CodecFormat::Video {
            width,
            height,
            frame_100ns,
            ..
        } => {
            let mut mt =
                MediaType::video_rgb(dshow::MEDIASUBTYPE_RGB24, width, height, frame_100ns as i64);
            for (at, value) in [(8, width), (12, height), (24, width), (28, height), (32, 0)] {
                mt.format[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            mt
        }
    }
}

fn output_index(reader: &Reader, output: u32) -> Option<usize> {
    let media = reader.media.as_ref()?;
    media
        .streams
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s.format, CodecFormat::Audio { .. }))
        .chain(
            media
                .streams
                .iter()
                .enumerate()
                .filter(|(_, s)| matches!(s.format, CodecFormat::Video { .. })),
        )
        .nth(output as usize)
        .map(|(i, _)| i)
}

fn output_number(reader: &Reader, index: usize) -> u32 {
    (0..reader.streams.len() as u32)
        .find(|&n| output_index(reader, n) == Some(index))
        .unwrap()
}

fn config(env: &mut Env<'_>, this: u32, index: usize) -> Result<u32, u32> {
    let Some(media) = reader(env, this).and_then(|r| r.media.as_ref()) else {
        return Err(NS_E_INVALID_REQUEST);
    };
    let stream = media.streams.get(index).ok_or(E_INVALIDARG)?;
    let value = Obj::Config {
        number: stream.number,
        bit_rate: stream.bit_rate,
        media_type: {
            let mut mt = MediaType::compressed(&stream.format, stream.bit_rate);
            if mt.major == dshow::MEDIATYPE_VIDEO {
                mt.format[40..48].fill(0);
                mt.format[68..72].fill(0);
            }
            mt
        },
    };
    create(env, &[Face::StreamConfig, Face::MediaProps], value)
}

fn profile_method(env: &mut Env<'_>, this: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let (a1, a2) = (env.arg(1), env.arg(2));
    let count = reader(env, this)?.media.as_ref().map(|m| m.streams.len());
    match method {
        "GetStreamCount" => {
            let Some(count) = count else {
                return r(NS_E_INVALID_REQUEST);
            };
            r(if write_u32(env, a1, count as u32) {
                S_OK
            } else {
                E_INVALIDARG
            })
        }
        "GetStream" | "GetStreamByNumber" => {
            if a2 == 0 || !env.cpu.accessible(a2, 4, Prot::WRITE) {
                return r(E_POINTER);
            }
            let _ = env.cpu.write_u32(a2, 0);
            let index = if method == "GetStream" {
                Some(a1 as usize)
            } else {
                stream_index(env, this, a1 as u16)
            };
            let Some(index) = index else {
                return r(NS_E_NO_STREAM);
            };
            match config(env, this, index) {
                Ok(pointer) => {
                    let _ = env.cpu.write_u32(a2, pointer);
                    r(S_OK)
                }
                Err(error) => r(error),
            }
        }
        _ => None,
    }
}

enum Attribute {
    Dword(u32),
    Qword(u64),
    Bool(bool),
}

fn attribute(media: &Demuxed, name: &str) -> Option<Attribute> {
    let video = media.streams.iter().find_map(|s| match s.format {
        CodecFormat::Video { width, height, .. } => Some((width, height, s.packets.len())),
        CodecFormat::Audio { .. } => None,
    });
    match name {
        "Duration" => Some(Attribute::Qword(media.duration_100ns)),
        "Seekable" => Some(Attribute::Bool(true)),
        "WM/VideoWidth" => video.map(|v| Attribute::Dword(v.0)),
        "WM/VideoHeight" => video.map(|v| Attribute::Dword(v.1)),

        "NumberOfFrames" => video.map(|v| Attribute::Qword(v.2 as u64)),
        _ => None,
    }
}

fn header_method(env: &mut Env<'_>, this: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    match method {
        "GetAttributeByName" => {
            let [stream, name, kind, value, length] =
                std::array::from_fn(|i| env.arg(1 + i as u32));
            if stream == 0 || name == 0 || kind == 0 || length == 0 {
                return r(E_INVALIDARG);
            }
            let name = String::from_utf16_lossy(&read_wide(env, name, 256));
            let Ok(stream) = env.cpu.read_u16(stream) else {
                return r(E_POINTER);
            };
            let found = reader(env, this)?
                .media
                .as_ref()
                .filter(|_| stream == 0)
                .and_then(|media| attribute(media, &name));
            let Some(found) = found else {
                log::debug!("IWMHeaderInfo::GetAttributeByName({name:?}): not present");
                return r(ASF_E_NOTFOUND);
            };
            let (type_id, bytes) = match found {
                Attribute::Dword(v) => (WMT_TYPE_DWORD, v.to_le_bytes().to_vec()),
                Attribute::Qword(v) => (WMT_TYPE_QWORD, v.to_le_bytes().to_vec()),
                Attribute::Bool(v) => (WMT_TYPE_BOOL, u32::from(v).to_le_bytes().to_vec()),
            };
            let Ok(capacity) = env.cpu.read_u16(length) else {
                return r(E_POINTER);
            };
            if !write_u16(env, length, bytes.len() as u16) {
                return r(E_POINTER);
            }
            if value != 0 && usize::from(capacity) < bytes.len() {
                return r(ASF_E_BUFFERTOOSMALL);
            }
            write_u32(env, kind, type_id);
            if value == 0 {
                return r(S_OK);
            }
            r(if env.cpu.write(value, &bytes).is_ok() {
                S_OK
            } else {
                E_POINTER
            })
        }
        _ => None,
    }
}

fn config_method(env: &mut Env<'_>, this: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let (a1, a2) = (env.arg(1), env.arg(2));
    let (number, bit_rate, media_type) = match payload(env, this)? {
        Obj::Config {
            number,
            bit_rate,
            media_type,
        } => (*number, *bit_rate, media_type.clone()),
        Obj::Output(mt) => (0, 0, mt.clone()),
        _ => return r(E_UNEXPECTED),
    };
    match method {
        "GetStreamType" | "GetType" => {
            r(if dshow::write_guid(env, a1, &media_type.major).is_ok() {
                S_OK
            } else {
                E_POINTER
            })
        }
        "GetStreamNumber" => r(if write_u16(env, a1, number) {
            S_OK
        } else {
            E_POINTER
        }),
        "GetBitrate" => r(if write_u32(env, a1, bit_rate) {
            S_OK
        } else {
            E_POINTER
        }),
        "SetMediaType" if matches!(payload(env, this), Some(Obj::Output(_))) => {
            if a1 == 0 {
                return r(E_POINTER);
            }
            let Ok(mt) = MediaType::read(env, a1) else {
                return r(E_POINTER);
            };
            if mt.major != dshow::MEDIATYPE_AUDIO && mt.major != dshow::MEDIATYPE_VIDEO {
                return r(0x8000_4005);
            }
            if let Some(Obj::Output(current)) = payload(env, this) {
                *current = mt;
            }
            r(S_OK)
        }
        "GetMediaType" => {
            // WM_MEDIA_TYPE with its format block placed directly after it.
            let size = MEDIA_TYPE_SIZE + media_type.format.len() as u32;
            if a2 == 0 {
                return r(E_INVALIDARG);
            }
            if a1 == 0 {
                return r(if write_u32(env, a2, size) {
                    S_OK
                } else {
                    E_POINTER
                });
            }
            let Ok(capacity) = env.cpu.read_u32(a2) else {
                return r(E_POINTER);
            };
            if capacity < size {
                write_u32(env, a2, size);
                return r(ASF_E_BUFFERTOOSMALL);
            }
            write_u32(env, a2, size);
            r(match write_media_type(env, a1, &media_type) {
                Ok(()) => S_OK,
                Err(error) => error,
            })
        }
        _ => None,
    }
}

fn write_media_type(env: &mut Env<'_>, pointer: u32, mt: &MediaType) -> Result<(), u32> {
    let format = pointer + MEDIA_TYPE_SIZE;
    let mut raw = [0u8; MEDIA_TYPE_SIZE as usize];
    raw[0..16].copy_from_slice(&mt.major);
    raw[16..32].copy_from_slice(&mt.sub);
    raw[32..36].copy_from_slice(&u32::from(mt.fixed_size).to_le_bytes());
    raw[36..40].copy_from_slice(&u32::from(mt.temporal).to_le_bytes());
    raw[40..44].copy_from_slice(&mt.sample_size.to_le_bytes());
    raw[44..60].copy_from_slice(&mt.format_type);
    raw[64..68].copy_from_slice(&(mt.format.len() as u32).to_le_bytes());
    raw[68..72].copy_from_slice(&format.to_le_bytes());
    env.cpu.write(pointer, &raw).map_err(|_| E_POINTER)?;
    env.cpu.write(format, &mt.format).map_err(|_| E_POINTER)
}

fn buffer_method(env: &mut Env<'_>, this: u32, method: &str) -> Option<Outcome> {
    let r = |value: u32| Some(Outcome::Return(value));
    let (a1, a2) = (env.arg(1), env.arg(2));
    let Some(Obj::Buffer { data, length, max }) = payload(env, this) else {
        return r(E_UNEXPECTED);
    };
    let (data, length, max) = (*data, *length, *max);
    let ok = |done: bool| r(if done { S_OK } else { E_POINTER });
    match method {
        "GetLength" => ok(write_u32(env, a1, length)),
        "GetMaxLength" => ok(write_u32(env, a1, max)),
        "SetLength" => {
            if a1 > max {
                return r(E_INVALIDARG);
            }
            if let Some(Obj::Buffer { length, .. }) = payload(env, this) {
                *length = a1;
            }
            r(S_OK)
        }
        "GetBuffer" => ok(write_u32(env, a1, data)),
        "GetBufferAndLength" => ok(write_u32(env, a1, data) && write_u32(env, a2, length)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
