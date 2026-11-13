//! DirectSound PCM buffers. Guest-visible clocks stay independent of device consumption.
use adv_abi::{GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::{Backend, Guest, GuestExt, Prot};
use adv_device::{AudioControl, AudioEvent, RingState, ring::RingConfig};
use adv_machine::{Conv, Env, Machine, Objects, Outcome};
use adv_os::Heaps;
use adv_os::com::{Com, ComIdentity, QueryError};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;

mod enumeration;
mod spatial;
use spatial::{Deferred, Listener, Source, Vector};

const INVALID: u32 = 0x80070057;
const UNSUPPORTED: u32 = 0x80004001;
const NO_INTERFACE: u32 = 0x80004002;
const OUT_OF_MEMORY: u32 = 0x8007000e;
const INVALID_CALL: u32 = 0x88780032;
const NO_DRIVER: u32 = 0x88780078;
const CONTROL_UNAVAILABLE: u32 = 0x8878001e;
const OBJECT_NOT_FOUND: u32 = 0x88781161;
const CLASS_NOT_REGISTERED: u32 = 0x80040154;
const CTRL_3D: u32 = 0x10;
const CTRL_PAN: u32 = 0x40;
const CTRL_FX: u32 = 0x200;
const MUTE_3D_AT_MAX_DISTANCE: u32 = 0x20000;
const LOC_DEFER: u32 = 0x40000;
const MAX_FX_DESCRIPTORS: u32 = 64;
const DEVICE: &[(&str, u8)] = &[
    ("QueryInterface", 3),
    ("AddRef", 1),
    ("Release", 1),
    ("CreateSoundBuffer", 4),
    ("GetCaps", 2),
    ("DuplicateSoundBuffer", 3),
    ("SetCooperativeLevel", 3),
    ("Compact", 1),
    ("GetSpeakerConfig", 2),
    ("SetSpeakerConfig", 2),
    ("Initialize", 2),
    ("VerifyCertification", 2),
];
const BUFFER: &[(&str, u8)] = &[
    ("QueryInterface", 3),
    ("AddRef", 1),
    ("Release", 1),
    ("GetCaps", 2),
    ("GetCurrentPosition", 3),
    ("GetFormat", 4),
    ("GetVolume", 2),
    ("GetPan", 2),
    ("GetFrequency", 2),
    ("GetStatus", 2),
    ("Initialize", 3),
    ("Lock", 8),
    ("Play", 4),
    ("SetCurrentPosition", 2),
    ("SetFormat", 2),
    ("SetVolume", 2),
    ("SetPan", 2),
    ("SetFrequency", 2),
    ("Stop", 1),
    ("Unlock", 5),
    ("Restore", 1),
    ("SetFX", 4),
    ("AcquireResources", 4),
    ("GetObjectInPath", 5),
];
const NOTIFY: &[(&str, u8)] = &[
    ("QueryInterface", 3),
    ("AddRef", 1),
    ("Release", 1),
    ("SetNotificationPositions", 3),
];
const LISTENER: &[(&str, u8)] = &[
    ("QueryInterface", 3),
    ("AddRef", 1),
    ("Release", 1),
    ("GetAllParameters", 2),
    ("GetDistanceFactor", 2),
    ("GetDopplerFactor", 2),
    ("GetOrientation", 3),
    ("GetPosition", 2),
    ("GetRolloffFactor", 2),
    ("GetVelocity", 2),
    ("SetAllParameters", 3),
    ("SetDistanceFactor", 3),
    ("SetDopplerFactor", 3),
    ("SetOrientation", 8),
    ("SetPosition", 5),
    ("SetRolloffFactor", 3),
    ("SetVelocity", 5),
    ("CommitDeferredSettings", 1),
];
const BUFFER_3D: &[(&str, u8)] = &[
    ("QueryInterface", 3),
    ("AddRef", 1),
    ("Release", 1),
    ("GetAllParameters", 2),
    ("GetConeAngles", 3),
    ("GetConeOrientation", 2),
    ("GetConeOutsideVolume", 2),
    ("GetMaxDistance", 2),
    ("GetMinDistance", 2),
    ("GetMode", 2),
    ("GetPosition", 2),
    ("GetVelocity", 2),
    ("SetAllParameters", 3),
    ("SetConeAngles", 4),
    ("SetConeOrientation", 5),
    ("SetConeOutsideVolume", 3),
    ("SetMaxDistance", 3),
    ("SetMinDistance", 3),
    ("SetMode", 3),
    ("SetPosition", 5),
    ("SetVelocity", 5),
];
/// The interfaces whose method tables the front allocates, by table index.
const INTERFACES: [(&str, &[(&str, u8)]); 5] = [
    ("IDirectSound8", DEVICE),
    ("IDirectSoundBuffer8", BUFFER),
    ("IDirectSoundNotify", NOTIFY),
    ("IDirectSound3DListener", LISTENER),
    ("IDirectSound3DBuffer", BUFFER_3D),
];
const IID_DEVICE: [u8; 16] = [
    0x83, 0xfa, 0x9a, 0x27, 0x81, 0x49, 0xce, 0x11, 0xa5, 0x21, 0, 0x20, 0xaf, 0x0b, 0xe5, 0x60,
];
const IID_DEVICE8: [u8; 16] = [
    0x93, 0x7e, 0x0a, 0xc5, 0x95, 0xf3, 0x34, 0x48, 0x9e, 0xf6, 0x7f, 0xa9, 0x9d, 0xe5, 0x09, 0x66,
];
const CLSID_DEVICE: [u8; 16] = [
    0x46, 0xd9, 0xd4, 0x47, 0xe8, 0x62, 0xcf, 0x11, 0x93, 0xbc, 0x44, 0x45, 0x53, 0x54, 0, 0,
];
const CLSID_DEVICE8: [u8; 16] = [
    0x3f, 0xcc, 0x01, 0x39, 0xb5, 0x84, 0xa4, 0x4f, 0xba, 0x35, 0xaa, 0x81, 0x72, 0xb8, 0xa0, 0x9b,
];
/// The classes `CoCreateInstance` activates through this front.
pub const CLASSES: [[u8; 16]; 2] = [CLSID_DEVICE, CLSID_DEVICE8];
const IID_BUFFER: [u8; 16] = [
    0x85, 0xfa, 0x9a, 0x27, 0x81, 0x49, 0xce, 0x11, 0xa5, 0x21, 0, 0x20, 0xaf, 0x0b, 0xe5, 0x60,
];
const IID_BUFFER8: [u8; 16] = [
    0x49, 0xa4, 0x25, 0x68, 0x24, 0x75, 0x82, 0x4d, 0x92, 0x0f, 0x50, 0xe3, 0x6a, 0xb3, 0xab, 0x1e,
];
const IID_NOTIFY: [u8; 16] = [
    0x83, 0x07, 0x21, 0xb0, 0xcd, 0x89, 0xd0, 0x11, 0xaf, 8, 0, 0xa0, 0xc9, 0x25, 0xcd, 0x16,
];
const IID_LISTENER: [u8; 16] = [
    0x84, 0xfa, 0x9a, 0x27, 0x81, 0x49, 0xce, 0x11, 0xa5, 0x21, 0, 0x20, 0xaf, 0x0b, 0xe5, 0x60,
];
const IID_BUFFER_3D: [u8; 16] = [
    0x86, 0xfa, 0x9a, 0x27, 0x81, 0x49, 0xce, 0x11, 0xa5, 0x21, 0, 0x20, 0xaf, 0x0b, 0xe5, 0x60,
];

#[derive(Clone, Copy, Debug)]
struct Format {
    rate: u32,
    channels: u16,
    bits: u16,
}
impl Default for Format {
    fn default() -> Self {
        Self {
            rate: 44100,
            channels: 2,
            bits: 16,
        }
    }
}
impl Format {
    fn align(self) -> u32 {
        u32::from(self.channels) * u32::from(self.bits) / 8
    }
    fn read(cpu: &dyn Guest, ptr: u32) -> Result<Self, String> {
        let mut b = [0; 16];
        cpu.read(ptr, &mut b).map_err(|e| e.to_string())?;
        let f = Self {
            channels: u16::from_le_bytes(b[2..4].try_into().unwrap()),
            rate: u32::from_le_bytes(b[4..8].try_into().unwrap()),
            bits: u16::from_le_bytes(b[14..16].try_into().unwrap()),
        };
        if u16::from_le_bytes(b[..2].try_into().unwrap()) != 1
            || !matches!(f.channels, 1 | 2)
            || !matches!(f.bits, 8 | 16)
            || !(100..=200000).contains(&f.rate)
            || u16::from_le_bytes(b[12..14].try_into().unwrap()) != f.align() as u16
        {
            return Err("unsupported PCM format".into());
        }
        Ok(f)
    }
    fn bytes(self) -> [u8; 18] {
        let mut b = [0; 18];
        b[..2].copy_from_slice(&1u16.to_le_bytes());
        b[2..4].copy_from_slice(&self.channels.to_le_bytes());
        b[4..8].copy_from_slice(&self.rate.to_le_bytes());
        b[8..12].copy_from_slice(&(self.rate * self.align()).to_le_bytes());
        b[12..14].copy_from_slice(&(self.align() as u16).to_le_bytes());
        b[14..16].copy_from_slice(&self.bits.to_le_bytes());
        b
    }
}
#[derive(Clone, Copy)]
enum Kind {
    Device {
        initialized: bool,
        version8: bool,
    },
    Buffer,
    Notify(u32),
    /// The listener of a primary buffer, or the 3D control of a secondary one.
    Spatial(u32),
}
struct Object {
    kind: Kind,
    pointer: u32,
    notify: u32,
    spatial: u32,
    buffer: Option<Buffer>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Lock {
    p1: u32,
    n1: u32,
    p2: u32,
    n2: u32,
}
struct ChunkDump {
    writer: std::io::BufWriter<std::fs::File>,
    path: std::path::PathBuf,
    bytes: u64,
}
#[derive(Clone)]
struct SampleStorage {
    id: u64,
    addr: u32,
    #[allow(dead_code)]
    len: u32,
    read_alias: Arc<dyn adv_sync::shared::SharedReadView>,
}

struct StorageRecord {
    addr: u32,
    live_buffers: usize,
    pending_voices: std::collections::BTreeSet<u64>,
}

struct Buffer {
    audio_id: u64,
    voice: Option<Arc<RingState>>,
    playback_clock: Arc<adv_device::clock::PlaybackClock>,
    generation: u64,
    storage: Option<SampleStorage>,
    len: u32,
    flags: u32,
    format: Format,
    /// The playback rate, which a 3D source's Doppler shift may change.
    frequency: u32,
    volume: i32,
    pan: i32,
    spatial: Option<Deferred<Source>>,
    playing: bool,
    looping: bool,
    retuned: bool,
    min_lead_bytes: u32,
    base: u32,
    anchor: u64,
    queued_frames: u64,
    notice_bytes: u64,
    notifications: Vec<(u32, u32)>,
    lock: Option<Lock>,
    trace_pcm: Option<Vec<u8>>,
    trace_generation: u64,
    trace_chunks: Option<ChunkDump>,
}
impl Buffer {
    fn storage_addr(&self) -> u32 {
        self.storage.as_ref().map_or(0, |s| s.addr)
    }
    fn stereo_gain(&self) -> [f32; 2] {
        let db = |value: i32| {
            if value <= -10000 {
                0.0
            } else {
                10f32.powf(value as f32 / 2000.0)
            }
        };
        let gain = db(self.volume);
        [gain * db(-self.pan.max(0)), gain * db(self.pan.min(0))]
    }
    fn progress(&self, now: u64) -> u64 {
        u64::from(self.base)
            + if self.playing {
                now.saturating_sub(self.anchor)
                    .saturating_mul(u64::from(self.frequency))
                    / 1_000_000
                    * u64::from(self.format.align())
            } else {
                0
            }
    }
    fn cursors(&self, now: u64, device_connected: bool) -> (u32, u32) {
        if self.playback_clock.uses_device_time()
            && let Some((play, write)) = self
                .voice
                .as_ref()
                .and_then(|voice| voice.cursors(now, self.generation))
        {
            return (
                play,
                if device_connected {
                    write
                } else if !self.playing
                    || (!self.looping && self.progress(now) >= u64::from(self.len))
                {
                    play
                } else {
                    self.staged_lead(play, 0)
                },
            );
        }
        if self.len == 0 {
            return (0, 0);
        }
        if device_connected && self.playing && !self.retuned {
            // A fresh Play or SetCurrentPosition has not been mixed yet; hold
            // play at the start anchor and expose the initial staged lead so
            // neither cursor jumps or inverts when the first callback runs.
            let play = self.base;
            return (play, self.staged_lead(play, 0));
        }
        let progress = self.progress(now);
        if !self.looping && progress >= u64::from(self.len) {
            return (0, 0);
        }
        let align = self.format.align().max(1);
        let play = ((progress % u64::from(self.len)) as u32) / align * align;
        let write = if !self.playing {
            play
        } else if device_connected {
            let elapsed = progress.saturating_sub(u64::from(self.base));
            let carried = u64::from(self.min_lead_bytes).saturating_sub(elapsed);
            self.staged_lead(play, carried)
        } else {
            self.staged_lead(play, 0)
        };
        (play, write)
    }
    /// Without a live mixer frontier (or before the first callback), a playing
    /// buffer's write cursor leads its play cursor by the staged source window
    /// (up to 30 ms, capped at half the buffer); a stopped buffer's equals it.
    fn staged_lead(&self, play: u32, min_bytes: u64) -> u32 {
        if !self.playing || self.len == 0 {
            return play;
        }
        let align = u64::from(self.format.align().max(1));
        let frames = u64::from(self.len) / align;
        let lead = adv_device::ring::RingVoice::staged_lead_frames(frames, self.frequency) * align;
        let lead = lead
            .max(min_bytes)
            .min(u64::from(self.len.saturating_sub(align as u32)));
        let write = if self.looping {
            u64::from(play) + lead
        } else {
            (u64::from(play) + lead).min(u64::from(self.len))
        };
        (write % u64::from(self.len)) as u32
    }
    fn cursor(&self, now: u64, device_connected: bool) -> u32 {
        self.cursors(now, device_connected).0
    }
    fn stop(&mut self, now: u64, device_connected: bool) {
        self.base = self
            .cursor(now, device_connected)
            .min(self.len.saturating_sub(self.format.align()));
        self.playing = false;
        self.retuned = false;
        self.min_lead_bytes = 0;
    }
}
/// Optional deterministic decoder validation, before host volume or resampling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcmDigest {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub bytes: u32,
    pub fnv1a64: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioSourceStatus {
    pub buffers: u64,
    pub playing: u64,
    pub looping: u64,
    pub locked: u64,
}

pub struct Sound {
    pub enabled: bool,
    tables: [u32; 5],
    listener: Deferred<Listener>,
    com: Com,
    /// Non-owning iteration index; payload and all references live in Objects.
    buffer_ids: BTreeMap<u64, ComIdentity<Object>>,
    pub audio: AudioControl,
    owner: u32,
    next_audio_id: u64,
    next_storage_id: u64,
    storage_records: BTreeMap<u64, StorageRecord>,
    pending_release: BTreeMap<u64, (u64, bool)>,
    pending_notices: Vec<(u64, u32, u64, u64)>,
    ready_notices: Vec<u32>,
    pub queued_samples: u64,
    pub nonzero_samples: u64,
    pub capture_digests: bool,
    pub digests: Vec<PcmDigest>,
    trace: Option<std::io::BufWriter<std::fs::File>>,
    trace_pcm_dir: Option<std::path::PathBuf>,
    trace_pcm_chunks: bool,
    playback_clock: Arc<adv_device::clock::PlaybackClock>,
    host: Host,
}

/// What a userland other than the built-in one answers for the front.
#[derive(Clone, Copy, Default)]
pub struct Host {
    /// Guest storage for objects; the process heap when absent.
    pub allocator: Option<crate::interop::GuestAllocator>,
    /// Notification events; machine events when absent.
    pub events: Option<Events>,
}

/// Notification events that are not machine objects.
#[derive(Clone, Copy)]
pub struct Events {
    /// Whether `handle` may receive position notifications.
    pub valid: fn(&mut Env<'_>, u32) -> bool,
    /// Signal `handle` with audio wake priority.
    pub signal: fn(&mut Env<'_>, u32) -> Result<(), String>,
}
impl Sound {
    /// A mixer stream id for another front's owned PCM (movie audio).
    pub(crate) fn allocate_audio_id(&mut self) -> Option<u64> {
        let id = self.next_audio_id;
        self.next_audio_id = self.next_source_id()?;
        Some(id)
    }

    fn next_source_id(&self) -> Option<u64> {
        self.next_audio_id
            .checked_add(1)
            .filter(|next| (next >> 32) as u32 == self.owner)
    }

    fn buffer<'a>(&self, objects: &'a Objects, pointer: u32) -> Option<&'a Buffer> {
        self.com.get::<Object>(objects, pointer)?.buffer.as_ref()
    }

    fn buffers<'a>(&'a self, objects: &'a Objects) -> impl Iterator<Item = &'a Buffer> {
        self.buffer_ids
            .values()
            .filter_map(|&id| self.com.get_identity(objects, id)?.buffer.as_ref())
    }

    pub fn source_status(&self, objects: &Objects, guest_now: u64) -> AudioSourceStatus {
        let now = self.playback_clock.now_us(guest_now);
        let mut status = AudioSourceStatus::default();
        for buffer in self.buffers(objects).filter(|buffer| buffer.len != 0) {
            status.buffers += 1;
            status.locked += u64::from(buffer.lock.is_some());
            if buffer.playing && (buffer.looping || buffer.progress(now) < u64::from(buffer.len)) {
                status.playing += 1;
                status.looping += u64::from(buffer.looping);
            }
        }
        status
    }
}
pub fn register(reg: &mut Registry) {
    enumeration::register(reg, true);
    for name in ["#1", "DirectSoundCreate", "DirectSoundCreate8"] {
        reg.add("dsound.dll", name, Conv::Stdcall { argc: 3 }, call);
    }
    for (prefix, methods) in INTERFACES {
        for &(name, argc) in methods {
            reg.add(
                "dsound.dll",
                &format!("{prefix}::{name}"),
                Conv::Stdcall { argc },
                call,
            );
        }
    }
    crate::refcounts::install(reg, "dsound.dll");
}

/// The same no-driver failure used before the device personality is installed.
/// Kept in the DirectSound front; runtime compatibility assembly owns no API behavior.
pub fn register_unavailable(registry: &mut Registry) {
    enumeration::register(registry, false);
    registry.register(unavailable_create_ABI);
    registry.register(unavailable_create8_ABI);
    registry.add(
        "dsound.dll",
        "#1",
        unavailable_create_ABI.conv,
        unavailable_create_ABI.handler,
    );
}

#[win32(dsound, name = "DirectSoundCreate", level = safe_fail)]
fn unavailable_create(
    env: &mut Env<'_>,
    _device: GuestPtr<()>,
    output: GuestPtr<u32>,
    _outer: GuestPtr<()>,
) -> Outcome {
    if let Err(error) = env.cpu.write_u32(output.addr(), 0) {
        return Outcome::Fail(error.to_string());
    }
    Outcome::Return(0x8878_0078)
}

#[win32(dsound, name = "DirectSoundCreate8", level = safe_fail)]
fn unavailable_create8(
    env: &mut Env<'_>,
    device: GuestPtr<()>,
    output: GuestPtr<u32>,
    outer: GuestPtr<()>,
) -> Outcome {
    unavailable_create(env, device, output, outer)
}
pub fn install<B: Backend>(m: &mut Machine<B>) -> Result<(), String> {
    let audio = AudioControl::new(!m.sys.clock.is_virtual());
    install_audio(m, audio, false, Host::default())
}

/// Install process-local COM objects over the session's existing audio stream.
/// The process identity must be assigned before this call.
pub fn install_with_audio<B: Backend>(
    m: &mut Machine<B>,
    audio: AudioControl,
) -> Result<(), String> {
    install_audio(m, audio, true, Host::default())
}

/// Install the front over `audio` with `host`'s storage and events, for a
/// userland whose process heap and events are not the built-in stack's.
pub fn install_on_host<B: Backend>(
    m: &mut Machine<B>,
    audio: AudioControl,
    host: Host,
) -> Result<(), String> {
    install_audio(m, audio, true, host)
}

fn install_audio<B: Backend>(
    m: &mut Machine<B>,
    audio: AudioControl,
    shared: bool,
    host: Host,
) -> Result<(), String> {
    let trace = std::env::var_os("ADV_AUDIO_TRACE")
        .map(|path| {
            let mut path = std::path::PathBuf::from(path);
            if shared {
                // Child installation must not truncate the live parent's
                // diagnostics when both inherit the same host environment.
                let mut name = path
                    .file_name()
                    .ok_or("invalid ADV_AUDIO_TRACE path")?
                    .to_os_string();
                name.push(format!(".pid-{:08x}", m.sys.pid));
                path.set_file_name(name);
            }
            std::fs::File::create(&path)
                .map(std::io::BufWriter::new)
                .map_err(|error| format!("cannot create ADV_AUDIO_TRACE {path:?}: {error}"))
        })
        .transpose()?;
    let trace_pcm_dir = if trace.is_some() {
        std::env::var_os("ADV_AUDIO_DUMP_DIR")
            .map(|path| {
                let mut path = std::path::PathBuf::from(path);
                if shared {
                    path.push(format!("pid-{:08x}", m.sys.pid));
                }
                std::fs::create_dir_all(&path)
                    .map(|()| std::path::PathBuf::from(&path))
                    .map_err(|error| format!("cannot create ADV_AUDIO_DUMP_DIR {path:?}: {error}"))
            })
            .transpose()?
    } else {
        None
    };
    let mut tables = [0; 5];
    for (i, (prefix, methods)) in INTERFACES.into_iter().enumerate() {
        let table = m
            .sys
            .vm
            .alloc(&mut m.cpu, 4096, Prot::RW)
            .map_err(|e| e.to_string())?;
        for (slot, (name, _)) in methods.iter().enumerate() {
            let function = m
                .resolve_import("dsound.dll", &format!("{prefix}::{name}"))
                .map_err(|e| e.to_string())?;
            m.cpu
                .write_u32(table + slot as u32 * 4, function)
                .map_err(|e| e.to_string())?;
        }
        tables[i] = table;
    }
    let playback_clock = audio.playback_clock();
    let com = Com::new(&mut m.sys.objects).ok_or("audio object namespace exhausted")?;
    m.ext.insert(Sound {
        enabled: false,
        tables,
        listener: Deferred::new(Listener::default()),
        com,
        buffer_ids: BTreeMap::new(),
        audio,
        owner: m.sys.pid,
        next_audio_id: (u64::from(m.sys.pid) << 32) | 1,
        next_storage_id: 1,
        storage_records: BTreeMap::new(),
        pending_release: BTreeMap::new(),
        pending_notices: Vec::new(),
        ready_notices: Vec::new(),
        queued_samples: 0,
        nonzero_samples: 0,
        capture_digests: false,
        digests: Vec::new(),
        trace,
        trace_pcm_dir,
        trace_pcm_chunks: std::env::var_os("ADV_AUDIO_DUMP_CHUNKS").is_some(),
        playback_clock,
        host,
    });
    let classes = m.ext.get_or_default::<adv_os::com::ClassFactories>();
    classes.register(CLSID_DEVICE, create_class);
    classes.register(CLSID_DEVICE8, create_class8);
    Ok(())
}

/// Queue source retirement before releasing an exiting machine's guest memory.
/// The supervisor retries a false result after servicing the shared audio stream.
pub fn shutdown(ext: &mut adv_machine::Ext) -> bool {
    let Some(sound) = ext.try_get_mut::<Sound>() else {
        return true;
    };
    sound.enabled = false;
    sound.pending_notices.clear();
    sound.ready_notices.clear();
    sound.audio.remove_owner(sound.owner)
}

fn create_class(e: &mut Env<'_>, iid: [u8; 16], outer: u32, output: u32) -> Outcome {
    create_com_device(e, iid, outer, output, false)
}

fn create_class8(e: &mut Env<'_>, iid: [u8; 16], outer: u32, output: u32) -> Outcome {
    create_com_device(e, iid, outer, output, true)
}

fn create_com_device(
    e: &mut Env<'_>,
    iid: [u8; 16],
    outer: u32,
    output: u32,
    version8: bool,
) -> Outcome {
    if outer != 0 {
        return Outcome::Return(0x8004_0110);
    }
    if iid != adv_os::com::IUNKNOWN && iid != IID_DEVICE && !(version8 && iid == IID_DEVICE8) {
        return Outcome::Return(NO_INTERFACE);
    }
    let ptr = match object(
        e,
        Kind::Device {
            initialized: false,
            version8,
        },
        false,
        None,
    ) {
        Ok(Some(ptr)) => ptr,
        Ok(None) => return Outcome::Return(OUT_OF_MEMORY),
        Err(error) => return Outcome::Fail(error),
    };
    if let Err(error) = e.cpu.write_u32(output, ptr) {
        release(e, ptr);
        return Outcome::Fail(error.to_string());
    }
    Outcome::Return(0)
}
fn alloc(e: &mut Env<'_>, len: u32) -> Option<u32> {
    if let Some(allocator) = e.ext.get::<Sound>().host.allocator {
        return (allocator.allocate)(e, len);
    }
    let heaps = e.ext.get_mut::<Heaps>();
    heaps.alloc(e.cpu, &mut e.sys.vm, heaps.process_heap, len, true)
}
fn free(e: &mut Env<'_>, ptr: u32) {
    if let Some(allocator) = e.ext.get::<Sound>().host.allocator {
        return (allocator.free)(e, ptr);
    }
    let heaps = e.ext.get_mut::<Heaps>();
    heaps.free(e.cpu, &mut e.sys.vm, heaps.process_heap, ptr);
}

/// Signal position notifications on the calling thread.
fn notify(e: &mut Env<'_>, handles: Vec<u32>) -> Result<(), String> {
    match e.ext.get::<Sound>().host.events {
        Some(events) => handles
            .into_iter()
            .try_for_each(|handle| (events.signal)(e, handle)),
        None => {
            for handle in handles {
                e.sys
                    .set_event_with_priority(handle, adv_machine::WakePriority::Audio);
            }
            Ok(())
        }
    }
}
fn release(e: &mut Env<'_>, ptr: u32) -> u32 {
    let s = e.ext.get_mut::<Sound>();
    let Some(released) = s.com.release::<Object>(&mut e.sys.objects, ptr) else {
        return 0;
    };
    let Some(destroyed) = released.destroyed else {
        return released.references;
    };
    if let Some(buffer) = destroyed.value.buffer {
        s.buffer_ids.remove(&buffer.audio_id);
        if let Some(storage) = buffer.storage {
            let queued = s.audio.remove_pcm(buffer.audio_id);
            s.pending_release
                .insert(buffer.audio_id, (storage.id, queued));
            if let Some(record) = s.storage_records.get_mut(&storage.id) {
                record.live_buffers = record.live_buffers.saturating_sub(1);
                record.pending_voices.insert(buffer.audio_id);
            }
        }
    }
    // Retire later allocations first so failed creation restores the free-list head.
    let mut interfaces = destroyed.interfaces;
    interfaces.sort_by_key(|pointer| *pointer == destroyed.value.pointer);
    for pointer in interfaces {
        free(e, pointer);
    }
    0
}
/// An interface pointer whose method table is `INTERFACES[table]`.
fn interface(e: &mut Env<'_>, table: usize) -> Result<Option<u32>, String> {
    let Some(ptr) = alloc(e, 8) else {
        return Ok(None);
    };
    if let Err(error) = e.cpu.write_u32(ptr, e.ext.get::<Sound>().tables[table]) {
        free(e, ptr);
        return Err(error.to_string());
    }
    Ok(Some(ptr))
}
/// The 3D interface a buffer created with `flags` exposes, by table index:
/// every primary buffer the listener, a secondary buffer with `CTRL_3D` its
/// 3D control.
fn spatial_interface(flags: u32) -> Option<(usize, [u8; 16])> {
    if flags & 1 != 0 {
        Some((3, IID_LISTENER))
    } else if flags & CTRL_3D != 0 {
        Some((4, IID_BUFFER_3D))
    } else {
        None
    }
}
fn object(
    e: &mut Env<'_>,
    kind: Kind,
    with_notify: bool,
    spatial: Option<(usize, [u8; 16])>,
) -> Result<Option<u32>, String> {
    let i = match kind {
        Kind::Device { .. } => 0,
        Kind::Buffer => 1,
        Kind::Notify(_) | Kind::Spatial(_) => unreachable!("aliases belong to the buffer"),
    };
    let Some(ptr) = interface(e, i)? else {
        return Ok(None);
    };
    let mut aliases = Vec::new();
    for (table, iid) in [with_notify.then_some((2, IID_NOTIFY)), spatial]
        .into_iter()
        .flatten()
    {
        match interface(e, table) {
            Ok(Some(alias)) => aliases.push((alias, iid)),
            failed => {
                for &(alias, _) in &aliases {
                    free(e, alias);
                }
                free(e, ptr);
                return failed.map(|_| None);
            }
        }
    }
    let alias_of = |iid: [u8; 16]| {
        aliases
            .iter()
            .find(|&&(_, known)| known == iid)
            .map_or(0, |&(alias, _)| alias)
    };
    let (notify, spatial) = (
        alias_of(IID_NOTIFY),
        spatial.map_or(0, |(_, iid)| alias_of(iid)),
    );
    let com = e.ext.get::<Sound>().com;
    let (iid, iid8) = if matches!(kind, Kind::Device { .. }) {
        (IID_DEVICE, IID_DEVICE8)
    } else {
        (IID_BUFFER, IID_BUFFER8)
    };
    let Some(id) = com.create(
        &mut e.sys.objects,
        ptr,
        iid,
        Object {
            kind,
            pointer: ptr,
            notify,
            spatial,
            buffer: None,
        },
    ) else {
        for &(alias, _) in &aliases {
            free(e, alias);
        }
        free(e, ptr);
        return Ok(None);
    };
    let supports8 = !matches!(
        kind,
        Kind::Device {
            version8: false,
            ..
        }
    );
    if supports8 && !com.register_interface(&mut e.sys.objects, id, iid8, ptr) {
        for &(alias, _) in &aliases {
            free(e, alias);
        }
        release(e, ptr);
        return Ok(None);
    }
    for (at, &(alias, iid)) in aliases.iter().enumerate() {
        if !com.register_interface(&mut e.sys.objects, id, iid, alias) {
            // Registered aliases are released with the owner. A failed
            // registration leaves the remaining allocations outside the table
            // and the unpublished owner holds one ref.
            for &(rest, _) in &aliases[at..] {
                free(e, rest);
            }
            release(e, ptr);
            return Ok(None);
        }
    }
    com.enable_guest(e, id);
    Ok(Some(ptr))
}
/// The shared buffer always owns an empty effects chain. Nonempty requests fail
/// without publishing an effect object or discarding a requested DSP as success.
fn empty_fx(e: &mut Env<'_>, pointer: u32, name: &str) -> Result<u32, String> {
    let args: [u32; 5] = std::array::from_fn(|i| e.arg(i as u32));
    let com = e.ext.get::<Sound>().com;
    let Some(buffer) = com
        .get::<Object>(&e.sys.objects, pointer)
        .and_then(|object| object.buffer.as_ref())
    else {
        return Ok(INVALID);
    };
    let (flags, len, format) = (buffer.flags, buffer.len, buffer.format);
    if flags & CTRL_FX == 0 {
        return Ok(CONTROL_UNAVAILABLE);
    }
    if name == "GetObjectInPath" {
        if args[1] == 0 || args[3] == 0 || args[4] == 0 {
            return Ok(INVALID);
        }
        for address in [args[1], args[3]] {
            check_range(e.cpu, address, 16, Prot::READ).map_err(|error| error.to_string())?;
        }
        check_range(e.cpu, args[4], 4, Prot::WRITE).map_err(|error| error.to_string())?;
        // The native empty-chain failure preserves *ppObject, including a
        // caller's sentinel. No interface reference or COM header is allocated.
        return Ok(OBJECT_NOT_FOUND);
    }
    let playing = buffer.playing
        && (buffer.looping
            || buffer.len == 0
            || buffer.progress(e.sys.clock.now_us()) < u64::from(buffer.len));
    if playing || buffer.lock.is_some() {
        return Ok(INVALID_CALL);
    }
    let count = args[1];
    if count == 0 {
        // No DSP, interface, array or per-effect allocation exists to retire.
        return Ok(0);
    }
    let api = "dsound.dll!IDirectSoundBuffer8::SetFX";
    if count > MAX_FX_DESCRIPTORS {
        adv_abi::warn_unavailable(
            e,
            api,
            "effect descriptor count exceeds the bounded 64-effect profile; no chain was changed",
        );
        return Ok(UNSUPPORTED);
    }
    if args[2] == 0 {
        return Ok(INVALID);
    }
    check_range(e.cpu, args[2], count * 32, Prot::READ).map_err(|error| error.to_string())?;
    if args[3] != 0 {
        check_range(e.cpu, args[3], count * 4, Prot::WRITE).map_err(|error| error.to_string())?;
    }
    let mut registered_class = false;
    for index in 0..count {
        let mut descriptor = [0; 32];
        e.cpu
            .read(args[2] + index * 32, &mut descriptor)
            .map_err(|error| error.to_string())?;
        let word = |offset| u32::from_le_bytes(descriptor[offset..offset + 4].try_into().unwrap());
        if word(0) != 32 || word(4) & !3 != 0 || word(4) == 3 || word(24) != 0 || word(28) != 0 {
            return Ok(INVALID);
        }
        let class: [u8; 16] = descriptor[8..24].try_into().unwrap();
        registered_class |= e
            .ext
            .try_get::<adv_os::com::ClassFactories>()
            .is_some_and(|factories| factories.get(&class).is_some());
    }
    if args[3] != 0 {
        for index in 0..count {
            e.cpu
                .write_u32(args[3] + index * 4, 5)
                .map_err(|error| error.to_string())?; // DSFXR_UNKNOWN
        }
    }
    let hr = if registered_class {
        UNSUPPORTED
    } else {
        CLASS_NOT_REGISTERED
    };
    adv_abi::warn_unavailable(
        e,
        api,
        &format!(
            "nonempty effect DSP is unavailable; chain remains empty: count={count}, flags=0x{flags:08x}, bytes={len}, PCM rate={}, channels={}, bits={}, HRESULT=0x{hr:08x}",
            format.rate, format.channels, format.bits,
        ),
    );
    Ok(hr)
}

fn result(e: &mut Env<'_>) -> Result<u32, String> {
    e.ext.get::<Sound>().audio.sync_clock(e.sys.clock.now_us());
    let full = e.sys.thunks.get(e.thunk).name.clone();
    let name = full.split("::").last().unwrap();
    let a = e.arg(0);
    let b = e.arg(1);
    let c = e.arg(2);
    let now = e
        .ext
        .get::<Sound>()
        .playback_clock
        .now_us(e.sys.clock.now_us());
    if e.ext.get::<Sound>().trace.is_some() {
        let args: [u32; 8] = std::array::from_fn(|i| e.arg(i as u32));
        let ctx = e.context;
        let caller = e.caller();
        let files: Vec<_> = if matches!(name, "Play" | "Stop" | "SetCurrentPosition" | "Release") {
            e.sys
                .objects
                .hosts::<adv_os::vfs::FileObj>()
                .map(|(handle, file)| {
                    let native = &file.file;
                    (
                        handle,
                        file.guest_path.clone(),
                        native.stream_position().ok(),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let s = e.ext.get_mut::<Sound>();
        let snapshot = s.buffer(&e.sys.objects, a).map(|buf| {
            (
                buf.storage_addr(),
                buf.len,
                buf.playing,
                buf.base,
                buf.queued_frames,
            )
        });
        let connected = s.audio.device_connected();
        let cursor = s
            .buffer(&e.sys.objects, a)
            .map(|buf| buf.cursor(now, connected));
        if let Some(trace) = s.trace.as_mut() {
            let _ = writeln!(
                trace,
                "api us={now} ctx={ctx} caller={caller:08x} method={name} args={args:08x?} buffer={snapshot:?} cursor={cursor:?} files={files:?}"
            );
            if matches!(name, "Play" | "Stop" | "Release") {
                let _ = trace.flush();
            }
        }
    }
    if matches!(name, "#1" | "DirectSoundCreate" | "DirectSoundCreate8") {
        e.cpu.write_u32(b, 0).map_err(|e| e.to_string())?;
        if !e.ext.get::<Sound>().enabled {
            return Ok(NO_DRIVER);
        }
        if a != 0 {
            match enumeration::read_guid(e, a) {
                None => return Ok(INVALID),
                Some(guid) if !enumeration::playback(&guid) => return Ok(NO_DRIVER),
                Some(_) => {}
            }
        }
        if c != 0 {
            return Ok(0x80040110);
        }
        let Some(obj) = object(
            e,
            Kind::Device {
                initialized: true,
                version8: name == "DirectSoundCreate8",
            },
            false,
            None,
        )?
        else {
            return Ok(OUT_OF_MEMORY);
        };
        e.cpu.write_u32(b, obj).map_err(|e| e.to_string())?;
        return Ok(0);
    }
    let com = e.ext.get::<Sound>().com;
    let Some(obj) = com.get::<Object>(&e.sys.objects, a) else {
        return Ok(INVALID);
    };
    let kind = if a == obj.notify {
        Kind::Notify(obj.pointer)
    } else if a == obj.spatial {
        Kind::Spatial(obj.pointer)
    } else {
        obj.kind
    };
    if name == "AddRef" {
        return Ok(com.add_ref::<Object>(&mut e.sys.objects, a).unwrap_or(0));
    }
    if name == "Release" {
        return Ok(release(e, a));
    }
    if name == "QueryInterface" {
        e.cpu.write_u32(c, 0).map_err(|e| e.to_string())?;
        let mut iid = [0; 16];
        e.cpu.read(b, &mut iid).map_err(|e| e.to_string())?;
        let ptr = match com.query::<Object>(&mut e.sys.objects, a, iid) {
            Ok(ptr) => ptr,
            Err(QueryError::NoInterface) => return Ok(NO_INTERFACE),
            Err(QueryError::ReferenceLimit) => return Ok(OUT_OF_MEMORY),
            Err(QueryError::InvalidObject) => return Ok(INVALID),
        };
        e.cpu.write_u32(c, ptr).map_err(|e| e.to_string())?;
        return Ok(0);
    }
    if let Kind::Device {
        initialized,
        version8,
    } = kind
    {
        if name == "Initialize" {
            if initialized {
                return Ok(0x88780082);
            }
            if !e.ext.get::<Sound>().enabled || b != 0 {
                return Ok(0x88780078);
            }
            com.get_mut::<Object>(&mut e.sys.objects, a).unwrap().kind = Kind::Device {
                initialized: true,
                version8,
            };
            return Ok(0);
        }
        if !initialized {
            return Ok(0x887800aa);
        }
        return match name {
            "CreateSoundBuffer" => {
                if e.arg(3) != 0 {
                    return Ok(0x80040110);
                }
                check_range(e.cpu, b, 4, Prot::READ).map_err(|error| error.to_string())?;
                let size = e.cpu.read_u32(b).map_err(|e| e.to_string())?;
                if !matches!(size, 20 | 36) {
                    e.cpu.write_u32(c, 0).map_err(|e| e.to_string())?;
                    return Ok(INVALID);
                }
                check_range(e.cpu, b, size, Prot::READ).map_err(|error| error.to_string())?;
                let flags = e.cpu.read_u32(b + 4).map_err(|e| e.to_string())?;
                let fx = flags & CTRL_FX != 0;
                check_range(e.cpu, c, 4, Prot::WRITE).map_err(|error| error.to_string())?;
                if !fx {
                    e.cpu.write_u32(c, 0).map_err(|e| e.to_string())?;
                }
                // Software PCM supports 3D and an empty FX chain, without
                // hardware voices or effect-DSP constructors.
                let unsupported = flags
                    & !(0x1
                        | 0x2
                        | 0x8
                        | CTRL_3D
                        | 0x20
                        | 0x40
                        | 0x80
                        | 0x100
                        | CTRL_FX
                        | 0x4000
                        | 0x8000
                        | 0x10000
                        | MUTE_3D_AT_MAX_DISTANCE
                        | LOC_DEFER
                        | 0x80000);
                if flags & LOC_DEFER != 0 && flags & 0xc != 0 {
                    return Ok(INVALID); // Deferred and fixed allocation are mutually exclusive.
                }
                if flags & (CTRL_3D | MUTE_3D_AT_MAX_DISTANCE) == MUTE_3D_AT_MAX_DISTANCE {
                    return Ok(INVALID);
                }
                if unsupported != 0 {
                    adv_abi::warn_unavailable(
                        e,
                        &format!("dsound.dll!{full}"),
                        &format!(
                            "software PCM buffer flags are unsupported: flags=0x{flags:08x}, unsupported_mask=0x{unsupported:08x}; hardware voices are unavailable (HRESULT=0x80004001)"
                        ),
                    );
                    return Ok(UNSUPPORTED);
                }
                let len = e.cpu.read_u32(b + 8).map_err(|e| e.to_string())?;
                let fp = e.cpu.read_u32(b + 16).map_err(|e| e.to_string())?;
                let primary = flags & 1 != 0;
                if fx && (!version8 || primary) {
                    return Ok(if !version8 { 0x887800be } else { INVALID });
                }
                if fx {
                    check_range(e.cpu, fp, 16, Prot::READ).map_err(|error| error.to_string())?;
                }
                let format = if primary && fp == 0 {
                    Format::default()
                } else {
                    match Format::read(e.cpu, fp) {
                        Ok(f) => f,
                        Err(_) => return Ok(0x88780064),
                    }
                };
                // DirectSound 8 takes 3D sources in mono and without pan
                // control; earlier versions take either.
                if version8
                    && !primary
                    && flags & CTRL_3D != 0
                    && (format.channels != 1 || flags & CTRL_PAN != 0)
                {
                    return Ok(INVALID);
                }
                if (!primary
                    && (len == 0 || len > 32 * 1024 * 1024 || !len.is_multiple_of(format.align())))
                    || (primary && len != 0)
                {
                    return Ok(INVALID);
                }
                if fx {
                    if e.cpu.read_u32(fp + 8).map_err(|error| error.to_string())?
                        != format.rate * format.align()
                    {
                        return Ok(0x88780064); // DSERR_BADFORMAT, no publication.
                    }
                    if format.bits != 16 {
                        adv_abi::warn_unavailable(
                            e,
                            &format!("dsound.dll!{full}"),
                            &format!(
                                "CTRLFX empty-chain profile requires 16-bit PCM: flags=0x{flags:08x}, bytes={len}, rate={}, channels={}, bits={} (HRESULT=0x80004001)",
                                format.rate, format.channels, format.bits,
                            ),
                        );
                        return Ok(UNSUPPORTED);
                    }
                    if u64::from(len) * 1000
                        < u64::from(format.rate) * u64::from(format.align()) * 150
                    {
                        return Ok(0x887800b4); // DSERR_BUFFERTOOSMALL; preserve output.
                    }
                    e.cpu.write_u32(c, 0).map_err(|e| e.to_string())?;
                }
                let audio_id = e.ext.get::<Sound>().next_audio_id;
                let Some(next_audio_id) = e.ext.get::<Sound>().next_source_id() else {
                    return Ok(OUT_OF_MEMORY);
                };
                let Some(ptr) = object(
                    e,
                    Kind::Buffer,
                    flags & 0x100 != 0,
                    spatial_interface(flags),
                )?
                else {
                    return Ok(OUT_OF_MEMORY);
                };
                let (base, region) = if primary {
                    (0, None)
                } else {
                    let mapped_len = adv_machine::vm::align_up(len, 4096);
                    let Some(base) = e.sys.vm.reserve(mapped_len) else {
                        release(e, ptr);
                        return Ok(OUT_OF_MEMORY);
                    };
                    let region = match e.cpu.map_shared_audio(base, mapped_len) {
                        Ok(region) => region,
                        Err(error) => {
                            let _ = e.sys.vm.free(e.cpu, base);
                            release(e, ptr);
                            return Err(format!("shared audio mapping: {error}"));
                        }
                    };
                    (base, Some(region))
                };
                if !primary && format.bits == 8 {
                    let silence = vec![128; len as usize];
                    if let Err(error) = e.cpu.write(base, &silence) {
                        e.sys
                            .vm
                            .free(e.cpu, base)
                            .map_err(|error| error.to_string())?;
                        release(e, ptr);
                        return Err(error.to_string());
                    }
                }
                let s = e.ext.get_mut::<Sound>();
                let voice = region
                    .as_ref()
                    .map(|region| {
                        s.audio.add_ring(
                            audio_id,
                            RingConfig {
                                view: region.read_alias.clone(),
                                bytes: len,
                                channels: format.channels,
                                bits: format.bits,
                                frequency: format.rate,
                                output_rate: 44100,
                                notifications: Vec::new(),
                            },
                        )
                    })
                    .transpose();
                let voice = match voice {
                    Ok(voice) => voice,
                    Err(error) => {
                        if let Some(trace) = s.trace.as_mut() {
                            let _ = writeln!(trace, "create-buffer rejected: {error}");
                        }
                        // add_ring publishes nothing on failure, so there is no
                        // callback-owned alias requiring deferred retirement.
                        if base != 0 {
                            e.sys
                                .vm
                                .free(e.cpu, base)
                                .map_err(|error| error.to_string())?;
                        }
                        release(e, ptr);
                        return Ok(OUT_OF_MEMORY);
                    }
                };
                s.next_audio_id = next_audio_id;
                let sample_storage = region.map(|region| {
                    let storage_id = s.next_storage_id;
                    s.next_storage_id += 1;
                    s.storage_records.insert(
                        storage_id,
                        StorageRecord {
                            addr: base,
                            live_buffers: 1,
                            pending_voices: std::collections::BTreeSet::new(),
                        },
                    );
                    SampleStorage {
                        id: storage_id,
                        addr: base,
                        len,
                        read_alias: region.read_alias,
                    }
                });
                let id = com.identity::<Object>(&e.sys.objects, ptr).unwrap();
                com.get_identity_mut(&mut e.sys.objects, id).unwrap().buffer = Some(Buffer {
                    audio_id,
                    voice,
                    playback_clock: s.playback_clock.clone(),
                    generation: 0,
                    storage: sample_storage,
                    len,
                    flags,
                    format,
                    frequency: format.rate,
                    volume: 0,
                    pan: 0,
                    spatial: (!primary && flags & CTRL_3D != 0)
                        .then(|| Deferred::new(Source::new(format.rate))),
                    playing: false,
                    looping: false,
                    retuned: false,
                    min_lead_bytes: 0,
                    base: 0,
                    anchor: now,
                    queued_frames: 0,
                    notice_bytes: 0,
                    notifications: Vec::new(),
                    lock: None,
                    trace_pcm: None,
                    trace_generation: 0,
                    trace_chunks: None,
                });
                let buffer = com
                    .get_identity(&e.sys.objects, id)
                    .and_then(|object| object.buffer.as_ref())
                    .unwrap();
                if buffer.spatial.is_some() {
                    s.audio
                        .set_stereo_gain(audio_id, mix(&s.listener.applied, buffer).0);
                }
                s.buffer_ids.insert(audio_id, id);
                e.cpu.write_u32(c, ptr).map_err(|e| e.to_string())?;
                Ok(0)
            }
            "DuplicateSoundBuffer" => {
                if c == 0 {
                    return Ok(INVALID);
                }
                e.cpu.write_u32(c, 0).map_err(|e| e.to_string())?;
                if b == 0 {
                    return Ok(INVALID);
                }
                let Some(orig_id) = com.identity::<Object>(&e.sys.objects, b) else {
                    return Ok(INVALID);
                };
                let (
                    orig_storage,
                    orig_len,
                    orig_flags,
                    orig_format,
                    orig_vol,
                    orig_pan,
                    orig_freq,
                    orig_spatial,
                ) = match com
                    .get_identity(&e.sys.objects, orig_id)
                    .and_then(|o| o.buffer.as_ref())
                {
                    Some(buf) => (
                        buf.storage.clone(),
                        buf.len,
                        buf.flags,
                        buf.format,
                        buf.volume,
                        buf.pan,
                        buf.frequency,
                        buf.spatial.map(|source| Deferred::new(source.applied)),
                    ),
                    None => return Ok(INVALID),
                };
                if orig_flags & (1 | CTRL_FX) != 0 {
                    // Cannot duplicate primary or effects-capable buffers.
                    return Ok(INVALID_CALL);
                }
                let Some(sample_storage) = orig_storage else {
                    return Ok(INVALID_CALL);
                };
                let audio_id = e.ext.get::<Sound>().next_audio_id;
                let Some(next_audio_id) = e.ext.get::<Sound>().next_source_id() else {
                    return Ok(OUT_OF_MEMORY);
                };
                let Some(ptr) = object(
                    e,
                    Kind::Buffer,
                    orig_flags & 0x100 != 0,
                    spatial_interface(orig_flags),
                )?
                else {
                    return Ok(OUT_OF_MEMORY);
                };
                let s = e.ext.get_mut::<Sound>();
                let voice = match s.audio.add_ring(
                    audio_id,
                    RingConfig {
                        view: sample_storage.read_alias.clone(),
                        bytes: orig_len,
                        channels: orig_format.channels,
                        bits: orig_format.bits,
                        frequency: orig_format.rate,
                        output_rate: 44100,
                        notifications: Vec::new(),
                    },
                ) {
                    Ok(voice) => voice,
                    Err(error) => {
                        if let Some(trace) = s.trace.as_mut() {
                            let _ = writeln!(trace, "duplicate-buffer rejected: {error}");
                        }
                        release(e, ptr);
                        return Ok(OUT_OF_MEMORY);
                    }
                };
                s.next_audio_id = next_audio_id;
                if let Some(record) = s.storage_records.get_mut(&sample_storage.id) {
                    record.live_buffers += 1;
                }
                let id = com.identity::<Object>(&e.sys.objects, ptr).unwrap();
                let dup_buffer = Buffer {
                    audio_id,
                    voice: Some(voice),
                    playback_clock: s.playback_clock.clone(),
                    generation: 0,
                    storage: Some(sample_storage),
                    len: orig_len,
                    flags: orig_flags,
                    format: orig_format,
                    frequency: orig_freq,
                    volume: orig_vol,
                    pan: orig_pan,
                    spatial: orig_spatial,
                    playing: false,
                    looping: false,
                    retuned: false,
                    min_lead_bytes: 0,
                    base: 0,
                    anchor: now,
                    queued_frames: 0,
                    notice_bytes: 0,
                    notifications: Vec::new(),
                    lock: None,
                    trace_pcm: None,
                    trace_generation: 0,
                    trace_chunks: None,
                };
                s.audio
                    .set_stereo_gain(audio_id, mix(&s.listener.applied, &dup_buffer).0);
                if orig_freq != orig_format.rate {
                    s.audio.set_frequency(audio_id, orig_freq)?;
                }
                com.get_identity_mut(&mut e.sys.objects, id).unwrap().buffer = Some(dup_buffer);
                s.buffer_ids.insert(audio_id, id);
                e.cpu.write_u32(c, ptr).map_err(|e| e.to_string())?;
                Ok(0)
            }
            "GetCaps" => {
                if e.cpu.read_u32(b).map_err(|e| e.to_string())? < 96 {
                    return Ok(INVALID);
                }
                let mut caps = [0u8; 96];
                for (off, value) in [(0, 96u32), (4, 0xf3f), (8, 100), (12, 200000), (16, 1)] {
                    caps[off..off + 4].copy_from_slice(&value.to_le_bytes());
                }
                e.cpu.write(b, &caps).map_err(|e| e.to_string())?;
                Ok(0)
            }
            "SetCooperativeLevel" => Ok(if (1..=4).contains(&c) { 0 } else { INVALID }),
            "GetSpeakerConfig" => {
                e.cpu.write_u32(b, 4).map_err(|e| e.to_string())?;
                Ok(0)
            }
            "SetSpeakerConfig" => Ok(if b & 0xff == 4 { 0 } else { UNSUPPORTED }),
            "Compact" => Ok(0),
            "VerifyCertification" => {
                e.cpu.write_u32(b, 1).map_err(|e| e.to_string())?;
                Ok(0)
            }
            _ => Err(format!("unimplemented audio method {full}")),
        };
    }
    if let Kind::Notify(parent) = kind {
        if name != "SetNotificationPositions" {
            return Err(format!("unimplemented audio method {full}"));
        }
        if b > 1024 {
            return Ok(INVALID);
        }
        let buffer = e.ext.get::<Sound>().buffer(&e.sys.objects, parent).unwrap();
        let len = buffer.len;
        if buffer.playing {
            return Ok(INVALID_CALL);
        }
        let mut entries = Vec::new();
        for i in 0..b {
            let offset = e.cpu.read_u32(c + i * 8).map_err(|e| e.to_string())?;
            let handle = e.cpu.read_u32(c + i * 8 + 4).map_err(|e| e.to_string())?;
            let event = match e.ext.get::<Sound>().host.events {
                Some(events) => (events.valid)(e, handle),
                None => matches!(
                    e.sys.objects.get(handle),
                    Some(adv_machine::Object::Event { .. })
                ),
            };
            if (offset != u32::MAX && offset >= len) || !event {
                return Ok(INVALID);
            }
            entries.push((offset, handle));
        }
        let buffer = com
            .get_mut::<Object>(&mut e.sys.objects, parent)
            .unwrap()
            .buffer
            .as_mut()
            .unwrap();
        buffer.notifications = entries;
        let s = e.ext.get_mut::<Sound>();
        s.audio
            .set_notifications(buffer.audio_id, buffer.notifications.clone());
        if let Some(trace) = s.trace.as_mut() {
            let _ = writeln!(
                trace,
                "notifications us={now} id={parent:08x} entries={:?}",
                buffer.notifications
            );
        }
        return Ok(0);
    }
    if let Kind::Spatial(parent) = kind {
        return spatial_call(e, name, parent, now);
    }
    if matches!(name, "SetFX" | "GetObjectInPath") {
        return empty_fx(e, a, name);
    }
    if name == "AcquireResources" {
        let flags = com
            .get::<Object>(&e.sys.objects, a)
            .unwrap()
            .buffer
            .as_ref()
            .unwrap()
            .flags;
        if flags & LOC_DEFER == 0 {
            return Ok(INVALID_CALL);
        }
        if b & !6 != 0 || b == 6 || c != 0 {
            return Ok(INVALID);
        }
        if b == 2 {
            adv_abi::warn_unavailable(
                e,
                "dsound.dll!IDirectSoundBuffer8::AcquireResources",
                "deferred hardware voices are unavailable",
            );
            return Ok(INVALID_CALL);
        }
        // The software PCM backing is already reserved and the FX chain is
        // empty. No hardware/DSP resource is claimed and nothing starts playing.
        return Ok(0);
    }
    if name == "Play" {
        let flags = com
            .get::<Object>(&e.sys.objects, a)
            .unwrap()
            .buffer
            .as_ref()
            .unwrap()
            .flags;
        let allocation = e.arg(3) & !1;
        if b != 0
            || (flags & LOC_DEFER == 0 && (c != 0 || allocation != 0))
            || !matches!(allocation, 0 | 2 | 4)
        {
            return Ok(INVALID);
        }
        if allocation == 2 {
            adv_abi::warn_unavailable(
                e,
                "dsound.dll!IDirectSoundBuffer8::Play",
                "deferred hardware voices are unavailable",
            );
            return Ok(INVALID_CALL);
        }
    }
    let args: [u32; 8] = std::array::from_fn(|i| e.arg(i as u32));
    let s = e.ext.get_mut::<Sound>();
    let buf = com
        .get_mut::<Object>(&mut e.sys.objects, a)
        .unwrap()
        .buffer
        .as_mut()
        .unwrap();
    let value = match name {
        "GetCaps" => {
            if e.cpu.read_u32(b).map_err(|e| e.to_string())? < 20 {
                return Ok(INVALID);
            }
            let flags = if buf.flags & LOC_DEFER != 0 {
                buf.flags
            } else {
                buf.flags | 8
            };
            for (i, v) in [20, flags, buf.len, 0, 0].into_iter().enumerate() {
                e.cpu
                    .write_u32(b + i as u32 * 4, v)
                    .map_err(|e| e.to_string())?;
            }
            0
        }
        "GetCurrentPosition" => {
            let (play, write) = buf.cursors(now, s.audio.device_connected());
            if b != 0 {
                e.cpu.write_u32(b, play).map_err(|e| e.to_string())?;
            }
            if c != 0 {
                e.cpu.write_u32(c, write).map_err(|e| e.to_string())?;
            }
            0
        }
        "GetFormat" => {
            if args[3] != 0 {
                e.cpu.write_u32(args[3], 18).map_err(|e| e.to_string())?;
            }
            if b != 0 {
                if c < 18 {
                    return Ok(INVALID);
                }
                e.cpu
                    .write(b, &buf.format.bytes())
                    .map_err(|e| e.to_string())?;
            }
            0
        }
        "GetVolume" | "GetPan" | "GetFrequency" | "GetStatus" => {
            let value = match name {
                "GetVolume" => buf.volume as u32,
                "GetPan" => buf.pan as u32,
                "GetFrequency" => buf
                    .spatial
                    .map_or(buf.frequency, |source| source.applied.frequency),
                _ => {
                    u32::from(
                        buf.playing
                            && (buf.len == 0
                                || buf.looping
                                || buf.progress(now) < u64::from(buf.len)),
                    ) | if buf.playing && buf.looping { 4 } else { 0 }
                }
            };
            e.cpu.write_u32(b, value).map_err(|e| e.to_string())?;
            0
        }
        "SetVolume" => {
            if !(-10000..=0).contains(&(b as i32)) {
                return Ok(INVALID);
            }
            buf.volume = b as i32;
            spatialize(&s.audio, &s.listener.applied, buf, now);
            0
        }
        "SetPan" => {
            if !(-10000..=10000).contains(&(b as i32)) {
                return Ok(INVALID);
            }
            buf.pan = b as i32;
            spatialize(&s.audio, &s.listener.applied, buf, now);
            0
        }
        "SetFrequency" => {
            let freq = if b == 0 { buf.format.rate } else { b };
            if !(100..=200000).contains(&freq) {
                return Ok(INVALID);
            }
            // A 3D source keeps the requested rate and plays it Doppler-shifted.
            let rate = match buf.spatial.as_mut() {
                Some(source) => {
                    source.set(spatial::IMMEDIATE, |source| source.frequency = freq);
                    mix(&s.listener.applied, buf).1
                }
                None => freq,
            };
            if (buf.spatial.is_none() || rate != buf.frequency) && !retune(&s.audio, buf, rate, now)
            {
                return Ok(INVALID_CALL);
            }
            0
        }
        "SetFormat" => {
            if buf.flags & 1 == 0 {
                return Ok(INVALID_CALL);
            }
            let Ok(format) = Format::read(e.cpu, b) else {
                return Ok(0x88780064);
            };
            buf.format = format;
            buf.frequency = format.rate;
            0
        }
        "SetCurrentPosition" => {
            if b >= buf.len {
                return Ok(INVALID);
            }
            let base = b / buf.format.align() * buf.format.align();
            let generation = buf.generation.wrapping_add(1);
            if !s.audio.seek_ring(buf.audio_id, base, generation) {
                return Ok(INVALID_CALL);
            }
            buf.base = base;
            buf.anchor = now;
            buf.retuned = false;
            buf.min_lead_bytes = 0;
            buf.queued_frames = 0;
            buf.notice_bytes = u64::from(buf.base);
            buf.generation = generation;
            0
        }
        "Play" => {
            let ended = !buf.looping && buf.len != 0 && buf.progress(now) >= u64::from(buf.len);
            let starting = !buf.playing || ended;
            let generation = if starting {
                buf.generation.wrapping_add(1)
            } else {
                buf.generation
            };
            let base = if ended { 0 } else { buf.base };
            if starting {
                // A fresh start is a new playback generation. A naturally ended
                // buffer can retain unconsumed PCM in the device latency window;
                // it must not survive reuse of the same DirectSound object.
                if let Some(trace) = s.trace.as_mut() {
                    let mut bytes = vec![0; buf.len as usize];
                    if buf.len != 0 {
                        e.cpu
                            .read(buf.storage_addr(), &mut bytes)
                            .map_err(|error| error.to_string())?;
                    }
                    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
                        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
                    });
                    buf.trace_generation += 1;
                    let generation = buf.trace_generation;
                    let _ = writeln!(
                        trace,
                        "start us={now} id={a:08x} generation={generation} storage={:08x} bytes={} hash={hash:016x} base={} loop={} rate={} channels={} bits={}",
                        buf.storage_addr(),
                        buf.len,
                        base,
                        args[3] & 1,
                        buf.frequency,
                        buf.format.channels,
                        buf.format.bits
                    );
                    if buf.len != 0
                        && let Some(dir) = s.trace_pcm_dir.as_ref()
                    {
                        let path = dir.join(format!("{now}-{a:08x}-{generation}-{hash:016x}.pcm"));
                        std::fs::write(&path, &bytes).map_err(|error| {
                            format!("cannot write audio diagnostic {path:?}: {error}")
                        })?;
                        let _ = writeln!(
                            trace,
                            "dump us={now} id={a:08x} generation={generation} path={path:?}"
                        );
                        if s.trace_pcm_chunks {
                            let path = dir.join(format!("chunks-{now}-{a:08x}-{generation}.pcm"));
                            let file = std::fs::File::create(&path).map_err(|error| {
                                format!("cannot write audio chunks {path:?}: {error}")
                            })?;
                            buf.trace_chunks = Some(ChunkDump {
                                writer: std::io::BufWriter::new(file),
                                path,
                                bytes: 0,
                            });
                        }
                    }
                    buf.trace_pcm = Some(bytes);
                    let _ = trace.flush();
                }
                if s.capture_digests && s.digests.len() < 4 && buf.len != 0 {
                    let mut bytes = vec![0; buf.len as usize];
                    e.cpu
                        .read(buf.storage_addr(), &mut bytes)
                        .map_err(|error| error.to_string())?;
                    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
                        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
                    });
                    s.digests.push(PcmDigest {
                        sample_rate: buf.format.rate,
                        channels: buf.format.channels,
                        bits_per_sample: buf.format.bits,
                        bytes: buf.len,
                        fnv1a64: hash,
                    });
                }
            }
            let looping = args[3] & 1 != 0;
            if !s
                .audio
                .play_ring_at(buf.audio_id, looping, generation, ended.then_some(0))
            {
                return Ok(INVALID_CALL);
            }
            if starting {
                buf.base = base;
                buf.anchor = now;
                buf.retuned = false;
                buf.min_lead_bytes = 0;
                buf.queued_frames = 0;
                buf.notice_bytes = u64::from(base);
            }
            buf.generation = generation;
            buf.playing = true;
            buf.looping = looping;
            0
        }
        "Stop" => {
            let generation = buf.generation.wrapping_add(1);
            if !s.audio.stop_ring_generation(buf.audio_id, generation) {
                return Ok(INVALID_CALL);
            }
            buf.stop(now, s.audio.device_connected());
            buf.generation = generation;
            for &(offset, handle) in &buf.notifications {
                if offset == u32::MAX {
                    s.ready_notices.push(handle);
                }
            }
            0
        }
        "Restore" => 0,
        "Initialize" => 0x88780082,
        "Lock" => {
            if buf.len == 0 {
                return Ok(INVALID_CALL);
            }
            if buf.lock.is_some() {
                return Ok(INVALID);
            }
            if args[3] == 0 || args[4] == 0 {
                return Ok(INVALID);
            }
            let flags = args[7];
            if flags & !3 != 0 {
                return Ok(INVALID);
            }
            let offset = if flags & 1 != 0 {
                buf.cursors(now, s.audio.device_connected()).1
            } else {
                b
            };
            let (offset, len) = if flags & 2 != 0 {
                (0, buf.len)
            } else {
                (offset, c)
            };
            if offset >= buf.len || len == 0 || len > buf.len {
                return Ok(INVALID);
            }
            let n1 = len.min(buf.len - offset);
            let n2 = if args[5] == 0 { 0 } else { len - n1 };
            if n2 != 0 && args[6] == 0 {
                return Ok(INVALID);
            }
            let Some(storage) = buf.storage.as_ref() else {
                return Ok(INVALID_CALL);
            };
            let lock = Lock {
                p1: storage.addr + offset,
                n1,
                p2: if n2 != 0 { storage.addr } else { 0 },
                n2,
            };
            for (ptr, value) in [
                (args[3], lock.p1),
                (args[4], lock.n1),
                (args[5], lock.p2),
                (args[6], lock.n2),
            ] {
                if ptr != 0 {
                    e.cpu.write_u32(ptr, value).map_err(|e| e.to_string())?;
                } else if ptr == args[3] || ptr == args[4] {
                    return Ok(INVALID);
                }
            }
            buf.lock = Some(lock);
            let storage_id = storage.id;
            let sharing_audio_ids: Vec<u64> = s
                .buffer_ids
                .values()
                .filter_map(|&ident| {
                    let other = s.com.get_identity(&e.sys.objects, ident)?.buffer.as_ref()?;
                    (other.storage.as_ref()?.id == storage_id).then_some(other.audio_id)
                })
                .collect();
            for audio_id in sharing_audio_ids {
                s.audio.written(audio_id, offset, n1 + n2);
            }
            0
        }
        "Unlock" => {
            if !buf.lock.is_some_and(|lock| {
                lock.p1 == b && lock.p2 == args[3] && c <= lock.n1 && args[4] <= lock.n2
            }) {
                return Ok(INVALID);
            }
            let storage_addr = buf.storage_addr();
            let storage_id = buf.storage.as_ref().map(|storage| storage.id);
            if let Some(lock) = buf.lock.take() {
                if let Some(dump) = buf.trace_chunks.as_mut() {
                    let file_offset = dump.bytes;
                    for (address, len) in [(lock.p1, c), (lock.p2, args[4])] {
                        if len != 0 {
                            let mut bytes = vec![0; len as usize];
                            e.cpu
                                .read(address, &mut bytes)
                                .map_err(|error| error.to_string())?;
                            dump.writer
                                .write_all(&bytes)
                                .map_err(|error| error.to_string())?;
                            dump.bytes += u64::from(len);
                        }
                    }
                    if let Some(trace) = s.trace.as_mut() {
                        let _ = writeln!(
                            trace,
                            "dma-write us={now} id={a:08x} generation={} byte_offset={} count={} file_offset={file_offset} path={:?}",
                            buf.trace_generation,
                            lock.p1 - storage_addr,
                            c + args[4],
                            dump.path
                        );
                    }
                }
                if let Some(storage_id) = storage_id {
                    let sharing_audio_ids: Vec<u64> = s
                        .buffer_ids
                        .values()
                        .filter_map(|&ident| {
                            let other =
                                s.com.get_identity(&e.sys.objects, ident)?.buffer.as_ref()?;
                            (other.storage.as_ref()?.id == storage_id).then_some(other.audio_id)
                        })
                        .collect();
                    for audio_id in sharing_audio_ids {
                        if c != 0 {
                            s.audio.committed(audio_id, lock.p1 - storage_addr, c);
                        }
                        if args[4] != 0 {
                            s.audio.committed(audio_id, lock.p2 - storage_addr, args[4]);
                        }
                    }
                }
            }
            0
        }
        _ => return Err(format!("unimplemented audio method {full}")),
    };
    let ready = std::mem::take(&mut s.ready_notices);
    notify(e, ready)?;
    Ok(value)
}
/// A deferred change to a buffer's 3D parameters.
type SourceUpdate = Box<dyn Fn(&mut Source)>;

/// `IDirectSound3DListener` on a primary buffer and `IDirectSound3DBuffer`
/// on a secondary one; `parent` is the buffer's own interface.
fn spatial_call(e: &mut Env<'_>, name: &str, parent: u32, now: u64) -> Result<u32, String> {
    let args: [u32; 8] = std::array::from_fn(|i| e.arg(i as u32));
    let float = |i: usize| f32::from_bits(args[i]);
    let vector = |i: usize| Vector::from_words([args[i], args[i + 1], args[i + 2]]);
    let com = e.ext.get::<Sound>().com;
    let Some(id) = com.identity::<Object>(&e.sys.objects, parent) else {
        return Ok(INVALID);
    };
    let primary = com
        .get_identity(&e.sys.objects, id)
        .and_then(|object| object.buffer.as_ref())
        .is_some_and(|buffer| buffer.flags & 1 != 0);
    // Getters write one value to the first argument's pointer.
    let out = args[1];
    if name.starts_with("Get") && (out == 0 || (name == "GetConeAngles" && args[2] == 0)) {
        return Ok(INVALID);
    }
    // Setters end with the apply flag.
    let apply = match name {
        "SetOrientation" => Some(args[7]),
        "SetPosition" | "SetVelocity" | "SetConeOrientation" => Some(args[4]),
        "SetConeAngles" => Some(args[3]),
        _ if name.starts_with("Set") => Some(args[2]),
        _ => None,
    };
    if apply.is_some_and(|apply| apply > spatial::DEFERRED) {
        return Ok(INVALID);
    }
    let apply = apply.unwrap_or(spatial::IMMEDIATE);
    let write_float = |cpu: &mut dyn Guest, value: f32| {
        cpu.write_u32(out, value.to_bits())
            .map_err(|error| error.to_string())
    };
    let valid_size = |cpu: &dyn Guest, at: u32| {
        at != 0
            && cpu
                .read_u32(at)
                .is_ok_and(|size| size == spatial::PARAMETERS)
    };

    if primary {
        let listener = e.ext.get::<Sound>().listener.applied;
        let changed = |e: &mut Env<'_>, change: &dyn Fn(&mut Listener)| {
            e.ext.get_mut::<Sound>().listener.set(apply, change)
        };
        let applied = match name {
            "GetAllParameters" => {
                if !valid_size(e.cpu, out) {
                    return Ok(INVALID);
                }
                listener.write(e.cpu, out)?;
                false
            }
            "GetDistanceFactor" => write_float(e.cpu, listener.distance_factor).map(|()| false)?,
            "GetDopplerFactor" => write_float(e.cpu, listener.doppler_factor).map(|()| false)?,
            "GetRolloffFactor" => write_float(e.cpu, listener.rolloff_factor).map(|()| false)?,
            "GetOrientation" => {
                if args[2] == 0 {
                    return Ok(INVALID);
                }
                listener.front.write(e.cpu, out)?;
                listener.top.write(e.cpu, args[2])?;
                false
            }
            "GetPosition" => listener.position.write(e.cpu, out).map(|()| false)?,
            "GetVelocity" => listener.velocity.write(e.cpu, out).map(|()| false)?,
            "SetAllParameters" => {
                if !valid_size(e.cpu, out) {
                    return Ok(INVALID);
                }
                let Some(new) = Listener::read(e.cpu, out)?.checked() else {
                    return Ok(INVALID);
                };
                changed(e, &|listener| *listener = new)
            }
            "SetDistanceFactor" => {
                let factor = float(1);
                if factor.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
                    return Ok(INVALID);
                }
                changed(e, &|listener| listener.distance_factor = factor)
            }
            "SetDopplerFactor" | "SetRolloffFactor" => {
                let factor = float(1);
                if !(0.0..=10.0).contains(&factor) {
                    return Ok(INVALID);
                }
                if name == "SetDopplerFactor" {
                    changed(e, &|listener| listener.doppler_factor = factor)
                } else {
                    changed(e, &|listener| listener.rolloff_factor = factor)
                }
            }
            "SetOrientation" => {
                let Some((front, top)) = spatial::orientation(vector(1), vector(4)) else {
                    return Ok(INVALID);
                };
                changed(e, &|listener| (listener.front, listener.top) = (front, top))
            }
            "SetPosition" => {
                let position = vector(1);
                changed(e, &|listener| listener.position = position)
            }
            "SetVelocity" => {
                let velocity = vector(1);
                changed(e, &|listener| listener.velocity = velocity)
            }
            "CommitDeferredSettings" => {
                let s = e.ext.get_mut::<Sound>();
                s.listener.commit();
                for id in s.buffer_ids.values().copied().collect::<Vec<_>>() {
                    if let Some(source) = com
                        .get_identity_mut(&mut e.sys.objects, id)
                        .and_then(|object| object.buffer.as_mut())
                        .and_then(|buffer| buffer.spatial.as_mut())
                    {
                        source.commit();
                    }
                }
                true
            }
            _ => {
                return Err(format!(
                    "unimplemented audio method IDirectSound3DListener::{name}"
                ));
            }
        };
        if applied {
            let s = e.ext.get_mut::<Sound>();
            for id in s.buffer_ids.values().copied().collect::<Vec<_>>() {
                if let Some(buffer) = com
                    .get_identity_mut(&mut e.sys.objects, id)
                    .and_then(|object| object.buffer.as_mut())
                    .filter(|buffer| buffer.spatial.is_some())
                {
                    spatialize(&s.audio, &s.listener.applied, buffer, now);
                }
            }
        }
        return Ok(0);
    }

    let source = com
        .get_identity(&e.sys.objects, id)
        .and_then(|object| object.buffer.as_ref())
        .and_then(|buffer| buffer.spatial)
        .ok_or("3D control of a buffer without 3D processing")?;
    let current = source.applied;
    let mut update: Option<SourceUpdate> = None;
    match name {
        "GetAllParameters" => {
            if !valid_size(e.cpu, out) {
                return Ok(INVALID);
            }
            current.write(e.cpu, out)?;
        }
        "GetConeAngles" => {
            e.cpu
                .write_u32(out, current.inside_cone)
                .map_err(|e| e.to_string())?;
            e.cpu
                .write_u32(args[2], current.outside_cone)
                .map_err(|e| e.to_string())?;
        }
        "GetConeOrientation" => current.cone.write(e.cpu, out)?,
        "GetConeOutsideVolume" => e
            .cpu
            .write_u32(out, current.cone_outside_volume as u32)
            .map_err(|e| e.to_string())?,
        "GetMaxDistance" => write_float(e.cpu, current.max_distance)?,
        "GetMinDistance" => write_float(e.cpu, current.min_distance)?,
        "GetMode" => e
            .cpu
            .write_u32(out, current.mode)
            .map_err(|e| e.to_string())?,
        "GetPosition" => current.position.write(e.cpu, out)?,
        "GetVelocity" => current.velocity.write(e.cpu, out)?,
        "SetAllParameters" => {
            if !valid_size(e.cpu, out) {
                return Ok(INVALID);
            }
            let new = Source::read(e.cpu, out, current.frequency)?;
            update = Some(Box::new(move |source| {
                *source = Source {
                    frequency: source.frequency,
                    ..new
                }
            }));
        }
        "SetConeAngles" => {
            let (inside, outside) = (args[1], args[2]);
            update = Some(Box::new(move |source| {
                (source.inside_cone, source.outside_cone) = (inside, outside)
            }));
        }
        "SetConeOrientation" => {
            let cone = vector(1);
            update = Some(Box::new(move |source| source.cone = cone));
        }
        "SetConeOutsideVolume" => {
            let volume = args[1] as i32;
            update = Some(Box::new(move |source| source.cone_outside_volume = volume));
        }
        "SetMaxDistance" => {
            let distance = float(1);
            update = Some(Box::new(move |source| source.max_distance = distance));
        }
        "SetMinDistance" => {
            let distance = float(1);
            update = Some(Box::new(move |source| source.min_distance = distance));
        }
        "SetMode" => {
            let mode = args[1];
            update = Some(Box::new(move |source| source.mode = mode));
        }
        "SetPosition" => {
            let position = vector(1);
            update = Some(Box::new(move |source| source.position = position));
        }
        "SetVelocity" => {
            let velocity = vector(1);
            update = Some(Box::new(move |source| source.velocity = velocity));
        }
        _ => {
            return Err(format!(
                "unimplemented audio method IDirectSound3DBuffer::{name}"
            ));
        }
    }
    let Some(update) = update else {
        return Ok(0);
    };
    // Each setter checks the parameters as they would be after it.
    let mut checked = current;
    update(&mut checked);
    if !checked.valid() {
        return Ok(INVALID);
    }
    let s = e.ext.get_mut::<Sound>();
    let buffer = com
        .get_identity_mut(&mut e.sys.objects, id)
        .and_then(|object| object.buffer.as_mut())
        .unwrap();
    if buffer.spatial.as_mut().unwrap().set(apply, update) {
        spatialize(&s.audio, &s.listener.applied, buffer, now);
    }
    Ok(0)
}
/// The stereo gains and playback rate of `buf`: its 3D output, or its
/// volume and pan when it has no 3D processing.
fn mix(listener: &Listener, buf: &Buffer) -> ([f32; 2], u32) {
    let Some(source) = buf.spatial.as_ref() else {
        return (buf.stereo_gain(), buf.frequency);
    };
    let mute = buf.flags & MUTE_3D_AT_MAX_DISTANCE != 0;
    match spatial::output(listener, &source.applied, buf.volume, mute) {
        Some(output) => (output.gains, output.rate),
        None => (buf.stereo_gain(), source.applied.frequency),
    }
}
/// Sends `buf`'s gains and rate to the mixer. A rate change the mixer
/// refuses under backpressure keeps the previous rate.
fn spatialize(audio: &AudioControl, listener: &Listener, buf: &mut Buffer, now: u64) {
    let (gains, rate) = mix(listener, buf);
    audio.set_stereo_gain(buf.audio_id, gains);
    if rate != buf.frequency {
        retune(audio, buf, rate, now);
    }
}
/// Restarts `buf`'s playback clock at `rate` from its current position.
fn retune(audio: &AudioControl, buf: &mut Buffer, rate: u32, now: u64) -> bool {
    let connected = audio.device_connected();
    let (base, write) = buf.cursors(now, connected);
    let generation = buf.generation.wrapping_add(1);
    if audio
        .set_frequency_generation(buf.audio_id, rate, generation)
        .is_err()
    {
        return false;
    }
    if buf.playing && buf.len != 0 {
        buf.retuned = true;
        buf.min_lead_bytes =
            ((u64::from(write) + u64::from(buf.len) - u64::from(base)) % u64::from(buf.len)) as u32;
    } else {
        buf.retuned = false;
        buf.min_lead_bytes = 0;
    }
    buf.base = base;
    buf.anchor = now;
    buf.frequency = rate;
    buf.queued_frames = 0;
    buf.notice_bytes = u64::from(buf.base);
    buf.generation = generation;
    true
}
fn call(e: &mut Env<'_>) -> Outcome {
    match result(e) {
        Ok(v) => Outcome::Return(v),
        Err(err) => Outcome::Fail(err),
    }
}

