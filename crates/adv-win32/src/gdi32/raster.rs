//! GDI raster calls on the shared surface store: window and memory DCs, DIB sections and
//! `BitBlt`. Pixels live in [`Surfaces`]; DCs in [`DeviceContexts`], which DirectDraw
//! surfaces also lend DCs from.
use adv_device::surface::{
    BlitOptions, BlitRect, PixelFormat, Presenter, Rect, Sampling, SurfaceMemory, Surfaces, size,
};
use adv_os::gdi::{DeviceContexts, STOCK_BITMAP, WINDOW_DC};
use std::collections::BTreeMap;

pub mod api {
    pub const GET_DC: u32 = 100;
    pub const CREATE_DC: u32 = 101;
    pub const CREATE_DIB: u32 = 102;
    pub const SELECT: u32 = 103;
    pub const BITBLT: u32 = 104;
    pub const DELETE_OBJECT: u32 = 105;
    pub const DELETE_DC: u32 = 106;
    pub const FLUSH: u32 = 107;
    pub const LAST_ERROR: u32 = 108;
    pub const RELEASE_DC: u32 = 109;
    pub const STRETCHBLT: u32 = 110;
    pub const SET_STRETCH_MODE: u32 = 111;
}

/// (id, module, name, argc) of every raster call.
pub const API_SPECS: &[(u32, &str, &str, u8)] = &[
    (api::GET_DC, "user32.dll", "GetDC", 1),
    (api::CREATE_DC, "gdi32.dll", "CreateCompatibleDC", 1),
    (api::CREATE_DIB, "gdi32.dll", "CreateDIBSection", 6),
    (api::SELECT, "gdi32.dll", "SelectObject", 2),
    (api::BITBLT, "gdi32.dll", "BitBlt", 9),
    (api::DELETE_OBJECT, "gdi32.dll", "DeleteObject", 1),
    (api::DELETE_DC, "gdi32.dll", "DeleteDC", 1),
    (api::FLUSH, "gdi32.dll", "GdiFlush", 0),
    (api::LAST_ERROR, "kernel32.dll", "GetLastError", 0),
    (api::RELEASE_DC, "user32.dll", "ReleaseDC", 2),
    (api::STRETCHBLT, "gdi32.dll", "StretchBlt", 11),
    (api::SET_STRETCH_MODE, "gdi32.dll", "SetStretchBltMode", 2),
];

#[derive(Default)]
pub struct Raster {
    last_error: u32,
    stretch_modes: BTreeMap<u32, Sampling>,
}

impl Raster {
    fn fail(&mut self, code: u32) -> u32 {
        self.last_error = code;
        0
    }

