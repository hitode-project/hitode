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
//! Comparison events and levels over Wine's loaded data, following Wine 11.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct Text<'a> {
    pub at: u32,
    pub len: u32,
    pub cp: Option<&'a CodePage>,
    skip: u32,
    primary: u32,
}
impl<'a> Text<'a> {
    pub(super) fn new(at: u32, len: u32, cp: Option<&'a CodePage>) -> Self {
        Self {
            at,
            len,
            cp,
            skip: 0,
            primary: 0,
        }
    }
}
#[derive(Clone, Copy)]
struct Mark {
    pos: u32,
    tail: Option<u16>,
}
#[derive(Clone)]
struct Cursor<'a, const CODE: u8> {
    text: Text<'a>,
    pos: u32,
    tail: Option<u16>,
    input: ReadPage,
}
impl<'a, const CODE: u8> Cursor<'a, CODE> {
    fn mark(&self) -> Mark {
        Mark {
            pos: self.pos,
            tail: self.tail,
        }
    }
    fn at_mark(&self, mark: Mark) -> Self {
        let mut cursor = self.clone();
        cursor.pos = mark.pos;
        cursor.tail = mark.tail;
        cursor
    }
    fn new(text: Text<'a>) -> Self {
        Self {
            text,
            pos: text.skip,
            tail: None,
            input: ReadPage::new(),
        }
    }
    #[inline(always)]
    fn next<M: Mem>(&mut self, m: &mut Memory<'_, M>) -> Result<Option<u16>> {
        if let Some(ch) = self.tail.take() {
            return Ok(Some(ch));
        }
        if self.pos == self.text.len {
            return Ok(None);
        }
        let at = self.text.at + self.pos;
        let ch = if CODE == 0 {
            let ch = self.input.word(m, at)?;
            if self.text.len == u32::MAX && ch == 0 {
                self.text.len = self.pos;
                return Ok(None);
            }
            self.pos += 2;
            ch
        } else {
            let cp = self.text.cp.unwrap();
            if CODE == 3 {
                let (ch, size) = decode_utf8_char(m, at, self.text.len - self.pos)?;
                self.pos += size;
                if ch > 0xffff {
                    self.tail = Some(0xdc00 | ((ch - 0x10000) & 0x3ff) as u16);
                    0xd800 | ((ch - 0x10000) >> 10) as u16
                } else {
                    ch as u16
                }
            } else if CODE == 2 {
                let (ch, size) = mbstowcs_dbcs(m, cp, at, self.text.len - self.pos, &self.input)?;
                self.pos += size;
                ch
            } else {
                self.pos += 1;
                mbstowcs_sbcs(m, cp, at, &self.input)?
            }
        };
        Ok(Some(ch))
    }
}

// Byte identity implies identical converted units and Wine weights. Callers
// validate Wine's arguments, locales and flags before accepting this result.
// Page-bounded loads cannot cross an inaccessible page after a terminator.
pub(super) fn same_bytes<M: Mem>(
    m: &mut Memory<'_, M>,
    a: u32,
    alen: u32,
    b: u32,
    blen: u32,
    width: u32,
) -> Result<bool> {
    if alen != blen {
        return Ok(false);
    }
    let terminated = alen == u32::MAX;
    let mut pos = 0u32;
    loop {
        if !terminated && pos == alen {
            return Ok(true);
        }
        let (at, bt) = (a.wrapping_add(pos), b.wrapping_add(pos));
        let room = (4096 - (at & 4095)).min(4096 - (bt & 4095));
        let count = if terminated {
            room
        } else {
            room.min(alen - pos)
        };
        let (ap, bp) = (m.page(at >> 12)?, m.page(bt >> 12)?);
        if (!M::WINDOW && (ap.is_null() || bp.is_null())) || count < width {
            let (av, bv) = if width == 1 {
                (u16::from(m.byte(at)?), u16::from(m.byte(bt)?))
            } else {
                (m.word(at)?, m.word(bt)?)
            };
            if av != bv {
                return Ok(false);
            }
            if terminated && av == 0 {
                return Ok(true);
            }
            pos += width;
            continue;
        }
        let count = count / width * width;
        // SAFETY: these ranges lie inside the retained readable pages.
        let (ap, bp) = unsafe { (ap.add((at & 4095) as usize), bp.add((bt & 4095) as usize)) };
        let mut i = 0usize;
        while i + 8 <= count as usize {
            // SAFETY: the complete eight-byte chunks are inside both ranges.
            let (x, y) = unsafe {
                (
                    u64::from_le(ap.add(i).cast::<u64>().read_unaligned()),
                    u64::from_le(bp.add(i).cast::<u64>().read_unaligned()),
                )
            };
            if terminated {
                let (one, high, shift) = if width == 1 {
                    (0x0101010101010101u64, 0x8080808080808080u64, 3)
                } else {
                    (0x0001000100010001u64, 0x8000800080008000u64, 4)
                };
                let zeroes = (x.wrapping_sub(one) & !x & high) | (y.wrapping_sub(one) & !y & high);
                if x != y {
                    return Ok(zeroes.trailing_zeros() >> shift < (x ^ y).trailing_zeros() >> shift);
                }
                if zeroes != 0 {
                    return Ok(true);
                }
            } else if x != y {
                return Ok(false);
            }
            i += 8;
        }
        while i < count as usize {
            // SAFETY: the complete byte or wide unit is inside both ranges.
            let (x, y) = unsafe {
                if width == 1 {
                    (u16::from(ap.add(i).read()), u16::from(bp.add(i).read()))
                } else {
                    (
                        u16::from_le(ap.add(i).cast::<u16>().read_unaligned()),
                        u16::from_le(bp.add(i).cast::<u16>().read_unaligned()),
                    )
                }
            };
            if x != y {
                return Ok(false);
            }
            if terminated && x == 0 {
                return Ok(true);
            }
            i += width as usize;
        }
        pos += count;
    }
}

