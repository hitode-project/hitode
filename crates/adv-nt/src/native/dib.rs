//! Wine 11's DIB primitives, with the pinned Unix module's layouts.
//! Work stays in host snapshots of the rows touched until every access
//! succeeds. Overlapping surfaces share a snapshot, preserving Wine's
//! traversal and aliasing.

use adv_cpu::backend::{Guest, Prot, Reg};
use adv_machine::{Conv, Env, Outcome, Sys};

#[path = "dib_layout.rs"]
mod layout;
use layout::{dib as d, *};

macro_rules! handler {
    (halftone) => {
        resume_halftone
    };
    ($body:ident) => {
        super::resume_original
    };
}
macro_rules! bodies {
    ($( $body:ident, $argc:literal => [$($name:literal),*]; )*) => {
        pub(super) const BODIES: &[super::Body] = &[$($(
            super::Body {
                name: std::borrow::Cow::Borrowed(concat!("win32u.so!", $name)),
                conv: Conv::Cdecl { argc: $argc },
                body: Some($body),
                handler: handler!($body),
            },
        )*)*];
    };
}
bodies! {
    solid_rects, 5 => ["solid_rects_32", "solid_rects_24", "solid_rects_16"];
    solid_line, 5 => ["solid_line_32", "solid_line_24", "solid_line_16"];
    pattern_rects, 6 => ["pattern_rects_32", "pattern_rects_24", "pattern_rects_16"];
    copy_rect, 6 => ["copy_rect_32", "copy_rect_24", "copy_rect_16"];
    blend_rects, 6 => ["blend_rects_8888", "blend_rects_32", "blend_rects_24", "blend_rects_555", "blend_rects_16"];
    gradient_rect, 4 => ["gradient_rect_8888", "gradient_rect_32", "gradient_rect_24", "gradient_rect_555", "gradient_rect_16"];
    mask_rect, 5 => ["mask_rect_32", "mask_rect_24", "mask_rect_16"];
    draw_glyph, 6 => ["draw_glyph_8888", "draw_glyph_32", "draw_glyph_24", "draw_glyph_555", "draw_glyph_16"];
    draw_subpixel_glyph, 6 => ["draw_subpixel_glyph_8888", "draw_subpixel_glyph_32", "draw_subpixel_glyph_24", "draw_subpixel_glyph_555", "draw_subpixel_glyph_16"];
    stretch_row, 7 => ["stretch_row_32", "stretch_row_24", "stretch_row_16"];
    shrink_row, 7 => ["shrink_row_32", "shrink_row_24", "shrink_row_16"];
    convert_to, 4 => ["convert_to_8888", "convert_to_32", "convert_to_24", "convert_to_555", "convert_to_16", "convert_to_8", "convert_to_4", "convert_to_1"];
    halftone, 5 => ["halftone_888", "halftone_32", "halftone_24", "halftone_555", "halftone_16"];
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn short(bytes: &[u8], at: usize) -> u32 {
    u32::from(u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap()))
}
fn read_into(cpu: &dyn Guest, at: u32, bytes: &mut [u8]) -> Option<()> {
    if !cpu.accessible(at, u32::try_from(bytes.len()).ok()?, Prot::READ) {
        return None;
    }
    cpu.read(at, bytes).ok()
}
fn read(cpu: &dyn Guest, at: u32, len: usize) -> Option<Vec<u8>> {
    if !cpu.accessible(at, u32::try_from(len).ok()?, Prot::READ) {
        return None;
    }
    let mut bytes = vec![0; len];
    cpu.read(at, &mut bytes).ok()?;
    Some(bytes)
}
fn module(env: &Env<'_>) -> Option<u32> {
    env.ext
        .try_get::<crate::identity::Images>()?
        .0
        .values()
        .find(|m| m.name == "win32u.so")
        .map(|m| m.base)
}

struct Region {
    base: u32,
    bytes: Vec<u8>,
    dirty: std::ops::Range<usize>,
}
#[derive(Default)]
struct Buffers {
    regions: Vec<Region>,
    spare: Vec<Vec<u8>>,
    colors: Vec<u32>,
    lookup: Vec<u16>,
}
struct Memory<'a> {
    cpu: &'a mut dyn Guest,
    pool: &'a mut Buffers,
}
impl Drop for Memory<'_> {
    fn drop(&mut self) {
        for r in self.pool.regions.drain(..) {
            self.pool.spare.push(r.bytes);
        }
    }
}
impl Memory<'_> {
    fn load(&mut self, base: u32, len: u32, writable: bool) -> Option<()> {
        self.region(base, len, writable, true)
    }
    fn region(&mut self, base: u32, len: u32, writable: bool, snapshot: bool) -> Option<()> {
        base.checked_add(len)?;
        if !self
            .cpu
            .accessible(base, len, if writable { Prot::RW } else { Prot::READ })
        {
            return None;
        }
        if self.get(base, len as usize).is_some() {
            return Some(());
        }
        let mut start = base;
        let mut end = base + len;
        for r in &self.pool.regions {
            if start < r.base + r.bytes.len() as u32 && r.base < end {
                start = start.min(r.base);
                end = end.max(r.base + r.bytes.len() as u32);
            }
        }
        let len = (end - start) as usize;
        let index = self.pool.spare.iter().position(|b| b.capacity() >= len);
        let mut bytes = index
            .map(|i| self.pool.spare.swap_remove(i))
            .unwrap_or_default();
        bytes.resize(len, 0);
        if snapshot {
            read_into(self.cpu, start, &mut bytes)?;
        }
        let mut dirty = bytes.len()..0;
        self.pool.regions.retain_mut(|r| {
            if start <= r.base && r.base + r.bytes.len() as u32 <= end {
                let offset = (r.base - start) as usize;
                bytes[offset..offset + r.bytes.len()].copy_from_slice(&r.bytes);
                if !r.dirty.is_empty() {
                    dirty.start = dirty.start.min(offset + r.dirty.start);
                    dirty.end = dirty.end.max(offset + r.dirty.end);
                }
                self.pool.spare.push(std::mem::take(&mut r.bytes));
                false
            } else {
                true
            }
        });
        self.pool.regions.push(Region {
            base: start,
            bytes,
            dirty,
        });
        Some(())
    }
    fn get(&self, at: u32, len: usize) -> Option<&[u8]> {
        self.pool.regions.iter().find_map(|r| {
            let offset = at.checked_sub(r.base)? as usize;
            r.bytes.get(offset..offset.checked_add(len)?)
        })
    }
    fn get_mut(&mut self, at: u32, len: usize) -> Option<&mut [u8]> {
        for r in &mut self.pool.regions {
            if let Some(offset) = at.checked_sub(r.base).map(|n| n as usize)
                && let Some(end) = offset.checked_add(len)
                && end <= r.bytes.len()
            {
                r.dirty.start = r.dirty.start.min(offset);
                r.dirty.end = r.dirty.end.max(end);
                return r.bytes.get_mut(offset..end);
            }
        }
        None
    }
    fn pixel(&self, at: u32, bytes: usize) -> Option<u32> {
        let data = self.get(at, bytes)?;
        let mut value = [0; 4];
        value[..bytes].copy_from_slice(data);
        Some(u32::from_le_bytes(value))
    }
    fn put(&mut self, at: u32, bytes: usize, value: u32) -> Option<()> {
        self.get_mut(at, bytes)?
            .copy_from_slice(&value.to_le_bytes()[..bytes]);
        Some(())
    }
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Option<()> {
        let (si, so) = self.locate(src, len)?;
        let (di, offset) = self.locate(dst, len)?;
        if si == di {
            self.get_mut(dst, len)?;
            self.pool.regions[si]
                .bytes
                .copy_within(so..so + len, offset);
        } else {
            let (source, target) = self.pair(src, len, dst, len)?;
            target.copy_from_slice(source);
        }
        Some(())
    }
    fn locate(&self, at: u32, len: usize) -> Option<(usize, usize)> {
        self.pool.regions.iter().enumerate().find_map(|(i, r)| {
            let offset = at.checked_sub(r.base)? as usize;
            r.bytes.get(offset..offset.checked_add(len)?)?;
            Some((i, offset))
        })
    }
    fn pair(&mut self, src: u32, slen: usize, dst: u32, dlen: usize) -> Option<(&[u8], &mut [u8])> {
        let (si, so) = self.locate(src, slen)?;
        let (di, offset) = self.locate(dst, dlen)?;
        self.get_mut(dst, dlen)?;
        if si == di {
            let bytes = &mut self.pool.regions[si].bytes;
            if so + slen <= offset {
                let (a, b) = bytes.split_at_mut(offset);
                Some((&a[so..so + slen], &mut b[..dlen]))
            } else {
                let (a, b) = bytes.split_at_mut(so);
                Some((&b[..slen], &mut a[offset..offset + dlen]))
            }
        } else {
            let [a, b] = self.pool.regions.get_disjoint_mut([si, di]).ok()?;
            Some((&a.bytes[so..so + slen], &mut b.bytes[offset..offset + dlen]))
        }
    }
    fn commit(self) -> Option<u64> {
        // A merged source may extend beyond a destination; check only the
        // dirty interval before committing any region.
        for r in &self.pool.regions {
            if !r.dirty.is_empty()
                && !self.cpu.accessible(
                    r.base + r.dirty.start as u32,
                    r.dirty.len() as u32,
                    Prot::WRITE,
                )
            {
                return None;
            }
        }
        for r in &self.pool.regions {
            if !r.dirty.is_empty() {
                self.cpu
                    .write(r.base + r.dirty.start as u32, &r.bytes[r.dirty.clone()])
                    .ok()?;
            }
        }
        Some(0)
    }
}

