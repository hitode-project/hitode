//! Wine 11 code page loops, using its loaded CPTABLEINFO and normalization data.

use adv_cpu::backend::{Guest, Prot};
use adv_machine::Env;
use std::{mem::MaybeUninit, rc::Rc};

// Offsets and RVAs checked against the pinned PE modules' DWARF.
#[allow(non_upper_case_globals)]
mod cp {
    pub const SIZE: u32 = 0x2c;
    pub const CodePage: u32 = 0;
    pub const DefaultChar: u32 = 4;
    pub const UniDefaultChar: u32 = 6;
    pub const TransUniDefaultChar: u32 = 10;
    pub const DBCSCodePage: u32 = 12;
    pub const MultiByteTable: u32 = 28;
    pub const WideCharTable: u32 = 32;
    pub const DBCSOffsets: u32 = 40;
}
#[allow(non_upper_case_globals)]
mod norm {
    pub const decomp_size: u32 = 0x2e;
    pub const comp_size: u32 = 0x30;
    pub const props_level1: u32 = 0x36;
    pub const props_level2: u32 = 0x38;
    pub const decomp_hash: u32 = 0x3a;
    pub const decomp_map: u32 = 0x3c;
    pub const decomp_seq: u32 = 0x3e;
    pub const comp_hash: u32 = 0x40;
    pub const comp_seq: u32 = 0x42;
}
#[allow(non_upper_case_globals)]
mod kb {
    pub const locale_section: u32 = 0x871a0;
    pub const ansi_cpinfo: u32 = 0xc0b00;
    pub const oem_cpinfo: u32 = 0xc0ac0;
    pub const codepages: u32 = 0xbf480;
    pub const nb_codepages: u32 = 0xbf460;
    pub const norm_info: u32 = 0xbf45c;
    pub const unix_cp: u32 = 0x871b8;
}
#[allow(non_upper_case_globals)]
mod critical {
    pub const LockCount: u32 = 4;
}
const NLS_INFO: u32 = 0x723e0;
const ANSI_TABLE: u32 = 0x2c;
const LAST_ERROR: u32 = 0x34;

// Short conversions stay inline; the single variable output spills into reusable scratch.
struct Buffer {
    stack: [u8; 256],
    len: usize,
    heap: usize,
}
impl Buffer {
    fn new() -> Self {
        Self {
            stack: [0; 256],
            len: 0,
            heap: usize::MAX,
        }
    }
    fn extend_from_slice(&mut self, data: &[u8], heap: &mut Vec<u8>) {
        if self.len + data.len() <= self.stack.len() {
            self.stack[self.len..self.len + data.len()].copy_from_slice(data);
        } else {
            if self.heap == usize::MAX {
                self.heap = heap.len();
                heap.extend_from_slice(&self.stack[..self.len]);
            }
            heap.extend_from_slice(data);
        }
        self.len += data.len();
    }
    fn push(&mut self, ch: u8, heap: &mut Vec<u8>) {
        self.extend_from_slice(&[ch], heap);
    }
    fn bytes<'a>(&'a self, heap: &'a [u8]) -> &'a [u8] {
        if self.len <= self.stack.len() {
            &self.stack[..self.len]
        } else {
            &heap[self.heap..self.heap + self.len]
        }
    }
}

