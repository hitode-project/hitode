//! DirectShow (quartz) front: the filter graph manager, the system filters a game
//! can name by CLSID, and the pin, allocator and sample protocol shared with filters
//! the game implements itself. Graph semantics live here; demuxing and decoding are
//! adv-device's `media` decoder, and audio output is the DirectSound mixer.
//!
//! Every interface pointer handed to the guest is a guest-memory vtable of host
//! thunks. Pointers the guest hands back are called through their own vtables, so
//! host and guest objects are interchangeable on either side of a connection.
use std::collections::{BTreeMap, VecDeque};

use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, MemFault, Prot};
use adv_machine::{Conv, Env, Outcome};
use adv_os::Heaps;
use adv_os::com::{Com, Iid, QueryError};

mod filter;
mod graph;
mod sample;
mod stream;
mod video;
pub use video::{VideoProvider, VideoWindowGeometry, pump_video, set_video_provider};

pub(crate) const S_OK: u32 = 0;
pub(crate) const S_FALSE: u32 = 1;
pub(crate) const E_NOTIMPL: u32 = 0x8000_4001;
pub(crate) const E_NOINTERFACE: u32 = 0x8000_4002;
pub(crate) const E_POINTER: u32 = 0x8000_4003;
pub(crate) const E_UNEXPECTED: u32 = 0x8000_ffff;
pub(crate) const E_OUTOFMEMORY: u32 = 0x8007_000e;
pub(crate) const E_INVALIDARG: u32 = 0x8007_0057;
pub(crate) const CLASS_E_NOAGGREGATION: u32 = 0x8004_0110;
pub(crate) const VFW_E_NOT_CONNECTED: u32 = 0x8004_0209;
pub(crate) const VFW_E_NO_ALLOCATOR: u32 = 0x8004_020a;
pub(crate) const VFW_E_ALREADY_CONNECTED: u32 = 0x8004_0204;
pub(crate) const VFW_E_NO_ACCEPTABLE_TYPES: u32 = 0x8004_0207;
pub(crate) const VFW_E_INVALID_DIRECTION: u32 = 0x8004_0208;
pub(crate) const VFW_E_TYPE_NOT_ACCEPTED: u32 = 0x8004_022a;
pub(crate) const VFW_E_NOT_COMMITTED: u32 = 0x8004_0211;
pub(crate) const VFW_E_WRONG_STATE: u32 = 0x8004_0227;
pub(crate) const VFW_E_NOT_FOUND: u32 = 0x8004_0216;
pub(crate) const VFW_E_UNSUPPORTED_STREAM: u32 = 0x8004_0265;

/// A GUID in its in-memory (little-endian first three fields) byte order.
pub(crate) const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Iid {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5],
        d4[6], d4[7],
    ]
}
const fn quartz(d1: u32) -> Iid {
    guid(
        d1,
        0x0ad4,
        0x11ce,
        [0xb0, 0x3a, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70],
    )
}
const fn amovie(d1: u32) -> Iid {
    guid(
        d1,
        0x524f,
        0x11ce,
        [0x9f, 0x53, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70],
    )
}
const fn fourcc_type(d1: u32) -> Iid {
    guid(
        d1,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
    )
}

