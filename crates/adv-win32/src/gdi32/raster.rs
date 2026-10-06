//! GDI raster calls on the shared surface store: window and memory DCs, DIB sections and
//! `BitBlt`. Pixels live in [`Surfaces`]; DCs in [`DeviceContexts`], which DirectDraw
//! surfaces also lend DCs from.
use adv_device::surface::{
    BlitOptions, BlitRect, Presenter, Rect, Sampling, Surface, SurfaceMemory, Surfaces,
};
use adv_os::gdi::{DeviceContexts, GdiObj, STOCK_BITMAP, WINDOW_DC};

pub(crate) mod clip;
mod color_table;
mod dib;
mod fill;
pub mod front;
pub use front::{DrawingProvider, install, register, set_provider};

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
    pub const STRETCH_DIB: u32 = 112;
}

#[derive(Default)]
pub struct Raster {
    last_error: u32,
}

impl Raster {
    pub fn create_display_dc<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        memory: &mut dyn SurfaceMemory,
    ) -> Result<u32, String> {
        // Creating a device-name DC does not change the display mode or resize
        // an existing client canvas. All DCs borrow the current shared pixels.
        let primary = surfaces.primary(memory)?;
        let dc = surfaces.new_id();
        if !dcs.create(objects, dc, primary) {
            return Ok(self.fail(8));
        }
        Ok(dc)
    }
    /// Last failing raster operation, for diagnostics; ABI handlers update the TEB.
    pub fn last_error(&self) -> u32 {
        self.last_error
    }
    fn fail(&mut self, code: u32) -> u32 {
        self.last_error = code;
        0
    }
}