// Compute first, then check every store. At most three stores and twenty read
// ranges cover the source, metadata, conversion and normalization tables.
struct Writes {
    entries: [Option<(u32, Buffer)>; 3],
    len: usize,
}
struct Memory<'a> {
    cpu: &'a dyn Guest,
    memo: &'a mut super::Memos,
    heap: &'a mut Vec<u8>,
    scratch: &'a mut [MaybeUninit<u8>],
    copies: &'a mut Vec<Vec<u8>>,
    reads: [(u32, u32); 20],
    read_count: usize,
    writes: Writes,
}
impl<'a> Memory<'a> {
    fn new(
        cpu: &'a dyn Guest,
        scratch: &'a mut [MaybeUninit<u8>],
        copies: &'a mut Vec<Vec<u8>>,
        memo: &'a mut super::Memos,
        heap: &'a mut Vec<u8>,
    ) -> Self {
        Self {
            cpu,
            memo,
            heap,
            scratch,
            copies,
            reads: [(0, 0); 20],
            read_count: 0,
            writes: Writes {
                entries: [None, None, None],
                len: 0,
            },
        }
    }
    fn track(&mut self, at: u32, len: u32) -> Option<()> {
        at.checked_add(len)?;
        self.reads[self.read_count] = (at, len);
        self.read_count += 1;
        Some(())
    }
    fn read(&mut self, at: u32, len: u32) -> Option<&'a [u8]> {
        if len == 0 {
            return Some(&[]);
        }
        self.track(at, len)?;
        self.bytes(at, len)
    }
    fn bytes(&mut self, at: u32, len: u32) -> Option<&'a [u8]> {
        if !self.cpu.accessible(at, len, Prot::READ) {
            return None;
        }
        if let Some(bytes) = self.cpu.shared_bytes(at, len as usize) {
            return Some(bytes);
        }
        // Shared file mappings cannot lend ordinary references. Copy each
        // table once into stack storage, initializing only the span it uses.
        if len as usize > self.scratch.len() {
            let mut bytes = vec![0; len as usize];
            self.cpu.read(at, &mut bytes).ok()?;
            let ptr = bytes.as_ptr();
            self.copies.push(bytes);
            // SAFETY: copies is borrowed for 'a from the caller. Its buffers
            // stay allocated and immutable while later copies are appended.
            return Some(unsafe { std::slice::from_raw_parts(ptr, len as usize) });
        }
        let (bytes, rest) = std::mem::take(&mut self.scratch).split_at_mut(len as usize);
        self.scratch = rest;
        for byte in &mut *bytes {
            byte.write(0);
        }
        // SAFETY: every byte above is initialized, and split_at_mut keeps
        // returned slices disjoint from all subsequent scratch allocations.
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<u8>(), bytes.len()) };
        self.cpu.read(at, bytes).ok()?;
        Some(bytes)
    }
    fn byte(&mut self, at: u32) -> Option<u8> {
        Some(self.read(at, 1)?[0])
    }
    fn word(&mut self, at: u32) -> Option<u16> {
        word(self.read(at, 2)?, 0)
    }
    fn dword(&mut self, at: u32) -> Option<u32> {
        Some(u32::from_le_bytes(self.read(at, 4)?.try_into().ok()?))
    }
    fn output(&mut self, at: u32, data: Buffer) {
        if data.len != 0 {
            self.writes.entries[self.writes.len] = Some((at, data));
            self.writes.len += 1;
        }
    }
    fn number(&mut self, at: u32, value: u32) {
        let mut data = Buffer::new();
        data.extend_from_slice(&value.to_le_bytes(), self.heap);
        self.output(at, data);
    }
    fn finish(self, result: u32) -> Option<(u64, Writes)> {
        for (i, entry) in self.writes.entries[..self.writes.len].iter().enumerate() {
            let (at, data) = entry.as_ref()?;
            let len = u32::try_from(data.len).ok()?;
            let end = at.checked_add(len)?;
            if !self.cpu.accessible(*at, len, Prot::WRITE)
                || self.reads[..self.read_count]
                    .iter()
                    .any(|(p, n)| *at < p + n && *p < end)
                || self.writes.entries[..i]
                    .iter()
                    .flatten()
                    .any(|(p, b)| *at < *p + b.len as u32 && *p < end)
            {
                return None;
            }
        }
        Some((u64::from(result), self.writes))
    }
}
fn commit(env: &mut Env<'_>, (result, writes): (u64, Writes)) -> Option<u64> {
    let heap = &env.ext.get::<super::Scratch>().0;
    for (at, data) in writes.entries.into_iter().flatten() {
        env.cpu.write(at, data.bytes(heap)).ok()?;
    }
    Some(result)
}
fn word(bytes: &[u8], index: u32) -> Option<u16> {
    let at = (index as usize).checked_mul(2)?;
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

struct TableData {
    bytes: Box<[u8]>,
    max_word: u32,
    glyph: bool,
}

// Keep full spans checked and tracked, even when only individual entries are read.
struct Table<'a> {
    cpu: &'a dyn Guest,
    at: u32,
    len: u32,
    data: Option<&'a [u8]>,
    memo: Option<Rc<TableData>>,
}
impl<'a> Table<'a> {
    fn load(mem: &mut Memory<'a>, at: u32, len: u32) -> Option<Self> {
        mem.track(at, len)?;
        let memo = mem
            .memo
            .get(mem.cpu, at, len as usize, |bytes| {
                Rc::new(TableData {
                    bytes: bytes.into(),
                    max_word: bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|w| u32::from(u16::from_le_bytes(*w)))
                        .max()
                        .unwrap_or(0),
                    glyph: bytes == [0, 1],
                })
            })
            .cloned();
        let data = if len == 0 {
            Some(&[][..])
        } else if memo.is_some() {
            None
        } else {
            if !mem.cpu.accessible(at, len, Prot::READ) {
                return None;
            }
            mem.cpu.shared_bytes(at, len as usize)
        };
        Some(Self {
            cpu: mem.cpu,
            at,
            len,
            data,
            memo,
        })
    }
    fn full(mem: &mut Memory<'a>, at: u32, len: u32) -> Option<Self> {
        let mut table = Self::load(mem, at, len)?;
        if table.bytes().is_none() {
            table.data = Some(mem.bytes(at, len)?);
        }
        Some(table)
    }
    fn empty(cpu: &'a dyn Guest) -> Self {
        Self {
            cpu,
            at: 0,
            len: 0,
            data: Some(&[]),
            memo: None,
        }
    }
    fn bytes(&self) -> Option<&[u8]> {
        self.memo.as_ref().map(|m| m.bytes.as_ref()).or(self.data)
    }
    fn max_word(&self) -> Option<u32> {
        if let Some(memo) = &self.memo {
            return Some(memo.max_word);
        }
        (0..self.len / 2).try_fold(0, |end, i| Some(end.max(u32::from(self.word(i)?))))
    }
    fn glyph(&self) -> Option<bool> {
        match &self.memo {
            Some(memo) => Some(memo.glyph),
            None => Some(self.word(0)? == 256),
        }
    }
    fn byte(&self, index: u32) -> Option<u8> {
        if let Some(data) = self.bytes() {
            return data.get(index as usize).copied();
        }
        if index >= self.len {
            return None;
        }
        let mut bytes = [0];
        self.cpu.read(self.at + index, &mut bytes).ok()?;
        Some(bytes[0])
    }
    fn word(&self, index: u32) -> Option<u16> {
        if let Some(data) = self.bytes() {
            return word(data, index);
        }
        let offset = index.checked_mul(2)?;
        if offset.checked_add(2)? > self.len {
            return None;
        }
        let mut bytes = [0; 2];
        self.cpu.read(self.at + offset, &mut bytes).ok()?;
        Some(u16::from_le_bytes(bytes))
    }
}