// Wine converts the complete ANSI spans before comparing, even when the first
// primary difference is early. Validate every source page before streaming.
pub(super) fn validate<M: Mem>(m: &mut Memory<'_, M>, at: u32, len: u32) -> Result<()> {
    let mut pos = 0;
    while pos < len {
        let address = at.wrapping_add(pos);
        let count = (4096 - (address & 4095)).min(len - pos);
        if m.page(address >> 12)?.is_null() {
            m.byte(address)?;
            m.byte(address + count - 1)?;
        }
        pos += count;
    }
    Ok(())
}

#[derive(Default)]
struct Event {
    primary: [u8; 16],
    len: u8,
    dia: Option<u8>,
    case: Option<u8>,
    mark: bool,
    extra: Option<[u8; 4]>,
    special: Option<[u8; 4]>,
}
impl Event {
    fn primary(&mut self, bytes: &[u8]) {
        self.primary[..bytes.len()].copy_from_slice(bytes);
        self.len = bytes.len() as u8;
    }
}
struct First {
    ch: u16,
    raw: Weights,
    previous: Option<Weights>,
    start: Mark,
}
struct Events<'a, const CODE: u8> {
    cursor: Cursor<'a, CODE>,
    id: SortId,
    flags: u32,
    mask: u8,
    primary: u32,
    previous: Option<Weights>,
    expansion: Option<u16>,
    tail: Option<u16>,
    tables: [u32; 8],
}
impl<'a, const CODE: u8> Events<'a, CODE> {
    fn new(text: Text<'a>, id: SortId, flags: u32) -> Self {
        Self {
            cursor: Cursor::<CODE>::new(text),
            id,
            flags,
            mask: case_mask(flags),
            primary: text.primary,
            previous: None,
            expansion: None,
            tail: None,
            tables: [0; 8],
        }
    }
    fn remember(&mut self, raw: Weights) {
        // Wine's backward kana scan skips unsortables, nonspace marks and
        // repeat/prolongation marks, and sees the last *source* character.
        if raw.script > 1 && !(raw.script == 3 && raw.primary <= 1) {
            self.previous = Some(raw);
        }
    }
    // Wine append_normal_weights.
    #[inline(always)]
    fn append_normal_weights(&self, mut w: Weights, event: &mut Event) {
        event.primary(&[w.script, w.primary]);
        if (169..=175).contains(&w.script)
            || (self.id.flags & 1 != 0 && (192..=239).contains(&w.script))
        {
            event.primary(&[w.script, w.primary, w.diacritic]);
        } else {
            if w.script <= 41 && w.script != 40 && self.flags & LING_IGNOREDIACRITIC != 0 {
                w.diacritic = 2;
            }
            event.dia = Some(w.diacritic);
            if w.script <= 41 && w.script != 40 && self.flags & LING_IGNORECASE != 0 {
                w.case = 2;
            }
        }
        event.case = Some(w.case);
    }
    // Wine append_nonspace_weights / append_expansion_weights.
    fn append_nonspace_weights(&self, w: Weights, event: &mut Event) {
        event.mark = true;
        event.dia = Some(if self.flags & LING_IGNOREDIACRITIC != 0 {
            2
        } else {
            w.diacritic
        });
    }
    // Wine append_weights. Expanded characters are yielded separately, while
    // their source position and primary byte count stay in the same stream.
    #[inline(always)]
    fn next<M: Mem>(&mut self, m: &mut Memory<'_, M>, sort: &Sort) -> Result<Option<Event>> {
        if self.expansion.is_some() || self.tail.is_some() {
            return self.next_complex(m, sort, None);
        }
        let start = self.cursor.mark();
        let Some(ch) = self.cursor.next(m)? else {
            return Ok(None);
        };
        let raw = get_char_weights(m, sort, ch, self.id.except)?;
        let previous = self.previous;
        self.remember(raw);
        let simple = raw.case & 0xc0 == 0
            && (raw.script >= 14
                || raw.script == 13 && self.flags & DIGITSASNUMBERS == 0
                || raw.script == 3 && raw.primary > 1
                || (7..=12).contains(&raw.script)
                || raw.script == 0);
        if !simple {
            return self.next_complex(
                m,
                sort,
                Some(First {
                    ch,
                    raw,
                    previous,
                    start,
                }),
            );
        }
        // These Wine branches have no lookahead or backward dependency. Emit
        // their exact normal/kana/symbol weights without entering complex state.
        let mut w = raw;
        w.case &= self.mask;
        let mut event = Event::default();
        match w.script {
            0 => (),
            3 => {
                event.extra = Some([
                    0xc4 | (w.case & 2),
                    3,
                    0xc4 | (w.case & 0x20),
                    0xc4 | (w.case & 1),
                ]);
                w.script = 34;
                w.case = 2;
                self.append_normal_weights(w, &mut event);
            }
            7..=12 => {
                if self.flags & IGNORESYMBOLS == 0 {
                    event.primary(&[w.script, w.primary]);
                    event.dia = Some(w.diacritic);
                    event.case = Some(w.case);
                }
            }
            _ => self.append_normal_weights(w, &mut event),
        }
        Ok(Some(self.finish(event)))
    }
    #[inline(always)]
    fn finish(&mut self, mut event: Event) -> Event {
        if self.flags & IGNORENONSPACE != 0 {
            event.dia = None;
        }
        self.primary = self.primary.wrapping_add(u32::from(event.len));
        event
    }
    // Wine append_weights: expansion and context-dependent cases.
    #[inline(never)]
    fn next_complex<M: Mem>(
        &mut self,
        m: &mut Memory<'_, M>,
        sort: &Sort,
        mut first: Option<First>,
    ) -> Result<Option<Event>> {
        let mut event = Event::default();
        let (mut w, expanded, previous, start) = loop {
            if let Some(index) = self.expansion.take() {
                let expansions = m.dword(sort.at + sort::expansions)?;
                let a = m.word(expansions + u32::from(index) * expansion::SIZE + expansion::exp)?;
                self.tail = Some(
                    m.word(expansions + u32::from(index) * expansion::SIZE + expansion::exp + 2)?,
                );
                break (
                    get_char_weights(m, sort, a, self.id.except)?,
                    true,
                    None,
                    self.cursor.mark(),
                );
            }
            if let Some(ch) = self.tail.take() {
                let w = get_char_weights(m, sort, ch, self.id.except)?;
                if w.script == 2 {
                    self.expansion = Some((u16::from(w.case) << 8) | u16::from(w.diacritic));
                    continue;
                }
                break (w, true, None, self.cursor.mark());
            }
            let First {
                ch,
                raw,
                previous,
                start,
            } = if let Some(first) = first.take() {
                first
            } else {
                let start = self.cursor.mark();
                let Some(ch) = self.cursor.next(m)? else {
                    return Ok(None);
                };
                let raw = get_char_weights(m, sort, ch, self.id.except)?;
                let previous = self.previous;
                self.remember(raw);
                First {
                    ch,
                    raw,
                    previous,
                    start,
                }
            };
            let mut w = raw;
            let index = (u16::from(raw.case & !0xc0) << 8) | u16::from(raw.diacritic);
            if w.case & 0xc0 != 0 {
                self.get_compression_weights(m, sort, ch, &mut w)?;
            }
            if w.script == 2 {
                self.expansion = Some(index);
                continue;
            }
            break (w, false, previous, start);
        };
        w.case &= self.mask;
        if expanded {
            match w.script {
                0 => (),
                1 => self.append_nonspace_weights(w, &mut event),
                _ => self.append_normal_weights(w, &mut event),
            }
        } else {
            match w.script {
                0 => (),
                1 => self.append_nonspace_weights(w, &mut event),
                3 => {
                    if append_extra_kana_weights(previous, self.mask, &mut w, &mut event) {
                        w.case = 2;
                        self.append_normal_weights(w, &mut event);
                    } else {
                        event.primary(&[0xff, 0xff]);
                    }
                }
                4 => {
                    self.append_hangul_weights(m, sort, start, &mut event)?;
                    event.dia = Some(2);
                    event.case = Some(2);
                }
                5 => {
                    event.primary(&[0xfd, 0xff, w.primary, w.diacritic]);
                    event.dia = Some(2);
                    event.case = Some(2);
                }
                6 if self.flags & STRINGSORT == 0 => {
                    if self.flags & IGNORESYMBOLS == 0 {
                        let position = (-(i64::from(self.primary) / 2) - 1) as i16;
                        if self.flags & LING_IGNORECASE != 0 {
                            w.case = 2;
                        }
                        if self.flags & LING_IGNOREDIACRITIC != 0 {
                            w.diacritic = 2;
                        }
                        event.special = Some([
                            (position >> 8) as u8,
                            position as u8,
                            w.primary,
                            w.case | w.diacritic.wrapping_shl(3),
                        ]);
                    }
                }
                6..=12 => {
                    if self.flags & IGNORESYMBOLS == 0 {
                        event.primary(&[w.script, w.primary]);
                        event.dia = Some(w.diacritic);
                        event.case = Some(w.case);
                    }
                }
                13 if self.flags & DIGITSASNUMBERS != 0 => {
                    if !self.append_digit_weights(m, sort, start, w, &mut event)? {
                        self.append_normal_weights(w, &mut event);
                    }
                }
                _ => self.append_normal_weights(w, &mut event),
            }
        }
        Ok(Some(self.finish(event)))
    }
    // Wine get_compression_weights / find_compression, over a decoded lookahead.
    fn get_compression_weights<M: Mem>(
        &mut self,
        m: &mut Memory<'_, M>,
        sort: &Sort,
        first: u16,
        w: &mut Weights,
    ) -> Result<()> {
        if self.id.compr >= m.dword(sort.at + sort::compr_count)? {
            return Ok(());
        }
        let compr = m.dword(sort.at + sort::compressions)? + self.id.compr * compression::SIZE;
        let max = match w.case & 0xc0 {
            0xc0 => 8,
            0x80 => 5,
            0x40 => 3,
            _ => 1,
        };
        let minchar = m.word(compr + compression::minchar)?;
        let maxchar = m.word(compr + compression::maxchar)?;
        if first < minchar || first > maxchar {
            return Ok(());
        }
        let mut cursor = self.cursor.clone();
        let mut chars = [0; 8];
        chars[0] = first;
        let mut positions: [Cursor<'a, CODE>; 8] = std::array::from_fn(|_| cursor.clone());
        let mut count = 1;
        while count < max {
            let Some(ch) = cursor.next(m)? else {
                break;
            };
            if ch < minchar || ch > maxchar {
                break;
            }
            chars[count] = ch;
            positions[count] = cursor.clone();
            count += 1;
        }
        if self.tables[0] == 0 {
            self.tables[0] =
                m.dword(sort.at + sort::compr_data)? + 2 * m.dword(compr + compression::offset)?;
            for i in 1..8 {
                self.tables[i] = self.tables[i - 1]
                    + 2 * u32::from(m.word(compr + compression::len + 2 * (i as u32 - 1))?)
                        * compression_size(i as u32 + 1);
            }
        }
        for len in (2..=count).rev() {
            let count = m.word(compr + compression::len + 2 * (len as u32 - 2))?;
            if let Some(weight) = find_compression(m, &chars[..len], self.tables[len - 2], count)? {
                *w = weight;
                for &ch in &chars[1..len] {
                    let raw = get_char_weights(m, sort, ch, self.id.except)?;
                    self.remember(raw);
                }
                self.cursor = positions[len - 1].clone();
                break;
            }
        }
        Ok(())
    }
    // Wine append_digit_weights.
    fn append_digit_weights<M: Mem>(
        &mut self,
        m: &mut Memory<'_, M>,
        sort: &Sort,
        start: Mark,
        w: Weights,
        event: &mut Event,
    ) -> Result<bool> {
        let first = self.cursor.at_mark(start).next(m)?.unwrap();
        let zero = get_digit_zero_char(first);
        if zero == 0 {
            return Ok(false);
        }
        let mut values = [0u8; 19];
        values[0] = (first - zero) as u8;
        let mut count = 1;
        while count < 19 {
            let mut next = self.cursor.clone();
            let Some(ch) = next.next(m)? else {
                break;
            };
            if ch < zero || ch > zero + 9 {
                break;
            }
            values[count] = (ch - zero) as u8;
            count += 1;
            self.cursor = next;
            self.remember(get_char_weights(m, sort, ch, self.id.except)?);
        }
        let zeros = values[..count].iter().take_while(|&&v| v == 0).count();
        let mut bytes = [0; 16];
        bytes[..3].copy_from_slice(&[13, 2, (2 + count - zeros) as u8]);
        let (mut at, mut val) = (3, 2u8);
        for (i, &value) in values[..count].iter().enumerate().skip(zeros) {
            if (count - i) % 2 != 0 {
                bytes[at] = val.wrapping_shl(4).wrapping_add(value + 2);
                at += 1;
            } else {
                val = value + 2;
            }
        }
        bytes[at] = 0xfe - zeros as u8;
        event.primary(&bytes[..at + 1]);
        event.dia = Some(w.diacritic);
        event.case = Some(w.case);
        Ok(true)
    }
    // Wine append_hangul_weights.
    fn append_hangul_weights<M: Mem>(
        &mut self,
        m: &mut Memory<'_, M>,
        sort: &Sort,
        start: Mark,
        event: &mut Event,
    ) -> Result<()> {
        let start = self.cursor.at_mark(start);
        let mut cursor = start.clone();
        let (mut leading, mut vowel, mut trailing) = (0x5f, 0x60, None);
        let mut chars = [0; 3];
        let mut count = 0;
        let mut next = cursor.clone();
        let ch = next.next(m)?.unwrap();
        if (0x1100..=0x115f).contains(&ch) {
            leading = u32::from(ch - 0x1100);
            cursor = next;
            chars[count] = ch;
            count += 1;
        } else if (0xa960..=0xa97c).contains(&ch) {
            leading = u32::from(ch - (0xa960 - 0x100));
            cursor = next;
            chars[count] = ch;
            count += 1;
        }
        let mut next = cursor.clone();
        if let Some(ch) = next.next(m)? {
            if (0x1160..=0x11a7).contains(&ch) {
                vowel = u32::from(ch - 0x1100);
                cursor = next;
                chars[count] = ch;
                count += 1;
            } else if (0xd7b0..=0xd7c6).contains(&ch) {
                vowel = u32::from(ch - (0xd7b0 - 0x11d));
                cursor = next;
                chars[count] = ch;
                count += 1;
            }
        }
        let mut next = cursor.clone();
        if let Some(ch) = next.next(m)? {
            if (0x11a8..=0x11ff).contains(&ch) {
                trailing = Some(u32::from(ch - 0x1100));
                cursor = next;
                chars[count] = ch;
                count += 1;
            } else if (0xd7cb..=0xd7fb).contains(&ch) {
                trailing = Some(u32::from(ch - (0xd7cb - 0x134)));
                cursor = next;
                chars[count] = ch;
                count += 1;
            }
        }
        let jamo_table = m.dword(sort.at + sort::jamo)?;
        let jamo = |m: &mut Memory<'_, M>, index| {
            m.read::<{ jamo::SIZE as usize }>(jamo_table + index * jamo::SIZE)
        };
        let (l, mut v) = (jamo(m, leading)?, jamo(m, vowel)?);
        let mut t = if let Some(index) = trailing {
            jamo(m, index)?
        } else {
            [0; 8]
        };
        if l[jamo::is_old] == 0 && v[jamo::is_old] == 0 && t[jamo::is_old] == 0 {
            cursor = start;
            cursor.next(m)?;
            count = 1;
            vowel = 0x60;
            trailing = None;
            v = jamo(m, vowel)?;
            t = [0; 8];
        }
        // The first character was already remembered by next().
        for &ch in &chars[1..count] {
            self.remember(get_char_weights(m, sort, ch, self.id.except)?);
        }
        self.cursor = cursor;
        let lo = l[jamo::leading].max(v[jamo::leading]);
        let vo = l[jamo::vowel].max(v[jamo::vowel]);
        let to = l[jamo::trailing]
            .max(v[jamo::trailing])
            .max(t[jamo::trailing]);
        let mut composed =
            0xac00u16.wrapping_add((u16::from(lo) * 21 + u16::from(vo)) * 28 + u16::from(to));
        let filler = if leading == 0x5f || vowel == 0x60 {
            composed = composed.wrapping_sub(1);
            0x80
        } else {
            0
        };
        if composed < 0xac00 {
            composed = 0x3260;
        }
        let w = get_char_weights(m, sort, composed, self.id.except)?;
        event.primary(&[
            w.script,
            w.primary,
            0xff,
            l[jamo::weight] | filler,
            0xff,
            v[jamo::weight],
            0xff,
            if trailing.is_some() {
                t[jamo::weight]
            } else {
                2
            },
        ]);
        Ok(())
    }
}
// Wine append_extra_kana_weights; previous is exactly the result of Wine's
// backward scan, retained while consuming source characters in order.
fn append_extra_kana_weights(
    previous: Option<Weights>,
    mask: u8,
    w: &mut Weights,
    event: &mut Event,
) -> bool {
    let (mut extra, mut case) = (3, w.case);
    if w.primary <= 1 {
        let Some(mut prev) = previous else {
            return false;
        };
        if prev.script == 2 {
            return false;
        }
        if prev.script != 3 {
            *w = prev;
            return true;
        }
        case = prev.case & mask;
        if w.primary == 1 {
            prev.primary &= 0x87;
            case = (case & !1) | (w.case & 1);
        }
        extra = 4 + w.primary;
        w.primary = prev.primary;
    }
    event.extra = Some([
        0xc4 | (case & 2),
        extra,
        0xc4 | (case & 0x20),
        0xc4 | (case & 1),
    ]);
    w.script = 34;
    true
}