pub(crate) const IID_IPERSIST: Iid = guid(0x0000_010c, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
pub(crate) const IID_IFILTERGRAPH: Iid = quartz(0x56a8_689f);
pub(crate) const IID_IGRAPHBUILDER: Iid = quartz(0x56a8_68a9);
pub(crate) const IID_IFILTERGRAPH2: Iid = guid(
    0x36b7_3882,
    0xc2c8,
    0x11cf,
    [0x8b, 0x46, 0x00, 0x80, 0x5f, 0x6c, 0xef, 0x60],
);
pub(crate) const IID_IMEDIACONTROL: Iid = quartz(0x56a8_68b1);
pub(crate) const IID_IMEDIAEVENT: Iid = quartz(0x56a8_68b6);
pub(crate) const IID_IMEDIAEVENTEX: Iid = quartz(0x56a8_68c0);
pub(crate) const IID_IMEDIAEVENTSINK: Iid = quartz(0x56a8_68a2);
pub(crate) const IID_IMEDIASEEKING: Iid = guid(
    0x36b7_3880,
    0xc2c8,
    0x11cf,
    [0x8b, 0x46, 0x00, 0x80, 0x5f, 0x6c, 0xef, 0x60],
);
pub(crate) const IID_IMEDIAPOSITION: Iid = quartz(0x56a8_68b2);
pub(crate) const IID_IBASICAUDIO: Iid = quartz(0x56a8_68b3);
pub(crate) const IID_IVIDEOWINDOW: Iid = quartz(0x56a8_68b4);
pub(crate) const IID_IBASICVIDEO: Iid = quartz(0x56a8_68b5);
pub(crate) const IID_IMEDIAFILTER: Iid = quartz(0x56a8_6899);
pub(crate) const IID_IBASEFILTER: Iid = quartz(0x56a8_6895);
pub(crate) const IID_IENUMFILTERS: Iid = quartz(0x56a8_6893);
pub(crate) const IID_IENUMPINS: Iid = quartz(0x56a8_6892);
pub(crate) const IID_IPIN: Iid = quartz(0x56a8_6891);
pub(crate) const IID_IENUMMEDIATYPES: Iid = guid(
    0x89c3_1040,
    0x846b,
    0x11ce,
    [0x97, 0xd3, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a],
);
pub(crate) const IID_IMEMINPUTPIN: Iid = quartz(0x56a8_689d);
pub(crate) const IID_IMEMALLOCATOR: Iid = quartz(0x56a8_689c);
pub(crate) const IID_IMEDIASAMPLE: Iid = quartz(0x56a8_689a);
pub(crate) const IID_IASYNCREADER: Iid = quartz(0x56a8_68aa);
pub(crate) const IID_IFILESOURCEFILTER: Iid = quartz(0x56a8_68a6);

pub(crate) const MEDIATYPE_VIDEO: Iid = fourcc_type(0x7364_6976);
pub(crate) const MEDIATYPE_AUDIO: Iid = fourcc_type(0x7364_7561);
pub(crate) const MEDIATYPE_STREAM: Iid = amovie(0xe436_eb83);
pub(crate) const MEDIASUBTYPE_MPEG1SYSTEM: Iid = amovie(0xe436_eb84);
pub(crate) const MEDIASUBTYPE_MPEG1PAYLOAD: Iid = amovie(0xe436_eb81);
pub(crate) const MEDIASUBTYPE_MPEG1AUDIOPAYLOAD: Iid = fourcc_type(0x0000_0050);
pub(crate) const MEDIASUBTYPE_RGB565: Iid = amovie(0xe436_eb7b);
pub(crate) const MEDIASUBTYPE_RGB555: Iid = amovie(0xe436_eb7c);
pub(crate) const MEDIASUBTYPE_RGB24: Iid = amovie(0xe436_eb7d);
pub(crate) const MEDIASUBTYPE_RGB32: Iid = amovie(0xe436_eb7e);
pub(crate) const MEDIASUBTYPE_PCM: Iid = fourcc_type(0x0000_0001);
pub(crate) const FORMAT_VIDEOINFO: Iid = guid(
    0x0558_9f80,
    0xc356,
    0x11ce,
    [0xbf, 0x01, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a],
);
pub(crate) const FORMAT_WAVEFORMATEX: Iid = guid(
    0x0558_9f81,
    0xc356,
    0x11ce,
    [0xbf, 0x01, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a],
);
pub(crate) const GUID_NULL: Iid = [0; 16];

const CLSID_FILTERGRAPH: Iid = amovie(0xe436_ebb3);
const CLSID_VIDEO_RENDER: Iid = guid(
    0x70e1_02b0,
    0x5556,
    0x11ce,
    [0x97, 0xc0, 0, 0xaa, 0, 0x55, 0x59, 0x5a],
);
const CLSID_FILTERGRAPH_NO_THREAD: Iid = amovie(0xe436_ebb8);
const CLSID_MPEG1_SPLITTER: Iid = guid(
    0x3364_75d0,
    0x942a,
    0x11ce,
    [0xa8, 0x70, 0x00, 0xaa, 0x00, 0x2f, 0xea, 0xb5],
);
const CLSID_MPEG_VIDEO_CODEC: Iid = guid(
    0xfeb5_0740,
    0x7bef,
    0x11ce,
    [0x9b, 0xd9, 0x00, 0x00, 0xe2, 0x02, 0x59, 0x9c],
);
const CLSID_MPEG_AUDIO_CODEC: Iid = guid(
    0x4a22_86e0,
    0x7bef,
    0x11ce,
    [0x9b, 0xd9, 0x00, 0x00, 0xe2, 0x02, 0x59, 0x9c],
);
const CLSID_DSOUND_RENDER: Iid = guid(
    0x7937_6820,
    0x07d0,
    0x11cf,
    [0xa2, 0x4d, 0x00, 0x20, 0xaf, 0xd7, 0x97, 0x67],
);
const CLSID_AUDIO_RENDER: Iid = guid(
    0xe306_29d1,
    0x27e5,
    0x11ce,
    [0x87, 0x5d, 0x00, 0x60, 0x8c, 0xb7, 0x80, 0x66],
);

/// One guest-visible vtable layout. The generated COM table supplies slots and
/// stack widths; a face may answer for every IID its vtable is a superset of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Face {
    FilterGraph,
    MediaControl,
    MediaEvent,
    MediaEventSink,
    MediaSeeking,
    MediaPosition,
    BasicAudio,
    BasicVideo,
    VideoWindow,
    MediaFilter,
    BaseFilter,
    FileSource,
    EnumFilters,
    EnumPins,
    Pin,
    MemInputPin,
    EnumMediaTypes,
    MemAllocator,
    MediaSample,
}

