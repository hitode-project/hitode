//! Wine 11 D3DX conversion, including floating, indexed and block formats.
//! The pinned conversion kernels in pixels.c run on spans admitted through Mem.
use crate::identity::{self, Images};
use adv_cpu::backend::{CodeCheck, Guest, GuestExt};
use adv_cpu::body::{EAX, Input, Mem, Ret, Stop};
use adv_machine::{Ext, Sys};
use std::collections::HashMap;

#[derive(Default)]
struct Prepared(HashMap<u32, Binding>);
struct Binding {
    checks: Vec<CodeCheck>,
    copies: Vec<(u32, String)>,
}

pub(super) fn prepare(cpu: &dyn Guest, ext: &mut Ext, base: u32, pe: &adv_pe::Pe, relocs: &[u32]) {
    let Some(module) = ext.try_get::<Images>().and_then(|i| i.0.get(&base)) else {
        return;
    };
    if !module.name.starts_with("d3dx9_") {
        return;
    }
    let binding = (|| {
        let table = module.table()?;
        let mut checks = Vec::new();
        for f in &table.functions {
            let name = f.name.as_ref();
            if ![
                "argb",
                "d3dx_color",
                "d3dx_pixels_",
                "float_16_to_32",
                "float_32_to_16",
                "get_d3dx_pixel_format_info",
                "copy_pixels",
                "bcdec_",
                "stb__",
                "stb_compress_",
                "d3dx_init_color_key",
                "check_color_key",
                "get_range_for_component_type",
                "vec4_add",
                "vec4_scale",
                "d3dx_size_is_",
                "sqrtf",
                "powf",
            ]
            .iter()
            .any(|s| name.contains(s))
            {
                continue;
            }
            let at = base.wrapping_add(f.rva);
            if !identity::intact(cpu, at, f, |a| {
                relocs.binary_search(&a.wrapping_sub(base)).is_ok()
            }) {
                return None;
            }
            let mut bytes = vec![0; f.size as usize];
            cpu.read(at, &mut bytes).ok()?;
            checks.push((at, bytes.into_boxed_slice()));
        }
        let copies = pe
            .imports()
            .ok()?
            .into_iter()
            .flat_map(|m| m.imports)
            .filter_map(|i| {
                if let adv_pe::ImportName::Name { name, .. } = i.name
                    && matches!(name.as_str(), "memcpy" | "memmove" | "memset")
                {
                    return Some((base.wrapping_add(i.iat_rva), name));
                }
                None
            })
            .collect();
        Some(Binding { checks, copies })
    })();
    if let Some(binding) = binding {
        ext.get_or_default::<Prepared>().0.insert(base, binding);
    }
}

