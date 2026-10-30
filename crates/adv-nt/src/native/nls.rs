/*
 * Rust port of Wine 11 kernelbase locale/string and ntdll locale routines.
 *
 * Copyright 1995 Martin von Loewis
 * Copyright 1998 David Lee Lambert
 * Copyright 2000 Julio César Gázquez
 * Copyright 2003 Jon Griffiths
 * Copyright 2005 Dmitry Timoshkov
 * Copyright 2002, 2004, 2019, 2022 Alexandre Julliard
 * Copyright 2019 Nikolay Sivov for CodeWeavers
 *
 * This library is free software; you can redistribute it and/or
 * modify it under the terms of the GNU Lesser General Public
 * License as published by the Free Software Foundation; either
 * version 2.1 of the License, or (at your option) any later version.
 *
 * This library is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
 * Lesser General Public License for more details.
 *
 * You should have received a copy of the GNU Lesser General Public
 * License along with this library; if not, write to the Free Software
 * Foundation, Inc., 51 Franklin St, Fifth Floor, Boston, MA 02110-1301, USA
 */
//! Wine 11 `kernelbase/locale.c` comparison, reading Wine's NLS data in place.
//!
//! Function names follow Wine. Comparison levels stream their weights, and
//! ANSI arguments decode directly from the caller's bytes without UTF-16 buffers.
//! Cold locale resolution stays in
//! Wine because it requests the registry and publishes shared cache entries.

use adv_cpu::body::{EAX, Input, Mem, Ret, Stop};
use std::mem::MaybeUninit;

mod stream;

type Result<T> = std::result::Result<T, Error>;
enum Error {
    Stop(Stop),
    Pages,
}
impl From<Stop> for Error {
    fn from(stop: Stop) -> Self {
        Self::Stop(stop)
    }
}

// Offsets and RVAs checked against the pinned modules' DWARF.
#[allow(non_upper_case_globals)]
mod cp {
    pub const SIZE: u32 = 0x2c;
    pub const CodePage: u32 = 0;
    pub const UniDefaultChar: u32 = 6;
    pub const DBCSCodePage: u32 = 12;
    pub const MultiByteTable: u32 = 28;
    pub const DBCSOffsets: u32 = 40;
}
#[allow(non_upper_case_globals)]
mod kb {
    pub const locale_section: u32 = 0x871a0;
    pub const ansi_cpinfo: u32 = 0xc0b00;
    pub const oem_cpinfo: u32 = 0xc0ac0;
    pub const codepages: u32 = 0xbf480;
    pub const nb_codepages: u32 = 0xbf460;
    pub const sort: u32 = 0xbf420;
    pub const lcids_index: u32 = 0xc0a94;
    pub const lcnames_index: u32 = 0xc0a90;
    pub const locale_table: u32 = 0xc0a8c;
    pub const locale_strings: u32 = 0xc0a88;
    pub const locale_sorts: u32 = 0xbf458;
    pub const current_locale_sort: u32 = 0xbf454;
    pub const system_lcid: u32 = 0xc0aa8;
    pub const user_lcid: u32 = 0xc0aa4;
    pub const system_locale: u32 = 0xc0a84;
    pub const user_locale: u32 = 0xc0a80;
}
#[allow(non_upper_case_globals)]
mod sort {
    pub const compr_count: u32 = 0x0c;
    pub const keys: u32 = 0x10;
    pub const expansions: u32 = 0x24;
    pub const compressions: u32 = 0x28;
    pub const compr_data: u32 = 0x2c;
    pub const jamo: u32 = 0x30;
}
#[allow(non_upper_case_globals)]
mod sortguid {
    pub const flags: u32 = 0x10;
    pub const compr: u32 = 0x14;
    pub const except: u32 = 0x18;
    pub const ling_except: u32 = 0x1c;
}
#[allow(non_upper_case_globals)]
mod compression {
    pub const SIZE: u32 = 0x18;
    pub const offset: u32 = 0;
    pub const minchar: u32 = 4;
    pub const maxchar: u32 = 6;
    pub const len: u32 = 8;
}
#[allow(non_upper_case_globals)]
mod locale_header {
    pub const nb_lcids: u32 = 0x1e;
    pub const locale_size: u32 = 0x22;
    pub const locales_offset: u32 = 0x24;
    pub const nb_lcnames: u32 = 0x28;
}
#[allow(non_upper_case_globals)]
mod locale_data {
    pub const sname: u32 = 0;
    pub const inotneutral: u32 = 0x18;
    pub const idefaultansicodepage: u32 = 0x6e;
    pub const ssortlocale: u32 = 0x124;
}
#[allow(non_upper_case_globals)]
mod lcid_index {
    pub const SIZE: u32 = 8;
    pub const id: u32 = 0;
    pub const idx: u32 = 4;
    pub const name: u32 = 6;
}
#[allow(non_upper_case_globals)]
mod lcname_index {
    pub const SIZE: u32 = 8;
    pub const name: u32 = 0;
    pub const idx: u32 = 2;
}
#[allow(non_upper_case_globals)]
mod critical {
    pub const LockCount: u32 = 4;
}
#[allow(non_upper_case_globals)]
mod expansion {
    pub const SIZE: u32 = 4;
    pub const exp: u32 = 0;
}
#[allow(non_upper_case_globals)]
mod jamo {
    pub const SIZE: u32 = 8;
    pub const is_old: usize = 0;
    pub const leading: usize = 1;
    pub const vowel: usize = 2;
    pub const trailing: usize = 3;
    pub const weight: usize = 4;
}
#[allow(non_upper_case_globals)]
mod teb {
    pub const LastErrorValue: u32 = 0x34;
    pub const CurrentLocale: u32 = 0xc4;
}
const NT_NLS_INFO: u32 = 0x723e0;
const NT_UPPER_CASE: u32 = 0x58;

const IGNORECASE: u32 = 1;
const IGNORENONSPACE: u32 = 2;
const IGNORESYMBOLS: u32 = 4;
const DIGITSASNUMBERS: u32 = 8;
const LING_IGNORECASE: u32 = 0x10;
const LING_IGNOREDIACRITIC: u32 = 0x20;
const STRINGSORT: u32 = 0x1000;
const IGNOREKANATYPE: u32 = 0x10000;
const IGNOREWIDTH: u32 = 0x20000;
const LING_CASING: u32 = 0x8000000;
const USE_CP_ACP: u32 = 0x40000000;
const SUPPORTED: u32 = 0x5803103f;