#[derive(Clone)]
struct Dib {
    bpp: u32,
    kind: u32,
    width: i32,
    height: i32,
    left: i32,
    top: i32,
    stride: i32,
    bits: u32,
    compression: u32,
    masks: [u32; 3],
    shifts: [i32; 3],
    lens: [u32; 3],
    colors: u32,
    color_count: u32,
}
impl Dib {
    fn read(cpu: &dyn Guest, at: u32, base: u32) -> Option<Self> {
        let mut bytes = [0; d::SIZE];
        read_into(cpu, at, &mut bytes)?;
        let kind = word(&bytes, d::funcs).wrapping_sub(base);
        Some(Self {
            bpp: word(&bytes, d::bit_count),
            kind,
            width: word(&bytes, d::width) as i32,
            height: word(&bytes, d::height) as i32,
            left: word(&bytes, d::rect + rect::left) as i32,
            top: word(&bytes, d::rect + rect::top) as i32,
            stride: word(&bytes, d::stride) as i32,
            bits: word(&bytes, d::bits + bits::ptr),
            compression: word(&bytes, d::compression),
            masks: [
                word(&bytes, d::red_mask),
                word(&bytes, d::green_mask),
                word(&bytes, d::blue_mask),
            ],
            shifts: [
                word(&bytes, d::red_shift) as i32,
                word(&bytes, d::green_shift) as i32,
                word(&bytes, d::blue_shift) as i32,
            ],
            lens: [
                word(&bytes, d::red_len),
                word(&bytes, d::green_len),
                word(&bytes, d::blue_len),
            ],
            colors: word(&bytes, d::color_table),
            color_count: word(&bytes, d::color_table_size),
        })
    }
    fn supported(&self) -> Option<()> {
        matches!(
            self.kind,
            FUNCS_8888 | FUNCS_32 | FUNCS_24 | FUNCS_555 | FUNCS_16
        )
        .then_some(())
    }
    fn size(&self) -> usize {
        self.bpp.div_ceil(8) as usize
    }
    fn at(&self, x: i32, y: i32) -> u32 {
        self.bits
            .wrapping_add(self.top.wrapping_add(y).wrapping_mul(self.stride) as u32)
            .wrapping_add(
                self.left
                    .wrapping_add(x)
                    .wrapping_mul(self.bpp as i32)
                    .wrapping_div(8) as u32,
            )
    }
    fn row_bytes(&self) -> Option<u32> {
        u32::try_from(self.width)
            .ok()?
            .checked_mul(self.bpp)?
            .div_ceil(32)
            .checked_mul(4)
    }
    fn bounds(&self) -> Option<(u32, u32)> {
        let rows = u32::try_from(self.height.checked_sub(1)?).ok()?;
        let stride = self.stride.unsigned_abs();
        let len = rows.checked_mul(stride)?.checked_add(self.row_bytes()?)?;
        let base = if self.stride < 0 {
            self.bits.checked_sub(rows.checked_mul(stride)?)?
        } else {
            self.bits
        };
        Some((base, len))
    }
    fn load(&self, mem: &mut Memory<'_>, writable: bool) -> Option<()> {
        let (base, len) = self.bounds()?;
        mem.load(base, len, writable)
    }
    fn contains(&self, at: u32, len: u32) -> Option<()> {
        let (base, size) = self.bounds()?;
        (at >= base && at.checked_add(len)? <= base.checked_add(size)?).then_some(())
    }
    // Output rows need no old bytes. Keep stride gaps and halftone padding
    // outside these regions, and retain the surface's original access bounds.
    fn output_rows(&self, mem: &mut Memory<'_>, rows: i32, len: u32) -> Option<()> {
        if rows > 0 {
            let (base, len) = self.rows(0..rows)?;
            if !mem.cpu.accessible(base, len, Prot::RW) {
                return None;
            }
        }
        let mut y = 0;
        while y < rows {
            let n = if self.stride.unsigned_abs() == len {
                rows - y
            } else {
                1
            };
            let at = self.at(0, if self.stride < 0 { y + n - 1 } else { y });
            let count = len.checked_mul(n as u32)?;
            self.contains(at, count)?;
            mem.region(at, count, true, false)?;
            y += n;
        }
        Some(())
    }
    /// Snapshots only the rows `rows` of the clip rect touches, or the whole
    /// surface when they leave it, so stray accesses still decline.
    fn load_rows(
        &self,
        mem: &mut Memory<'_>,
        writable: bool,
        rows: std::ops::Range<i32>,
    ) -> Option<()> {
        if rows.is_empty() {
            return Some(());
        }
        let (base, len) = self.rows(rows)?;
        mem.load(base, len, writable)
    }
    fn rows(&self, rows: std::ops::Range<i32>) -> Option<(u32, u32)> {
        let first = self.top.checked_add(rows.start)?;
        let last = self.top.checked_add(rows.end - 1)?;
        if first < 0 || last >= self.height {
            return self.bounds();
        }
        let len = ((last - first) as u32)
            .checked_mul(self.stride.unsigned_abs())?
            .checked_add(self.row_bytes()?)?;
        let lowest = if self.stride < 0 { last } else { first };
        let base = self
            .bits
            .wrapping_add(lowest.wrapping_mul(self.stride) as u32);
        Some((base, len))
    }
    fn rgb(&self, pixel: u32) -> [u32; 3] {
        if self.kind == FUNCS_8888 || self.kind == FUNCS_24 {
            [(pixel >> 16) & 255, (pixel >> 8) & 255, pixel & 255]
        } else if self.kind == FUNCS_555 {
            [
                ((pixel >> 7) & 248) | ((pixel >> 12) & 7),
                ((pixel >> 2) & 248) | ((pixel >> 7) & 7),
                ((pixel << 3) & 248) | ((pixel >> 2) & 7),
            ]
        } else {
            std::array::from_fn(|i| get_field(pixel, self.shifts[i], self.lens[i]))
        }
    }
    fn pack(&self, rgb: [u32; 3]) -> u32 {
        if self.kind == FUNCS_8888 || self.kind == FUNCS_24 {
            (rgb[0] & 255) << 16 | (rgb[1] & 255) << 8 | (rgb[2] & 255)
        } else if self.kind == FUNCS_555 {
            ((rgb[0] << 7) & 0x7c00) | ((rgb[1] << 2) & 0x3e0) | ((rgb[2] >> 3) & 31)
        } else {
            (0..3).fold(0, |p, i| {
                p | put_field(rgb[i], self.shifts[i], self.lens[i])
            })
        }
    }
    fn palette(&self, cpu: &dyn Guest, base: u32, colors: &mut Vec<u32>) -> Option<()> {
        let (at, count) = if self.colors != 0 {
            (self.colors, self.color_count)
        } else {
            (
                base + match self.bpp {
                    1 => TABLE_1,
                    4 => TABLE_4,
                    8 => TABLE_8,
                    _ => return None,
                },
                1 << self.bpp,
            )
        };
        let len = count.checked_mul(rgb::SIZE as u32)?;
        if !cpu.accessible(at, len, Prot::READ) {
            return None;
        }
        colors.resize(count as usize, 0);
        for (i, color) in colors.iter_mut().enumerate() {
            let mut bytes = [0; rgb::SIZE];
            cpu.read(at + (i * rgb::SIZE) as u32, &mut bytes).ok()?;
            *color = word(&bytes, 0);
        }
        Some(())
    }
}
fn destination(env: &Env<'_>) -> Option<Dib> {
    let dib = Dib::read(env.cpu, env.arg(0), module(env)?)?;
    let name = &env.sys.thunks.get(env.thunk).name;
    let (family, format) = name.rsplit_once('_')?;
    let common = [
        "solid_rects",
        "solid_line",
        "pattern_rects",
        "copy_rect",
        "mask_rect",
        "stretch_row",
        "shrink_row",
    ]
    .iter()
    .any(|f| family.ends_with(f));
    let valid = if common {
        match format {
            "32" => dib.bpp == 32 && matches!(dib.kind, FUNCS_8888 | FUNCS_32),
            "24" => dib.bpp == 24 && dib.kind == FUNCS_24,
            "16" => dib.bpp == 16 && matches!(dib.kind, FUNCS_555 | FUNCS_16),
            _ => false,
        }
    } else {
        match format {
            "888" | "8888" => dib.bpp == 32 && dib.kind == FUNCS_8888,
            "32" => dib.bpp == 32 && dib.kind == FUNCS_32,
            "24" => dib.bpp == 24 && dib.kind == FUNCS_24,
            "555" => dib.bpp == 16 && dib.kind == FUNCS_555,
            "16" => dib.bpp == 16 && dib.kind == FUNCS_16,
            "8" => dib.bpp == 8 && dib.kind == FUNCS_8,
            "4" => dib.bpp == 4 && dib.kind == FUNCS_4,
            "1" => dib.bpp == 1 && dib.kind == FUNCS_1,
            _ => false,
        }
    };
    valid.then_some(dib)
}

