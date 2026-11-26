//! DirectMusic software synthesizer (`dmsynth.dll`) over `adv-device::midi::Synth`
//! and the session's shared PCM mixer.
//!
//! Wine's `dmusic.dll` and `dmloader.dll` retain port enumeration, loader caching,
//! collection parsing and master clock ownership. Replacing `dmsynth.dll` moves
//! MIDI synthesis off guest x86 threads onto native OxiSynth on host time, while
//! [`default_gm_dls`] supplies a minimal General MIDI metadata collection so
//! `dmloader` resolves `GUID_DefaultGMCollection` without shipping an uncompressed
//! SoundFont or DLS sample bank.
use std::collections::{BTreeMap, BinaryHeap};

use adv_abi::{Conv, Registry};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_device::{
    AudioControl,
    midi::{MidiEvent, SAMPLE_RATE, Synth as MidiSynth},
    mixer::PcmBlock,
};
use adv_machine::{Env, Machine, Outcome, layout::PAGE};

use crate::interop::GuestAllocator;

pub const MODULE: &str = "dmsynth.dll";

const S_OK: u32 = 0;
const S_FALSE: u32 = 1;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
const E_FAIL: u32 = 0x8000_4005;
const E_INVALIDARG: u32 = 0x8007_0057;
const CLASS_E_NOAGGREGATION: u32 = 0x8004_0110;
const CLASS_E_CLASSNOTAVAILABLE: u32 = 0x8004_0111;
const DMUS_E_ALREADYOPEN: u32 = 0x8878_1013;
const DMUS_E_NOT_INIT: u32 = 0x8878_1015;
const DMUS_E_SYNTHNOTCONFIGURED: u32 = 0x8878_1045;
const DMUS_E_SYNTHACTIVE: u32 = 0x8878_1047;
const DMUS_E_SYNTHINACTIVE: u32 = 0x8878_104b;
const DMUS_E_NOSYNTHSINK: u32 = 0x8878_104c;
const DMUS_E_NO_MASTER_CLOCK: u32 = 0x8878_1050;
const DMUS_E_INVALID_DOWNLOADID: u32 = 0x8878_1041;
const DMUS_E_UNKNOWN_PROPERTY: u32 = 0x8878_1051;

