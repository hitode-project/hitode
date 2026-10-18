//! Whole-packet execution for admitted static Theora decoder ABIs.
use crate::codec_kernels::Mode;
use adv_abi::{Registry, code_signature::ExecutableRange, library_signature::Family};
use adv_cpu::backend::{Backend, Guest, GuestExt, Reg, asm::Asm};
use adv_device::theora::{Decoder, Setup, State};
use adv_machine::{Conv, Env, Machine, Outcome, layout::thunk_addr, patch::Detour};
use adv_os::ModuleKind;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

const FAMILY: &str = include_str!("../../../data/codec/theora-msvc-x86.txt");
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
    allocator: u32,
    init: Detour,
    packet: Detour,
    clear: Detour,
    snapshots: Vec<(u32, Vec<u8>)>,
}
struct Instance {
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

pub fn stats<B: Backend>(machine: &Machine<B>) -> Stats {
    machine
        .ext
        .try_get::<Provider>()
        .map_or_else(Stats::default, |p| p.stats.clone())
}

pub fn install<B: Backend>(machine: &mut Machine<B>, mode: Mode) -> Result<u64, String> {
    let family = Family::parse(FAMILY)?;
    if machine.sys.thunks.resolver_mut::<Registry>().is_none() {
        return Err("missing ABI registry".into());
    }
    let images: Vec<_> = adv_os::modules(&machine.ext)
        .iter()
        .filter(|(_, m)| m.kind == ModuleKind::Image && m.lifetime.mapped)
        .map(|(_, m)| (m.base, m.size))
        .collect();
    let mut provider = Provider {
        mode,
        bindings: Vec::new(),
        decoders: BTreeMap::new(),
        stats: Stats::default(),
    };
    'images: for (base, size) in images {
        let mut header = vec![0; (size as usize).min(65536)];
        if machine.cpu.read(base, &mut header).is_err() {
            continue;
        }
        let Ok(pe) = adv_pe::Pe::parse_headers(&header) else {
            continue;
        };
        let ranges: Vec<_> = pe
            .sections
            .iter()
            .filter(|s| s.characteristics & adv_pe::scn::MEM_EXECUTE != 0)
            .filter_map(|s| {
                Some(ExecutableRange {
                    start: base.checked_add(s.virtual_address)?,
                    len: s
                        .virtual_size
                        .max(s.raw_size)
                        .min(size.checked_sub(s.virtual_address)?),
                })
            })
            .collect();
        let Some(mut library) = family.scan(&ranges, &mut |a, n| {
            let mut b = vec![0; n];
            machine.cpu.read(a, &mut b).ok()?;
            Some(b)
        }) else {
            continue;
        };
        let index = provider.bindings.len();
        let mut hooks = Vec::new();
        let mut displaced: Vec<(u32, Vec<u8>)> = Vec::new();
        for (name, handler, argc) in [
            ("init", init as adv_machine::Handler, 1),
            ("packet", packet as adv_machine::Handler, 1),
            ("clear", clear as adv_machine::Handler, 0),
        ] {
            let unique = format!("{name}_{index}");
            machine
                .sys
                .thunks
                .resolver_mut::<Registry>()
                .expect("checked ABI registry")
                .add_with_data(MODULE, &unique, Conv::Cdecl { argc }, handler, index as u64);
            let &(address, prefix) = library.hooks.get(name).expect("reviewed Theora hook");
            let original = library
                .snapshots
                .iter()
                .find(|(a, _)| *a == address)
                .expect("verified hook body")
                .1[..prefix]
                .to_vec();
            let detour = match machine.detour(address, prefix as u32, MODULE, &unique) {
                Ok(detour) => detour,
                Err(error) => {
                    // Installation precedes execution. A failed allocation must
                    // leave all three library entry points on the guest path.
                    for (entry, bytes) in &displaced {
                        machine
                            .cpu
                            .write(*entry, bytes)
                            .expect("restore owned detour prefix");
                        machine.cpu.invalidate_code(*entry, bytes.len() as u32);
                    }
                    provider.stats.reason = Some(error.to_string());
                    break 'images;
                }
            };
            displaced.push((address, original));
            let mut installed = Asm::new(address);
            installed.jmp(thunk_addr(detour.thunk));
            installed.bytes.resize(prefix, 0xcc);
            for (a, bytes) in &mut library.snapshots {
                if *a == address {
                    bytes[..prefix].copy_from_slice(&installed.bytes);
                }
            }
            hooks.push(detour);
        }
        provider.bindings.push(Binding {
            allocator: library.symbols["malloc"],
            init: hooks[0],
            packet: hooks[1],
            clear: hooks[2],
            snapshots: library.snapshots,
        });
    }
    provider.stats.installed = provider.bindings.len() as u64;
    let count = provider.stats.installed;
    machine.ext.insert(provider);
    Ok(count)
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
    let result = decoder.decode(
        &input,
        wide(&packet, 24),
        wide(&packet, 16),
        word(&packet, 12) != 0,
        word(&stripe, 4) != 0 && provider.mode == Mode::Native,
        &mut state,
    );
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
mod tests {
    use super::*;
    use adv_cpu::{JitCpu, backend::Prot};
    use adv_machine::{
        Clock, RunResult,
        layout::{PAGE, PARK_ADDR},
    };
    const CODE: u32 = 0x0040_0000;
    const CONTEXT: u32 = 0x0041_0000;
    const PACKET: u32 = CONTEXT + 0xf000;
    const GRANULE: u32 = PACKET + 32;
    const SLOT: u32 = PACKET + 40;
    const RESULT: u32 = PACKET + 48;
    const STORAGE: u32 = CONTEXT + 0x10000;
    struct Allocator {
        fail: bool,
        allocations: u32,
        frees: u32,
    }
    fn create(e: &mut Env<'_>) -> Outcome {
        let mut info = [0u8; 64];
        info[..3].copy_from_slice(&[3, 2, 1]);
        for (at, value) in [
            (4, 16u32),
            (8, 16),
            (12, 16),
            (16, 16),
            (28, 30),
            (32, 1),
            (60, 6),
        ] {
            info[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let setup = Setup {
            info,
            huffman: std::array::from_fn(|_| vec![1, -271, -312]),
            dc_scale: [100; 64],
            ac_scale: [100; 64],
            loop_filter_limits: [16; 64],
            ranges: std::array::from_fn(|_| (vec![63], vec![16; 128])),
        };
        native_init(e, 0, CONTEXT, Decoder::new(&setup).unwrap())
    }
    // spec: DEV-VIDEO-1
    #[test]
    fn native_lifetime_uses_guest_allocator_and_preserves_first_duplicate_frame() {
        for fail in [false, true] {
            let mut registry = Registry::default();
            registry.add(MODULE, "create", Conv::Cdecl { argc: 0 }, create);
            registry.add_with_data(MODULE, "packet", Conv::Cdecl { argc: 1 }, packet, 0);
            registry.add_with_data(MODULE, "clear", Conv::Cdecl { argc: 0 }, clear, 0);
            registry.add(MODULE, "malloc", Conv::Cdecl { argc: 1 }, |e| {
                assert_eq!(e.cpu.arg(0).unwrap(), 48 * 48 + 2 * 24 * 24);
                let allocator = e.ext.get_mut::<Allocator>();
                allocator.allocations += 1;
                Outcome::Return(if allocator.fail { 0 } else { STORAGE })
            });
            registry.add(MODULE, "free", Conv::Cdecl { argc: 1 }, |e| {
                assert_eq!(e.cpu.arg(0).unwrap(), STORAGE);
                e.ext.get_mut::<Allocator>().frees += 1;
                Outcome::Return(0)
            });
            registry.add(MODULE, "done", Conv::Cdecl { argc: 0 }, |_| {
                Outcome::ExitProcess(0)
            });
            let mut machine = Machine::new(
                JitCpu::new(PARK_ADDR, false).unwrap(),
                registry,
                Clock::virtual_time(),
            )
            .unwrap();
            machine.cpu.map(CODE, PAGE, Prot::RX).unwrap();
            machine.cpu.map(CONTEXT, PAGE * 32, Prot::RW).unwrap();
            let ctor = machine.resolve_import(MODULE, "create").unwrap();
            let packet = machine.resolve_import(MODULE, "packet").unwrap();
            let clear = machine.resolve_import(MODULE, "clear").unwrap();
            let malloc = machine.resolve_import(MODULE, "malloc").unwrap();
            let free = machine.resolve_import(MODULE, "free").unwrap();
            let done = machine.resolve_import(MODULE, "done").unwrap();
            machine.cpu.write_u32(SLOT, malloc).unwrap();
            let mut original_clear = Asm::new(CODE + 0x300);
            original_clear
                .mov_eax_mem(CONTEXT + 588)
                .push_eax()
                .call(free)
                .add_esp(4)
                .ret();
            machine
                .cpu
                .write(CODE + 0x300, &original_clear.bytes)
                .unwrap();
            let detour = Detour {
                entry: 0,
                original: CODE + 0x300,
                thunk: 0,
            };
            machine.ext.insert(Provider {
                mode: Mode::Native,
                bindings: vec![Binding {
                    allocator: SLOT,
                    init: detour,
                    packet: detour,
                    clear: detour,
                    snapshots: Vec::new(),
                }],
                decoders: BTreeMap::new(),
                stats: Stats::default(),
            });
            machine.ext.insert(Allocator {
                fail,
                allocations: 0,
                frees: 0,
            });
            let mut caller = Asm::new(CODE);
            caller
                .call(ctor)
                .mov_mem_eax(RESULT)
                .cmp_mem_imm8(RESULT, 0)
                .jnz(done);
            caller
                .mov_mem_imm(CONTEXT + CURRENT, 0)
                .mov_mem_imm(CONTEXT + CURRENT + 4, 0)
                .mov_ecx_imm(CONTEXT);
            caller.bytes.push(0xba);
            caller.bytes.extend_from_slice(&PACKET.to_le_bytes());
            caller
                .push_imm(GRANULE)
                .call(packet)
                .add_esp(4)
                .mov_mem_eax(RESULT + 4);
            caller.mov_ecx_imm(CONTEXT).call(clear).call(done);
            machine.cpu.write(CODE, &caller.bytes).unwrap();
            machine
                .spawn("authored Theora ABI fixture", CODE, &[], 0x10000)
                .unwrap();
            assert!(matches!(
                machine.run_virtual(1_000_000),
                RunResult::Exited(0)
            ));
            assert_eq!(machine.ext.get::<Allocator>().allocations, 1);
            assert_eq!(machine.ext.get::<Allocator>().frees, u32::from(!fail));
            assert!(machine.ext.get::<Provider>().decoders.is_empty());
            assert_eq!(
                machine.cpu.read_u32(RESULT).unwrap(),
                if fail { u32::MAX } else { 0 }
            );
            if fail {
                continue;
            }
            assert_eq!(machine.cpu.read_u32(RESULT + 4).unwrap(), 1);
            assert_eq!(wide(&read(&machine.cpu, GRANULE, 8).unwrap(), 0), 64);
            let descriptors = read(&machine.cpu, CONTEXT + OUTPUT, 48).unwrap();
            for plane in 0..3 {
                let width = word(&descriptors, plane * 16);
                let height = word(&descriptors, plane * 16 + 4);
                let stride = word(&descriptors, plane * 16 + 8) as i32;
                let data = word(&descriptors, plane * 16 + 12);
                for row in 0..height {
                    assert!(
                        read(
                            &machine.cpu,
                            data.wrapping_add((row as i32 * stride) as u32),
                            width as usize
                        )
                        .unwrap()
                        .iter()
                        .all(|&b| b == 128)
                    );
                }
            }
        }
    }
}