struct CodePage<'a> {
    code: u16,
    def: u16,
    uni_def: u16,
    trans_uni_def: u16,
    dbcs: bool,
    mb: u32,
    wc: u32,
    offsets: u32,
    mb_data: Option<Table<'a>>,
    wc_data: Option<Table<'a>>,
    offset_data: Option<Table<'a>>,
    ranges: Option<Table<'a>>,
}
impl<'a> CodePage<'a> {
    fn load(mem: &mut Memory<'a>, at: u32) -> Option<Self> {
        let bytes = mem.read(at, cp::SIZE)?;
        let pointer = |field: u32| {
            Some(u32::from_le_bytes(
                bytes
                    .get(field as usize..field as usize + 4)?
                    .try_into()
                    .ok()?,
            ))
        };
        Some(Self {
            code: word(bytes, cp::CodePage / 2)?,
            def: word(bytes, cp::DefaultChar / 2)?,
            uni_def: word(bytes, cp::UniDefaultChar / 2)?,
            trans_uni_def: word(bytes, cp::TransUniDefaultChar / 2)?,
            dbcs: word(bytes, cp::DBCSCodePage / 2)? != 0,
            mb: pointer(cp::MultiByteTable)?,
            wc: pointer(cp::WideCharTable)?,
            offsets: pointer(cp::DBCSOffsets)?,
            mb_data: None,
            wc_data: None,
            offset_data: None,
            ranges: None,
        })
    }
    fn tables(
        &mut self,
        mem: &mut Memory<'a>,
        wide: bool,
        decode: bool,
        glyph: bool,
    ) -> Option<()> {
        if wide {
            self.wc_data = Some(Table::load(
                mem,
                self.wc,
                if self.dbcs { 131072 } else { 65536 },
            )?);
        }
        if decode {
            if glyph && Table::load(mem, self.mb + 512, 2)?.glyph()? {
                self.mb += 514;
            }
            self.mb_data = Some(Table::load(mem, self.mb, 512)?);
            if self.offsets != 0 {
                let offsets = Table::load(mem, self.offsets, 512)?;
                let end = offsets.max_word()?;
                self.offset_data = Some(offsets);
                self.ranges = Some(Table::load(mem, self.offsets + 512, end * 2)?);
            }
        }
        Some(())
    }
    fn offset(&self, index: u32) -> Option<u16> {
        if self.offsets == 0 {
            Some(0)
        } else {
            self.offset_data.as_ref()?.word(index)
        }
    }
    fn dbcs_char(&self, index: u32) -> Option<u16> {
        if index < 256 {
            self.offset_data.as_ref()?.word(index)
        } else {
            self.ranges.as_ref()?.word(index - 256)
        }
    }
    fn encode(&self, ch: u16) -> Option<u16> {
        if self.dbcs {
            self.wc_data.as_ref()?.word(u32::from(ch))
        } else {
            Some(u16::from(self.wc_data.as_ref()?.byte(u32::from(ch))?))
        }
    }
    fn valid(&self, flags: u32, ch: u16, encoded: u16) -> Option<bool> {
        if flags & 0x400 == 0 && encoded != self.def {
            return Some(true);
        }
        let decoded = if self.dbcs && encoded > 0xff {
            let off = self.offset(u32::from(encoded >> 8))?;
            self.dbcs_char(u32::from(off) + u32::from(encoded & 0xff))?
        } else {
            self.mb_data.as_ref()?.word(u32::from(encoded))?
        };
        Some(decoded == ch)
    }
}

