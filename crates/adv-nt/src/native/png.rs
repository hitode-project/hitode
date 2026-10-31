//! Pinned libpng row ABIs, with forward scalar filters on every host.
use adv_cpu::body::{Input, Mem, Ret, Stop};
use adv_machine::Conv;

#[derive(Clone, Copy)]
enum PngFilter {
    Sub,
    Up,
    Average,
    Paeth,
}

const ARGS: &[Input] = &[Input::Stack(0), Input::Stack(1), Input::Stack(2)];
adv_cpu::body!(SUB = sub, inputs: ARGS, outputs: 0, pop: 0);
adv_cpu::body!(UP = up, inputs: ARGS, outputs: 0, pop: 0);
adv_cpu::body!(AVERAGE = average, inputs: ARGS, outputs: 0, pop: 0);
adv_cpu::body!(PAETH_ONE = paeth_one, inputs: ARGS, outputs: 0, pop: 0);
adv_cpu::body!(PAETH = paeth, inputs: ARGS, outputs: 0, pop: 0);

pub(super) fn bodies() -> Vec<super::Body> {
    [
        ("png_read_filter_row_sub", SUB),
        ("png_read_filter_row_up", UP),
        ("png_read_filter_row_avg", AVERAGE),
        ("png_read_filter_row_paeth_1byte_pixel", PAETH_ONE),
        ("png_read_filter_row_paeth_multibyte_pixel", PAETH),
    ]
    .into_iter()
    .map(|(name, direct)| super::Body {
        name: format!("windowscodecs.dll!{name}").into(),
        conv: Conv::Cdecl { argc: 3 },
        body: None,
        handler: super::resume_original,
        direct: Some(direct),
    })
    .collect()
}
fn sub<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    filter(mem, args, PngFilter::Sub, false)
}
fn up<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    filter(mem, args, PngFilter::Up, false)
}
fn average<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    filter(mem, args, PngFilter::Average, false)
}
fn paeth_one<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    filter(mem, args, PngFilter::Paeth, true)
}
fn paeth<M: Mem>(mem: &mut M, args: [u32; 7]) -> Result<Ret, Stop> {
    filter(mem, args, PngFilter::Paeth, false)
}
// spec: NAT-BODY-9
#[inline(always)]
fn filter<M: Mem>(
    mem: &mut M,
    [info, row, previous, ..]: [u32; 7],
    kind: PngFilter,
    one: bool,
) -> Result<Ret, Stop> {
    // Wine's i386 png_row_info: rowbytes at 4, pixel_depth at 11 (DWARF).
    let len = mem.u32(info.wrapping_add(4))?;
    let bpp = if one {
        1
    } else if matches!(kind, PngFilter::Up) {
        0
    } else {
        let depth = mem.input(info.wrapping_add(11), 1)?;
        // SAFETY: the metadata byte was checked.
        u32::from(unsafe { *depth }).div_ceil(8)
    };
    if matches!(kind, PngFilter::Average) && len < bpp {
        return average_wrapped(mem, row, previous, len, bpp);
    }
    // The multibyte Paeth routine reads rowbytes after writing its first
    // pixel. Compute that pixel before admission when it aliases rowbytes.
    let length_at = info.wrapping_add(4);
    let len = if matches!(kind, PngFilter::Paeth)
        && !one
        && bpp != 0
        && u64::from(row) < u64::from(length_at) + 4
        && u64::from(length_at) < u64::from(row) + u64::from(bpp)
    {
        let input = mem.input(row, bpp)?;
        let above = mem.input(previous, bpp)?;
        let mut pixel = [0u8; 32];
        // SAFETY: both first-pixel spans were checked; earlier aliased
        // writes are read from the computed prefix in traversal order.
        unsafe {
            for i in 0..bpp as usize {
                let at = previous.wrapping_add(i as u32);
                let b = if at >= row && u64::from(at) < u64::from(row) + i as u64 {
                    pixel[(at - row) as usize]
                } else {
                    *above.add(i)
                };
                pixel[i] = (*input.add(i)).wrapping_add(b);
            }
        }
        let mut bytes = len.to_le_bytes();
        for (i, b) in bytes.iter_mut().enumerate() {
            let at = length_at.wrapping_add(i as u32);
            if at >= row && u64::from(at) < u64::from(row) + u64::from(bpp) {
                *b = pixel[(at - row) as usize];
            }
        }
        u32::from_le_bytes(bytes)
    } else {
        len
    };
    let len = if one {
        len.max(1)
    } else if matches!(kind, PngFilter::Paeth) {
        len.max(bpp)
    } else {
        len
    };
    let above_len = if matches!(kind, PngFilter::Sub) {
        0
    } else {
        len
    };
    let [output, above] = mem.spans([(row, len, true), (previous, above_len, false)])?;
    if matches!(kind, PngFilter::Paeth)
        && (u64::from(row) + u64::from(len) <= u64::from(previous)
            || u64::from(previous) + u64::from(len) <= u64::from(row))
    {
        let processed = match bpp {
            1 => {
                paeth_pixels::<1>(output, above, len as usize);
                true
            }
            2 => {
                paeth_pixels::<2>(output, above, len as usize);
                true
            }
            3 => {
                paeth_pixels::<3>(output, above, len as usize);
                true
            }
            4 => {
                paeth_pixels::<4>(output, above, len as usize);
                true
            }
            6 => {
                paeth_pixels::<6>(output, above, len as usize);
                true
            }
            8 => {
                paeth_pixels::<8>(output, above, len as usize);
                true
            }
            _ => false,
        };
        if processed {
            mem.commit()?;
            return Ok(0u32.into());
        }
    }
    // SAFETY: both complete spans were checked before writing. Raw pointers
    // preserve forward traversal when the previous row aliases the output.
    unsafe {
        let mut cached_above = 0;
        for i in 0..len as usize {
            let a = if i >= bpp as usize {
                *output.add(i - bpp as usize)
            } else {
                0
            };
            let b = if above_len != 0 { *above.add(i) } else { 0 };
            let c = if one {
                cached_above
            } else if above_len != 0 && i >= bpp as usize {
                *above.add(i - bpp as usize)
            } else {
                0
            };
            let prediction = match kind {
                PngFilter::Sub => a,
                PngFilter::Up => b,
                PngFilter::Average => ((u16::from(a) + u16::from(b)) / 2) as u8,
                PngFilter::Paeth => {
                    let (a, b, c) = (i32::from(a), i32::from(b), i32::from(c));
                    let p = a + b - c;
                    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                    if pa <= pb && pa <= pc {
                        a as u8
                    } else if pb <= pc {
                        b as u8
                    } else {
                        c as u8
                    }
                }
            };
            cached_above = b;
            *output.add(i) = (*output.add(i)).wrapping_add(prediction);
        }
    }
    mem.commit()?;
    Ok(0u32.into())
}