#[derive(Clone, Copy)]
struct Page {
    number: u32,
    pointer: *const u8,
}
// The common call needs only a handful of pages. A call touching more than
// 32 pages replays with this lazily initialized bitmap on the cold stack frame.
// Two bits retain each grant and its window/private-alias selection.
struct Grants {
    initialized: [u64; 256],
    bits: [MaybeUninit<u128>; 16384],
    bases: [*const u8; 2],
}
struct Memory<'a, M> {
    mem: &'a mut M,
    pages: [Option<Page>; 32],
    large: Option<&'a mut Grants>,
}
impl<'a, M: Mem> Memory<'a, M> {
    fn new(mem: &'a mut M, large: Option<&'a mut Grants>) -> Self {
        Self {
            mem,
            pages: [None; 32],
            large,
        }
    }
    #[inline(always)]
    fn page(&mut self, number: u32) -> Result<*const u8> {
        let first = number as usize & 31;
        for offset in 0..2 {
            let index = (first + offset) & 31;
            if let Some(page) = self.pages[index] {
                if page.number == number {
                    return Ok(page.pointer);
                }
            } else if self.large.is_none() {
                let pointer = self.mem.read_page(number)?.unwrap_or(std::ptr::null());
                self.pages[index] = Some(Page { number, pointer });
                return Ok(pointer);
            }
        }
        self.page_miss(number, first)
    }
    #[inline(never)]
    fn page_miss(&mut self, number: u32, first: usize) -> Result<*const u8> {
        if let Some(grants) = self.large.as_mut() {
            let block = number as usize / 64;
            let mask = 1u128 << (number % 64);
            let initialized = &mut grants.initialized[block / 64];
            if *initialized & (1 << (block % 64)) == 0 {
                grants.bits[block].write(0);
                *initialized |= 1 << (block % 64);
            }
            // SAFETY: this bitmap word was initialized above.
            let bits = unsafe { grants.bits[block].assume_init_mut() };
            if *bits & mask == 0 {
                let pointer = self.mem.read_page(number)?.unwrap();
                let base = pointer.wrapping_sub((number as usize) << 12);
                if grants.bases[0].is_null() {
                    grants.bases[0] = base;
                }
                if base != grants.bases[0] {
                    grants.bases[1] = base;
                    *bits |= mask << 64;
                }
                *bits |= mask;
            }
            let pointer = grants.bases[usize::from(*bits & (mask << 64) != 0)]
                .wrapping_add((number as usize) << 12);
            // Retain a hot direct-mapped slot over the complete bitmap.
            self.pages[first] = Some(Page { number, pointer });
            return Ok(pointer);
        }
        for i in 0..32 {
            let slot = &mut self.pages[(first + i) % 32];
            if let Some(page) = slot {
                if page.number == number {
                    return Ok(page.pointer);
                }
            } else {
                let pointer = self.mem.read_page(number)?.unwrap_or(std::ptr::null());
                *slot = Some(Page { number, pointer });
                return Ok(pointer);
            }
        }
        if M::WINDOW {
            return Err(Error::Pages);
        }
        let pointer = self.mem.read_page(number)?.unwrap_or(std::ptr::null());
        self.pages[first] = Some(Page { number, pointer });
        Ok(pointer)
    }
    #[inline(always)]
    fn read<const N: usize>(&mut self, at: u32) -> Result<[u8; N]> {
        let pointer = self.page(at >> 12)?;
        if !M::WINDOW && pointer.is_null() {
            return Ok(self.mem.read(at)?);
        }
        if (at & 0xfff) as usize + N <= 4096 {
            // SAFETY: this page has a retained readable grant.
            return Ok(unsafe {
                pointer
                    .add((at & 0xfff) as usize)
                    .cast::<[u8; N]>()
                    .read_unaligned()
            });
        }
        self.cross_page(at)
    }
    #[inline(never)]
    fn cross_page<const N: usize>(&mut self, at: u32) -> Result<[u8; N]> {
        let mut value = [0; N];
        for (i, byte) in value.iter_mut().enumerate() {
            *byte = self.byte(at.wrapping_add(i as u32))?;
        }
        Ok(value)
    }
    #[inline(always)]
    fn byte(&mut self, at: u32) -> Result<u8> {
        Ok(self.read::<1>(at)?[0])
    }
    #[inline(always)]
    fn word(&mut self, at: u32) -> Result<u16> {
        Ok(u16::from_le_bytes(self.read(at)?))
    }
    #[inline(always)]
    fn dword(&mut self, at: u32) -> Result<u32> {
        Ok(u32::from_le_bytes(self.read(at)?))
    }
    fn error(&mut self, thread: u32, code: u32) -> Result<u32> {
        let pointer = self.mem.output(thread + teb::LastErrorValue, 4)?;
        // SAFETY: output checked all four bytes; this is the last guest access.
        unsafe {
            pointer.cast::<u32>().write_unaligned(code.to_le());
        }
        self.mem.commit()?;
        Ok(0)
    }
}

// A table/source cursor retains only a granted page address, never table data.
#[derive(Clone)]
struct ReadPage(std::cell::Cell<(u32, *const u8)>);
impl ReadPage {
    fn new() -> Self {
        Self(std::cell::Cell::new((u32::MAX, std::ptr::null())))
    }
    #[inline(always)]
    fn read<const N: usize, M: Mem>(&self, m: &mut Memory<'_, M>, at: u32) -> Result<[u8; N]> {
        if (at & 0xfff) as usize + N > 4096 {
            return m.cross_page(at);
        }
        let (page, mut pointer) = self.0.get();
        if page != at >> 12 {
            pointer = m.page(at >> 12)?;
            self.0.set((at >> 12, pointer));
        }
        if !M::WINDOW && pointer.is_null() {
            return Ok(m.mem.read(at)?);
        }
        // SAFETY: this cursor retains a readable page grant for this call.
        Ok(unsafe {
            pointer
                .add((at & 0xfff) as usize)
                .cast::<[u8; N]>()
                .read_unaligned()
        })
    }
    #[inline(always)]
    fn byte<M: Mem>(&self, m: &mut Memory<'_, M>, at: u32) -> Result<u8> {
        Ok(self.read::<1, M>(m, at)?[0])
    }
    #[inline(always)]
    fn word<M: Mem>(&self, m: &mut Memory<'_, M>, at: u32) -> Result<u16> {
        Ok(u16::from_le_bytes(self.read(m, at)?))
    }
    #[inline(always)]
    fn dword<M: Mem>(&self, m: &mut Memory<'_, M>, at: u32) -> Result<u32> {
        Ok(u32::from_le_bytes(self.read(m, at)?))
    }
}
#[derive(Clone)]
struct CodePage {
    mb_page: ReadPage,
    offset_page: ReadPage,
    dbcs_page: ReadPage,
    code: u32,
    mb: u32,
    offsets: u32,
    default: u16,
}
// Wine get_codepage_table. Cached non-ACP/OEM lookups take locale_section;
// Wine owns that synchronization and any first-load publication.
fn get_codepage_table<M: Mem>(m: &mut Memory<'_, M>, base: u32, code: u32) -> Result<CodePage> {
    if code == 65001 {
        return Ok(CodePage {
            mb_page: ReadPage::new(),
            offset_page: ReadPage::new(),
            dbcs_page: ReadPage::new(),
            code,
            mb: 0,
            offsets: 0,
            default: 0xfffd,
        });
    }
    let load = |m: &mut Memory<'_, M>, at| -> Result<CodePage> {
        Ok(CodePage {
            mb_page: ReadPage::new(),
            offset_page: ReadPage::new(),
            dbcs_page: ReadPage::new(),
            code,
            mb: m.dword(at + cp::MultiByteTable)?,
            offsets: if m.word(at + cp::DBCSCodePage)? != 0 {
                m.dword(at + cp::DBCSOffsets)?
            } else {
                0
            },
            default: m.word(at + cp::UniDefaultChar)?,
        })
    };
    for (id, field) in [(0, kb::ansi_cpinfo), (1, kb::oem_cpinfo)] {
        let at = base + field;
        if code == id || code == u32::from(m.word(at + cp::CodePage)?) {
            return load(m, at);
        }
    }
    // Execution has one owner. An unowned locale lock protects no concurrent
    // writer, so already-published entries can be read without taking it.
    // A held lock or an uncached page needs Wine's wait/load/publication path.
    if m.dword(base + kb::locale_section + critical::LockCount)? != u32::MAX {
        return Err(Stop::Guest.into());
    }
    let count = m.dword(base + kb::nb_codepages)?;
    for i in 0..count {
        let at = base + kb::codepages + i * cp::SIZE;
        if code == u32::from(m.word(at + cp::CodePage)?) {
            return load(m, at);
        }
    }
    Err(Stop::Guest.into())
}