const FACES: [(Face, &str); 19] = [
    (Face::FilterGraph, "IFilterGraph2"),
    (Face::MediaControl, "IMediaControl"),
    (Face::MediaEvent, "IMediaEventEx"),
    (Face::MediaEventSink, "IMediaEventSink"),
    (Face::MediaSeeking, "IMediaSeeking"),
    (Face::MediaPosition, "IMediaPosition"),
    (Face::BasicAudio, "IBasicAudio"),
    (Face::BasicVideo, "IBasicVideo"),
    (Face::VideoWindow, "IVideoWindow"),
    (Face::MediaFilter, "IMediaFilter"),
    (Face::BaseFilter, "IBaseFilter"),
    (Face::FileSource, "IFileSourceFilter"),
    (Face::EnumFilters, "IEnumFilters"),
    (Face::EnumPins, "IEnumPins"),
    (Face::Pin, "IPin"),
    (Face::MemInputPin, "IMemInputPin"),
    (Face::EnumMediaTypes, "IEnumMediaTypes"),
    (Face::MemAllocator, "IMemAllocator"),
    (Face::MediaSample, "IMediaSample"),
];

impl Face {
    fn interface(self) -> &'static str {
        FACES[self as usize].1
    }
    /// The IIDs this vtable answers for, besides IUnknown.
    fn iids(self) -> &'static [Iid] {
        match self {
            Face::FilterGraph => &[IID_IFILTERGRAPH, IID_IGRAPHBUILDER, IID_IFILTERGRAPH2],
            Face::MediaControl => &[IID_IMEDIACONTROL],
            Face::MediaEvent => &[IID_IMEDIAEVENT, IID_IMEDIAEVENTEX],
            Face::MediaEventSink => &[IID_IMEDIAEVENTSINK],
            Face::MediaSeeking => &[IID_IMEDIASEEKING],
            Face::MediaPosition => &[IID_IMEDIAPOSITION],
            Face::BasicAudio => &[IID_IBASICAUDIO],
            Face::BasicVideo => &[IID_IBASICVIDEO],
            Face::VideoWindow => &[IID_IVIDEOWINDOW],
            Face::MediaFilter => &[IID_IMEDIAFILTER, IID_IPERSIST],
            Face::BaseFilter => &[IID_IBASEFILTER, IID_IMEDIAFILTER, IID_IPERSIST],
            Face::FileSource => &[IID_IFILESOURCEFILTER],
            Face::EnumFilters => &[IID_IENUMFILTERS],
            Face::EnumPins => &[IID_IENUMPINS],
            Face::Pin => &[IID_IPIN],
            Face::MemInputPin => &[IID_IMEMINPUTPIN],
            Face::EnumMediaTypes => &[IID_IENUMMEDIATYPES],
            Face::MemAllocator => &[IID_IMEMALLOCATOR],
            Face::MediaSample => &[IID_IMEDIASAMPLE],
        }
    }
}