fn base(env: &Env<'_>, name: &str) -> Option<u32> {
    Some(
        env.ext
            .try_get::<crate::identity::Images>()?
            .0
            .values()
            .find(|m| m.name == name)?
            .base,
    )
}

fn codepage<'a>(mem: &mut Memory<'a>, base: u32, mut code: u32) -> Option<CodePage<'a>> {
    if code == 65010 {
        code = mem.dword(base + kb::unix_cp)?;
    }
    for (id, rva) in [(0, kb::ansi_cpinfo), (1, kb::oem_cpinfo)] {
        let at = base + rva;
        if code == id || code == u32::from(mem.word(at)?) {
            return CodePage::load(mem, at);
        }
    }
    if mem.dword(base + kb::locale_section + critical::LockCount)? != u32::MAX {
        return None;
    }
    let count = mem.dword(base + kb::nb_codepages)?;
    let pages = mem.read(base + kb::codepages, count.checked_mul(cp::SIZE)?)?;
    for (i, info) in pages
        .as_chunks::<{ cp::SIZE as usize }>()
        .0
        .iter()
        .enumerate()
    {
        let at = base + kb::codepages + i as u32 * cp::SIZE;
        if code == u32::from(word(info, 0)?) {
            return CodePage::load(mem, at);
        }
    }
    // Wine loads uncached pages through NT services, outside a body.
    None
}

fn input<'a>(mem: &mut Memory<'a>, src: u32, len: i32, width: u32) -> Option<&'a [u8]> {
    if len >= 0 {
        return mem.read(src, (len as u32).checked_mul(width)?);
    }
    let data = super::crt::string(mem.cpu, src, width)?;
    mem.track(src, data.len() as u32)?;
    Some(data)
}