/// Service device notifications and retirements. The callback owns a PCM read alias.
pub fn pump<B: Backend>(m: &mut Machine<B>) -> Result<Option<u64>, String> {
    let guest_now = m.sys.clock.now_us();
    let mut fired = Vec::new();
    let s = m.ext.get_mut::<Sound>();
    s.audio.sync_clock(guest_now);
    for (&source, (_, queued)) in &mut s.pending_release {
        if !*queued {
            *queued = s.audio.remove_pcm(source);
        }
    }
    (s.queued_samples, s.nonzero_samples) = s.audio.stream().rendered_samples();
    let now = s.playback_clock.now_us(guest_now);
    for event in s.audio.take_events_for_owner(s.owner) {
        match event {
            AudioEvent::Notification {
                source,
                generation,
                event,
                due_us,
            } => {
                if s.playback_clock.uses_device_time() {
                    s.pending_notices.push((source, event, due_us, generation));
                }
            }
            AudioEvent::Released { source } => {
                if let Some((storage_id, _)) = s.pending_release.remove(&source) {
                    let should_free = if let Some(record) = s.storage_records.get_mut(&storage_id) {
                        record.pending_voices.remove(&source);
                        record.live_buffers == 0 && record.pending_voices.is_empty()
                    } else {
                        false
                    };
                    if should_free && let Some(record) = s.storage_records.remove(&storage_id) {
                        m.sys
                            .vm
                            .free(&mut m.cpu, record.addr)
                            .map_err(|error| error.to_string())?;
                    }
                }
            }
        }
    }
    let mut next = None;
    if !s.playback_clock.uses_device_time() {
        for &identity in s.buffer_ids.values() {
            let buffer = s
                .com
                .get_identity_mut(&mut m.sys.objects, identity)
                .unwrap()
                .buffer
                .as_mut()
                .unwrap();
            if !buffer.playing || buffer.len == 0 {
                continue;
            }
            let progress = buffer.progress(now).min(if buffer.looping {
                u64::MAX
            } else {
                u64::from(buffer.len)
            });
            let total = u64::from(buffer.len);
            for &(offset, event) in &buffer.notifications {
                let signal = if offset == u32::MAX {
                    !buffer.looping && progress >= total
                } else {
                    let at = buffer.notice_bytes / total * total + u64::from(offset);
                    let at = if at < buffer.notice_bytes {
                        at + total
                    } else {
                        at
                    };
                    at < progress
                };
                if signal {
                    s.ready_notices.push(event);
                    next = Some(guest_now);
                }
            }
            buffer.notice_bytes = progress;
        }
        fired.append(&mut s.ready_notices);
    }
    s.pending_notices
        .retain(|&(source, event, due, generation)| {
            if !s
                .buffer_ids
                .get(&source)
                .and_then(|&id| s.com.get_identity(&m.sys.objects, id)?.buffer.as_ref())
                .is_some_and(|buffer| buffer.generation == generation)
            {
                return false;
            }
            if due <= now {
                fired.push(event);
                if let Some(trace) = s.trace.as_mut() {
                    let _ = writeln!(
                        trace,
                        "dma-notice us={now} source={source} event={event:08x} due={due}"
                    );
                }
                next = Some(guest_now);
                false
            } else {
                let deadline = guest_now + due - now;
                next = Some(next.map_or(deadline, |previous: u64| previous.min(deadline)));
                true
            }
        });
    for &identity in s.buffer_ids.values() {
        let buffer = s
            .com
            .get_identity_mut(&mut m.sys.objects, identity)
            .unwrap()
            .buffer
            .as_mut()
            .unwrap();
        if buffer.playing
            && !buffer.looping
            && buffer.len != 0
            && buffer.progress(now) >= u64::from(buffer.len)
        {
            buffer.playing = false;
            buffer.base = 0;
        }
    }
    // A headless virtual device needs only the next source boundary. The real
    // callback wakes execution when notifications/retirements are available.
    if !s.playback_clock.uses_device_time() {
        for buffer in s
            .buffers(&m.sys.objects)
            .filter(|buffer| buffer.playing && buffer.len != 0)
        {
            let progress = buffer.progress(now);
            let total = u64::from(buffer.len);
            let mut boundary = if buffer.looping { None } else { Some(total) };
            for &(offset, _) in &buffer.notifications {
                if offset == u32::MAX {
                    continue;
                }
                let at = progress / total * total + u64::from(offset);
                let at = if at <= progress { at + total } else { at };
                boundary = Some(boundary.map_or(at, |old| old.min(at)));
            }
            if let Some(boundary) = boundary {
                let frames = boundary.saturating_sub(progress) / u64::from(buffer.format.align());
                let delay = (frames * 1_000_000).div_ceil(u64::from(buffer.frequency));
                let deadline = guest_now + delay.max(1);
                next = Some(next.map_or(deadline, |old| old.min(deadline)));
            }
        }
    }
    let events = s.host.events;
    match events {
        Some(events) => m.visit_context(None, |env| {
            fired
                .into_iter()
                .try_for_each(|handle| (events.signal)(env, handle))
        })??,
        None => {
            for event in fired {
                m.sys
                    .set_event_with_priority(event, adv_machine::WakePriority::Audio);
            }
        }
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::JitCpu;
    use adv_cpu::backend::Reg;
    use adv_machine::{Clock, Object as WaitObject};
    mod deferred;
    mod empty_fx;
    const DATA: u32 = 0x500000;
    const STACK: u32 = 0x600000;

    fn machine() -> Machine<JitCpu> {
        machine_with_limits(adv_machine::ObjectLimits::default())
    }
    fn machine_with_limits(limits: adv_machine::ObjectLimits) -> Machine<JitCpu> {
        machine_for_process(limits, 0x1000, None)
    }
    fn machine_for_process(
        limits: adv_machine::ObjectLimits,
        pid: u32,
        audio: Option<AudioControl>,
    ) -> Machine<JitCpu> {
        let mut reg = Registry::default();
        crate::test_support::register(&mut reg);
        register(&mut reg);
        let mut m = Machine::new(
            JitCpu::new(adv_machine::layout::PARK_ADDR, true).unwrap(),
            reg,
            Clock::virtual_time(),
        )
        .unwrap();
        m.sys.set_process_identity(pid).unwrap();
        m.sys.objects = Objects::with_limits(limits);
        crate::test_support::install(&mut m);
        match audio {
            Some(audio) => install_with_audio(&mut m, audio).unwrap(),
            None => install(&mut m).unwrap(),
        }
        m.ext.get_mut::<Sound>().enabled = true;
        m.cpu.map(DATA, 4096, Prot::RW).unwrap();
        m.cpu.map(STACK, 4096, Prot::RW).unwrap();
        m
    }

    #[test]
    fn child_sound_uses_shared_output_with_independent_source_namespace() {
        let mut parent = machine();
        let audio = parent.ext.get::<Sound>().audio.clone();
        let mut child =
            machine_for_process(adv_machine::ObjectLimits::default(), 0x2000, Some(audio));
        assert!(Arc::ptr_eq(
            &parent.ext.get::<Sound>().playback_clock,
            &child.ext.get::<Sound>().playback_clock,
        ));
        let format = Format {
            rate: 1000,
            channels: 1,
            bits: 8,
        };
        let parent_buffer = buffer(&mut parent, format, 1000);
        let child_buffer = buffer(&mut child, format, 1000);
        let parent_source = parent
            .ext
            .get::<Sound>()
            .buffer(&parent.sys.objects, parent_buffer)
            .unwrap()
            .audio_id;
        let child_source = child
            .ext
            .get::<Sound>()
            .buffer(&child.sys.objects, child_buffer)
            .unwrap()
            .audio_id;
        assert_eq!(parent_source, (0x1000_u64 << 32) | 1);
        assert_eq!(child_source, (0x2000_u64 << 32) | 1);
        let parent_movie = parent.ext.get_mut::<Sound>().allocate_audio_id().unwrap();
        let child_movie = child.ext.get_mut::<Sound>().allocate_audio_id().unwrap();
        assert_eq!(parent_movie, (0x1000_u64 << 32) | 2);
        assert_eq!(child_movie, (0x2000_u64 << 32) | 2);
        assert!(shutdown(&mut parent.ext));
        drop(parent);
        assert_eq!(
            invoke(
                &mut child,
                "IDirectSoundBuffer8::Play",
                &[child_buffer, 0, 0, 1]
            ),
            0
        );
        pump(&mut child).unwrap();
        assert!(child.ext.get::<Sound>().audio.active());
    }
    // spec: EXEC-CORE-4
    #[test]
    fn directsound_binding_marks_dispatch() {
        use adv_cpu::backend::asm::Asm;
        let mut m = machine();
        let pointer = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            1000,
        );
        let code_at = 0x0040_0000;
        m.cpu.map(code_at, 4096, Prot::RX).unwrap();
        let target = m
            .resolve_import("dsound.dll", "IDirectSoundBuffer8::GetStatus")
            .unwrap();
        let mut code = Asm::new(code_at);
        code.push_imm(DATA).push_imm(pointer).call(target).ret();
        m.cpu.write(code_at, &code.bytes).unwrap();
        let thread = m.spawn("audio marker", code_at, &[], 65536).unwrap();
        assert!(matches!(
            m.run_virtual(10_000),
            adv_machine::RunResult::Exited(_)
        ));
        assert!(m.sys.audio_called_after(thread, 0));
    }

    fn invoke(m: &mut Machine<JitCpu>, name: &str, args: &[u32]) -> u32 {
        m.resolve_import("dsound.dll", name).unwrap();
        let thunk = m
            .sys
            .thunks
            .iter()
            .find(|(_, t)| t.module == "dsound.dll" && t.name == name)
            .unwrap()
            .0;
        m.cpu.set_reg(Reg::Esp, STACK);
        m.cpu.write(STACK, &[0; 40]).unwrap();
        for (i, &arg) in args.iter().enumerate() {
            m.cpu.write_u32(STACK + 4 + i as u32 * 4, arg).unwrap();
        }
        result(&mut Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk,
        })
        .unwrap()
    }
    fn device(m: &mut Machine<JitCpu>) -> u32 {
        assert_eq!(invoke(m, "DirectSoundCreate", &[0, DATA + 128, 0]), 0);
        m.cpu.read_u32(DATA + 128).unwrap()
    }
    fn activate(
        m: &mut Machine<JitCpu>,
        class: [u8; 16],
        iid: [u8; 16],
        outer: u32,
        output: u32,
    ) -> Outcome {
        crate::test_support::activate(
            &mut Env {
                context: m.sys.current,
                cpu: &mut m.cpu,
                sys: &mut m.sys,
                ext: &mut m.ext,
                thunk: 0,
            },
            class,
            iid,
            outer,
            output,
        )
    }
    #[test]
    fn com_activation_uses_the_same_audio_owner_and_requires_initialize() {
        let mut m = machine();
        let owners = m.sys.objects.object_count();
        // Windows SDK dsound.h: use the external GUID here so a shared typo in
        // registration and the fixture cannot make activation falsely pass.
        let sdk_device8 = [
            0x3f, 0xcc, 0x01, 0x39, 0xb5, 0x84, 0xa4, 0x4f, 0xba, 0x35, 0xaa, 0x81, 0x72, 0xb8,
            0xa0, 0x9b,
        ];
        for (class, iid) in [(CLSID_DEVICE, IID_DEVICE), (sdk_device8, IID_DEVICE8)] {
            assert!(matches!(
                activate(&mut m, class, iid, 0, DATA + 128),
                Outcome::Return(0)
            ));
            let ptr = m.cpu.read_u32(DATA + 128).unwrap();
            assert_ne!(ptr, 0);
            assert_eq!(m.sys.objects.object_count(), owners + 1);
            assert_eq!(
                invoke(&mut m, "IDirectSound8::GetCaps", &[ptr, 0]),
                0x887800aa
            );
            m.ext.get_mut::<Sound>().enabled = false;
            assert_eq!(
                invoke(&mut m, "IDirectSound8::Initialize", &[ptr, 0]),
                0x88780078
            );
            m.ext.get_mut::<Sound>().enabled = true;
            assert_eq!(invoke(&mut m, "IDirectSound8::Initialize", &[ptr, 0]), 0);
            assert_eq!(
                invoke(&mut m, "IDirectSound8::Initialize", &[ptr, 0]),
                0x88780082
            );
            assert_eq!(query(&mut m, ptr, &adv_os::com::IUNKNOWN), ptr);
            assert_eq!(invoke(&mut m, "IDirectSound8::Release", &[ptr]), 1);
            m.cpu.write_u32(DATA, 96).unwrap();
            assert_eq!(invoke(&mut m, "IDirectSound8::GetCaps", &[ptr, DATA]), 0);
            assert_eq!(invoke(&mut m, "IDirectSound8::Release", &[ptr]), 0);
            assert_eq!(m.sys.objects.object_count(), owners);
        }
        let direct = device(&mut m);
        assert_eq!(
            invoke(&mut m, "IDirectSound8::Initialize", &[direct, 0]),
            0x88780082
        );
        assert_eq!(invoke(&mut m, "IDirectSound8::Release", &[direct]), 0);
    }
    #[test]
    fn class_constructor_failures_do_not_allocate_or_leak() {
        let mut m = machine();
        let owners = m.sys.objects.object_count();
        for (class, iid, outer, expected) in [
            (CLSID_DEVICE, [0xff; 16], 0, NO_INTERFACE),
            (CLSID_DEVICE, IID_DEVICE8, 0, NO_INTERFACE),
            (CLSID_DEVICE, IID_DEVICE, DATA, 0x80040110),
        ] {
            m.cpu.write_u32(DATA + 128, 0xdeadbeef).unwrap();
            assert!(
                matches!(activate(&mut m, class, iid, outer, DATA + 128), Outcome::Return(hr) if hr == expected)
            );
            assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 0xdeadbeef);
            assert_eq!(m.sys.objects.object_count(), owners);
        }
        assert!(matches!(
            activate(&mut m, CLSID_DEVICE, IID_DEVICE, 0, 0x700000),
            Outcome::Fail(_)
        ));
        assert_eq!(m.sys.objects.object_count(), owners);
    }
    fn buffer(m: &mut Machine<JitCpu>, format: Format, bytes: u32) -> u32 {
        let device = device(m);
        m.cpu.write(DATA + 64, &format.bytes()).unwrap();
        for (i, value) in [20, 0x1e8, bytes, 0, DATA + 64].into_iter().enumerate() {
            m.cpu.write_u32(DATA + i as u32 * 4, value).unwrap();
        }
        assert_eq!(
            invoke(
                m,
                "IDirectSound8::CreateSoundBuffer",
                &[device, DATA, DATA + 128, 0]
            ),
            0
        );
        m.cpu.read_u32(DATA + 128).unwrap()
    }
    // spec: DEV-AUDIO-6
    #[test]
    fn three_d_controls_follow_windows_and_drive_the_mix() {
        let mut m = machine();
        let device = device(&mut m);
        assert_eq!(invoke(&mut m, "DirectSoundCreate8", &[0, DATA + 128, 0]), 0);
        let device8 = m.cpu.read_u32(DATA + 128).unwrap();
        let create = |m: &mut Machine<JitCpu>, device: u32, flags: u32, channels: u16| {
            let format = Format {
                rate: 22050,
                channels,
                bits: 16,
            };
            m.cpu.write(DATA + 64, &format.bytes()).unwrap();
            let (bytes, at) = if flags & 1 != 0 {
                (0, 0)
            } else {
                (22050 * 2 * u32::from(channels), DATA + 64)
            };
            for (i, value) in [20, flags, bytes, 0, at].into_iter().enumerate() {
                m.cpu.write_u32(DATA + i as u32 * 4, value).unwrap();
            }
            let hr = invoke(
                m,
                "IDirectSound8::CreateSoundBuffer",
                &[device, DATA, DATA + 128, 0],
            );
            (hr, m.cpu.read_u32(DATA + 128).unwrap())
        };
        let f = |value: f32| value.to_bits();
        let read_vector = |m: &Machine<JitCpu>| {
            Vector::from_words(std::array::from_fn(|i| {
                m.cpu.read_u32(DATA + 160 + i as u32 * 4).unwrap()
            }))
        };

        // The calls a game makes when it enables 3D sound, with Windows'
        // results: a zero orientation is rejected and the default stays.
        let (hr, primary) = create(&mut m, device, 0x91, 0);
        assert_eq!(hr, 0);
        let listener = query(&mut m, primary, &IID_LISTENER);
        let call = |m: &mut Machine<JitCpu>, name: &str, args: &[u32]| {
            invoke(m, &format!("IDirectSound3DListener::{name}"), args)
        };
        assert_eq!(call(&mut m, "SetPosition", &[listener, 0, 0, 0, 0]), 0);
        assert_eq!(call(&mut m, "SetVelocity", &[listener, 0, 0, 0, 0]), 0);
        let zero = [listener, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(call(&mut m, "SetOrientation", &zero), INVALID);
        assert_eq!(
            call(&mut m, "SetPosition", &[listener, f(9.0), 0, 0, 2]),
            INVALID
        );
        assert_eq!(
            call(&mut m, "SetRolloffFactor", &[listener, f(10.5), 0]),
            INVALID
        );
        let skewed = [listener, 0, 0, f(3.0), 0, f(1.0), f(1.0), 0];
        assert_eq!(call(&mut m, "SetOrientation", &skewed), 0);
        assert_eq!(
            call(
                &mut m,
                "GetOrientation",
                &[listener, DATA + 160, DATA + 176]
            ),
            0
        );
        assert_eq!(read_vector(&m), Vector::from_words([0, 0, f(1.0)]));
        // Deferred settings stay invisible until the listener commits them.
        assert_eq!(
            call(
                &mut m,
                "SetPosition",
                &[listener, f(1.0), f(2.0), f(3.0), 1]
            ),
            0
        );
        assert_eq!(call(&mut m, "GetPosition", &[listener, DATA + 160]), 0);
        assert_eq!(read_vector(&m), Vector::default());
        assert_eq!(call(&mut m, "CommitDeferredSettings", &[listener]), 0);
        assert_eq!(call(&mut m, "GetPosition", &[listener, DATA + 160]), 0);
        assert_eq!(
            read_vector(&m),
            Vector::from_words([f(1.0), f(2.0), f(3.0)])
        );
        assert_eq!(call(&mut m, "SetPosition", &[listener, 0, 0, 0, 0]), 0);

        // Creation rules: DirectSound 8 takes mono 3D sources without pan.
        assert_eq!(create(&mut m, device, 0x20000, 1).0, INVALID);
        assert_eq!(create(&mut m, device, 0x10, 2).0, 0);
        assert_eq!(create(&mut m, device, 0x50, 1).0, 0);
        assert_eq!(create(&mut m, device8, 0x10, 2).0, INVALID);
        assert_eq!(create(&mut m, device8, 0x50, 1).0, INVALID);
        assert_eq!(create(&mut m, device8, 0x20010, 1).0, 0);

        let (hr, source) = create(&mut m, device, 0x100b0, 1);
        assert_eq!(hr, 0);
        let control = query(&mut m, source, &IID_BUFFER_3D);
        let call = |m: &mut Machine<JitCpu>, name: &str, args: &[u32]| {
            invoke(m, &format!("IDirectSound3DBuffer::{name}"), args)
        };
        m.cpu.write_u32(DATA + 192, spatial::PARAMETERS).unwrap();
        assert_eq!(call(&mut m, "GetAllParameters", &[control, DATA + 192]), 0);
        let defaults = Source::read(&m.cpu, DATA + 192, 22050).unwrap();
        assert_eq!(defaults, Source::new(22050));
        assert_eq!(defaults.cone, Vector::from_words([0, 0, f(1.0)]));
        assert_eq!(
            call(&mut m, "SetConeAngles", &[control, 270, 90, 0]),
            INVALID
        );
        assert_eq!(call(&mut m, "SetMode", &[control, 3, 0]), INVALID);
        assert_eq!(
            call(&mut m, "SetConeOrientation", &[control, 0, 0, 0, 0]),
            INVALID
        );
        assert_eq!(call(&mut m, "SetMaxDistance", &[control, f(0.5), 0]), 0);
        assert_eq!(call(&mut m, "SetMaxDistance", &[control, f(1e9), 0]), 0);

        // An approaching source plays faster while reporting its own rate.
        assert_eq!(call(&mut m, "SetPosition", &[control, 0, 0, f(10.0), 0]), 0);
        assert_eq!(
            call(&mut m, "SetVelocity", &[control, 0, 0, f(-36.0), 0]),
            0
        );
        let playing = |m: &Machine<JitCpu>, buffer: u32| {
            let sound = m.ext.get::<Sound>();
            let buffer = sound.buffer(&m.sys.objects, buffer).unwrap();
            (buffer.frequency, mix(&sound.listener.applied, buffer).0)
        };
        assert_eq!(playing(&m, source).0, (22050.0f32 * 360.0 / 324.0) as u32);
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::GetFrequency",
                &[source, DATA + 128]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 22050);
        // A source to the right plays from the right speaker only.
        assert_eq!(call(&mut m, "SetVelocity", &[control, 0, 0, 0, 0]), 0);
        assert_eq!(call(&mut m, "SetPosition", &[control, f(1.0), 0, 0, 0]), 0);
        let right = |(rate, [left, right]): (u32, [f32; 2])| {
            rate == 22050 && left < 1e-3 && (right - 1.0).abs() < 1e-6
        };
        assert!(right(playing(&m, source)));

        // A duplicate carries the 3D parameters.
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSound8::DuplicateSoundBuffer",
                &[device, source, DATA + 128]
            ),
            0
        );
        let duplicate = m.cpu.read_u32(DATA + 128).unwrap();
        let copy = query(&mut m, duplicate, &IID_BUFFER_3D);
        assert_eq!(call(&mut m, "GetPosition", &[copy, DATA + 160]), 0);
        assert_eq!(read_vector(&m), Vector::from_words([f(1.0), 0, 0]));
        assert!(right(playing(&m, duplicate)));
    }
    fn query(m: &mut Machine<JitCpu>, object: u32, iid: &[u8; 16]) -> u32 {
        m.cpu.write(DATA + 256, iid).unwrap();
        assert_eq!(
            invoke(
                m,
                "IDirectSoundBuffer8::QueryInterface",
                &[object, DATA + 256, DATA + 128]
            ),
            0
        );
        m.cpu.read_u32(DATA + 128).unwrap()
    }
    fn position(m: &mut Machine<JitCpu>, buffer: u32) -> u32 {
        assert_eq!(
            invoke(
                m,
                "IDirectSoundBuffer8::GetCurrentPosition",
                &[buffer, DATA + 128, 0]
            ),
            0
        );
        m.cpu.read_u32(DATA + 128).unwrap()
    }
    fn write_cursor(m: &mut Machine<JitCpu>, buffer: u32) -> u32 {
        assert_eq!(
            invoke(
                m,
                "IDirectSoundBuffer8::GetCurrentPosition",
                &[buffer, 0, DATA + 132]
            ),
            0
        );
        m.cpu.read_u32(DATA + 132).unwrap()
    }
    fn advance(m: &mut Machine<JitCpu>, us: u64) {
        m.sys.clock.advance_to(m.sys.clock.now_us() + us);
    }

    #[test]
    fn notification_and_buffer_interfaces_share_identity_and_last_release() {
        let mut m = machine();
        let b = buffer(&mut m, Format::default(), 400);
        let before = m.sys.objects.object_count();
        let notify = query(&mut m, b, &IID_NOTIFY);
        assert_ne!(notify, b);
        assert_eq!(query(&mut m, b, &IID_NOTIFY), notify);
        assert_eq!(query(&mut m, notify, &adv_os::com::IUNKNOWN), b);
        assert_eq!(query(&mut m, b, &IID_BUFFER8), b);
        assert_eq!(m.sys.objects.object_count(), before);
        assert!(m.sys.objects.close(notify).is_none());
        for (pointer, remaining) in [(b, 4), (notify, 3), (b, 2), (b, 1), (notify, 0)] {
            assert_eq!(
                invoke(&mut m, "IDirectSoundBuffer8::Release", &[pointer]),
                remaining
            );
        }
        assert_eq!(m.sys.objects.object_count(), before - 1);
        assert!(m.ext.get::<Sound>().buffer(&m.sys.objects, b).is_none());
        assert!(m.ext.get::<Heaps>().owner(&m.cpu, b).is_none());
        assert!(m.ext.get::<Heaps>().owner(&m.cpu, notify).is_none());
        assert_eq!(
            invoke(&mut m, "IDirectSoundNotify::AddRef", &[notify]),
            INVALID
        );
        pump(&mut m).unwrap();
        assert!(m.ext.get::<Sound>().pending_release.is_empty());
    }

    fn heap_next(m: &mut Machine<JitCpu>) -> u32 {
        let heaps = m.ext.get_mut::<Heaps>();
        let pointer = heaps
            .alloc(&mut m.cpu, &mut m.sys.vm, heaps.process_heap, 4, false)
            .unwrap();
        assert!(heaps.free(&mut m.cpu, &mut m.sys.vm, heaps.process_heap, pointer));
        pointer
    }

    #[test]
    fn failed_device_and_notification_publication_reclaims_every_header() {
        use adv_machine::ObjectLimits;
        for limits in [
            ObjectLimits {
                objects: 0,
                ..ObjectLimits::default()
            },
            ObjectLimits {
                aliases: 0,
                ..ObjectLimits::default()
            },
            ObjectLimits {
                references: 1,
                ..ObjectLimits::default()
            },
        ] {
            let mut m = machine_with_limits(limits);
            let next = heap_next(&mut m);
            for _ in 0..64 {
                m.cpu.write_u32(DATA + 128, 0xcccccccc).unwrap();
                assert_eq!(
                    invoke(&mut m, "DirectSoundCreate", &[0, DATA + 128, 0]),
                    OUT_OF_MEMORY
                );
                assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 0);
                assert_eq!(m.sys.objects.object_count(), 0);
                assert_eq!(heap_next(&mut m), next);
            }
        }
        for limits in [
            ObjectLimits {
                objects: 1,
                ..ObjectLimits::default()
            },
            ObjectLimits {
                aliases: 2,
                ..ObjectLimits::default()
            },
            ObjectLimits {
                references: 2,
                ..ObjectLimits::default()
            },
        ] {
            let mut m = machine_with_limits(limits);
            let d = device(&mut m);
            let next = heap_next(&mut m);
            m.cpu.write(DATA + 64, &Format::default().bytes()).unwrap();
            for (i, value) in [20, 0x100, 400, 0, DATA + 64].into_iter().enumerate() {
                m.cpu.write_u32(DATA + i as u32 * 4, value).unwrap();
            }
            for _ in 0..64 {
                assert_eq!(
                    invoke(
                        &mut m,
                        "IDirectSound8::CreateSoundBuffer",
                        &[d, DATA, DATA + 128, 0]
                    ),
                    OUT_OF_MEMORY
                );
                assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 0);
                assert_eq!(m.sys.objects.object_count(), 1);
                assert_eq!(heap_next(&mut m), next);
                assert!(m.ext.get::<Sound>().buffer_ids.is_empty());
                assert!(m.ext.get::<Sound>().pending_release.is_empty());
            }
        }
    }

    #[test]
    fn rejected_ring_creation_reclaims_common_object_headers_and_mapping() {
        let mut m = machine();
        let d = device(&mut m);
        m.cpu.write(DATA + 64, &Format::default().bytes()).unwrap();
        for (i, value) in [20, 0x100, 400, 0, DATA + 64].into_iter().enumerate() {
            m.cpu.write_u32(DATA + i as u32 * 4, value).unwrap();
        }
        let mut created = Vec::new();
        for _ in 0..256 {
            assert_eq!(
                invoke(
                    &mut m,
                    "IDirectSound8::CreateSoundBuffer",
                    &[d, DATA, DATA + 128, 0]
                ),
                0
            );
            created.push(m.cpu.read_u32(DATA + 128).unwrap());
        }
        let next_header = heap_next(&mut m);
        let next_mapping = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
        m.sys.vm.free(&mut m.cpu, next_mapping).unwrap();
        for _ in 0..32 {
            assert_eq!(
                invoke(
                    &mut m,
                    "IDirectSound8::CreateSoundBuffer",
                    &[d, DATA, DATA + 128, 0]
                ),
                OUT_OF_MEMORY
            );
            assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 0);
            assert_eq!(m.sys.objects.object_count(), 257);
            assert_eq!(heap_next(&mut m), next_header);
            assert!(m.sys.vm.region(next_mapping).is_none());
            assert!(m.cpu.read_u8(next_mapping).is_err());
            let probe = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
            assert_eq!(probe, next_mapping);
            m.sys.vm.free(&mut m.cpu, probe).unwrap();
        }
        for pointer in created {
            assert_eq!(
                invoke(&mut m, "IDirectSoundBuffer8::Release", &[pointer]),
                0
            );
        }
        pump(&mut m).unwrap();
        assert!(m.ext.get::<Sound>().buffer_ids.is_empty());
        assert!(m.ext.get::<Sound>().pending_release.is_empty());
    }

    #[test]
    fn wrap_lock_aliases_guest_storage_and_rejects_invalid_unlock() {
        let mut m = machine();
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            100,
        );
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::Lock",
                &[b, 90, 20, DATA + 128, DATA + 132, DATA + 136, DATA + 140, 0]
            ),
            0
        );
        let p1 = m.cpu.read_u32(DATA + 128).unwrap();
        let p2 = m.cpu.read_u32(DATA + 136).unwrap();
        assert_eq!(m.cpu.read_u32(DATA + 132).unwrap(), 10);
        assert_eq!(m.cpu.read_u32(DATA + 140).unwrap(), 10);
        assert_eq!(p1, p2 + 90);
        m.cpu.write(p1, &[255; 10]).unwrap();
        m.cpu.write(p2, &[0; 10]).unwrap();
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Unlock", &[b, p1, 11, p2, 10]),
            INVALID
        );
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Unlock", &[b, p1, 9, p2, 10]),
            0
        );
        assert!(
            m.ext
                .get::<Sound>()
                .buffer(&m.sys.objects, b)
                .unwrap()
                .lock
                .is_none()
        );
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::Lock",
                &[b, 0, 20, 0, DATA + 132, 0, 0, 0]
            ),
            INVALID
        );
        assert!(
            m.ext
                .get::<Sound>()
                .buffer(&m.sys.objects, b)
                .unwrap()
                .lock
                .is_none()
        );
    }
    #[test]
    fn outstanding_write_lock_and_failed_unlock_do_not_freeze_playback() {
        let mut m = machine();
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            100,
        );
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::Lock",
                &[b, 0, 100, DATA + 128, DATA + 132, 0, 0, 0]
            ),
            0
        );
        let data = m.cpu.read_u32(DATA + 128).unwrap();
        m.cpu.write(data, &[192; 100]).unwrap();
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::Unlock",
                &[b, DATA + 128, 100, 0, 0]
            ),
            INVALID
        );
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 1]),
            0
        );
        pump(&mut m).unwrap();
        let stream = m.ext.get::<Sound>().audio.stream();
        let mut out = [0.0; 40];
        stream.read_stereo(&mut out, 1000);
        assert_eq!(
            out, [0.5; 40],
            "outstanding Lock writes must be device-visible"
        );
        advance(&mut m, 25_000);
        assert_eq!(position(&mut m, b), 25);
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Unlock", &[b, data, 100, 0, 0]),
            0
        );
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Unlock", &[b, data, 100, 0, 0]),
            INVALID
        );
    }
    #[test]
    // spec: DEV-AUDIO-1
    fn cursor_frequency_stop_restart_are_guest_clock_driven() {
        let mut m = machine();
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 2,
                bits: 16,
            },
            400,
        );
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 1]),
            0
        );
        advance(&mut m, 25_000);
        assert_eq!(position(&mut m, b), 100);
        // Without live output the write cursor leads by 30 ms, as on Windows.
        assert_eq!(write_cursor(&mut m, b), 220);
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::SetFrequency", &[b, 2000]),
            0
        );
        advance(&mut m, 25_000);
        assert_eq!(position(&mut m, b), 300);
        // At 2000 Hz this 100-frame buffer is 50 ms long, so the 30 ms lead caps at half the
        // buffer (50 frames = 200 bytes): (300 + 200) % 400 == 100.
        assert_eq!(write_cursor(&mut m, b), 100);
        invoke(&mut m, "IDirectSoundBuffer8::Stop", &[b]);
        advance(&mut m, 500_000);
        assert_eq!(position(&mut m, b), 300);
        assert_eq!(write_cursor(&mut m, b), 300);
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 1]);
        advance(&mut m, 25_000);
        assert_eq!(position(&mut m, b), 100);
        invoke(&mut m, "IDirectSoundBuffer8::SetCurrentPosition", &[b, 399]);
        assert_eq!(position(&mut m, b), 396);
    }

    #[test]
    // spec: DEV-AUDIO-1
    fn live_write_cursor_releases_each_half_only_after_the_mixer_reads_it() {
        let audio = AudioControl::new(true);
        let mut m = machine_for_process(
            adv_machine::ObjectLimits::default(),
            0x1000,
            Some(audio.clone()),
        );
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            64,
        );
        let storage = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, b)
            .unwrap()
            .storage_addr();
        m.cpu.write(storage, &[192; 64]).unwrap();
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 1]),
            0
        );
        let mut reader = audio.stream().device_reader(1000, 2).unwrap();
        let cursors = |m: &mut Machine<JitCpu>| {
            assert_eq!(
                invoke(
                    m,
                    "IDirectSoundBuffer8::GetCurrentPosition",
                    &[b, DATA + 124, DATA + 128]
                ),
                0
            );
            (
                m.cpu.read_u32(DATA + 124).unwrap(),
                m.cpu.read_u32(DATA + 128).unwrap(),
            )
        };

        // Immediately after Play, 30 frames are staged ahead of play, so the first 32-byte half
        // is not yet completely released.
        assert_eq!(cursors(&mut m), (0, 30));
        let mut first = [0.0; 2];
        reader.read(&mut first, 1_000_000);
        assert_eq!(first, [0.5; 2]);
        assert_eq!(cursors(&mut m).1, 31);
        let mut first_tail = [0.0; 2];
        reader.read(&mut first_tail, 1_000_000);
        assert_eq!(first_tail, [0.5; 2]);
        assert_eq!(cursors(&mut m).1, 32);
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::Lock",
                &[b, 0, 1, DATA + 128, DATA + 132, 0, 0, 1]
            ),
            0
        );
        let pointer = m.cpu.read_u32(DATA + 128).unwrap();
        assert_eq!(pointer, storage + 32);
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::Unlock",
                &[b, pointer, 0, 0, 0]
            ),
            0
        );

        // Refill the released first half while frames 2..32 are still staged in the mixer.
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::Lock",
                &[b, 0, 32, DATA + 128, DATA + 132, 0, 0, 0]
            ),
            0
        );
        let pointer = m.cpu.read_u32(DATA + 128).unwrap();
        m.cpu.write(pointer, &[64; 32]).unwrap();
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::Unlock",
                &[b, pointer, 32, 0, 0]
            ),
            0
        );
        let mut second = [0.0; 64];
        reader.read(&mut second, 1_000_000);
        assert_eq!(second, [0.5; 64]);
        assert_eq!(cursors(&mut m).1, 0);
        let mut second_tail = [0.0; 60];
        reader.read(&mut second_tail, 1_000_000);
        assert_eq!(second_tail, [0.5; 60]);
        let mut wrapped = [0.0; 16];
        reader.read(&mut wrapped, 1_000_000);
        assert_eq!(wrapped, [-0.5; 16]);
    }

    #[test]
    fn unsigned_mono_and_signed_stereo_pcm_are_read_from_shared_guest_memory() {
        for format in [
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            Format {
                rate: 1000,
                channels: 2,
                bits: 16,
            },
        ] {
            let mut m = machine();
            let b = buffer(&mut m, format, format.align() * 100);
            let ptr = m
                .ext
                .get::<Sound>()
                .buffer(&m.sys.objects, b)
                .unwrap()
                .storage_addr();
            let bytes = if format.bits == 8 {
                vec![192; 100]
            } else {
                [16384i16, -8192]
                    .into_iter()
                    .flat_map(i16::to_le_bytes)
                    .cycle()
                    .take(400)
                    .collect()
            };
            m.cpu.write(ptr, &bytes).unwrap();
            invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 1]);
            assert!(
                pump(&mut m).unwrap().is_none(),
                "a static loop needs no execution-side polling"
            );
            let stream: adv_device::AudioStream = m.ext.get::<Sound>().audio.stream();
            let mut out = [0.0; 40];
            stream.read_stereo(&mut out, 1000);
            for frame in out.as_chunks::<2>().0 {
                assert_eq!(frame[0], 0.5);
                assert_eq!(frame[1], if format.channels == 1 { 0.5 } else { -0.25 });
            }
            // Frames 20..50 are already staged ahead of the play cursor; guest writes take effect
            // starting at the write cursor (frame 50).
            m.cpu
                .write(
                    ptr,
                    &vec![if format.bits == 8 { 128 } else { 0 }; bytes.len()],
                )
                .unwrap();
            let mut staged = [0.0; 60];
            stream.read_stereo(&mut staged, 1000);
            for frame in staged.as_chunks::<2>().0 {
                assert_eq!(frame[0], 0.5);
                assert_eq!(frame[1], if format.channels == 1 { 0.5 } else { -0.25 });
            }
            stream.read_stereo(&mut out, 1000);
            assert_eq!(
                out, [0.0; 40],
                "guest writes ahead of the write cursor stay visible through the read alias"
            );
            assert_eq!(stream.rendered_samples(), (140, 100));
        }
    }
    #[test]
    fn volume_and_pan_preserve_pending_pcm_when_device_lags_guest() {
        let mut m = machine();
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            100,
        );
        let ptr = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, b)
            .unwrap()
            .storage_addr();
        m.cpu.write(ptr, &[192; 100]).unwrap();
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 1]);
        pump(&mut m).unwrap();
        let stream: adv_device::AudioStream = m.ext.get::<Sound>().audio.stream();
        let mut first = [0.0; 20];
        stream.read_stereo(&mut first, 1000);
        assert_eq!(first, [0.5; 20]);
        // Guest is at 20 ms, while the device has consumed only through 10 ms.
        advance(&mut m, 20_000);
        pump(&mut m).unwrap();
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::SetVolume",
                &[b, (-2000i32) as u32]
            ),
            0
        );
        pump(&mut m).unwrap();
        let mut faded = [0.0; 40];
        stream.read_stereo(&mut faded, 1000);
        assert!(
            faded.iter().all(|value| *value >= 0.049 && *value <= 0.5)
                && faded[10..].iter().all(|value| (*value - 0.05).abs() < 1e-6),
            "volume change introduced a silent gap: {faded:?}"
        );
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::SetPan", &[b, 10000]),
            0
        );
        let mut panned = [0.0; 20];
        stream.read_stereo(&mut panned, 1000);
        assert!(
            panned[10..]
                .as_chunks::<2>()
                .0
                .iter()
                .all(|frame| frame[0] == 0.0 && (frame[1] - 0.05).abs() < 1e-6)
        );
        invoke(
            &mut m,
            "IDirectSoundBuffer8::SetVolume",
            &[b, (-10000i32) as u32],
        );
        let mut muted = [1.0; 20];
        stream.read_stereo(&mut muted, 1000);
        assert!(muted.iter().all(|value| *value >= 0.0 && *value <= 0.05));
        assert_eq!(muted[10..], [0.0; 10]);
    }
    #[test]
    // spec: DEV-AUDIO-1
    fn stop_seek_and_buffer_reuse_cannot_emit_an_older_voice_generation() {
        let mut m = machine();
        let format = Format {
            rate: 1000,
            channels: 1,
            bits: 8,
        };
        let voice = buffer(&mut m, format, 1000);
        let music = buffer(&mut m, format, 1000);
        let voice_storage = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, voice)
            .unwrap()
            .storage_addr();
        let music_storage = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, music)
            .unwrap()
            .storage_addr();
        m.cpu.write(voice_storage, &[192; 1000]).unwrap();
        m.cpu.write(music_storage, &[160; 1000]).unwrap();
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[voice, 0, 0, 0]);
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[music, 0, 0, 1]);
        pump(&mut m).unwrap();
        let stream: adv_device::AudioStream = m.ext.get::<Sound>().audio.stream();
        let mut out = [0.0; 20];
        stream.read_stereo(&mut out, 1000);
        assert_eq!(out, [0.75; 20]);
        advance(&mut m, 10_000);
        invoke(&mut m, "IDirectSoundBuffer8::Stop", &[voice]);
        invoke(
            &mut m,
            "IDirectSoundBuffer8::Lock",
            &[voice, 0, 1000, DATA + 128, DATA + 132, 0, 0, 2],
        );
        let pointer = m.cpu.read_u32(DATA + 128).unwrap();
        m.cpu.write(pointer, &[64; 1000]).unwrap();
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::Unlock",
                &[voice, pointer, 1000, 0, 0]
            ),
            0
        );
        invoke(
            &mut m,
            "IDirectSoundBuffer8::SetCurrentPosition",
            &[voice, 0],
        );
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[voice, 0, 0, 0]);
        pump(&mut m).unwrap();
        stream.read_stereo(&mut out, 1000);
        assert_eq!(out, [-0.25; 20]);
        invoke(&mut m, "IDirectSoundBuffer8::Stop", &[voice]);
        stream.read_stereo(&mut out, 1000);
        assert_eq!(out, [0.25; 20]);
    }
    #[test]
    fn restarting_a_naturally_ended_buffer_discards_its_unconsumed_old_tail() {
        let mut m = machine();
        let format = Format {
            rate: 1000,
            channels: 1,
            bits: 8,
        };
        let voice = buffer(&mut m, format, 30);
        let storage = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, voice)
            .unwrap()
            .storage_addr();
        m.cpu.write(storage, &[192; 30]).unwrap();
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[voice, 0, 0, 0]);
        pump(&mut m).unwrap();
        advance(&mut m, 30_000);
        // No callback has consumed the first generation and no pump has normalized
        // natural completion yet. The new Play must not inherit its queued PCM.
        m.cpu.write(storage, &[64; 30]).unwrap();
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[voice, 0, 0, 0]);
        pump(&mut m).unwrap();
        let stream: adv_device::AudioStream = m.ext.get::<Sound>().audio.stream();
        let mut out = [0.0; 80];
        stream.read_stereo(&mut out, 1000);
        assert_eq!(out[..60], [-0.5; 60]);
        assert_eq!(out[60..], [0.0; 20]);
    }
    #[test]
    fn seeking_a_playing_voice_discards_prefetch_from_the_previous_cursor() {
        let mut m = machine();
        let voice = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            1000,
        );
        let storage = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, voice)
            .unwrap()
            .storage_addr();
        m.cpu.write(storage, &[192; 500]).unwrap();
        m.cpu.write(storage + 500, &[64; 500]).unwrap();
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[voice, 0, 0, 0]);
        pump(&mut m).unwrap();
        let stream: adv_device::AudioStream = m.ext.get::<Sound>().audio.stream();
        let mut out = [0.0; 20];
        stream.read_stereo(&mut out, 1000);
        assert_eq!(out, [0.5; 20]);
        advance(&mut m, 10_000);
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::SetCurrentPosition",
                &[voice, 500]
            ),
            0
        );
        pump(&mut m).unwrap();
        stream.read_stereo(&mut out, 1000);
        assert_eq!(out, [-0.5; 20]);
        assert_eq!(position(&mut m, voice), 500);
    }
    #[test]
    // spec: DEV-AUDIO-1
    fn releasing_a_voice_cannot_leak_its_queue_into_the_next_buffer() {
        let mut m = machine();
        let format = Format {
            rate: 1000,
            channels: 1,
            bits: 8,
        };
        let voice = buffer(&mut m, format, 1000);
        let music = buffer(&mut m, format, 1000);
        let voice_storage = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, voice)
            .unwrap()
            .storage_addr();
        let music_storage = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, music)
            .unwrap()
            .storage_addr();
        m.cpu.write(voice_storage, &[192; 1000]).unwrap();
        m.cpu.write(music_storage, &[160; 1000]).unwrap();
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[voice, 0, 0, 0]);
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[music, 0, 0, 1]);
        pump(&mut m).unwrap();
        assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Release", &[voice]), 0);
        let next_voice = buffer(&mut m, format, 1000);
        let next_storage = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, next_voice)
            .unwrap()
            .storage_addr();
        m.cpu.write(next_storage, &[64; 1000]).unwrap();
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[next_voice, 0, 0, 0]);
        pump(&mut m).unwrap();
        let stream: adv_device::AudioStream = m.ext.get::<Sound>().audio.stream();
        let mut out = [0.0; 20];
        stream.read_stereo(&mut out, 1000);
        assert_eq!(out, [-0.25; 20]);
    }
    #[test]
    // spec: DEV-AUDIO-2
    fn a_single_release_reclaims_its_mapping_after_a_saturated_command_lane_recovers() {
        let mut m = machine();
        let voice = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            1000,
        );
        let (source, storage) = {
            let buffer = &m.ext.get::<Sound>().buffer(&m.sys.objects, voice).unwrap();
            (buffer.audio_id, buffer.storage_addr())
        };
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[voice, 0, 0, 1]);
        let audio = m.ext.get::<Sound>().audio.clone();
        let mut reader = audio.stream().device_reader(1000, 2).unwrap();
        for _ in 0..7000 {
            audio.set_gain(source, 0.5);
        }
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Release", &[voice]),
            0,
            "COM references are released exactly once"
        );
        assert!(
            m.ext
                .get::<Sound>()
                .com
                .get::<Object>(&m.sys.objects, voice)
                .is_none()
        );
        assert!(
            m.cpu.read_u8(storage).is_ok(),
            "mapping stays alive until the callback releases the alias"
        );
        let mut output = [0.0; 512];
        for _ in 0..8 {
            reader.read(&mut output, 0);
            pump(&mut m).unwrap();
        }
        assert!(m.ext.get::<Sound>().pending_release.is_empty());
        assert!(
            m.cpu.read_u8(storage).is_err(),
            "the pending removal is retried internally, without a second Release call"
        );
    }
    #[test]
    // spec: DEV-AUDIO-2
    fn saturated_controls_preserve_buffer_state_and_stop_notifications() {
        let mut m = machine();
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            1000,
        );
        let source = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, b)
            .unwrap()
            .audio_id;
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 1]),
            0
        );
        let event = m.sys.objects.insert(WaitObject::Event {
            manual_reset: false,
            signaled: false,
        });
        let sound = m.ext.get::<Sound>();
        sound
            .com
            .get_mut::<Object>(&mut m.sys.objects, b)
            .unwrap()
            .buffer
            .as_mut()
            .unwrap()
            .notifications
            .push((u32::MAX, event));
        let audio = sound.audio.clone();
        let mut reader = audio.stream().device_reader(1000, 2).unwrap();
        for _ in 0..6144 {
            assert!(audio.set_gain(source, 1.0));
        }
        assert!(!audio.set_gain(source, 1.0));
        let before = {
            let buf = m.ext.get::<Sound>().buffer(&m.sys.objects, b).unwrap();
            (
                buf.playing,
                buf.looping,
                buf.base,
                buf.anchor,
                buf.frequency,
                buf.generation,
            )
        };
        for (name, args) in [
            ("Stop", vec![b]),
            ("SetCurrentPosition", vec![b, 100]),
            ("Play", vec![b, 0, 0, 0]),
            ("SetFrequency", vec![b, 2000]),
        ] {
            assert_eq!(
                invoke(&mut m, &format!("IDirectSoundBuffer8::{name}"), &args),
                INVALID_CALL,
                "{name} must report backpressure"
            );
            let buf = m.ext.get::<Sound>().buffer(&m.sys.objects, b).unwrap();
            assert_eq!(
                (
                    buf.playing,
                    buf.looping,
                    buf.base,
                    buf.anchor,
                    buf.frequency,
                    buf.generation
                ),
                before
            );
        }
        assert!(m.ext.get::<Sound>().ready_notices.is_empty());
        for _ in 0..4 {
            reader.read(&mut [0.0; 2], 0);
            audio.maintain();
        }
        assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Stop", &[b]), 0);
        reader.read(&mut [0.0; 2], 0);
        pump(&mut m).unwrap();
        assert!(!audio.active());
        assert!(matches!(
            m.sys.objects.get(event),
            Some(WaitObject::Event { signaled: true, .. })
        ));
    }

    #[test]
    fn seek_with_one_command_slot_commits_position_and_generation_together() {
        let mut m = machine();
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            1000,
        );
        let (source, voice) = {
            let buf = m.ext.get::<Sound>().buffer(&m.sys.objects, b).unwrap();
            (buf.audio_id, buf.voice.clone().unwrap())
        };
        let audio = m.ext.get::<Sound>().audio.clone();
        let mut reader = audio.stream().device_reader(1000, 2).unwrap();
        for _ in 0..6143 {
            assert!(audio.set_gain(source, 1.0));
        }
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::SetCurrentPosition", &[b, 100]),
            0
        );
        let generation = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, b)
            .unwrap()
            .generation;
        for _ in 0..4 {
            reader.read(&mut [0.0; 2], 0);
            audio.maintain();
        }
        assert_eq!(voice.cursor(0, generation), Some(100));
    }
    #[test]
    // spec: DEV-AUDIO-2
    fn notifications_wrap_and_natural_stop_signal_real_events() {
        let mut m = machine();
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            100,
        );
        let notify = query(&mut m, b, &IID_NOTIFY);
        let at_zero = m.sys.objects.insert(WaitObject::Event {
            manual_reset: true,
            signaled: false,
        });
        let at_end = m.sys.objects.insert(WaitObject::Event {
            manual_reset: true,
            signaled: false,
        });
        for (i, value) in [0, at_zero, u32::MAX, at_end].into_iter().enumerate() {
            m.cpu.write_u32(DATA + 300 + i as u32 * 4, value).unwrap();
        }
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundNotify::SetNotificationPositions",
                &[notify, 2, DATA + 300]
            ),
            0
        );
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 0]);
        advance(&mut m, 1_000);
        pump(&mut m).unwrap();
        assert!(matches!(
            m.sys.objects.get(at_zero),
            Some(WaitObject::Event { signaled: true, .. })
        ));
        assert!(matches!(
            m.sys.objects.get(at_end),
            Some(WaitObject::Event {
                signaled: false,
                ..
            })
        ));
        advance(&mut m, 100_000);
        assert_eq!(pump(&mut m).unwrap(), Some(m.sys.clock.now_us()));
        assert!(matches!(
            m.sys.objects.get(at_end),
            Some(WaitObject::Event { signaled: true, .. })
        ));
        assert_eq!(position(&mut m, b), 0);
        assert!(
            !m.ext
                .get::<Sound>()
                .buffer(&m.sys.objects, b)
                .unwrap()
                .playing
        );
        assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Release", &[b]), 1);
        assert_eq!(invoke(&mut m, "IDirectSoundNotify::Release", &[notify]), 0);
        assert!(m.ext.get::<Sound>().buffer(&m.sys.objects, b).is_none());
    }
    #[test]
    fn headless_streaming_stays_bounded_without_device_consumption() {
        let mut m = machine();
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            100,
        );
        let ptr = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, b)
            .unwrap()
            .storage_addr();
        m.cpu.write(ptr, &[192; 100]).unwrap();
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 1]);
        for _ in 0..1000 {
            pump(&mut m).unwrap();
            advance(&mut m, 10_000);
        }
        assert!(m.ext.get::<Sound>().nonzero_samples > 19000);
        assert_eq!(position(&mut m, b), 0);
        assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Release", &[b]), 0);
        assert!(!m.ext.get::<Sound>().audio.active());
    }
    #[test]
    fn guest_calls_com_vtable_and_stdcall_restores_stack() {
        use iced_x86::code_asm::*;
        let mut m = machine();
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            100,
        );
        let code = 0x400000;
        m.cpu.map(code, 4096, Prot::RX).unwrap();
        let mut a = CodeAssembler::new(32).unwrap();
        a.mov(ebx, esp).unwrap();
        a.mov(eax, b).unwrap();
        a.mov(ecx, dword_ptr(eax)).unwrap();
        a.push(1).unwrap();
        a.push(0).unwrap();
        a.push(0).unwrap();
        a.push(eax).unwrap();
        a.call(dword_ptr(ecx + 12 * 4)).unwrap();
        a.sub(ebx, esp).unwrap();
        a.mov(dword_ptr((DATA + 512) as i32), ebx).unwrap();
        a.ret().unwrap();
        m.cpu
            .write(code, &a.assemble(u64::from(code)).unwrap())
            .unwrap();
        m.spawn("audio-com", code, &[], 65536).unwrap();
        assert!(matches!(m.run(), adv_machine::RunResult::Exited(0)));
        assert_eq!(m.cpu.read_u32(DATA + 512).unwrap(), 0);
        assert!(
            m.ext
                .get::<Sound>()
                .buffer(&m.sys.objects, b)
                .unwrap()
                .playing
        );
    }

    #[test]
    fn duplicate_sound_buffer_plays_concurrently_and_controls_independently() {
        let mut m = machine();
        let dev = device(&mut m);
        let format = Format {
            rate: 1000,
            channels: 1,
            bits: 8,
        };
        let orig = buffer(&mut m, format, 100);
        let orig_storage = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, orig)
            .unwrap()
            .storage_addr();
        m.cpu.write(orig_storage, &[192; 100]).unwrap(); // amplitude 0.5

        // Duplicate orig -> dup
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSound8::DuplicateSoundBuffer",
                &[dev, orig, DATA + 128]
            ),
            0
        );
        let dup = m.cpu.read_u32(DATA + 128).unwrap();
        assert_ne!(orig, dup);

        // Both can play simultaneously
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[orig, 0, 0, 1]);
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[dup, 0, 0, 1]);
        pump(&mut m).unwrap();
        let stream: adv_device::AudioStream = m.ext.get::<Sound>().audio.stream();
        let mut out = [0.0; 20];
        stream.read_stereo(&mut out, 1000);
        assert_eq!(
            out, [1.0; 20],
            "both voices play concurrently and mix: 0.5 + 0.5 = 1.0"
        );

        // Stop orig, dup keeps playing
        invoke(&mut m, "IDirectSoundBuffer8::Stop", &[orig]);
        pump(&mut m).unwrap();
        stream.read_stereo(&mut out, 1000);
        assert_eq!(out, [0.5; 20], "dup continues playing after orig stops");

        // Adjust volume on dup, orig is unaffected
        invoke(
            &mut m,
            "IDirectSoundBuffer8::SetVolume",
            &[dup, (-600i32) as u32],
        );
        assert_eq!(
            m.ext
                .get::<Sound>()
                .buffer(&m.sys.objects, dup)
                .unwrap()
                .volume,
            -600
        );
        assert_eq!(
            m.ext
                .get::<Sound>()
                .buffer(&m.sys.objects, orig)
                .unwrap()
                .volume,
            0
        );

        // Seek on dup, orig cursor is unaffected
        invoke(
            &mut m,
            "IDirectSoundBuffer8::SetCurrentPosition",
            &[dup, 50],
        );
        assert_eq!(position(&mut m, dup), 50);
        assert_eq!(position(&mut m, orig), 0);

        // Lock & Unlock on orig writes to shared storage and affects both
        invoke(
            &mut m,
            "IDirectSoundBuffer8::Lock",
            &[orig, 0, 100, DATA + 132, DATA + 136, 0, 0, 2],
        );
        let p1 = m.cpu.read_u32(DATA + 132).unwrap();
        assert_eq!(p1, orig_storage);
        // Lock on dup while orig is locked must be independent (orig is locked, dup is not)
        assert!(
            m.ext
                .get::<Sound>()
                .buffer(&m.sys.objects, orig)
                .unwrap()
                .lock
                .is_some()
        );
        assert!(
            m.ext
                .get::<Sound>()
                .buffer(&m.sys.objects, dup)
                .unwrap()
                .lock
                .is_none()
        );

        // Write new samples via orig lock
        m.cpu.write(p1, &[128; 100]).unwrap(); // silence
        invoke(
            &mut m,
            "IDirectSoundBuffer8::Unlock",
            &[orig, p1, 100, 0, 0],
        );

        // Lock dup to verify it points to the exact same shared memory
        invoke(
            &mut m,
            "IDirectSoundBuffer8::Lock",
            &[dup, 0, 100, DATA + 140, DATA + 144, 0, 0, 2],
        );
        let dup_p1 = m.cpu.read_u32(DATA + 140).unwrap();
        assert_eq!(dup_p1, orig_storage);
        invoke(
            &mut m,
            "IDirectSoundBuffer8::Unlock",
            &[dup, dup_p1, 100, 0, 0],
        );

        // Clean up
        invoke(&mut m, "IDirectSoundBuffer8::Release", &[orig]);
        invoke(&mut m, "IDirectSoundBuffer8::Release", &[dup]);
        invoke(&mut m, "IDirectSound8::Release", &[dev]);
    }

    #[test]
    fn releasing_original_keeps_duplicate_alive_for_playback_lock_and_further_cloning() {
        let mut m = machine();
        let dev = device(&mut m);
        let format = Format {
            rate: 1000,
            channels: 1,
            bits: 8,
        };
        let orig = buffer(&mut m, format, 100);
        let storage = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, orig)
            .unwrap()
            .storage_addr();
        m.cpu.write(storage, &[192; 100]).unwrap();

        // Duplicate orig -> dup1
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSound8::DuplicateSoundBuffer",
                &[dev, orig, DATA + 128]
            ),
            0
        );
        let dup1 = m.cpu.read_u32(DATA + 128).unwrap();

        // Release original buffer
        assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Release", &[orig]), 0);
        pump(&mut m).unwrap();

        // Storage MUST still be mapped and valid in guest memory
        assert!(m.cpu.read_u8(storage).is_ok());

        // dup1 can still play
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[dup1, 0, 0, 1]);
        pump(&mut m).unwrap();
        let stream: adv_device::AudioStream = m.ext.get::<Sound>().audio.stream();
        let mut out = [0.0; 20];
        stream.read_stereo(&mut out, 1000);
        assert_eq!(out, [0.5; 20]);

        // dup1 can still Lock and write
        invoke(
            &mut m,
            "IDirectSoundBuffer8::Lock",
            &[dup1, 0, 100, DATA + 132, DATA + 136, 0, 0, 2],
        );
        let p = m.cpu.read_u32(DATA + 132).unwrap();
        assert_eq!(p, storage);
        m.cpu.write(p, &[255; 100]).unwrap(); // amplitude ~1.0
        invoke(&mut m, "IDirectSoundBuffer8::Unlock", &[dup1, p, 100, 0, 0]);

        // dup1 can be cloned to dup2 even after original is long dead!
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSound8::DuplicateSoundBuffer",
                &[dev, dup1, DATA + 140]
            ),
            0
        );
        let dup2 = m.cpu.read_u32(DATA + 140).unwrap();
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[dup2, 0, 0, 1]);
        pump(&mut m).unwrap();

        // Now test saturated retirement when releasing remaining clones
        let audio = m.ext.get::<Sound>().audio.clone();
        let mut reader = audio.stream().device_reader(1000, 2).unwrap();
        for _ in 0..7000 {
            audio.set_gain(1, 0.5);
        }

        // Release dup1
        assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Release", &[dup1]), 0);
        pump(&mut m).unwrap();
        // dup2 is still alive, so storage MUST NOT be freed yet
        assert!(m.cpu.read_u8(storage).is_ok());

        // Release dup2
        assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Release", &[dup2]), 0);

        // Drain reader & pump to let retirement complete
        let mut output = [0.0; 512];
        for _ in 0..8 {
            reader.read(&mut output, 0);
            pump(&mut m).unwrap();
        }

        assert!(m.ext.get::<Sound>().pending_release.is_empty());
        assert!(m.ext.get::<Sound>().storage_records.is_empty());
        assert!(
            m.cpu.read_u8(storage).is_err(),
            "shared storage reclaimed exactly once after all clones release"
        );
    }

    #[test]
    fn duplicate_sound_buffer_rejects_primary_buffer_and_invalid_args() {
        let mut m = machine();
        let dev = device(&mut m);
        let objects = m.sys.objects.object_count();
        let header = heap_next(&mut m);
        // Invalid structure size is distinct from an unsupported capability.
        m.cpu.write_u32(DATA, 19).unwrap();
        // A 3D hardware voice, which Windows also refuses with E_NOTIMPL.
        m.cpu.write_u32(DATA + 4, 0x14).unwrap();
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSound8::CreateSoundBuffer",
                &[dev, DATA, DATA + 128, 0]
            ),
            INVALID
        );
        assert!(
            m.ext
                .get_or_default::<adv_abi::ApiDiagnostics>()
                .warnings
                .is_empty()
        );
        m.cpu.write_u32(DATA, 20).unwrap();
        for _ in 0..128 {
            m.cpu.write_u32(DATA + 128, 0xcccccccc).unwrap();
            assert_eq!(
                invoke(
                    &mut m,
                    "IDirectSound8::CreateSoundBuffer",
                    &[dev, DATA, DATA + 128, 0]
                ),
                UNSUPPORTED
            );
            assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 0);
        }
        let diagnostics = m.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostics.warnings.len(), 1);
        let ((api, reason), count) = diagnostics.warnings.first_key_value().unwrap();
        assert_eq!(api, "dsound.dll!IDirectSound8::CreateSoundBuffer");
        assert!(reason.contains("flags=0x00000014"));
        assert!(reason.contains("unsupported_mask=0x00000004"));
        assert!(reason.contains("HRESULT=0x80004001"));
        assert_eq!(*count, 128);
        assert_eq!(m.sys.objects.object_count(), objects);
        assert_eq!(heap_next(&mut m), header);
        assert!(m.ext.get::<Sound>().buffer_ids.is_empty());
        // Create primary buffer
        m.cpu.write_u32(DATA, 20).unwrap();
        m.cpu.write_u32(DATA + 4, 1).unwrap(); // DSBCAPS_PRIMARYBUFFER
        m.cpu.write_u32(DATA + 8, 0).unwrap();
        m.cpu.write_u32(DATA + 12, 0).unwrap();
        m.cpu.write_u32(DATA + 16, 0).unwrap();
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSound8::CreateSoundBuffer",
                &[dev, DATA, DATA + 128, 0]
            ),
            0
        );
        let primary = m.cpu.read_u32(DATA + 128).unwrap();
        assert_eq!(m.ext.get::<adv_abi::ApiDiagnostics>().warnings.len(), 1);
        assert_eq!(
            *m.ext
                .get::<adv_abi::ApiDiagnostics>()
                .warnings
                .values()
                .next()
                .unwrap(),
            128
        );
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSound8::DuplicateSoundBuffer",
                &[dev, primary, DATA + 132]
            ),
            0x88780032, // DSERR_INVALIDCALL
        );
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSound8::DuplicateSoundBuffer",
                &[dev, primary, 0]
            ),
            INVALID,
        );
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSound8::DuplicateSoundBuffer",
                &[dev, 0, DATA + 132]
            ),
            INVALID,
        );
        invoke(&mut m, "IDirectSoundBuffer8::Release", &[primary]);
        invoke(&mut m, "IDirectSound8::Release", &[dev]);
    }

    #[test]
    // spec: DEV-AUDIO-1
    fn live_cursors_hold_staged_window_before_first_callback_and_across_seek() {
        let audio = AudioControl::new(true);
        let mut m = machine_for_process(
            adv_machine::ObjectLimits::default(),
            0x1000,
            Some(audio.clone()),
        );
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 2,
                bits: 16,
            },
            400,
        );
        let storage = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, b)
            .unwrap()
            .storage_addr();
        let first_half: Vec<u8> = 16384i16
            .to_le_bytes()
            .into_iter()
            .cycle()
            .take(200)
            .collect();
        let second_half: Vec<u8> = (-16384i16)
            .to_le_bytes()
            .into_iter()
            .cycle()
            .take(200)
            .collect();
        m.cpu.write(storage, &first_half).unwrap();
        m.cpu.write(storage + 200, &second_half).unwrap();

        let mut reader = audio.stream().device_reader(1000, 2).unwrap();
        reader.read(&mut [0.0; 4000], 0);
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 1]),
            0
        );
        let cursors = |m: &mut Machine<JitCpu>| {
            assert_eq!(
                invoke(
                    m,
                    "IDirectSoundBuffer8::GetCurrentPosition",
                    &[b, DATA + 124, DATA + 128]
                ),
                0
            );
            (
                m.cpu.read_u32(DATA + 124).unwrap(),
                m.cpu.read_u32(DATA + 128).unwrap(),
            )
        };

        // Before the first callback services Play, play stays anchored at base (0) while write
        // already exposes the initial 30 ms (120-byte) staged lead, even if guest time advances.
        advance(&mut m, 15_000);
        assert_eq!(cursors(&mut m), (0, 120));

        // First callback mixes 10 frames (40 bytes) and stages 30 frames ahead (write = 160).
        let mut out = [0.0; 20];
        reader.read(&mut out, 1_000_000);
        assert!(out.iter().all(|&sample| (sample - 0.5).abs() < 1e-6));
        assert_eq!(cursors(&mut m), (0, 160));

        // Seeking mid-playback discards the old generation's staged tail and immediately reports
        // the new staged window [200, 320) before the next callback arrives.
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::SetCurrentPosition", &[b, 200]),
            0
        );
        advance(&mut m, 15_000);
        assert_eq!(cursors(&mut m), (200, 320));

        reader.read(&mut out, 1_000_000);
        assert!(out.iter().all(|&sample| (sample + 0.5).abs() < 1e-6));
        assert_eq!(cursors(&mut m), (200, 360));
    }

    #[test]
    // spec: DEV-AUDIO-1
    fn retune_resamples_staged_source_frames_without_rereading_or_rewinding_write_cursor() {
        let audio = AudioControl::new(true);
        let mut m = machine_for_process(
            adv_machine::ObjectLimits::default(),
            0x1000,
            Some(audio.clone()),
        );
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            200,
        );
        let storage = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, b)
            .unwrap()
            .storage_addr();
        m.cpu.write(storage, &[192; 200]).unwrap();
        let mut reader = audio.stream().device_reader(1000, 2).unwrap();
        reader.read(&mut [0.0; 4000], 0);
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 1]),
            0
        );
        let mut first = [0.0; 20];
        reader.read(&mut first, 1_000_000);
        assert_eq!(first, [0.5; 20]);
        assert_eq!(write_cursor(&mut m, b), 40);

        // Overwrite already-committed bytes 0..40 in guest memory, then step down to 500 Hz.
        // The write cursor must not rewind below 40 before the next callback, and the mixer must
        // resample the staged +0.5 source frames 10..20 rather than rereading -0.5 from RAM.
        m.cpu.write(storage, &[64; 40]).unwrap();
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::SetFrequency", &[b, 500]),
            0
        );
        assert_eq!(write_cursor(&mut m, b), 40);
        let mut half_rate = [0.0; 40];
        reader.read(&mut half_rate, 1_000_000);
        assert!(
            half_rate.iter().all(|&sample| sample > 0.2)
                && half_rate[4..].iter().all(|&sample| sample > 0.45),
            "expected staged +0.5 frames after downward retune, got {half_rate:?}"
        );

        // Step up to 2000 Hz while frames 20..40 are still staged; those 20 source frames must
        // play out over 10 device frames as +0.5 without rereading overwritten guest bytes.
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::SetFrequency", &[b, 2000]),
            0
        );
        let mut double_rate = [0.0; 20];
        reader.read(&mut double_rate, 1_000_000);
        assert!(
            double_rate.iter().all(|&sample| sample > 0.2)
                && double_rate[4..].iter().all(|&sample| sample > 0.45),
            "expected staged +0.5 frames after upward retune, got {double_rate:?}"
        );
    }

    #[test]
    // spec: DEV-AUDIO-1
    fn short_ring_caps_staged_lead_at_half_buffer_and_non_looping_end_resets_both_cursors() {
        let format = Format {
            rate: 1000,
            channels: 1,
            bits: 8,
        };

        // Without live output: a 40-ms (40-byte) ring caps lead at 20 bytes (half the buffer)
        // instead of 30 bytes, and a 1-frame ring stays at 0.
        let mut offline = machine();
        let short_offline = buffer(&mut offline, format, 40);
        assert_eq!(
            invoke(
                &mut offline,
                "IDirectSoundBuffer8::Play",
                &[short_offline, 0, 0, 1]
            ),
            0
        );
        assert_eq!(position(&mut offline, short_offline), 0);
        assert_eq!(write_cursor(&mut offline, short_offline), 20);
        let single_frame = buffer(&mut offline, format, 1);
        assert_eq!(
            invoke(
                &mut offline,
                "IDirectSoundBuffer8::Play",
                &[single_frame, 0, 0, 1]
            ),
            0
        );
        assert_eq!(position(&mut offline, single_frame), 0);
        assert_eq!(write_cursor(&mut offline, single_frame), 0);

        // With live output: a 40-byte ring stages 20 bytes on Play, and a non-looping buffer
        // resets both cursors to (0, 0) as soon as the mixer finishes it, even before pump().
        let audio = AudioControl::new(true);
        let mut live = machine_for_process(
            adv_machine::ObjectLimits::default(),
            0x1000,
            Some(audio.clone()),
        );
        let short_live = buffer(&mut live, format, 40);
        let one_shot = buffer(&mut live, format, 40);
        let mut reader = audio.stream().device_reader(1000, 2).unwrap();
        reader.read(&mut [0.0; 4000], 0);
        assert_eq!(
            invoke(
                &mut live,
                "IDirectSoundBuffer8::Play",
                &[short_live, 0, 0, 1]
            ),
            0
        );
        assert_eq!(position(&mut live, short_live), 0);
        assert_eq!(write_cursor(&mut live, short_live), 20);

        assert_eq!(
            invoke(
                &mut live,
                "IDirectSoundBuffer8::SetCurrentPosition",
                &[one_shot, 12]
            ),
            0
        );
        assert_eq!(
            invoke(&mut live, "IDirectSoundBuffer8::Play", &[one_shot, 0, 0, 0]),
            0
        );
        let mut chunk = [0.0; 20];
        reader.read(&mut chunk, 1_000_000);
        // Hardware delay keeps playback clock behind pts, so non-looping play clamps at minimum
        // 12 while staging through all 40 bytes wraps write to 0 (< dwBufferBytes).
        assert_eq!(position(&mut live, one_shot), 12);
        assert_eq!(write_cursor(&mut live, one_shot), 0);

        let mut finish = [0.0; 60];
        reader.read(&mut finish, 1_000_000);
        // While the final 18 mixed frames are still draining through hardware delay, play stays
        // clamped at minimum (12) and write stays at 0 without wrapping past 0.
        assert_eq!(position(&mut live, one_shot), 12);
        assert_eq!(write_cursor(&mut live, one_shot), 0);

        // Once DAC playback passes the final frame, both cursors reset to (0, 0) even before pump.
        reader.read(&mut [0.0; 20], 0);
        assert_eq!(position(&mut live, one_shot), 0);
        assert_eq!(write_cursor(&mut live, one_shot), 0);
    }

    #[test]
    // spec: DEV-AUDIO-1
    fn partial_preload_streaming_plays_sub_30ms_packets_without_silence_hole() {
        let audio = AudioControl::new(true);
        let mut m = machine_for_process(
            adv_machine::ObjectLimits::default(),
            0x1000,
            Some(audio.clone()),
        );
        let b = buffer(
            &mut m,
            Format {
                rate: 1000,
                channels: 1,
                bits: 8,
            },
            100,
        );
        let mut reader = audio.stream().device_reader(1000, 2).unwrap();
        reader.read(&mut [0.0; 4000], 0);

        let lock_write_unlock = |m: &mut Machine<JitCpu>, offset: u32, bytes: &[u8]| {
            let len = bytes.len() as u32;
            assert_eq!(
                invoke(
                    m,
                    "IDirectSoundBuffer8::Lock",
                    &[b, offset, len, DATA + 128, DATA + 132, 0, 0, 0]
                ),
                0
            );
            let p1 = m.cpu.read_u32(DATA + 128).unwrap();
            let n1 = m.cpu.read_u32(DATA + 132).unwrap();
            assert_eq!(n1, len);
            m.cpu.write(p1, bytes).unwrap();
            assert_eq!(
                invoke(m, "IDirectSoundBuffer8::Unlock", &[b, p1, n1, 0, 0]),
                0
            );
        };

        // Preload a single 20-ms packet (< 30 ms write lead), start playback, mix 10 ms, and then
        // deliver packet 2 at 20..40 ms (inside the active 30-ms write-cursor lead).
        lock_write_unlock(&mut m, 0, &[192; 20]);
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 1]),
            0
        );
        let mut first_10ms = [0.0; 20];
        reader.read(&mut first_10ms, 1_000_000);
        assert_eq!(first_10ms, [0.5; 20]);
        assert_eq!(write_cursor(&mut m, b), 40);

        lock_write_unlock(&mut m, 20, &[192; 20]);
        let mut next_30ms = [0.0; 60];
        reader.read(&mut next_30ms, 1_000_000);
        assert_eq!(
            next_30ms, [0.5; 60],
            "sub-30ms streaming packet unlocked inside write lead must not hit pre-staged silence"
        );
    }
}