/// COM payload. Larger state lives in [`State`] maps keyed by the canonical pointer.
pub(crate) enum Obj {
    Graph,
    Filter,
    Pin,
    EnumFilters {
        list: Vec<u32>,
        position: usize,
    },
    EnumPins {
        filter: u32,
        position: usize,
    },
    EnumTypes {
        types: Vec<MediaType>,
        position: usize,
    },
    Allocator,
    Sample,
}

pub(crate) struct State {
    com: Com,
    tables: BTreeMap<Face, u32>,
    pub(crate) graphs: BTreeMap<u32, graph::Graph>,
    pub(crate) filters: BTreeMap<u32, filter::Filter>,
    pub(crate) pins: BTreeMap<u32, filter::Pin>,
    pub(crate) allocators: BTreeMap<u32, sample::Allocator>,
    pub(crate) samples: BTreeMap<u32, sample::Sample>,
    pub(crate) sessions: BTreeMap<u32, stream::Session>,
    pub(crate) next_session: u32,
    /// Graph window notifications (PostMessageA thunk, arguments) awaiting a guest call.
    pub(crate) pending_posts: Vec<(u32, [u32; 4])>,
}

/// Dispatch data: face index and slot, bound once per thunk.
fn data(face: Face, slot: u16) -> u64 {
    ((face as u64) << 16) | u64::from(slot)
}

pub fn register(registry: &mut Registry) {
    for (face, interface) in FACES {
        for signature in adv_abi::signature::com_signatures()
            .iter()
            .filter(|signature| signature.module == interface)
        {
            let (Some(conv), Some(slot)) = (signature.conv, signature.com_slot) else {
                continue;
            };
            registry.add_with_data(
                "quartz.dll",
                &format!("{interface}::{}", signature.name),
                conv,
                dispatch,
                data(face, slot),
            );
        }
    }
    registry.add(
        "quartz.dll",
        stream::THREAD_ENTRY,
        Conv::Stdcall { argc: 1 },
        stream::thread_entry,
    );
}

/// Register the classes; vtables are built on first activation.
pub fn install<B: adv_cpu::backend::Backend>(m: &mut adv_machine::Machine<B>) {
    let classes = m.ext.get_or_default::<adv_os::com::ClassFactories>();
    classes.register(CLSID_FILTERGRAPH, graph::create);
    classes.register(CLSID_FILTERGRAPH_NO_THREAD, graph::create);
    classes.register(CLSID_MPEG1_SPLITTER, filter::create_splitter);
    classes.register(CLSID_MPEG_VIDEO_CODEC, filter::create_video_decoder);
    classes.register(CLSID_MPEG_AUDIO_CODEC, filter::create_audio_decoder);
    classes.register(CLSID_DSOUND_RENDER, filter::create_audio_renderer);
    classes.register(CLSID_AUDIO_RENDER, filter::create_audio_renderer);
}

fn initialize(env: &mut Env<'_>) -> Result<(), String> {
    if env.ext.try_get::<State>().is_some() {
        return Ok(());
    }
    let mut tables = BTreeMap::new();
    for (face, interface) in FACES {
        let methods: Vec<_> = adv_abi::signature::com_signatures()
            .iter()
            .filter(|signature| signature.module == interface)
            .collect();
        let table = alloc(env, methods.len() as u32 * 4).ok_or("DirectShow vtable allocation")?;
        for signature in methods {
            let slot = u32::from(signature.com_slot.ok_or("COM slot")?);
            let name = format!("{interface}::{}", signature.name);
            let method = env
                .sys
                .thunks
                .publish(env.cpu, "quartz.dll", &name)
                .map_err(|e| e.to_string())?;
            env.cpu
                .write_u32(table + slot * 4, method)
                .map_err(|e| e.to_string())?;
        }
        tables.insert(face, table);
    }
    let com = Com::new(&mut env.sys.objects).ok_or("COM namespace exhausted")?;
    env.ext.insert(State {
        com,
        tables,
        graphs: BTreeMap::new(),
        filters: BTreeMap::new(),
        pins: BTreeMap::new(),
        allocators: BTreeMap::new(),
        samples: BTreeMap::new(),
        sessions: BTreeMap::new(),
        next_session: 1,
        pending_posts: Vec::new(),
    });
    Ok(())
}

