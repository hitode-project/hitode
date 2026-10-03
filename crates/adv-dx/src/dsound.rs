//! DirectSound PCM buffers. Guest-visible clocks stay independent of device consumption.
use adv_abi::Registry;
use adv_cpu::backend::{Backend, Guest, GuestExt, Prot};
use adv_device::{AudioControl, AudioEvent, RingState, ring::RingConfig};
use adv_machine::{Conv, Env, Machine, Outcome};
use adv_os::Heaps;
use std::collections::BTreeMap;
use std::io::{Seek, Write};
use std::sync::Arc;

const INVALID: u32 = 0x80070057;
const UNSUPPORTED: u32 = 0x80004001;
const NO_INTERFACE: u32 = 0x80004002;
const INVALID_CALL: u32 = 0x88780032;
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
    ("Initialize", 4),
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
const IUNKNOWN: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
const IID_DEVICE: [u8; 16] = [
    0x83, 0xfa, 0x9a, 0x27, 0x81, 0x49, 0xce, 0x11, 0xa5, 0x21, 0, 0x20, 0xaf, 0x0b, 0xe5, 0x60,
];
const IID_DEVICE8: [u8; 16] = [
    0x93, 0x7e, 0x0a, 0xc5, 0x95, 0xf3, 0x34, 0x48, 0x9e, 0xf6, 0x7f, 0xa9, 0x9d, 0xe5, 0x09, 0x66,
];
const IID_BUFFER: [u8; 16] = [
    0x85, 0xfa, 0x9a, 0x27, 0x81, 0x49, 0xce, 0x11, 0xa5, 0x21, 0, 0x20, 0xaf, 0x0b, 0xe5, 0x60,
];
const IID_BUFFER8: [u8; 16] = [
    0x49, 0xa4, 0x25, 0x68, 0x24, 0x75, 0x82, 0x4d, 0x92, 0x0f, 0x50, 0xe3, 0x6a, 0xb3, 0xab, 0x1e,
];
const IID_NOTIFY: [u8; 16] = [
    0x83, 0x07, 0x21, 0xb0, 0xcd, 0x89, 0xd0, 0x11, 0xaf, 8, 0, 0xa0, 0xc9, 0x25, 0xcd, 0x16,
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
    Device,
    Buffer,
    Notify(u32),
}
struct Object {
    refs: u32,
    kind: Kind,
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
struct Buffer {
    audio_id: u64,
    voice: Option<Arc<RingState>>,
    playback_clock: Arc<adv_device::clock::PlaybackClock>,
    generation: u64,
    storage: u32,
    len: u32,
    flags: u32,
    format: Format,
    frequency: u32,
    volume: i32,
    pan: i32,
    playing: bool,
    looping: bool,
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
    fn cursor(&self, now: u64) -> u32 {
        if self.playback_clock.uses_device_time()
            && let Some(position) = self
                .voice
                .as_ref()
                .and_then(|voice| voice.cursor(now, self.generation))
        {
            return position;
        }
        if self.len == 0 {
            0
        } else if self.looping {
            (self.progress(now) % u64::from(self.len)) as u32
        } else {
            let progress = self.progress(now);
            if progress >= u64::from(self.len) {
                0
            } else {
                progress as u32
            }
        }
    }
    fn stop(&mut self, now: u64) {
        self.base = self
            .cursor(now)
            .min(self.len.saturating_sub(self.format.align()));
        self.playing = false;
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
    tables: [u32; 3],
    objects: BTreeMap<u32, Object>,
    buffers: BTreeMap<u32, Buffer>,
    pub audio: AudioControl,
    next_audio_id: u64,
    pending_release: BTreeMap<u64, (u32, bool)>,
    pending_notices: Vec<(u64, u32, u64, u64)>,
    pub queued_samples: u64,
    pub nonzero_samples: u64,
    pub capture_digests: bool,
    pub digests: Vec<PcmDigest>,
    trace: Option<std::io::BufWriter<std::fs::File>>,
    trace_pcm_dir: Option<std::path::PathBuf>,
    trace_pcm_chunks: bool,
    playback_clock: Arc<adv_device::clock::PlaybackClock>,
}
impl Sound {
    pub fn source_status(&self, guest_now: u64) -> AudioSourceStatus {
        let now = self.playback_clock.now_us(guest_now);
        let mut status = AudioSourceStatus::default();
        for buffer in self.buffers.values().filter(|buffer| buffer.len != 0) {
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
    for name in ["#1", "DirectSoundCreate", "DirectSoundCreate8"] {
        reg.add("dsound.dll", name, Conv::Stdcall { argc: 3 }, call);
    }
    for (prefix, methods) in [
        ("IDirectSound8", DEVICE),
        ("IDirectSoundBuffer8", BUFFER),
        ("IDirectSoundNotify", NOTIFY),
    ] {
        for &(name, argc) in methods {
            reg.add(
                "dsound.dll",
                &format!("{prefix}::{name}"),
                Conv::Stdcall { argc },
                call,
            );
        }
    }
}
pub fn install<B: Backend>(m: &mut Machine<B>) -> Result<(), String> {
    let trace = std::env::var_os("ADV_AUDIO_TRACE")
        .map(|path| {
            std::fs::File::create(&path)
                .map(std::io::BufWriter::new)
                .map_err(|error| format!("cannot create ADV_AUDIO_TRACE {path:?}: {error}"))
        })
        .transpose()?;
    let trace_pcm_dir = if trace.is_some() {
        std::env::var_os("ADV_AUDIO_DUMP_DIR")
            .map(|path| {
                std::fs::create_dir_all(&path)
                    .map(|()| std::path::PathBuf::from(&path))
                    .map_err(|error| format!("cannot create ADV_AUDIO_DUMP_DIR {path:?}: {error}"))
            })
            .transpose()?
    } else {
        None
    };
    let mut tables = [0; 3];
    for (i, (prefix, methods)) in [
        ("IDirectSound8", DEVICE),
        ("IDirectSoundBuffer8", BUFFER),
        ("IDirectSoundNotify", NOTIFY),
    ]
    .into_iter()
    .enumerate()
    {
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
    let audio = AudioControl::new(!m.sys.clock.is_virtual());
    let playback_clock = audio.playback_clock();
    m.ext.insert(Sound {
        enabled: false,
        tables,
        objects: BTreeMap::new(),
        buffers: BTreeMap::new(),
        audio,
        next_audio_id: 1,
        pending_release: BTreeMap::new(),
        pending_notices: Vec::new(),
        queued_samples: 0,
        nonzero_samples: 0,
        capture_digests: false,
        digests: Vec::new(),
        trace,
        trace_pcm_dir,
        trace_pcm_chunks: std::env::var_os("ADV_AUDIO_DUMP_CHUNKS").is_some(),
        playback_clock,
    });
    Ok(())
}
fn alloc(e: &mut Env<'_>, len: u32) -> Result<u32, String> {
    let heaps = e.ext.get_mut::<Heaps>();
    heaps
        .alloc(e.cpu, &mut e.sys.vm, heaps.process_heap, len, true)
        .ok_or("audio heap exhausted".into())
}
fn free(e: &mut Env<'_>, ptr: u32) {
    let heaps = e.ext.get_mut::<Heaps>();
    heaps.free(e.cpu, &mut e.sys.vm, heaps.process_heap, ptr);
}
fn release(e: &mut Env<'_>, ptr: u32) -> u32 {
    let s = e.ext.get_mut::<Sound>();
    let Some(obj) = s.objects.get_mut(&ptr) else {
        return 0;
    };
    obj.refs -= 1;
    let refs = obj.refs;
    if refs != 0 {
        return refs;
    }
    let kind = s.objects.remove(&ptr).unwrap().kind;
    if let Some(buffer) = s.buffers.remove(&ptr)
        && buffer.storage != 0
    {
        let queued = s.audio.remove_pcm(buffer.audio_id);
        s.pending_release
            .insert(buffer.audio_id, (buffer.storage, queued));
    }
    free(e, ptr);
    if let Kind::Notify(parent) = kind {
        release(e, parent);
    }
    0
}
fn object(e: &mut Env<'_>, kind: Kind) -> Result<u32, String> {
    let i = match kind {
        Kind::Device => 0,
        Kind::Buffer => 1,
        Kind::Notify(_) => 2,
    };
    let table = e.ext.get::<Sound>().tables[i];
    let ptr = alloc(e, 4)?;
    e.cpu.write_u32(ptr, table).map_err(|e| e.to_string())?;
    e.ext
        .get_mut::<Sound>()
        .objects
        .insert(ptr, Object { refs: 1, kind });
    Ok(ptr)
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
        let ctx = e.sys.current;
        let caller = e.caller();
        let files: Vec<_> = if matches!(name, "Play" | "Stop" | "SetCurrentPosition" | "Release") {
            e.sys
                .objects
                .hosts::<adv_os::vfs::FileObj>()
                .map(|(handle, file)| {
                    let mut native = &file.file;
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
        let snapshot = s.buffers.get(&a).map(|buf| {
            (
                buf.storage,
                buf.len,
                buf.playing,
                buf.base,
                buf.queued_frames,
            )
        });
        let cursor = s.buffers.get(&a).map(|buf| buf.cursor(now));
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
            return Ok(0x88780078);
        }
        if a != 0 {
            return Ok(0x88780078);
        }
        if c != 0 {
            return Ok(0x80040110);
        }
        let obj = object(e, Kind::Device)?;
        e.cpu.write_u32(b, obj).map_err(|e| e.to_string())?;
        return Ok(0);
    }
    let Some(obj) = e.ext.get::<Sound>().objects.get(&a) else {
        return Ok(INVALID);
    };
    let kind = obj.kind;
    if name == "AddRef" {
        let o = e.ext.get_mut::<Sound>().objects.get_mut(&a).unwrap();
        o.refs += 1;
        return Ok(o.refs);
    }
    if name == "Release" {
        return Ok(release(e, a));
    }
    if name == "QueryInterface" {
        e.cpu.write_u32(c, 0).map_err(|e| e.to_string())?;
        let mut iid = [0; 16];
        e.cpu.read(b, &mut iid).map_err(|e| e.to_string())?;
        let parent = if let Kind::Notify(parent) = kind {
            parent
        } else {
            a
        };
        let supported = iid == IUNKNOWN
            || match kind {
                Kind::Device => iid == IID_DEVICE || iid == IID_DEVICE8,
                Kind::Buffer | Kind::Notify(_) => iid == IID_BUFFER || iid == IID_BUFFER8,
            };
        let ptr = if supported {
            e.ext
                .get_mut::<Sound>()
                .objects
                .get_mut(&parent)
                .unwrap()
                .refs += 1;
            parent
        } else if iid == IID_NOTIFY && matches!(kind, Kind::Buffer | Kind::Notify(_)) {
            if e.ext.get::<Sound>().buffers[&parent].flags & 0x100 == 0 {
                return Ok(NO_INTERFACE);
            }
            if matches!(kind, Kind::Notify(_)) {
                e.ext.get_mut::<Sound>().objects.get_mut(&a).unwrap().refs += 1;
                a
            } else {
                let ptr = object(e, Kind::Notify(a))?;
                e.ext.get_mut::<Sound>().objects.get_mut(&a).unwrap().refs += 1;
                ptr
            }
        } else {
            return Ok(NO_INTERFACE);
        };
        e.cpu.write_u32(c, ptr).map_err(|e| e.to_string())?;
        return Ok(0);
    }
    if matches!(kind, Kind::Device) {
        return match name {
            "CreateSoundBuffer" => {
                if e.arg(3) != 0 {
                    return Ok(0x80040110);
                }
                e.cpu.write_u32(c, 0).map_err(|e| e.to_string())?;
                let size = e.cpu.read_u32(b).map_err(|e| e.to_string())?;
                if !matches!(size, 20 | 36) {
                    return Ok(INVALID);
                }
                let flags = e.cpu.read_u32(b + 4).map_err(|e| e.to_string())?;
                // This device implements software PCM, without 3D, hardware voices or FX.
                if flags
                    & !(0x1
                        | 0x2
                        | 0x8
                        | 0x20
                        | 0x40
                        | 0x80
                        | 0x100
                        | 0x4000
                        | 0x8000
                        | 0x10000
                        | 0x80000)
                    != 0
                {
                    return Ok(UNSUPPORTED);
                }
                let len = e.cpu.read_u32(b + 8).map_err(|e| e.to_string())?;
                let fp = e.cpu.read_u32(b + 16).map_err(|e| e.to_string())?;
                let primary = flags & 1 != 0;
                let format = if primary && fp == 0 {
                    Format::default()
                } else {
                    match Format::read(e.cpu, fp) {
                        Ok(f) => f,
                        Err(_) => return Ok(0x88780064),
                    }
                };
                if (!primary
                    && (len == 0 || len > 32 * 1024 * 1024 || !len.is_multiple_of(format.align())))
                    || (primary && len != 0)
                {
                    return Ok(INVALID);
                }
                let (storage, region) = if primary {
                    (0, None)
                } else {
                    let mapped_len = adv_machine::vm::align_up(len, 4096);
                    let base = e
                        .sys
                        .vm
                        .reserve(mapped_len)
                        .ok_or("audio address space exhausted")?;
                    let region = match e.cpu.map_shared_audio(base, mapped_len) {
                        Ok(region) => region,
                        Err(error) => {
                            let _ = e.sys.vm.free(e.cpu, base);
                            return Err(format!("shared audio mapping: {error}"));
                        }
                    };
                    (base, Some(region))
                };
                if !primary && format.bits == 8 {
                    let silence = vec![128; len as usize];
                    e.cpu.write(storage, &silence).map_err(|e| e.to_string())?;
                }
                let ptr = object(e, Kind::Buffer)?;
                let s = e.ext.get_mut::<Sound>();
                let audio_id = s.next_audio_id;
                s.next_audio_id = s
                    .next_audio_id
                    .checked_add(1)
                    .ok_or("audio source IDs exhausted")?;
                let voice = region
                    .map(|region| {
                        s.audio.add_ring(
                            audio_id,
                            RingConfig {
                                view: region.read_alias,
                                bytes: len,
                                channels: format.channels,
                                bits: format.bits,
                                frequency: format.rate,
                                output_rate: 44100,
                                notifications: Vec::new(),
                            },
                        )
                    })
                    .transpose()?;
                s.buffers.insert(
                    ptr,
                    Buffer {
                        audio_id,
                        voice,
                        playback_clock: s.playback_clock.clone(),
                        generation: 0,
                        storage,
                        len,
                        flags,
                        format,
                        frequency: format.rate,
                        volume: 0,
                        pan: 0,
                        playing: false,
                        looping: false,
                        base: 0,
                        anchor: now,
                        queued_frames: 0,
                        notice_bytes: 0,
                        notifications: Vec::new(),
                        lock: None,
                        trace_pcm: None,
                        trace_generation: 0,
                        trace_chunks: None,
                    },
                );
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
            "Initialize" => Ok(0x88780082),
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
        let len = e.ext.get::<Sound>().buffers[&parent].len;
        if e.ext.get::<Sound>().buffers[&parent].playing {
            return Ok(INVALID_CALL);
        }
        let mut entries = Vec::new();
        for i in 0..b {
            let offset = e.cpu.read_u32(c + i * 8).map_err(|e| e.to_string())?;
            let handle = e.cpu.read_u32(c + i * 8 + 4).map_err(|e| e.to_string())?;
            if (offset != u32::MAX && offset >= len)
                || !matches!(
                    e.sys.objects.get(handle),
                    Some(adv_machine::Object::Event { .. })
                )
            {
                return Ok(INVALID);
            }
            entries.push((offset, handle));
        }
        e.ext
            .get_mut::<Sound>()
            .buffers
            .get_mut(&parent)
            .unwrap()
            .notifications = entries;
        let s = e.ext.get_mut::<Sound>();
        let buffer = &s.buffers[&parent];
        s.audio
            .set_notifications(buffer.audio_id, buffer.notifications.clone());
        if let Some(trace) = s.trace.as_mut() {
            let _ = writeln!(
                trace,
                "notifications us={now} id={parent:08x} entries={:?}",
                s.buffers[&parent].notifications
            );
        }
        return Ok(0);
    }
    let args: [u32; 8] = std::array::from_fn(|i| e.arg(i as u32));
    let s = e.ext.get_mut::<Sound>();
    let buf = s.buffers.get_mut(&a).unwrap();
    let value = match name {
        "GetCaps" => {
            if e.cpu.read_u32(b).map_err(|e| e.to_string())? < 20 {
                return Ok(INVALID);
            }
            for (i, v) in [20, buf.flags | 8, buf.len, 0, 0].into_iter().enumerate() {
                e.cpu
                    .write_u32(b + i as u32 * 4, v)
                    .map_err(|e| e.to_string())?;
            }
            0
        }
        "GetCurrentPosition" => {
            let pos = buf.cursor(now);
            if b != 0 {
                e.cpu.write_u32(b, pos).map_err(|e| e.to_string())?;
            }
            if c != 0 {
                let write = if buf.len == 0 {
                    0
                } else {
                    (pos + buf.frequency / 25 * buf.format.align()) % buf.len
                };
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
                "GetFrequency" => buf.frequency,
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
            s.audio.set_stereo_gain(buf.audio_id, buf.stereo_gain());
            0
        }
        "SetPan" => {
            if !(-10000..=10000).contains(&(b as i32)) {
                return Ok(INVALID);
            }
            buf.pan = b as i32;
            s.audio.set_stereo_gain(buf.audio_id, buf.stereo_gain());
            0
        }
        "SetFrequency" => {
            let freq = if b == 0 { buf.format.rate } else { b };
            if !(100..=200000).contains(&freq) {
                return Ok(INVALID);
            }
            buf.base = buf.cursor(now);
            buf.anchor = now;
            buf.frequency = freq;
            buf.queued_frames = 0;
            buf.notice_bytes = u64::from(buf.base);
            s.audio.set_frequency(buf.audio_id, freq)?;
            buf.generation = buf.generation.wrapping_add(1);
            s.audio.change_generation(buf.audio_id, buf.generation);
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
            buf.base = b / buf.format.align() * buf.format.align();
            buf.anchor = now;
            buf.queued_frames = 0;
            buf.notice_bytes = u64::from(buf.base);
            s.audio.set_position(buf.audio_id, buf.base);
            buf.generation = buf.generation.wrapping_add(1);
            s.audio.change_generation(buf.audio_id, buf.generation);
            0
        }
        "Play" => {
            if b != 0 || c != 0 || args[3] & !1 != 0 {
                return Ok(INVALID);
            }
            if !buf.looping && buf.len != 0 && buf.progress(now) >= u64::from(buf.len) {
                buf.playing = false;
                buf.base = 0;
                s.audio.stop_ring(buf.audio_id);
                s.audio.set_position(buf.audio_id, 0);
            }
            if !buf.playing {
                // A fresh start is a new playback generation. A naturally ended
                // buffer can retain unconsumed PCM in the device latency window;
                // it must not survive reuse of the same DirectSound object.
                buf.generation = buf.generation.wrapping_add(1);
                if let Some(trace) = s.trace.as_mut() {
                    let mut bytes = vec![0; buf.len as usize];
                    if buf.len != 0 {
                        e.cpu
                            .read(buf.storage, &mut bytes)
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
                        buf.storage,
                        buf.len,
                        buf.base,
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
                        .read(buf.storage, &mut bytes)
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
                buf.anchor = now;
                buf.queued_frames = 0;
                buf.notice_bytes = u64::from(buf.base);
            }
            buf.playing = true;
            buf.looping = args[3] & 1 != 0;
            s.audio.play_ring(buf.audio_id, buf.looping, buf.generation);
            0
        }
        "Stop" => {
            buf.stop(now);
            s.audio.stop_ring(buf.audio_id);
            buf.generation = buf.generation.wrapping_add(1);
            s.audio.change_generation(buf.audio_id, buf.generation);
            for &(offset, handle) in &buf.notifications {
                if offset == u32::MAX {
                    e.sys
                        .set_event_with_priority(handle, adv_machine::WakePriority::Audio);
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
                (buf.cursor(now) + buf.frequency / 25 * buf.format.align()) % buf.len
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
            let lock = Lock {
                p1: buf.storage + offset,
                n1,
                p2: if n2 != 0 { buf.storage } else { 0 },
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
            s.audio.written(buf.audio_id, offset, n1 + n2);
            0
        }
        "Unlock" => {
            if !buf.lock.is_some_and(|lock| {
                lock.p1 == b && lock.p2 == args[3] && c <= lock.n1 && args[4] <= lock.n2
            }) {
                return Ok(INVALID);
            }
            if let Some(lock) = buf.lock.take()
                && let Some(dump) = buf.trace_chunks.as_mut()
            {
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
                        lock.p1 - buf.storage,
                        c + args[4],
                        dump.path
                    );
                }
            }
            0
        }
        _ => return Err(format!("unimplemented audio method {full}")),
    };
    Ok(value)
}
fn call(e: &mut Env<'_>) -> Outcome {
    match result(e) {
        Ok(v) => Outcome::Return(v),
        Err(err) => Outcome::Fail(err),
    }
}

/// Queue a small owned PCM window; the device callback never borrows guest memory.
pub fn pump<B: Backend>(m: &mut Machine<B>) -> Result<Option<u64>, String> {
    let guest_now = m.sys.clock.now_us();
    let s = m.ext.get_mut::<Sound>();
    s.audio.sync_clock(guest_now);
    for (&source, (_, queued)) in &mut s.pending_release {
        if !*queued {
            *queued = s.audio.remove_pcm(source);
        }
    }
    (s.queued_samples, s.nonzero_samples) = s.audio.stream().rendered_samples();
    let now = s.playback_clock.now_us(guest_now);
    for event in s.audio.take_events() {
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
                if let Some((storage, _)) = s.pending_release.remove(&source) {
                    m.sys
                        .vm
                        .free(&mut m.cpu, storage)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
    }
    let mut next = None;
    if !s.playback_clock.uses_device_time() {
        for buffer in s
            .buffers
            .values_mut()
            .filter(|buffer| buffer.playing && buffer.len != 0)
        {
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
                    m.sys
                        .set_event_with_priority(event, adv_machine::WakePriority::Audio);
                    next = Some(guest_now);
                }
            }
            buffer.notice_bytes = progress;
        }
    }
    s.pending_notices
        .retain(|&(source, event, due, generation)| {
            if !s
                .buffers
                .values()
                .any(|buffer| buffer.audio_id == source && buffer.generation == generation)
            {
                return false;
            }
            if due <= now {
                m.sys
                    .set_event_with_priority(event, adv_machine::WakePriority::Audio);
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
    for buffer in s.buffers.values_mut() {
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
            .buffers
            .values()
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
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::JitCpu;
    use adv_cpu::backend::Reg;
    use adv_machine::{Clock, Object as WaitObject};
    const DATA: u32 = 0x500000;
    const STACK: u32 = 0x600000;

    fn machine() -> Machine<JitCpu> {
        let mut reg = Registry::default();
        adv_win32::register(&mut reg);
        register(&mut reg);
        let mut m = Machine::new(
            JitCpu::new(adv_machine::layout::PARK_ADDR, true).unwrap(),
            reg,
            Clock::virtual_time(),
        )
        .unwrap();
        adv_win32::Win32::install(
            &mut m,
            adv_win32::Win32Config::new(".", "audio-test.exe", "target/audio-test"),
        )
        .unwrap();
        install(&mut m).unwrap();
        m.ext.get_mut::<Sound>().enabled = true;
        m.cpu.map(DATA, 4096, Prot::RW).unwrap();
        m.cpu.map(STACK, 4096, Prot::RW).unwrap();
        m
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
    fn advance(m: &mut Machine<JitCpu>, us: u64) {
        m.sys.clock.advance_to(m.sys.clock.now_us() + us);
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
        assert!(m.ext.get::<Sound>().buffers[&b].lock.is_none());
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::Lock",
                &[b, 0, 20, 0, DATA + 132, 0, 0, 0]
            ),
            INVALID
        );
        assert!(m.ext.get::<Sound>().buffers[&b].lock.is_none());
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
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::SetFrequency", &[b, 2000]),
            0
        );
        advance(&mut m, 25_000);
        assert_eq!(position(&mut m, b), 300);
        invoke(&mut m, "IDirectSoundBuffer8::Stop", &[b]);
        advance(&mut m, 500_000);
        assert_eq!(position(&mut m, b), 300);
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[b, 0, 0, 1]);
        advance(&mut m, 25_000);
        assert_eq!(position(&mut m, b), 100);
        invoke(&mut m, "IDirectSoundBuffer8::SetCurrentPosition", &[b, 399]);
        assert_eq!(position(&mut m, b), 396);
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
            let ptr = m.ext.get::<Sound>().buffers[&b].storage;
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
            m.cpu
                .write(
                    ptr,
                    &vec![if format.bits == 8 { 128 } else { 0 }; bytes.len()],
                )
                .unwrap();
            stream.read_stereo(&mut out, 1000);
            assert_eq!(
                out, [0.0; 40],
                "guest writes stay visible through the independent read alias"
            );
            assert_eq!(stream.rendered_samples(), (80, 40));
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
        let ptr = m.ext.get::<Sound>().buffers[&b].storage;
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
    fn stop_seek_and_buffer_reuse_cannot_emit_an_older_voice_generation() {
        let mut m = machine();
        let format = Format {
            rate: 1000,
            channels: 1,
            bits: 8,
        };
        let voice = buffer(&mut m, format, 1000);
        let music = buffer(&mut m, format, 1000);
        let voice_storage = m.ext.get::<Sound>().buffers[&voice].storage;
        let music_storage = m.ext.get::<Sound>().buffers[&music].storage;
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
        let storage = m.ext.get::<Sound>().buffers[&voice].storage;
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
        let storage = m.ext.get::<Sound>().buffers[&voice].storage;
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
    fn releasing_a_voice_cannot_leak_its_queue_into_the_next_buffer() {
        let mut m = machine();
        let format = Format {
            rate: 1000,
            channels: 1,
            bits: 8,
        };
        let voice = buffer(&mut m, format, 1000);
        let music = buffer(&mut m, format, 1000);
        let voice_storage = m.ext.get::<Sound>().buffers[&voice].storage;
        let music_storage = m.ext.get::<Sound>().buffers[&music].storage;
        m.cpu.write(voice_storage, &[192; 1000]).unwrap();
        m.cpu.write(music_storage, &[160; 1000]).unwrap();
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[voice, 0, 0, 0]);
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[music, 0, 0, 1]);
        pump(&mut m).unwrap();
        assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Release", &[voice]), 0);
        let next_voice = buffer(&mut m, format, 1000);
        let next_storage = m.ext.get::<Sound>().buffers[&next_voice].storage;
        m.cpu.write(next_storage, &[64; 1000]).unwrap();
        invoke(&mut m, "IDirectSoundBuffer8::Play", &[next_voice, 0, 0, 0]);
        pump(&mut m).unwrap();
        let stream: adv_device::AudioStream = m.ext.get::<Sound>().audio.stream();
        let mut out = [0.0; 20];
        stream.read_stereo(&mut out, 1000);
        assert_eq!(out, [-0.25; 20]);
    }
    #[test]
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
            let buffer = &m.ext.get::<Sound>().buffers[&voice];
            (buffer.audio_id, buffer.storage)
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
        assert!(!m.ext.get::<Sound>().objects.contains_key(&voice));
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
        assert!(!m.ext.get::<Sound>().buffers[&b].playing);
        assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Release", &[b]), 1);
        assert_eq!(invoke(&mut m, "IDirectSoundNotify::Release", &[notify]), 0);
        assert!(!m.ext.get::<Sound>().buffers.contains_key(&b));
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
        let ptr = m.ext.get::<Sound>().buffers[&b].storage;
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
        assert!(m.ext.get::<Sound>().buffers[&b].playing);
    }
}