fn error(mem: &mut Memory<'_>, teb: u32, value: u32) {
    mem.number(teb + LAST_ERROR, value);
}

struct Normalization<'a> {
    decomp_size: u32,
    comp_size: u32,
    l1: Table<'a>,
    l2: Table<'a>,
    hashes: Table<'a>,
    pairs: Table<'a>,
    seq: Table<'a>,
}
impl<'a> Normalization<'a> {
    fn load(mem: &mut Memory<'a>, table: u32, compose: bool) -> Option<Self> {
        let header = Table::full(mem, table, 0x44)?;
        let offset = |field| Some(u32::from(header.word(field / 2)?) * 2);
        let mut result = Self {
            decomp_size: u32::from(header.word(norm::decomp_size / 2)?),
            comp_size: u32::from(header.word(norm::comp_size / 2)?),
            l1: Table::empty(mem.cpu),
            l2: Table::empty(mem.cpu),
            hashes: Table::empty(mem.cpu),
            pairs: Table::empty(mem.cpu),
            seq: Table::empty(mem.cpu),
        };
        if compose {
            result.hashes = Table::full(
                mem,
                table.checked_add(offset(norm::comp_hash)?)?,
                (result.comp_size + 1) * 2,
            )?;
            result.seq = Table::full(
                mem,
                table.checked_add(offset(norm::comp_seq)?)?,
                u32::from(result.hashes.word(result.comp_size)?) * 2,
            )?;
        } else {
            let fields = [
                norm::props_level1,
                norm::props_level2,
                norm::decomp_hash,
                norm::decomp_map,
                norm::decomp_seq,
                norm::comp_hash,
            ];
            let mut slices = std::array::from_fn::<_, 5, _>(|_| Table::empty(mem.cpu));
            for (i, fields) in fields.windows(2).enumerate() {
                let start = offset(fields[0])?;
                slices[i] = Table::full(
                    mem,
                    table.checked_add(start)?,
                    offset(fields[1])?.checked_sub(start)?,
                )?;
            }
            [
                result.l1,
                result.l2,
                result.hashes,
                result.pairs,
                result.seq,
            ] = slices;
        }
        Some(result)
    }
    fn compose(&self, a: u16, b: u16) -> Option<u16> {
        let hash = (u32::from(a) + 95 * u32::from(b)).checked_rem(self.comp_size)?;
        let mut pos = u32::from(self.hashes.word(hash)?);
        let end = u32::from(self.hashes.word(hash + 1)?);
        while pos < end {
            let mut chars = [0; 3];
            for ch in &mut chars {
                *ch = self.seq.word(pos)?;
                pos += if (0xd800..=0xdbff).contains(ch) { 2 } else { 1 };
            }
            if chars[0] == a && chars[1] == b {
                return Some(chars[2]);
            }
        }
        Some(0)
    }
    // None inside Some denotes a character with no decomposition.
    fn decompose(&self, ch: u16) -> Option<Option<&[u8]>> {
        let hash = u32::from(ch).checked_rem(self.decomp_size)?;
        let mut pos = self.hashes.word(hash)?;
        let encoded = if pos >> 13 != 0 {
            let off = self.l1.byte(u32::from(ch) / 128)?;
            let props = if off == 0 || off >= 0xfb {
                off.rotate_left(5)
            } else {
                self.l2
                    .byte((u32::from(off) - 1) * 128 + u32::from(ch) % 128)?
            };
            if props != 0xbf {
                return Some(None);
            }
            pos
        } else {
            let mut i = hash + 1;
            while i < self.decomp_size && self.hashes.word(i)? >> 13 != 0 {
                i += 1;
            }
            let end = if i < self.decomp_size {
                self.hashes.word(i)?
            } else {
                let mut end = pos;
                while self.pairs.word(u32::from(end) * 2)? != 0 {
                    end += 1;
                }
                end
            };
            while pos < end && self.pairs.word(u32::from(pos) * 2)? != ch {
                pos += 1;
            }
            if pos >= end {
                return Some(None);
            }
            self.pairs.word(u32::from(pos) * 2 + 1)?
        };
        let start = u32::from(encoded & 0x1fff);
        let mut len = u32::from(encoded >> 13);
        if len == 7 {
            while self.seq.word(start + len)? != 0 {
                len += 1;
            }
        }
        if self.seq.word(start)? == 0 {
            len = 0;
        }
        Some(Some(
            self.seq
                .bytes()?
                .get(start as usize * 2..(start + len) as usize * 2)?,
        ))
    }
}