fn field_mask(len: u32) -> u32 {
    if len == 0 {
        0
    } else {
        (255u32 << (8 - len.min(8))) & 255
    }
}
fn get_field(mut field: u32, shift: i32, len: u32) -> u32 {
    let shift = shift - (8 - len as i32);
    field = if shift < 0 {
        field.wrapping_shl(-shift as u32)
    } else {
        field.wrapping_shr(shift as u32)
    };
    field &= field_mask(len);
    field | field.wrapping_shr(len)
}
fn put_field(mut field: u32, shift: i32, len: u32) -> u32 {
    let shift = shift - (8 - len as i32);
    field &= field_mask(len);
    if shift < 0 {
        field.wrapping_shr(-shift as u32)
    } else {
        field.wrapping_shl(shift as u32)
    }
}
fn rects(cpu: &dyn Guest, at: u32, count: u32) -> Option<Vec<[i32; 4]>> {
    let bytes = read(cpu, at, (count as i32).max(0) as usize * rect::SIZE)?;
    Some(
        bytes
            .as_chunks::<{ rect::SIZE }>()
            .0
            .iter()
            .map(|b| {
                [
                    word(b, rect::left) as i32,
                    word(b, rect::top) as i32,
                    word(b, rect::right) as i32,
                    word(b, rect::bottom) as i32,
                ]
            })
            .collect(),
    )
}
/// The rows any of `rects` covers.
fn span(rects: &[[i32; 4]]) -> std::ops::Range<i32> {
    let rows = rects.iter().filter(|r| r[1] < r[3]);
    let start = rows.clone().map(|r| r[1]).min().unwrap_or(0);
    start..rows.map(|r| r[3]).max().unwrap_or(0)
}
fn shift(rows: &std::ops::Range<i32>, by: i32) -> Option<std::ops::Range<i32>> {
    Some(rows.start.checked_add(by)?..rows.end.checked_add(by)?)
}
fn point(cpu: &dyn Guest, at: u32) -> Option<(i32, i32)> {
    let mut b = [0; point::SIZE];
    read_into(cpu, at, &mut b)?;
    Some((word(&b, point::x) as i32, word(&b, point::y) as i32))
}
fn rop(dst: u32, src: u32, code: u32) -> u32 {
    match code {
        1 => 0,
        2 => !(dst | src),
        3 => dst & !src,
        4 => !src,
        5 => !dst & src,
        6 => !dst,
        7 => dst ^ src,
        8 => !(dst & src),
        9 => dst & src,
        10 => !(dst ^ src),
        11 => dst,
        12 => dst | !src,
        13 => src,
        14 => !dst | src,
        15 => dst | src,
        16 => u32::MAX,
        _ => dst,
    }
}

// NEON handles complete 16-byte groups; the tail uses the same scalar masks.
fn solid_row(bytes: &mut [u8], size: usize, and: u32, xor: u32) {
    let mut a = [0; 48];
    let mut b = [0; 48];
    for i in 0..48 {
        a[i] = (and >> ((i % size) * 8)) as u8;
        b[i] = (xor >> ((i % size) * 8)) as u8;
    }
    let mut at = 0;
    #[cfg(target_arch = "aarch64")]
    {
        use std::arch::aarch64::*;
        while at + 16 <= bytes.len() {
            // SAFETY: each vector is contained in the checked row and masks.
            unsafe {
                let av = vld1q_u8(a.as_ptr().add(at % 48));
                let bv = vld1q_u8(b.as_ptr().add(at % 48));
                let p = bytes.as_mut_ptr().add(at);
                let v = if and == 0 {
                    bv
                } else {
                    veorq_u8(vandq_u8(vld1q_u8(p), av), bv)
                };
                vst1q_u8(p, v);
            }
            at += 16;
        }
    }
    while at < bytes.len() {
        bytes[at] = (bytes[at] & a[at % 48]) ^ b[at % 48];
        at += 1;
    }
}

// spec: NAT-BODY-4
fn solid_rects(env: &mut Env<'_>) -> Option<u64> {
    let dib = destination(env)?;
    let rects = rects(env.cpu, env.arg(2), env.arg(1))?;
    let (and, xor) = (env.arg(3), env.arg(4));
    let mut mem = Memory {
        cpu: env.cpu,
        pool: env.ext.get_or_default::<Buffers>(),
    };
    dib.load_rows(&mut mem, true, span(&rects))?;
    for [l, t, r, b] in rects {
        let len = usize::try_from(r.checked_sub(l)?)
            .ok()?
            .checked_mul(dib.size())?;
        for y in t..b {
            solid_row(mem.get_mut(dib.at(l, y), len)?, dib.size(), and, xor);
        }
    }
    mem.commit()
}

fn solid_line(env: &mut Env<'_>) -> Option<u64> {
    let dib = destination(env)?;
    let (mut x, mut y) = point(env.cpu, env.arg(1))?;
    let mut p = [0; line::SIZE];
    read_into(env.cpu, env.arg(2), &mut p)?;
    let mut err = word(&p, line::err_start) as i32;
    let (and, xor) = (env.arg(3), env.arg(4));
    let mut mem = Memory {
        cpu: env.cpu,
        pool: env.ext.get_or_default::<Buffers>(),
    };
    let (mut ey, mut e, mut low, mut high) = (y, err, y, y);
    for _ in 1..word(&p, line::length) {
        let minor = e.wrapping_add(word(&p, line::bias) as i32) > 0;
        let add = if minor {
            line::err_add_1
        } else {
            line::err_add_2
        };
        e = e.wrapping_add(word(&p, add) as i32);
        if word(&p, line::x_major) == 0 || minor {
            ey = ey.wrapping_add(word(&p, line::y_inc) as i32);
        }
        low = low.min(ey);
        high = high.max(ey);
    }
    dib.load_rows(&mut mem, true, low..high.checked_add(1)?)?;
    for _ in 0..word(&p, line::length) {
        let at = dib.at(x, y);
        let val = mem.pixel(at, dib.size())?;
        mem.put(at, dib.size(), (val & and) ^ xor)?;
        let minor = err.wrapping_add(word(&p, line::bias) as i32) > 0;
        err = err.wrapping_add(word(
            &p,
            if minor {
                line::err_add_1
            } else {
                line::err_add_2
            },
        ) as i32);
        if word(&p, line::x_major) != 0 {
            x = x.wrapping_add(word(&p, line::x_inc) as i32);
            if minor {
                y = y.wrapping_add(word(&p, line::y_inc) as i32);
            }
        } else {
            y = y.wrapping_add(word(&p, line::y_inc) as i32);
            if minor {
                x = x.wrapping_add(word(&p, line::x_inc) as i32);
            }
        }
    }
    mem.commit()
}

