//! Whole-packet execution for the reviewed static Theora decoder ABI.
use super::{
    Entry, Mode,
    ogg::{self, Packet, Source},
    run_ahead::{self, RunAhead, Staging},
};
use adv_abi::{code_signature::ExecutableRange, library_signature::Family};
use adv_cpu::backend::{Guest, GuestExt, Reg};
use adv_device::theora::{Decoder, Frame, Output, RUN_AHEAD_DEPTH, Setup, State};
use adv_machine::{Conv, Env, Ext, HostFn, Outcome};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, atomic::Ordering};
use std::time::Instant;

const FAMILY: &str = include_str!("../../../../data/codec/theora-msvc-x86.txt");
const MODULE: &str = "adv-theora";
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub installed: u64,
    pub initialized: u64,
    pub frames: u64,
    pub native_frames: u64,
    pub compared_bytes: u64,
    pub different_bytes: u64,
    pub state_mismatches: u64,
    pub decode_ns: u64,
    pub run_ahead: run_ahead::Stats,
    pub reason: Option<String>,
}
#[derive(Clone, Copy)]
struct Layout {
    info_bytes: u32,
    init_bytes: u32,
    info: u32,
    ref_planes: u32,
    ref_indices: u32,
    plane_allocation: u32,
    keyframe: u32,
    current: u32,
    granpos: u32,
    frame_type: u32,
    bias: u32,
    pp_level: u32,
    output: u32,
    stripe: u32,
}
impl Layout {
    fn from_fields(fields: &BTreeMap<String, u32>) -> Result<Self, String> {
        let get = |name: &str| {
            fields
                .get(name)
                .copied()
                .ok_or_else(|| format!("missing Theora layout field {name}"))
        };
        let layout = Self {
            info_bytes: get("info_bytes")?,
            init_bytes: get("init_bytes")?,
            info: get("info")?,
            ref_planes: get("ref_planes")?,
            ref_indices: get("ref_indices")?,
            plane_allocation: get("plane_allocation")?,
            keyframe: get("keyframe")?,
            current: get("current")?,
            granpos: get("granpos")?,
            frame_type: get("frame_type")?,
            bias: get("bias")?,
            pp_level: get("pp_level")?,
            output: get("output")?,
            stripe: get("stripe")?,
        };
        for (offset, size) in [
            (layout.info, layout.info_bytes),
            (layout.ref_planes, 48),
            (layout.ref_indices, 24),
            (layout.plane_allocation, 4),
            (layout.keyframe, 8),
            (layout.current, 8),
            (layout.granpos, 8),
            (layout.frame_type, 1),
            (layout.bias, 1),
            (layout.pp_level, 4),
            (layout.output, 48),
            (layout.stripe, 8),
        ] {
            if offset
                .checked_add(size)
                .is_none_or(|end| end > layout.init_bytes)
            {
                return Err("Theora layout field outside decoder header".into());
            }
        }
        Ok(layout)
    }
}

struct Binding {
    layout: Layout,
    live: bool,
    allocator: u32,
    init: Entry,
    packet: Entry,
    clear: Entry,
    ctl: Option<Entry>,
    snapshots: Vec<(u32, Vec<u8>)>,
}
struct Instance {
    binding: usize,
    decoder: RunAhead<Decoder>,
    verify: Option<Decoder>,
    source: Option<Source>,
    next_packetno: Option<i64>,
    uploads: [Vec<u8>; 3],
    published: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Control {
    state: State,
    stripe: [u8; 8],
}

pub(super) struct Stage {
    result: i32,
    state: State,
    frame: Frame,
}

impl Staging for Decoder {
    type Input = Packet;
    type Control = Control;
    type Stage = Stage;

    fn stage(&mut self, input: &Packet, control: &mut Control) -> Stage {
        let result = decode_packet(self, input, false, &mut control.state);
        Stage {
            result,
            state: control.state,
            frame: self.snapshot(),
        }
    }