// Wine's ordinal comparison needs only a wide source and its checked page.
struct Source {
    at: u32,
    len: u32,
    input: ReadPage,
}
impl Source {
    fn wide<M: Mem>(m: &mut Memory<'_, M>, at: u32, len: u32) -> Result<Self> {
        let len = if (len as i32) < 0 {
            terminated(m, at, 2)?
        } else {
            len
        };
        Ok(Self {
            at,
            len,
            input: ReadPage::new(),
        })
    }
    #[inline(always)]
    fn get<M: Mem>(&self, m: &mut Memory<'_, M>, index: u32) -> Result<u16> {
        self.input
            .word(m, self.at.wrapping_add(index.wrapping_mul(2)))
    }
}
// Wine strlen/lstrlenW: scan each granted page up to its zero unit.
fn terminated<M: Mem>(m: &mut Memory<'_, M>, at: u32, width: u32) -> Result<u32> {
    let mut len = 0u32;
    loop {
        let address = at.wrapping_add(len.wrapping_mul(width));
        let pointer = m.page(address >> 12)?;
        let count = (4096 - (address & 0xfff)) / width;
        if (!M::WINDOW && pointer.is_null()) || count == 0 {
            let ch = if width == 1 {
                u16::from(m.byte(address)?)
            } else {
                m.word(address)?
            };
            if ch == 0 {
                return Ok(len);
            }
            len += 1;
            continue;
        }
        for i in 0..count {
            // SAFETY: every unit is inside this retained readable page.
            let ch = unsafe {
                let p = pointer.add(((address & 0xfff) + i * width) as usize);
                if width == 1 {
                    u16::from(p.read())
                } else {
                    u16::from_le(p.cast::<u16>().read_unaligned())
                }
            };
            if ch == 0 {
                return Ok(len + i);
            }
        }
        len += count;
    }
}
// Wine mbstowcs_sbcs, flags zero: one table lookup per input byte.
#[inline(always)]
fn mbstowcs_sbcs<M: Mem>(
    m: &mut Memory<'_, M>,
    cp: &CodePage,
    at: u32,
    input: &ReadPage,
) -> Result<u16> {
    let byte = input.byte(m, at)?;
    cp.mb_page.word(m, cp.mb + u32::from(byte) * 2)
}
// Wine mbstowcs_dbcs, flags zero, including a lone lead byte and lead + NUL.
#[inline(always)]
fn mbstowcs_dbcs<M: Mem>(
    m: &mut Memory<'_, M>,
    cp: &CodePage,
    at: u32,
    len: u32,
    input: &ReadPage,
) -> Result<(u16, u32)> {
    let byte = input.byte(m, at)?;
    let off = cp.offset_page.word(m, cp.offsets + u32::from(byte) * 2)?;
    if off != 0 {
        if len > 1 {
            let next = input.byte(m, at + 1)?;
            if next != 0 {
                return Ok((
                    cp.dbcs_page
                        .word(m, cp.offsets + (u32::from(off) + u32::from(next)) * 2)?,
                    2,
                ));
            }
        }
        Ok((cp.default, 1))
    } else {
        Ok((cp.mb_page.word(m, cp.mb + u32::from(byte) * 2)?, 1))
    }
}

// Wine ntdll/locale_private.h decode_utf8_char. Invalid sequences consume
// exactly the prefix Wine consumed before producing U+FFFD.
fn decode_utf8_char<M: Mem>(m: &mut Memory<'_, M>, at: u32, available: u32) -> Result<(u32, u32)> {
    let ch = m.byte(at)?;
    if ch < 0x80 {
        return Ok((u32::from(ch), 1));
    }
    let len = match ch {
        0xc2..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf4 => 3,
        _ => 0,
    };
    let mut res = u32::from(ch & [0x7f, 0x1f, 0x0f, 7][len as usize]);
    if available < len + 1 {
        return Ok((0xfffd, available));
    }
    let mut used = 1;
    if len == 3 {
        let ch = m.byte(at + used)? ^ 0x80;
        if ch >= 0x40 {
            return Ok((0xfffd, used));
        }
        res = (res << 6) | u32::from(ch);
        used += 1;
        if res < 0x10 {
            return Ok((0xfffd, used));
        }
    }
    if len >= 2 {
        let ch = m.byte(at + used)? ^ 0x80;
        if ch >= 0x40 {
            return Ok((0xfffd, used));
        }
        res = (res << 6) | u32::from(ch);
        if res >= (0x110000 >> 6) {
            return Ok((0xfffd, used));
        }
        used += 1;
        if res < 0x20 || ((0xd800 >> 6)..=(0xdfff >> 6)).contains(&res) {
            return Ok((0xfffd, used));
        }
    }
    if len >= 1 {
        let ch = m.byte(at + used)? ^ 0x80;
        if ch >= 0x40 {
            return Ok((0xfffd, used));
        }
        res = (res << 6) | u32::from(ch);
        used += 1;
        if res >= 0x80 {
            return Ok((res, used));
        }
    }
    Ok((0xfffd, used))
}