#[inline]
fn predictor(a: u8, b: u8, c: u8) -> u8 {
    let x = i16::from(b) - i16::from(c);
    let y = i16::from(a) - i16::from(c);
    let (pa, pb, pc) = (x.abs(), y.abs(), (x + y).abs());
    let (best, min) = if pb < pa { (b, pb) } else { (a, pa) };
    if pc < min { c } else { best }
}
fn paeth_pixels<const N: usize>(row: *mut u8, above: *const u8, len: usize) {
    let mut left = [0; N];
    let mut corner = [0; N];
    // SAFETY: filter checked both disjoint complete rows, with at least N bytes.
    unsafe {
        for i in 0..N {
            corner[i] = *above.add(i);
            left[i] = (*row.add(i)).wrapping_add(corner[i]);
            *row.add(i) = left[i];
        }
        let mut at = N;
        while at + N <= len {
            for i in 0..N {
                let b = *above.add(at + i);
                let out = (*row.add(at + i)).wrapping_add(predictor(left[i], b, corner[i]));
                *row.add(at + i) = out;
                left[i] = out;
                corner[i] = b;
            }
            at += N;
        }
        for i in 0..len - at {
            *row.add(at + i) =
                (*row.add(at + i)).wrapping_add(predictor(left[i], *above.add(at + i), corner[i]));
        }
    }
}

// i386 size_t subtraction wraps for an Average row shorter than one pixel.
// Check its complete traversal before writing, including the address wrap.
fn average_wrapped<M: Mem>(
    mem: &mut M,
    row: u32,
    previous: u32,
    len: u32,
    bpp: u32,
) -> Result<Ret, Stop> {
    let total = (1u64 << 32) + u64::from(len);
    let mut done = 0;
    while done < total {
        let at = row.wrapping_add(done as u32);
        let above = previous.wrapping_add(done as u32);
        let count = (4096 - (at & 4095))
            .min(4096 - (above & 4095))
            .min((total - done).min(u64::from(u32::MAX)) as u32);
        mem.input(at, count)?;
        mem.input(above, count)?;
        mem.output(at, count)?;
        done += u64::from(count);
    }
    let halves = mem.spans([(0, 0x80000000, true), (0x80000000, 0x80000000, true)])?;
    let pointer = |at: u32| halves[(at >> 31) as usize].wrapping_add((at & 0x7fffffff) as usize);
    // SAFETY: the complete address space was admitted. Both rows use the same
    // two views, so wrapped and overlapping history observes preceding writes.
    unsafe {
        for i in 0..total {
            let at = row.wrapping_add(i as u32);
            let p = pointer(at);
            let a = if i < u64::from(bpp) {
                0
            } else {
                *pointer(at.wrapping_sub(bpp))
            };
            let b = *pointer(previous.wrapping_add(i as u32));
            *p = (*p).wrapping_add(((u16::from(a) + u16::from(b)) / 2) as u8);
        }
    }
    mem.commit()?;
    Ok(0u32.into())
}