    fn restore(&mut self, committed: &Stage) {
        self.restore(&committed.frame);
    }
}

impl Instance {
    fn new(binding: usize, decoder: Decoder, verify: Option<Decoder>) -> Self {
        let initial = Stage {
            result: 0,
            state: State::default(),
            frame: decoder.snapshot(),
        };
        Self {
            binding,
            decoder: RunAhead::new(decoder, initial),
            verify,
            source: None,
            next_packetno: None,
            uploads: std::array::from_fn(|_| Vec::new()),
            published: false,
        }
    }
}
struct Provider {
    mode: Mode,
    bindings: Vec<Binding>,
    decoders: BTreeMap<u32, Instance>,
    stats: Stats,
    file_counts: Arc<run_ahead::FileCounts>,
}

pub(super) fn stats(ext: &Ext) -> Stats {
    ext.try_get::<Provider>().map_or_else(Stats::default, |p| {
        let mut stats = p.stats.clone();
        stats.run_ahead.predicted += p.file_counts.predicted.load(Ordering::Relaxed);
        stats.run_ahead.readahead_packets = p.file_counts.readahead_packets.load(Ordering::Relaxed);
        stats
    })
}

pub(super) fn run_ahead_stats(ext: &mut Ext) -> &mut run_ahead::Stats {
    &mut ext.get_mut::<Provider>().stats.run_ahead
}

pub(super) fn install(ext: &mut Ext, mode: Mode) {
    ext.insert(Provider {
        mode,
        bindings: Vec::new(),
        decoders: BTreeMap::new(),
        stats: Stats::default(),
        file_counts: Arc::default(),
    });
}

pub(super) fn host_fn(module: &str, name: &str) -> Option<HostFn> {
    if module != MODULE {
        return None;
    }
    let (name, index) = name.split_once('_')?;
    let (handler, argc) = match name {
        "init" => (init as adv_machine::Handler, 1),
        "packet" => (packet as adv_machine::Handler, 1),
        "clear" => (clear as adv_machine::Handler, 0),
        "ctl" => (ctl as adv_machine::Handler, 4),
        _ => return None,
    };
    Some(HostFn {
        conv: Conv::Cdecl { argc },
        handler,
        data: index.parse().ok()?,
        leaf: None,
        trace: false,
        guest: None,
    })
}

pub(super) fn bind(e: &mut Env<'_>, ranges: &[ExecutableRange]) -> Result<(), String> {
    bind_family(e, ranges, &Family::parse(FAMILY)?)
}

fn bind_family(e: &mut Env<'_>, ranges: &[ExecutableRange], family: &Family) -> Result<(), String> {
    let Some(library) = family.scan(ranges, &mut |a, n| read(e.cpu, a, n).ok()) else {
        return Ok(());
    };
    let layout = Layout::from_fields(&library.layout)?;
    let index = e.ext.get::<Provider>().bindings.len();
    let mut hooks = Vec::new();
    for name in ["init", "packet", "clear"]
        .into_iter()
        .chain(library.hooks.contains_key("ctl").then_some("ctl"))
    {
        let &(address, prefix) = &library.hooks[name];
        let bytes = &library
            .snapshots
            .iter()
            .find(|(a, _)| *a == address)
            .expect("verified hook body")
            .1;
        hooks.push(super::entry(
            e.cpu,
            e.sys,
            MODULE,
            &format!("{name}_{index}"),
            address,
            prefix,
            bytes,
        )?);
    }
    // Publish the redirects together, after every trampoline exists.
    for hook in &hooks {
        let bytes = &library
            .snapshots
            .iter()
            .find(|(a, _)| *a == hook.entry)
            .expect("verified hook body")
            .1;
        super::redirect(e.sys, hook, bytes);
    }
    let mut hooks = hooks.into_iter();
    let provider = e.ext.get_mut::<Provider>();
    provider.bindings.push(Binding {
        layout,
        live: true,
        allocator: library.symbols["malloc"],
        init: hooks.next().unwrap(),
        packet: hooks.next().unwrap(),
        clear: hooks.next().unwrap(),
        ctl: hooks.next(),
        snapshots: library.snapshots,
    });
    provider.stats.installed += 1;
    Ok(())
}

pub(super) fn forget(ext: &mut Ext, base: u32, size: u32) {
    let provider = ext.get_mut::<Provider>();
    for binding in &mut provider.bindings {
        if binding.init.entry >= base
            && u64::from(binding.init.entry) < u64::from(base) + u64::from(size)
        {
            binding.live = false;
        }
    }
    provider.decoders.retain(|_, instance| {
        let live = provider.bindings[instance.binding].live;
        if !live {
            instance.decoder.invalidate(&mut provider.stats.run_ahead);
        }
        live
    });
}

pub(super) fn invalidate_sources(ext: &mut Ext, invalid: impl Fn(Source) -> bool) {
    let provider = ext.get_mut::<Provider>();
    for instance in provider.decoders.values_mut() {
        if instance.source.is_some_and(&invalid) {
            instance.decoder.invalidate(&mut provider.stats.run_ahead);
            instance.source = None;
        }
    }
}

fn original(e: &mut Env<'_>, target: u32) -> Outcome {
    match e.checkpoint() {
        Ok(mut cp) => {
            cp.state.eip = target;
            Outcome::ResumeExecution(cp)
        }
        Err(error) => Outcome::Fail(error.to_string()),
    }
}
fn index(e: &Env<'_>) -> usize {
    e.sys.thunks.get(e.thunk).f.data as usize
}
fn clear(e: &mut Env<'_>) -> Outcome {
    let index = index(e);
    if !e.ext.get::<Provider>().bindings[index].live {
        return Outcome::Fail("Theora binding image was retired".into());
    }
    let address = e.cpu.reg(Reg::Ecx);
    let provider = e.ext.get_mut::<Provider>();
    remove_decoder(provider, address);
    original(e, e.ext.get::<Provider>().bindings[index].clear.original)
}

fn remove_decoder(provider: &mut Provider, address: u32) {
    if let Some(mut instance) = provider.decoders.remove(&address) {
        instance.decoder.invalidate(&mut provider.stats.run_ahead);
    }
}

fn ctl(e: &mut Env<'_>) -> Outcome {
    let index = index(e);
    let address = e.arg(0);
    let provider = e.ext.get_mut::<Provider>();
    let binding = &provider.bindings[index];
    if !binding.live {
        return Outcome::Fail("Theora binding image was retired".into());
    }
    let target = binding.ctl.as_ref().unwrap().original;
    if let Some(instance) = provider.decoders.get_mut(&address) {
        instance.decoder.invalidate(&mut provider.stats.run_ahead);
    }
    original(e, target)
}
fn read(cpu: &dyn Guest, address: u32, len: usize) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0; len];
    cpu.read(address, &mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}
fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn setup(cpu: &dyn Guest, info: u32, address: u32, info_bytes: u32) -> Result<Setup, String> {
    let raw = read(cpu, address, 712)?;
    let huffman = (0..80)
        .map(|i| {
            let address = word(&raw, i * 4);
            let mut pending = vec![0usize];
            let mut visited = BTreeSet::new();
            let mut length = 0;
            while let Some(at) = pending.pop() {
                if !visited.insert(at) {
                    return Err("cyclic Huffman tree".into());
                }
                let bits =
                    i16::from_le_bytes(read(cpu, address + at as u32 * 2, 2)?.try_into().unwrap());
                if !(0..=15).contains(&bits) {
                    return Err("invalid Huffman tree width".into());
                }
                let count = 1usize << bits;
                let end = at + 1 + count;
                if end > 32768 {
                    return Err("Huffman tree exceeds 16-bit indices".into());
                }
                length = length.max(end);
                let children = read(cpu, address + (at as u32 + 1) * 2, count * 2)?;
                let mut i = 0;
                while i < count {
                    let child = i16::from_le_bytes(children[i * 2..i * 2 + 2].try_into().unwrap());
                    if child > 0 {
                        pending.push(child as usize);
                        i += 1;
                    } else {
                        let used = -(i32::from(child)) >> 8;
                        if used > i32::from(bits) {
                            return Err("invalid Huffman leaf".into());
                        }
                        i += 1usize << (i32::from(bits) - used);
                    }
                }
            }
            Ok(read(cpu, address, length * 2)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b))
                .collect::<Vec<_>>())
        })
        .collect::<Result<Vec<_>, String>>()?;
    let ranges = (0..6)
        .map(|i| {
            let at = 640 + i * 12;
            let count = word(&raw, at) as usize;
            if !(1..=63).contains(&count) {
                return Err("invalid quantization ranges".into());
            }
            let sizes = read(cpu, word(&raw, at + 4), count * 4)?
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| i32::from_le_bytes(*b))
                .collect::<Vec<_>>();
            if sizes.iter().any(|&n| n <= 0)
                || sizes.iter().map(|&n| i64::from(n)).sum::<i64>() != 63
            {
                return Err("invalid quantization sizes".into());
            }
            Ok((sizes, read(cpu, word(&raw, at + 8), (count + 1) * 64)?))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Setup {
        info: read(cpu, info, info_bytes as usize)?
            .try_into()
            .map_err(|_| "unsupported Theora info size")?,
        huffman: huffman.try_into().ok().unwrap(),
        dc_scale: std::array::from_fn(|i| {
            u16::from_le_bytes(raw[320 + i * 2..322 + i * 2].try_into().unwrap())
        }),
        ac_scale: std::array::from_fn(|i| {
            u16::from_le_bytes(raw[448 + i * 2..450 + i * 2].try_into().unwrap())
        }),
        loop_filter_limits: raw[576..640].try_into().unwrap(),
        ranges: ranges.try_into().ok().unwrap(),
    })
}
fn init(e: &mut Env<'_>) -> Outcome {
    let index = index(e);
    if !e.ext.get::<Provider>().bindings[index].live {
        return Outcome::Fail("Theora binding image was retired".into());
    }
    let address = e.cpu.reg(Reg::Ecx);
    let info = e.cpu.reg(Reg::Edx);
    let Ok(tables) = e.cpu.arg(0) else {
        return Outcome::Fail("unreadable Theora initialization".into());
    };
    let provider = e.ext.get_mut::<Provider>();
    remove_decoder(provider, address);
    let target = provider.bindings[index].init.original;
    let mode = provider.mode;
    let native = setup(
        e.cpu,
        info,
        tables,
        provider.bindings[index].layout.info_bytes,
    )
    .and_then(|setup| {
        let decoder = Decoder::with_run_ahead(&setup)?;
        let verify = (mode == Mode::Verify)
            .then(|| Decoder::new(&setup))
            .transpose()?;
        Ok((decoder, verify))
    });
    if mode == Mode::Native {
        return match native {
            Ok((native, _)) => native_init(e, index, address, native),
            Err(reason) => {
                e.ext.get_mut::<Provider>().stats.reason = Some(reason);
                original(e, target)
            }
        };
    }
    Outcome::call(target, vec![tables], move |e, result| {
        if result.value() == 0 {
            let provider = e.ext.get_mut::<Provider>();
            match native {
                Ok((native, verify)) => {
                    provider
                        .decoders
                        .insert(address, Instance::new(index, native, verify));
                    provider.stats.initialized += 1;
                }
                Err(reason) => provider.stats.reason = Some(reason),
            }
        }
        Outcome::Return(result.value())
    })
}
/// Only public output storage remains guest-owned. The native decoder owns
/// its fragment maps, Huffman copies, references and reconstruction scratch.
fn native_init(e: &mut Env<'_>, index: usize, address: u32, native: Decoder) -> Outcome {
    let (info, bias) = native.initialized_info();
    let width = word(&info, 4);
    let height = word(&info, 8);
    let format = word(&info, 48);
    let hdec = u32::from(format & 1 == 0);
    let vdec = u32::from(format & 2 == 0);
    let ystride = width + 32;
    let cstride = ystride >> hdec;
    let ysize = u64::from(ystride) * u64::from(height + 32);
    let csize = u64::from(cstride) * u64::from((height + 32) >> vdec);
    let Ok(size) = u32::try_from(ysize + 2 * csize) else {
        return Outcome::Return((-23i32) as u32);
    };
    let binding = &e.ext.get::<Provider>().bindings[index];
    let layout = binding.layout;
    let slot = binding.allocator;
    let allocator = match read(e.cpu, slot, 4) {
        Ok(value) => word(&value, 0),
        Err(error) => return Outcome::Fail(error),
    };
    Outcome::call(allocator, vec![size], move |e, result| {
        let allocation = result.value();
        if allocation == 0 {
            return Outcome::Return((-1i32) as u32);
        }
        let mut header = vec![0u8; layout.init_bytes as usize];
        header[layout.ref_indices as usize..layout.ref_indices as usize + 24].fill(255);
        let mut write = |offset: u32, value: &[u8]| {
            let start = offset as usize;
            header[start..start + value.len()].copy_from_slice(value);
        };
        write(layout.info, &info);
        write(layout.current, &(-1i64).to_le_bytes());
        write(layout.frame_type, &[255]);
        write(layout.bias, &[bias]);
        // The reviewed clear routine frees this allocation once; all other
        // guest-private allocations and Huffman pointers are null.
        write(layout.plane_allocation, &allocation.to_le_bytes());
        let mut base = allocation;
        for plane in 0..3 {
            let (w, h, stride, pad_x, pad_y, bytes) = if plane == 0 {
                (width, height, ystride, 16, 16, ysize as u32)
            } else {
                (
                    width >> hdec,
                    height >> vdec,
                    cstride,
                    16 >> hdec,
                    16 >> vdec,
                    csize as u32,
                )
            };
            let top = base + pad_x + pad_y * stride;
            let data = top + (h - 1) * stride;
            let at = layout.ref_planes + plane * 16;
            for (offset, value) in [(0, w), (4, h), (8, (-(stride as i32)) as u32), (12, data)] {
                write(at + offset, &value.to_le_bytes());
            }
            base += bytes;
        }
        if let Err(error) = e.cpu.write(address, &header) {
            return Outcome::Fail(error.to_string());
        }
        let provider = e.ext.get_mut::<Provider>();
        provider
            .decoders
            .insert(address, Instance::new(index, native, None));
        provider.stats.initialized += 1;
        Outcome::Return(0)
    })
}