fn pattern_rects(env: &mut Env<'_>) -> Option<u64> {
    let base = module(env)?;
    let dib = destination(env)?;
    let rects = rects(env.cpu, env.arg(2), env.arg(1))?;
    let origin = point(env.cpu, env.arg(3))?;
    let brush = Dib::read(env.cpu, env.arg(4), base)?;
    let masks = read(env.cpu, env.arg(5), masks::SIZE)?;
    let (and, xor) = (word(&masks, masks::and_), word(&masks, masks::xor_));
    if brush.width <= 0 || brush.height <= 0 {
        return None;
    }
    let mut mem = Memory {
        cpu: env.cpu,
        pool: env.ext.get_or_default::<Buffers>(),
    };
    dib.load_rows(&mut mem, true, span(&rects))?;
    let mut mask = brush.clone();
    mask.left = 0;
    mask.top = 0;
    mask.bpp = dib.bpp;
    mask.bits = xor;
    mask.load(&mut mem, false)?;
    if and != 0 {
        mask.bits = and;
        mask.load(&mut mem, false)?;
    }
    for [l, t, r, b] in rects {
        for y in t..b {
            let by = (y - origin.1).rem_euclid(brush.height);
            for x in l..r {
                let bx = (x - origin.0).rem_euclid(brush.width);
                let offset = by
                    .wrapping_mul(brush.stride)
                    .wrapping_add(bx * dib.size() as i32) as u32;
                let v = mem.pixel(xor.wrapping_add(offset), dib.size())?;
                let at = dib.at(x, y);
                let value = if and == 0 {
                    v
                } else {
                    (mem.pixel(at, dib.size())?
                        & mem.pixel(and.wrapping_add(offset), dib.size())?)
                        ^ v
                };
                mem.put(at, dib.size(), value)?;
            }
        }
    }
    mem.commit()
}

fn copy_rect(env: &mut Env<'_>) -> Option<u64> {
    let base = module(env)?;
    let dst = destination(env)?;
    let src = Dib::read(env.cpu, env.arg(2), base)?;
    let [l, t, r, b] = rects(env.cpu, env.arg(1), 1)?[0];
    let (sx, sy) = point(env.cpu, env.arg(3))?;
    let (code, overlap) = (env.arg(4), env.arg(5));
    let mut mem = Memory {
        cpu: env.cpu,
        pool: env.ext.get_or_default::<Buffers>(),
    };
    dst.load_rows(&mut mem, true, t..b)?;
    src.load_rows(&mut mem, false, shift(&(0..b.wrapping_sub(t)), sy)?)?;
    for row in 0..b - t {
        let row = if overlap & 8 != 0 {
            b - t - row - 1
        } else {
            row
        };
        let da = dst.at(l, t + row);
        let sa = src.at(sx, sy + row);
        let len = usize::try_from(r - l).ok()? * dst.size();
        if code == 13 {
            let data = mem.get(sa, len)?.to_vec();
            mem.get_mut(da, len)?.copy_from_slice(&data);
        } else {
            // Wine's 24-bit path performs ROPs byte by byte, including reverse copies.
            let size = if dst.bpp == 24 { 1 } else { dst.size() };
            for i in 0..len / size {
                let i = if overlap & 2 != 0 {
                    len / size - i - 1
                } else {
                    i
                };
                let offset = (i * size) as u32;
                let value = rop(
                    mem.pixel(da + offset, size)?,
                    mem.pixel(sa + offset, size)?,
                    code,
                );
                mem.put(da + offset, size, value)?;
            }
        }
    }
    mem.commit()
}

fn blend_color(dst: u32, src: u32, alpha: u32) -> u32 {
    (((src & 255) * alpha + (dst & 255) * (255 - alpha) + 127) / 255) & 255
}
fn blend_8888_pixel(dst: u32, src: u32, alpha: u32, source_alpha: bool, compression: u32) -> u32 {
    let a = if source_alpha {
        ((src >> 24) * alpha + 127) / 255
    } else {
        alpha
    };
    (0..4).fold(0, |value, i| {
        let s = if !source_alpha && i == 3 && compression != 0 {
            255
        } else {
            (src >> (i * 8)) & 255
        };
        let d = (dst >> (i * 8)) & 255;
        let c = if source_alpha {
            (s * alpha + 127) / 255 + (d * (255 - a) + 127) / 255
        } else {
            blend_color(d, s, alpha)
        };
        value | c << (i * 8)
    })
}
fn blend_8888_row(dst: &mut [u8], src: &[u8], alpha: u32, source_alpha: bool, compression: u32) {
    let mut at = 0;
    #[cfg(target_arch = "aarch64")]
    {
        use std::arch::aarch64::*;
        // For these products n <= 65152; (n + 1 + (n >> 8)) >> 8 is n / 255.
        // SAFETY: callers pass equally sized, disjoint complete pixel rows.
        unsafe {
            let indices =
                vld1q_u8([3, 3, 3, 3, 7, 7, 7, 7, 11, 11, 11, 11, 15, 15, 15, 15].as_ptr());
            let alpha_mask =
                vld1q_u8([0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255].as_ptr());
            while at + 16 <= dst.len() {
                let mut s = vld1q_u8(src.as_ptr().add(at));
                let d = vld1q_u8(dst.as_ptr().add(at));
                if !source_alpha && compression != 0 {
                    s = vorrq_u8(s, alpha_mask);
                }
                let alphas = vqtbl1q_u8(s, indices);
                let mut channels = [0u16; 16];
                for (half, (s, d, a)) in [
                    (
                        vmovl_u8(vget_low_u8(s)),
                        vmovl_u8(vget_low_u8(d)),
                        vmovl_u8(vget_low_u8(alphas)),
                    ),
                    (vmovl_high_u8(s), vmovl_high_u8(d), vmovl_high_u8(alphas)),
                ]
                .into_iter()
                .enumerate()
                {
                    let a = if source_alpha {
                        let n = vaddq_u16(vmulq_n_u16(a, alpha as u16), vdupq_n_u16(127));
                        vshrq_n_u16(
                            vaddq_u16(vaddq_u16(n, vdupq_n_u16(1)), vshrq_n_u16(n, 8)),
                            8,
                        )
                    } else {
                        vdupq_n_u16(alpha as u16)
                    };
                    let s = vmulq_n_u16(s, alpha as u16);
                    let d = vmulq_u16(d, vsubq_u16(vdupq_n_u16(255), a));
                    let n = vaddq_u16(
                        if source_alpha { s } else { vaddq_u16(s, d) },
                        vdupq_n_u16(127),
                    );
                    let mut out = vshrq_n_u16(
                        vaddq_u16(vaddq_u16(n, vdupq_n_u16(1)), vshrq_n_u16(n, 8)),
                        8,
                    );
                    if source_alpha {
                        let n = vaddq_u16(d, vdupq_n_u16(127));
                        out = vaddq_u16(
                            out,
                            vshrq_n_u16(
                                vaddq_u16(vaddq_u16(n, vdupq_n_u16(1)), vshrq_n_u16(n, 8)),
                                8,
                            ),
                        );
                    }
                    vst1q_u16(channels.as_mut_ptr().add(half * 8), out);
                }
                // Keep channel overflow: Wine ORs the promoted integer sums.
                for i in 0..4 {
                    let c = &channels[i * 4..i * 4 + 4];
                    let value = u32::from(c[0])
                        | u32::from(c[1]) << 8
                        | u32::from(c[2]) << 16
                        | u32::from(c[3]) << 24;
                    dst[at + i * 4..at + i * 4 + 4].copy_from_slice(&value.to_le_bytes());
                }
                at += 16;
            }
        }
    }
    while at < dst.len() {
        let value = blend_8888_pixel(
            word(dst, at),
            word(src, at),
            alpha,
            source_alpha,
            compression,
        );
        dst[at..at + 4].copy_from_slice(&value.to_le_bytes());
        at += 4;
    }
}