fn invalid(cp: &CodePage<'_>, src: &[u8]) -> Option<bool> {
    let mut i = 0;
    while i < src.len() {
        let c = u32::from(src[i]);
        let off = if cp.offsets != 0 { cp.offset(c)? } else { 0 };
        if off != 0 {
            if i + 1 == src.len() {
                return Some(true);
            }
            let ch = cp.dbcs_char(u32::from(off) + u32::from(src[i + 1]))?;
            if ch == cp.uni_def
                && (u16::from(src[i]) << 8 | u16::from(src[i + 1])) != cp.trans_uni_def
            {
                return Some(true);
            }
            i += 2;
        } else {
            let ch = cp.mb_data.as_ref()?.word(c)?;
            if (ch == cp.uni_def && c != u32::from(cp.trans_uni_def))
                || (0xe000..=0xf8ff).contains(&ch)
            {
                return Some(true);
            }
            i += 1;
        }
    }
    Some(false)
}

// spec: NAT-BODY-5
pub(super) fn multi_byte_to_wide_char(env: &mut Env<'_>) -> Option<u64> {
    let output = multi_byte(env)?;
    commit(env, output)
}

fn multi_byte(env: &mut Env<'_>) -> Option<(u64, Writes)> {
    let (code, flags, src, len, dst, cap) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3) as i32,
        env.arg(4),
        env.arg(5) as i32,
    );
    let (base, teb) = (base(env, "kernelbase.dll")?, env.ctx().teb);
    let mut scratch = [MaybeUninit::uninit(); 256 * 1024];
    let mut copies = Vec::new();
    env.ext.get_or_default::<super::Memos>();
    let (memo, heap) = env.ext.get2_mut::<super::Memos, super::Scratch>();
    heap.0.clear();
    let mut mem = Memory::new(env.cpu, &mut scratch, &mut copies, memo, &mut heap.0);
    if src == 0 || len == 0 || (dst == 0 && cap != 0) || cap < 0 {
        error(&mut mem, teb, 87);
        return mem.finish(0);
    }
    let mut cp = codepage(&mut mem, base, code)?;
    if cp.code == 65001 {
        return None;
    }
    let source = input(&mut mem, src, len, 1)?;
    if flags & !0xf != 0 {
        error(&mut mem, teb, 1004);
        return mem.finish(0);
    }
    cp.tables(&mut mem, false, true, flags & 4 != 0)?;
    if flags & 8 != 0 && invalid(&cp, source)? {
        error(&mut mem, teb, 1113);
        return mem.finish(0);
    }
    let norm = if flags & 2 != 0 {
        let table = mem.dword(base + kb::norm_info)?;
        Some(Normalization::load(&mut mem, table, false)?)
    } else {
        None
    };
    let mut output = Buffer::new();
    let (mut i, mut count) = (0, 0);
    while i < source.len() && (cap == 0 || count < cap as usize) {
        let c = u32::from(source[i]);
        let off = if cp.offsets != 0 { cp.offset(c)? } else { 0 };
        let ch = if off != 0 {
            if i + 1 < source.len() && source[i + 1] != 0 {
                i += 1;
                cp.dbcs_char(u32::from(off) + u32::from(source[i]))?
            } else {
                cp.uni_def
            }
        } else {
            cp.mb_data.as_ref()?.word(c)?
        };
        if let Some(norm) = &norm {
            let single = ch.to_le_bytes();
            let chars = norm.decompose(ch)?.unwrap_or(&single);
            if cap != 0 && count + chars.len() / 2 > cap as usize {
                break;
            }
            count += chars.len() / 2;
            if cap != 0 {
                output.extend_from_slice(chars, mem.heap);
            }
        } else {
            count += 1;
            if cap != 0 {
                output.extend_from_slice(&ch.to_le_bytes(), mem.heap);
            }
        }
        i += 1;
    }
    mem.output(dst, output);
    let result = if i < source.len() {
        error(&mut mem, teb, 122);
        0
    } else {
        count as u32
    };
    mem.finish(result)
}

