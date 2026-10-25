//! Wine 11 code page loops, using its loaded CPTABLEINFO and normalization data.

use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::Env;

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

// Compute first, then check every store. Aliases that can change subsequent
// table or source reads resume Wine, as do inaccessible inputs and outputs.
struct Memory<'a> {
    cpu: &'a mut dyn Guest,
    reads: Vec<(u32, u32)>,
    writes: Vec<(u32, Vec<u8>)>,
}
impl Memory<'_> {
    fn check(&mut self, at: u32, len: u32) -> Option<()> {
        at.checked_add(len)?;
        if !self.cpu.accessible(at, len, Prot::READ) {
            return None;
        }
        self.reads.push((at, len));
        Some(())
    }
    fn read(&mut self, at: u32, len: u32) -> Option<Vec<u8>> {
        if len == 0 {
            return Some(Vec::new());
        }
        self.check(at, len)?;
        let mut data = vec![0; len as usize];
        self.cpu.read(at, &mut data).ok()?;
        Some(data)
    }
    fn byte(&mut self, at: u32) -> Option<u8> {
        self.check(at, 1)?;
        self.cpu.read_u8(at).ok()
    }
    fn word(&mut self, at: u32) -> Option<u16> {
        self.check(at, 2)?;
        self.cpu.read_u16(at).ok()
    }
    fn dword(&mut self, at: u32) -> Option<u32> {
        self.check(at, 4)?;
        self.cpu.read_u32(at).ok()
    }
    fn table(&mut self, at: u32, index: u32) -> Option<u16> {
        self.word(at.checked_add(index.checked_mul(2)?)?)
    }
    fn output(&mut self, at: u32, data: Vec<u8>) {
        if !data.is_empty() {
            self.writes.push((at, data));
        }
    }
    fn finish(self, result: u32) -> Option<u64> {
        for (i, (at, data)) in self.writes.iter().enumerate() {
            let len = u32::try_from(data.len()).ok()?;
            let end = at.checked_add(len)?;
            if !self.cpu.accessible(*at, len, Prot::WRITE)
                || self.reads.iter().any(|(p, n)| *at < p + n && *p < end)
                || self.writes[..i]
                    .iter()
                    .any(|(p, b)| *at < *p + b.len() as u32 && *p < end)
            {
                return None;
            }
        }
        for (at, data) in self.writes {
            self.cpu.write(at, &data).ok()?;
        }
        Some(u64::from(result))
    }
}