    /// Run raster call `id` with arguments `a`.
    pub fn call<P: Presenter>(
        &mut self,
        id: u32,
        a: &[u32],
        surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        m: &mut dyn SurfaceMemory,
    ) -> Result<u32, String> {
        Ok(match id {
            api::GET_DC => {
                surfaces.primary(m)?;
                WINDOW_DC
            }
            api::RELEASE_DC => u32::from(a[1] == WINDOW_DC),
            api::LAST_ERROR => self.last_error,
            api::FLUSH => 1,
            api::CREATE_DC => {
                let id = surfaces.new_id();
                if !dcs.create(objects, id, STOCK_BITMAP) {
                    return Ok(self.fail(8));
                }
                id
            }
            api::CREATE_DIB => {
                m.put_u32(a[3], 0)?;
                let h = m.read(a[1], 40)?;
                let w = i32::from_le_bytes(h[4..8].try_into().unwrap());
                let height = i32::from_le_bytes(h[8..12].try_into().unwrap());
                if u32::from_le_bytes(h[..4].try_into().unwrap()) != 40
                    || w <= 0
                    || height == 0
                    || height == i32::MIN
                    || h[12..14] != 1u16.to_le_bytes()
                    || !matches!(u16::from_le_bytes(h[14..16].try_into().unwrap()), 16 | 32)
                    || h[16..20] != [0; 4]
                    || a[2] != 0
                    || a[4] != 0
                {
                    return Ok(self.fail(87));
                }
                if size(w as u32, height.unsigned_abs()).is_err() {
                    return Ok(self.fail(87));
                }
                let format = if h[14..16] == 16u16.to_le_bytes() {
                    PixelFormat::Rgb555
                } else {
                    PixelFormat::Bgra32
                };
                let bitmap =
                    surfaces.create(w as u32, height.unsigned_abs(), height > 0, format, m)?;
                m.put_u32(a[3], surfaces.get(bitmap).unwrap().pixels)?;
                bitmap
            }
            api::SELECT => {
                if !dcs.contains(objects, a[0])
                    || (a[1] != STOCK_BITMAP && !surfaces.contains(a[1]))
                {
                    return Ok(self.fail(6));
                }
                if a[1] != STOCK_BITMAP && dcs.selects(objects, a[1], Some(a[0])) {
                    return Ok(self.fail(87));
                }
                dcs.select(objects, a[0], a[1]).unwrap()
            }
            api::DELETE_DC => {
                self.stretch_modes.remove(&a[0]);
                u32::from(dcs.remove(objects, a[0]).is_some())
            }
            api::SET_STRETCH_MODE => {
                if a[0] != WINDOW_DC && !dcs.contains(objects, a[0]) {
                    return Ok(self.fail(6));
                }
                let mode = match a[1] {
                    1 => Sampling::And,
                    2 => Sampling::Or,
                    3 => Sampling::Nearest,
                    _ => return Ok(self.fail(87)),
                };
                match self
                    .stretch_modes
                    .insert(a[0], mode)
                    .unwrap_or(Sampling::And)
                {
                    Sampling::And => 1,
                    Sampling::Or => 2,
                    Sampling::Nearest => 3,
                }
            }
            api::DELETE_OBJECT => {
                if dcs.selects(objects, a[0], None) || surfaces.primary_id() == Some(a[0]) {
                    return Ok(self.fail(170));
                }
                if surfaces.delete(a[0]) {
                    1
                } else {
                    self.fail(6)
                }
            }
            api::BITBLT => {
                if a[8] != 0x00cc0020 {
                    return Ok(self.fail(87));
                }
                let Some(src) = dcs.get(objects, a[5]) else {
                    return Ok(self.fail(6));
                };
                let dst = if a[0] == WINDOW_DC {
                    surfaces.primary(m)?
                } else {
                    dcs.get(objects, a[0]).unwrap_or(0)
                };
                let (Some(s), Some(d)) = (surfaces.get(src), surfaces.get(dst)) else {
                    return Ok(self.fail(6));
                };
                let (x, y, w, h, sx, sy) = (
                    a[1] as i32 as i64,
                    a[2] as i32 as i64,
                    a[3] as i32 as i64,
                    a[4] as i32 as i64,
                    a[6] as i32 as i64,
                    a[7] as i32 as i64,
                );
                if w < 0 || h < 0 {
                    return Ok(self.fail(87));
                }
                let (left, top, right, bottom) = (
                    x.max(0),
                    y.max(0),
                    (x + w).min(i64::from(d.width)),
                    (y + h).min(i64::from(d.height)),
                );
                if right <= left || bottom <= top {
                    return Ok(1);
                }
                let (sx, sy) = (sx + left - x, sy + top - y);
                if sx < 0
                    || sy < 0
                    || sx + right - left > i64::from(s.width)
                    || sy + bottom - top > i64::from(s.height)
                {
                    return Ok(self.fail(87));
                }
                let sr = Rect {
                    x: sx as u32,
                    y: sy as u32,
                    w: (right - left) as u32,
                    h: (bottom - top) as u32,
                };
                let dr = Rect {
                    x: left as u32,
                    y: top as u32,
                    ..sr
                };
                surfaces.copy(src, sr, dst, dr, m)?;
                if a[0] == WINDOW_DC {
                    surfaces.present(dst, m)?;
                }
                1
            }
            api::STRETCHBLT => {
                if a[10] != 0x00cc0020 {
                    return Ok(self.fail(87));
                }
                let src = if a[5] == WINDOW_DC {
                    surfaces.primary(m)?
                } else {
                    dcs.get(objects, a[5]).unwrap_or(0)
                };
                let dst = if a[0] == WINDOW_DC {
                    surfaces.primary(m)?
                } else {
                    dcs.get(objects, a[0]).unwrap_or(0)
                };
                let (Some(s), Some(_d)) = (surfaces.get(src), surfaces.get(dst)) else {
                    return Ok(self.fail(6));
                };
                let (x, y, w, h, sx, sy, sw, sh) = (
                    i64::from(a[1] as i32),
                    i64::from(a[2] as i32),
                    a[3] as i32,
                    a[4] as i32,
                    i64::from(a[6] as i32),
                    i64::from(a[7] as i32),
                    a[8] as i32,
                    a[9] as i32,
                );
                if w == 0 || h == 0 || sw == 0 || sh == 0 {
                    return Ok(1);
                }
                let left = sx + i64::from(sw.min(0));
                let top = sy + i64::from(sh.min(0));
                if left < 0
                    || top < 0
                    || left + i64::from(sw.unsigned_abs()) > i64::from(s.width)
                    || top + i64::from(sh.unsigned_abs()) > i64::from(s.height)
                {
                    return Ok(self.fail(87));
                }
                let sr = Rect {
                    x: left as u32,
                    y: top as u32,
                    w: sw.unsigned_abs(),
                    h: sh.unsigned_abs(),
                };
                let dr = BlitRect {
                    x: x + i64::from(w.min(0)),
                    y: y + i64::from(h.min(0)),
                    w: w.unsigned_abs(),
                    h: h.unsigned_abs(),
                };
                let options = BlitOptions {
                    mirror_x: (w < 0) != (sw < 0),
                    mirror_y: (h < 0) != (sh < 0),
                    sampling: self
                        .stretch_modes
                        .get(&a[0])
                        .copied()
                        .unwrap_or(Sampling::And),
                };
                surfaces.blit(src, sr, dst, dr, options, m)?;
                if a[0] == WINDOW_DC {
                    surfaces.present(dst, m)?;
                }
                1
            }
            _ => return Err(format!("unknown GDI API {id}")),
        })
    }
}