const IUNKNOWN: [u8; 16] = adv_abi::com::IUNKNOWN;
const ICLASSFACTORY: [u8; 16] = [1, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
/// `CLSID_DirectMusicSynth` = `{58C2B4D0-46E7-11D1-89AC-00A0C9054129}`.
pub const CLSID_SYNTH: [u8; 16] = [
    0xd0, 0xb4, 0xc2, 0x58, 0xe7, 0x46, 0xd1, 0x11, 0x89, 0xac, 0x00, 0xa0, 0xc9, 0x05, 0x41, 0x29,
];
/// `CLSID_DirectMusicSynthSink` = `{AEC17CE3-A514-11D1-AFA6-00AA0024D8B6}`.
pub const CLSID_SINK: [u8; 16] = [
    0xe3, 0x7c, 0xc1, 0xae, 0x14, 0xa5, 0xd1, 0x11, 0xaf, 0xa6, 0x00, 0xaa, 0x00, 0x24, 0xd8, 0xb6,
];
/// `IID_IDirectMusicSynth` = `{09823661-5C85-11D2-AFA6-00AA0024D8B6}`.
pub const IID_SYNTH: [u8; 16] = [
    0x61, 0x36, 0x82, 0x09, 0x85, 0x5c, 0xd2, 0x11, 0xaf, 0xa6, 0x00, 0xaa, 0x00, 0x24, 0xd8, 0xb6,
];
/// `IID_IDirectMusicSynth8` = `{53CAB625-2711-4C9F-9DE7-1B7F925F6FC8}`.
pub const IID_SYNTH8: [u8; 16] = [
    0x25, 0xb6, 0xca, 0x53, 0x11, 0x27, 0x9f, 0x4c, 0x9d, 0xe7, 0x1b, 0x7f, 0x92, 0x5f, 0x6f, 0xc8,
];
/// `IID_IDirectMusicSynthSink` = `{09823663-5C85-11D2-AFA6-00AA0024D8B6}`.
pub const IID_SINK: [u8; 16] = [
    0x63, 0x36, 0x82, 0x09, 0x85, 0x5c, 0xd2, 0x11, 0xaf, 0xa6, 0x00, 0xaa, 0x00, 0x24, 0xd8, 0xb6,
];
/// `IID_IKsControl` = `{28F54685-06FD-11D2-B27A-00A0C9223196}`.
const IID_KSCONTROL: [u8; 16] = [
    0x85, 0x46, 0xf5, 0x28, 0xfd, 0x06, 0xd2, 0x11, 0xb2, 0x7a, 0x00, 0xa0, 0xc9, 0x22, 0x31, 0x96,
];
/// `IID_IReferenceClock` = `{56A86897-0AD4-11CE-B03A-0020AF0BA770}`.
const IID_CLOCK: [u8; 16] = [
    0x97, 0x68, 0xa8, 0x56, 0xd4, 0x0a, 0xce, 0x11, 0xb0, 0x3a, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70,
];
/// `GUID_DefaultGMCollection` = `{F17E8673-C3B4-11D1-870B-00600893B1BD}`.
pub const GUID_DEFAULT_GM_COLLECTION: [u8; 16] = [
    0x73, 0x86, 0x7e, 0xf1, 0xb4, 0xc3, 0xd1, 0x11, 0x87, 0x0b, 0x00, 0x60, 0x08, 0x93, 0xb1, 0xbd,
];

const PROP_GM_HARDWARE: [u8; 16] = [
    0x24, 0x2f, 0x8f, 0x17, 0x64, 0xc3, 0xd1, 0x11, 0xa7, 0x60, 0x00, 0x00, 0xf8, 0x75, 0xac, 0x12,
];
const PROP_GS_HARDWARE: [u8; 16] = [
    0x25, 0x2f, 0x8f, 0x17, 0x64, 0xc3, 0xd1, 0x11, 0xa7, 0x60, 0x00, 0x00, 0xf8, 0x75, 0xac, 0x12,
];
const PROP_XG_HARDWARE: [u8; 16] = [
    0x26, 0x2f, 0x8f, 0x17, 0x64, 0xc3, 0xd1, 0x11, 0xa7, 0x60, 0x00, 0x00, 0xf8, 0x75, 0xac, 0x12,
];
const PROP_DLS1: [u8; 16] = [
    0x27, 0x2f, 0x8f, 0x17, 0x64, 0xc3, 0xd1, 0x11, 0xa7, 0x60, 0x00, 0x00, 0xf8, 0x75, 0xac, 0x12,
];
const PROP_DLS2: [u8; 16] = [
    0xe5, 0x99, 0x45, 0xf1, 0x89, 0x46, 0xd2, 0x11, 0xaf, 0xa6, 0x00, 0xaa, 0x00, 0x24, 0xd8, 0xb6,
];
const PROP_INSTRUMENT2: [u8; 16] = [
    0x72, 0xd3, 0x5f, 0x86, 0x67, 0x9f, 0xd2, 0x11, 0x87, 0x2a, 0x00, 0x60, 0x08, 0x93, 0xb1, 0xbd,
];
const PROP_VOLUME: [u8; 16] = [
    0x25, 0xae, 0xdf, 0xfe, 0x6e, 0xe4, 0xd1, 0x11, 0xaa, 0xce, 0x00, 0x00, 0xf8, 0x75, 0xac, 0x12,
];
const PROP_EFFECTS: [u8; 16] = [
    0x11, 0xd6, 0xa8, 0xcd, 0x4a, 0x68, 0xd2, 0x11, 0x87, 0x1e, 0x00, 0x60, 0x08, 0x93, 0xb1, 0xbd,
];
const PROP_MEMORY_SIZE: [u8; 16] = [
    0x28, 0x2f, 0x8f, 0x17, 0x64, 0xc3, 0xd1, 0x11, 0xa7, 0x60, 0x00, 0x00, 0xf8, 0x75, 0xac, 0x12,
];
const PROP_WRITE_LATENCY: [u8; 16] = [
    0xa0, 0x0f, 0x8a, 0x26, 0xf2, 0x60, 0xd2, 0x11, 0xaf, 0xa6, 0x00, 0xaa, 0x00, 0x24, 0xd8, 0xb6,
];
const PROP_WRITE_PERIOD: [u8; 16] = [
    0xa1, 0x0f, 0x8a, 0x26, 0xf2, 0x60, 0xd2, 0x11, 0xaf, 0xa6, 0x00, 0xaa, 0x00, 0x24, 0xd8, 0xb6,
];
const PROP_SINK_USES_DSOUND: [u8; 16] = [
    0x57, 0x88, 0x20, 0xbe, 0x52, 0x89, 0xd2, 0x11, 0xba, 0x1c, 0x00, 0x00, 0xf8, 0x75, 0xac, 0x12,
];

const KSPROPERTY_TYPE_GET: u32 = 1;
const KSPROPERTY_TYPE_SET: u32 = 2;
const KSPROPERTY_TYPE_BASICSUPPORT: u32 = 0x200;
const KSPROPERTY_SUPPORT_GET: u32 = 1;
const KSPROPERTY_SUPPORT_SET: u32 = 2;

const DMUS_PORTCAPS_SIZE: u32 = 308;
const DMUS_PC_DLS: u32 = 0x0001;
const DMUS_PC_SOFTWARESYNTH: u32 = 0x0040;
const DMUS_PC_DIRECTSOUND: u32 = 0x0080;
const DMUS_PC_GMINHARDWARE: u32 = 0x0100;
const DMUS_PC_DLS2: u32 = 0x0400;
const DMUS_PORT_USER_MODE_SYNTH: u32 = 2;
const DMUS_PC_SYSTEMMEMORY: u32 = 0x7fff_ffff;
const DMUS_EVENT_STRUCTURED: u32 = 1;

const LEAD_US: u64 = 30_000;
const LATENCY_100NS: i64 = 500_000;
const PUMP_INTERVAL_US: u64 = 10_000;
const MAX_CATCHUP_US: u64 = 200_000;
const SOURCE_BASE: u32 = 0x5000_0000;
/// DirectMusic software synthesizers default to +6 dB master volume (`Id = 0`).
const DEFAULT_VOLUME_DB: i32 = 600;
/// Scale `adv-device::midi::Synth`'s internal 0.2 gain up to unity at DirectMusic's
/// default +6 dB master volume without clipping dense multi-channel passages.
const BASE_GAIN: f32 = 2.5;

const FACTORY_METHODS: [(&str, u8); 5] = [
    ("IClassFactory::QueryInterface", 3),
    ("IClassFactory::AddRef", 1),
    ("IClassFactory::Release", 1),
    ("IClassFactory::CreateInstance", 4),
    ("IClassFactory::LockServer", 2),
];
const SYNTH_METHODS: [(&str, u8); 25] = [
    ("IDirectMusicSynth8::QueryInterface", 3),
    ("IDirectMusicSynth8::AddRef", 1),
    ("IDirectMusicSynth8::Release", 1),
    ("IDirectMusicSynth8::Open", 2),
    ("IDirectMusicSynth8::Close", 1),
    ("IDirectMusicSynth8::SetNumChannelGroups", 2),
    ("IDirectMusicSynth8::Download", 4),
    ("IDirectMusicSynth8::Unload", 4),
    ("IDirectMusicSynth8::PlayBuffer", 5),
    ("IDirectMusicSynth8::GetRunningStats", 2),
    ("IDirectMusicSynth8::GetPortCaps", 2),
    ("IDirectMusicSynth8::SetMasterClock", 2),
    ("IDirectMusicSynth8::GetLatencyClock", 2),
    ("IDirectMusicSynth8::Activate", 2),
    ("IDirectMusicSynth8::SetSynthSink", 2),
    ("IDirectMusicSynth8::Render", 5),
    ("IDirectMusicSynth8::SetChannelPriority", 4),
    ("IDirectMusicSynth8::GetChannelPriority", 4),
    ("IDirectMusicSynth8::GetFormat", 3),
    ("IDirectMusicSynth8::GetAppend", 2),
    ("IDirectMusicSynth8::PlayVoice", 15),
    ("IDirectMusicSynth8::StopVoice", 4),
    ("IDirectMusicSynth8::GetVoiceState", 4),
    ("IDirectMusicSynth8::Refresh", 3),
    ("IDirectMusicSynth8::AssignChannelToBuses", 5),
];
const SYNTH_KS_METHODS: [(&str, u8); 6] = [
    ("SynthKsControl::QueryInterface", 3),
    ("SynthKsControl::AddRef", 1),
    ("SynthKsControl::Release", 1),
    ("SynthKsControl::KsProperty", 6),
    ("SynthKsControl::KsMethod", 6),
    ("SynthKsControl::KsEvent", 6),
];
const SINK_METHODS: [(&str, u8); 11] = [
    ("IDirectMusicSynthSink::QueryInterface", 3),
    ("IDirectMusicSynthSink::AddRef", 1),
    ("IDirectMusicSynthSink::Release", 1),
    ("IDirectMusicSynthSink::Init", 2),
    ("IDirectMusicSynthSink::SetMasterClock", 2),
    ("IDirectMusicSynthSink::GetLatencyClock", 2),
    ("IDirectMusicSynthSink::Activate", 2),
    ("IDirectMusicSynthSink::SampleToRefTime", 4),
    ("IDirectMusicSynthSink::RefTimeToSample", 4),
    ("IDirectMusicSynthSink::SetDirectSound", 3),
    ("IDirectMusicSynthSink::GetDesiredBufferSize", 2),
];
const SINK_KS_METHODS: [(&str, u8); 6] = [
    ("SinkKsControl::QueryInterface", 3),
    ("SinkKsControl::AddRef", 1),
    ("SinkKsControl::Release", 1),
    ("SinkKsControl::KsProperty", 6),
    ("SinkKsControl::KsMethod", 6),
    ("SinkKsControl::KsEvent", 6),
];
const CLOCK_METHODS: [(&str, u8); 7] = [
    ("LatencyClock::QueryInterface", 3),
    ("LatencyClock::AddRef", 1),
    ("LatencyClock::Release", 1),
    ("LatencyClock::GetTime", 2),
    ("LatencyClock::AdviseTime", 7),
    ("LatencyClock::AdvisePeriodic", 7),
    ("LatencyClock::Unadvise", 2),
];

/// Observable DirectMusic synthesis counters for integration tests.
#[derive(Default, Clone, Copy, Debug)]
pub struct Stats {
    pub downloads: u64,
    pub unloads: u64,
    pub buffers: u64,
    pub short_events: u64,
    pub sysex_events: u64,
    pub rendered_frames: u64,
}

#[derive(Clone, Copy)]
struct QueuedEvent {
    at_us: u64,
    seq: u64,
    group: u32,
    event: MidiEvent,
}
impl PartialEq for QueuedEvent {
    fn eq(&self, other: &Self) -> bool {
        self.at_us == other.at_us && self.seq == other.seq
    }
}
impl Eq for QueuedEvent {}
impl PartialOrd for QueuedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for QueuedEvent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .at_us
            .cmp(&self.at_us)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

struct GroupSynth {
    engine: MidiSynth,
    running: u8,
}

struct Synth {
    id: u32,
    refs: u32,
    open: bool,
    active: bool,
    voices: u32,
    groups: u32,
    effects: u32,
    volume_db: i32,
    volume_boost_db: i32,
    master_clock: u32,
    sink: u32,
    next_download: u32,
    downloads: BTreeMap<u32, u32>,
    priorities: BTreeMap<(u32, u32), u32>,
    synths: BTreeMap<u32, GroupSynth>,
    events: BinaryHeap<QueuedEvent>,
    next_seq: u64,
    origin_us: u64,
    frames: u64,
}

struct Sink {
    refs: u32,
    synth: u32,
    master_clock: u32,
    dsound: u32,
    dsound_buffer: u32,
    active: bool,
    origin_rt: i64,
    write_latency_ms: u32,
    write_period_ms: u32,
}

struct State {
    allocator: GuestAllocator,
    synth_vtbl: u32,
    synth_ks_vtbl: u32,
    sink_vtbl: u32,
    sink_ks_vtbl: u32,
    clock_vtbl: u32,
    synth_factory: u32,
    sink_factory: u32,
    scratch: u32,
    next_id: u32,
    synths: BTreeMap<u32, Synth>,
    sinks: BTreeMap<u32, Sink>,
    stats: Stats,
}

/// Register all `dmsynth.dll` exports and COM vtable thunks.
pub fn register(registry: &mut Registry) {
    registry.add(
        MODULE,
        "DllCanUnloadNow",
        Conv::Stdcall { argc: 0 },
        |env| {
            let state = env.ext.get::<State>();
            Outcome::Return(u32::from(
                !state.synths.is_empty() || !state.sinks.is_empty(),
            ))
        },
    );
    registry.add(
        MODULE,
        "DllGetClassObject",
        Conv::Stdcall { argc: 3 },
        dll_get_class_object,
    );
    for name in ["DllRegisterServer", "DllUnregisterServer"] {
        registry.add(MODULE, name, Conv::Stdcall { argc: 0 }, |_| {
            Outcome::Return(S_OK)
        });
    }
    register_table(registry, &FACTORY_METHODS, factory_method);
    register_table(registry, &SYNTH_METHODS, synth_method);
    register_table(registry, &SYNTH_KS_METHODS, synth_ks_method);
    register_table(registry, &SINK_METHODS, sink_method);
    register_table(registry, &SINK_KS_METHODS, sink_ks_method);
    register_table(registry, &CLOCK_METHODS, clock_method);
}

fn register_table(
    registry: &mut Registry,
    methods: &[(&'static str, u8)],
    handler: fn(&mut Env<'_>) -> Outcome,
) {
    for (index, &(name, argc)) in methods.iter().enumerate() {
        registry.add_with_data(MODULE, name, Conv::Stdcall { argc }, handler, index as u64);
    }
}

/// Allocate guest vtables and install `dmsynth` state on `machine`.
pub fn install<B: Backend>(
    machine: &mut Machine<B>,
    allocator: GuestAllocator,
) -> Result<(), String> {
    let page = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, PAGE, Prot::RW)
        .map_err(|e| e.to_string())?;
    let mut cursor = page;
    let factory_vtbl = write_vtable(machine, &mut cursor, &FACTORY_METHODS)?;
    let synth_vtbl = write_vtable(machine, &mut cursor, &SYNTH_METHODS)?;
    let synth_ks_vtbl = write_vtable(machine, &mut cursor, &SYNTH_KS_METHODS)?;
    let sink_vtbl = write_vtable(machine, &mut cursor, &SINK_METHODS)?;
    let sink_ks_vtbl = write_vtable(machine, &mut cursor, &SINK_KS_METHODS)?;
    let clock_vtbl = write_vtable(machine, &mut cursor, &CLOCK_METHODS)?;

    let synth_factory = cursor;
    machine
        .cpu
        .write_u32(synth_factory, factory_vtbl)
        .map_err(|e| e.to_string())?;
    cursor += 4;
    let sink_factory = cursor;
    machine
        .cpu
        .write_u32(sink_factory, factory_vtbl)
        .map_err(|e| e.to_string())?;
    cursor += 4;
    let scratch = (cursor + 7) & !7;
    debug_assert!(scratch + 16 <= page + PAGE);

    machine.ext.insert(State {
        allocator,
        synth_vtbl,
        synth_ks_vtbl,
        sink_vtbl,
        sink_ks_vtbl,
        clock_vtbl,
        synth_factory,
        sink_factory,
        scratch,
        next_id: 0,
        synths: BTreeMap::new(),
        sinks: BTreeMap::new(),
        stats: Stats::default(),
    });
    Ok(())
}

fn write_vtable<B: Backend>(
    machine: &mut Machine<B>,
    cursor: &mut u32,
    methods: &[(&str, u8)],
) -> Result<u32, String> {
    let base = *cursor;
    for &(name, _) in methods {
        let entry = machine
            .sys
            .thunks
            .publish(&mut machine.cpu, MODULE, name)
            .map_err(|e| e.to_string())?;
        machine
            .cpu
            .write_u32(*cursor, entry)
            .map_err(|e| e.to_string())?;
        *cursor += 4;
    }
    Ok(base)
}

/// Snapshot of synthesis counters for tests.
pub fn stats<B: Backend>(machine: &Machine<B>) -> Stats {
    machine
        .ext
        .try_get::<State>()
        .map_or(Stats::default(), |state| state.stats)
}

/// Advance all active DirectMusic synthesizers on host time and push PCM blocks
/// to the session's shared mixer.
pub fn pump<B: Backend>(machine: &mut Machine<B>) -> Result<Option<u64>, String> {
    let Some(audio) = machine.ext.try_get::<AudioControl>().cloned() else {
        return Ok(None);
    };
    let pid = machine.sys.pid;
    let now = machine.sys.clock.now_us();
    let Some(state) = machine.ext.try_get_mut::<State>() else {
        return Ok(None);
    };
    let active_ids: Vec<u32> = state
        .synths
        .iter()
        .filter(|(_, s)| s.active && is_native_active_sink(&state.sinks, s.sink))
        .map(|(&addr, _)| addr)
        .collect();
    if active_ids.is_empty() {
        return Ok(None);
    }
    audio.sync_clock(now);
    let mut any_active = false;
    for addr in active_ids {
        let mut rendered = 0u64;
        if let Some(synth) = state.synths.get_mut(&addr) {
            rendered = render_to_mixer(synth, &audio, pid, now)?;
            any_active = true;
        }
        state.stats.rendered_frames += rendered;
    }
    Ok(any_active.then_some(now + PUMP_INTERVAL_US))
}

fn is_native_active_sink(sinks: &BTreeMap<u32, Sink>, sink: u32) -> bool {
    sinks.get(&sink).is_some_and(|s| s.active)
}

fn source_id(pid: u32, id: u32) -> u64 {
    (u64::from(pid) << 32) | u64::from(SOURCE_BASE | id)
}

fn db_to_gain(db_hundredths: i32) -> f32 {
    if db_hundredths <= -10_000 {
        0.0
    } else {
        10.0f32.powf(db_hundredths.clamp(-10_000, 2_000) as f32 / 2000.0)
    }
}

fn synth_gain(synth: &Synth) -> f32 {
    BASE_GAIN * db_to_gain(synth.volume_db.saturating_add(synth.volume_boost_db))
}

fn apply_event(synth: &mut Synth, group: u32, event: MidiEvent) -> Result<(), String> {
    if group == 0 {
        if synth.synths.is_empty() {
            ensure_group(synth, 1)?;
        }
        for gs in synth.synths.values_mut() {
            apply_group_event(gs, event)?;
        }
        return Ok(());
    }
    let gs = ensure_group(synth, group)?;
    apply_group_event(gs, event)
}

fn ensure_group(synth: &mut Synth, group: u32) -> Result<&mut GroupSynth, String> {
    if let std::collections::btree_map::Entry::Vacant(entry) = synth.synths.entry(group) {
        let mut engine = MidiSynth::new()?;
        engine.event(MidiEvent::Reset)?;
        entry.insert(GroupSynth { engine, running: 0 });
    }
    Ok(synth.synths.get_mut(&group).unwrap())
}

fn apply_group_event(gs: &mut GroupSynth, event: MidiEvent) -> Result<(), String> {
    match event {
        MidiEvent::Short(raw) => {
            let first = raw as u8;
            let packed = if first & 0x80 != 0 {
                if first < 0xf0 {
                    gs.running = first;
                } else if first <= 0xf7 {
                    gs.running = 0;
                }
                raw
            } else if gs.running != 0 {
                (raw << 8) | u32::from(gs.running)
            } else {
                return Ok(());
            };
            gs.engine.event(MidiEvent::Short(packed))
        }
        other => {
            gs.running = 0;
            gs.engine.event(other)
        }
    }
}

fn render_mixed_frames(synth: &mut Synth, frames: usize) -> Vec<f32> {
    if synth.synths.is_empty() {
        return vec![0.0; frames * 2];
    }
    let mut mixed = vec![0.0f32; frames * 2];
    for gs in synth.synths.values_mut() {
        let part = gs.engine.render(frames);
        for (dst, src) in mixed.iter_mut().zip(part) {
            *dst += src;
        }
    }
    mixed
}

fn render_to_mixer(
    synth: &mut Synth,
    audio: &AudioControl,
    pid: u32,
    now_us: u64,
) -> Result<u64, String> {
    let until = now_us + LEAD_US;
    let frontier = synth.origin_us + synth.frames * 1_000_000 / u64::from(SAMPLE_RATE);
    if frontier + MAX_CATCHUP_US < now_us {
        while let Some(&ev) = synth.events.peek() {
            if ev.at_us > now_us {
                break;
            }
            let ev = synth.events.pop().unwrap();
            apply_event(synth, ev.group, ev.event)?;
        }
        synth.origin_us = now_us + LEAD_US;
        synth.frames = 0;
    }
    let end = until.saturating_sub(synth.origin_us) * u64::from(SAMPLE_RATE) / 1_000_000;
    let stream = source_id(pid, synth.id);
    let mut total_rendered = 0u64;
    while synth.frames < end {
        let mut chunk_end = (synth.frames + 441).min(end);
        while let Some(&ev) = synth.events.peek() {
            let ev_frame =
                ev.at_us.saturating_sub(synth.origin_us) * u64::from(SAMPLE_RATE) / 1_000_000;
            if ev_frame <= synth.frames {
                let ev = synth.events.pop().unwrap();
                apply_event(synth, ev.group, ev.event)?;
            } else {
                chunk_end = chunk_end.min(ev_frame);
                break;
            }
        }
        let frames = (chunk_end - synth.frames) as usize;
        if frames == 0 {
            continue;
        }
        let samples = render_mixed_frames(synth, frames);
        let pts = synth.origin_us + synth.frames * 1_000_000 / u64::from(SAMPLE_RATE);
        if audio
            .push_pcm(PcmBlock {
                stream_id: stream,
                guest_pts_us: pts,
                sample_rate: SAMPLE_RATE,
                channels: 2,
                samples,
            })
            .is_err()
        {
            break;
        }
        synth.frames += frames as u64;
        total_rendered += frames as u64;
    }
    // Drain any events scheduled right at `end`.
    while let Some(&ev) = synth.events.peek() {
        let ev_frame =
            ev.at_us.saturating_sub(synth.origin_us) * u64::from(SAMPLE_RATE) / 1_000_000;
        if ev_frame <= synth.frames {
            let ev = synth.events.pop().unwrap();
            apply_event(synth, ev.group, ev.event)?;
        } else {
            break;
        }
    }
    let gain = synth_gain(synth);
    audio.set_stereo_gain(stream, [gain, gain]);
    Ok(total_rendered)
}

fn read_guid(env: &Env<'_>, at: u32) -> Result<[u8; 16], String> {
    let mut bytes = [0u8; 16];
    env.cpu.read(at, &mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn read_u32(env: &Env<'_>, at: u32) -> Result<u32, String> {
    env.cpu.read_u32(at).map_err(|e| e.to_string())
}

fn write_u32(env: &mut Env<'_>, at: u32, value: u32) -> Result<(), String> {
    env.cpu.write_u32(at, value).map_err(|e| e.to_string())
}

fn read_i64(env: &Env<'_>, at: u32) -> Result<i64, String> {
    let mut bytes = [0u8; 8];
    env.cpu.read(at, &mut bytes).map_err(|e| e.to_string())?;
    Ok(i64::from_le_bytes(bytes))
}

fn write_i64(env: &mut Env<'_>, at: u32, value: i64) -> Result<(), String> {
    env.cpu
        .write(at, &value.to_le_bytes())
        .map_err(|e| e.to_string())
}

fn finish(result: Result<u32, String>) -> Outcome {
    match result {
        Ok(hr) => Outcome::Return(hr),
        Err(error) => Outcome::Fail(error),
    }
}

fn com_call(
    env: &mut Env<'_>,
    object: u32,
    slot: u32,
    extra_args: Vec<u32>,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let target = (|| {
        let vtable = read_u32(env, object)?;
        read_u32(env, vtable + slot * 4)
    })();
    match target {
        Ok(entry) => {
            let mut args = Vec::with_capacity(1 + extra_args.len());
            args.push(object);
            args.extend(extra_args);
            Outcome::call(entry, args, move |env, result| then(env, result.value()))
        }
        Err(error) => Outcome::Fail(error),
    }
}

fn release_com_list(env: &mut Env<'_>, mut objects: Vec<u32>, ret: u32) -> Outcome {
    while let Some(obj) = objects.pop() {
        if obj != 0 {
            return com_call(env, obj, 2, Vec::new(), move |env, _| {
                release_com_list(env, objects, ret)
            });
        }
    }
    Outcome::Return(ret)
}

fn dll_get_class_object(env: &mut Env<'_>) -> Outcome {
    let (clsid_ptr, iid_ptr, out_ptr) = (env.arg(0), env.arg(1), env.arg(2));
    if out_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    finish((|| {
        write_u32(env, out_ptr, 0)?;
        if clsid_ptr == 0 || iid_ptr == 0 {
            return Ok(E_INVALIDARG);
        }
        let clsid = read_guid(env, clsid_ptr)?;
        let iid = read_guid(env, iid_ptr)?;
        let state = env.ext.get::<State>();
        let factory = if clsid == CLSID_SYNTH {
            state.synth_factory
        } else if clsid == CLSID_SINK {
            state.sink_factory
        } else {
            return Ok(CLASS_E_CLASSNOTAVAILABLE);
        };
        if iid != IUNKNOWN && iid != ICLASSFACTORY {
            return Ok(E_NOINTERFACE);
        }
        write_u32(env, out_ptr, factory)?;
        Ok(S_OK)
    })())
}

fn factory_method(env: &mut Env<'_>) -> Outcome {
    let slot = env.sys.thunks.get(env.thunk).f.data;
    match slot {
        0 => {
            let (this, iid_ptr, out_ptr) = (env.arg(0), env.arg(1), env.arg(2));
            if out_ptr == 0 {
                return Outcome::Return(E_POINTER);
            }
            finish((|| {
                write_u32(env, out_ptr, 0)?;
                let iid = read_guid(env, iid_ptr)?;
                if iid == IUNKNOWN || iid == ICLASSFACTORY {
                    write_u32(env, out_ptr, this)?;
                    Ok(S_OK)
                } else {
                    Ok(E_NOINTERFACE)
                }
            })())
        }
        1 | 2 => Outcome::Return(1),
        3 => {
            let (this, outer, iid_ptr, out_ptr) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
            if out_ptr == 0 {
                return Outcome::Return(E_POINTER);
            }
            finish((|| {
                write_u32(env, out_ptr, 0)?;
                if outer != 0 {
                    return Ok(CLASS_E_NOAGGREGATION);
                }
                if iid_ptr == 0 {
                    return Ok(E_INVALIDARG);
                }
                let iid = read_guid(env, iid_ptr)?;
                let is_synth = this == env.ext.get::<State>().synth_factory;
                if is_synth {
                    create_synth(env, iid, out_ptr)
                } else {
                    create_sink(env, iid, out_ptr)
                }
            })())
        }
        4 => Outcome::Return(S_OK),
        _ => Outcome::Return(E_NOTIMPL),
    }
}

fn create_synth(env: &mut Env<'_>, iid: [u8; 16], out_ptr: u32) -> Result<u32, String> {
    let iface_offset = if iid == IUNKNOWN || iid == IID_SYNTH || iid == IID_SYNTH8 {
        0u32
    } else if iid == IID_KSCONTROL {
        4u32
    } else {
        return Ok(E_NOINTERFACE);
    };
    let (alloc, synth_vtbl, ks_vtbl) = {
        let s = env.ext.get::<State>();
        (s.allocator, s.synth_vtbl, s.synth_ks_vtbl)
    };
    let base = (alloc.allocate)(env, 8).ok_or("dmsynth: guest allocation failed")?;
    write_u32(env, base, synth_vtbl)?;
    write_u32(env, base + 4, ks_vtbl)?;
    let state = env.ext.get_mut::<State>();
    state.next_id += 1;
    let id = state.next_id;
    state.synths.insert(
        base,
        Synth {
            id,
            refs: 1,
            open: false,
            active: false,
            voices: 128,
            groups: 1,
            effects: 0,
            volume_db: DEFAULT_VOLUME_DB,
            volume_boost_db: 0,
            master_clock: 0,
            sink: 0,
            next_download: 1,
            downloads: BTreeMap::new(),
            priorities: BTreeMap::new(),
            synths: BTreeMap::new(),
            events: BinaryHeap::new(),
            next_seq: 0,
            origin_us: 0,
            frames: 0,
        },
    );
    write_u32(env, out_ptr, base + iface_offset)?;
    Ok(S_OK)
}

fn create_sink(env: &mut Env<'_>, iid: [u8; 16], out_ptr: u32) -> Result<u32, String> {
    let iface_offset = if iid == IUNKNOWN || iid == IID_SINK {
        0u32
    } else if iid == IID_KSCONTROL {
        4u32
    } else {
        return Ok(E_NOINTERFACE);
    };
    let (alloc, sink_vtbl, ks_vtbl, clock_vtbl) = {
        let s = env.ext.get::<State>();
        (s.allocator, s.sink_vtbl, s.sink_ks_vtbl, s.clock_vtbl)
    };
    let base = (alloc.allocate)(env, 12).ok_or("dmsynth: guest allocation failed")?;
    write_u32(env, base, sink_vtbl)?;
    write_u32(env, base + 4, ks_vtbl)?;
    write_u32(env, base + 8, clock_vtbl)?;
    env.ext.get_mut::<State>().sinks.insert(
        base,
        Sink {
            refs: 1,
            synth: 0,
            master_clock: 0,
            dsound: 0,
            dsound_buffer: 0,
            active: false,
            origin_rt: 0,
            write_latency_ms: 50,
            write_period_ms: 10,
        },
    );
    write_u32(env, out_ptr, base + iface_offset)?;
    Ok(S_OK)
}

fn synth_base(env: &Env<'_>, this: u32) -> Option<u32> {
    let synths = &env.ext.get::<State>().synths;
    if synths.contains_key(&this) {
        Some(this)
    } else if this >= 4 && synths.contains_key(&(this - 4)) {
        Some(this - 4)
    } else {
        None
    }
}

fn sink_base(env: &Env<'_>, this: u32) -> Option<u32> {
    let sinks = &env.ext.get::<State>().sinks;
    if sinks.contains_key(&this) {
        Some(this)
    } else if this >= 4 && sinks.contains_key(&(this - 4)) {
        Some(this - 4)
    } else if this >= 8 && sinks.contains_key(&(this - 8)) {
        Some(this - 8)
    } else {
        None
    }
}

fn synth_qi(env: &mut Env<'_>, base: u32, iid_ptr: u32, out_ptr: u32) -> Outcome {
    if out_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    finish((|| {
        write_u32(env, out_ptr, 0)?;
        let iid = read_guid(env, iid_ptr)?;
        let ptr = if iid == IUNKNOWN || iid == IID_SYNTH || iid == IID_SYNTH8 {
            base
        } else if iid == IID_KSCONTROL {
            base + 4
        } else {
            return Ok(E_NOINTERFACE);
        };
        env.ext
            .get_mut::<State>()
            .synths
            .get_mut(&base)
            .unwrap()
            .refs += 1;
        write_u32(env, out_ptr, ptr)?;
        Ok(S_OK)
    })())
}

fn synth_addref(env: &mut Env<'_>, base: u32) -> Outcome {
    let refs = {
        let s = env.ext.get_mut::<State>().synths.get_mut(&base).unwrap();
        s.refs += 1;
        s.refs
    };
    Outcome::Return(refs)
}

fn synth_release(env: &mut Env<'_>, base: u32) -> Outcome {
    let (refs, release_list, free_fn, stream) = {
        let state = env.ext.get_mut::<State>();
        let synth = state.synths.get_mut(&base).unwrap();
        synth.refs = synth.refs.saturating_sub(1);
        if synth.refs > 0 {
            return Outcome::Return(synth.refs);
        }
        let synth = state.synths.remove(&base).unwrap();
        let stream = source_id(env.sys.pid, synth.id);
        let mut to_release = Vec::new();
        if synth.master_clock != 0 {
            to_release.push(synth.master_clock);
        }
        if synth.sink != 0 {
            if let Some(native_sink) = state.sinks.get_mut(&synth.sink) {
                native_sink.synth = 0;
            }
            to_release.push(synth.sink);
        }
        (0u32, to_release, state.allocator.free, stream)
    };
    if let Some(audio) = env.ext.try_get::<AudioControl>() {
        audio.remove_pcm(stream);
    }
    free_fn(env, base);
    release_com_list(env, release_list, refs)
}

fn synth_method(env: &mut Env<'_>) -> Outcome {
    let slot = env.sys.thunks.get(env.thunk).f.data;
    let this = env.arg(0);
    let Some(base) = synth_base(env, this) else {
        return Outcome::Fail(format!("{MODULE}: unknown synth {this:#x}"));
    };
    match slot {
        0 => synth_qi(env, base, env.arg(1), env.arg(2)),
        1 => synth_addref(env, base),
        2 => synth_release(env, base),
        3 => synth_open(env, base, env.arg(1)),
        4 => synth_close(env, base),
        5 => synth_set_num_channel_groups(env, base, env.arg(1)),
        6 => synth_download(env, base, env.arg(1), env.arg(2), env.arg(3)),
        7 => synth_unload(env, base, env.arg(1), env.arg(2), env.arg(3)),
        8 => synth_play_buffer(env, base, env.arg(1), env.arg(2), env.arg(3), env.arg(4)),
        9 => synth_get_running_stats(env, base, env.arg(1)),
        10 => synth_get_port_caps(env, env.arg(1)),
        11 => synth_set_master_clock(env, base, env.arg(1)),
        12 => synth_get_latency_clock(env, base, env.arg(1)),
        13 => synth_activate(env, base, env.arg(1)),
        14 => synth_set_synth_sink(env, base, env.arg(1)),
        15 => synth_render(env, base, env.arg(1), env.arg(2), env.arg(3), env.arg(4)),
        16 => synth_set_channel_priority(env, base, env.arg(1), env.arg(2), env.arg(3)),
        17 => synth_get_channel_priority(env, base, env.arg(1), env.arg(2), env.arg(3)),
        18 => synth_get_format(env, base, env.arg(1), env.arg(2)),
        19 => {
            let out = env.arg(1);
            if out == 0 {
                return Outcome::Return(E_POINTER);
            }
            finish(write_u32(env, out, 0).map(|()| S_OK))
        }
        20..=22 => Outcome::Return(E_NOTIMPL),
        23 => Outcome::Return(S_OK), // Refresh
        24 => Outcome::Return(S_OK), // AssignChannelToBuses
        _ => Outcome::Return(E_NOTIMPL),
    }
}

fn synth_open(env: &mut Env<'_>, base: u32, params_ptr: u32) -> Outcome {
    finish((|| {
        if env.ext.get::<State>().synths.get(&base).unwrap().open {
            return Ok(DMUS_E_ALREADYOPEN);
        }
        let mut voices = 128u32;
        let mut groups = 1u32;
        let mut effects = 0u32;
        let mut modified = false;
        if params_ptr != 0 {
            let size = read_u32(env, params_ptr)?;
            if size < 32 {
                return Ok(E_INVALIDARG);
            }
            let valid = read_u32(env, params_ptr + 4)?;
            if valid & 0x01 != 0 {
                let req = read_u32(env, params_ptr + 8)?;
                voices = req.clamp(1, 1000);
                if voices != req {
                    modified = true;
                }
            }
            if valid & 0x02 != 0 {
                let req = read_u32(env, params_ptr + 12)?;
                groups = req.clamp(1, 1000);
                if groups != req {
                    modified = true;
                }
            }
            if valid & 0x04 != 0 && read_u32(env, params_ptr + 16)? != 2 {
                modified = true;
            }
            if valid & 0x08 != 0 && read_u32(env, params_ptr + 20)? != SAMPLE_RATE {
                modified = true;
            }
            if valid & 0x10 != 0 {
                effects = read_u32(env, params_ptr + 24)? & 3;
            }
            write_u32(env, params_ptr + 4, valid | 0x1f)?;
            write_u32(env, params_ptr + 8, voices)?;
            write_u32(env, params_ptr + 12, groups)?;
            write_u32(env, params_ptr + 16, 2)?;
            write_u32(env, params_ptr + 20, SAMPLE_RATE)?;
            write_u32(env, params_ptr + 24, effects)?;
            write_u32(env, params_ptr + 28, 0)?;
        }
        let synth = env.ext.get_mut::<State>().synths.get_mut(&base).unwrap();
        synth.open = true;
        synth.voices = voices;
        synth.groups = groups;
        synth.effects = effects;
        Ok(if modified { S_FALSE } else { S_OK })
    })())
}

fn synth_close(env: &mut Env<'_>, base: u32) -> Outcome {
    let stream = {
        let synth = env.ext.get_mut::<State>().synths.get_mut(&base).unwrap();
        if !synth.open {
            return Outcome::Return(DMUS_E_ALREADYOPEN);
        }
        synth.open = false;
        synth.active = false;
        synth.events.clear();
        synth.synths.clear();
        source_id(env.sys.pid, synth.id)
    };
    if let Some(audio) = env.ext.try_get::<AudioControl>() {
        audio.remove_pcm(stream);
    }
    Outcome::Return(S_OK)
}

fn synth_set_num_channel_groups(env: &mut Env<'_>, base: u32, groups: u32) -> Outcome {
    if groups == 0 || groups > 1000 {
        return Outcome::Return(E_INVALIDARG);
    }
    let synth = env.ext.get_mut::<State>().synths.get_mut(&base).unwrap();
    synth.groups = groups;
    Outcome::Return(S_OK)
}

fn synth_download(
    env: &mut Env<'_>,
    base: u32,
    ph_download: u32,
    pv_data: u32,
    pb_free: u32,
) -> Outcome {
    if ph_download == 0 || pv_data == 0 || pb_free == 0 {
        return Outcome::Return(E_POINTER);
    }
    finish((|| {
        let dl_type = read_u32(env, pv_data)?;
        let dl_id = read_u32(env, pv_data + 4)?;
        let state = env.ext.get_mut::<State>();
        let synth = state.synths.get_mut(&base).unwrap();
        if !synth.open {
            return Ok(DMUS_E_SYNTHNOTCONFIGURED);
        }
        let handle = synth.next_download;
        synth.next_download = synth.next_download.wrapping_add(1).max(1);
        synth.downloads.insert(handle, (dl_type << 16) ^ dl_id);
        state.stats.downloads += 1;
        write_u32(env, ph_download, handle)?;
        // Returning TRUE tells dmusic it may free the download buffer immediately.
        write_u32(env, pb_free, 1)?;
        Ok(S_OK)
    })())
}

fn synth_unload(
    env: &mut Env<'_>,
    base: u32,
    handle: u32,
    _lp_free: u32,
    _user_data: u32,
) -> Outcome {
    let state = env.ext.get_mut::<State>();
    let synth = state.synths.get_mut(&base).unwrap();
    if synth.downloads.remove(&handle).is_some() {
        state.stats.unloads += 1;
        Outcome::Return(S_OK)
    } else {
        Outcome::Return(DMUS_E_INVALID_DOWNLOADID)
    }
}

fn synth_play_buffer(
    env: &mut Env<'_>,
    base: u32,
    rt_lo: u32,
    rt_hi: u32,
    buf_ptr: u32,
    buf_len: u32,
) -> Outcome {
    if buf_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    let rt = i64::from_le_bytes([
        rt_lo as u8,
        (rt_lo >> 8) as u8,
        (rt_lo >> 16) as u8,
        (rt_lo >> 24) as u8,
        rt_hi as u8,
        (rt_hi >> 8) as u8,
        (rt_hi >> 16) as u8,
        (rt_hi >> 24) as u8,
    ]);
    let (open, active, master_clock, scratch) = {
        let state = env.ext.get::<State>();
        let synth = state.synths.get(&base).unwrap();
        (synth.open, synth.active, synth.master_clock, state.scratch)
    };
    if !open {
        return Outcome::Return(DMUS_E_SYNTHNOTCONFIGURED);
    }
    if !active {
        return Outcome::Return(DMUS_E_SYNTHINACTIVE);
    }
    let mut bytes = vec![0u8; buf_len as usize];
    if let Err(error) = env.cpu.read(buf_ptr, &mut bytes) {
        return Outcome::Fail(error.to_string());
    }
    if master_clock != 0 {
        return com_call(env, master_clock, 3, vec![scratch], move |env, hr| {
            let now_rt = if hr == S_OK {
                read_i64(env, scratch).unwrap_or(0)
            } else {
                0
            };
            finish(enqueue_buffer(env, base, rt, now_rt, &bytes))
        });
    }
    let now_rt = (env.sys.clock.now_us() as i64) * 10;
    finish(enqueue_buffer(env, base, rt, now_rt, &bytes))
}

fn enqueue_buffer(
    env: &mut Env<'_>,
    base: u32,
    rt: i64,
    now_rt: i64,
    bytes: &[u8],
) -> Result<u32, String> {
    let now_us = env.sys.clock.now_us();
    let audio = env.ext.try_get::<AudioControl>().cloned();
    let pid = env.sys.pid;
    let state = env.ext.get_mut::<State>();
    let Some(synth) = state.synths.get_mut(&base) else {
        return Ok(S_OK);
    };
    state.stats.buffers += 1;
    let mut offset = 0usize;
    while offset + 20 <= bytes.len() {
        let cb_event = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let group = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let rt_delta = i64::from_le_bytes(bytes[offset + 8..offset + 16].try_into().unwrap());
        let flags = u32::from_le_bytes(bytes[offset + 16..offset + 20].try_into().unwrap());
        let Some(data_end) = offset.checked_add(20).and_then(|v| v.checked_add(cb_event)) else {
            break;
        };
        if data_end > bytes.len() {
            break;
        }
        let payload = &bytes[offset + 20..data_end];
        let event_rt = if rt == 0 {
            now_rt.saturating_add(rt_delta)
        } else {
            rt.saturating_add(rt_delta)
        };
        let delta_100ns = event_rt.saturating_sub(now_rt).max(0) as u64;
        let target_us = now_us.saturating_add(delta_100ns / 10);
        if flags & DMUS_EVENT_STRUCTURED != 0 {
            let mut packed = 0u32;
            for (i, &b) in payload.iter().take(4).enumerate() {
                packed |= u32::from(b) << (i * 8);
            }
            if cb_event > 0 {
                let seq = synth.next_seq;
                synth.next_seq += 1;
                synth.events.push(QueuedEvent {
                    at_us: target_us,
                    seq,
                    group,
                    event: MidiEvent::Short(packed),
                });
                state.stats.short_events += 1;
            }
        } else if let Some(sysex) = MidiEvent::sysex(payload) {
            let seq = synth.next_seq;
            synth.next_seq += 1;
            synth.events.push(QueuedEvent {
                at_us: target_us,
                seq,
                group,
                event: sysex,
            });
            state.stats.sysex_events += 1;
        }
        let stride = (20usize.saturating_add(cb_event).saturating_add(7)) & !7;
        if stride == 0 {
            break;
        }
        offset = offset.saturating_add(stride);
    }
    let sink = synth.sink;
    let active_native = synth.active && is_native_active_sink(&state.sinks, sink);
    if active_native && let Some(audio) = audio {
        audio.sync_clock(now_us);
        let rendered = render_to_mixer(state.synths.get_mut(&base).unwrap(), &audio, pid, now_us)?;
        state.stats.rendered_frames += rendered;
    }
    Ok(S_OK)
}

fn synth_get_running_stats(env: &mut Env<'_>, base: u32, stats_ptr: u32) -> Outcome {
    if stats_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    finish((|| {
        let size = read_u32(env, stats_ptr)?;
        if size < 28 {
            return Ok(E_INVALIDARG);
        }
        let active = env.ext.get::<State>().synths.get(&base).unwrap().active;
        let mut out = [0u8; 28];
        out[0..4].copy_from_slice(&28u32.to_le_bytes());
        out[4..8].copy_from_slice(&(if active { 1u32 } else { 0 }).to_le_bytes());
        env.cpu.write(stats_ptr, &out).map_err(|e| e.to_string())?;
        Ok(S_OK)
    })())
}

fn synth_get_port_caps(env: &mut Env<'_>, caps_ptr: u32) -> Outcome {
    if caps_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    finish((|| {
        let size = read_u32(env, caps_ptr)?;
        if size < DMUS_PORTCAPS_SIZE {
            return Ok(E_INVALIDARG);
        }
        let mut caps = [0u8; DMUS_PORTCAPS_SIZE as usize];
        caps[0x00..0x04].copy_from_slice(&DMUS_PORTCAPS_SIZE.to_le_bytes());
        let flags = DMUS_PC_DLS
            | DMUS_PC_SOFTWARESYNTH
            | DMUS_PC_DIRECTSOUND
            | DMUS_PC_GMINHARDWARE
            | DMUS_PC_DLS2;
        caps[0x04..0x08].copy_from_slice(&flags.to_le_bytes());
        caps[0x08..0x18].copy_from_slice(&CLSID_SYNTH);
        caps[0x18..0x1c].copy_from_slice(&0u32.to_le_bytes()); // DMUS_PC_OUTPUTCLASS
        caps[0x1c..0x20].copy_from_slice(&DMUS_PORT_USER_MODE_SYNTH.to_le_bytes());
        caps[0x20..0x24].copy_from_slice(&DMUS_PC_SYSTEMMEMORY.to_le_bytes());
        caps[0x24..0x28].copy_from_slice(&1000u32.to_le_bytes());
        caps[0x28..0x2c].copy_from_slice(&1000u32.to_le_bytes());
        caps[0x2c..0x30].copy_from_slice(&2u32.to_le_bytes());
        caps[0x30..0x34].copy_from_slice(&3u32.to_le_bytes()); // REVERB | CHORUS
        for (i, unit) in "Hitode DirectMusic Synth".encode_utf16().enumerate() {
            caps[0x34 + i * 2..0x36 + i * 2].copy_from_slice(&unit.to_le_bytes());
        }
        env.cpu.write(caps_ptr, &caps).map_err(|e| e.to_string())?;
        Ok(S_OK)
    })())
}

fn synth_set_master_clock(env: &mut Env<'_>, base: u32, clock: u32) -> Outcome {
    if clock == 0 {
        return Outcome::Return(E_POINTER);
    }
    let (old_clock, sink, is_native_sink) = {
        let state = env.ext.get_mut::<State>();
        let synth = state.synths.get_mut(&base).unwrap();
        let old = synth.master_clock;
        synth.master_clock = clock;
        let sink = synth.sink;
        let is_native = state.sinks.contains_key(&sink);
        (old, sink, is_native)
    };
    com_call(env, clock, 1, Vec::new(), move |env, _| {
        let after_old = move |env: &mut Env<'_>| {
            if sink == 0 {
                return Outcome::Return(S_OK);
            }
            if is_native_sink {
                return sink_set_master_clock(env, sink, clock);
            }
            com_call(env, sink, 4, vec![clock], |_, hr| Outcome::Return(hr))
        };
        if old_clock != 0 {
            com_call(env, old_clock, 2, Vec::new(), move |env, _| after_old(env))
        } else {
            after_old(env)
        }
    })
}

fn synth_get_latency_clock(env: &mut Env<'_>, base: u32, out_ptr: u32) -> Outcome {
    if out_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    let (sink, is_native) = {
        let state = env.ext.get::<State>();
        let sink = state.synths.get(&base).unwrap().sink;
        (sink, state.sinks.contains_key(&sink))
    };
    if sink == 0 {
        return Outcome::Return(DMUS_E_NOSYNTHSINK);
    }
    if is_native {
        return sink_get_latency_clock(env, sink, out_ptr);
    }
    com_call(env, sink, 5, vec![out_ptr], |_, hr| Outcome::Return(hr))
}

fn synth_activate(env: &mut Env<'_>, base: u32, enable: u32) -> Outcome {
    let enable = enable != 0;
    let (open, already, sink, is_native_sink, stream) = {
        let state = env.ext.get::<State>();
        let synth = state.synths.get(&base).unwrap();
        (
            synth.open,
            synth.active,
            synth.sink,
            state.sinks.contains_key(&synth.sink),
            source_id(env.sys.pid, synth.id),
        )
    };
    if !open {
        return Outcome::Return(DMUS_E_SYNTHNOTCONFIGURED);
    }
    if sink == 0 {
        return Outcome::Return(DMUS_E_NOSYNTHSINK);
    }
    if enable == already {
        return Outcome::Return(S_FALSE);
    }
    if enable {
        let now = env.sys.clock.now_us();
        let synth = env.ext.get_mut::<State>().synths.get_mut(&base).unwrap();
        if let Err(error) = ensure_group(synth, 1) {
            return Outcome::Fail(error);
        }
        synth.origin_us = now + LEAD_US;
        synth.frames = 0;
        synth.active = true;
    } else {
        let synth = env.ext.get_mut::<State>().synths.get_mut(&base).unwrap();
        synth.active = false;
        synth.events.clear();
        for gs in synth.synths.values_mut() {
            let _ = gs.engine.event(MidiEvent::Silence);
            gs.running = 0;
        }
        if let Some(audio) = env.ext.try_get::<AudioControl>() {
            audio.remove_pcm(stream);
        }
    }
    if is_native_sink {
        return sink_activate(env, sink, u32::from(enable));
    }
    com_call(env, sink, 6, vec![u32::from(enable)], |_, hr| {
        Outcome::Return(hr)
    })
}

fn synth_set_synth_sink(env: &mut Env<'_>, base: u32, sink: u32) -> Outcome {
    let (active, old_sink, is_native_new, master_clock) = {
        let state = env.ext.get_mut::<State>();
        let synth = state.synths.get_mut(&base).unwrap();
        if synth.active {
            return Outcome::Return(DMUS_E_SYNTHACTIVE);
        }
        let old = synth.sink;
        synth.sink = sink;
        let mc = synth.master_clock;
        if old != 0
            && let Some(s) = state.sinks.get_mut(&old)
        {
            s.synth = 0;
        }
        let is_native = state.sinks.contains_key(&sink);
        (synth.active, old, is_native, mc)
    };
    let _ = active;
    let init_new = move |env: &mut Env<'_>| {
        if sink == 0 {
            return Outcome::Return(S_OK);
        }
        if is_native_new {
            if let Some(s) = env.ext.get_mut::<State>().sinks.get_mut(&sink) {
                s.refs += 1;
                s.synth = base;
            }
            if master_clock != 0 {
                return sink_set_master_clock(env, sink, master_clock);
            }
            return Outcome::Return(S_OK);
        }
        com_call(env, sink, 1, Vec::new(), move |env, _| {
            com_call(env, sink, 3, vec![base], move |env, hr| {
                if hr != S_OK || master_clock == 0 {
                    return Outcome::Return(hr);
                }
                com_call(env, sink, 4, vec![master_clock], |_, hr| {
                    Outcome::Return(hr)
                })
            })
        })
    };
    if old_sink != 0 {
        com_call(env, old_sink, 2, Vec::new(), move |env, _| init_new(env))
    } else {
        init_new(env)
    }
}

fn synth_render(
    env: &mut Env<'_>,
    base: u32,
    buf_ptr: u32,
    length: u32,
    pos_lo: u32,
    pos_hi: u32,
) -> Outcome {
    if buf_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    let _position = i64::from_le_bytes([
        pos_lo as u8,
        (pos_lo >> 8) as u8,
        (pos_lo >> 16) as u8,
        (pos_lo >> 24) as u8,
        pos_hi as u8,
        (pos_hi >> 8) as u8,
        (pos_hi >> 16) as u8,
        (pos_hi >> 24) as u8,
    ]);
    finish((|| {
        let now_us = env.sys.clock.now_us();
        let state = env.ext.get_mut::<State>();
        let synth = state.synths.get_mut(&base).unwrap();
        if !synth.open {
            return Ok(DMUS_E_SYNTHNOTCONFIGURED);
        }
        ensure_group(synth, 1)?;
        while let Some(&ev) = synth.events.peek() {
            if ev.at_us > now_us {
                break;
            }
            let ev = synth.events.pop().unwrap();
            apply_event(synth, ev.group, ev.event)?;
        }
        let frames = length as usize;
        let samples = render_mixed_frames(synth, frames);
        let gain = synth_gain(synth);
        let mut pcm = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            let scaled = (s * gain).clamp(-1.0, 1.0);
            let i16_val = (scaled * 32767.0).round() as i16;
            pcm.extend_from_slice(&i16_val.to_le_bytes());
        }
        state.stats.rendered_frames += length as u64;
        env.cpu.write(buf_ptr, &pcm).map_err(|e| e.to_string())?;
        Ok(S_OK)
    })())
}

fn synth_set_channel_priority(
    env: &mut Env<'_>,
    base: u32,
    group: u32,
    channel: u32,
    priority: u32,
) -> Outcome {
    if group == 0 || channel >= 16 {
        return Outcome::Return(E_INVALIDARG);
    }
    env.ext
        .get_mut::<State>()
        .synths
        .get_mut(&base)
        .unwrap()
        .priorities
        .insert((group, channel), priority);
    Outcome::Return(S_OK)
}

fn synth_get_channel_priority(
    env: &mut Env<'_>,
    base: u32,
    group: u32,
    channel: u32,
    out_ptr: u32,
) -> Outcome {
    if out_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    if group == 0 || channel >= 16 {
        return Outcome::Return(E_INVALIDARG);
    }
    let priority = env
        .ext
        .get::<State>()
        .synths
        .get(&base)
        .unwrap()
        .priorities
        .get(&(group, channel))
        .copied()
        .unwrap_or(if channel == 9 {
            0x8000_0000
        } else {
            0x4000_0000
        });
    finish(write_u32(env, out_ptr, priority).map(|()| S_OK))
}

fn synth_get_format(env: &mut Env<'_>, base: u32, fmt_ptr: u32, size_ptr: u32) -> Outcome {
    if size_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    if !env.ext.get::<State>().synths.get(&base).unwrap().open {
        return Outcome::Return(DMUS_E_SYNTHNOTCONFIGURED);
    }
    finish((|| {
        if fmt_ptr == 0 {
            write_u32(env, size_ptr, 18)?;
            return Ok(S_OK);
        }
        let capacity = read_u32(env, size_ptr)?;
        write_u32(env, size_ptr, 18)?;
        if capacity < 18 {
            return Ok(E_INVALIDARG);
        }
        let mut wf = [0u8; 18];
        wf[0..2].copy_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
        wf[2..4].copy_from_slice(&2u16.to_le_bytes()); // stereo
        wf[4..8].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
        wf[8..12].copy_from_slice(&(SAMPLE_RATE * 4).to_le_bytes());
        wf[12..14].copy_from_slice(&4u16.to_le_bytes());
        wf[14..16].copy_from_slice(&16u16.to_le_bytes());
        env.cpu.write(fmt_ptr, &wf).map_err(|e| e.to_string())?;
        Ok(S_OK)
    })())
}

fn synth_ks_method(env: &mut Env<'_>) -> Outcome {
    let slot = env.sys.thunks.get(env.thunk).f.data;
    let this = env.arg(0);
    let Some(base) = synth_base(env, this) else {
        return Outcome::Fail(format!("{MODULE}: unknown synth KsControl {this:#x}"));
    };
    match slot {
        0 => synth_qi(env, base, env.arg(1), env.arg(2)),
        1 => synth_addref(env, base),
        2 => synth_release(env, base),
        3 => synth_ks_property(
            env,
            base,
            env.arg(1),
            env.arg(2),
            env.arg(3),
            env.arg(4),
            env.arg(5),
        ),
        4 | 5 => Outcome::Return(DMUS_E_UNKNOWN_PROPERTY),
        _ => Outcome::Return(E_NOTIMPL),
    }
}

fn synth_ks_property(
    env: &mut Env<'_>,
    base: u32,
    prop_ptr: u32,
    prop_len: u32,
    data_ptr: u32,
    data_len: u32,
    bytes_ret_ptr: u32,
) -> Outcome {
    if prop_ptr == 0 || prop_len < 24 {
        return Outcome::Return(E_INVALIDARG);
    }
    finish((|| {
        let set = read_guid(env, prop_ptr)?;
        let id = read_u32(env, prop_ptr + 16)?;
        let flags = read_u32(env, prop_ptr + 20)?;
        if flags & KSPROPERTY_TYPE_BASICSUPPORT != 0 {
            if data_ptr == 0 || data_len < 4 {
                return Ok(E_INVALIDARG);
            }
            let support = if set == PROP_VOLUME || set == PROP_EFFECTS {
                KSPROPERTY_SUPPORT_GET | KSPROPERTY_SUPPORT_SET
            } else if set == PROP_GM_HARDWARE
                || set == PROP_GS_HARDWARE
                || set == PROP_XG_HARDWARE
                || set == PROP_DLS1
                || set == PROP_DLS2
                || set == PROP_INSTRUMENT2
                || set == PROP_MEMORY_SIZE
            {
                KSPROPERTY_SUPPORT_GET
            } else {
                0
            };
            write_u32(env, data_ptr, support)?;
            if bytes_ret_ptr != 0 {
                write_u32(env, bytes_ret_ptr, 4)?;
            }
            return Ok(S_OK);
        }
        if flags & KSPROPERTY_TYPE_GET != 0 {
            if data_ptr == 0 || data_len < 4 {
                return Ok(E_INVALIDARG);
            }
            let synth = env.ext.get::<State>().synths.get(&base).unwrap();
            let value = if set == PROP_GM_HARDWARE
                || set == PROP_DLS1
                || set == PROP_DLS2
                || set == PROP_INSTRUMENT2
            {
                1u32
            } else if set == PROP_GS_HARDWARE || set == PROP_XG_HARDWARE {
                0u32
            } else if set == PROP_MEMORY_SIZE {
                DMUS_PC_SYSTEMMEMORY
            } else if set == PROP_EFFECTS {
                synth.effects
            } else if set == PROP_VOLUME {
                (if id == 1 {
                    synth.volume_boost_db
                } else {
                    synth.volume_db
                }) as u32
            } else {
                0u32
            };
            write_u32(env, data_ptr, value)?;
            if bytes_ret_ptr != 0 {
                write_u32(env, bytes_ret_ptr, 4)?;
            }
            return Ok(S_OK);
        }
        if flags & KSPROPERTY_TYPE_SET != 0 {
            if data_ptr == 0 || data_len < 4 {
                return Ok(E_INVALIDARG);
            }
            let val = read_u32(env, data_ptr)?;
            if set == PROP_VOLUME {
                let clamped = (val as i32).clamp(-20_000, 2_000);
                let (stream, gain) = {
                    let synth = env.ext.get_mut::<State>().synths.get_mut(&base).unwrap();
                    if id == 1 {
                        synth.volume_boost_db = clamped;
                    } else {
                        synth.volume_db = clamped;
                    }
                    (source_id(env.sys.pid, synth.id), synth_gain(synth))
                };
                if let Some(audio) = env.ext.try_get::<AudioControl>() {
                    audio.set_stereo_gain(stream, [gain, gain]);
                }
            } else if set == PROP_EFFECTS {
                env.ext
                    .get_mut::<State>()
                    .synths
                    .get_mut(&base)
                    .unwrap()
                    .effects = val & 3;
            }
            return Ok(S_OK);
        }
        Ok(S_FALSE)
    })())
}

fn sink_qi(env: &mut Env<'_>, base: u32, iid_ptr: u32, out_ptr: u32, from_clock: bool) -> Outcome {
    if out_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    finish((|| {
        write_u32(env, out_ptr, 0)?;
        let iid = read_guid(env, iid_ptr)?;
        let ptr = if from_clock {
            if iid == IUNKNOWN || iid == IID_CLOCK {
                base + 8
            } else {
                return Ok(E_NOINTERFACE);
            }
        } else if iid == IUNKNOWN || iid == IID_SINK {
            base
        } else if iid == IID_KSCONTROL {
            base + 4
        } else {
            return Ok(E_NOINTERFACE);
        };
        env.ext
            .get_mut::<State>()
            .sinks
            .get_mut(&base)
            .unwrap()
            .refs += 1;
        write_u32(env, out_ptr, ptr)?;
        Ok(S_OK)
    })())
}

fn sink_addref(env: &mut Env<'_>, base: u32) -> Outcome {
    let refs = {
        let s = env.ext.get_mut::<State>().sinks.get_mut(&base).unwrap();
        s.refs += 1;
        s.refs
    };
    Outcome::Return(refs)
}

fn sink_release(env: &mut Env<'_>, base: u32) -> Outcome {
    let (refs, release_list, free_fn) = {
        let state = env.ext.get_mut::<State>();
        let sink = state.sinks.get_mut(&base).unwrap();
        sink.refs = sink.refs.saturating_sub(1);
        if sink.refs > 0 {
            return Outcome::Return(sink.refs);
        }
        let sink = state.sinks.remove(&base).unwrap();
        let mut to_release = Vec::new();
        if sink.master_clock != 0 {
            to_release.push(sink.master_clock);
        }
        if sink.dsound_buffer != 0 {
            to_release.push(sink.dsound_buffer);
        }
        if sink.dsound != 0 {
            to_release.push(sink.dsound);
        }
        (0u32, to_release, state.allocator.free)
    };
    free_fn(env, base);
    release_com_list(env, release_list, refs)
}

fn sink_method(env: &mut Env<'_>) -> Outcome {
    let slot = env.sys.thunks.get(env.thunk).f.data;
    let this = env.arg(0);
    let Some(base) = sink_base(env, this) else {
        return Outcome::Fail(format!("{MODULE}: unknown sink {this:#x}"));
    };
    match slot {
        0 => sink_qi(env, base, env.arg(1), env.arg(2), false),
        1 => sink_addref(env, base),
        2 => sink_release(env, base),
        3 => {
            env.ext
                .get_mut::<State>()
                .sinks
                .get_mut(&base)
                .unwrap()
                .synth = env.arg(1);
            Outcome::Return(S_OK)
        }
        4 => sink_set_master_clock(env, base, env.arg(1)),
        5 => sink_get_latency_clock(env, base, env.arg(1)),
        6 => sink_activate(env, base, env.arg(1)),
        7 => sink_sample_to_reftime(env, base, env.arg(1), env.arg(2), env.arg(3)),
        8 => sink_reftime_to_sample(env, base, env.arg(1), env.arg(2), env.arg(3)),
        9 => sink_set_dsound(env, base, env.arg(1), env.arg(2)),
        10 => {
            let out = env.arg(1);
            if out == 0 {
                return Outcome::Return(E_POINTER);
            }
            finish(write_u32(env, out, SAMPLE_RATE * 4 * 2).map(|()| S_OK))
        }
        _ => Outcome::Return(E_NOTIMPL),
    }
}

fn sink_set_master_clock(env: &mut Env<'_>, base: u32, clock: u32) -> Outcome {
    if clock == 0 {
        return Outcome::Return(E_POINTER);
    }
    let old = {
        let sink = env.ext.get_mut::<State>().sinks.get_mut(&base).unwrap();
        let old = sink.master_clock;
        sink.master_clock = clock;
        old
    };
    com_call(env, clock, 1, Vec::new(), move |env, _| {
        if old != 0 {
            com_call(env, old, 2, Vec::new(), |_, _| Outcome::Return(S_OK))
        } else {
            Outcome::Return(S_OK)
        }
    })
}

fn sink_get_latency_clock(env: &mut Env<'_>, base: u32, out_ptr: u32) -> Outcome {
    if out_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    let sink = env.ext.get_mut::<State>().sinks.get_mut(&base).unwrap();
    if sink.master_clock == 0 {
        return Outcome::Return(DMUS_E_NO_MASTER_CLOCK);
    }
    sink.refs += 1;
    finish(write_u32(env, out_ptr, base + 8).map(|()| S_OK))
}

fn sink_activate(env: &mut Env<'_>, base: u32, enable: u32) -> Outcome {
    let enable = enable != 0;
    let (master_clock, scratch) = {
        let state = env.ext.get_mut::<State>();
        let scratch = state.scratch;
        let sink = state.sinks.get_mut(&base).unwrap();
        if sink.synth == 0 {
            return Outcome::Return(DMUS_E_NOT_INIT);
        }
        if sink.active == enable {
            return Outcome::Return(S_FALSE);
        }
        sink.active = enable;
        (sink.master_clock, scratch)
    };
    if enable && master_clock != 0 {
        return com_call(env, master_clock, 3, vec![scratch], move |env, hr| {
            if hr == S_OK
                && let Ok(rt) = read_i64(env, scratch)
                && let Some(sink) = env.ext.get_mut::<State>().sinks.get_mut(&base)
            {
                sink.origin_rt = rt;
            }
            Outcome::Return(S_OK)
        });
    }
    Outcome::Return(S_OK)
}

fn sink_sample_to_reftime(
    env: &mut Env<'_>,
    base: u32,
    sample_lo: u32,
    sample_hi: u32,
    out_ptr: u32,
) -> Outcome {
    if out_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    let sample = i64::from_le_bytes([
        sample_lo as u8,
        (sample_lo >> 8) as u8,
        (sample_lo >> 16) as u8,
        (sample_lo >> 24) as u8,
        sample_hi as u8,
        (sample_hi >> 8) as u8,
        (sample_hi >> 16) as u8,
        (sample_hi >> 24) as u8,
    ]);
    let origin = env.ext.get::<State>().sinks.get(&base).unwrap().origin_rt;
    let rt =
        origin.saturating_add(((i128::from(sample) * 10_000_000) / i128::from(SAMPLE_RATE)) as i64);
    finish(write_i64(env, out_ptr, rt).map(|()| S_OK))
}

fn sink_reftime_to_sample(
    env: &mut Env<'_>,
    base: u32,
    rt_lo: u32,
    rt_hi: u32,
    out_ptr: u32,
) -> Outcome {
    if out_ptr == 0 {
        return Outcome::Return(E_POINTER);
    }
    let rt = i64::from_le_bytes([
        rt_lo as u8,
        (rt_lo >> 8) as u8,
        (rt_lo >> 16) as u8,
        (rt_lo >> 24) as u8,
        rt_hi as u8,
        (rt_hi >> 8) as u8,
        (rt_hi >> 16) as u8,
        (rt_hi >> 24) as u8,
    ]);
    let origin = env.ext.get::<State>().sinks.get(&base).unwrap().origin_rt;
    let sample =
        ((i128::from(rt.saturating_sub(origin)) * i128::from(SAMPLE_RATE)) / 10_000_000) as i64;
    finish(write_i64(env, out_ptr, sample).map(|()| S_OK))
}

fn sink_set_dsound(env: &mut Env<'_>, base: u32, dsound: u32, buffer: u32) -> Outcome {
    let (old_ds, old_buf) = {
        let sink = env.ext.get_mut::<State>().sinks.get_mut(&base).unwrap();
        if sink.active {
            return Outcome::Return(DMUS_E_SYNTHACTIVE);
        }
        let old = (sink.dsound, sink.dsound_buffer);
        sink.dsound = dsound;
        sink.dsound_buffer = buffer;
        old
    };
    let mut to_release = Vec::new();
    if old_buf != 0 {
        to_release.push(old_buf);
    }
    if old_ds != 0 {
        to_release.push(old_ds);
    }
    let add_buf = move |env: &mut Env<'_>| {
        if buffer != 0 {
            com_call(env, buffer, 1, Vec::new(), move |env, _| {
                release_com_list(env, to_release, S_OK)
            })
        } else {
            release_com_list(env, to_release, S_OK)
        }
    };
    if dsound != 0 {
        com_call(env, dsound, 1, Vec::new(), move |env, _| add_buf(env))
    } else {
        add_buf(env)
    }
}

fn sink_ks_method(env: &mut Env<'_>) -> Outcome {
    let slot = env.sys.thunks.get(env.thunk).f.data;
    let this = env.arg(0);
    let Some(base) = sink_base(env, this) else {
        return Outcome::Fail(format!("{MODULE}: unknown sink KsControl {this:#x}"));
    };
    match slot {
        0 => sink_qi(env, base, env.arg(1), env.arg(2), false),
        1 => sink_addref(env, base),
        2 => sink_release(env, base),
        3 => sink_ks_property(
            env,
            base,
            env.arg(1),
            env.arg(2),
            env.arg(3),
            env.arg(4),
            env.arg(5),
        ),
        4 | 5 => Outcome::Return(DMUS_E_UNKNOWN_PROPERTY),
        _ => Outcome::Return(E_NOTIMPL),
    }
}

fn sink_ks_property(
    env: &mut Env<'_>,
    base: u32,
    prop_ptr: u32,
    prop_len: u32,
    data_ptr: u32,
    data_len: u32,
    bytes_ret_ptr: u32,
) -> Outcome {
    if prop_ptr == 0 || prop_len < 24 {
        return Outcome::Return(E_INVALIDARG);
    }
    finish((|| {
        let set = read_guid(env, prop_ptr)?;
        let flags = read_u32(env, prop_ptr + 20)?;
        if flags & KSPROPERTY_TYPE_BASICSUPPORT != 0 {
            if data_ptr == 0 || data_len < 4 {
                return Ok(E_INVALIDARG);
            }
            let support = if set == PROP_WRITE_LATENCY || set == PROP_WRITE_PERIOD {
                KSPROPERTY_SUPPORT_GET | KSPROPERTY_SUPPORT_SET
            } else if set == PROP_SINK_USES_DSOUND {
                KSPROPERTY_SUPPORT_GET
            } else {
                0
            };
            write_u32(env, data_ptr, support)?;
            if bytes_ret_ptr != 0 {
                write_u32(env, bytes_ret_ptr, 4)?;
            }
            return Ok(S_OK);
        }
        if flags & KSPROPERTY_TYPE_GET != 0 {
            if data_ptr == 0 || data_len < 4 {
                return Ok(E_INVALIDARG);
            }
            let sink = env.ext.get::<State>().sinks.get(&base).unwrap();
            let value = if set == PROP_WRITE_LATENCY {
                sink.write_latency_ms
            } else if set == PROP_WRITE_PERIOD {
                sink.write_period_ms
            } else {
                0
            };
            write_u32(env, data_ptr, value)?;
            if bytes_ret_ptr != 0 {
                write_u32(env, bytes_ret_ptr, 4)?;
            }
            return Ok(S_OK);
        }
        if flags & KSPROPERTY_TYPE_SET != 0 {
            if data_ptr == 0 || data_len < 4 {
                return Ok(E_INVALIDARG);
            }
            let value = read_u32(env, data_ptr)?;
            let sink = env.ext.get_mut::<State>().sinks.get_mut(&base).unwrap();
            if set == PROP_WRITE_LATENCY {
                sink.write_latency_ms = value.clamp(5, 1000);
            } else if set == PROP_WRITE_PERIOD {
                sink.write_period_ms = value.clamp(5, 1000);
            }
            return Ok(S_OK);
        }
        Ok(S_FALSE)
    })())
}

fn clock_method(env: &mut Env<'_>) -> Outcome {
    let slot = env.sys.thunks.get(env.thunk).f.data;
    let this = env.arg(0);
    let Some(base) = sink_base(env, this) else {
        return Outcome::Fail(format!("{MODULE}: unknown latency clock {this:#x}"));
    };
    match slot {
        0 => sink_qi(env, base, env.arg(1), env.arg(2), true),
        1 => sink_addref(env, base),
        2 => sink_release(env, base),
        3 => {
            let out = env.arg(1);
            if out == 0 {
                return Outcome::Return(E_POINTER);
            }
            let master_clock = env
                .ext
                .get::<State>()
                .sinks
                .get(&base)
                .unwrap()
                .master_clock;
            if master_clock == 0 {
                return Outcome::Return(DMUS_E_NO_MASTER_CLOCK);
            }
            com_call(env, master_clock, 3, vec![out], move |env, hr| {
                if hr != S_OK {
                    return Outcome::Return(hr);
                }
                finish((|| {
                    let rt = read_i64(env, out)?;
                    write_i64(env, out, rt.saturating_add(LATENCY_100NS))?;
                    Ok(S_OK)
                })())
            })
        }
        4..=6 => Outcome::Return(E_NOTIMPL),
        _ => Outcome::Return(E_FAIL),
    }
}

/// Build a minimal valid DLS Level 1 collection carrying `GUID_DefaultGMCollection`
/// and all 128 General MIDI melodic programs plus standard GM/GS drum kits.
///
/// Wine's `dmloader.dll` and `dmusic.dll` parse this file when guests request
/// `GUID_DefaultGMCollection` and download patches to the port; actual waveform
/// synthesis runs natively through `adv-device::midi::Synth`.
pub fn default_gm_dls() -> Vec<u8> {
    const DRUM_PROGRAMS: [u32; 9] = [0, 8, 16, 24, 25, 32, 40, 48, 56];
    const F_INSTRUMENT_DRUMS: u32 = 0x8000_0000;

    fn chunk(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + payload.len() + (payload.len() & 1));
        out.extend_from_slice(fourcc);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        if payload.len() & 1 != 0 {
            out.push(0);
        }
        out
    }

    fn list(kind: &[u8; 4], children: &[u8]) -> Vec<u8> {
        let mut payload = Vec::with_capacity(4 + children.len());
        payload.extend_from_slice(kind);
        payload.extend_from_slice(children);
        chunk(b"LIST", &payload)
    }

    fn build_instrument(bank: u32, program: u32, name: &str) -> Vec<u8> {
        let mut insh = [0u8; 12];
        insh[0..4].copy_from_slice(&1u32.to_le_bytes()); // cRegions = 1
        insh[4..8].copy_from_slice(&bank.to_le_bytes());
        insh[8..12].copy_from_slice(&program.to_le_bytes());

        let mut rgnh = [0u8; 12];
        rgnh[0..2].copy_from_slice(&0u16.to_le_bytes());
        rgnh[2..4].copy_from_slice(&127u16.to_le_bytes());
        rgnh[4..6].copy_from_slice(&0u16.to_le_bytes());
        rgnh[6..8].copy_from_slice(&127u16.to_le_bytes());

        let mut wsmp = [0u8; 20];
        wsmp[0..4].copy_from_slice(&20u32.to_le_bytes());
        wsmp[4..6].copy_from_slice(&60u16.to_le_bytes());

        let mut wlnk = [0u8; 12];
        wlnk[4..8].copy_from_slice(&1u32.to_le_bytes()); // F_WAVELINK_CHANNEL_LEFT

        let mut rgn_body = Vec::new();
        rgn_body.extend_from_slice(&chunk(b"rgnh", &rgnh));
        rgn_body.extend_from_slice(&chunk(b"wsmp", &wsmp));
        rgn_body.extend_from_slice(&chunk(b"wlnk", &wlnk));
        let lrgn = list(b"lrgn", &list(b"rgn ", &rgn_body));

        let mut inam = name.as_bytes().to_vec();
        inam.push(0);
        let info = list(b"INFO", &chunk(b"INAM", &inam));

        let mut ins_body = Vec::new();
        ins_body.extend_from_slice(&chunk(b"insh", &insh));
        ins_body.extend_from_slice(&lrgn);
        ins_body.extend_from_slice(&info);
        list(b"ins ", &ins_body)
    }

    let mut lins_body = Vec::new();
    for program in 0..128u32 {
        lins_body.extend_from_slice(&build_instrument(0, program, &format!("GM {program}")));
    }
    for program in DRUM_PROGRAMS {
        lins_body.extend_from_slice(&build_instrument(
            F_INSTRUMENT_DRUMS,
            program,
            &format!("Drum {program}"),
        ));
    }
    let count = 128u32 + DRUM_PROGRAMS.len() as u32;

    let mut ptbl = [0u8; 12];
    ptbl[0..4].copy_from_slice(&8u32.to_le_bytes()); // cbSize
    ptbl[4..8].copy_from_slice(&1u32.to_le_bytes()); // cCues
    ptbl[8..12].copy_from_slice(&0u32.to_le_bytes()); // ulOffset[0]

    let mut fmt = [0u8; 16];
    fmt[0..2].copy_from_slice(&1u16.to_le_bytes()); // PCM
    fmt[2..4].copy_from_slice(&1u16.to_le_bytes()); // mono
    fmt[4..8].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
    fmt[8..12].copy_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    fmt[12..14].copy_from_slice(&2u16.to_le_bytes());
    fmt[14..16].copy_from_slice(&16u16.to_le_bytes());
    let mut wave_body = Vec::new();
    wave_body.extend_from_slice(&chunk(b"fmt ", &fmt));
    wave_body.extend_from_slice(&chunk(b"data", &[0u8; 64]));
    let wvpl = list(b"wvpl", &list(b"wave", &wave_body));

    let info = list(b"INFO", &chunk(b"INAM", b"Hitode GM Collection\0"));

    let mut body = Vec::new();
    body.extend_from_slice(b"DLS ");
    body.extend_from_slice(&chunk(b"dlid", &GUID_DEFAULT_GM_COLLECTION));
    body.extend_from_slice(&chunk(b"colh", &count.to_le_bytes()));
    body.extend_from_slice(&list(b"lins", &lins_body));
    body.extend_from_slice(&chunk(b"ptbl", &ptbl));
    body.extend_from_slice(&wvpl);
    body.extend_from_slice(&info);
    chunk(b"RIFF", &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::Cpu;
    use adv_machine::{Clock, layout::PARK_ADDR};

    fn test_allocator() -> GuestAllocator {
        GuestAllocator {
            allocate: |env, size| {
                env.sys
                    .vm
                    .alloc(env.cpu, size.div_ceil(PAGE) * PAGE, Prot::RW)
                    .ok()
            },
            free: |env, addr| {
                let _ = env.sys.vm.free(env.cpu, addr);
            },
        }
    }

    // spec: WINE-AUD-13
    #[test]
    fn default_gm_dls_has_valid_riff_header_guid_and_instruments() {
        let dls = default_gm_dls();
        assert_eq!(&dls[0..4], b"RIFF");
        assert_eq!(&dls[8..12], b"DLS ");
        let riff_size = u32::from_le_bytes(dls[4..8].try_into().unwrap()) as usize;
        assert_eq!(riff_size + 8, dls.len());
        assert_eq!(&dls[12..16], b"dlid");
        assert_eq!(&dls[20..36], &GUID_DEFAULT_GM_COLLECTION);
        assert_eq!(&dls[36..40], b"colh");
        let count = u32::from_le_bytes(dls[44..48].try_into().unwrap());
        assert_eq!(count, 137);
        assert!(dls.len() < 32 * 1024);
    }

    // spec: WINE-AUD-13
    #[test]
    fn volume_conversion_and_play_buffer_synthesize_non_silent_pcm() {
        assert_eq!(db_to_gain(-10_000), 0.0);
        assert!((db_to_gain(0) - 1.0).abs() < 1e-6);
        assert!((db_to_gain(-2000) - 0.1).abs() < 1e-5);

        let mut registry = Registry::default();
        register(&mut registry);
        let mut machine = Machine::new(
            Cpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let audio = AudioControl::new(false);
        let stream = audio.stream();
        machine.ext.insert(audio);
        install(&mut machine, test_allocator()).unwrap();

        let (synth_ptr, sink_ptr, t0) = {
            let mut env = Env {
                context: machine.sys.current,
                cpu: &mut machine.cpu,
                sys: &mut machine.sys,
                ext: &mut machine.ext,
                thunk: 0,
            };
            let out = env.sys.vm.alloc(env.cpu, PAGE, Prot::RW).unwrap();
            assert_eq!(create_synth(&mut env, IID_SYNTH8, out).unwrap(), S_OK);
            let synth_ptr = read_u32(&env, out).unwrap();
            assert_eq!(create_sink(&mut env, IID_SINK, out).unwrap(), S_OK);
            let sink_ptr = read_u32(&env, out).unwrap();

            let t0 = env.sys.clock.now_us();
            let s = env.ext.get_mut::<State>();
            s.synths.get_mut(&synth_ptr).unwrap().open = true;
            s.synths.get_mut(&synth_ptr).unwrap().sink = sink_ptr;
            s.synths.get_mut(&synth_ptr).unwrap().active = true;
            s.synths.get_mut(&synth_ptr).unwrap().origin_us = t0 + LEAD_US;
            s.sinks.get_mut(&sink_ptr).unwrap().synth = synth_ptr;
            s.sinks.get_mut(&sink_ptr).unwrap().active = true;

            // Pack program change (piano) + note on (C4, vel 100).
            let mut buf = Vec::new();
            for msg in [0x0000_00c0u32, 0x0064_3c90u32] {
                buf.extend_from_slice(&3u32.to_le_bytes()); // cbEvent
                buf.extend_from_slice(&1u32.to_le_bytes()); // dwChannelGroup
                buf.extend_from_slice(&0i64.to_le_bytes()); // rtDelta
                buf.extend_from_slice(&DMUS_EVENT_STRUCTURED.to_le_bytes());
                buf.extend_from_slice(&msg.to_le_bytes());
            }
            assert_eq!(
                enqueue_buffer(&mut env, synth_ptr, 0, 0, &buf).unwrap(),
                S_OK
            );
            (synth_ptr, sink_ptr, t0)
        };

        assert!(machine.ext.get::<State>().synths.contains_key(&synth_ptr));
        assert!(machine.ext.get::<State>().sinks.contains_key(&sink_ptr));
        machine.sys.clock.advance_to(t0 + 80_000);
        let next = pump(&mut machine).unwrap();
        assert!(next.is_some());
        assert_eq!(stats(&machine).short_events, 2);
        assert!(stats(&machine).rendered_frames > 0);

        let mut reader = stream.device_reader(SAMPLE_RATE, 2).unwrap();
        let mut samples = vec![0.0f32; 4410 * 2];
        reader.read(&mut samples, 0);
        assert!(samples.iter().all(|v| v.is_finite()));
        assert!(samples.iter().any(|v| v.abs() > 0.001));
    }
}