pub(super) fn wide_char_to_multi_byte(env: &mut Env<'_>) -> Option<u64> {
    let output = wide_char(env)?;
    commit(env, output)
}

fn wide_char(env: &mut Env<'_>) -> Option<(u64, Writes)> {
    let (code, flags, src, len, dst, cap, def, used) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3) as i32,
        env.arg(4),
        env.arg(5) as i32,
        env.arg(6),
        env.arg(7),
    );
    let (base, teb) = (base(env, "kernelbase.dll")?, env.ctx().teb);
    let mut scratch = [MaybeUninit::uninit(); 256 * 1024];
    let mut copies = Vec::new();
    env.ext.get_or_default::<super::Memos>();
    let (memo, heap) = env.ext.get2_mut::<super::Memos, super::Scratch>();
    heap.0.clear();
    let mut mem = Memory::new(env.cpu, &mut scratch, &mut copies, memo, &mut heap.0);
    if src == 0 || len == 0 || (dst == 0 && cap != 0) || cap < 0 {
        error(&mut mem, teb, 87);
        return mem.finish(0);
    }
    let mut cp = codepage(&mut mem, base, code)?;
    cp.dbcs = cp.offsets != 0;
    if cp.code == 65001 {
        return None;
    }
    let source = input(&mut mem, src, len, 2)?;
    if flags & !0x6f0 != 0 {
        error(&mut mem, teb, 1004);
        return mem.finish(0);
    }
    let slow = flags != 0 || def != 0 || used != 0;
    cp.tables(&mut mem, true, slow, false)?;
    let default = if def == 0 {
        if cp.dbcs && cp.def > 0xff {
            cp.def.swap_bytes()
        } else {
            cp.def
        }
    } else if cp.dbcs {
        let first = u16::from(mem.byte(def)?);
        let second = u16::from(mem.byte(def + 1)?);
        if second == 0 {
            first
        } else {
            first << 8 | second
        }
    } else {
        u16::from(mem.byte(def)?)
    };
    let norm = if flags & 0x200 != 0 {
        let table = mem.dword(base + kb::norm_info)?;
        Some(Normalization::load(&mut mem, table, true)?)
    } else {
        None
    };
    let (mut i, mut count, mut used_default) = (0, 0, false);
    let mut output = Buffer::new();
    while i < source.len() / 2 && (cap == 0 || count < cap as usize) {
        let ch = word(source, i as u32)?;
        let mut consumed = 1;
        let mut composed_result = None;
        if let Some(norm) = &norm
            && i + 1 < source.len() / 2
        {
            let composed = norm.compose(ch, word(source, i as u32 + 1)?)?;
            if composed != 0 {
                let encoded = cp.encode(composed)?;
                if cp.valid(flags, composed, encoded)? {
                    composed_result = Some(encoded);
                    consumed = 2;
                } else if flags & 0x40 != 0 {
                    composed_result = Some(default);
                    used_default = true;
                    consumed = 2;
                } else if flags & 0x10 != 0 {
                    consumed = 2;
                }
            }
        }
        let encoded = if let Some(encoded) = composed_result {
            encoded
        } else {
            let encoded = cp.encode(ch)?;
            if slow && !cp.valid(flags, ch, encoded)? {
                used_default = true;
                default
            } else {
                encoded
            }
        };
        let size = if cp.dbcs && encoded > 0xff { 2 } else { 1 };
        if cap != 0 && count + size > cap as usize {
            break;
        }
        count += size;
        if cap != 0 {
            if size == 2 {
                output.push((encoded >> 8) as u8, mem.heap);
            }
            output.push(encoded as u8, mem.heap);
        }
        i += consumed;
    }
    mem.output(dst, output);
    if used != 0 {
        mem.number(used, u32::from(used_default));
    }
    let result = if i < source.len() / 2 {
        error(&mut mem, teb, 122);
        0
    } else {
        count as u32
    };
    mem.finish(result)
}