pub(crate) fn state<'a>(env: &'a mut Env<'_>) -> &'a mut State {
    env.ext.get_mut::<State>()
}

pub(crate) fn alloc(env: &mut Env<'_>, len: u32) -> Option<u32> {
    let heaps = env.ext.get_mut::<Heaps>();
    heaps.alloc(env.cpu, &mut env.sys.vm, heaps.process_heap, len, true)
}

pub(crate) fn free(env: &mut Env<'_>, pointer: u32) {
    let heaps = env.ext.get_mut::<Heaps>();
    heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer);
}

/// Publish a host object with one vtable pointer per face. Returns the face
/// pointers in order; the first is canonical. The caller stores its own state
/// under that pointer before handing any of them to the guest.
pub(crate) fn create(env: &mut Env<'_>, faces: &[Face], value: Obj) -> Result<Vec<u32>, u32> {
    initialize(env).map_err(|error| {
        log::warn!("DirectShow: {error}");
        E_OUTOFMEMORY
    })?;
    let block = alloc(env, faces.len() as u32 * 4).ok_or(E_OUTOFMEMORY)?;
    let pointers: Vec<u32> = (0..faces.len() as u32).map(|i| block + i * 4).collect();
    for (&face, &pointer) in faces.iter().zip(&pointers) {
        let table = state(env).tables[&face];
        if env.cpu.write_u32(pointer, table).is_err() {
            free(env, block);
            return Err(E_OUTOFMEMORY);
        }
    }
    let com = state(env).com;
    let Some(id) = com.create(&mut env.sys.objects, block, faces[0].iids()[0], value) else {
        free(env, block);
        return Err(E_OUTOFMEMORY);
    };
    for (&face, &pointer) in faces.iter().zip(&pointers) {
        for &iid in face.iids() {
            if !com.register_interface(&mut env.sys.objects, id, iid, pointer) {
                release(env, block);
                return Err(E_OUTOFMEMORY);
            }
        }
    }
    Ok(pointers)
}

/// Canonical (IUnknown) pointer of a host object, given any of its faces.
pub(crate) fn canonical(env: &Env<'_>, pointer: u32) -> Option<u32> {
    let state = env.ext.try_get::<State>()?;
    let id = state.com.identity::<Obj>(&env.sys.objects, pointer)?;
    state.com.canonical(&env.sys.objects, id)
}

pub(crate) fn payload<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a Obj> {
    let state = env.ext.try_get::<State>()?;
    state.com.get::<Obj>(&env.sys.objects, pointer)
}

pub(crate) fn payload_mut<'a>(env: &'a mut Env<'_>, pointer: u32) -> Option<&'a mut Obj> {
    let com = env.ext.try_get::<State>()?.com;
    com.get_mut::<Obj>(&mut env.sys.objects, pointer)
}

pub(crate) fn add_ref(env: &mut Env<'_>, pointer: u32) -> u32 {
    let Some(state) = env.ext.try_get::<State>() else {
        return 0;
    };
    state
        .com
        .add_ref::<Obj>(&mut env.sys.objects, pointer)
        .unwrap_or(0)
}

/// Host-side Release; samples return to their allocator instead of being destroyed.
pub(crate) fn release(env: &mut Env<'_>, pointer: u32) -> u32 {
    let Some(state) = env.ext.try_get::<State>() else {
        return 0;
    };
    let com = state.com;
    let canonical = canonical(env, pointer);
    let Some(released) = com.release::<Obj>(&mut env.sys.objects, pointer) else {
        return 0;
    };
    if released.references == 0
        && let Some(canonical) = canonical
    {
        sample::returned(env, canonical);
    }
    if let Some(destroyed) = released.destroyed {
        if let Some(canonical) = canonical {
            destroyed_object(env, canonical, &destroyed.value);
        }
        // All faces share one block, which starts at the canonical pointer.
        if let Some(&first) = destroyed.interfaces.iter().min() {
            free(env, first);
        }
    }
    released.references
}