// Wine compare_locale_names; locale names fold ASCII and equate '_' with '-'.
fn compare_locale_names<M: Mem>(m: &mut Memory<'_, M>, a: u32, b: u32) -> Result<i32> {
    fn fold(ch: u16) -> u16 {
        match ch {
            0x61..=0x7a => ch - 0x20,
            0x5f => 0x2d,
            _ => ch,
        }
    }
    let mut pos = 0;
    loop {
        let (at, bt) = (a.wrapping_add(pos), b.wrapping_add(pos));
        let (left, right) = (m.page(at >> 12)?, m.page(bt >> 12)?);
        let count = ((4096 - (at & 4095)).min(4096 - (bt & 4095)) / 2).max(1);
        for i in 0..count {
            let (x, y) = if (at & 4095) == 4095
                || (bt & 4095) == 4095
                || (!M::WINDOW && (left.is_null() || right.is_null()))
            {
                (
                    m.word(at.wrapping_add(i * 2))?,
                    m.word(bt.wrapping_add(i * 2))?,
                )
            } else {
                // SAFETY: every complete wide unit lies in its granted page.
                unsafe {
                    (
                        u16::from_le(
                            left.add(((at & 4095) + i * 2) as usize)
                                .cast::<u16>()
                                .read_unaligned(),
                        ),
                        u16::from_le(
                            right
                                .add(((bt & 4095) + i * 2) as usize)
                                .cast::<u16>()
                                .read_unaligned(),
                        ),
                    )
                }
            };
            let (x, y) = (fold(x), fold(y));
            if x == 0 || x != y {
                return Ok(i32::from(x) - i32::from(y));
            }
        }
        pos += count * 2;
    }
}
// Wine find_lcid_entry.
fn find_lcid_entry<M: Mem>(m: &mut Memory<'_, M>, base: u32, lcid: u32) -> Result<Option<u32>> {
    let header = m.dword(base + kb::locale_table)?;
    let index = m.dword(base + kb::lcids_index)?;
    let (mut min, mut max) = (
        0i32,
        i32::from(m.word(header + locale_header::nb_lcids)?) - 1,
    );
    while min <= max {
        let pos = (min + max) / 2;
        let at = index + pos as u32 * lcid_index::SIZE;
        match lcid.cmp(&m.dword(at + lcid_index::id)?) {
            std::cmp::Ordering::Less => max = pos - 1,
            std::cmp::Ordering::Greater => min = pos + 1,
            std::cmp::Ordering::Equal => return Ok(Some(at)),
        }
    }
    Ok(None)
}
// LCID resolution passes a name from Wine's own string pool. Its unique
// shared offset identifies the same LCNAME entry as Wine's lexical search;
// the pinned-data test checks uniqueness under compare_locale_names. Scan the
// loaded records in place, without building a reverse index or copying names.
fn find_lcname_offset<M: Mem>(
    m: &mut Memory<'_, M>,
    index: u32,
    count: u32,
    offset: u16,
) -> Result<Option<u32>> {
    let mut pos = 0;
    while pos < count {
        let at = index + pos * lcname_index::SIZE;
        let pointer = m.page(at >> 12)?;
        let room = ((4096 - (at & 4095)) / lcname_index::SIZE).min(count - pos);
        if room == 0 || (!M::WINDOW && pointer.is_null()) {
            if m.word(at + lcname_index::name)? == offset {
                return Ok(Some(pos));
            }
            pos += 1;
            continue;
        }
        // SAFETY: these complete records are in a retained readable page.
        let pointer = unsafe { pointer.add((at & 4095) as usize) };
        let mut i = 0;
        #[cfg(target_arch = "aarch64")]
        while i + 8 <= room {
            use std::arch::aarch64::*;
            // SAFETY: NEON is baseline AArch64; ld4 reads eight eight-byte
            // records in this checked range and selects their name fields.
            let (low, high) = unsafe {
                let names = vld4q_u16(pointer.add((i * 8) as usize).cast::<u16>()).0;
                let mask = vreinterpretq_u64_u16(vceqq_u16(names, vdupq_n_u16(offset)));
                (vgetq_lane_u64::<0>(mask), vgetq_lane_u64::<1>(mask))
            };
            if low != 0 {
                return Ok(Some(pos + i + low.trailing_zeros() / 16));
            }
            if high != 0 {
                return Ok(Some(pos + i + 4 + high.trailing_zeros() / 16));
            }
            i += 8;
        }
        while i < room {
            // SAFETY: the name word is inside this checked record.
            let name = unsafe {
                u16::from_le(pointer.add((i * 8) as usize).cast::<u16>().read_unaligned())
            };
            if name == offset {
                return Ok(Some(pos + i));
            }
            i += 1;
        }
        pos += room;
    }
    Ok(None)
}
// Wine find_lcname_entry.
fn find_lcname_entry<M: Mem>(
    m: &mut Memory<'_, M>,
    base: u32,
    name: u32,
) -> Result<Option<(u32, u32)>> {
    let header = m.dword(base + kb::locale_table)?;
    let index = m.dword(base + kb::lcnames_index)?;
    let strings = m.dword(base + kb::locale_strings)?;
    let count = u32::from(m.word(header + locale_header::nb_lcnames)?);
    let delta = name.wrapping_sub(strings.wrapping_add(2));
    if delta <= u32::from(u16::MAX) * 2
        && delta & 1 == 0
        && let Some(pos) = find_lcname_offset(m, index, count, (delta / 2) as u16)?
    {
        return Ok(Some((index + pos * lcname_index::SIZE, pos)));
    }
    // Other pointers, including equal text stored at a different pool offset,
    // still need Wine's content comparison rather than pointer identity.
    let (mut min, mut max) = (0i32, count as i32 - 1);
    while min <= max {
        let pos = (min + max) / 2;
        let at = index + pos as u32 * lcname_index::SIZE;
        let string = strings + 2 * (u32::from(m.word(at + lcname_index::name)?) + 1);
        let res = compare_locale_names(m, name, string)?;
        if res < 0 {
            max = pos - 1;
        } else if res > 0 {
            min = pos + 1;
        } else {
            return Ok(Some((at, pos as u32)));
        }
    }
    Ok(None)
}
// Wine get_locale_data.
fn get_locale_data<M: Mem>(m: &mut Memory<'_, M>, base: u32, index: u16) -> Result<u32> {
    let header = m.dword(base + kb::locale_table)?;
    let offset = m.dword(header + locale_header::locales_offset)?;
    let size = m.word(header + locale_header::locale_size)?;
    Ok(header + offset + u32::from(index) * u32::from(size))
}
// Wine NlsValidateLocale, flags zero (get_lcid_codepage's only caller here).
fn nls_validate_locale<M: Mem>(m: &mut Memory<'_, M>, base: u32, lcid: u32) -> Result<Option<u32>> {
    match lcid {
        0x800 => return Ok(Some(m.dword(base + kb::system_locale)?)),
        0 | 0x400 | 0xc00 | 0x1000 | 0x1400 => return Ok(Some(m.dword(base + kb::user_locale)?)),
        _ => (),
    }
    let Some(entry) = find_lcid_entry(m, base, lcid)? else {
        return Ok(None);
    };
    let index = m.word(entry + lcid_index::idx)?;
    let mut locale = get_locale_data(m, base, index)?;
    if m.word(locale + locale_data::inotneutral)? == 0 {
        let strings = m.dword(base + kb::locale_strings)?;
        let name = strings + 2 * (m.dword(locale + locale_data::ssortlocale)? + 1);
        if let Some((entry, _)) = find_lcname_entry(m, base, name)? {
            let index = m.word(entry + lcname_index::idx)?;
            locale = get_locale_data(m, base, index)?;
        }
    }
    Ok(Some(locale))
}
// Wine get_lcid_codepage.
fn get_lcid_codepage<M: Mem>(
    m: &mut Memory<'_, M>,
    base: u32,
    lcid: u32,
    flags: u32,
) -> Result<u32> {
    let mut code = u32::from(m.word(base + kb::ansi_cpinfo + cp::CodePage)?);
    if flags & USE_CP_ACP == 0
        && lcid != m.dword(base + kb::system_lcid)?
        && let Some(locale) = nls_validate_locale(m, base, lcid)?
    {
        code = u32::from(m.word(locale + locale_data::idefaultansicodepage)?);
    }
    Ok(code)
}
// Wine get_language_sort. A cache miss requests registry data and writes
// locale_sorts; let Wine own that entire operation before any guest write.
fn get_language_sort<M: Mem>(
    m: &mut Memory<'_, M>,
    base: u32,
    mut name: u32,
) -> Result<Option<u32>> {
    if name == 0 {
        let current = m.dword(base + kb::current_locale_sort)?;
        if current != 0 {
            return Ok(Some(current));
        }
        let locale = m.dword(base + kb::user_locale)?;
        let strings = m.dword(base + kb::locale_strings)?;
        name = strings + 2 * (m.dword(locale + locale_data::sname)? + 1);
    } else if m.word(name)? == 0x21 {
        const SYSTEM: &[u8] = b"!x-sys-default-locale";
        let mut equal = true;
        for (i, ch) in SYSTEM.iter().copied().chain([0]).enumerate() {
            let actual = m.word(name + 2 * i as u32)?;
            let actual = if (0x41..=0x5a).contains(&actual) {
                actual + 32
            } else if actual == 0x5f {
                0x2d
            } else {
                actual
            };
            if actual != u16::from(ch) {
                equal = false;
                break;
            }
        }
        if equal {
            let locale = m.dword(base + kb::system_locale)?;
            let strings = m.dword(base + kb::locale_strings)?;
            name = strings + 2 * (m.dword(locale + locale_data::sname)? + 1);
        }
    }
    let Some((_, index)) = find_lcname_entry(m, base, name)? else {
        return Ok(None);
    };
    let sorts = m.dword(base + kb::locale_sorts)?;
    let sort = m.dword(sorts + 4 * index)?;
    if sort == 0 {
        return Err(Stop::Guest.into());
    }
    Ok(Some(sort))
}
// Wine CompareStringW's LCID switch. These defaults deliberately all select
// the user sort, including LOCALE_SYSTEM_DEFAULT and the system LCID.
fn resolve_lcid<M: Mem>(m: &mut Memory<'_, M>, base: u32, lcid: u32) -> Result<Option<u32>> {
    let name = match lcid {
        0 | 0x400 | 0x800 | 0xc00 | 0x1000 | 0x1400 => 0,
        _ if lcid == m.dword(base + kb::user_lcid)?
            || lcid == m.dword(base + kb::system_lcid)? =>
        {
            0
        }
        _ => {
            let Some(entry) = find_lcid_entry(m, base, lcid)? else {
                return Ok(None);
            };
            let strings = m.dword(base + kb::locale_strings)?;
            strings + 2 * (u32::from(m.word(entry + lcid_index::name)?) + 1)
        }
    };
    Ok(Some(name))
}

