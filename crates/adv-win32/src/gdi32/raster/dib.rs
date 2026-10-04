//! One BITMAPINFO decoder for DIB allocation and borrowed raster input.
use adv_device::surface::{PixelFormat, SurfaceMemory, size};
use std::sync::Arc;

pub(super) struct Dib {
    pub width: u32,
    pub height: u32,
    pub bottom_up: bool,
    pub format: PixelFormat,
    pub pitch: u32,
    pub palette: Option<Arc<[[u8; 4]; 256]>>,
    pub palette_count: u16,
}

pub(super) struct Transfer {
    pub source: adv_device::surface::Rect,
    pub destination: adv_device::surface::BlitRect,
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub scan_lines: u32,
}

/// Win32's uncompressed DIB transfer coordinates, verified with authored native
/// 8/16/24/32-bit memory-DC queries. Keep this observable front convention out
/// of the neutral top-left surface kernel.
#[allow(clippy::too_many_arguments)]
pub(super) fn transfer(
    dib: &Dib,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    sx: i32,
    sy: i32,
    sw: i32,
    sh: i32,
) -> Option<Transfer> {
    let direct =
        sx == 0 && sy == 0 && sw > 0 && sh > 0 && sw as u32 == dib.width && w == sw && h == sh;
    let left = i64::from(sx) + if sw < 0 { i64::from(sw) + 1 } else { 0 };
    let top = if direct && !dib.bottom_up {
        0
    } else {
        i64::from(dib.height) - i64::from(sy) - i64::from(sh)
            + if sh < 0 { i64::from(sh) + 1 } else { 0 }
    };
    if left < 0
        || top < 0
        || left + i64::from(sw.unsigned_abs()) > i64::from(dib.width)
        || top + i64::from(sh.unsigned_abs()) > i64::from(dib.height)
    {
        return None;
    }
    Some(Transfer {
        source: adv_device::surface::Rect {
            x: left as u32,
            y: top as u32,
            w: sw.unsigned_abs(),
            h: sh.unsigned_abs(),
        },
        destination: adv_device::surface::BlitRect {
            x: i64::from(x) + if w < 0 { i64::from(w) + 1 } else { 0 },
            y: i64::from(y) + if h < 0 { i64::from(h) + 1 } else { 0 },
            w: w.unsigned_abs(),
            h: h.unsigned_abs(),
        },
        mirror_x: (w < 0) != (sw < 0),
        mirror_y: (h < 0) != (sh < 0),
        scan_lines: if direct { sh as u32 } else { dib.height },
    })
}

/// Invalid/unsupported layouts are distinct from failed guest memory reads.
pub(super) fn read(
    memory: &dyn SurfaceMemory,
    info: u32,
    usage: u32,
) -> Result<Option<Dib>, String> {
    let h = memory.read(info, 40)?;
    let width = i32::from_le_bytes(h[4..8].try_into().unwrap());
    let height = i32::from_le_bytes(h[8..12].try_into().unwrap());
    let bpp = u16::from_le_bytes(h[14..16].try_into().unwrap());
    let compression = u32::from_le_bytes(h[16..20].try_into().unwrap());
    let used = u32::from_le_bytes(h[32..36].try_into().unwrap());
    if u32::from_le_bytes(h[..4].try_into().unwrap()) != 40
        || width <= 0
        || height == 0
        || height == i32::MIN
        || h[12..14] != 1u16.to_le_bytes()
        || !matches!(bpp, 8 | 16 | 24 | 32)
        || !(compression == 0 || (compression == 3 && bpp == 16))
        || usage != 0
        || size(width as u32, height.unsigned_abs()).is_err()
    {
        return Ok(None);
    }
    let palette = if bpp == 8 {
        let count = if used == 0 { 256 } else { used };
        if count > 256 {
            return Ok(None);
        }
        let Some(address) = info.checked_add(40) else {
            return Ok(None);
        };
        let colors = memory.read(address, count as usize * 4)?;
        let mut palette = [[0; 4]; 256];
        for (entry, color) in palette.iter_mut().zip(colors.as_chunks::<4>().0) {
            *entry = [color[0], color[1], color[2], 0]; // RGBQUAD reserved byte is not alpha.
        }
        Some(Arc::new(palette))
    } else {
        None
    };
    let format = match bpp {
        8 => PixelFormat::Indexed8,
        16 if compression == 0 => PixelFormat::Rgb555,
        16 => {
            let Some(address) = info.checked_add(40) else {
                return Ok(None);
            };
            let masks = memory.read(address, 12)?;
            let mask = |at| u32::from_le_bytes(masks[at..at + 4].try_into().unwrap());
            match (mask(0), mask(4), mask(8)) {
                (0x7c00, 0x3e0, 0x1f) => PixelFormat::Rgb555,
                (0xf800, 0x7e0, 0x1f) => PixelFormat::Rgb565,
                _ => return Ok(None),
            }
        }
        24 => PixelFormat::Bgr24,
        _ => PixelFormat::Bgra32,
    };
    let pitch = (width as u32 * format.bytes() + 3) & !3;
    Ok(Some(Dib {
        width: width as u32,
        height: height.unsigned_abs(),
        bottom_up: height > 0,
        format,
        pitch,
        palette,
        palette_count: if bpp == 8 { 256 } else { 0 },
    }))
}