fn destroyed_object(env: &mut Env<'_>, canonical: u32, value: &Obj) {
    let state = state(env);
    match value {
        Obj::Graph => {
            video::detach(env, canonical);
            self::state(env).graphs.remove(&canonical);
        }
        Obj::Filter => {
            let handle = state
                .filters
                .remove(&canonical)
                .and_then(|f| f.source.and_then(|s| s.file_handle));
            if let Some(handle) = handle {
                env.sys.objects.close(handle);
            }
        }
        Obj::Pin => {
            state.pins.remove(&canonical);
        }
        Obj::Allocator => {
            state.allocators.remove(&canonical);
        }
        Obj::Sample => {
            state.samples.remove(&canonical);
        }
        Obj::EnumFilters { .. } | Obj::EnumPins { .. } | Obj::EnumTypes { .. } => {}
    }
}

/// Keep a host object alive without a guest-visible reference.
pub(crate) fn retain_internal(env: &mut Env<'_>, pointer: u32) -> bool {
    let com = state(env).com;
    let Some(id) = com.identity::<Obj>(&env.sys.objects, pointer) else {
        return false;
    };
    com.retain_internal(&mut env.sys.objects, id)
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
            "DirectShow {}::{method}: unknown this {this:#x}",
            face.interface()
        );
        return Outcome::Return(E_POINTER);
    }
    match slot {
        0 => return Outcome::Return(query_interface(env, this, env.arg(1), env.arg(2))),
        1 => return Outcome::Return(add_ref(env, this)),
        2 => return graph::release_method(env, this),
        _ => {}
    }
    let outcome = match face {
        Face::FilterGraph
        | Face::MediaControl
        | Face::MediaEvent
        | Face::MediaEventSink
        | Face::MediaSeeking
        | Face::MediaPosition
        | Face::BasicAudio
        | Face::BasicVideo
        | Face::VideoWindow
        | Face::MediaFilter => graph::call(env, face, &method, this),
        Face::BaseFilter
        | Face::FileSource
        | Face::EnumFilters
        | Face::EnumPins
        | Face::Pin
        | Face::MemInputPin
        | Face::EnumMediaTypes => filter::call(env, face, &method, this),
        Face::MemAllocator | Face::MediaSample => sample::call(env, face, &method, this),
    };
    outcome.unwrap_or_else(|| {
        log::debug!("DirectShow {}::{method}: not implemented", face.interface());
        Outcome::Return(E_NOTIMPL)
    })
}

pub(crate) fn query_interface(env: &mut Env<'_>, this: u32, iid: u32, output: u32) -> u32 {
    if output == 0 || iid == 0 {
        return E_POINTER;
    }
    let _ = env.cpu.write_u32(output, 0);
    let Ok(requested) = read_guid(env, iid) else {
        return E_POINTER;
    };
    let com = state(env).com;
    match com.query::<Obj>(&mut env.sys.objects, this, requested) {
        Ok(pointer) => {
            let _ = env.cpu.write_u32(output, pointer);
            S_OK
        }
        Err(QueryError::NoInterface) => E_NOINTERFACE,
        Err(QueryError::ReferenceLimit) => E_OUTOFMEMORY,
        Err(QueryError::InvalidObject) => E_POINTER,
    }
}

pub(crate) fn read_guid(env: &Env<'_>, pointer: u32) -> Result<Iid, MemFault> {
    let mut bytes = [0; 16];
    env.cpu.read(pointer, &mut bytes)?;
    Ok(bytes)
}

pub(crate) fn write_guid(env: &mut Env<'_>, pointer: u32, value: &Iid) -> Result<(), MemFault> {
    env.cpu.write(pointer, value)
}

pub(crate) fn write_i64(env: &mut Env<'_>, pointer: u32, value: i64) -> bool {
    pointer != 0 && env.cpu.write(pointer, &value.to_le_bytes()).is_ok()
}

pub(crate) fn read_i64(env: &Env<'_>, pointer: u32) -> Option<i64> {
    let mut bytes = [0; 8];
    (pointer != 0 && env.cpu.read(pointer, &mut bytes).is_ok()).then(|| i64::from_le_bytes(bytes))
}

/// The i64 argument occupying stack words `index` and `index + 1`.
pub(crate) fn arg_i64(env: &Env<'_>, index: u32) -> i64 {
    (u64::from(env.arg(index)) | (u64::from(env.arg(index + 1)) << 32)) as i64
}

pub(crate) fn arg_f64(env: &Env<'_>, index: u32) -> f64 {
    f64::from_bits(u64::from(env.arg(index)) | (u64::from(env.arg(index + 1)) << 32))
}