fn state(cpu: &dyn Guest, address: u32, layout: Layout) -> Result<State, String> {
    let mut counter = [0; 8];
    let mut read_counter = |offset| {
        cpu.read(address + offset, &mut counter)
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(i64::from_le_bytes(counter))
    };
    let mut frame_type = [0];
    cpu.read(address + layout.frame_type, &mut frame_type)
        .map_err(|e| e.to_string())?;
    Ok(State {
        keyframe_num: read_counter(layout.keyframe)?,
        curframe_num: read_counter(layout.current)?,
        granpos: read_counter(layout.granpos)?,
        frame_type: frame_type[0],
        pp_level: cpu
            .read_u32(address + layout.pp_level)
            .map_err(|e| e.to_string())? as i32,
    })
}
fn packet(e: &mut Env<'_>) -> Outcome {
    let index = index(e);
    if !e.ext.get::<Provider>().bindings[index].live {
        return Outcome::Fail("Theora binding image was retired".into());
    }
    let address = e.cpu.reg(Reg::Ecx);
    let op = e.cpu.reg(Reg::Edx);
    let target = e.ext.get::<Provider>().bindings[index].packet.original;
    if op == 0 {
        let provider = e.ext.get_mut::<Provider>();
        if let Some(instance) = provider.decoders.get_mut(&address) {
            instance.decoder.invalidate(&mut provider.stats.run_ahead);
        }
        return Outcome::Return((-1i32) as u32);
    }
    if !e.ext.get::<Provider>().decoders.contains_key(&address) {
        return original(e, target);
    }
    match decode(e, index, address, op, target, None) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::Fail(format!("Theora: {error}")),
    }
}
fn decode_packet(decoder: &mut Decoder, packet: &Packet, striped: bool, state: &mut State) -> i32 {
    decoder.decode(
        &packet.bytes,
        packet.packetno,
        packet.granulepos,
        packet.eos != 0,
        striped,
        state,
    )
}