// Wine compression_size and find_compression (wcsncmp stops at embedded NUL).
fn compression_size(len: u32) -> u32 {
    2 + len + (len & 1)
}
fn find_compression<M: Mem>(
    m: &mut Memory<'_, M>,
    chars: &[u16],
    table: u32,
    count: u16,
) -> Result<Option<Weights>> {
    let size = compression_size(chars.len() as u32);
    let (mut min, mut max) = (0i32, i32::from(count) - 1);
    while min <= max {
        let pos = (min + max) / 2;
        let mut diff = 0;
        for (i, &ch) in chars.iter().enumerate() {
            let other = m.word(table + pos as u32 * size * 2 + i as u32 * 2)?;
            diff = i32::from(ch) - i32::from(other);
            if ch == 0 || diff != 0 {
                break;
            }
        }
        if diff == 0 {
            return Ok(Some(
                m.dword(table + (pos as u32 + 1) * size * 2 - 4)?.into(),
            ));
        }
        if diff > 0 {
            min = pos + 1;
        } else {
            max = pos - 1;
        }
    }
    Ok(None)
}

#[derive(Default, Clone, Copy)]
struct Seen {
    dia: bool,
    case: bool,
    extra: bool,
    special: bool,
}
struct Level<'a, const CODE: u8> {
    events: Events<'a, CODE>,
    phase: u8,
    pending: Option<u8>,
    bytes: [u8; 16],
    at: u8,
    len: u8,
    seen: Seen,
}
impl<'a, const CODE: u8> Level<'a, CODE> {
    fn new(text: Text<'a>, id: SortId, flags: u32, phase: u8) -> Self {
        Self {
            events: Events::<CODE>::new(text, id, flags),
            phase,
            pending: None,
            bytes: [0; 16],
            at: 0,
            len: 0,
            seen: Seen::default(),
        }
    }
    #[inline(always)]
    fn next<M: Mem>(&mut self, m: &mut Memory<'_, M>, sort: &Sort) -> Result<Option<u8>> {
        loop {
            if self.at < self.len {
                let value = self.bytes[self.at as usize];
                self.at += 1;
                return Ok(Some(value));
            }
            let Some(event) = self.events.next(m, sort)? else {
                return Ok(self.pending.take());
            };
            self.seen.dia |= event.dia.is_some_and(|v| v > 2 || event.mark);
            self.seen.case |= event.case.is_some_and(|v| v > 2);
            self.seen.extra |= event.extra.is_some();
            self.seen.special |= event.special.is_some();
            if self.phase == 1 {
                if let Some(dia) = event.dia {
                    if event.mark {
                        self.pending = Some(self.pending.unwrap_or(0).wrapping_add(dia));
                    } else if let Some(previous) = self.pending.replace(dia) {
                        return Ok(Some(previous));
                    }
                }
                continue;
            }
            self.at = 0;
            self.len = match self.phase {
                0 => {
                    self.bytes = event.primary;
                    event.len
                }
                2 => {
                    if let Some(case) = event.case {
                        self.bytes[0] = case;
                        1
                    } else {
                        0
                    }
                }
                3..=6 => {
                    if let Some(extra) = event.extra {
                        if self.phase <= 4 && self.events.flags & IGNORENONSPACE != 0 {
                            0
                        } else {
                            self.bytes[0] = extra[(self.phase - 3) as usize];
                            1
                        }
                    } else {
                        0
                    }
                }
                7 => {
                    if let Some(special) = event.special {
                        self.bytes[..4].copy_from_slice(&special);
                        4
                    } else {
                        0
                    }
                }
                _ => unreachable!(),
            };
        }
    }
}
fn ignored(phase: u8, value: u8) -> bool {
    match phase {
        1 | 2 => value <= 2,
        3 => value == 0xc6,
        4 => value == 3,
        5 => value == 0xe4,
        6 => value == 0xc5,
        _ => false,
    }
}
fn length_order(phase: u8, a: u32, b: u32) -> i32 {
    let order = a.cmp(&b) as i32;
    if matches!(phase, 3 | 5 | 6) {
        -order
    } else {
        order
    }
}
// Wine compare_sortkeys / remove_unneeded_weights. A differing byte is final
// only after both streams retain its position past their neutral tail. Until
// then, keep the candidate difference and each last non-neutral position.
fn compare_level<M: Mem, const CODE: u8>(
    m: &mut Memory<'_, M>,
    sort: &Sort,
    mut a: Level<'_, CODE>,
    mut b: Level<'_, CODE>,
) -> Result<i32> {
    let mut difference = None;
    let (mut pos, mut last_a, mut last_b) = (0u32, 0u32, 0u32);
    let (mut done_a, mut done_b) = (false, false);
    loop {
        let x = if done_a { None } else { a.next(m, sort)? };
        let y = if done_b { None } else { b.next(m, sort)? };
        if let Some(x) = x {
            if !ignored(a.phase, x) {
                last_a = pos + 1;
            }
        } else {
            done_a = true;
        }
        if let Some(y) = y {
            if !ignored(b.phase, y) {
                last_b = pos + 1;
            }
        } else {
            done_b = true;
        }
        if x != y && difference.is_none() {
            difference = Some((pos, x.cmp(&y) as i32));
        }
        if let Some((at, diff)) = difference {
            if last_a > at && last_b > at {
                return Ok(diff);
            }
            if done_a && last_b > last_a && last_a <= at {
                return Ok(length_order(a.phase, last_a, last_b));
            }
            if done_b && last_a > last_b && last_b <= at {
                return Ok(length_order(a.phase, last_a, last_b));
            }
        }
        if done_a && done_b {
            return Ok(length_order(a.phase, last_a, last_b));
        }
        pos += 1;
    }
}
// Wine reverse_sortkey followed by trailing neutral removal. Lengths align
// original suffixes; their last forward difference is the first reversed one.
fn compare_reversed<M: Mem, const CODE: u8>(
    m: &mut Memory<'_, M>,
    sort: &Sort,
    texts: [Text<'_>; 2],
    id: SortId,
    flags: u32,
) -> Result<i32> {
    let measure = |m: &mut Memory<'_, M>, text| -> Result<(u32, u32)> {
        let mut level = Level::<CODE>::new(text, id, flags, 1);
        let (mut count, mut leading) = (0, 0);
        while let Some(value) = level.next(m, sort)? {
            if count == leading && value <= 2 {
                leading += 1;
            }
            count += 1;
        }
        Ok((count, count - leading))
    };
    let (alen, asize) = measure(m, texts[0])?;
    let (blen, bsize) = measure(m, texts[1])?;
    let count = asize.min(bsize);
    let (mut a, mut b) = (
        Level::<CODE>::new(texts[0], id, flags, 1),
        Level::<CODE>::new(texts[1], id, flags, 1),
    );
    for _ in 0..alen - count {
        a.next(m, sort)?;
    }
    for _ in 0..blen - count {
        b.next(m, sort)?;
    }
    let mut diff = 0;
    for _ in 0..count {
        let next = a.next(m, sort)?.cmp(&b.next(m, sort)?) as i32;
        if next != 0 {
            diff = next;
        }
    }
    Ok(if diff != 0 {
        diff
    } else {
        asize.cmp(&bsize) as i32
    })
}