pub(crate) fn read_wide(env: &Env<'_>, pointer: u32, max: usize) -> Vec<u16> {
    let mut text = Vec::new();
    if pointer == 0 {
        return text;
    }
    while text.len() < max {
        match env.cpu.read_u16(pointer + text.len() as u32 * 2) {
            Ok(0) | Err(_) => break,
            Ok(c) => text.push(c),
        }
    }
    text
}

/// Call method `slot` of any COM pointer, host or guest, through its vtable.
pub(crate) fn guest_call(
    env: &mut Env<'_>,
    this: u32,
    slot: u32,
    args: Vec<u32>,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let target = env
        .cpu
        .read_u32(this)
        .and_then(|table| env.cpu.read_u32(table + slot * 4));
    match target {
        Ok(target) if this != 0 && target != 0 => {
            let mut all = Vec::with_capacity(args.len() + 1);
            all.push(this);
            all.extend(args);
            Outcome::call(target, all, move |env, resume| then(env, resume.value()))
        }
        _ => then(env, E_POINTER),
    }
}

pub(crate) type Then = Box<dyn FnOnce(&mut Env<'_>, Vec<u32>) -> Outcome>;

/// One queued guest COM call: `this`, slot, arguments after `this`.
pub(crate) struct Call(pub u32, pub u32, pub Vec<u32>);

/// Run guest calls in order, then `then` with every result.
pub(crate) fn chain(
    env: &mut Env<'_>,
    mut calls: VecDeque<Call>,
    mut results: Vec<u32>,
    then: Then,
) -> Outcome {
    let Some(Call(this, slot, args)) = calls.pop_front() else {
        return then(env, results);
    };
    guest_call(env, this, slot, args, move |env, value| {
        results.push(value);
        chain(env, calls, results, then)
    })
}

/// AM_MEDIA_TYPE without its pUnk (always null here).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MediaType {
    pub major: Iid,
    pub sub: Iid,
    pub fixed_size: bool,
    pub temporal: bool,
    pub sample_size: u32,
    pub format_type: Iid,
    pub format: Vec<u8>,
}

pub(crate) const MEDIA_TYPE_SIZE: u32 = 72;

impl MediaType {
    pub fn read(env: &Env<'_>, pointer: u32) -> Result<Self, MemFault> {
        let mut raw = [0u8; MEDIA_TYPE_SIZE as usize];
        env.cpu.read(pointer, &mut raw)?;
        let word = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
        let (size, data) = (word(64), word(68));
        let mut format = vec![0; size.min(0x10000) as usize];
        if !format.is_empty() && data != 0 {
            env.cpu.read(data, &mut format)?;
        } else {
            format.clear();
        }
        Ok(Self {
            major: raw[0..16].try_into().unwrap(),
            sub: raw[16..32].try_into().unwrap(),
            fixed_size: word(32) != 0,
            temporal: word(36) != 0,
            sample_size: word(40),
            format_type: raw[44..60].try_into().unwrap(),
            format,
        })
    }

    /// Fill a guest AM_MEDIA_TYPE; the format block is CoTaskMemAlloc'd so the
    /// receiver may free it with DeleteMediaType / FreeMediaType.
    pub fn write(&self, env: &mut Env<'_>, pointer: u32) -> Result<(), u32> {
        if pointer == 0 || !env.cpu.accessible(pointer, MEDIA_TYPE_SIZE, Prot::WRITE) {
            return Err(E_POINTER);
        }
        let data = if self.format.is_empty() {
            0
        } else {
            let data = adv_os::com::task_alloc(env, self.format.len() as u32);
            if data == 0 {
                return Err(E_OUTOFMEMORY);
            }
            if env.cpu.write(data, &self.format).is_err() {
                adv_os::com::task_free(env, data);
                return Err(E_POINTER);
            }
            data
        };
        let mut raw = [0u8; MEDIA_TYPE_SIZE as usize];
        raw[0..16].copy_from_slice(&self.major);
        raw[16..32].copy_from_slice(&self.sub);
        raw[32..36].copy_from_slice(&u32::from(self.fixed_size).to_le_bytes());
        raw[36..40].copy_from_slice(&u32::from(self.temporal).to_le_bytes());
        raw[40..44].copy_from_slice(&self.sample_size.to_le_bytes());
        raw[44..60].copy_from_slice(&self.format_type);
        raw[64..68].copy_from_slice(&(self.format.len() as u32).to_le_bytes());
        raw[68..72].copy_from_slice(&data.to_le_bytes());
        if env.cpu.write(pointer, &raw).is_err() {
            if data != 0 {
                adv_os::com::task_free(env, data);
            }
            return Err(E_POINTER);
        }
        Ok(())
    }

