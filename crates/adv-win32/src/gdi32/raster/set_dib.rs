//! SetDIBitsToDevice's scan-band convention belongs to the GDI personality.
use super::*;

#[allow(clippy::too_many_arguments)]
impl Raster {
    pub(super) fn set_dibits_to_device<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        objects: &mut adv_machine::Objects,
        memory: &mut dyn SurfaceMemory,
        dc: u32,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        sx: i32,
        sy: i32,
        start: u32,
        lines: u32,
        bits: u32,
        dib: dib::Dib,
    ) -> Result<u32, String> {
        let dst = if dcs.is_window_dc(objects, dc) {
            surfaces.primary(memory)?
        } else {
            dcs.get(objects, dc).unwrap_or(0)
        };
        if !surfaces.contains(dst) {
            return Ok(self.fail(6));
        }
        let Some(modes) = dcs.modes(objects, dc) else {
            return Ok(self.fail(6));
        };
        if modes.map != 1 {
            return Ok(self.fail(50));
        }
        let Some([x, y]) = modes.device_point(x, y) else {
            return Ok(self.fail(50));
        };
        // The supplied array contains this band only, not the whole bitmap.
        // Both orientations locate the band using bottom-origin scan indices;
        // the sign of biHeight controls byte-row order within the band.
        let left = i64::from(sx).max(0);
        let right = (i64::from(sx) + i64::from(width)).min(i64::from(dib.width));
        let low = i64::from(sy).max(i64::from(start));
        let high = (i64::from(sy) + i64::from(height)).min(i64::from(start) + i64::from(lines));
        let result = if dib.bottom_up {
            (high - i64::from(start)).max(0) as u32
        } else {
            lines
        };
        if lines == 0 || right <= left || high <= low {
            return Ok(result);
        }
        let source = Surface {
            pixels: bits,
            width: dib.width,
            height: lines,
            pitch: dib.pitch,
            bottom_up: dib.bottom_up,
            locked: None,
            version: 0,
            format: dib.format,
            palette: dib.palette,
        };
        let source_rect = Rect {
            x: left as u32,
            y: (i64::from(start) + i64::from(lines) - high) as u32,
            w: (right - left) as u32,
            h: (high - low) as u32,
        };
        let destination = BlitRect {
            x: i64::from(x) + left - i64::from(sx),
            y: i64::from(y) + i64::from(height) + i64::from(sy) - high,
            w: source_rect.w,
            h: source_rect.h,
        };
        let clip = dcs.clip(objects, dc);
        let surface = surfaces.get(dst).unwrap().clone();
        let mut memory = clip::ClippedMemory::new(memory, surface, &clip);
        surfaces.blit_from(
            &source,
            source_rect,
            dst,
            destination,
            BlitOptions {
                mirror_x: false,
                mirror_y: false,
                sampling: Sampling::Nearest,
                key: None,
            },
            &mut memory,
        )?;
        if dcs.is_window_dc(objects, dc) {
            surfaces.present_from(self.context, dst, &memory, memory.damage())?;
        }
        Ok(result)
    }
}