fn rtl(env: &mut Env<'_>, wide: bool, oem: bool, size: bool) -> Option<u64> {
    let output = rtl_convert(env, wide, oem, size)?;
    commit(env, output)
}

fn rtl_convert(env: &mut Env<'_>, wide: bool, oem: bool, size: bool) -> Option<(u64, Writes)> {
    let base = base(env, "ntdll.dll")?;
    let (dst, cap, result, src, len) = if size {
        (0, 0, env.arg(0), env.arg(1), env.arg(2))
    } else {
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4))
    };
    let mut scratch = [MaybeUninit::uninit(); 256 * 1024];
    let mut copies = Vec::new();
    env.ext.get_or_default::<super::Memos>();
    let (memo, heap) = env.ext.get2_mut::<super::Memos, super::Scratch>();
    heap.0.clear();
    let mut mem = Memory::new(env.cpu, &mut scratch, &mut copies, memo, &mut heap.0);
    let mut cp = CodePage::load(&mut mem, base + NLS_INFO + if oem { 0 } else { ANSI_TABLE })?;
    if cp.code == 65001 {
        return None;
    }
    if size && !cp.dbcs {
        let count = if wide { len / 2 } else { len.wrapping_mul(2) };
        mem.number(result, count);
        return mem.finish(0);
    }
    cp.tables(&mut mem, wide, !wide, false)?;
    let source = mem.read(src, if wide { len & !1 } else { len })?;
    let mut output = Buffer::new();
    let (mut i, mut count) = (0, 0);
    let cap = if wide { cap } else { cap / 2 } as usize;
    while i < source.len() && (size || count < cap) {
        if wide {
            let ch = u16::from_le_bytes([source[i], source[i + 1]]);
            let encoded = cp.encode(ch)?;
            let n = if cp.dbcs && encoded > 0xff { 2 } else { 1 };
            if !size && count + n > cap {
                break;
            }
            if !size {
                if n == 2 {
                    output.push((encoded >> 8) as u8, mem.heap);
                }
                output.push(encoded as u8, mem.heap);
            }
            i += 2;
            count += n;
        } else {
            let c = u32::from(source[i]);
            let off = if cp.offsets != 0 { cp.offset(c)? } else { 0 };
            let ch = if off != 0 && i + 1 < source.len() {
                i += 1;
                cp.dbcs_char(u32::from(off) + u32::from(source[i]))?
            } else {
                cp.mb_data.as_ref()?.word(c)?
            };
            if !size {
                output.extend_from_slice(&ch.to_le_bytes(), mem.heap);
            }
            i += 1;
            count += 1;
        }
    }
    mem.output(dst, output);
    if size || result != 0 {
        let count = if wide { count } else { count * 2 } as u32;
        mem.number(result, count);
    }
    mem.finish(0)
}

pub(super) fn rtl_multi_byte_to_unicode_n(env: &mut Env<'_>) -> Option<u64> {
    rtl(env, false, false, false)
}
pub(super) fn rtl_unicode_to_multi_byte_n(env: &mut Env<'_>) -> Option<u64> {
    rtl(env, true, false, false)
}
pub(super) fn rtl_multi_byte_to_unicode_size(env: &mut Env<'_>) -> Option<u64> {
    rtl(env, false, false, true)
}
pub(super) fn rtl_unicode_to_multi_byte_size(env: &mut Env<'_>) -> Option<u64> {
    rtl(env, true, false, true)
}
pub(super) fn rtl_oem_to_unicode_n(env: &mut Env<'_>) -> Option<u64> {
    rtl(env, false, true, false)
}
pub(super) fn rtl_unicode_to_oem_n(env: &mut Env<'_>) -> Option<u64> {
    rtl(env, true, true, false)
}