struct CodePage {
    code: u16,
    def: u16,
    uni_def: u16,
    trans_uni_def: u16,
    dbcs: bool,
    mb: u32,
    wc: u32,
    offsets: u32,
}
impl CodePage {
    fn load(mem: &mut Memory<'_>, at: u32) -> Option<Self> {
        Some(Self {
            code: mem.word(at + cp::CodePage)?,
            def: mem.word(at + cp::DefaultChar)?,
            uni_def: mem.word(at + cp::UniDefaultChar)?,
            trans_uni_def: mem.word(at + cp::TransUniDefaultChar)?,
            dbcs: mem.word(at + cp::DBCSCodePage)? != 0,
            mb: mem.dword(at + cp::MultiByteTable)?,
            wc: mem.dword(at + cp::WideCharTable)?,
            offsets: mem.dword(at + cp::DBCSOffsets)?,
        })
    }
    fn encode(&self, mem: &mut Memory<'_>, ch: u16) -> Option<u16> {
        if self.dbcs {
            mem.table(self.wc, u32::from(ch))
        } else {
            Some(u16::from(mem.byte(self.wc.checked_add(u32::from(ch))?)?))
        }
    }
    fn valid(&self, mem: &mut Memory<'_>, flags: u32, ch: u16, encoded: u16) -> Option<bool> {
        if flags & 0x400 == 0 && encoded != self.def {
            return Some(true);
        }
        let decoded = if self.dbcs && encoded > 0xff {
            let off = mem.table(self.offsets, u32::from(encoded >> 8))?;
            mem.table(self.offsets, u32::from(off) + u32::from(encoded & 0xff))?
        } else {
            mem.table(self.mb, u32::from(encoded))?
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

fn codepage(mem: &mut Memory<'_>, base: u32, mut code: u32) -> Option<CodePage> {
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
    for i in 0..count {
        let at = base + kb::codepages + i * cp::SIZE;
        if code == u32::from(mem.word(at)?) {
            return CodePage::load(mem, at);
        }
    }
    // Wine loads uncached pages through NT services, outside a leaf body.
    None
}

fn input(mem: &mut Memory<'_>, src: u32, len: i32, width: u32) -> Option<Vec<u8>> {
    if len >= 0 {
        return mem.read(src, (len as u32).checked_mul(width)?);
    }
    let mut data = Vec::new();
    let mut at = src;
    loop {
        let ch = if width == 1 {
            u16::from(mem.byte(at)?)
        } else {
            mem.word(at)?
        };
        data.extend_from_slice(&ch.to_le_bytes()[..width as usize]);
        if ch == 0 {
            return Some(data);
        }
        at = at.checked_add(width)?;
    }
}

fn error(mem: &mut Memory<'_>, teb: u32, value: u32) {
    mem.output(teb + LAST_ERROR, value.to_le_bytes().to_vec());
}

fn norm_table(mem: &mut Memory<'_>, table: u32, field: u32) -> Option<u32> {
    table.checked_add(u32::from(mem.word(table + field)?) * 2)
}

fn compose(mem: &mut Memory<'_>, table: u32, a: u16, b: u16) -> Option<u16> {
    let size = u32::from(mem.word(table + norm::comp_size)?);
    let hash = (u32::from(a) + 95 * u32::from(b)).checked_rem(size)?;
    let hashes = norm_table(mem, table, norm::comp_hash)?;
    let seq = norm_table(mem, table, norm::comp_seq)?;
    let mut pos = u32::from(mem.table(hashes, hash)?);
    let end = u32::from(mem.table(hashes, hash + 1)?);
    while pos < end {
        let mut chars = [0; 3];
        for ch in &mut chars {
            *ch = mem.table(seq, pos)?;
            pos += if (0xd800..=0xdbff).contains(ch) { 2 } else { 1 };
        }
        if chars[0] == a && chars[1] == b {
            return Some(chars[2]);
        }
    }
    Some(0)
}

fn decompose(mem: &mut Memory<'_>, table: u32, ch: u16) -> Option<Vec<u16>> {
    let size = u32::from(mem.word(table + norm::decomp_size)?);
    let hash = u32::from(ch).checked_rem(size)?;
    let hashes = norm_table(mem, table, norm::decomp_hash)?;
    let mut pos = mem.table(hashes, hash)?;
    let encoded = if pos >> 13 != 0 {
        let l1 = norm_table(mem, table, norm::props_level1)?;
        let off = mem.byte(l1 + u32::from(ch) / 128)?;
        let props = if off == 0 || off >= 0xfb {
            off.rotate_left(5)
        } else {
            let l2 = norm_table(mem, table, norm::props_level2)?;
            mem.byte(l2 + (u32::from(off) - 1) * 128 + u32::from(ch) % 128)?
        };
        if props != 0xbf {
            return Some(vec![ch]);
        }
        pos
    } else {
        let pairs = norm_table(mem, table, norm::decomp_map)?;
        let mut i = hash + 1;
        while i < size && mem.table(hashes, i)? >> 13 != 0 {
            i += 1;
        }
        let end = if i < size {
            mem.table(hashes, i)?
        } else {
            let mut end = pos;
            while mem.table(pairs, u32::from(end) * 2)? != 0 {
                end += 1;
            }
            end
        };
        while pos < end && mem.table(pairs, u32::from(pos) * 2)? != ch {
            pos += 1;
        }
        if pos >= end {
            return Some(vec![ch]);
        }
        mem.table(pairs, u32::from(pos) * 2 + 1)?
    };
    let seq = norm_table(mem, table, norm::decomp_seq)?;
    let start = u32::from(encoded & 0x1fff);
    let mut len = u32::from(encoded >> 13);
    if len == 7 {
        while mem.table(seq, start + len)? != 0 {
            len += 1;
        }
    }
    if mem.table(seq, start)? == 0 {
        len = 0;
    }
    (0..len).map(|i| mem.table(seq, start + i)).collect()
}

fn invalid(mem: &mut Memory<'_>, cp: &CodePage, src: &[u8]) -> Option<bool> {
    let mut i = 0;
    while i < src.len() {
        let c = u32::from(src[i]);
        let off = if cp.offsets != 0 {
            mem.table(cp.offsets, c)?
        } else {
            0
        };
        if off != 0 {
            if i + 1 == src.len() {
                return Some(true);
            }
            let ch = mem.table(cp.offsets, u32::from(off) + u32::from(src[i + 1]))?;
            if ch == cp.uni_def
                && (u16::from(src[i]) << 8 | u16::from(src[i + 1])) != cp.trans_uni_def
            {
                return Some(true);
            }
            i += 2;
        } else {
            let ch = mem.table(cp.mb, c)?;
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

// spec: NAT-BODY-4
pub(super) fn multi_byte_to_wide_char(env: &mut Env<'_>) -> Option<u64> {
    let (code, flags, src, len, dst, cap) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3) as i32,
        env.arg(4),
        env.arg(5) as i32,
    );
    let (base, teb) = (base(env, "kernelbase.dll")?, env.ctx().teb);
    let mut mem = Memory {
        cpu: env.cpu,
        reads: Vec::new(),
        writes: Vec::new(),
    };
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
    if flags & 4 != 0 && mem.table(cp.mb, 256)? == 256 {
        cp.mb += 514;
    }
    if flags & 8 != 0 && invalid(&mut mem, &cp, &source)? {
        error(&mut mem, teb, 1113);
        return mem.finish(0);
    }
    let norm = if flags & 2 != 0 {
        mem.dword(base + kb::norm_info)?
    } else {
        0
    };
    let mut output = Vec::new();
    let (mut i, mut count) = (0, 0);
    while i < source.len() && (cap == 0 || count < cap as usize) {
        let c = u32::from(source[i]);
        let off = if cp.offsets != 0 {
            mem.table(cp.offsets, c)?
        } else {
            0
        };
        let ch = if off != 0 {
            if i + 1 < source.len() && source[i + 1] != 0 {
                i += 1;
                mem.table(cp.offsets, u32::from(off) + u32::from(source[i]))?
            } else {
                cp.uni_def
            }
        } else {
            mem.table(cp.mb, c)?
        };
        if flags & 2 != 0 {
            let chars = decompose(&mut mem, norm, ch)?;
            if cap != 0 && count + chars.len() > cap as usize {
                break;
            }
            count += chars.len();
            if cap != 0 {
                for ch in chars {
                    output.extend_from_slice(&ch.to_le_bytes());
                }
            }
        } else {
            count += 1;
            if cap != 0 {
                output.extend_from_slice(&ch.to_le_bytes());
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
    let mut mem = Memory {
        cpu: env.cpu,
        reads: Vec::new(),
        writes: Vec::new(),
    };
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
    let source: Vec<_> = source
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    if flags & !0x6f0 != 0 {
        error(&mut mem, teb, 1004);
        return mem.finish(0);
    }
    let slow = flags != 0 || def != 0 || used != 0;
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
        mem.dword(base + kb::norm_info)?
    } else {
        0
    };
    let (mut i, mut count, mut used_default) = (0, 0, false);
    let mut output = Vec::new();
    while i < source.len() && (cap == 0 || count < cap as usize) {
        let ch = source[i];
        let mut consumed = 1;
        let mut composed_result = None;
        if flags & 0x200 != 0 && i + 1 < source.len() {
            let composed = compose(&mut mem, norm, ch, source[i + 1])?;
            if composed != 0 {
                let encoded = cp.encode(&mut mem, composed)?;
                if cp.valid(&mut mem, flags, composed, encoded)? {
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
            let encoded = cp.encode(&mut mem, ch)?;
            if slow && !cp.valid(&mut mem, flags, ch, encoded)? {
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
                output.push((encoded >> 8) as u8);
            }
            output.push(encoded as u8);
        }
        i += consumed;
    }
    mem.output(dst, output);
    if used != 0 {
        mem.output(used, u32::from(used_default).to_le_bytes().to_vec());
    }
    let result = if i < source.len() {
        error(&mut mem, teb, 122);
        0
    } else {
        count as u32
    };
    mem.finish(result)
}

fn rtl(env: &mut Env<'_>, wide: bool, oem: bool, size: bool) -> Option<u64> {
    let base = base(env, "ntdll.dll")?;
    let (dst, cap, result, src, len) = if size {
        (0, 0, env.arg(0), env.arg(1), env.arg(2))
    } else {
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4))
    };
    let mut mem = Memory {
        cpu: env.cpu,
        reads: Vec::new(),
        writes: Vec::new(),
    };
    let cp = CodePage::load(&mut mem, base + NLS_INFO + if oem { 0 } else { ANSI_TABLE })?;
    if cp.code == 65001 {
        return None;
    }
    if size && !cp.dbcs {
        let count = if wide { len / 2 } else { len.wrapping_mul(2) };
        mem.output(result, count.to_le_bytes().to_vec());
        return mem.finish(0);
    }
    let source = mem.read(src, if wide { len & !1 } else { len })?;
    let mut output = Vec::new();
    let (mut i, mut count) = (0, 0);
    let cap = if wide { cap } else { cap / 2 } as usize;
    while i < source.len() && (size || count < cap) {
        if wide {
            let ch = u16::from_le_bytes([source[i], source[i + 1]]);
            let encoded = cp.encode(&mut mem, ch)?;
            let n = if cp.dbcs && encoded > 0xff { 2 } else { 1 };
            if !size && count + n > cap {
                break;
            }
            if !size {
                if n == 2 {
                    output.push((encoded >> 8) as u8);
                }
                output.push(encoded as u8);
            }
            i += 2;
            count += n;
        } else {
            let c = u32::from(source[i]);
            let off = if cp.offsets != 0 {
                mem.table(cp.offsets, c)?
            } else {
                0
            };
            let ch = if off != 0 && i + 1 < source.len() {
                i += 1;
                mem.table(cp.offsets, u32::from(off) + u32::from(source[i]))?
            } else {
                mem.table(cp.mb, c)?
            };
            if !size {
                output.extend_from_slice(&ch.to_le_bytes());
            }
            i += 1;
            count += 1;
        }
    }
    mem.output(dst, output);
    if size || result != 0 {
        let count = if wide { count } else { count * 2 } as u32;
        mem.output(result, count.to_le_bytes().to_vec());
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
