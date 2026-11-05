//! The CPU reference for software presentation, shared by snapshot conversion.
use super::{PixelFormat, Rect, Snapshot};

pub(super) const BLACK_PALETTE: [[u8; 4]; 256] = [[0; 4]; 256];

pub(super) fn decode_pixel(
    format: PixelFormat,
    palette: Option<&[[u8; 4]; 256]>,
    raw: &[u8],
) -> [u8; 4] {
    if format.indexed() {
        palette.unwrap_or(&BLACK_PALETTE)[raw[0] as usize]
    } else {
        format.decode(raw)
    }
}

pub(super) fn index(format: PixelFormat, row: &[u8], x: u32) -> u8 {
    let value = row[format.byte_offset(x) as usize];
    match format {
        PixelFormat::Indexed1 => (value >> (7 - x % 8)) & 1,
        PixelFormat::Indexed4 => (value >> (4 - (x % 2) * 4)) & 15,
        _ => value,
    }
}

pub(crate) fn convert(
    format: PixelFormat,
    palette: Option<&[[u8; 4]; 256]>,
    width: u32,
    pitch: u32,
    pixels: &[u8],
    out: &mut Snapshot,
    damage: &[Rect],
) {
    for r in damage {
        for y in r.y..r.y + r.h {
            let src = &pixels[y as usize * pitch as usize..];
            let start = (y as usize * width as usize + r.x as usize) * 4;
            let dst = &mut out.bgra[start..start + r.w as usize * 4];
            if format == PixelFormat::Bgra32 {
                dst.copy_from_slice(&src[r.x as usize * 4..(r.x + r.w) as usize * 4]);
            } else {
                for (i, pixel) in dst.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let x = r.x + i as u32;
                    let raw = if format.packed() {
                        &[index(format, src, x)][..]
                    } else {
                        &src[format.byte_offset(x) as usize..]
                    };
                    pixel.copy_from_slice(&decode_pixel(format, palette, raw));
                }
            }
        }
    }
}