// Imports are written by the guest loader after mapping. Admission finishes
// on the first entry, before any pixel access, and publishes the direct body.
pub(super) fn bind_call(
    cpu: &mut dyn Guest,
    sys: &mut Sys,
    ext: &Ext,
    entry: u32,
    direct: adv_cpu::body::DirectBody,
) -> Option<()> {
    let images = ext.try_get::<Images>()?;
    let module = images.containing(entry)?;
    let prepared = ext.try_get::<Prepared>()?.0.get(&module.base)?;
    let mut checks = prepared.checks.clone();
    for (slot, name) in &prepared.copies {
        let target = cpu.read_u32(*slot).ok()?;
        let module = images.containing(target)?;
        let table = module.table()?;
        let f = table.at(target - module.base)?;
        if f.rva != target - module.base || f.name.as_ref() != name {
            return None;
        }
        let mut bytes = vec![0; f.size as usize];
        cpu.read(target, &mut bytes).ok()?;
        if identity::fnv1a64(&bytes) as u32 != f.hash {
            return None;
        }
        checks.push((*slot, target.to_le_bytes().into()));
        checks.push((target, bytes.into_boxed_slice()));
        if matches!(name.as_str(), "memcpy" | "memmove") && !sys.redirects.contains_key(&target) {
            let code = cpu.shared_bytes(target, 5)?;
            if code[0] != 0xe9 {
                return None;
            }
            let next = target
                .wrapping_add(5)
                .wrapping_add(u32::from_le_bytes(code[1..5].try_into().ok()?));
            let (_, bytes) = sys.redirects.get(&next)?;
            checks.push((next, bytes.clone()));
        }
    }
    for (at, bytes) in &checks {
        if cpu.shared_bytes(*at, bytes.len())? != bytes.as_ref() {
            return None;
        }
    }
    sys.redirect_checks.insert(entry, checks);
    sys.direct_bodies.insert(entry, direct);
    // SAFETY: initialize conversion TLS during cold admission.
    unsafe {
        adv_pixel_set_controls(0x037f1f80);
    }
    adv_cpu::floating::softfloat::configure(0x037f);
    // A caller translated before loader admission watched this entry as a
    // redirect. Republishing its byte retires that trace and exposes the body.
    let byte = cpu.shared_bytes(entry, 1)?[0];
    cpu.write(entry, &[byte]).ok()?;
    Some(())
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Format {
    id: u32,
    bits: [u8; 4],
    shifts: [u8; 4],
    bytes: u32,
    bw: u32,
    bh: u32,
    block: u32,
    alpha: u32,
    rgb: u32,
    flags: u32,
}
#[repr(C)]
struct Pixels {
    data: *mut u8,
    pitch: u32,
    slice: u32,
    palette: *const u8,
    size: [u32; 3],
    rect: [i32; 4],
}
unsafe extern "C" {
    fn adv_pixel_convert(
        dst: *mut Pixels,
        df: *const Format,
        src: *mut Pixels,
        sf: *const Format,
        filter: u32,
        key: u32,
        controls: u32,
    ) -> u64;
    fn adv_pixel_set_controls(controls: u32);
    #[cfg(test)]
    fn adv_pixel_d3dx_load_pixels_from_pixels(
        dst: *mut Pixels,
        df: *const Format,
        src: *mut Pixels,
        sf: *const Format,
        filter: u32,
        key: u32,
    ) -> u32;
}
fn read<M: Mem, const N: usize>(mem: &mut M, at: u32) -> Result<[u8; N], Stop> {
    let p = mem.input(at, N as u32)?;
    let mut bytes = [0; N];
    // SAFETY: the complete argument was checked.
    unsafe {
        std::ptr::copy_nonoverlapping(p, bytes.as_mut_ptr(), N);
    }
    Ok(bytes)
}
fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn format<M: Mem>(mem: &mut M, at: u32) -> Result<Format, Stop> {
    let b = read::<_, 40>(mem, at)?;
    Ok(Format {
        id: word(&b, 0),
        bits: b[4..8].try_into().unwrap(),
        shifts: b[8..12].try_into().unwrap(),
        bytes: word(&b, 12),
        bw: word(&b, 16),
        bh: word(&b, 20),
        block: word(&b, 24),
        alpha: word(&b, 28),
        rgb: word(&b, 32),
        flags: word(&b, 36),
    })
}
fn pixels<M: Mem>(mem: &mut M, at: u32) -> Result<(Pixels, u32, u32), Stop> {
    let b = read::<_, 44>(mem, at)?;
    Ok((
        Pixels {
            data: std::ptr::null_mut(),
            pitch: word(&b, 4),
            slice: word(&b, 8),
            palette: std::ptr::null(),
            size: std::array::from_fn(|i| word(&b, 16 + 4 * i)),
            rect: std::array::from_fn(|i| word(&b, 28 + 4 * i) as i32),
        },
        word(&b, 0),
        word(&b, 12),
    ))
}
fn span(p: &Pixels, f: &Format, copy: bool, write: bool, at: u32) -> Result<u32, Stop> {
    let [w, h, d] = p.size;
    if w == 0 || h == 0 || d == 0 || f.block == 0 {
        return Ok(0);
    }
    let width = if f.flags & 1 != 0 {
        p.rect[2] as u32
    } else {
        w
    };
    let height = if f.flags & 1 != 0 {
        p.rect[3] as u32
    } else {
        h
    };
    let rows = height
        .wrapping_add(f.bh.wrapping_sub(1))
        .checked_div(f.bh)
        .ok_or(Stop::DivideByZero)?;
    let blocks = width
        .wrapping_add(f.bw.wrapping_sub(1))
        .checked_div(f.bw)
        .ok_or(Stop::DivideByZero)?;
    let row = if copy {
        Some(blocks.wrapping_mul(f.block))
    } else {
        blocks.checked_mul(f.block)
    };
    if rows == 0 || row == Some(0) {
        return Ok(0);
    }
    // These are host borrow bounds, rather than an i386 expression. A
    // wrapped admission length must not lend a shorter buffer to the kernel.
    let len = row
        .and_then(|row| {
            d.wrapping_sub(1)
                .checked_mul(p.slice)?
                .checked_add(rows.wrapping_sub(1).checked_mul(p.pitch)?)?
                .checked_add(row)
        })
        .ok_or(Stop::Fault { address: at, write })?;
    Ok(len)
}
adv_cpu::body!(pub(super) LOAD = load, inputs: &[Input::Stack(0),Input::Stack(1),Input::Stack(2),Input::Stack(3),Input::Stack(4),Input::Stack(5),Input::FloatControl], outputs: EAX, pop: 0);

// spec: NAT-BODY-8
fn load<M: Mem>(mem: &mut M, [dp, df, sp, sf, filter, key, mxcsr]: [u32; 7]) -> Result<Ret, Stop> {
    if M::WINDOW && (mxcsr & 0x1f80 != 0x1f80 || (mxcsr >> 16) & 0x3f != 0x3f) {
        return Err(Stop::Slow);
    }

    let (mut dst, da, _) = pixels(mem, dp)?;
    let (mut src, sa, palette) = pixels(mem, sp)?;
    let (df, sf) = (format(mem, df)?, format(mem, sf)?);
    let copy = df.id == sf.id
        && dst.size == src.size
        && key == 0
        && dst.size[0].checked_rem(df.bw).ok_or(Stop::DivideByZero)? == 0
        && dst.size[1].checked_rem(df.bh).ok_or(Stop::DivideByZero)? == 0
        && (src.rect[0] as u32 & sf.bw.wrapping_sub(1)) == 0
        && (src.rect[1] as u32 & sf.bh.wrapping_sub(1)) == 0
        && (dst.rect[0] as u32 & df.bw.wrapping_sub(1)) == 0
        && (dst.rect[1] as u32 & df.bh.wrapping_sub(1)) == 0;
    if !copy
        && (sf.id == 58
            || df.id == 58
            || sf.flags & 2 != 0
            || df.flags & 2 != 0
            || df.alpha == 5
            || df.rgb == 5
            || df.rgb == 6)
    {
        return Ok(0x80004001u32.into());
    }
    let mut dl = span(&dst, &df, copy, true, da)?;
    if !copy && filter & 15 == 1 && src.size[1] < dst.size[1] {
        dl = dl.max(dst.size[1].wrapping_mul(dst.pitch));
    }
    if !copy && filter & 15 == 1 && src.size[2] < dst.size[2] {
        dl = dl.max(dst.size[2].wrapping_mul(dst.slice));
    }
    let sl = span(&src, &sf, copy, false, sa)?;
    let pl = if !copy && (sf.alpha == 5 || sf.rgb == 5) {
        1024
    } else {
        0
    };
    let [d, s, p] = mem.spans([(da, dl, true), (sa, sl, false), (palette, pl, false)])?;
    if !copy
        && key == 0
        && dst.size == src.size
        && matches!(filter & 15, 1..=4)
        && df.bytes == 4
        && sf.bytes == 4
        && df.flags == 0
        && sf.flags == 0
        && matches!(df.alpha, 0 | 1)
        && matches!(sf.alpha, 0 | 1)
        && df.rgb == 1
        && sf.rgb == 1
        && df.bits.iter().chain(&sf.bits).all(|b| matches!(b, 0 | 8))
        && df
            .shifts
            .iter()
            .chain(&sf.shifts)
            .all(|b| *b <= 24 && b % 8 == 0)
        && (u64::from(da) + u64::from(dl) <= u64::from(sa)
            || u64::from(sa) + u64::from(sl) <= u64::from(da))
    {
        shuffle(&dst, &src, &df, &sf, d, s);
        mem.commit()?;
        return Ok(0u32.into());
    }
    dst.data = d;
    src.data = s;
    src.palette = p;
    let _control = FloatControl::new(mxcsr);
    // SAFETY: this thread owns the conversion until it returns.
    unsafe {
        adv_pixel_set_controls(mxcsr);
    }
    // SAFETY: layouts are repr(C), and every external byte the kernels access
    // belongs to these checked spans. Their temporary buffers are kernel-owned.
    let result = unsafe { adv_pixel_convert(&mut dst, &df, &mut src, &sf, filter, key, mxcsr) };
    if result >> 32 == 256 {
        // The primitive calls the guest CRT assertion handler (A64-CALL-6).
        return Err(Stop::Guest);
    }
    mem.commit()?;
    if result >> 32 != 0 {
        Err(Stop::Trap((result >> 32) as u8))
    } else {
        Ok((result as u32).into())
    }
}

fn shuffle(dst: &Pixels, src: &Pixels, df: &Format, sf: &Format, d: *mut u8, s: *const u8) {
    let mut indices = [255u8; 16];
    let mut fill = [0u8; 16];
    for pixel in 0..4 {
        for c in 0..4 {
            if df.bits[c] == 0 {
                continue;
            }
            let to = pixel * 4 + df.shifts[c] as usize / 8;
            if sf.bits[c] == 0 {
                fill[to] = 255;
            } else {
                indices[to] = (pixel * 4 + sf.shifts[c] as usize / 8) as u8;
            }
        }
    }
    // SAFETY: the caller admits complete disjoint spans with four bytes per
    // pixel, and constructs byte indices from admitted eight-bit fields.
    unsafe {
        #[cfg(target_arch = "aarch64")]
        let (index, constant) = {
            use std::arch::aarch64::*;
            (vld1q_u8(indices.as_ptr()), vld1q_u8(fill.as_ptr()))
        };
        for z in 0..dst.size[2] {
            for y in 0..dst.size[1] {
                let target = d.add(
                    z.wrapping_mul(dst.slice)
                        .wrapping_add(y.wrapping_mul(dst.pitch)) as usize,
                );
                let source = s.add(
                    z.wrapping_mul(src.slice)
                        .wrapping_add(y.wrapping_mul(src.pitch)) as usize,
                );
                let mut x = 0;
                #[cfg(target_arch = "aarch64")]
                {
                    use std::arch::aarch64::*;
                    while dst.size[0] - x >= 4 {
                        let value = vorrq_u8(
                            vqtbl1q_u8(vld1q_u8(source.add(x.wrapping_mul(4) as usize)), index),
                            constant,
                        );
                        vst1q_u8(target.add(x.wrapping_mul(4) as usize), value);
                        x += 4;
                    }
                }
                while x < dst.size[0] {
                    for c in 0..4 {
                        let from = indices[c] as usize;
                        *target.add(x.wrapping_mul(4) as usize + c) = if from == 255 {
                            fill[c]
                        } else {
                            *source.add(x.wrapping_mul(4) as usize + from)
                        };
                    }
                    x += 1;
                }
            }
        }
    }
}

pub(super) struct FloatControl {
    #[cfg(target_arch = "aarch64")]
    saved: u64,
    #[cfg(target_arch = "x86_64")]
    saved: u32,
}
impl FloatControl {
    pub(super) fn new(mxcsr: u32) -> Self {
        #[cfg(target_arch = "aarch64")]
        {
            let saved: u64;
            let rounding = [0u64, 2, 1, 3][(mxcsr >> 13 & 3) as usize];
            let control = rounding << 22;
            // SAFETY: the body's floating controls are restored by Drop.
            unsafe {
                std::arch::asm!("mrs {saved}, fpcr", "msr fpcr, {control}", saved = out(reg) saved, control = in(reg) control, options(nostack));
            }
            Self { saved }
        }
        #[cfg(target_arch = "x86_64")]
        {
            let mut saved = 0;
            // SAFETY: SSE controls use valid guest control bits, with host traps masked.
            unsafe {
                std::arch::asm!("stmxcsr [{p}]", p=in(reg) &mut saved, options(nostack));
            }
            let control = (mxcsr & 0xffff & !0x8040) | 0x1f80;
            unsafe {
                std::arch::asm!("ldmxcsr [{p}]", p=in(reg) &control, options(nostack));
            }
            Self { saved }
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        {
            let _ = mxcsr;
            Self {}
        }
    }
}
impl Drop for FloatControl {
    fn drop(&mut self) {
        #[cfg(target_arch = "aarch64")]
        // SAFETY: saved is this thread's entry FPCR.
        unsafe {
            std::arch::asm!("msr fpcr, {saved}", saved=in(reg) self.saved, options(nostack));
        }
        #[cfg(target_arch = "x86_64")]
        unsafe {
            std::arch::asm!("ldmxcsr [{p}]", p=in(reg) &self.saved, options(nostack));
        }
    }
}

// sqrtf stores its x87 double return, then narrows with SSE controls.
#[unsafe(no_mangle)]
extern "C" fn adv_pixel_x87_sqrt(bits: u32, controls: u32) -> u64 {
    use adv_cpu::floating::softfloat as sf;
    let cw = (controls >> 16) as u16;
    sf::configure(cw);
    let value = sf::from(u64::from(bits), 0);
    let value = sf::unary(value, 0);
    let flags = sf::flags();
    let double = sf::to(value, 1, sf::rounding(cw >> 10));
    let x87 = flags | sf::flags();
    sf::configure_sse(controls);
    let result = sf::sse_convert(double, 1, 0, sf::rounding((controls >> 13) as u16));
    result | u64::from(sf::flags()) << 32 | u64::from(x87) << 48
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: NAT-BODY-19
    #[test]
    fn shuffle_matches_scalar_conversion() {
        for width in 1..33 {
            for (source_bits, dest_bits) in [
                ([8; 4], [8; 4]),
                ([0, 8, 8, 8], [8; 4]),
                ([8; 4], [0, 8, 8, 8]),
            ] {
                for shifts in [[24, 0, 8, 16], [24, 16, 8, 0], [0, 8, 16, 24]] {
                    let sf = Format {
                        id: 1,
                        bits: source_bits,
                        shifts: [24, 16, 8, 0],
                        bytes: 4,
                        bw: 1,
                        bh: 1,
                        block: 4,
                        alpha: u32::from(source_bits[0] != 0),
                        rgb: 1,
                        flags: 0,
                    };
                    let df = Format {
                        id: 3,
                        bits: dest_bits,
                        shifts,
                        alpha: u32::from(dest_bits[0] != 0),
                        ..sf
                    };
                    let mut source: Vec<_> = (0..width * 4).map(|i| (i * 73) as u8).collect();
                    let mut actual = vec![0; source.len()];
                    let mut expected = actual.clone();
                    let mut src = Pixels {
                        data: source.as_mut_ptr(),
                        pitch: width * 4,
                        slice: width * 4,
                        palette: std::ptr::null(),
                        size: [width, 1, 1],
                        rect: [0, 0, width as i32, 1],
                    };
                    let mut dst = Pixels {
                        data: expected.as_mut_ptr(),
                        ..src
                    };
                    shuffle(&dst, &src, &df, &sf, actual.as_mut_ptr(), source.as_ptr());
                    // SAFETY: these owned buffers contain the complete rows.
                    assert_eq!(
                        unsafe {
                            adv_pixel_d3dx_load_pixels_from_pixels(
                                &mut dst, &df, &mut src, &sf, 2, 0,
                            )
                        },
                        0
                    );
                    assert_eq!(actual, expected, "width {width} shifts {shifts:?}");
                }
            }
        }
    }
}
