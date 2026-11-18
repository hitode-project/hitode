//! Read-only prediction from a reviewed libogg packet queue.
use super::Entry;
use adv_abi::{code_signature::ExecutableRange, library_signature::Family};
use adv_cpu::backend::{Guest, GuestExt, Reg};
use adv_machine::{Conv, Env, Ext, HostFn, Outcome};
use std::collections::BTreeMap;

const FAMILY: &str = include_str!("../../../../data/codec/ogg-msvc-x86.txt");
const MODULE: &str = "adv-ogg";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Packet {
    pub bytes: Vec<u8>,
    pub bos: u32,
    pub eos: u32,
    pub granulepos: i64,
    pub packetno: i64,
}

impl Packet {
    pub(super) fn read(cpu: &dyn Guest, address: u32) -> Result<Self, String> {
        let mut raw = [0; 32];
        cpu.read(address, &mut raw).map_err(|err| err.to_string())?;
        let word = |offset| u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap());
        let wide = |offset| i64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap());
        let length = usize::try_from(word(4) as i32).map_err(|_| "negative packet length")?;
        let mut bytes = vec![0; length];
        cpu.read(word(0), &mut bytes)
            .map_err(|err| err.to_string())?;
        Ok(Self {
            bytes,
            bos: word(8),
            eos: word(12),
            granulepos: wide(16),
            packetno: wide(24),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Source {
    binding: usize,
    stream: u32,
}

#[derive(Clone, Copy)]
struct Layout {
    body_data: u32,
    body_fill: u32,
    body_returned: u32,
    lacing_vals: u32,
    granule_vals: u32,
    lacing_packet: u32,
    lacing_returned: u32,
    packetno: u32,
}

impl Layout {
    fn from_fields(fields: &BTreeMap<String, u32>) -> Result<Self, String> {
        let field = |key: &str| {
            fields
                .get(key)
                .copied()
                .ok_or_else(|| format!("missing Ogg field {key}"))
        };
        Ok(Self {
            body_data: field("body_data")?,
            body_fill: field("body_fill")?,
            body_returned: field("body_returned")?,
            lacing_vals: field("lacing_vals")?,
            granule_vals: field("granule_vals")?,
            lacing_packet: field("lacing_packet")?,
            lacing_returned: field("lacing_returned")?,
            packetno: field("packetno")?,
        })
    }

    fn cursor(self, cpu: &dyn Guest, stream: u32) -> Option<Cursor> {
        let word = |offset| cpu.read_u32(stream.checked_add(offset)?).ok();
        let cursor = Cursor {
            body: word(self.body_data)?,
            filled: word(self.body_fill)?,
            returned: word(self.body_returned)?,
            laces: word(self.lacing_vals)?,
            granules: word(self.granule_vals)?,
            end: word(self.lacing_packet)?,
            lace: word(self.lacing_returned)?,
            packetno: read_i64(cpu, stream.checked_add(self.packetno)?)?,
        };
        (cursor.body != 0 && cursor.laces != 0 && cursor.granules != 0).then_some(cursor)
    }
}

struct Cursor {
    body: u32,
    filled: u32,
    returned: u32,
    laces: u32,
    granules: u32,
    end: u32,
    lace: u32,
    packetno: i64,
}

impl Cursor {
    fn next(&mut self, cpu: &dyn Guest) -> Option<Packet> {
        let mut length = 0u32;
        let mut lace = self.lace;
        let mut flags = 0;
        loop {
            if lace >= self.end {
                return None;
            }
            let val = cpu
                .read_u32(self.laces.checked_add(lace.checked_mul(4)?)?)
                .ok()?;
            if val & 0x400 != 0 {
                return None;
            }
            if lace == self.lace {
                flags = val & 0x100;
            }
            flags |= val & 0x200;
            let segment = val & 0xff;
            length = length.checked_add(segment)?;
            if segment != 255 {
                break;
            }
            lace += 1;
        }
        let end = self.returned.checked_add(length)?;
        if end > self.filled || length > i32::MAX as u32 {
            return None;
        }
        let data = self.body.checked_add(self.returned)?;
        let granulepos = read_i64(cpu, self.granules.checked_add(lace.checked_mul(8)?)?)?;
        let mut bytes = vec![0; length as usize];
        cpu.read(data, &mut bytes).ok()?;
        let packet = Packet {
            bytes,
            bos: flags & 0x100,
            eos: flags & 0x200,
            granulepos,
            packetno: self.packetno,
        };
        self.returned = end;
        self.lace = lace + 1;
        self.packetno = self.packetno.wrapping_add(1);
        Some(packet)
    }
}

fn read_i64(cpu: &dyn Guest, address: u32) -> Option<i64> {
    let mut bytes = [0; 8];
    cpu.read(address, &mut bytes).ok()?;
    Some(i64::from_le_bytes(bytes))
}

struct Binding {
    live: bool,
    layout: Layout,
    packet: Entry,
    reset: Entry,
    pagein: Entry,
    snapshots: Vec<(u32, Vec<u8>)>,
}

#[derive(Default)]
struct Streams {
    bindings: Vec<Binding>,
    // A packet object may be filled on one guest thread and consumed on another.
    observed: BTreeMap<u32, (Source, bool)>,
}

pub(super) fn install(ext: &mut Ext) {
    ext.insert(Streams::default());
}

pub(super) fn host_fn(module: &str, name: &str) -> Option<HostFn> {
    if module != MODULE {
        return None;
    }
    let (name, index) = name.split_once('_')?;
    let (handler, argc) = match name {
        "packet" => (packet as adv_machine::Handler, 1),
        "reset" => (reset as adv_machine::Handler, 1),
        "pagein" => (pagein as adv_machine::Handler, 2),
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
    let Some(library) = family.scan(ranges, &mut |a, n| {
        let mut bytes = vec![0; n];
        e.cpu.read(a, &mut bytes).ok()?;
        Some(bytes)
    }) else {
        return Ok(());
    };
    let layout = Layout::from_fields(&library.layout)?;
    let index = e.ext.get::<Streams>().bindings.len();
    let mut entry = |name: &str| {
        let (address, prefix) = library.hooks[name];
        let bytes = library
            .snapshots
            .iter()
            .find(|(a, _)| *a == address)
            .unwrap()
            .1
            .as_slice();
        super::entry(
            e.cpu,
            e.sys,
            MODULE,
            &format!("{name}_{index}"),
            address,
            prefix,
            bytes,
        )
        .map(|entry| (entry, bytes))
    };
    let (packet, packet_bytes) = entry("packet")?;
    let (reset, reset_bytes) = entry("reset")?;
    let (pagein, pagein_bytes) = entry("pagein")?;
    super::redirect(e.sys, &packet, packet_bytes);
    super::redirect(e.sys, &reset, reset_bytes);
    super::redirect(e.sys, &pagein, pagein_bytes);
    e.ext.get_mut::<Streams>().bindings.push(Binding {
        live: true,
        layout,
        packet,
        reset,
        pagein,
        snapshots: library.snapshots,
    });
    Ok(())
}

fn pagein(e: &mut Env<'_>) -> Outcome {
    let index = e.sys.thunks.get(e.thunk).f.data as usize;
    let binding = &e.ext.get::<Streams>().bindings[index];
    if !binding.live {
        return Outcome::Fail("Ogg binding image was retired".into());
    }
    let source = Source {
        binding: index,
        stream: e.arg(0),
    };
    let original = binding.pagein.original;
    let waiting = super::theora::waiting_for_page(e.ext, source);
    if waiting.is_empty() {
        return super::original(e, original);
    }
    Outcome::call(original, vec![source.stream, e.arg(1)], move |e, result| {
        if result.value() == 0 {
            for address in waiting {
                super::theora::page_buffered(e, address, source);
            }
        }
        Outcome::Return(result.value())
    })
}

pub(super) fn forget(ext: &mut Ext, base: u32, size: u32) {
    let streams = ext.get_mut::<Streams>();
    let mut retired = Vec::new();
    for (index, binding) in streams.bindings.iter_mut().enumerate() {
        if binding.packet.entry >= base && binding.packet.entry - base < size {
            binding.live = false;
            retired.push(index);
        }
    }
    streams
        .observed
        .retain(|_, (source, _)| streams.bindings[source.binding].live);
    super::theora::invalidate_sources(ext, |source| retired.contains(&source.binding));
}

fn packet(e: &mut Env<'_>) -> Outcome {
    let index = e.sys.thunks.get(e.thunk).f.data as usize;
    let binding = &e.ext.get::<Streams>().bindings[index];
    if !binding.live {
        return Outcome::Fail("Ogg binding image was retired".into());
    }
    let source = Source {
        binding: index,
        stream: e.cpu.reg(Reg::Ecx),
    };
    let output = e.cpu.reg(Reg::Edx);
    let advance = e.arg(0);
    Outcome::call(binding.packet.original, vec![advance], move |e, result| {
        let value = result.value();
        if value == 1 && output != 0 {
            e.ext
                .get_mut::<Streams>()
                .observed
                .insert(output, (source, advance != 0));
        } else if value == (-1i32) as u32 {
            invalidate(e.ext, source);
        }
        Outcome::Return(value)
    })
}

fn reset(e: &mut Env<'_>) -> Outcome {
    let index = e.sys.thunks.get(e.thunk).f.data as usize;
    let binding = &e.ext.get::<Streams>().bindings[index];
    if !binding.live {
        return Outcome::Fail("Ogg binding image was retired".into());
    }
    let original = binding.reset.original;
    invalidate(
        e.ext,
        Source {
            binding: index,
            stream: e.arg(0),
        },
    );
    super::original(e, original)
}

fn invalidate(ext: &mut Ext, source: Source) {
    ext.get_mut::<Streams>()
        .observed
        .retain(|_, (seen, _)| *seen != source);
    super::theora::invalidate_sources(ext, |seen| seen == source);
}

pub(super) fn take(ext: &mut Ext, output: u32) -> Option<(Source, bool)> {
    ext.get_mut::<Streams>().observed.remove(&output)
}

pub(super) fn predict(
    cpu: &dyn Guest,
    ext: &Ext,
    source: Source,
    next: i64,
    depth: usize,
) -> Vec<Packet> {
    let binding = &ext.get::<Streams>().bindings[source.binding];
    if !binding.live
        || !binding.snapshots.iter().all(|(a, expected)| {
            cpu.shared_bytes(*a, expected.len()).map_or_else(
                || {
                    let mut bytes = vec![0; expected.len()];
                    cpu.read(*a, &mut bytes).is_ok() && bytes == *expected
                },
                |bytes| bytes == expected,
            )
        })
    {
        return Vec::new();
    }
    let Some(mut cursor) = binding.layout.cursor(cpu, source.stream) else {
        return Vec::new();
    };
    if cursor.packetno != next {
        return Vec::new();
    }
    (0..depth).map_while(|_| cursor.next(cpu)).collect()
}

#[cfg(test)]
#[path = "../../tests/codec/ogg.rs"]
mod tests;
