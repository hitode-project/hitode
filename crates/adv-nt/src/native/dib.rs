//! Wine 11 DIB primitives over checked, directly accessible guest spans.

use adv_cpu::body::{EAX, EDX, Input, Mem, Ret, Stop};
use adv_machine::Conv;
#[path = "dib_layout.rs"]
mod layout;
use layout::{dib as d, *};

struct Access<'a, M: Mem> {
    mem: &'a mut M,
    stop: Option<Stop>,
}
impl<M: Mem> Access<'_, M> {
    fn input(&mut self, at: u32, len: u32) -> Option<*const u8> {
        match self.mem.input(at, len) {
            Ok(p) => Some(p),
            Err(e) => {
                self.stop = Some(e);
                None
            }
        }
    }
    fn read(&mut self, at: u32, bytes: &mut [u8]) -> Result<(), ()> {
        let p = self.input(at, bytes.len() as u32).ok_or(())?;
        // SAFETY: input checked the complete argument.
        unsafe {
            std::ptr::copy_nonoverlapping(p, bytes.as_mut_ptr(), bytes.len());
        }
        Ok(())
    }
}
struct Args<'a, M: Mem> {
    cpu: Access<'a, M>,
    args: [u32; 7],
    base: u32,
    bpp: u32,
    kind: u32,
}
fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn short(bytes: &[u8], at: usize) -> u32 {
    u32::from(u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap()))
}
fn read_into(cpu: &mut Access<'_, impl Mem>, at: u32, bytes: &mut [u8]) -> Option<()> {
    cpu.read(at, bytes).ok()
}
fn read<'a>(cpu: &mut Access<'_, impl Mem>, at: u32, len: usize) -> Option<&'a [u8]> {
    let p = cpu.input(at, len as u32)?;
    // SAFETY: the range remains live until the body returns.
    Some(unsafe { std::slice::from_raw_parts(p, len) })
}

#[derive(Clone, Copy)]
struct Region {
    base: u32,
    len: u32,
    write: bool,
    pointer: *mut u8,
}
struct Memory<'a, 'b, M: Mem> {
    cpu: &'a mut Access<'b, M>,
    regions: [Region; 3],
    count: usize,
    prepared: bool,
}
impl<'a, 'b, M: Mem> Memory<'a, 'b, M> {
    fn new(cpu: &'a mut Access<'b, M>) -> Self {
        Self {
            cpu,
            regions: [Region {
                base: 0,
                len: 0,
                write: false,
                pointer: std::ptr::null_mut(),
            }; 3],
            count: 0,
            prepared: false,
        }
    }
    fn load(&mut self, base: u32, len: u32, write: bool) -> Option<()> {
        self.regions[self.count] = Region {
            base,
            len,
            write,
            pointer: std::ptr::null_mut(),
        };
        self.count += 1;
        Some(())
    }
    fn prepare(&mut self) -> Option<()> {
        if !self.prepared {
            let spans = self.regions.map(|r| (r.base, r.len, r.write));
            match self.cpu.mem.spans(spans) {
                Ok(pointers) => {
                    for (r, p) in self.regions.iter_mut().zip(pointers) {
                        r.pointer = p;
                    }
                }
                Err(e) => {
                    self.cpu.stop = Some(e);
                    return None;
                }
            }
            self.prepared = true;
        }
        Some(())
    }
    fn pointer(&mut self, at: u32, len: usize) -> Option<*mut u8> {
        self.prepare()?;
        for r in &self.regions[..self.count] {
            if at >= r.base && u64::from(at) + len as u64 <= u64::from(r.base) + u64::from(r.len) {
                return Some(r.pointer.wrapping_add((at - r.base) as usize));
            }
        }
        self.cpu.stop = Some(Stop::Fault {
            address: at,
            write: true,
        });
        None
    }
    fn get_mut(&mut self, at: u32, len: usize) -> Option<&mut [u8]> {
        let p = self.pointer(at, len)?;
        // SAFETY: the complete row is checked, and callers use disjoint slices.
        Some(unsafe { std::slice::from_raw_parts_mut(p, len) })
    }
    fn pixel(&mut self, at: u32, size: usize) -> Option<u32> {
        let p = self.pointer(at, size)?;
        let mut bytes = [0; 4];
        // SAFETY: pointer checked this pixel. Reads precede writes for aliases.
        unsafe {
            std::ptr::copy_nonoverlapping(p, bytes.as_mut_ptr(), size);
        }
        Some(u32::from_le_bytes(bytes))
    }
    fn put(&mut self, at: u32, size: usize, value: u32) -> Option<()> {
        let p = self.pointer(at, size)?;
        // SAFETY: pointer checked this pixel.
        unsafe {
            std::ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), p, size);
        }
        Some(())
    }
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Option<()> {
        let s = self.pointer(src, len)?;
        let d = self.pointer(dst, len)?;
        // SAFETY: both spans are checked; Wine uses memmove for row copies.
        unsafe {
            std::ptr::copy(s, d, len);
        }
        Some(())
    }
    fn pair(&mut self, src: u32, slen: usize, dst: u32, dlen: usize) -> Option<(&[u8], &mut [u8])> {
        let s = self.pointer(src, slen)?;
        let d = self.pointer(dst, dlen)?;
        // SAFETY: callers establish disjoint row ranges before forming slices.
        Some(unsafe {
            (
                std::slice::from_raw_parts(s, slen),
                std::slice::from_raw_parts_mut(d, dlen),
            )
        })
    }
    fn commit(mut self) -> Option<u64> {
        self.prepare()?;
        match self.cpu.mem.commit() {
            Ok(()) => Some(0),
            Err(e) => {
                self.cpu.stop = Some(e);
                None
            }
        }
    }
}
macro_rules! bodies {
    ($( $body:ident, $argc:literal => [$($name:ident: $bpp:literal, $kind:ident);*]; )*) => {
        mod direct {
            use super::*;
            $($(pub(super) mod $name {
                use super::*;
                fn run<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
                    let mut env = Args { cpu: Access { mem, stop: None }, args,
                        base: if $argc < 7 { args[6] } else { 0 }, bpp: $bpp, kind: $kind };
                    match super::super::$body(&mut env) {
                        Some(value) => Ok(value.into()),
                        None => Err(env.cpu.stop.unwrap_or(Stop::DivideByZero)),
                    }
                }
                adv_cpu::body!(pub(in crate::native::dib) BODY = run, inputs: &{
                    let mut inputs = [Input::Const(0); 7];
                    let mut i = 0; while i < $argc { inputs[i] = Input::Stack(i as u8); i += 1; }
                    inputs
                }, outputs: EAX | EDX, pop: 0);
            })*)*
        }
        pub(super) const BODIES: &[super::Body] = &[$($(
            super::Body {
                name: std::borrow::Cow::Borrowed(concat!("win32u.so!", stringify!($name))),
                conv: Conv::Cdecl { argc: $argc }, body: None,
                handler: super::resume_original, direct: Some(direct::$name::BODY),
            },
        )*)*];
    };
}
bodies! {
    solid_rects, 5 => [solid_rects_32: 32, FUNCS_32; solid_rects_24: 24, FUNCS_24; solid_rects_16: 16, FUNCS_16];
    solid_line, 5 => [solid_line_32: 32, FUNCS_32; solid_line_24: 24, FUNCS_24; solid_line_16: 16, FUNCS_16];
    pattern_rects, 6 => [pattern_rects_32: 32, FUNCS_32; pattern_rects_24: 24, FUNCS_24; pattern_rects_16: 16, FUNCS_16];
    copy_rect, 6 => [copy_rect_32: 32, FUNCS_32; copy_rect_24: 24, FUNCS_24; copy_rect_16: 16, FUNCS_16];
    blend_rects, 6 => [blend_rects_8888: 32, FUNCS_8888; blend_rects_32: 32, FUNCS_32; blend_rects_24: 24, FUNCS_24; blend_rects_555: 16, FUNCS_555; blend_rects_16: 16, FUNCS_16];
    gradient_rect, 4 => [gradient_rect_8888: 32, FUNCS_8888; gradient_rect_32: 32, FUNCS_32; gradient_rect_24: 24, FUNCS_24; gradient_rect_555: 16, FUNCS_555; gradient_rect_16: 16, FUNCS_16];
    mask_rect, 5 => [mask_rect_32: 32, FUNCS_32; mask_rect_24: 24, FUNCS_24; mask_rect_16: 16, FUNCS_16];
    draw_glyph, 6 => [draw_glyph_8888: 32, FUNCS_8888; draw_glyph_32: 32, FUNCS_32; draw_glyph_24: 24, FUNCS_24; draw_glyph_555: 16, FUNCS_555; draw_glyph_16: 16, FUNCS_16];
    draw_subpixel_glyph, 6 => [draw_subpixel_glyph_8888: 32, FUNCS_8888; draw_subpixel_glyph_32: 32, FUNCS_32; draw_subpixel_glyph_24: 24, FUNCS_24; draw_subpixel_glyph_555: 16, FUNCS_555; draw_subpixel_glyph_16: 16, FUNCS_16];
    stretch_row, 7 => [stretch_row_32: 32, FUNCS_32; stretch_row_24: 24, FUNCS_24; stretch_row_16: 16, FUNCS_16];
    shrink_row, 7 => [shrink_row_32: 32, FUNCS_32; shrink_row_24: 24, FUNCS_24; shrink_row_16: 16, FUNCS_16];
    convert_to, 4 => [convert_to_8888: 32, FUNCS_8888; convert_to_32: 32, FUNCS_32; convert_to_24: 24, FUNCS_24; convert_to_555: 16, FUNCS_555; convert_to_16: 16, FUNCS_16; convert_to_8: 8, FUNCS_8; convert_to_4: 4, FUNCS_4; convert_to_1: 1, FUNCS_1];
    halftone, 4 => [halftone_888: 32, FUNCS_8888; halftone_32: 32, FUNCS_32; halftone_24: 24, FUNCS_24; halftone_555: 16, FUNCS_555; halftone_16: 16, FUNCS_16];
}