// Named raster operations; typed fronts never reconstruct an argument array.
#[allow(clippy::too_many_arguments)]
impl Raster {
    pub fn create_dc<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        _m: &mut dyn SurfaceMemory,
        _source: u32,
    ) -> Result<u32, String> {
        Ok({
            let id = surfaces.new_id();
            if !dcs.create(objects, id, STOCK_BITMAP) {
                return Ok(self.fail(8));
            }
            id
        })
    }
    pub fn create_dib<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        m: &mut dyn SurfaceMemory,
        _dc: u32,
        info: u32,
        usage: u32,
        bits_out: u32,
        section: u32,
        _offset: u32,
    ) -> Result<u32, String> {
        Ok({
            m.put_u32(bits_out, 0)?;
            if section != 0 {
                return Ok(self.fail(87));
            }
            let Some(dib) = dib::read(m, info, usage)? else {
                return Ok(self.fail(87));
            };
            self.create_decoded_dib(surfaces, dcs, objects, m, dib, bits_out)?
        })
    }
    fn create_decoded_dib<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        m: &mut dyn SurfaceMemory,
        dib: dib::Dib,
        bits_out: u32,
    ) -> Result<u32, String> {
        m.put_u32(bits_out, 0)?;
        Ok({
            let (w, height, format) = (
                dib.width as i32,
                if dib.bottom_up {
                    dib.height as i32
                } else {
                    -(dib.height as i32)
                },
                dib.format,
            );
            let palette = dib.palette;
            let bitmap = surfaces.create(w as u32, height.unsigned_abs(), height > 0, format, m)?;
            if let Some(palette) = palette {
                surfaces.set_palette(bitmap, 0, palette.as_ref())?;
            }
            let surface = surfaces.get(bitmap).unwrap();
            let bits = surface.pixels;
            let metadata = GdiObj::Bitmap {
                width: w,
                height,
                bpp: (format.bytes() * 8) as u16,
                bits,
                stride: surface.pitch,
                owned: false,
                mapping_view: None,
                dib_colors: dib.palette_count,
            };
            if !dcs.register_bitmap(objects, bitmap, metadata) {
                surfaces.delete(bitmap);
                return Ok(self.fail(8));
            }
            if let Err(error) = m.put_u32(bits_out, bits) {
                dcs.remove_bitmap(objects, bitmap);
                surfaces.delete(bitmap);
                return Err(error);
            }
            bitmap
        })
    }
    pub fn select_bitmap<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        _m: &mut dyn SurfaceMemory,
        dc: u32,
        bitmap: u32,
    ) -> Result<u32, String> {
        Ok({
            if !dcs.contains(objects, dc) || (bitmap != STOCK_BITMAP && !surfaces.contains(bitmap))
            {
                return Ok(self.fail(6));
            }
            if dcs.get(objects, dc) == surfaces.primary_id() {
                // A display DC draws into the display. Bitmap selection belongs
                // to a memory DC and must not replace the display's target.
                return Ok(self.fail(87));
            }
            if bitmap != STOCK_BITMAP && dcs.selects(objects, bitmap, Some(dc)) {
                return Ok(self.fail(87));
            }
            dcs.select(objects, dc, bitmap).unwrap()
        })
    }
    pub fn delete_dc<P: Presenter>(
        &mut self,
        _surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        _m: &mut dyn SurfaceMemory,
        dc: u32,
    ) -> Result<u32, String> {
        Ok(u32::from(dcs.remove(objects, dc).is_some()))
    }
    pub fn set_stretch_mode<P: Presenter>(
        &mut self,
        _surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        _m: &mut dyn SurfaceMemory,
        dc: u32,
        mode: u32,
    ) -> Result<u32, String> {
        Ok({
            if dc != WINDOW_DC && !dcs.contains(objects, dc) {
                return Ok(self.fail(6));
            }
            if !(1..=3).contains(&mode) {
                return Ok(self.fail(87));
            }
            dcs.set_stretch_mode(objects, dc, mode)
                .unwrap_or_else(|| self.fail(8))
        })
    }
    pub fn delete_bitmap<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        _m: &mut dyn SurfaceMemory,
        bitmap: u32,
    ) -> Result<u32, String> {
        Ok({
            if dcs.selects(objects, bitmap, None) || surfaces.primary_id() == Some(bitmap) {
                return Ok(self.fail(170));
            }
            if surfaces.delete(bitmap) {
                dcs.remove_bitmap(objects, bitmap);
                1
            } else {
                self.fail(6)
            }
        })
    }
    pub fn bit_blt<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        m: &mut dyn SurfaceMemory,
        destination_dc: u32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        source_dc: u32,
        source_x: u32,
        source_y: u32,
        rop: u32,
    ) -> Result<u32, String> {
        Ok({
            if rop != 0x00cc0020 {
                return Ok(self.fail(87));
            }
            let Some(src) = dcs.get(objects, source_dc) else {
                return Ok(self.fail(6));
            };
            let dst = if destination_dc == WINDOW_DC {
                surfaces.primary(m)?
            } else {
                dcs.get(objects, destination_dc).unwrap_or(0)
            };
            let (Some(s), Some(d)) = (surfaces.get(src), surfaces.get(dst)) else {
                return Ok(self.fail(6));
            };
            let (Some(dm), Some(sm)) = (
                dcs.modes(objects, destination_dc),
                dcs.modes(objects, source_dc),
            ) else {
                return Ok(self.fail(6));
            };
            if dm.map != 1 || sm.map != 1 {
                return Ok(self.fail(50));
            }
            let [x, y] = dm.to_device(i64::from(x as i32), i64::from(y as i32));
            let [sx, sy] = sm.to_device(i64::from(source_x as i32), i64::from(source_y as i32));
            let (x, y, w, h, sx, sy) = (x, y, width as i32 as i64, height as i32 as i64, sx, sy);
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
            let clip = dcs.clip(objects, destination_dc);
            let surface = surfaces.get(dst).unwrap().clone();
            let mut m = clip::ClippedMemory::new(m, surface, &clip);
            surfaces.copy(src, sr, dst, dr, &mut m)?;
            if destination_dc == WINDOW_DC {
                surfaces.present(dst, &m)?;
            }
            1
        })
    }
    pub fn stretch_dib<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        m: &mut dyn SurfaceMemory,
        destination_dc: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        source_x: i32,
        source_y: i32,
        source_width: i32,
        source_height: i32,
        bits: u32,
        info: u32,
        usage: u32,
        rop: u32,
    ) -> Result<u32, String> {
        if rop != 0x00cc0020 || bits == 0 {
            return Ok(self.fail(87));
        }
        let Some(dib) = dib::read(m, info, usage)? else {
            return Ok(self.fail(87));
        };
        let dst = if destination_dc == WINDOW_DC {
            surfaces.primary(m)?
        } else {
            dcs.get(objects, destination_dc).unwrap_or(0)
        };
        if !surfaces.contains(dst) {
            return Ok(self.fail(6));
        }
        if source_width == 0 || source_height == 0 {
            return Ok(0);
        }
        if width == 0 || height == 0 {
            // Native reports the input DIB scan count for a zero destination;
            // no pixels are accessed and no completed frame is produced.
            return Ok(dib.height);
        }
        let Some(modes) = dcs.modes(objects, destination_dc) else {
            return Ok(self.fail(6));
        };
        if modes.map != 1 {
            return Ok(self.fail(50));
        }
        let Some([x, y]) = modes.device_point(x, y) else {
            return Ok(self.fail(50));
        };
        let Some(transfer) = dib::transfer(
            &dib,
            x,
            y,
            width,
            height,
            source_x,
            source_y,
            source_width,
            source_height,
        ) else {
            return Ok(self.fail(87));
        };
        let source = Surface {
            pixels: bits,
            width: dib.width,
            height: dib.height,
            pitch: dib.pitch,
            bottom_up: dib.bottom_up,
            locked: None,
            version: 0,
            format: dib.format,
            palette: dib.palette,
        };
        let options = BlitOptions {
            mirror_x: transfer.mirror_x,
            mirror_y: transfer.mirror_y,
            sampling: match dcs.stretch_mode(objects, destination_dc) {
                2 => Sampling::OrColour,
                3 => Sampling::PixelCentre,
                _ => Sampling::AndColour,
            },
        };
        let clip = dcs.clip(objects, destination_dc);
        let surface = surfaces.get(dst).unwrap().clone();
        let mut m = clip::ClippedMemory::new(m, surface, &clip);
        surfaces.blit_from(
            &source,
            transfer.source,
            dst,
            transfer.destination,
            options,
            &mut m,
        )?;
        if destination_dc == WINDOW_DC {
            surfaces.present(dst, &m)?;
        }
        Ok(transfer.scan_lines)
    }

    pub fn stretch_blt<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        m: &mut dyn SurfaceMemory,
        destination_dc: u32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        source_dc: u32,
        source_x: u32,
        source_y: u32,
        source_width: u32,
        source_height: u32,
        rop: u32,
    ) -> Result<u32, String> {
        Ok({
            if rop != 0x00cc0020 {
                return Ok(self.fail(87));
            }
            let src = if source_dc == WINDOW_DC {
                surfaces.primary(m)?
            } else {
                dcs.get(objects, source_dc).unwrap_or(0)
            };
            let dst = if destination_dc == WINDOW_DC {
                surfaces.primary(m)?
            } else {
                dcs.get(objects, destination_dc).unwrap_or(0)
            };
            let (Some(s), Some(_d)) = (surfaces.get(src), surfaces.get(dst)) else {
                return Ok(self.fail(6));
            };
            let (Some(dm), Some(sm)) = (
                dcs.modes(objects, destination_dc),
                dcs.modes(objects, source_dc),
            ) else {
                return Ok(self.fail(6));
            };
            if dm.map != 1 || sm.map != 1 {
                return Ok(self.fail(50));
            }
            let [x, y] = dm.to_device(i64::from(x as i32), i64::from(y as i32));
            let [sx, sy] = sm.to_device(i64::from(source_x as i32), i64::from(source_y as i32));
            let (x, y, w, h, sx, sy, sw, sh) = (
                x,
                y,
                width as i32,
                height as i32,
                sx,
                sy,
                source_width as i32,
                source_height as i32,
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
                sampling: match dcs.stretch_mode(objects, destination_dc) {
                    2 => Sampling::Or,
                    3 => Sampling::Nearest,
                    _ => Sampling::And,
                },
            };
            let clip = dcs.clip(objects, destination_dc);
            let surface = surfaces.get(dst).unwrap().clone();
            let mut m = clip::ClippedMemory::new(m, surface, &clip);
            surfaces.blit(src, sr, dst, dr, options, &mut m)?;
            if destination_dc == WINDOW_DC {
                surfaces.present(dst, &m)?;
            }
            1
        })
    }
}