// Only ordinary ASCII characters with neutral diacritics can be removed from
// both prefixes. Check the next character too, retaining a base beside any
// combining, width/kana, expansion, compression or ignored character. Keeping
// the primary byte offset preserves Wine's absolute punctuation positions.
fn common_prefix<M: Mem, const CODE: u8>(
    m: &mut Memory<'_, M>,
    sort: &Sort,
    id: SortId,
    flags: u32,
    a: &mut Text<'_>,
    b: &mut Text<'_>,
) -> Result<()> {
    fn ordinary(ch: u16, w: Weights, flags: u32) -> bool {
        ch <= 0x7f
            && w.diacritic == 2
            && w.case & 0xc1 == 0
            && (w.script == 14
                || w.script == 13 && flags & DIGITSASNUMBERS == 0
                || (7..=12).contains(&w.script) && flags & IGNORESYMBOLS == 0)
    }
    let mask = case_mask(flags);
    let (mut left, mut right) = (Cursor::<CODE>::new(*a), Cursor::<CODE>::new(*b));
    let mut pending = None;
    loop {
        let (x, y) = (left.next(m)?, right.next(m)?);
        if x.is_none() && a.len == u32::MAX {
            a.len = left.pos;
        }
        if y.is_none() && b.len == u32::MAX {
            b.len = right.pos;
        }
        let mut weights = [None; 2];
        let mut safe = true;
        for (i, ch) in [x, y].into_iter().enumerate() {
            if let Some(ch) = ch {
                if ch > 0x7f {
                    safe = false;
                    break;
                }
                let w = if i == 1 && x == y {
                    weights[0].unwrap()
                } else {
                    get_char_weights(m, sort, ch, id.except)?
                };
                if !ordinary(ch, w, flags) {
                    safe = false;
                    break;
                }
                weights[i] = Some(w);
            }
        }
        if !safe {
            break;
        }
        // Delay each cut until its next characters are known independent.
        // An unsafe neighbor therefore leaves the preceding base in the stream.
        if let Some((aend, bend)) = pending.take() {
            a.skip = aend;
            b.skip = bend;
            a.primary = a.primary.wrapping_add(2);
            b.primary = b.primary.wrapping_add(2);
        }
        let (Some(wx), Some(wy)) = (weights[0], weights[1]) else {
            break;
        };
        if wx.primary != wy.primary || wx.script != wy.script || wx.case & mask != wy.case & mask {
            break;
        }
        pending = Some((left.pos, right.pos));
    }
    Ok(())
}
// Wine compare_string, streamed by level. The primary pass also establishes
// which secondary levels can contain retained weights, so neutral levels need
// no second pass. A primary difference never builds or scans secondary keys.
pub(super) fn compare<M: Mem, const CODE: u8>(
    m: &mut Memory<'_, M>,
    sort: &Sort,
    id: SortId,
    flags: u32,
    mut texts: [Text<'_>; 2],
) -> Result<i32> {
    let [a, b] = &mut texts;
    common_prefix::<M, CODE>(m, sort, id, flags, a, b)?;
    if a.skip == a.len && b.skip == b.len {
        return Ok(0);
    }
    // A complete safe prefix also proves both terminated lengths. Otherwise
    // finish both length scans before deciding linguistic order, as Wine does;
    // the already checked prefix need not be scanned for terminators again.
    if CODE == 0 {
        for text in &mut texts {
            if text.len == u32::MAX {
                text.len = text.skip + terminated(m, text.at + text.skip, 2)? * 2;
            }
        }
    }
    let (mut left, mut right) = (
        Level::<CODE>::new(texts[0], id, flags, 0),
        Level::<CODE>::new(texts[1], id, flags, 0),
    );
    loop {
        let (x, y) = (left.next(m, sort)?, right.next(m, sort)?);
        if x != y {
            return Ok(x.cmp(&y) as i32);
        }
        if x.is_none() {
            break;
        }
    }
    for phase in 1..8 {
        let needed = match phase {
            1 => left.seen.dia || right.seen.dia,
            2 => left.seen.case || right.seen.case,
            3 => {
                if left.seen.extra != right.seen.extra {
                    return Ok(i32::from(left.seen.extra) - i32::from(right.seen.extra));
                }
                left.seen.extra
            }
            4..=6 => left.seen.extra,
            7 => left.seen.special || right.seen.special,
            _ => unreachable!(),
        };
        if !needed || (flags & IGNORENONSPACE != 0 && matches!(phase, 1 | 3 | 4)) {
            continue;
        }
        let diff = if phase == 1 && id.flags & 0x10 != 0 {
            compare_reversed::<M, CODE>(m, sort, texts, id, flags)?
        } else {
            compare_level(
                m,
                sort,
                Level::<CODE>::new(texts[0], id, flags, phase),
                Level::<CODE>::new(texts[1], id, flags, phase),
            )?
        };
        if diff != 0 {
            return Ok(diff);
        }
    }
    Ok(0)
}