    /// A new task-memory AM_MEDIA_TYPE, as CreateMediaType returns.
    pub fn create(&self, env: &mut Env<'_>) -> Result<u32, u32> {
        let pointer = adv_os::com::task_alloc(env, MEDIA_TYPE_SIZE);
        if pointer == 0 {
            return Err(E_OUTOFMEMORY);
        }
        if let Err(error) = self.write(env, pointer) {
            adv_os::com::task_free(env, pointer);
            return Err(error);
        }
        Ok(pointer)
    }

    pub fn video_rgb(sub: Iid, width: u32, height: u32, frame_100ns: i64) -> Self {
        let bits: u16 = match sub {
            s if s == MEDIASUBTYPE_RGB32 => 32,
            s if s == MEDIASUBTYPE_RGB24 => 24,
            _ => 16,
        };
        let stride = (width * u32::from(bits) / 8 + 3) & !3;
        let image = stride * height;
        let masks = sub == MEDIASUBTYPE_RGB565;
        let mut format = vec![0u8; 88 + if masks { 12 } else { 0 }];
        // rcSource / rcTarget stay empty: the whole image.
        let bit_rate = (u64::from(image) * 8 * 10_000_000 / frame_100ns.max(1) as u64) as u32;
        format[32..36].copy_from_slice(&bit_rate.to_le_bytes());
        format[40..48].copy_from_slice(&frame_100ns.to_le_bytes());
        let header = &mut format[48..88];
        header[0..4].copy_from_slice(&40u32.to_le_bytes());
        header[4..8].copy_from_slice(&width.to_le_bytes());
        header[8..12].copy_from_slice(&height.to_le_bytes());
        header[12..14].copy_from_slice(&1u16.to_le_bytes());
        header[14..16].copy_from_slice(&bits.to_le_bytes());
        // BI_BITFIELDS for 565; BI_RGB otherwise.
        header[16..20].copy_from_slice(&(if masks { 3u32 } else { 0 }).to_le_bytes());
        header[20..24].copy_from_slice(&image.to_le_bytes());
        if masks {
            for (i, mask) in [0xf800u32, 0x07e0, 0x001f].into_iter().enumerate() {
                format[88 + i * 4..92 + i * 4].copy_from_slice(&mask.to_le_bytes());
            }
        }
        Self {
            major: MEDIATYPE_VIDEO,
            sub,
            fixed_size: true,
            temporal: false,
            sample_size: image,
            format_type: FORMAT_VIDEOINFO,
            format,
        }
    }

    pub fn audio_pcm(rate: u32, channels: u16) -> Self {
        let align = channels * 2;
        let mut format = vec![0u8; 18];
        format[0..2].copy_from_slice(&1u16.to_le_bytes());
        format[2..4].copy_from_slice(&channels.to_le_bytes());
        format[4..8].copy_from_slice(&rate.to_le_bytes());
        format[8..12].copy_from_slice(&(rate * u32::from(align)).to_le_bytes());
        format[12..14].copy_from_slice(&align.to_le_bytes());
        format[14..16].copy_from_slice(&16u16.to_le_bytes());
        Self {
            major: MEDIATYPE_AUDIO,
            sub: MEDIASUBTYPE_PCM,
            fixed_size: true,
            temporal: false,
            sample_size: u32::from(align),
            format_type: FORMAT_WAVEFORMATEX,
            format,
        }
    }

    /// Image geometry of a VIDEOINFOHEADER type: width, signed height, bit depth.
    pub fn video_geometry(&self) -> Option<(u32, i32, u16)> {
        if self.format_type != FORMAT_VIDEOINFO || self.format.len() < 88 {
            return None;
        }
        let h = &self.format[48..88];
        Some((
            u32::from_le_bytes(h[4..8].try_into().unwrap()),
            i32::from_le_bytes(h[8..12].try_into().unwrap()),
            u16::from_le_bytes(h[14..16].try_into().unwrap()),
        ))
    }
}

#[cfg(test)]
mod tests;