fn blend_rects(env: &mut Env<'_>) -> Option<u64> {
    let base = module(env)?;
    let dst = destination(env)?;
    let src = Dib::read(env.cpu, env.arg(3), base)?;
    let rects = rects(env.cpu, env.arg(2), env.arg(1))?;
    let (sx, sy) = point(env.cpu, env.arg(4))?;
    let blend = env.arg(5).to_le_bytes();
    let alpha = u32::from(blend[blend::SourceConstantAlpha]);
    let src_alpha = blend[blend::AlphaFormat] & 1 != 0;
    let mut mem = Memory {
        cpu: env.cpu,
        pool: env.ext.get_or_default::<Buffers>(),
    };
    let rows = span(&rects);
    dst.load_rows(&mut mem, true, rows.clone())?;
    src.load_rows(&mut mem, false, shift(&rows, sy)?)?;
    for [l, t, r, b] in rects {
        for y in t..b {
            if dst.kind == FUNCS_8888 {
                let da = dst.at(l, y);
                let sa = src.at(l + sx, y + sy);
                let len = usize::try_from(r - l).ok()?.checked_mul(4)?;
                if u64::from(da) + len as u64 <= u64::from(sa)
                    || u64::from(sa) + len as u64 <= u64::from(da)
                {
                    let source = mem.get(sa, len)?.to_vec();
                    blend_8888_row(
                        mem.get_mut(da, len)?,
                        &source,
                        alpha,
                        src_alpha,
                        src.compression,
                    );
                    continue;
                }
            }
            for x in l..r {
                let s = mem.pixel(src.at(x + sx, y + sy), 4)?;
                let at = dst.at(x, y);
                let d = mem.pixel(at, dst.size())?;
                let rgb = dst.rgb(d);
                let out = if dst.kind == FUNCS_8888 {
                    blend_8888_pixel(d, s, alpha, src_alpha, src.compression)
                } else if src_alpha {
                    let a = ((s >> 24) * alpha + 127) / 255;
                    let channels: [u32; 3] = std::array::from_fn(|i| {
                        let sc = ((s >> (i * 8)) & 255) * alpha;
                        let dc = rgb[2 - i];
                        (sc + 127) / 255 + (dc * (255 - a) + 127) / 255
                    });
                    channels[0] | channels[1] << 8 | channels[2] << 16
                } else {
                    blend_color(rgb[2], s, alpha)
                        | blend_color(rgb[1], s >> 8, alpha) << 8
                        | blend_color(rgb[0], s >> 16, alpha) << 16
                };
                let out = if dst.kind == FUNCS_8888 {
                    out
                } else {
                    dst.pack([(out >> 16) & 255, (out >> 8) & 255, out & 255])
                };
                mem.put(at, dst.size(), out)?;
            }
        }
    }
    mem.commit()
}

