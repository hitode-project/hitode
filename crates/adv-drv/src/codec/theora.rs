//! Whole-packet execution for the reviewed static Theora decoder ABI.
use super::{Entry, Mode};
use adv_abi::{code_signature::ExecutableRange, library_signature::Family};
use adv_cpu::backend::{Guest, GuestExt, Reg};
use adv_device::theora::{Decoder, Setup, State};
use adv_machine::{Conv, Env, Ext, HostFn, Outcome};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

const FAMILY: &str = include_str!("../../../../data/codec/theora-msvc-x86.txt");
const MODULE: &str = "adv-theora";
// Reviewed Xiph x86/MMX ABI: reference indices precede the plane buffers.
const INFO: usize = 64;
const REF_PLANES: u32 = 300;
const KEYFRAME: u32 = 888;
const CURRENT: u32 = 896;
const GRANPOS: u32 = 904;
const FRAME_TYPE: u32 = 912;
const PP_LEVEL: u32 = 53572;
const OUTPUT: u32 = 54104;
const STRIPE: u32 = 54152;

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
    pub reason: Option<String>,
}
struct Binding {
    live: bool,
    allocator: u32,
    init: Entry,
    packet: Entry,
    clear: Entry,
    snapshots: Vec<(u32, Vec<u8>)>,
}
struct Instance {
    binding: usize,
    decoder: Decoder,
    uploads: [Vec<u8>; 3],
    published: bool,
}
struct Provider {
    mode: Mode,
    bindings: Vec<Binding>,
    decoders: BTreeMap<u32, Instance>,
    stats: Stats,
}

pub(super) fn stats(ext: &Ext) -> Stats {
    ext.try_get::<Provider>()
        .map_or_else(Stats::default, |p| p.stats.clone())
}