#[derive(Clone)]
struct Dib {
    bpp: u32,
    pixel_bpp: u32,
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
    fn read(cpu: &mut Access<'_, impl Mem>, at: u32, base: u32) -> Option<Self> {
        let mut bytes = [0; d::SIZE];
        read_into(cpu, at, &mut bytes)?;
        let kind = word(&bytes, d::funcs).wrapping_sub(base);
        Some(Self {
            bpp: word(&bytes, d::bit_count),
            pixel_bpp: word(&bytes, d::bit_count),
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
    fn size(&self) -> usize {
        self.pixel_bpp.div_ceil(8) as usize
    }
    fn at(&self, x: i32, y: i32) -> u32 {
        let x = self.left.wrapping_add(x);
        let offset = match self.pixel_bpp {
            1 => x / 8,
            4 => x / 2,
            _ => x.wrapping_mul(self.size() as i32),
        };
        self.bits
            .wrapping_add(self.top.wrapping_add(y).wrapping_mul(self.stride) as u32)
            .wrapping_add(offset as u32)
    }
    fn row_bytes(&self) -> Option<u32> {
        Some(
            (self
                .width
                .wrapping_mul(self.pixel_bpp as i32)
                .wrapping_add(31)
                >> 3) as u32
                & !3,
        )
    }
    fn load(&self, mem: &mut Memory<'_, '_, impl Mem>, writable: bool) -> Option<()> {
        self.load_rows(mem, writable, 0..self.height)
    }
    fn output_rows(&self, mem: &mut Memory<'_, '_, impl Mem>, rows: i32, len: u32) -> Option<()> {
        if rows <= 0 {
            return Some(());
        }
        let at = self.at(
            0,
            if self.stride < 0 {
                rows.wrapping_sub(1)
            } else {
                0
            },
        );
        mem.load(
            at,
            (rows as u32 - 1)
                .wrapping_mul(self.stride.unsigned_abs())
                .wrapping_add(len),
            true,
        )
    }
    fn load_rect(
        &self,
        mem: &mut Memory<'_, '_, impl Mem>,
        writable: bool,
        [l, t, r, b]: [i32; 4],
    ) -> Option<()> {
        if l >= r || t >= b {
            return Some(());
        }
        self.load_extent(
            mem,
            writable,
            l,
            t,
            r.wrapping_sub(l) as u32,
            b.wrapping_sub(t) as u32,
        )
    }
    fn load_extent(
        &self,
        mem: &mut Memory<'_, '_, impl Mem>,
        writable: bool,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) -> Option<()> {
        if width == 0 || height == 0 {
            return Some(());
        }
        let at = self.at(
            x,
            if self.stride < 0 {
                y.wrapping_add(height.wrapping_sub(1) as i32)
            } else {
                y
            },
        );
        let bytes = if self.pixel_bpp >= 8 {
            width.wrapping_mul(self.size() as u32)
        } else {
            let pos = (self.left.wrapping_add(x) as u32).wrapping_mul(self.pixel_bpp) & 7;
            width
                .wrapping_mul(self.pixel_bpp)
                .wrapping_add(pos)
                .wrapping_add(7)
                / 8
        };
        let len = height
            .wrapping_sub(1)
            .wrapping_mul(self.stride.unsigned_abs())
            .wrapping_add(bytes);
        mem.load(at, len, writable)
    }
    fn load_rects(
        &self,
        mem: &mut Memory<'_, '_, impl Mem>,
        writable: bool,
        rects: impl Iterator<Item = [i32; 4]>,
        sx: i32,
        sy: i32,
    ) -> Option<()> {
        let mut bounds: Option<(u32, u32)> = None;
        for [l, t, r, b] in rects.filter(|r| r[1] < r[3]) {
            let first = self.at(l.wrapping_add(sx), t.wrapping_add(sy));
            let last = self.at(l.wrapping_add(sx), b.wrapping_sub(1).wrapping_add(sy));
            let len = r.wrapping_sub(l).wrapping_mul(self.size() as i32) as u32;
            let start = first.min(last);
            let end = first.max(last).wrapping_add(len);
            bounds = Some(bounds.map_or((start, end), |(a, b)| (a.min(start), b.max(end))));
        }
        if let Some((start, end)) = bounds {
            mem.load(start, end.wrapping_sub(start), writable)?;
        }
        Some(())
    }
    fn load_row(&self, mem: &mut Memory<'_, '_, impl Mem>, writable: bool, y: i32) -> Option<()> {
        let at = self
            .bits
            .wrapping_add(self.top.wrapping_add(y).wrapping_mul(self.stride) as u32);
        mem.load(at, self.row_bytes()?, writable)
    }
    fn load_rows(
        &self,
        mem: &mut Memory<'_, '_, impl Mem>,
        writable: bool,
        rows: std::ops::Range<i32>,
    ) -> Option<()> {
        if rows.is_empty() {
            return Some(());
        }
        let at = self.bits.wrapping_add(
            self.top
                .wrapping_add(if self.stride < 0 {
                    rows.end.wrapping_sub(1)
                } else {
                    rows.start
                })
                .wrapping_mul(self.stride) as u32,
        );
        let len = (rows.end.wrapping_sub(rows.start).wrapping_sub(1) as u32)
            .wrapping_mul(self.stride.unsigned_abs())
            .wrapping_add(self.row_bytes()?);
        mem.load(at, len, writable)
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
    fn palette(&self, cpu: &mut Access<'_, impl Mem>, base: u32) -> Option<Palette> {
        let (at, count) = if self.colors != 0 {
            (self.colors, self.color_count)
        } else {
            (
                match self.bpp {
                    1 => base.wrapping_add(TABLE_1),
                    4 => base.wrapping_add(TABLE_4),
                    8 => base.wrapping_add(TABLE_8),
                    _ => 0,
                },
                1u32.wrapping_shl(self.bpp),
            )
        };
        // rgb_to_pixel_colortable assigns the DWORD size to an i386 int.
        let len = (count as i32).max(0) as u32;
        let mut remaining = u64::from(len) * 4;
        let mut address = at;
        while remaining != 0 {
            let bytes = remaining
                .min((1u64 << 32) - u64::from(address))
                .min(u64::from(u32::MAX)) as u32;
            cpu.input(address, bytes)?;
            address = address.wrapping_add(bytes);
            remaining -= u64::from(bytes);
        }
        let mut palette = Palette {
            at,
            len,
            cached: [0; 256],
        };
        for i in 0..len.min(256) {
            let mut bytes = [0; 4];
            cpu.read(at.wrapping_add(i.wrapping_mul(4)), &mut bytes)
                .ok()?;
            palette.cached[i as usize] = word(&bytes, 0);
        }
        Some(palette)
    }
}
struct Palette {
    at: u32,
    len: u32,
    cached: [u32; 256],
}
impl Palette {
    fn get(&self, cpu: &mut Access<'_, impl Mem>, index: u32) -> Option<u32> {
        if index < self.len && index < 256 {
            return Some(self.cached[index as usize]);
        }
        let mut bytes = [0; 4];
        cpu.read(self.at.wrapping_add(index.wrapping_mul(4)), &mut bytes)
            .ok()?;
        Some(word(&bytes, 0))
    }
    fn nearest(&self, cpu: &mut Access<'_, impl Mem>, rgb: [u32; 3]) -> Option<u32> {
        let mut best = u32::MAX;
        let mut index = 0;
        for i in 0..self.len {
            let color = self.get(cpu, i)?;
            let mut diff = 0;
            for (c, shift) in rgb.into_iter().zip([16, 8, 0]) {
                let d = c as i32 - ((color >> shift) & 255) as i32;
                diff += (d * d) as u32;
            }
            if diff < best {
                best = diff;
                index = i;
            }
            if diff == 0 {
                break;
            }
        }
        Some(index)
    }
}

fn destination(env: &mut Args<'_, impl Mem>) -> Option<Dib> {
    let mut dib = Dib::read(&mut env.cpu, env.args[0], env.base)?;
    // Pixel addressing and arithmetic come from the bound primitive's C type;
    // bit_count remains available to the primitive's palette/header checks.
    dib.pixel_bpp = env.bpp;
    dib.kind = env.kind;
    Some(dib)
}

fn field_mask(len: u32) -> u32 {
    if len == 0 {
        0
    } else {
        (255u32 << (8 - len.min(8))) & 255
    }
}
fn get_field(mut field: u32, shift: i32, len: u32) -> u32 {
    let shift = shift.wrapping_sub(8i32.wrapping_sub(len as i32));
    field = if shift < 0 {
        field.wrapping_shl(shift.wrapping_neg() as u32)
    } else {
        field.wrapping_shr(shift as u32)
    };
    field &= field_mask(len);
    field | field.wrapping_shr(len)
}
fn put_field(mut field: u32, shift: i32, len: u32) -> u32 {
    let shift = shift.wrapping_sub(8i32.wrapping_sub(len as i32));
    field &= field_mask(len);
    if shift < 0 {
        field.wrapping_shr(shift.wrapping_neg() as u32)
    } else {
        field.wrapping_shl(shift as u32)
    }
}
#[derive(Clone, Copy)]
struct Rects {
    bytes: *const u8,
    count: usize,
}
fn rects(cpu: &mut Access<'_, impl Mem>, at: u32, count: u32) -> Option<Rects> {
    let count = (count as i32).max(0) as usize;
    Some(Rects {
        bytes: cpu.input(at, (count * rect::SIZE) as u32)?,
        count,
    })
}
impl Rects {
    fn get(self, i: usize) -> [i32; 4] {
        // SAFETY: rects checked every rectangle before any writes.
        unsafe {
            std::array::from_fn(|j| {
                self.bytes
                    .add(i * 16 + j * 4)
                    .cast::<i32>()
                    .read_unaligned()
            })
        }
    }
    fn iter(self) -> impl Iterator<Item = [i32; 4]> + Clone {
        (0..self.count).map(move |i| self.get(i))
    }
}
fn point(cpu: &mut Access<'_, impl Mem>, at: u32) -> Option<(i32, i32)> {
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
        a[i] = and.wrapping_shr(((i % size) * 8) as u32) as u8;
        b[i] = xor.wrapping_shr(((i % size) * 8) as u32) as u8;
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
fn solid_rects(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    if env.args[1] as i32 <= 0 {
        return Some(0);
    }
    let dib = destination(env)?;
    let rects = rects(&mut env.cpu, env.args[2], env.args[1])?;
    let (and, xor) = (env.args[3], env.args[4]);
    let mut mem = Memory::new(&mut env.cpu);
    dib.load_rects(&mut mem, true, rects.iter().filter(|r| r[0] < r[2]), 0, 0)?;
    for [l, t, r, b] in rects.iter() {
        let len = r.wrapping_sub(l).wrapping_mul(dib.size() as i32) as u32 as usize;
        for y in t..b {
            solid_row(mem.get_mut(dib.at(l, y), len)?, dib.size(), and, xor);
        }
    }
    mem.commit()
}

fn solid_line(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    let dib = destination(env)?;
    let (x, y) = point(&mut env.cpu, env.args[1])?;
    let mut p = [0; line::SIZE];
    read_into(&mut env.cpu, env.args[2], &mut p)?;
    let mut err = word(&p, line::err_start) as i32;
    let (and, xor) = (env.args[3], env.args[4]);
    if word(&p, line::length) == 0 {
        return Some(0);
    }
    // The primitive touches this pixel before advancing its line state. Check
    // it before walking the remaining coordinates of a possibly faulting line.
    if let Err(stop) = env.cpu.mem.output(dib.at(x, y), dib.size() as u32) {
        env.cpu.stop = Some(stop);
        return None;
    }
    let size = dib.size() as u32;
    let row_inc = dib.stride.wrapping_mul(word(&p, line::y_inc) as i32);
    let row_inc = if matches!(size, 2 | 4) {
        row_inc / size as i32 * size as i32
    } else {
        row_inc
    };
    let column_inc = (word(&p, line::x_inc) as i32).wrapping_mul(size as i32);
    let (major, minor) = if word(&p, line::x_major) != 0 {
        (column_inc, row_inc)
    } else {
        (row_inc, column_inc)
    };
    let mut at = dib.at(x, y);
    let (mut next, mut e, mut low, mut high) = (at, err, at, at);
    let mut page = at >> 12;
    for _ in 1..word(&p, line::length) {
        if e.wrapping_add(word(&p, line::bias) as i32) > 0 {
            next = next.wrapping_add(minor as u32);
            e = e.wrapping_add(word(&p, line::err_add_1) as i32);
        } else {
            e = e.wrapping_add(word(&p, line::err_add_2) as i32);
        }
        next = next.wrapping_add(major as u32);
        if next >> 12 != page {
            if let Err(stop) = env.cpu.mem.output(next, size) {
                env.cpu.stop = Some(stop);
                return None;
            }
            page = next >> 12;
        }
        low = low.min(next);
        high = high.max(next);
    }
    let mut mem = Memory::new(&mut env.cpu);
    mem.load(low, high.wrapping_sub(low).wrapping_add(size), true)?;
    for _ in 0..word(&p, line::length) {
        let val = mem.pixel(at, size as usize)?;
        mem.put(at, size as usize, (val & and) ^ xor)?;
        if err.wrapping_add(word(&p, line::bias) as i32) > 0 {
            at = at.wrapping_add(minor as u32);
            err = err.wrapping_add(word(&p, line::err_add_1) as i32);
        } else {
            err = err.wrapping_add(word(&p, line::err_add_2) as i32);
        }
        at = at.wrapping_add(major as u32);
    }
    mem.commit()
}

fn brush_offset(edge: i32, size: i32, origin: i32) -> Option<i32> {
    if edge.wrapping_sub(origin) >= 0 {
        edge.wrapping_sub(origin).checked_rem(size)
    } else {
        let offset = origin.wrapping_sub(edge).checked_rem(size)?;
        Some(if offset != 0 {
            size.wrapping_sub(offset)
        } else {
            0
        })
    }
}

fn pattern_rects(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    if env.args[1] as i32 <= 0 {
        return Some(0);
    }
    let base = env.base;
    let dib = destination(env)?;
    let rects = rects(&mut env.cpu, env.args[2], env.args[1])?;
    let origin = point(&mut env.cpu, env.args[3])?;
    let brush = Dib::read(&mut env.cpu, env.args[4], base)?;
    let masks = read(&mut env.cpu, env.args[5], masks::SIZE)?;
    let (and, xor) = (word(masks, masks::and_), word(masks, masks::xor_));
    if brush.width == 0 || brush.height == 0 {
        return None;
    }
    let mut mem = Memory::new(&mut env.cpu);
    dib.load_rects(&mut mem, true, rects.iter().filter(|r| r[0] < r[2]), 0, 0)?;
    let mut mask = brush.clone();
    mask.left = 0;
    mask.top = 0;
    mask.pixel_bpp = dib.pixel_bpp;
    mask.bits = xor;
    mask.load(&mut mem, false)?;
    if and != 0 {
        mask.bits = and;
        mask.load(&mut mem, false)?;
    }
    let size = dib.size() as i32;
    let row_step = if matches!(size, 2 | 4) {
        brush.stride / size * size
    } else {
        brush.stride
    };
    for [l, t, r, b] in rects.iter() {
        let mut by = brush_offset(t, brush.height, origin.1)?;
        let first_x = brush_offset(l, brush.width, origin.0)?;
        let row_offset = by.wrapping_mul(brush.stride);
        let row_offset = if matches!(size, 2 | 4) {
            row_offset / size * size
        } else {
            row_offset
        } as u32;
        let (mut row_and, mut row_xor) =
            (and.wrapping_add(row_offset), xor.wrapping_add(row_offset));
        for y in t..b {
            let mut bx = first_x;
            if and == 0 {
                let mut x = l;
                while x < r {
                    let n = r.wrapping_sub(x).min(brush.width.wrapping_sub(bx));
                    let sa = row_xor.wrapping_add(bx.wrapping_mul(size) as u32);
                    mem.copy(sa, dib.at(x, y), n.wrapping_mul(size) as u32 as usize)?;
                    x = x.wrapping_add(n);
                    bx = 0;
                }
            } else {
                for x in l..r {
                    let offset = bx.wrapping_mul(size) as u32;
                    let v = mem.pixel(row_xor.wrapping_add(offset), size as usize)?;
                    let at = dib.at(x, y);
                    let value = (mem.pixel(at, size as usize)?
                        & mem.pixel(row_and.wrapping_add(offset), size as usize)?)
                        ^ v;
                    mem.put(at, size as usize, value)?;
                    bx = bx.wrapping_add(1);
                    if bx == brush.width {
                        bx = 0;
                    }
                }
            }
            by = by.wrapping_add(1);
            if by == brush.height {
                by = 0;
                row_and = and;
                row_xor = xor;
            } else {
                row_and = row_and.wrapping_add(row_step as u32);
                row_xor = row_xor.wrapping_add(row_step as u32);
            }
        }
    }
    mem.commit()
}

fn copy_rect(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    let base = env.base;
    let dst = destination(env)?;
    let mut src = Dib::read(&mut env.cpu, env.args[2], base)?;
    src.pixel_bpp = dst.pixel_bpp;
    let [l, t, r, b] = rects(&mut env.cpu, env.args[1], 1)?.get(0);
    let (sx, sy) = point(&mut env.cpu, env.args[3])?;
    let (code, overlap) = (env.args[4], env.args[5]);
    let mut mem = Memory::new(&mut env.cpu);
    dst.load_rect(&mut mem, true, [l, t, r, b])?;
    src.load_extent(
        &mut mem,
        false,
        sx,
        sy,
        r.wrapping_sub(l) as u32,
        b.wrapping_sub(t) as u32,
    )?;
    for row in 0..b.wrapping_sub(t) {
        let row = if overlap & 8 != 0 {
            b.wrapping_sub(t).wrapping_sub(row).wrapping_sub(1)
        } else {
            row
        };
        let da = dst.at(l, t.wrapping_add(row));
        let sa = src.at(sx, sy.wrapping_add(row));
        let len = r.wrapping_sub(l).wrapping_mul(dst.size() as i32) as u32 as usize;
        if code == 13 {
            mem.copy(sa, da, len)?;
        } else {
            // Wine's 24-bit path performs ROPs byte by byte, including reverse copies.
            let size = if dst.pixel_bpp == 24 { 1 } else { dst.size() };
            for i in 0..len / size {
                let i = if overlap & 2 != 0 {
                    len / size - i - 1
                } else {
                    i
                };
                let offset = (i * size) as u32;
                let value = rop(
                    mem.pixel(da.wrapping_add(offset), size)?,
                    mem.pixel(sa.wrapping_add(offset), size)?,
                    code,
                );
                mem.put(da.wrapping_add(offset), size, value)?;
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

// The admitted loops compare left/right before their first pixel, then use
// the wrapped signed difference for continuation. An overflowing positive
// coordinate interval therefore still executes its first pixel.
fn blend_width(left: i32, right: i32) -> i32 {
    if left >= right {
        0
    } else {
        let width = right.wrapping_sub(left);
        if width > 0 { width } else { 1 }
    }
}

fn blend_rects(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    if env.args[1] as i32 <= 0 {
        return Some(0);
    }
    let base = env.base;
    let dst = destination(env)?;
    let mut src = Dib::read(&mut env.cpu, env.args[3], base)?;
    src.pixel_bpp = 32;
    let rects = rects(&mut env.cpu, env.args[2], env.args[1])?;
    let (sx, sy) = point(&mut env.cpu, env.args[4])?;
    let blend = env.args[5].to_le_bytes();
    let alpha = u32::from(blend[blend::SourceConstantAlpha]);
    let src_alpha = blend[blend::AlphaFormat] & 1 != 0;
    let mut mem = Memory::new(&mut env.cpu);
    dst.load_rects(
        &mut mem,
        true,
        rects
            .iter()
            .filter(|r| r[0] < r[2])
            .map(|[l, t, r, b]| [l, t, l.wrapping_add(blend_width(l, r)), b]),
        0,
        0,
    )?;
    src.load_rects(
        &mut mem,
        false,
        rects
            .iter()
            .filter(|r| r[0] < r[2])
            .map(|[l, t, r, b]| [l, t, l.wrapping_add(blend_width(l, r)), b]),
        sx,
        sy,
    )?;
    for [l, t, r, b] in rects.iter() {
        let width = blend_width(l, r);
        if width <= 0 {
            continue;
        }
        for y in t..b {
            if dst.kind == FUNCS_8888 {
                let da = dst.at(l, y);
                let sa = src.at(l.wrapping_add(sx), y.wrapping_add(sy));
                let len = width.wrapping_mul(4) as u32 as usize;
                if u64::from(da) + len as u64 <= u64::from(sa)
                    || u64::from(sa) + len as u64 <= u64::from(da)
                {
                    let (source, target) = mem.pair(sa, len, da, len)?;
                    blend_8888_row(target, source, alpha, src_alpha, src.compression);
                    continue;
                }
            }
            for column in 0..width {
                let x = l.wrapping_add(column);
                let s = mem.pixel(src.at(x.wrapping_add(sx), y.wrapping_add(sy)), 4)?;
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
    Some(
        x.wrapping_add(
            y.wrapping_sub(yy)
                .wrapping_mul(b.0.wrapping_sub(a.0))
                .checked_div(b.1.wrapping_sub(a.1))?,
        ),
    )
}
fn gradient_rect(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    let dst = destination(env)?;
    let [l, t, r, b] = rects(&mut env.cpu, env.args[1], 1)?.get(0);
    let mode = env.args[3];
    if mode > 2 {
        return Some(1);
    }
    let mut vertices = [0; vertex::SIZE * 3];
    read_into(
        &mut env.cpu,
        env.args[2],
        &mut vertices[..vertex::SIZE * if mode == 2 { 3 } else { 2 }],
    )?;
    let xy: [(i32, i32); 3] = std::array::from_fn(|i| {
        let v = &vertices[i * vertex::SIZE..];
        (word(v, vertex::x) as i32, word(v, vertex::y) as i32)
    });
    let colors: [[u32; 4]; 3] = std::array::from_fn(|i| {
        let v = &vertices[i * vertex::SIZE..];
        [
            short(v, vertex::Red),
            short(v, vertex::Green),
            short(v, vertex::Blue),
            short(v, vertex::Alpha),
        ]
    });
    let det = if mode == 2 {
        xy[2]
            .1
            .wrapping_sub(xy[1].1)
            .wrapping_mul(xy[2].0.wrapping_sub(xy[0].0))
            .wrapping_sub(
                xy[2]
                    .0
                    .wrapping_sub(xy[1].0)
                    .wrapping_mul(xy[2].1.wrapping_sub(xy[0].1)),
            )
    } else {
        1
    };
    if det == 0 {
        return Some(0);
    }
    let mut mem = Memory::new(&mut env.cpu);
    dst.load_rect(&mut mem, true, [l, t, r, b])?;
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
                        let pos = if mode == 0 {
                            x.wrapping_sub(xy[0].0)
                        } else {
                            y.wrapping_sub(xy[0].1)
                        } as u32;
                        let len = if mode == 0 {
                            xy[1].0.wrapping_sub(xy[0].0)
                        } else {
                            xy[1].1.wrapping_sub(xy[0].1)
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
                            xy[1]
                                .1
                                .wrapping_sub(xy[2].1)
                                .wrapping_mul(x.wrapping_sub(xy[2].0))
                                .wrapping_sub(
                                    xy[1]
                                        .0
                                        .wrapping_sub(xy[2].0)
                                        .wrapping_mul(y.wrapping_sub(xy[2].1)),
                                ),
                        );
                        let bb = i64::from(
                            xy[2]
                                .1
                                .wrapping_sub(xy[0].1)
                                .wrapping_mul(x.wrapping_sub(xy[2].0))
                                .wrapping_sub(
                                    xy[2]
                                        .0
                                        .wrapping_sub(xy[0].0)
                                        .wrapping_mul(y.wrapping_sub(xy[2].1)),
                                ),
                        );
                        (i64::from(colors[0][i]) * a
                            + i64::from(colors[1][i]) * bb
                            + i64::from(colors[2][i]) * (i64::from(det) - a - bb))
                            / i64::from(det)
                    }
                    _ => continue,
                };
            }
            let out = if dst.pixel_bpp == 16 {
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

fn mask_rect(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    let base = env.base;
    let dst = destination(env)?;
    let mut src = Dib::read(&mut env.cpu, env.args[2], base)?;
    let [l, t, r, b] = rects(&mut env.cpu, env.args[1], 1)?.get(0);
    let (sx, sy) = point(&mut env.cpu, env.args[3])?;
    let code = env.args[4];
    src.color_count = 2;
    let palette = src.palette(&mut env.cpu, base)?;
    let colors = [palette.get(&mut env.cpu, 0)?, palette.get(&mut env.cpu, 1)?]
        .map(|p| dst.pack([(p >> 16) & 255, (p >> 8) & 255, p & 255]));
    src.pixel_bpp = 1;
    let mut mem = Memory::new(&mut env.cpu);
    dst.load_rect(&mut mem, true, [l, t, r, b])?;
    src.load_extent(
        &mut mem,
        false,
        sx,
        sy,
        r.wrapping_sub(l) as u32,
        b.wrapping_sub(t) as u32,
    )?;
    for y in t..b {
        for x in l..r {
            let pos = (sx & 7).wrapping_add(x).wrapping_sub(l);
            let value = mem.pixel(
                src.at(sx, sy.wrapping_add(y).wrapping_sub(t))
                    .wrapping_add((pos / 8) as u32),
                1,
            )?;
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
fn draw_glyph(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    glyph(env, false)
}
fn draw_subpixel_glyph(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    glyph(env, true)
}
fn glyph(env: &mut Args<'_, impl Mem>, subpixel: bool) -> Option<u64> {
    let base = env.base;
    let dst = destination(env)?;
    let mut glyph = Dib::read(&mut env.cpu, env.args[2], base)?;
    glyph.pixel_bpp = if subpixel { 32 } else { 8 };
    let [l, t, r, b] = rects(&mut env.cpu, env.args[1], 1)?.get(0);
    let (sx, sy) = point(&mut env.cpu, env.args[3])?;
    let text_pixel = env.args[4];
    let text = dst.rgb(text_pixel);
    let ranges = if subpixel {
        &[]
    } else {
        read(&mut env.cpu, env.args[5], intensity::SIZE * 17)?
    };
    let gamma = if subpixel && dst.pixel_bpp != 16 && env.args[5] != 0 {
        let gamma = read(&mut env.cpu, env.args[5], gamma::SIZE)?;
        if word(gamma, gamma::gamma) != 1000 {
            gamma
        } else {
            &[]
        }
    } else {
        &[]
    };
    let mut mem = Memory::new(&mut env.cpu);
    dst.load_rect(&mut mem, true, [l, t, r, b])?;
    glyph.load_extent(
        &mut mem,
        false,
        sx,
        sy,
        r.wrapping_sub(l) as u32,
        b.wrapping_sub(t) as u32,
    )?;
    for y in t..b {
        for x in l..r {
            let mask = mem.pixel(
                glyph.at(
                    sx.wrapping_add(x).wrapping_sub(l),
                    sy.wrapping_add(y).wrapping_sub(t),
                ),
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

fn stretch_row(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    scale_row(env, false)
}
fn shrink_row(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    scale_row(env, true)
}
fn scale_row(env: &mut Args<'_, impl Mem>, shrink: bool) -> Option<u64> {
    let base = env.base;
    let dst = destination(env)?;
    let mut src = Dib::read(&mut env.cpu, env.args[2], base)?;
    src.pixel_bpp = dst.pixel_bpp;
    let (mut dx, dy) = point(&mut env.cpu, env.args[1])?;
    let (mut sx, sy) = point(&mut env.cpu, env.args[3])?;
    let p = read(&mut env.cpu, env.args[4], stretch::SIZE)?;
    let (mode, keep) = (env.args[5], env.args[6] != 0);
    let code = match mode {
        1 => 9,
        2 => 15,
        _ => 13,
    };
    let mut err = word(p, stretch::err_start) as i32;
    let mut new_pixel = true;
    let mut mem = Memory::new(&mut env.cpu);
    dst.load_row(&mut mem, true, dy)?;
    src.load_row(&mut mem, false, sy)?;
    for _ in 0..word(p, stretch::length) {
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
            sx = sx.wrapping_add(word(p, stretch::src_inc) as i32);
        } else {
            dx = dx.wrapping_add(word(p, stretch::dst_inc) as i32);
        }
        if err > 0 {
            if shrink {
                dx = dx.wrapping_add(word(p, stretch::dst_inc) as i32);
                new_pixel = true;
            } else {
                sx = sx.wrapping_add(word(p, stretch::src_inc) as i32);
            }
            err = err.wrapping_add(word(p, stretch::err_add_1) as i32);
        } else {
            err = err.wrapping_add(word(p, stretch::err_add_2) as i32);
        }
    }
    mem.commit()
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
        let shifts = std::array::from_fn::<_, 3, _>(|i| {
            dib.shifts[i].wrapping_sub(8i32.wrapping_sub(dib.lens[i] as i32))
        });
        (
            dib.lens.map(field_mask),
            shifts.map(|s| {
                if s < 0 {
                    (s.wrapping_neg() as u32) & 31
                } else {
                    0
                }
            }),
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

fn convert_to(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    let base = env.base;
    let dst = destination(env)?;
    let src = Dib::read(&mut env.cpu, env.args[1], base)?;
    let mut rect = [0; rect::SIZE];
    read_into(&mut env.cpu, env.args[2], &mut rect)?;
    let [l, t, r, b] = std::array::from_fn(|i| word(&rect, i * 4) as i32);
    let dither = env.args[3] != 0;
    // The admitted converters have no default arm in their source-depth switch.
    if !matches!(src.bpp, 1 | 4 | 8 | 16 | 24 | 32) {
        return Some(0);
    }
    let width = r.wrapping_sub(l) as u32;
    let row_len = dst.row_bytes()?;
    let used = width.wrapping_mul(dst.pixel_bpp).wrapping_add(7) / 8;
    let pad = row_len.wrapping_sub(used);
    let palette = if dst.pixel_bpp <= 8 {
        Some(dst.palette(&mut env.cpu, base)?)
    } else {
        None
    };
    let source_palette = if src.bpp <= 8 {
        let mut indexed = src.clone();
        indexed.color_count = 1u32 << src.bpp;
        Some(indexed.palette(&mut env.cpu, base)?)
    } else {
        None
    };
    let background = if dst.pixel_bpp == 1 {
        let bg = palette.as_ref()?.get(&mut env.cpu, 0)?;
        if bg >> 16 == 0x10ff {
            bg & 0xffff
        } else if src.bpp <= 8 {
            let source = src.clone();
            source
                .palette(&mut env.cpu, base)?
                .nearest(&mut env.cpu, [(bg >> 16) & 255, (bg >> 8) & 255, bg & 255])?
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
            && src.bpp == dst.pixel_bpp
            && src.masks == dst.masks);
    let same_palette = if src.bpp <= 8 && src.bpp == dst.pixel_bpp {
        if src.colors == 0 || dst.colors == 0 {
            src.colors == dst.colors
        } else {
            let mut same = true;
            for i in 0..1u32 << src.bpp {
                if source_palette.as_ref()?.get(&mut env.cpu, i)?
                    != palette.as_ref()?.get(&mut env.cpu, i)?
                {
                    same = false;
                    break;
                }
            }
            same
        }
    } else {
        false
    };
    let aligned = src.left.wrapping_add(l) as u32 & (8 / src.bpp.min(8) - 1) == 0;
    let raw = raw || (same_palette && aligned && dst.pixel_bpp != 1);
    let mut mem = Memory::new(&mut env.cpu);
    if raw && src.stride > 0 && src.stride == dst.stride && pad == 0 {
        let len = b.wrapping_sub(t).wrapping_mul(src.stride) as u32 as usize;
        let (sa, da) = (src.at(l, t), dst.at(0, 0));
        mem.load(sa, len as u32, false)?;
        mem.load(da, len as u32, true)?;
        mem.copy(sa, da, len)?;
        return mem.commit();
    }
    src.load_rows(&mut mem, false, t..b)?;
    dst.output_rows(&mut mem, b.wrapping_sub(t), row_len)?;
    let kernel = row_kernel(&src, &dst);
    for y in t..b {
        let da = dst.at(0, y.wrapping_sub(t));
        if raw {
            mem.copy(src.at(l, y), da, used as usize)?;
        } else if dst.pixel_bpp > 8 && src.bpp > 8 && {
            let source = u64::from(src.at(l, y));
            let target = u64::from(da);
            source + u64::from(width) * src.size() as u64 <= target
                || target + u64::from(used) <= source
        } {
            // Resolve disjoint rows once; overlapping conversions retain Wine's
            // per-pixel traversal below.
            let (source, target) = mem.pair(
                src.at(l, y),
                width.wrapping_mul(src.size() as u32) as usize,
                da,
                used as usize,
            )?;
            kernel?(source, target, &src, &dst);
        } else {
            for x in l..r {
                let pixel = if src.bpp < 8 {
                    let pos = (src.left.wrapping_add(x) as u32).wrapping_mul(src.bpp);
                    (mem.pixel(src.at(x, y), 1)? >> (8 - src.bpp - pos % 8)) & ((1 << src.bpp) - 1)
                } else {
                    mem.pixel(src.at(x, y), src.size())?
                };
                let rgb = if src.bpp <= 8 {
                    let c = source_palette.as_ref()?.get(mem.cpu, pixel)?;
                    [(c >> 16) & 255, (c >> 8) & 255, c & 255]
                } else {
                    src.rgb(pixel)
                };
                let xx = x.wrapping_sub(l) as u32;
                let index = if src.bpp == 1 {
                    let color = source_palette.as_ref()?.get(mem.cpu, pixel)?;
                    (color >> 16 == 0x10ff).then_some(color & 0xffff)
                } else {
                    None
                };
                match dst.pixel_bpp {
                    16 | 24 | 32 => mem.put(
                        dst.at(x.wrapping_sub(l), y.wrapping_sub(t)),
                        dst.size(),
                        dst.pack(rgb),
                    )?,
                    8 => {
                        let value = if let Some(index) = index {
                            index
                        } else if src.bpp <= 8 {
                            palette.as_ref()?.nearest(mem.cpu, rgb)?
                        } else {
                            palette
                                .as_ref()?
                                .nearest(mem.cpu, rgb.map(|v| (v & 248) | 4))?
                        };
                        mem.put(da.wrapping_add(xx), 1, value)?;
                    }
                    4 => {
                        let value = if let Some(index) = index {
                            index
                        } else {
                            palette.as_ref()?.nearest(mem.cpu, rgb)?
                        };
                        let at = da.wrapping_add(xx / 2);
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
                        let value = if let Some(index) = index {
                            (index >> (7 - (xx & 7))) & 1
                        } else if dst.color_count == 1 {
                            u32::from(raw_pixel == background)
                        } else {
                            let rgb = if dither {
                                let grey = (30 * rgb[0] + 59 * rgb[1] + 11 * rgb[2]) / 100;
                                [if grey + bayer16(x, y) > 255 { 255 } else { 0 }; 3]
                            } else {
                                rgb
                            };
                            u32::from(palette.as_ref()?.nearest(mem.cpu, rgb)? != 0)
                        };
                        let at = da.wrapping_add(xx / 8);
                        let old = if xx & 7 == 0 { 0 } else { mem.pixel(at, 1)? };
                        mem.put(at, 1, old | value << (7 - (xx & 7)))?;
                    }
                    _ => return None,
                }
            }
        }
        if pad != 0 {
            mem.get_mut(da.wrapping_add(used), pad as usize)?.fill(0);
        }
    }
    mem.commit()
}

fn coords(cpu: &mut Access<'_, impl Mem>, at: u32) -> Option<([i32; 4], [i32; 4])> {
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
    (start as f32 + (end as i32).wrapping_sub(start as i32) as f32 * delta + 0.5) as u8 as u32
}
fn halftone(env: &mut Args<'_, impl Mem>) -> Option<u64> {
    let _control = super::pixels::FloatControl::new(env.args[4]);
    let base = env.base;
    let dst = destination(env)?;
    let src = Dib::read(&mut env.cpu, env.args[2], base)?;
    let (dc, dr) = coords(&mut env.cpu, env.args[1])?;
    let (sc, sr) = coords(&mut env.cpu, env.args[3])?;
    let (width, height) = (dr[2].wrapping_sub(dr[0]), dr[3].wrapping_sub(dr[1]));
    let (sw, sh) = (sr[2].wrapping_sub(sr[0]), sr[3].wrapping_sub(sr[1]));
    if width <= 0 || height <= 0 || sw <= 0 || sh <= 0 {
        return Some(0);
    }
    // Even an integral sample adds 0.5 before truncation, raising SSE
    // precision when that exception is unmasked in the pinned primitive.
    if env.args[4] & 0x1000 == 0 {
        env.cpu.stop = Some(Stop::Trap(19));
        return None;
    }
    let (mx, my) = ((dc[2] < 0) != (sc[2] < 0), (dc[3] < 0) != (sc[3] < 0));
    let (start_x, start_y) = (
        if mx { sr[2].wrapping_sub(1) } else { sr[0] },
        if my { sr[3].wrapping_sub(1) } else { sr[1] },
    );
    let (inc_x, inc_y) = (
        (if mx { sw.wrapping_neg() } else { sw }) as f32 / width as f32,
        (if my { sh.wrapping_neg() } else { sh }) as f32 / height as f32,
    );
    let mut fy = start_y as f32;
    let mut mem = Memory::new(&mut env.cpu);
    src.load_rows(&mut mem, false, sr[1]..sr[3])?;
    dst.output_rows(
        &mut mem,
        height,
        (width as u32).wrapping_mul(dst.size() as u32),
    )?;
    for y in 0..height {
        fy = fy.clamp(sr[1] as f32, (sr[3].wrapping_sub(1)) as f32);
        let y0 = fy as i32;
        let y1 = y0.wrapping_add(1).clamp(sr[1], sr[3].wrapping_sub(1));
        let dy = fy - y0 as f32;
        let mut fx = start_x as f32;
        for x in 0..width {
            fx = fx.clamp(sr[0] as f32, (sr[2].wrapping_sub(1)) as f32);
            let x0 = fx as i32;
            let x1 = x0.wrapping_add(1).clamp(sr[0], sr[2].wrapping_sub(1));
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

#[cfg(test)]
mod tests {
    // spec: NAT-BODY-19
    #[test]
    fn neon_rows_match_scalar() {
        for size in [2, 3, 4] {
            for len in 0..98 {
                for and in [0, 0x7f1f3fff, u32::MAX] {
                    let mut actual: Vec<_> = (0..len).map(|i| (i * 71) as u8).collect();
                    let mut expected = actual.clone();
                    for (i, b) in expected.iter_mut().enumerate() {
                        *b = (*b & and.wrapping_shr(((i % size) * 8) as u32) as u8)
                            ^ (0xabf54321u32 >> ((i % size) * 8)) as u8;
                    }
                    super::solid_row(&mut actual, size, and, 0xabf54321);
                    assert_eq!(actual, expected);
                }
            }
        }
        for len in 1..34 {
            for alpha in [0, 1, 127, 128, 254, 255] {
                for source_alpha in [false, true] {
                    for compression in [0, 3] {
                        let mut actual: Vec<_> = (0..len * 4).map(|i| (i * 79) as u8).collect();
                        let mut expected = actual.clone();
                        let source: Vec<_> = (0..len * 4).map(|i| (i * 31 + 17) as u8).collect();
                        for i in 0..len {
                            let v = super::blend_8888_pixel(
                                super::word(&expected, i * 4),
                                super::word(&source, i * 4),
                                alpha,
                                source_alpha,
                                compression,
                            );
                            expected[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                        }
                        super::blend_8888_row(
                            &mut actual,
                            &source,
                            alpha,
                            source_alpha,
                            compression,
                        );
                        assert_eq!(actual, expected);
                    }
                }
            }
        }
    }
}