#[derive(Clone, Copy)]
struct SortId {
    flags: u32,
    compr: u32,
    except: u32,
}
impl SortId {
    fn load<M: Mem>(m: &mut Memory<'_, M>, at: u32, flags: u32) -> Result<Self> {
        let fields = m.read::<16>(at + sortguid::flags)?;
        let field = |offset: u32| {
            u32::from_le_bytes(
                fields
                    [(offset - sortguid::flags) as usize..(offset - sortguid::flags) as usize + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        let mut except = field(sortguid::except);
        let ling_except = field(sortguid::ling_except);
        if flags & LING_CASING != 0 && except != 0 && ling_except != 0 {
            except = ling_except;
        }
        Ok(Self {
            flags: field(sortguid::flags),
            compr: field(sortguid::compr),
            except,
        })
    }
}
struct Sort {
    index_page: ReadPage,
    key_page: ReadPage,
    keys: u32,
    at: u32,
}
impl Sort {
    fn load<M: Mem>(m: &mut Memory<'_, M>, base: u32) -> Result<Self> {
        let at = base + kb::sort;
        Ok(Self {
            index_page: ReadPage::new(),
            key_page: ReadPage::new(),
            keys: m.dword(at + sort::keys)?,
            at,
        })
    }
}
#[derive(Clone, Copy)]
struct Weights {
    primary: u8,
    script: u8,
    diacritic: u8,
    case: u8,
}
impl From<u32> for Weights {
    fn from(value: u32) -> Self {
        let [primary, script, diacritic, case] = value.to_le_bytes();
        Self {
            primary,
            script,
            diacritic,
            case,
        }
    }
}
// Wine get_char_weights.
#[inline(always)]
fn get_char_weights<M: Mem>(
    m: &mut Memory<'_, M>,
    sort: &Sort,
    ch: u16,
    except: u32,
) -> Result<Weights> {
    let index = if except == 0 {
        u32::from(ch)
    } else {
        sort.index_page
            .dword(m, sort.keys + 4 * (except + u32::from(ch >> 8)))?
            + u32::from(ch & 0xff)
    };
    Ok(sort.key_page.dword(m, sort.keys + index * 4)?.into())
}

// Wine get_digit_zero_char.
fn get_digit_zero_char(ch: u16) -> u16 {
    const ZEROES: [u16; 28] = [
        0x0030, 0x0660, 0x06f0, 0x0966, 0x09e6, 0x0a66, 0x0ae6, 0x0b66, 0x0be6, 0x0c66, 0x0ce6,
        0x0d66, 0x0e50, 0x0ed0, 0x0f20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x1bb0, 0x1c40,
        0x1c50, 0xa620, 0xa8d0, 0xa900, 0xaa50, 0xff10,
    ];
    let pos = ZEROES.partition_point(|zero| *zero <= ch);
    if pos != 0 && ch - ZEROES[pos - 1] <= 9 {
        ZEROES[pos - 1]
    } else {
        0
    }
}
fn case_mask(flags: u32) -> u8 {
    let mut mask = 0x3f;
    if flags & IGNORECASE != 0 {
        mask &= !(0x10 | 8);
    }
    if flags & IGNOREWIDTH != 0 {
        mask &= !1;
    }
    if flags & IGNOREKANATYPE != 0 {
        mask &= !0x20;
    }
    mask
}
// Wine casemap (also ntdll's casemap for RtlCompareUnicodeStrings).
fn casemap<M: Mem>(m: &mut Memory<'_, M>, table: u32, ch: u16) -> Result<u16> {
    let off = u32::from(m.word(table + u32::from(ch >> 8) * 2)?);
    let off = u32::from(m.word(table + (off + u32::from((ch >> 4) & 15)) * 2)?);
    Ok(ch.wrapping_add(m.word(table + (off + u32::from(ch & 15)) * 2)?))
}
// Wine CompareStringOrdinal / ntdll RtlCompareUnicodeStrings.
fn compare_string_ordinal<M: Mem>(
    m: &mut Memory<'_, M>,
    nt: u32,
    a: &Source,
    b: &Source,
    ignore_case: bool,
) -> Result<u32> {
    // Equal code units remain equal under Wine's ordinal case mapping.
    // The caller has already checked pointers and normalized both lengths.
    if stream::same_bytes(m, a.at, a.len * 2, b.at, b.len * 2, 2)? {
        return Ok(2);
    }
    let table = if ignore_case {
        m.dword(nt + NT_NLS_INFO + NT_UPPER_CASE)?
    } else {
        0
    };
    for i in 0..a.len.min(b.len) {
        let (mut x, mut y) = (a.get(m, i)?, b.get(m, i)?);
        if ignore_case {
            if table != 0 {
                x = casemap(m, table, x)?;
                y = casemap(m, table, y)?;
            } else {
                if (0x61..=0x7a).contains(&x) {
                    x -= 32;
                }
                if (0x61..=0x7a).contains(&y) {
                    y -= 32;
                }
            }
        }
        if x != y {
            return Ok((x.cmp(&y) as i32 + 2) as u32);
        }
    }
    Ok((a.len.cmp(&b.len) as i32 + 2) as u32)
}

// Wine CompareStringA/W/Ex and lstrcmp[A/W]/lstrcmpi[A/W]. KIND selects the
// export, retaining each API's validation and last-error ordering.
fn compare<M: Mem>(m: &mut Memory<'_, M>, args: [u32; 7], kind: u32) -> Result<u32> {
    let [stack, thread, base, nt, ..] = args;
    let mut params = [0u32; 6];
    if kind >= 4 {
        let bytes = m.read::<8>(stack)?;
        for (slot, bytes) in params.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *slot = u32::from_le_bytes(*bytes);
        }
    } else if kind == 3 {
        let bytes = m.read::<20>(stack)?;
        for (slot, bytes) in params.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *slot = u32::from_le_bytes(*bytes);
        }
    } else {
        let bytes = m.read::<24>(stack)?;
        for (slot, bytes) in params.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *slot = u32::from_le_bytes(*bytes);
        }
    }
    let arg = |n: usize| params[n];
    let lstrcmp = kind >= 4;
    let ansi = kind == 0 || kind == 4 || kind == 6;
    let (locale, flags, s1, n1, s2, n2) = if lstrcmp {
        let (s1, s2) = (arg(0), arg(1));
        if s1 == 0 || s2 == 0 {
            return Ok((i32::from(s1 != 0) - i32::from(s2 != 0)) as u32);
        }
        let locale = m.dword(thread + teb::CurrentLocale)?;
        // GetThreadLocale publishes the user's LCID when CurrentLocale is zero.
        if locale == 0 {
            return Err(Stop::Guest.into());
        }
        (
            locale,
            if ansi { USE_CP_ACP } else { 0 } | u32::from(kind >= 6),
            s1,
            u32::MAX,
            s2,
            u32::MAX,
        )
    } else if kind == 3 {
        let (s1, n1, s2, n2, ignore) = (arg(0), arg(1), arg(2), arg(3), arg(4));
        if s1 == 0 || s2 == 0 {
            return m.error(thread, 87);
        }
        let a = Source::wide(m, s1, n1)?;
        let b = Source::wide(m, s2, n2)?;
        return compare_string_ordinal(m, nt, &a, &b, ignore as u8 != 0);
    } else {
        (arg(0), arg(1), arg(2), arg(3), arg(4), arg(5))
    };
    let width = if ansi { 1 } else { 2 };
    if ansi {
        if s1 == 0 || s2 == 0 {
            return m.error(thread, 87);
        }
        if flags & DIGITSASNUMBERS != 0 {
            return m.error(thread, 1004);
        }
    }
    let raw_length = |len: u32| {
        if (len as i32) < 0 {
            u32::MAX
        } else {
            len * width
        }
    };
    let (mut bytes1, mut bytes2) = (raw_length(n1), raw_length(n2));
    // Mixed explicit/terminated lengths must first become comparable lengths.
    if ansi && (bytes1 == u32::MAX) != (bytes2 == u32::MAX) {
        if bytes1 == u32::MAX {
            bytes1 = terminated(m, s1, width)? * width;
        }
        if bytes2 == u32::MAX {
            bytes2 = terminated(m, s2, width)? * width;
        }
    }
    let mut identity = false;
    if ansi {
        identity = stream::same_bytes(m, s1, bytes1, s2, bytes2, width)?;
        if !identity {
            if bytes1 == u32::MAX {
                bytes1 = terminated(m, s1, 1)?;
            }
            if bytes2 == u32::MAX {
                bytes2 = terminated(m, s2, 1)?;
            }
            stream::validate(m, s1, bytes1)?;
            stream::validate(m, s2, bytes2)?;
        }
    }
    let name = if kind == 2 {
        locale
    } else {
        let Some(name) = resolve_lcid(m, base, locale)? else {
            return m.error(thread, 87).map(|value| {
                if lstrcmp {
                    value.wrapping_sub(2)
                } else {
                    value
                }
            });
        };
        name
    };
    if flags & !SUPPORTED != 0 {
        return m.error(thread, 1004);
    }
    let Some(at) = get_language_sort(m, base, name)? else {
        return m.error(thread, 87).map(|value| {
            if lstrcmp {
                value.wrapping_sub(2)
            } else {
                value
            }
        });
    };
    if s1 == 0 || s2 == 0 {
        return m.error(thread, 87);
    }
    if !ansi {
        if (bytes1 == u32::MAX) != (bytes2 == u32::MAX) {
            if bytes1 == u32::MAX {
                bytes1 = terminated(m, s1, width)? * width;
            }
            if bytes2 == u32::MAX {
                bytes2 = terminated(m, s2, width)? * width;
            }
        }
        identity = stream::same_bytes(m, s1, bytes1, s2, bytes2, width)?;
    }
    if identity {
        // Identical ANSI bytes yield identical conversions in the same code
        // page. ACP is already loaded; other pages still use Wine's lookup so
        // cold loads, owned locale locks and their last-error effects stay Wine's.
        if ansi
            && bytes1 != 0
            && (bytes1 != u32::MAX || m.byte(s1)? != 0)
            && flags & USE_CP_ACP == 0
            && locale != m.dword(base + kb::system_lcid)?
        {
            let code = get_lcid_codepage(m, base, locale, flags)?;
            get_codepage_table(m, base, code)?;
        }
        return Ok(if lstrcmp { 0 } else { 2 });
    }
    let cp = if ansi && (bytes1 != 0 || bytes2 != 0) {
        let code = get_lcid_codepage(m, base, locale, flags)?;
        Some(get_codepage_table(m, base, code)?)
    } else {
        None
    };
    let sort = Sort::load(m, base)?;
    let id = SortId::load(m, at, flags)?;
    let texts = [
        stream::Text::new(s1, bytes1, cp.as_ref()),
        stream::Text::new(s2, bytes2, cp.as_ref()),
    ];
    // Encoding is fixed for a comparison, so each stream uses one decoder
    // specialization instead of selecting wide/SBCS/DBCS/UTF-8 per character.
    let order = if !ansi {
        stream::compare::<M, 0>(m, &sort, id, flags, texts)?
    } else if cp.as_ref().unwrap().code == 65001 {
        stream::compare::<M, 3>(m, &sort, id, flags, texts)?
    } else if cp.as_ref().unwrap().offsets != 0 {
        stream::compare::<M, 2>(m, &sort, id, flags, texts)?
    } else {
        stream::compare::<M, 1>(m, &sort, id, flags, texts)?
    };
    let value = order.signum() + 2;
    Ok(if lstrcmp {
        (value - 2) as u32
    } else {
        value as u32
    })
}