pub(super) fn install(ext: &mut Ext, mode: Mode) {
    ext.insert(Provider {
        mode,
        bindings: Vec::new(),
        decoders: BTreeMap::new(),
        stats: Stats::default(),
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
    let index = e.ext.get::<Provider>().bindings.len();
    let mut hooks = Vec::new();
    for name in ["init", "packet", "clear"] {
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
    // Publish all three redirects together, after every trampoline exists.
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
        live: true,
        allocator: library.symbols["malloc"],
        init: hooks.next().unwrap(),
        packet: hooks.next().unwrap(),
        clear: hooks.next().unwrap(),
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
    provider
        .decoders
        .retain(|_, instance| provider.bindings[instance.binding].live);
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
    provider.decoders.remove(&address);
    original(e, e.ext.get::<Provider>().bindings[index].clear.original)
}
fn read(cpu: &dyn Guest, address: u32, len: usize) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0; len];
    cpu.read(address, &mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}
fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn wide(bytes: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}
fn setup(cpu: &dyn Guest, info: u32, address: u32) -> Result<Setup, String> {
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
        info: read(cpu, info, INFO)?.try_into().unwrap(),
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
    provider.decoders.remove(&address);
    let target = provider.bindings[index].init.original;
    let native = setup(e.cpu, info, tables).and_then(|s| Decoder::new(&s).map_err(str::to_owned));
    if e.ext.get::<Provider>().mode == Mode::Native {
        return match native {
            Ok(native) => native_init(e, index, address, native),
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
                Ok(native) => {
                    provider.decoders.insert(
                        address,
                        Instance {
                            binding: index,
                            decoder: native,
                            uploads: std::array::from_fn(|_| Vec::new()),
                            published: false,
                        },
                    );
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
    let slot = e.ext.get::<Provider>().bindings[index].allocator;
    let allocator = match read(e.cpu, slot, 4) {
        Ok(value) => word(&value, 0),
        Err(error) => return Outcome::Fail(error),
    };
    Outcome::call(allocator, vec![size], move |e, result| {
        let allocation = result.value();
        if allocation == 0 {
            return Outcome::Return((-1i32) as u32);
        }
        let mut header = vec![0u8; (STRIPE + 8) as usize];
        header[..64].copy_from_slice(&info);
        header[CURRENT as usize..CURRENT as usize + 8].copy_from_slice(&(-1i64).to_le_bytes());
        header[FRAME_TYPE as usize] = 255;
        header[FRAME_TYPE as usize + 1] = bias;
        // The reviewed clear routine frees this allocation once; all other
        // guest-private allocations and Huffman pointers are null.
        header[588..592].copy_from_slice(&allocation.to_le_bytes());
        header[276..300].fill(255);
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
            let at = REF_PLANES as usize + plane * 16;
            for (offset, value) in [(0, w), (4, h), (8, (-(stride as i32)) as u32), (12, data)] {
                header[at + offset..at + offset + 4].copy_from_slice(&value.to_le_bytes());
            }
            base += bytes;
        }
        if let Err(error) = e.cpu.write(address, &header) {
            return Outcome::Fail(error.to_string());
        }
        let provider = e.ext.get_mut::<Provider>();
        provider.decoders.insert(
            address,
            Instance {
                binding: index,
                decoder: native,
                uploads: std::array::from_fn(|_| Vec::new()),
                published: false,
            },
        );
        provider.stats.initialized += 1;
        Outcome::Return(0)
    })
}

fn state(cpu: &dyn Guest, address: u32) -> Result<State, String> {
    let bytes = read(cpu, address + KEYFRAME, 25)?;
    Ok(State {
        keyframe_num: wide(&bytes, 0),
        curframe_num: wide(&bytes, 8),
        granpos: wide(&bytes, 16),
        frame_type: bytes[24],
        pp_level: word(&read(cpu, address + PP_LEVEL, 4)?, 0) as i32,
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
fn decode(
    e: &mut Env<'_>,
    index: usize,
    address: u32,
    op: u32,
    target: u32,
    saved: Option<(Vec<u8>, Vec<u8>, u32)>,
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
    let (packet, input, granpos_out) = if let Some(saved) = saved {
        saved
    } else {
        let packet = read(e.cpu, op, 32)?;
        let length = word(&packet, 4) as i32;
        if length < 0 {
            return Err("negative packet length".into());
        }
        let input = read(e.cpu, word(&packet, 0), length as usize)?;
        let granpos_out = e.cpu.arg(0).map_err(|e| e.to_string())?;
        (packet, input, granpos_out)
    };
    let mut state = state(e.cpu, address)?;
    let stripe = read(e.cpu, address + STRIPE, 8)?;
    let started = Instant::now();
    let provider = e.ext.get_mut::<Provider>();
    let instance = provider.decoders.get_mut(&address).unwrap();
    let decoder = &mut instance.decoder;
    // Stripe decoding stays on the execution owner; whole-frame device
    // dispatch waits for plane workers. Only guest callbacks yield here.
    let result = loop {
        let result = decoder.decode(
            &input,
            wide(&packet, 24),
            wide(&packet, 16),
            word(&packet, 12) != 0,
            true,
            &mut state,
        );
        if result != 2 || (provider.mode == Mode::Native && word(&stripe, 4) != 0) {
            break result;
        }
    };
    provider.stats.decode_ns += started.elapsed().as_nanos() as u64;
    provider.stats.frames += u64::from(result != 2);
    let mode = provider.mode;
    // Take owned output for the guest continuation. The native decoder retains
    // its reference frames across calls, independently of guest allocations.
    let rows = (result == 2).then(|| decoder.stripe());
    let planes = if result >= 0 && mode == Mode::Verify {
        decoder.planes().map(|p| {
            p.iter()
                .map(|p| (p.width, p.height, p.data.clone()))
                .collect::<Vec<_>>()
        })
    } else {
        None
    };
    if mode == Mode::Verify {
        return Ok(Outcome::call(
            target,
            vec![granpos_out],
            move |e, resume| {
                let comparison = (|| -> Result<(), String> {
                    let actual = state_read(e.cpu, address)?;
                    let provider = e.ext.get_mut::<Provider>();
                    provider.stats.state_mismatches += u64::from(
                        resume.value() != result as u32
                            || actual
                                != (
                                    state.keyframe_num,
                                    state.curframe_num,
                                    state.granpos,
                                    state.frame_type,
                                ),
                    );
                    if let Some(planes) = planes {
                        let descriptors = read(e.cpu, address + OUTPUT, 48)?;
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
                                s.compared_bytes += width as u64;
                                s.different_bytes += actual
                                    .iter()
                                    .zip(&bytes[y * width..(y + 1) * width])
                                    .filter(|(a, b)| a != b)
                                    .count()
                                    as u64;
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = comparison {
                    e.ext.get_mut::<Provider>().stats.reason = Some(error);
                }
                Outcome::Return(resume.value())
            },
        ));
    }
    if result >= 0 && (result != 1 || !instance.published) {
        let descriptors = read(e.cpu, address + REF_PLANES, 48)?;
        let dimensions = decoder.dimensions().ok_or("native frame unavailable")?;
        for (i, (width, height)) in dimensions.into_iter().enumerate() {
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
            let valid = decoder
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
            .write(address + OUTPUT, &descriptors)
            .map_err(|e| e.to_string())?;
        instance.published = true;
    }
    for (offset, value) in [
        (KEYFRAME, state.keyframe_num),
        (CURRENT, state.curframe_num),
        (GRANPOS, state.granpos),
    ] {
        e.cpu
            .write(address + offset, &value.to_le_bytes())
            .map_err(|e| e.to_string())?;
    }
    e.cpu
        .write(address + FRAME_TYPE, &[state.frame_type])
        .map_err(|e| e.to_string())?;
    if result >= 0 && granpos_out != 0 {
        e.cpu
            .write(granpos_out, &state.granpos.to_le_bytes())
            .map_err(|e| e.to_string())?;
    }
    if let Some((first, end)) = rows {
        let mut descriptors = read(e.cpu, address + OUTPUT, 48)?;
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
            move |e, _| match decode(
                e,
                index,
                address,
                op,
                target,
                Some((packet, input, granpos_out)),
            ) {
                Ok(outcome) => outcome,
                Err(error) => Outcome::Fail(format!("Theora continuation: {error}")),
            },
        ));
    }
    e.ext.get_mut::<Provider>().stats.native_frames += 1;
    Ok(Outcome::Return(result as u32))
}
fn state_read(cpu: &dyn Guest, address: u32) -> Result<(i64, i64, i64, u8), String> {
    let s = state(cpu, address)?;
    Ok((s.keyframe_num, s.curframe_num, s.granpos, s.frame_type))
}

#[cfg(test)]
#[path = "../../tests/codec/theora.rs"]
mod tests;