pub(super) fn paired(ext: &Ext, source: Source) -> Vec<u32> {
    ext.get::<Provider>()
        .decoders
        .iter()
        .filter_map(|(&address, instance)| (instance.source == Some(source)).then_some(address))
        .collect()
}

pub(super) fn page_buffered(e: &mut Env<'_>, address: u32, source: Source) {
    if e.ext
        .get::<Provider>()
        .decoders
        .get(&address)
        .is_some_and(|instance| instance.source == Some(source))
    {
        predict(e, address, false);
    }
}

fn packet_finished(e: &mut Env<'_>, address: u32, packetno: i64) {
    e.ext
        .get_mut::<Provider>()
        .decoders
        .get_mut(&address)
        .unwrap()
        .next_packetno = Some(packetno.wrapping_add(1));
    predict(e, address, true);
}

fn predict(e: &mut Env<'_>, address: u32, count_miss: bool) {
    if e.ext
        .get_mut::<Provider>()
        .decoders
        .get_mut(&address)
        .unwrap()
        .decoder
        .refill_file(RUN_AHEAD_DEPTH)
    {
        return;
    }
    let provider = e.ext.get::<Provider>();
    let instance = &provider.decoders[&address];
    if instance.decoder.has_predictions() {
        return;
    }
    let layout = provider.bindings[instance.binding].layout;
    let control = state(e.cpu, address, layout).and_then(|state| {
        let stripe = read(e.cpu, address + layout.stripe, 8)?.try_into().unwrap();
        Ok(Control { state, stripe })
    });
    let callback = control
        .as_ref()
        .is_ok_and(|control| word(&control.stripe, 4) != 0);
    let (packets, tail) = instance
        .source
        .zip(instance.next_packetno)
        .filter(|_| !callback && control.is_ok())
        .map_or_else(Default::default, |(source, next)| {
            ogg::predict(e.cpu, e.ext, source, next, RUN_AHEAD_DEPTH)
        });
    let provider = e.ext.get_mut::<Provider>();
    let stats = &mut provider.stats.run_ahead;
    let predicted = control.is_ok_and(|control| {
        let decoder = &mut provider.decoders.get_mut(&address).unwrap().decoder;
        if let Some(mut tail) = tail {
            let mut packets = packets.into_iter();
            decoder.predict_file(
                control,
                RUN_AHEAD_DEPTH,
                move || {
                    packets
                        .next()
                        .map(|p| (p, false))
                        .or_else(|| tail.next().map(|p| (p, true)))
                },
                Arc::clone(&provider.file_counts),
                stats,
            )
        } else {
            decoder.predict(control, RUN_AHEAD_DEPTH, |_| packets, stats)
        }
    });
    if count_miss && !predicted {
        stats.no_prediction += 1;
        stats.no_prediction_callback += u64::from(callback);
    }
}