fn run<M: Mem>(mem: &mut M, args: [u32; 7], kind: u32) -> std::result::Result<Ret, Stop> {
    let mut memory = Memory::new(mem, None);
    match compare(&mut memory, args, kind) {
        Ok(value) => Ok(value.into()),
        Err(Error::Stop(stop)) => Err(stop),
        Err(Error::Pages) => run_many_pages(memory.mem, args, kind, memory.pages),
    }
}
#[inline(never)]
fn run_many_pages<M: Mem>(
    mem: &mut M,
    args: [u32; 7],
    kind: u32,
    pages: [Option<Page>; 32],
) -> std::result::Result<Ret, Stop> {
    let mut grants = Grants {
        initialized: [0; 256],
        bits: [MaybeUninit::uninit(); 16384],
        bases: [std::ptr::null(); 2],
    };
    for page in pages.iter().flatten() {
        let block = page.number as usize / 64;
        let bit = 1u128 << (page.number % 64);
        if grants.initialized[block / 64] & (1 << (block % 64)) == 0 {
            grants.bits[block].write(0);
            grants.initialized[block / 64] |= 1 << (block % 64);
        }
        let base = page.pointer.wrapping_sub((page.number as usize) << 12);
        if grants.bases[0].is_null() {
            grants.bases[0] = base;
        }
        // SAFETY: the initialization bit above covers this word.
        let bits = unsafe { grants.bits[block].assume_init_mut() };
        *bits |= bit;
        if base != grants.bases[0] {
            grants.bases[1] = base;
            *bits |= bit << 64;
        }
    }
    let mut memory = Memory::new(mem, Some(&mut grants));
    memory.pages = pages;
    match compare(&mut memory, args, kind) {
        Ok(value) => Ok(value.into()),
        Err(Error::Stop(stop)) => Err(stop),
        Err(Error::Pages) => unreachable!(),
    }
}
fn compare_a<M: Mem>(m: &mut M, args: [u32; 7]) -> std::result::Result<Ret, Stop> {
    run(m, args, 0)
}
fn compare_w<M: Mem>(m: &mut M, args: [u32; 7]) -> std::result::Result<Ret, Stop> {
    run(m, args, 1)
}
fn compare_ex<M: Mem>(m: &mut M, args: [u32; 7]) -> std::result::Result<Ret, Stop> {
    run(m, args, 2)
}
fn compare_ordinal<M: Mem>(m: &mut M, args: [u32; 7]) -> std::result::Result<Ret, Stop> {
    run(m, args, 3)
}
fn lstrcmp_a<M: Mem>(m: &mut M, args: [u32; 7]) -> std::result::Result<Ret, Stop> {
    run(m, args, 4)
}
fn lstrcmp_w<M: Mem>(m: &mut M, args: [u32; 7]) -> std::result::Result<Ret, Stop> {
    run(m, args, 5)
}
fn lstrcmpi_a<M: Mem>(m: &mut M, args: [u32; 7]) -> std::result::Result<Ret, Stop> {
    run(m, args, 6)
}
fn lstrcmpi_w<M: Mem>(m: &mut M, args: [u32; 7]) -> std::result::Result<Ret, Stop> {
    run(m, args, 7)
}
const INPUTS: &[Input] = &[Input::Args, Input::Teb, Input::Const(0), Input::Const(0)];
adv_cpu::body!(COMPARE_A = compare_a, inputs: INPUTS, outputs: EAX, pop: 24);
adv_cpu::body!(COMPARE_W = compare_w, inputs: INPUTS, outputs: EAX, pop: 24);
adv_cpu::body!(COMPARE_EX = compare_ex, inputs: INPUTS, outputs: EAX, pop: 36);
adv_cpu::body!(COMPARE_ORDINAL = compare_ordinal, inputs: INPUTS, outputs: EAX, pop: 20);
adv_cpu::body!(LSTRCMP_A = lstrcmp_a, inputs: INPUTS, outputs: EAX, pop: 8);
adv_cpu::body!(LSTRCMP_W = lstrcmp_w, inputs: INPUTS, outputs: EAX, pop: 8);
adv_cpu::body!(LSTRCMPI_A = lstrcmpi_a, inputs: INPUTS, outputs: EAX, pop: 8);
adv_cpu::body!(LSTRCMPI_W = lstrcmpi_w, inputs: INPUTS, outputs: EAX, pop: 8);
pub(super) fn bodies() -> impl Iterator<Item = super::Body> {
    [
        ("CompareStringA", 6, COMPARE_A),
        ("CompareStringW", 6, COMPARE_W),
        ("CompareStringEx", 9, COMPARE_EX),
        ("CompareStringOrdinal", 5, COMPARE_ORDINAL),
        ("lstrcmpA", 2, LSTRCMP_A),
        ("lstrcmpW", 2, LSTRCMP_W),
        ("lstrcmpiA", 2, LSTRCMPI_A),
        ("lstrcmpiW", 2, LSTRCMPI_W),
    ]
    .into_iter()
    .map(|(name, argc, direct)| super::Body {
        name: format!("kernelbase.dll!{name}").into(),
        conv: adv_machine::Conv::Stdcall { argc },
        body: None,
        handler: super::resume_original,
        direct: Some(direct),
    })
}

