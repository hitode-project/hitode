//! DirectSound PCM buffers. Guest-visible clocks stay independent of device consumption.
use pal_audio::mixer::{Mixer, MixerHandle, PcmBlock};
use pal_cpu::{Backend, Guest, GuestExt, Prot};
use pal_machine::{Conv, Env, Machine, Outcome, Registry};
use pal_win32::Win32;
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

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
struct Buffer {
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

pub(crate) struct Sound {
    pub enabled: bool,
    tables: [u32; 3],
    objects: BTreeMap<u32, Object>,
    buffers: BTreeMap<u32, Buffer>,
    pub mixer: MixerHandle,
    pub queued_samples: u64,
    pub nonzero_samples: u64,
    pub capture_digests: bool,
    pub digests: Vec<PcmDigest>,
    trace: Option<std::io::BufWriter<std::fs::File>>,
}
pub(crate) fn register(reg: &mut Registry) {
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
pub(crate) fn install<B: Backend>(m: &mut Machine<B>) -> Result<(), String> {
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
    m.ext.insert(Sound {
        enabled: false,
        tables,
        objects: BTreeMap::new(),
        buffers: BTreeMap::new(),
        mixer: Arc::new(Mutex::new(Mixer::default())),
        queued_samples: 0,
        nonzero_samples: 0,
        capture_digests: false,
        digests: Vec::new(),
        trace: std::env::var_os("ADV_AUDIO_TRACE")
            .and_then(|path| std::fs::File::create(path).ok())
            .map(std::io::BufWriter::new),
    });
    Ok(())
}
fn alloc(e: &mut Env<'_>, len: u32) -> Result<u32, String> {
    let w = e.ext.get_mut::<Win32>();
    w.heaps
        .alloc(e.cpu, &mut e.sys.vm, w.heaps.process_heap, len, true)
        .ok_or("audio heap exhausted".into())
}
fn free(e: &mut Env<'_>, ptr: u32) {
    let w = e.ext.get_mut::<Win32>();
    w.heaps
        .free(e.cpu, &mut e.sys.vm, w.heaps.process_heap, ptr);
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
    let storage = s.buffers.remove(&ptr).map(|buffer| buffer.storage);
    if storage.is_some() {
        s.mixer.lock().unwrap().remove_pcm(u64::from(ptr));
    }
    free(e, ptr);
    if let Some(storage) = storage
        && storage != 0
    {
        free(e, storage);
    }
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
    let full = e.sys.thunks.get(e.thunk).name.clone();
    let name = full.split("::").last().unwrap();
    let a = e.arg(0);
    let b = e.arg(1);
    let c = e.arg(2);
    let now = e.sys.clock.now_us();
    if e.ext.get::<Sound>().trace.is_some() {
        let args: [u32;8] = std::array::from_fn(|i|e.arg(i as u32));
        let ctx=e.sys.current;
        let s=e.ext.get_mut::<Sound>();
        let snapshot=s.buffers.get(&a).map(|buf|(buf.storage,buf.len,buf.playing,buf.base,buf.queued_frames));
        if let Some(trace)=s.trace.as_mut() {
            let _=writeln!(trace,"api us={now} ctx={ctx} method={name} args={args:08x?} buffer={snapshot:?}");
            if matches!(name,"Play"|"Stop"|"Release") {let _=trace.flush();}
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
                let storage = if primary { 0 } else { alloc(e, len)? };
                if !primary && format.bits == 8 {
                    let silence = vec![128; len as usize];
                    e.cpu.write(storage, &silence).map_err(|e| e.to_string())?;
                }
                let ptr = object(e, Kind::Buffer)?;
                e.ext.get_mut::<Sound>().buffers.insert(
                    ptr,
                    Buffer {
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
                    Some(pal_machine::Object::Event { .. })
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
            s.mixer
                .lock()
                .unwrap()
                .set_stereo_gain(u64::from(a), buf.stereo_gain());
            0
        }
        "SetPan" => {
            if !(-10000..=10000).contains(&(b as i32)) {
                return Ok(INVALID);
            }
            buf.pan = b as i32;
            s.mixer
                .lock()
                .unwrap()
                .set_stereo_gain(u64::from(a), buf.stereo_gain());
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
            s.mixer.lock().unwrap().flush_pcm(u64::from(a));
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
            s.mixer.lock().unwrap().flush_pcm(u64::from(a));
            0
        }
        "Play" => {
            if b != 0 || c != 0 || args[3] & !1 != 0 {
                return Ok(INVALID);
            }
            if !buf.looping && buf.len != 0 && buf.progress(now) >= u64::from(buf.len) {
                buf.playing = false;
                buf.base = 0;
            }
            if !buf.playing {
                // A fresh start is a new playback generation. A naturally ended
                // buffer can retain unconsumed PCM in the device latency window;
                // it must not survive reuse of the same DirectSound object.
                s.mixer.lock().unwrap().flush_pcm(u64::from(a));
                if let Some(trace)=s.trace.as_mut() {
                    let mut bytes=vec![0;buf.len as usize];
                    if buf.len != 0 {e.cpu.read(buf.storage,&mut bytes).map_err(|error|error.to_string())?;}
                    let hash=bytes.iter().fold(0xcbf29ce484222325u64,|hash,byte|(hash^u64::from(*byte)).wrapping_mul(0x100000001b3));
                    let _=writeln!(trace,"start us={now} id={a:08x} bytes={} hash={hash:016x}",buf.len);
                    buf.trace_pcm=Some(bytes);
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
            0
        }
        "Stop" => {
            buf.stop(now);
            s.mixer.lock().unwrap().flush_pcm(u64::from(a));
            for &(offset, handle) in &buf.notifications {
                if offset == u32::MAX {
                    e.sys.set_event(handle);
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
            0
        }
        "Unlock" => {
            if !buf.lock.is_some_and(|lock| {
                lock.p1 == b && lock.p2 == args[3] && c <= lock.n1 && args[4] <= lock.n2
            }) {
                return Ok(INVALID);
            }
            buf.lock = None;
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
pub(crate) fn pump<B: Backend>(m: &mut Machine<B>) -> Result<Option<u64>, String> {
    let now = m.sys.clock.now_us();
    let s = m.ext.get_mut::<Sound>();
    if !s.enabled {
        return Ok(None);
    }
    let mut next = None;
    let mut events = Vec::new();
    // Guest reads and PCM conversion must stay outside the device queue lock.
    // The callback uses try_lock, so a long producer critical section emits silence.
    s.mixer.lock().unwrap().sync_clock(now);
    for (&id, buf) in &mut s.buffers {
        if !buf.playing || buf.len == 0 {
            continue;
        }
        let progress = if buf.looping {
            buf.progress(now)
        } else {
            buf.progress(now).min(u64::from(buf.len))
        };
        let old = buf.notice_bytes;
        let total = u64::from(buf.len);
        for &(offset, event) in &buf.notifications {
            if offset == u32::MAX {
                continue;
            }
            let first = (old / total) * total + u64::from(offset);
            let first = if first < old { first + total } else { first };
            if first < progress {
                events.push(event);
            }
        }
        buf.notice_bytes = progress;
        if !buf.looping && progress >= total {
            buf.playing = false;
            buf.base = 0;
            s.mixer.lock().unwrap().end_pcm(u64::from(id));
            events.extend(
                buf.notifications
                    .iter()
                    .filter(|n| n.0 == u32::MAX)
                    .map(|n| n.1),
            );
            continue;
        }
        next = Some(now + 10_000);
        // A software DirectSound Lock grants a write region; it does not pause
        // playback. Verified with the native Windows audio_unlock_probe fixture.
        let elapsed_frames = now.saturating_sub(buf.anchor) * u64::from(buf.frequency) / 1_000_000;
        if buf.queued_frames + u64::from(buf.frequency) / 20 < elapsed_frames {
            buf.queued_frames = elapsed_frames;
            s.mixer.lock().unwrap().flush_pcm(u64::from(id));
        }
        let mut end = elapsed_frames + u64::from(buf.frequency) / 25;
        if !buf.looping {
            end = end.min(u64::from((buf.len - buf.base) / buf.format.align()));
        }
        let count = end
            .saturating_sub(buf.queued_frames)
            .min(u64::from(buf.frequency) / 10) as usize;
        if count == 0 {
            continue;
        }
        let align = buf.format.align() as usize;
        let start = (u64::from(buf.base) + buf.queued_frames * align as u64) % total;
        let mut bytes = vec![0; count * align];
        let mut copied = 0;
        let mut offset = start as usize;
        while copied < bytes.len() {
            let len = (buf.len as usize - offset).min(bytes.len() - copied);
            m.cpu
                .read(
                    buf.storage + offset as u32,
                    &mut bytes[copied..copied + len],
                )
                .map_err(|e| e.to_string())?;
            copied += len;
            offset = 0;
        }
        let mut samples = Vec::with_capacity(count * 2);
        for frame in bytes.chunks_exact(align) {
            let sample = |channel: usize| {
                let at = channel * usize::from(buf.format.bits) / 8;
                if buf.format.bits == 8 {
                    (f32::from(frame[at]) - 128.0) / 128.0
                } else {
                    f32::from(i16::from_le_bytes([frame[at], frame[at + 1]])) / 32768.0
                }
            };
            let l = sample(0);
            let r = if buf.format.channels == 2 {
                sample(1)
            } else {
                l
            };
            samples.extend([l, r]);
        }
        let nonzero = samples.iter().filter(|v| v.abs() > 0.00001).count();
        if let Some(trace)=s.trace.as_mut() {
            let hash=bytes.iter().fold(0xcbf29ce484222325u64,|hash,byte|(hash^u64::from(*byte)).wrapping_mul(0x100000001b3));
            let changed=buf.trace_pcm.as_ref().map_or(0,|snapshot|bytes.iter().enumerate().filter(|(n,byte)|**byte!=snapshot[(start as usize+n)%snapshot.len()]).count());
            let _=writeln!(trace,"pcm us={now} id={id:08x} storage={:08x} offset={start} frames={} count={count} hash={hash:016x} nonzero={nonzero} changed={changed}",buf.storage,buf.queued_frames);
        }
        let len = samples.len();
        let block = PcmBlock {
            stream_id: u64::from(id),
            guest_pts_us: buf.anchor + buf.queued_frames * 1_000_000 / u64::from(buf.frequency),
            sample_rate: buf.frequency,
            channels: 2,
            samples,
        };
        let mut mixer = s.mixer.lock().unwrap();
        if mixer.push_pcm(block).is_ok() {
            mixer.set_stereo_gain(u64::from(id), buf.stereo_gain());
            buf.queued_frames += count as u64;
            s.queued_samples += len as u64;
            s.nonzero_samples += nonzero as u64;
        }
    }
    if !events.is_empty() {
        // Resume an event waiter without adding a second audio polling interval.
        next = Some(now);
    }
    for event in events {
        m.sys.set_event(event);
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_jit::pal::JitCpu;
    use pal_cpu::Reg;
    use pal_machine::{Clock, Object as WaitObject};
    const DATA: u32 = 0x500000;
    const STACK: u32 = 0x600000;

    fn machine() -> Machine<JitCpu> {
        let mut reg = Registry::default();
        pal_win32::register(&mut reg);
        register(&mut reg);
        let mut m = Machine::new(
            JitCpu::new(pal_machine::layout::PARK_ADDR, true).unwrap(),
            reg,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut m,
            pal_win32::Win32Config::new(".", "audio-test.exe", "target/audio-test"),
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
        assert_eq!(m.ext.get::<Sound>().nonzero_samples, 80);
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
    fn unsigned_mono_and_signed_stereo_pcm_reach_the_owned_mixer() {
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
            assert!(pump(&mut m).unwrap().is_some());
            // Owned blocks survive subsequent guest writes; the callback does no guest I/O.
            m.cpu.write(ptr, &vec![0; bytes.len()]).unwrap();
            let stream: pal_audio::AudioStream = m.ext.get::<Sound>().mixer.clone().into();
            let mut out = [0.0; 40];
            stream.read_stereo(&mut out, 1000);
            for frame in out.as_chunks::<2>().0 {
                assert_eq!(frame[0], 0.5);
                assert_eq!(frame[1], if format.channels == 1 { 0.5 } else { -0.25 });
            }
            assert_eq!(m.ext.get::<Sound>().nonzero_samples, 80);
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
        let stream: pal_audio::AudioStream = m.ext.get::<Sound>().mixer.clone().into();
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
            faded.iter().all(|value| (*value - 0.05).abs() < 1e-6),
            "volume change introduced a silent gap: {faded:?}"
        );
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::SetPan", &[b, 10000]),
            0
        );
        let mut panned = [0.0; 10];
        stream.read_stereo(&mut panned, 1000);
        assert!(
            panned
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
        let mut muted = [1.0; 10];
        stream.read_stereo(&mut muted, 1000);
        assert_eq!(muted, [0.0; 10]);
    }
    #[test]
    fn stop_seek_and_buffer_reuse_cannot_emit_an_older_voice_generation() {
        let mut m=machine();
        let format=Format {rate:1000,channels:1,bits:8};
        let voice=buffer(&mut m,format,1000);let music=buffer(&mut m,format,1000);
        let voice_storage=m.ext.get::<Sound>().buffers[&voice].storage;
        let music_storage=m.ext.get::<Sound>().buffers[&music].storage;
        m.cpu.write(voice_storage,&[192;1000]).unwrap();m.cpu.write(music_storage,&[160;1000]).unwrap();
        invoke(&mut m,"IDirectSoundBuffer8::Play",&[voice,0,0,0]);
        invoke(&mut m,"IDirectSoundBuffer8::Play",&[music,0,0,1]);pump(&mut m).unwrap();
        let stream:pal_audio::AudioStream=m.ext.get::<Sound>().mixer.clone().into();
        let mut out=[0.0;20];stream.read_stereo(&mut out,1000);assert_eq!(out,[0.75;20]);
        advance(&mut m,10_000);invoke(&mut m,"IDirectSoundBuffer8::Stop",&[voice]);
        invoke(&mut m,"IDirectSoundBuffer8::Lock",&[voice,0,1000,DATA+128,DATA+132,0,0,2]);
        let pointer=m.cpu.read_u32(DATA+128).unwrap();m.cpu.write(pointer,&[64;1000]).unwrap();
        assert_eq!(invoke(&mut m,"IDirectSoundBuffer8::Unlock",&[voice,pointer,1000,0,0]),0);
        invoke(&mut m,"IDirectSoundBuffer8::SetCurrentPosition",&[voice,0]);
        invoke(&mut m,"IDirectSoundBuffer8::Play",&[voice,0,0,0]);pump(&mut m).unwrap();
        stream.read_stereo(&mut out,1000);assert_eq!(out,[-0.25;20]);
        invoke(&mut m,"IDirectSoundBuffer8::Stop",&[voice]);
        stream.read_stereo(&mut out,1000);assert_eq!(out,[0.25;20]);
    }
    #[test]
    fn restarting_a_naturally_ended_buffer_discards_its_unconsumed_old_tail() {
        let mut m=machine();let format=Format {rate:1000,channels:1,bits:8};
        let voice=buffer(&mut m,format,30);let storage=m.ext.get::<Sound>().buffers[&voice].storage;
        m.cpu.write(storage,&[192;30]).unwrap();invoke(&mut m,"IDirectSoundBuffer8::Play",&[voice,0,0,0]);
        pump(&mut m).unwrap();advance(&mut m,30_000);
        // No callback has consumed the first generation and no pump has normalized
        // natural completion yet. The new Play must not inherit its queued PCM.
        m.cpu.write(storage,&[64;30]).unwrap();invoke(&mut m,"IDirectSoundBuffer8::Play",&[voice,0,0,0]);
        pump(&mut m).unwrap();let stream:pal_audio::AudioStream=m.ext.get::<Sound>().mixer.clone().into();
        let mut out=[0.0;80];stream.read_stereo(&mut out,1000);
        assert_eq!(out[..60],[0.0;60]);assert_eq!(out[60..],[-0.5;20]);
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
        assert!(!m.ext.get::<Sound>().mixer.lock().unwrap().active());
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
        assert!(matches!(m.run(), pal_machine::RunResult::Exited(0)));
        assert_eq!(m.cpu.read_u32(DATA + 512).unwrap(), 0);
        assert!(m.ext.get::<Sound>().buffers[&b].playing);
    }
}