fn decode(
    e: &mut Env<'_>,
    index: usize,
    address: u32,
    op: u32,
    target: u32,
    saved: Option<(Packet, u32)>,
) -> Result<Outcome, String> {
    e.cpu.quiesce_memory();
    let binding = &e.ext.get::<Provider>().bindings[index];
    if !binding.snapshots.iter().all(|(a, b)| {
        if let Some(current) = e.cpu.shared_bytes(*a, b.len()) {
            current == b
        } else {
            read(e.cpu, *a, b.len()).is_ok_and(|current| current == *b)
        }
    }) {
        return Err("admitted library changed during decoding".into());
    }
    let layout = binding.layout;
    let observation = saved.is_none().then(|| ogg::take(e.ext, op)).flatten();
    if let Some((source, true)) = observation {
        ogg::locate_page(e.ext, source);
    }
    let (packet, granpos_out) = if let Some(saved) = saved {
        saved
    } else {
        (
            Packet::read(e.cpu, op)?,
            e.cpu.arg(0).map_err(|e| e.to_string())?,
        )
    };
    let mut state = state(e.cpu, address, layout)?;
    let stripe = read(e.cpu, address + layout.stripe, 8)?.try_into().unwrap();
    let control = Control { state, stripe };
    let callback = word(&stripe, 4) != 0;
    let started = Instant::now();
    let provider = e.ext.get_mut::<Provider>();
    let instance = provider.decoders.get_mut(&address).unwrap();
    let mode = provider.mode;
    let stats = &mut provider.stats;
    if let Some((source, advanced)) = observation {
        let source = advanced.then_some(source);
        if instance.source.is_some() && instance.source != source {
            instance.decoder.invalidate(&mut stats.run_ahead);
        }
        instance.source = source;
    }
    let hit = instance
        .decoder
        .try_commit(&packet, &control, &mut stats.run_ahead);
    let (result, rows) = if hit {
        let committed = instance.decoder.committed();
        state = committed.state;
        (committed.result, None)
    } else {
        let decoder = instance.decoder.decoder(&mut stats.run_ahead);
        let result = decode_packet(
            decoder,
            &packet,
            mode == Mode::Native && callback,
            &mut state,
        );
        if result == 2 {
            (result, Some(decoder.stripe()))
        } else {
            let frame = decoder.snapshot();
            instance.decoder.publish(Stage {
                result,
                state,
                frame,
            });
            (result, None)
        }
    };
    stats.decode_ns += started.elapsed().as_nanos() as u64;
    stats.frames += u64::from(result != 2);
    if mode == Mode::Verify {
        let frame = &instance.decoder.committed().frame;
        let planes = if result >= 0 {
            frame.output().as_ref().map(tight_planes).transpose()?
        } else {
            None
        };
        if let Some(sync) = &mut instance.verify {
            let mut sync_state = control.state;
            let sync_result = decode_packet(sync, &packet, false, &mut sync_state);
            if hit {
                stats.state_mismatches += u64::from(
                    sync_result != result || sync_state != state || !frame.references_match(sync),
                );
                if result >= 0 {
                    match (planes.as_ref(), sync.planes()) {
                        (Some(planes), Some(expected)) => {
                            for ((width, height, bytes), expected) in planes.iter().zip(expected) {
                                stats.state_mismatches += u64::from(
                                    (*width, *height) != (expected.width, expected.height),
                                );
                                compare_bytes(stats, bytes, &expected.data);
                            }
                        }
                        (None, None) => {}
                        _ => stats.state_mismatches += 1,
                    }
                }
            }
        }
        return Ok(Outcome::call(
            target,
            vec![granpos_out],
            move |e, resume| {
                let comparison = (|| -> Result<(), String> {
                    let actual = state_read(e.cpu, address, layout)?;
                    let actual_granpos = if result >= 0 && granpos_out != 0 {
                        let mut bytes = [0; 8];
                        e.cpu
                            .read(granpos_out, &mut bytes)
                            .map_err(|err| err.to_string())?;
                        i64::from_le_bytes(bytes)
                    } else {
                        state.granpos
                    };
                    let provider = e.ext.get_mut::<Provider>();
                    provider.stats.state_mismatches += u64::from(
                        resume.value() != result as u32
                            || actual_granpos != state.granpos
                            || actual
                                != (
                                    state.keyframe_num,
                                    state.curframe_num,
                                    state.granpos,
                                    state.frame_type,
                                ),
                    );
                    if let Some(planes) = planes {
                        let descriptors = read(e.cpu, address + layout.output, 48)?;
                        for (i, (width, height, bytes)) in planes.into_iter().enumerate() {
                            let stride = word(&descriptors, i * 16 + 8) as i32;
                            let pointer = word(&descriptors, i * 16 + 12);
                            if word(&descriptors, i * 16) as usize != width
                                || word(&descriptors, i * 16 + 4) as usize != height
                            {
                                return Err("verification plane dimensions differ".into());
                            }
                            for y in 0..height {
                                let actual = read(
                                    e.cpu,
                                    pointer.wrapping_add((y as i32 * stride) as u32),
                                    width,
                                )?;
                                let s = &mut e.ext.get_mut::<Provider>().stats;
                                compare_bytes(s, &actual, &bytes[y * width..(y + 1) * width]);
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = comparison {
                    let stats = &mut e.ext.get_mut::<Provider>().stats;
                    stats.state_mismatches += 1;
                    stats.reason = Some(error);
                }
                packet_finished(e, address, packet.packetno);
                Outcome::Return(resume.value())
            },
        ));
    }
    if result >= 0 && (result != 1 || !instance.published) {
        let descriptors = read(e.cpu, address + layout.ref_planes, 48)?;
        let output = if rows.is_some() {
            instance.decoder.decoder(&mut stats.run_ahead).output()
        } else {
            instance.decoder.committed().frame.output()
        };
        let planes = output.ok_or("native frame unavailable")?;
        for (i, (width, height)) in planes.dimensions().into_iter().enumerate() {
            let stride = word(&descriptors, i * 16 + 8) as i32;
            let pointer = word(&descriptors, i * 16 + 12);
            if word(&descriptors, i * 16) as usize != width
                || word(&descriptors, i * 16 + 4) as usize != height
            {
                return Err("native plane dimensions differ".into());
            }
            let pitch = stride.unsigned_abs() as usize;
            let size = pitch
                .checked_mul(height - 1)
                .and_then(|n| n.checked_add(width))
                .ok_or("output plane size overflow")?;
            let start = if stride < 0 {
                pointer
                    .checked_sub(
                        ((height - 1) * pitch)
                            .try_into()
                            .map_err(|_| "output plane wraps")?,
                    )
                    .ok_or("output plane wraps")?
            } else {
                pointer
            };
            let length = u32::try_from(size).map_err(|_| "output plane wraps")?;
            if pitch < width
                || !e
                    .cpu
                    .accessible(start, length, adv_cpu::backend::Prot::WRITE)
            {
                return Err("output plane inaccessible".into());
            }
            let output = &mut instance.uploads[i];
            output.resize(size, 0);
            let valid = planes
                .copy_plane(i, output, stride)
                .map_err(str::to_owned)?;
            if !valid.is_empty() {
                let first = valid.start * pitch;
                let end = (valid.end - 1) * pitch + width;
                e.cpu
                    .write(start + first as u32, &output[first..end])
                    .map_err(|e| e.to_string())?;
            }
        }
        e.cpu
            .write(address + layout.output, &descriptors)
            .map_err(|e| e.to_string())?;
        instance.published = true;
    }
    for (offset, value) in [
        (layout.keyframe, state.keyframe_num),
        (layout.current, state.curframe_num),
        (layout.granpos, state.granpos),
    ] {
        e.cpu
            .write(address + offset, &value.to_le_bytes())
            .map_err(|e| e.to_string())?;
    }
    e.cpu
        .write(address + layout.frame_type, &[state.frame_type])
        .map_err(|e| e.to_string())?;
    if result >= 0 && granpos_out != 0 {
        e.cpu
            .write(granpos_out, &state.granpos.to_le_bytes())
            .map_err(|e| e.to_string())?;
    }
    if let Some((first, end)) = rows {
        let mut descriptors = read(e.cpu, address + layout.output, 48)?;
        for i in 0..3 {
            let stride = word(&descriptors, i * 16 + 8) as i32;
            let height = word(&descriptors, i * 16 + 4) as i32;
            let data = word(&descriptors, i * 16 + 12).wrapping_add(((height - 1) * stride) as u32);
            descriptors[i * 16 + 8..i * 16 + 12].copy_from_slice(&(-stride).to_le_bytes());
            descriptors[i * 16 + 12..i * 16 + 16].copy_from_slice(&data.to_le_bytes());
        }
        let scratch = e
            .cpu
            .reg(Reg::Esp)
            .checked_sub(48)
            .ok_or("callback stack wraps")?
            & !15;
        e.cpu
            .write(scratch, &descriptors)
            .map_err(|e| e.to_string())?;
        return Ok(Outcome::call_on_stack(
            word(&stripe, 4),
            vec![word(&stripe, 0), scratch, first as u32, end as u32],
            scratch,
            move |e, _| match decode(e, index, address, op, target, Some((packet, granpos_out))) {
                Ok(outcome) => outcome,
                Err(error) => Outcome::Fail(format!("Theora continuation: {error}")),
            },
        ));
    }
    e.ext.get_mut::<Provider>().stats.native_frames += 1;
    packet_finished(e, address, packet.packetno);
    Ok(Outcome::Return(result as u32))
}

type TightPlanes = [(usize, usize, Vec<u8>); 3];

fn tight_planes(output: &Output<'_>) -> Result<TightPlanes, String> {
    let mut planes = output.dimensions().map(|(w, h)| (w, h, vec![0; w * h]));
    for (i, (width, _, bytes)) in planes.iter_mut().enumerate() {
        output
            .copy_plane(i, bytes, *width as i32)
            .map_err(str::to_owned)?;
    }
    Ok(planes)
}

fn compare_bytes(stats: &mut Stats, actual: &[u8], expected: &[u8]) {
    stats.compared_bytes += expected.len() as u64;
    stats.different_bytes += actual.len().abs_diff(expected.len()) as u64
        + actual.iter().zip(expected).filter(|(a, b)| a != b).count() as u64;
}

fn state_read(
    cpu: &dyn Guest,
    address: u32,
    layout: Layout,
) -> Result<(i64, i64, i64, u8), String> {
    let s = state(cpu, address, layout)?;
    Ok((s.keyframe_num, s.curframe_num, s.granpos, s.frame_type))
}

#[cfg(test)]
#[path = "../../tests/codec/theora.rs"]
mod tests;
