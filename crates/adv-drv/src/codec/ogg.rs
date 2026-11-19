//! Read-only prediction from a reviewed libogg packet queue and its file.
use super::{
    Entry,
    readahead::{self, Page, Tail},
};
use adv_abi::{code_signature::ExecutableRange, library_signature::Family};
use adv_cpu::backend::{Guest, GuestExt, Reg};
use adv_machine::{Conv, Env, Ext, HostFn, Outcome};
use adv_nt::file::{ReadSource, RecentReads};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        mpsc::{self, Receiver, TryRecvError},
    },
};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
    pages: BTreeMap<Source, PageState>,
}

#[derive(Default)]
struct PageState {
    tail: Option<Tail>,
    lookup: Option<Lookup>,
}

struct Lookup {
    page: Page,
    result: Option<Receiver<Option<ReadSource>>>,
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
    e.ext.get_or_default::<RecentReads>();
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

fn observe_page(e: &Env<'_>, page: u32) -> Option<(Page, Option<ReadSource>)> {
    let header = e.cpu.read_u32(page).ok()?;
    let header_len = e.cpu.read_u32(page.checked_add(4)?).ok()?;
    let body = e.cpu.read_u32(page.checked_add(8)?).ok()?;
    let len = header_len.checked_add(e.cpu.read_u32(page.checked_add(12)?).ok()?)?;
    if header_len < 27 {
        return None;
    }
    let mut fields = [0; 27];
    e.cpu.read(header, &mut fields).ok()?;
    let page = Page::new(&fields, len)?;
    let direct = (header.checked_add(header_len)? == body)
        .then(|| e.ext.get::<RecentReads>().direct(header, len))
        .flatten();
    Some((page, direct))
}

fn link_page(ext: &mut Ext, source: Source, page: Page, file: Option<ReadSource>) {
    let state = ext.get_mut::<Streams>().pages.entry(source).or_default();
    state.lookup = None;
    let continued = state.tail.as_ref().is_some_and(|tail| {
        file.as_ref().is_some_and(|file| {
            tail.serial == page.serial()
                && tail.sequence == page.sequence()
                && Arc::ptr_eq(&tail.source.file, &file.file)
                && tail.source.offset == file.offset
        })
    });
    let unlinked = state.tail.is_some() && !continued;
    let found = file.is_some();
    state.tail = file.map(|file| page.tail(file));
    if unlinked {
        super::theora::invalidate_sources(ext, |seen| seen == source);
    }
    let stats = super::theora::run_ahead_stats(ext);
    stats.unlinked_wrote += u64::from(unlinked || !found);
    stats.linked += u64::from(found && !continued);
}

/// Poll a copied-page lookup without waiting on the owner. File staging can
/// start as soon as the latest accepted page has a known file position.
pub(super) fn locate_page(ext: &mut Ext, source: Source) {
    let Some(lookup) = ext
        .get_mut::<Streams>()
        .pages
        .get_mut(&source)
        .and_then(|state| state.lookup.as_mut())
    else {
        return;
    };
    let page = lookup.page;
    if let Some(result) = &lookup.result {
        match result.try_recv() {
            Ok(file) => link_page(ext, source, page, file),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => link_page(ext, source, page, None),
        }
    } else if adv_device::codec_workers::available() {
        let (send, result) = mpsc::channel();
        lookup.result = Some(result);
        let ranges: Vec<_> = ext.get::<RecentReads>().ranges().collect();
        adv_device::codec_workers::submit_speculation(move || {
            let _ = send.send(page.locate(&ranges));
        });
    }
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
    let page = observe_page(e, e.arg(1));
    let paired = super::theora::paired(e.ext, source);
    if page.is_none() && paired.is_empty() {
        return super::original(e, original);
    }
    Outcome::call(original, vec![source.stream, e.arg(1)], move |e, result| {
        if result.value() == 0 {
            if !paired.is_empty() {
                locate_page(e.ext, source);
            }
            if let Some((page, file)) = page {
                if file.is_some() {
                    link_page(e.ext, source, page, file);
                } else {
                    e.ext
                        .get_mut::<Streams>()
                        .pages
                        .entry(source)
                        .or_default()
                        .lookup = Some(Lookup { page, result: None });
                    if !paired.is_empty() {
                        locate_page(e.ext, source);
                    }
                }
            }
            for address in paired {
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
    streams
        .pages
        .retain(|source, _| !retired.contains(&source.binding));
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
    let streams = ext.get_mut::<Streams>();
    streams.observed.retain(|_, (seen, _)| *seen != source);
    streams.pages.remove(&source);
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
) -> (Vec<Packet>, Option<readahead::Packets>) {
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
        return (Vec::new(), None);
    }
    let Some(mut cursor) = binding.layout.cursor(cpu, source.stream) else {
        return (Vec::new(), None);
    };
    if cursor.packetno != next {
        return (Vec::new(), None);
    }
    let packets: Vec<_> = (0..depth).map_while(|_| cursor.next(cpu)).collect();
    let tail = (cursor.lace == cursor.end)
        .then(|| {
            let page = ext.get::<Streams>().pages.get(&source)?;
            let tail = page
                .tail
                .as_ref()
                .filter(|tail| !tail.eos && page.lookup.is_none())?;
            let len = cursor.filled.checked_sub(cursor.returned)? as usize;
            let start = cursor.body.checked_add(cursor.returned)?;
            let mut partial = vec![0; len];
            cpu.read(start, &mut partial).ok()?;
            let bos = if len == 0 {
                0
            } else {
                cpu.read_u32(cursor.laces.checked_add(cursor.lace.checked_mul(4)?)?)
                    .ok()?
                    & 0x100
            };
            Some(readahead::Packets::new(
                tail.clone(),
                cursor.packetno,
                partial,
                bos,
            ))
        })
        .flatten();
    (packets, tail)
}

#[cfg(test)]
#[path = "../../tests/codec/ogg.rs"]
pub(super) mod tests;