const BAYER4: [[i64; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
fn edge(y: i32, a: (i32, i32), b: (i32, i32)) -> Option<i32> {
    let (x, yy) = if b.0 > a.0 { b } else { a };
    Some(x + (y - yy).wrapping_mul(b.0 - a.0).checked_div(b.1 - a.1)?)
}
fn gradient_rect(env: &mut Env<'_>) -> Option<u64> {
    let dst = destination(env)?;
    let [l, t, r, b] = rects(env.cpu, env.arg(1), 1)?[0];
    let mode = env.arg(3);
    let vertices = read(
        env.cpu,
        env.arg(2),
        vertex::SIZE * if mode == 2 { 3 } else { 2 },
    )?;
    let xy: Vec<_> = vertices
        .as_chunks::<{ vertex::SIZE }>()
        .0
        .iter()
        .map(|v| (word(v, vertex::x) as i32, word(v, vertex::y) as i32))
        .collect();
    let colors: Vec<[u32; 4]> = vertices
        .as_chunks::<{ vertex::SIZE }>()
        .0
        .iter()
        .map(|v| {
            [
                short(v, vertex::Red),
                short(v, vertex::Green),
                short(v, vertex::Blue),
                short(v, vertex::Alpha),
            ]
        })
        .collect();
    let det = if mode == 2 {
        (xy[2].1 - xy[1].1)
            .wrapping_mul(xy[2].0 - xy[0].0)
            .wrapping_sub((xy[2].0 - xy[1].0).wrapping_mul(xy[2].1 - xy[0].1))
    } else {
        1
    };
    if det == 0 {
        return Some(0);
    }
    let mut mem = Memory {
        cpu: env.cpu,
        pool: env.ext.get_or_default::<Buffers>(),
    };
    dst.load_rows(&mut mem, true, t..b)?;
    for y in t..b {
        let (left, right) = if mode == 2 {
            let x1 = if y < xy[1].1 {
                edge(y, xy[0], xy[1])?
            } else {
                edge(y, xy[1], xy[2])?
            };
            let x2 = edge(y, xy[0], xy[2])?;
            (l.max(x1.min(x2)), r.min(x1.max(x2)))
        } else {
            (l, r)
        };
        for x in left..right {
            let mut c = [0i64; 4];
            for i in 0..4 {
                c[i] = match mode {
                    0 | 1 => {
                        let pos = if mode == 0 { x - xy[0].0 } else { y - xy[0].1 } as u32;
                        let len = if mode == 0 {
                            xy[1].0 - xy[0].0
                        } else {
                            xy[1].1 - xy[0].1
                        } as u32;
                        i64::from(
                            colors[0][i]
                                .wrapping_mul(len.wrapping_sub(pos))
                                .wrapping_add(colors[1][i].wrapping_mul(pos))
                                .checked_div(len)?,
                        )
                    }
                    2 => {
                        let a = i64::from(
                            (xy[1].1 - xy[2].1)
                                .wrapping_mul(x - xy[2].0)
                                .wrapping_sub((xy[1].0 - xy[2].0).wrapping_mul(y - xy[2].1)),
                        );
                        let bb = i64::from(
                            (xy[2].1 - xy[0].1)
                                .wrapping_mul(x - xy[2].0)
                                .wrapping_sub((xy[2].0 - xy[0].0).wrapping_mul(y - xy[2].1)),
                        );
                        (i64::from(colors[0][i]) * a
                            + i64::from(colors[1][i]) * bb
                            + i64::from(colors[2][i]) * (i64::from(det) - a - bb))
                            / i64::from(det)
                    }
                    _ => continue,
                };
            }
            if mode > 2 {
                continue;
            }
            let out = if dst.bpp == 16 {
                let bias = BAYER4[y.rem_euclid(4) as usize][x.rem_euclid(4) as usize];
                let rgb = std::array::from_fn(|i| {
                    let v = ((c[i] / 128 + bias) / 16).clamp(0, 31) as u32;
                    (v << 3) | (v >> 2)
                });
                // Wine first quantizes to 555, then expands its five bits for other masks.
                dst.pack(rgb)
            } else {
                let value = dst.pack(std::array::from_fn(|i| (c[i] / 256) as u8 as u32));
                value
                    | if dst.kind == FUNCS_8888 {
                        ((c[3] / 256) as u8 as u32) << 24
                    } else {
                        0
                    }
            };
            mem.put(dst.at(x, y), dst.size(), out)?;
        }
    }
    mem.commit()?;
    Some(1)
}

fn mask_rect(env: &mut Env<'_>) -> Option<u64> {
    let base = module(env)?;
    let dst = destination(env)?;
    let src = Dib::read(env.cpu, env.arg(2), base)?;
    if src.bpp != 1 {
        return None;
    }
    let [l, t, r, b] = rects(env.cpu, env.arg(1), 1)?[0];
    let (sx, sy) = point(env.cpu, env.arg(3))?;
    let code = env.arg(4);
    let mut colors = Vec::new();
    src.palette(env.cpu, base, &mut colors)?;
    let colors: Vec<_> = colors
        .iter()
        .take(2)
        .map(|p| dst.pack([(p >> 16) & 255, (p >> 8) & 255, p & 255]))
        .collect();
    let mut mem = Memory {
        cpu: env.cpu,
        pool: env.ext.get_or_default::<Buffers>(),
    };
    dst.load_rows(&mut mem, true, t..b)?;
    src.load_rows(&mut mem, false, shift(&(0..b.wrapping_sub(t)), sy)?)?;
    for y in t..b {
        for x in l..r {
            let pos = (sx & 7) + x - l;
            let value = mem.pixel(src.at(sx, sy + y - t).wrapping_add((pos / 8) as u32), 1)?;
            let bit = 7 - (pos & 7);
            let color = *colors.get(((value >> bit) & 1) as usize)?;
            let at = dst.at(x, y);
            let value = rop(mem.pixel(at, dst.size())?, color, code);
            mem.put(at, dst.size(), value)?;
        }
    }
    mem.commit()
}

fn aa_color(dst: u32, text: u32, min: u32, max: u32) -> u32 {
    if dst == text {
        dst
    } else if dst > text {
        text.wrapping_add((dst - text).wrapping_mul(max.wrapping_sub(text)) / (255 - text)) & 255
    } else {
        text.wrapping_sub((text - dst).wrapping_mul(text.wrapping_sub(min)) / text) & 255
    }
}
fn draw_glyph(env: &mut Env<'_>) -> Option<u64> {
    glyph(env, false)
}
fn draw_subpixel_glyph(env: &mut Env<'_>) -> Option<u64> {
    glyph(env, true)
}
fn glyph(env: &mut Env<'_>, subpixel: bool) -> Option<u64> {
    let base = module(env)?;
    let dst = destination(env)?;
    let glyph = Dib::read(env.cpu, env.arg(2), base)?;
    let [l, t, r, b] = rects(env.cpu, env.arg(1), 1)?[0];
    let (sx, sy) = point(env.cpu, env.arg(3))?;
    let text_pixel = env.arg(4);
    let text = dst.rgb(text_pixel);
    let ranges = if subpixel {
        Vec::new()
    } else {
        read(env.cpu, env.arg(5), intensity::SIZE * 17)?
    };
    let gamma = if subpixel && dst.bpp != 16 && env.arg(5) != 0 {
        let gamma = read(env.cpu, env.arg(5), gamma::SIZE)?;
        if word(&gamma, gamma::gamma) != 1000 {
            gamma
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    let mut mem = Memory {
        cpu: env.cpu,
        pool: env.ext.get_or_default::<Buffers>(),
    };
    dst.load_rows(&mut mem, true, t..b)?;
    glyph.load_rows(&mut mem, false, shift(&(0..b.wrapping_sub(t)), sy)?)?;
    for y in t..b {
        for x in l..r {
            let mask = mem.pixel(
                glyph.at(sx + x - l, sy + y - t),
                if subpixel { 4 } else { 1 },
            )?;
            if mask == 0 || (!subpixel && mask <= 1) {
                continue;
            }
            let at = dst.at(x, y);
            if !subpixel && mask >= 16 {
                mem.put(at, dst.size(), text_pixel)?;
                continue;
            }
            let rgb = dst.rgb(mem.pixel(at, dst.size())?);
            let mut out = [0; 3];
            for i in 0..3 {
                out[i] = if subpixel {
                    let alpha = (mask >> ((2 - i) * 8)) & 255;
                    if gamma.is_empty() {
                        blend_color(rgb[i], text[i], alpha)
                    } else if alpha == 0 || rgb[i] == text[i] {
                        rgb[i]
                    } else if alpha == 255 {
                        text[i]
                    } else {
                        let value = blend_color(
                            u32::from(gamma[gamma::decode + rgb[i] as usize]),
                            u32::from(gamma[gamma::decode + text[i] as usize]),
                            alpha,
                        );
                        u32::from(gamma[gamma::encode + value as usize])
                    }
                } else {
                    let offset = mask as usize * intensity::SIZE + i * 2;
                    aa_color(
                        rgb[i],
                        text[i],
                        u32::from(ranges[offset]),
                        u32::from(ranges[offset + 1]),
                    )
                };
            }
            mem.put(at, dst.size(), dst.pack(out))?;
        }
    }
    mem.commit()
}

fn stretch_row(env: &mut Env<'_>) -> Option<u64> {
    scale_row(env, false)
}
fn shrink_row(env: &mut Env<'_>) -> Option<u64> {
    scale_row(env, true)
}
fn scale_row(env: &mut Env<'_>, shrink: bool) -> Option<u64> {
    let base = module(env)?;
    let dst = destination(env)?;
    let src = Dib::read(env.cpu, env.arg(2), base)?;
    let (mut dx, dy) = point(env.cpu, env.arg(1))?;
    let (mut sx, sy) = point(env.cpu, env.arg(3))?;
    let p = read(env.cpu, env.arg(4), stretch::SIZE)?;
    let (mode, keep) = (env.arg(5), env.arg(6) != 0);
    let code = match mode {
        1 => 9,
        2 => 15,
        _ => 13,
    };
    let mut err = word(&p, stretch::err_start) as i32;
    let mut new_pixel = true;
    let mut mem = Memory {
        cpu: env.cpu,
        pool: env.ext.get_or_default::<Buffers>(),
    };
    dst.load_rows(&mut mem, true, dy..dy.checked_add(1)?)?;
    src.load_rows(&mut mem, false, sy..sy.checked_add(1)?)?;
    for _ in 0..word(&p, stretch::length) {
        let at = dst.at(dx, dy);
        let src = mem.pixel(src.at(sx, sy), dst.size())?;
        let value = if (shrink && mode != 3) || (!shrink && mode != 3 && keep) {
            let d = if shrink && new_pixel && !keep {
                if mode == 1 { u32::MAX } else { 0 }
            } else {
                mem.pixel(at, dst.size())?
            };
            rop(d, src, code)
        } else {
            src
        };
        mem.put(at, dst.size(), value)?;
        new_pixel = false;
        if shrink {
            sx = sx.wrapping_add(word(&p, stretch::src_inc) as i32);
        } else {
            dx = dx.wrapping_add(word(&p, stretch::dst_inc) as i32);
        }
        if err > 0 {
            if shrink {
                dx = dx.wrapping_add(word(&p, stretch::dst_inc) as i32);
                new_pixel = true;
            } else {
                sx = sx.wrapping_add(word(&p, stretch::src_inc) as i32);
            }
            err = err.wrapping_add(word(&p, stretch::err_add_1) as i32);
        } else {
            err = err.wrapping_add(word(&p, stretch::err_add_2) as i32);
        }
    }
    mem.commit()
}

fn nearest(colors: &[u32], rgb: [u32; 3]) -> u32 {
    let mut best = u32::MAX;
    let mut index = 0;
    for (i, &color) in colors.iter().enumerate() {
        let mut diff = 0;
        for (c, shift) in rgb.into_iter().zip([16, 8, 0]) {
            let d = c as i32 - ((color >> shift) & 255) as i32;
            diff += (d * d) as u32;
        }
        if diff < best {
            best = diff;
            index = i as u32;
        }
        if diff == 0 {
            break;
        }
    }
    index
}
fn bayer16(x: i32, y: i32) -> u32 {
    let (x, y) = (x as u32, y as u32);
    let mut value = 0;
    for bit in 0..4 {
        value = value * 4 + (((x >> bit) ^ (y >> bit)) & 1) * 2 + ((y >> bit) & 1);
    }
    value
}
// The format indices fix both pixel widths and color operations before the row
// loop: 8888, 24, 555, 32-bit fields, 16-bit fields.
type Row = fn(&[u8], &mut [u8], &Dib, &Dib);
fn row_kernel(src: &Dib, dst: &Dib) -> Option<Row> {
    let index = |kind| match kind {
        FUNCS_8888 => Some(0),
        FUNCS_24 => Some(1),
        FUNCS_555 => Some(2),
        FUNCS_32 => Some(3),
        FUNCS_16 => Some(4),
        _ => None,
    };
    macro_rules! targets {
        ($s:literal) => {
            [
                row::<$s, 0>,
                row::<$s, 1>,
                row::<$s, 2>,
                row::<$s, 3>,
                row::<$s, 4>,
            ]
        };
    }
    const KERNELS: [[Row; 5]; 5] = [
        targets!(0),
        targets!(1),
        targets!(2),
        targets!(3),
        targets!(4),
    ];
    let (s, d) = (index(src.kind)?, index(dst.kind)?);
    // Keep the accepted source header's declared pixel width.
    Some(if src.size() == [4, 3, 2, 4, 2][s] {
        KERNELS[s][d]
    } else {
        row_pixels
    })
}
fn row_pixels(source: &[u8], target: &mut [u8], src: &Dib, dst: &Dib) {
    for (input, output) in source
        .chunks_exact(src.size())
        .zip(target.chunks_exact_mut(dst.size()))
    {
        let mut pixel = [0; 4];
        pixel[..src.size()].copy_from_slice(input);
        let value = dst.pack(src.rgb(u32::from_le_bytes(pixel)));
        output.copy_from_slice(&value.to_le_bytes()[..dst.size()]);
    }
}
fn row<const S: u8, const D: u8>(input: &[u8], output: &mut [u8], src: &Dib, dst: &Dib) {
    // A signed field shift becomes one left and one right shift, with one zero.
    // Keep the original wrapping counts, masks and replicated low bits.
    let fields = |dib: &Dib| {
        let shifts = std::array::from_fn::<_, 3, _>(|i| dib.shifts[i] - (8 - dib.lens[i] as i32));
        (
            dib.lens.map(field_mask),
            shifts.map(|s| if s < 0 { (-s as u32) & 31 } else { 0 }),
            shifts.map(|s| if s >= 0 { s as u32 & 31 } else { 0 }),
        )
    };
    let (sm, sl, sr) = if S >= 3 {
        fields(src)
    } else {
        ([0; 3], [0; 3], [0; 3])
    };
    let (dm, dl, dr) = if D >= 3 {
        fields(dst)
    } else {
        ([0; 3], [0; 3], [0; 3])
    };
    let lens = src.lens;
    for (input, output) in input
        .chunks_exact([4, 3, 2, 4, 2][S as usize])
        .zip(output.chunks_exact_mut([4, 3, 2, 4, 2][D as usize]))
    {
        let pixel = match S {
            0 | 3 => u32::from_le_bytes(input.try_into().unwrap()),
            1 => u32::from(input[0]) | u32::from(input[1]) << 8 | u32::from(input[2]) << 16,
            _ => u32::from(u16::from_le_bytes(input.try_into().unwrap())),
        };
        let rgb = match S {
            0 | 1 => [(pixel >> 16) & 255, (pixel >> 8) & 255, pixel & 255],
            2 => [
                ((pixel >> 7) & 248) | ((pixel >> 12) & 7),
                ((pixel >> 2) & 248) | ((pixel >> 7) & 7),
                ((pixel << 3) & 248) | ((pixel >> 2) & 7),
            ],
            _ => std::array::from_fn(|i| {
                let field = pixel.wrapping_shl(sl[i]).wrapping_shr(sr[i]) & sm[i];
                field | field.wrapping_shr(lens[i])
            }),
        };
        let value = match D {
            0 | 1 => (rgb[0] & 255) << 16 | (rgb[1] & 255) << 8 | (rgb[2] & 255),
            2 => ((rgb[0] << 7) & 0x7c00) | ((rgb[1] << 2) & 0x3e0) | ((rgb[2] >> 3) & 31),
            _ => (0..3).fold(0, |p, i| {
                p | (rgb[i] & dm[i]).wrapping_shr(dl[i]).wrapping_shl(dr[i])
            }),
        };
        match D {
            0 | 3 => output.copy_from_slice(&value.to_le_bytes()),
            1 => output.copy_from_slice(&[value as u8, (value >> 8) as u8, (value >> 16) as u8]),
            _ => output.copy_from_slice(&(value as u16).to_le_bytes()),
        }
    }
}

fn convert_to(env: &mut Env<'_>) -> Option<u64> {
    let base = module(env)?;
    let dst = destination(env)?;
    let src = Dib::read(env.cpu, env.arg(1), base)?;
    src.supported()?;
    if !matches!(
        dst.kind,
        FUNCS_8888 | FUNCS_32 | FUNCS_24 | FUNCS_555 | FUNCS_16 | FUNCS_8 | FUNCS_4 | FUNCS_1
    ) {
        return None;
    }
    let mut rect = [0; rect::SIZE];
    read_into(env.cpu, env.arg(2), &mut rect)?;
    let [l, t, r, b] = std::array::from_fn(|i| word(&rect, i * 4) as i32);
    let dither = env.arg(3) != 0;
    let width = u32::try_from(r - l).ok()?;
    let row_len = u32::try_from(dst.width)
        .ok()?
        .checked_mul(dst.bpp)?
        .div_ceil(32)
        .checked_mul(4)?;
    let used = width.checked_mul(dst.bpp)?.div_ceil(8);
    let pad = row_len.checked_sub(used)?;
    let pool = env.ext.get_or_default::<Buffers>();
    if dst.bpp <= 8 {
        dst.palette(env.cpu, base, &mut pool.colors)?;
    }
    let colors = &pool.colors;
    let background = if dst.bpp == 1 {
        let bg = *colors.first()?;
        if bg >> 16 == 0x10ff {
            bg & 0xffff
        } else if src.bpp == 24 {
            ((bg >> 16) & 255) | (bg & 0xff00) | ((bg & 255) << 16)
        } else {
            (0..3).fold(0, |p, i| {
                p | put_field((bg >> (16 - i * 8)) & 255, src.shifts[i], src.lens[i])
            })
        }
    } else {
        0
    };
    let raw = (dst.kind == FUNCS_8888 && src.kind == FUNCS_8888)
        || (dst.kind == FUNCS_24 && src.kind == FUNCS_24)
        || (dst.kind == FUNCS_555 && src.kind == FUNCS_555)
        || (matches!(dst.kind, FUNCS_32 | FUNCS_16)
            && src.kind != FUNCS_8888
            && src.kind != FUNCS_555
            && src.bpp == dst.bpp
            && src.masks == dst.masks);
    let mut mem = Memory { cpu: env.cpu, pool };
    if raw && src.stride > 0 && src.stride == dst.stride && pad == 0 {
        let len = usize::try_from(b - t)
            .ok()?
            .checked_mul(src.stride as usize)?;
        let (sa, da) = (src.at(l, t), dst.at(0, 0));
        src.contains(sa, u32::try_from(len).ok()?)?;
        dst.contains(da, u32::try_from(len).ok()?)?;
        if t < b {
            for (dib, rows, prot) in [(&src, t..b, Prot::READ), (&dst, 0..b - t, Prot::RW)] {
                let (base, count) = dib.rows(rows)?;
                if !mem.cpu.accessible(base, count, prot) {
                    return None;
                }
            }
        }
        mem.load(sa, len as u32, false)?;
        mem.region(da, len as u32, true, false)?;
        mem.copy(sa, da, len)?;
        return mem.commit();
    }
    src.load_rows(&mut mem, false, t..b)?;
    dst.output_rows(&mut mem, b - t, row_len)?;
    // Wine's 8-bit conversion caches the nearest entry at the center of each
    // five-bit RGB bucket. Other indexed destinations use the exact color.
    if dst.bpp == 8 {
        mem.pool.lookup.resize(32768, u16::MAX);
        mem.pool.lookup.fill(u16::MAX);
    }
    let kernel = row_kernel(&src, &dst);
    for y in t..b {
        let da = dst.at(0, y - t);
        if raw {
            mem.copy(src.at(l, y), da, used as usize)?;
        } else if dst.bpp > 8 && {
            let source = u64::from(src.at(l, y));
            let target = u64::from(da);
            source + u64::from(width) * src.size() as u64 <= target
                || target + u64::from(used) <= source
        } {
            // Resolve disjoint rows once; overlapping conversions retain Wine's
            // per-pixel traversal below.
            let (source, target) =
                mem.pair(src.at(l, y), width as usize * src.size(), da, used as usize)?;
            kernel?(source, target, &src, &dst);
        } else {
            for x in l..r {
                let pixel = mem.pixel(src.at(x, y), src.size())?;
                let rgb = src.rgb(pixel);
                let xx = (x - l) as u32;
                match dst.bpp {
                    16 | 24 | 32 => mem.put(dst.at(x - l, y - t), dst.size(), dst.pack(rgb))?,
                    8 => {
                        let pos =
                            ((rgb[0] & 248) >> 3) | ((rgb[1] & 248) << 2) | ((rgb[2] & 248) << 7);
                        let value = &mut mem.pool.lookup[pos as usize];
                        if *value == u16::MAX {
                            *value = nearest(&mem.pool.colors, rgb.map(|v| (v & 248) | 4)) as u16;
                        }
                        let value = u32::from(*value);
                        mem.put(da + xx, 1, value)?;
                    }
                    4 => {
                        let value = nearest(&mem.pool.colors, rgb);
                        let at = da + xx / 2;
                        let value = if xx & 1 == 0 {
                            (value << 4) & 240
                        } else {
                            (value & 15) | (mem.pixel(at, 1)? & 240)
                        };
                        mem.put(at, 1, value)?;
                    }
                    1 => {
                        let mut raw_pixel = pixel;
                        if src.bpp == 24 {
                            raw_pixel =
                                ((pixel >> 16) & 255) | (pixel & 0xff00) | ((pixel & 255) << 16);
                        }
                        let value = if dst.color_count == 1 {
                            u32::from(raw_pixel == background)
                        } else {
                            let rgb = if dither {
                                let grey = (30 * rgb[0] + 59 * rgb[1] + 11 * rgb[2]) / 100;
                                [if grey + bayer16(x, y) > 255 { 255 } else { 0 }; 3]
                            } else {
                                rgb
                            };
                            u32::from(nearest(&mem.pool.colors, rgb) != 0)
                        };
                        let at = da + xx / 8;
                        let old = if xx & 7 == 0 { 0 } else { mem.pixel(at, 1)? };
                        mem.put(at, 1, old | value << (7 - (xx & 7)))?;
                    }
                    _ => return None,
                }
            }
        }
        mem.get_mut(da + used, pad as usize)?.fill(0);
    }
    mem.commit()
}

fn coords(cpu: &dyn Guest, at: u32) -> Option<([i32; 4], [i32; 4])> {
    let mut bytes = [0; coords::SIZE];
    read_into(cpu, at, &mut bytes)?;
    let (x, y, w, h) = (
        word(&bytes, coords::x) as i32,
        word(&bytes, coords::y) as i32,
        word(&bytes, coords::width) as i32,
        word(&bytes, coords::height) as i32,
    );
    let mut rect = [x, y, x.wrapping_add(w), y.wrapping_add(h)];
    if w < 0 {
        rect[0] = x.wrapping_add(w).wrapping_add(1);
        rect[2] = x.wrapping_add(1);
    }
    if h < 0 {
        rect[1] = y.wrapping_add(h).wrapping_add(1);
        rect[3] = y.wrapping_add(1);
    }
    for (i, offset) in [rect::left, rect::top, rect::right, rect::bottom]
        .into_iter()
        .enumerate()
    {
        let v = word(&bytes, coords::visrect + offset) as i32;
        rect[i] = if i < 2 {
            rect[i].max(v)
        } else {
            rect[i].min(v)
        };
    }
    if rect[0] >= rect[2] || rect[1] >= rect[3] {
        rect = [0; 4];
    }
    Some(([x, y, w, h], rect))
}
fn interpolate(start: u32, end: u32, delta: f32) -> u32 {
    (start as f32 + (end as i32 - start as i32) as f32 * delta + 0.5) as u8 as u32
}
fn halftone(env: &mut Env<'_>) -> Option<u64> {
    if env.arg(4) & !0x3f != 0x1f80 {
        return None;
    }
    let base = module(env)?;
    let dst = destination(env)?;
    let src = Dib::read(env.cpu, env.arg(2), base)?;
    src.supported()?;
    let (dc, dr) = coords(env.cpu, env.arg(1))?;
    let (sc, sr) = coords(env.cpu, env.arg(3))?;
    let (width, height) = (dr[2] - dr[0], dr[3] - dr[1]);
    let (sw, sh) = (sr[2] - sr[0], sr[3] - sr[1]);
    if width <= 0 || height <= 0 || sw <= 0 || sh <= 0 {
        return None;
    }
    let (mx, my) = ((dc[2] < 0) != (sc[2] < 0), (dc[3] < 0) != (sc[3] < 0));
    let (start_x, start_y) = (
        if mx { sr[2] - 1 } else { sr[0] },
        if my { sr[3] - 1 } else { sr[1] },
    );
    let (inc_x, inc_y) = (
        (if mx { -sw } else { sw }) as f32 / width as f32,
        (if my { -sh } else { sh }) as f32 / height as f32,
    );
    let mut fy = start_y as f32;
    let mut mem = Memory {
        cpu: env.cpu,
        pool: env.ext.get_or_default::<Buffers>(),
    };
    src.load_rows(&mut mem, false, sr[1]..sr[3])?;
    dst.output_rows(
        &mut mem,
        height,
        (width as u32).checked_mul(dst.size() as u32)?,
    )?;
    for y in 0..height {
        fy = fy.clamp(sr[1] as f32, (sr[3] - 1) as f32);
        let y0 = fy as i32;
        let y1 = (y0 + 1).clamp(sr[1], sr[3] - 1);
        let dy = fy - y0 as f32;
        let mut fx = start_x as f32;
        for x in 0..width {
            fx = fx.clamp(sr[0] as f32, (sr[2] - 1) as f32);
            let x0 = fx as i32;
            let x1 = (x0 + 1).clamp(sr[0], sr[2] - 1);
            let dx = fx - x0 as f32;
            let c00 = src.rgb(mem.pixel(src.at(x0, y0), src.size())?);
            let c01 = src.rgb(mem.pixel(src.at(x1, y0), src.size())?);
            let c10 = src.rgb(mem.pixel(src.at(x0, y1), src.size())?);
            let c11 = src.rgb(mem.pixel(src.at(x1, y1), src.size())?);
            let rgb = std::array::from_fn(|i| {
                interpolate(
                    interpolate(c00[i], c01[i], dx),
                    interpolate(c10[i], c11[i], dx),
                    dy,
                )
            });
            mem.put(dst.at(x, y), dst.size(), dst.pack(rgb))?;
            fx += inc_x;
        }
        fy += inc_y;
    }
    mem.commit()
}

// Capture the guest's SSE controls through the adapter.
pub(super) fn entry(cpu: &mut dyn Guest, sys: &mut Sys, thunk: u32) -> Result<u32, String> {
    let at = sys
        .vm
        .alloc(cpu, 0x1000, Prot::RX)
        .map_err(|e| e.to_string())?;
    let mut code = vec![0x83, 0xec, 4, 0x0f, 0xae, 0x1c, 0x24];
    for _ in 0..4 {
        code.extend_from_slice(&[0xff, 0x74, 0x24, 20]);
    }
    code.push(0xe8);
    let next = at + code.len() as u32 + 4;
    code.extend_from_slice(&thunk.wrapping_sub(next).to_le_bytes());
    code.extend_from_slice(&[0x83, 0xc4, 20, 0xc3]);
    cpu.write(at, &code).map_err(|e| e.to_string())?;
    Ok(at)
}
fn resume_halftone(env: &mut Env<'_>) -> Outcome {
    env.cpu.set_reg(Reg::Esp, env.cpu.reg(Reg::Esp) + 24);
    super::resume_original(env)
}