#[cfg(test)]
pub(super) fn check_comparison_memory(cpu: &mut dyn adv_cpu::backend::Guest) {
    use super::tests::{ALLOCATIONS, COUNTING};
    use adv_cpu::backend::{GuestExt, Prot};
    use adv_cpu::body::{BodyContext, GuestMem, Scratch};
    const BASE: u32 = 0x1000000;
    cpu.map(BASE, 0x200000, Prot::RW).unwrap();
    let (thread, stack, guid, keys, mb) = (
        BASE + 0x1000,
        BASE + 0x3000,
        BASE + 0x130000,
        BASE + 0x140000,
        BASE + 0x131000,
    );
    for (at, value) in [
        (BASE + kb::current_locale_sort, guid),
        (BASE + kb::user_lcid, 0x411),
        (BASE + kb::system_lcid, 0x411),
        (BASE + kb::sort + sort::keys, keys),
        (BASE + kb::ansi_cpinfo + cp::MultiByteTable, mb),
        (thread + teb::CurrentLocale, 0x411),
    ] {
        cpu.write_u32(at, value).unwrap();
    }
    cpu.write(BASE + kb::ansi_cpinfo, &1252u16.to_le_bytes())
        .unwrap();
    for ch in 0..=u16::MAX {
        cpu.write_u32(
            keys + u32::from(ch) * 4,
            u32::from_le_bytes([ch as u8, 14, 2, 2]),
        )
        .unwrap();
    }
    for ch in 0..256u32 {
        cpu.write(mb + ch * 2, &(ch as u16).to_le_bytes()).unwrap();
    }
    let mut classes = vec![0; 1 << 20];
    classes[(BASE >> 12) as usize..((BASE + 0x200000) >> 12) as usize].fill(3);
    let pointer = cpu
        .shared_bytes(BASE, 0x200000)
        .unwrap()
        .as_ptr()
        .cast_mut()
        .wrapping_sub(BASE as usize);
    let context = BodyContext {
        window: pointer,
        alias: pointer,
        classes: classes.as_ptr(),
        ..BodyContext::default()
    };
    let mut scratch = Scratch::default();
    for many_pages in [false, true] {
        for body in bodies() {
            let a = BASE + 0x100fff;
            let b = BASE + 0x110fff;
            let ansi = body.name.ends_with('A');
            let width = if ansi { 1 } else { 2 };
            for i in 0..700 {
                let ch = if many_pages && !ansi {
                    (i * 256) as u16
                } else {
                    0x41
                };
                if ansi {
                    cpu.write(a + i, &[ch as u8]).unwrap();
                    cpu.write(b + i, &[ch as u8]).unwrap();
                } else {
                    cpu.write(a + i * 2, &ch.to_le_bytes()).unwrap();
                    cpu.write(b + i * 2, &ch.to_le_bytes()).unwrap();
                }
            }
            cpu.write(a + 700 * width, &[0, 0]).unwrap();
            cpu.write(b + 700 * width, &[0, 0]).unwrap();
            if many_pages && !ansi {
                cpu.write(b + 699 * 2, &((699u32 * 256) as u16 + 1).to_le_bytes())
                    .unwrap();
            }
            let lstrcmp = body.name.contains("!lstrcmp");
            let args: Vec<u32> = if lstrcmp {
                vec![a, b]
            } else if body.name.ends_with("Ordinal") {
                vec![a, 700, b, 700, 0]
            } else {
                vec![
                    if body.name.ends_with("Ex") { 0 } else { 0x411 },
                    0,
                    a,
                    700,
                    b,
                    700,
                    0,
                    0,
                    0,
                ]
            };
            for (i, arg) in args.iter().enumerate() {
                cpu.write_u32(stack + i as u32 * 4, *arg).unwrap();
            }
            let expected = if lstrcmp {
                0
            } else if many_pages && !ansi {
                1
            } else {
                2
            };
            let body = body.direct.unwrap();
            let inputs = [stack, thread, BASE, BASE, 0, 0, 0];
            for _ in 0..2 {
                cpu.write_u32(thread + teb::LastErrorValue, 0x13579)
                    .unwrap();
                ALLOCATIONS.set(0);
                COUNTING.set(true);
                let full = (body.full)(&mut GuestMem::new(cpu, &mut scratch), inputs);
                // SAFETY: context owns the live mapped bytes and class table.
                let window =
                    unsafe { (body.direct.call)(&context, stack, thread, BASE, BASE, 0, 0, 0) };
                COUNTING.set(false);
                assert_eq!(ALLOCATIONS.get(), 0);
                assert_eq!(full.unwrap().value, expected);
                assert_eq!(window.extra, 0);
                assert_eq!(window.value, expected);
                assert_eq!(cpu.read_u32(thread + teb::LastErrorValue).unwrap(), 0x13579);
            }
        }
    }
    let a = BASE + 0x100fff;
    let inputs = [stack, thread, BASE, BASE, 0, 0, 0];
    cpu.write_u32(stack, a).unwrap();
    cpu.write_u32(stack + 4, a).unwrap();
    cpu.write_u32(thread + teb::CurrentLocale, 0).unwrap();
    assert_eq!(
        (LSTRCMP_A.full)(&mut GuestMem::new(cpu, &mut scratch), inputs),
        Err(Stop::Guest)
    );
    assert_eq!(cpu.read_u32(thread + teb::CurrentLocale).unwrap(), 0);
    let locale = BASE + 0x132000;
    cpu.write_u32(BASE + kb::user_locale, locale).unwrap();
    cpu.write(
        locale + locale_data::idefaultansicodepage,
        &932u16.to_le_bytes(),
    )
    .unwrap();
    cpu.write_u32(BASE + kb::nb_codepages, 1).unwrap();
    cpu.write(BASE + kb::codepages, &932u16.to_le_bytes())
        .unwrap();
    cpu.write_u32(BASE + kb::codepages + cp::MultiByteTable, mb)
        .unwrap();
    for (i, arg) in [0x400, 0, a, 1, a, 1].iter().enumerate() {
        cpu.write_u32(stack + 4 * i as u32, *arg).unwrap();
    }
    cpu.write_u32(BASE + kb::locale_section + critical::LockCount, 0)
        .unwrap();
    assert_eq!(
        (COMPARE_A.full)(&mut GuestMem::new(cpu, &mut scratch), inputs),
        Err(Stop::Guest)
    );
    cpu.write_u32(BASE + kb::locale_section + critical::LockCount, u32::MAX)
        .unwrap();
    assert_eq!(
        (COMPARE_A.full)(&mut GuestMem::new(cpu, &mut scratch), inputs)
            .unwrap()
            .value,
        2
    );
    cpu.write(
        locale + locale_data::idefaultansicodepage,
        &850u16.to_le_bytes(),
    )
    .unwrap();
    assert_eq!(
        (COMPARE_A.full)(&mut GuestMem::new(cpu, &mut scratch), inputs),
        Err(Stop::Guest)
    );
    assert_eq!(cpu.read_u32(thread + teb::LastErrorValue).unwrap(), 0x13579);
    for (i, arg) in [a, 1, a, 1, 0].iter().enumerate() {
        cpu.write_u32(stack + 4 * i as u32, *arg).unwrap();
    }
    cpu.protect(a & !0xfff, 0x1000, Prot(0)).unwrap();
    assert_eq!(
        (COMPARE_ORDINAL.full)(&mut GuestMem::new(cpu, &mut scratch), inputs),
        Err(Stop::Fault {
            address: a,
            write: false
        })
    );
    // CompareStringA converts both complete spans before CompareStringW can
    // reject flags or decide an early primary difference.
    cpu.protect(a & !0xfff, 0x1000, Prot::RW).unwrap();
    cpu.protect((a & !0xfff) + 0x1000, 0x1000, Prot(0)).unwrap();
    let b = BASE + 0x110fff;
    cpu.write(a, b"A").unwrap();
    cpu.write(b, b"B").unwrap();
    for (i, arg) in [0x411, 0x40, a, 2, b, 2].iter().enumerate() {
        cpu.write_u32(stack + 4 * i as u32, *arg).unwrap();
    }
    assert_eq!(
        (COMPARE_A.full)(&mut GuestMem::new(cpu, &mut scratch), inputs),
        Err(Stop::Fault {
            address: a + 1,
            write: false
        })
    );
    assert_eq!(cpu.read_u32(thread + teb::LastErrorValue).unwrap(), 0x13579);
}
