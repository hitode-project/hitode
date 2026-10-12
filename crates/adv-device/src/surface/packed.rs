//! Packed indices are expanded only while operating on a bounded pixel row.
//! The surface retains its real bit-packed backing, pitch and palette identity.
use super::*;

impl PixelFormat {
    pub fn bits(self) -> u32 {
        match self {
            Self::Indexed1 => 1,
            Self::Indexed4 => 4,
            _ => self.bytes() * 8,
        }
    }
    pub fn indexed(self) -> bool {
        matches!(self, Self::Indexed1 | Self::Indexed4 | Self::Indexed8)
    }
    pub fn packed(self) -> bool {
        matches!(self, Self::Indexed1 | Self::Indexed4)
    }
    pub fn colors(self) -> usize {
        if self.indexed() {
            1usize << self.bits()
        } else {
            0
        }
    }
    pub fn row_bytes(self, width: u32) -> u32 {
        (u64::from(width) * u64::from(self.bits())).div_ceil(8) as u32
    }
    pub fn byte_offset(self, x: u32) -> u32 {
        (u64::from(x) * u64::from(self.bits()) / 8) as u32
    }
    pub(super) fn index(self, row: &[u8], x: u32) -> u8 {
        let value = row[self.byte_offset(x) as usize];
        match self {
            Self::Indexed1 => (value >> (7 - x % 8)) & 1,
            Self::Indexed4 => (value >> (4 - (x % 2) * 4)) & 15,
            _ => value,
        }
    }
    fn put_index(self, row: &mut [u8], x: u32, value: u8) {
        let bits = self.bits();
        let shift = 8 - bits - (x * bits) % 8;
        let mask = ((1u16 << bits) - 1) as u8;
        let byte = &mut row[self.byte_offset(x) as usize];
        *byte = (*byte & !(mask << shift)) | ((value & mask) << shift);
    }
}

impl Surface {
    /// Bytes touched by a logical row range, including partial edge bytes.
    pub fn span(&self, x: u32, width: u32) -> u32 {
        if width == 0 {
            0
        } else {
            self.format.row_bytes(x + width) - self.format.byte_offset(x)
        }
    }
    pub fn read_pixels<'a>(
        &self,
        memory: &'a dyn SurfaceMemory,
        x: u32,
        y: u32,
        width: u32,
    ) -> Result<Cow<'a, [u8]>, String> {
        if x.checked_add(width).is_none_or(|end| end > self.width) || y >= self.height {
            return Err("pixel range outside surface".into());
        }
        let raw = memory.read(self.row(x, y), self.span(x, width) as usize)?;
        if !self.format.packed() {
            return Ok(raw);
        }
        let offset = x % (8 / self.format.bits());
        Ok(Cow::Owned(
            (0..width)
                .map(|i| self.format.index(&raw, offset + i))
                .collect(),
        ))
    }
    pub fn write_pixels(
        &self,
        memory: &mut dyn SurfaceMemory,
        x: u32,
        y: u32,
        pixels: &[u8],
    ) -> Result<(), String> {
        let width = u32::try_from(pixels.len() / self.format.bytes() as usize)
            .map_err(|_| "pixel range overflow")?;
        if !pixels.len().is_multiple_of(self.format.bytes() as usize)
            || x.checked_add(width).is_none_or(|end| end > self.width)
            || y >= self.height
        {
            return Err("pixel range outside surface".into());
        }
        if !self.format.packed() {
            return memory.write(self.row(x, y), pixels);
        }
        let address = self.row(x, y);
        let len = self.span(x, width) as usize;
        memory.writable(address, len)?;
        let mut raw = memory.read(address, len)?.into_owned();
        let offset = x % (8 / self.format.bits());
        for (i, &value) in pixels.iter().enumerate() {
            self.format.put_index(&mut raw, offset + i as u32, value);
        }
        memory.write(address, &raw)
    }
}

impl<P: Presenter> Surfaces<P> {
    pub(super) fn draw_monochrome_text(
        &mut self,
        id: u32,
        surface: &Surface,
        bounds: Rect,
        memory: &mut dyn SurfaceMemory,
        text: &mut crate::text::Text,
        font: &crate::text::FontRequest,
        paint: crate::text::Paint<'_>,
    ) -> Result<bool, String> {
        let pitch = (surface.format.row_bytes(bounds.w) + 3) & !3;
        let length = pitch
            .checked_mul(bounds.h)
            .and_then(|n| n.checked_mul(2))
            .filter(|&n| n <= 32 * 1024 * 1024)
            .ok_or("monochrome text patch exceeds allocation limit")? as usize;
        if length > self.text_patch.capacity() {
            self.text_patch
                .reserve_exact(length - self.text_patch.len());
            self.stats.text_scratch_growths += 1;
        }
        self.text_patch.resize(length, 0);
        let (painted, original) = self.text_patch.split_at_mut(length / 2);
        painted.fill(0);
        for y in 0..bounds.h {
            memory.writable(
                surface.row(bounds.x, bounds.y + y),
                surface.span(bounds.x, bounds.w) as usize,
            )?;
            let indices = surface.read_pixels(memory, bounds.x, bounds.y + y, bounds.w)?;
            let row = &mut painted[(y * pitch) as usize..((y + 1) * pitch) as usize];
            for (x, &index) in indices.iter().enumerate() {
                surface.format.put_index(row, x as u32, index);
            }
        }
        original.copy_from_slice(painted);
        let table = surface.palette.as_deref().unwrap_or(&BLACK_PALETTE);
        text.paint_monochrome(
            font,
            paint,
            crate::text::Pixels {
                bounds,
                pitch,
                format: PixelFormat::Indexed1,
                bytes: painted,
            },
            [table[0], table[1]],
        )
        .map_err(|e| e.to_string())?;
        if painted == original {
            return Ok(false);
        }
        for y in 0..bounds.h {
            let row = &painted[(y * pitch) as usize..((y + 1) * pitch) as usize];
            let indices: Vec<_> = (0..bounds.w)
                .map(|x| surface.format.index(row, x))
                .collect();
            surface.write_pixels(memory, bounds.x, bounds.y + y, &indices)?;
        }
        self.surfaces.get_mut(&id).unwrap().version += 1;
        Ok(true)
    }

    pub(super) fn blit_packed(
        &mut self,
        source: &Surface,
        sr: Rect,
        dst: u32,
        destination: &Surface,
        dr: BlitRect,
        options: BlitOptions,
        memory: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        let left = dr.x.max(0);
        let top = dr.y.max(0);
        let right = (dr.x + i64::from(dr.w)).min(i64::from(destination.width));
        let bottom = (dr.y + i64::from(dr.h)).min(i64::from(destination.height));
        let width = (right - left) as u32;
        let ox = (left - dr.x) as u32;
        let oy = (top - dr.y) as u32;
        let sb = source.format.bytes() as usize;
        let db = destination.format.bytes() as usize;
        let stride = sr.w as usize * sb;
        // Snapshot the bounded source rectangle before any write, including
        // overlapping views with different pitches or bit alignments.
        self.blit_source.resize(stride * sr.h as usize, 0);
        for y in 0..sr.h {
            let row = source.read_pixels(memory, sr.x, sr.y + y, sr.w)?;
            self.blit_source[y as usize * stride..(y as usize + 1) * stride].copy_from_slice(&row);
        }
        for y in top..bottom {
            let address = destination.row(left as u32, y as u32);
            let len = destination.span(left as u32, width) as usize;
            memory.writable(address, len)?;
            if destination.format.packed() {
                memory.read(address, len)?;
            }
        }
        let colour = matches!(options.sampling, Sampling::AndColour | Sampling::OrColour);
        let sample_bytes = if colour { 4 } else { sb };
        let same =
            source.format == destination.format && source.palette == destination.palette && !colour;
        self.blit_sample.resize(width as usize * sample_bytes, 0);
        let mut decoded = vec![0; sr.w as usize * 4];
        self.blit_row.resize(width as usize * db, 0);
        for y in top..bottom {
            let rows = sample_range(oy + (y - top) as u32, sr.h, dr.h, options.sampling);
            self.blit_sample.fill(
                if matches!(options.sampling, Sampling::And | Sampling::AndColour) {
                    255
                } else {
                    0
                },
            );
            for sy in rows.0..rows.1 {
                let sy = if options.mirror_y { sr.h - 1 - sy } else { sy };
                let input = &self.blit_source[sy as usize * stride..(sy as usize + 1) * stride];
                if colour {
                    for (raw, pixel) in input.chunks_exact(sb).zip(decoded.chunks_exact_mut(4)) {
                        pixel.copy_from_slice(&source.decode_pixel(raw));
                    }
                    sample_row(&decoded, &mut self.blit_sample, 4, ox, sr.w, dr.w, options);
                } else {
                    sample_row(input, &mut self.blit_sample, sb, ox, sr.w, dr.w, options);
                }
            }
            let output = if same {
                &self.blit_sample
            } else {
                for (input, output) in self
                    .blit_sample
                    .chunks_exact(sample_bytes)
                    .zip(self.blit_row.chunks_exact_mut(db))
                {
                    let pixel = if colour {
                        input.try_into().unwrap()
                    } else {
                        source.decode_pixel(input)
                    };
                    output.copy_from_slice(&destination.encode_pixel(pixel)[..db]);
                }
                &self.blit_row
            };
            destination.write_pixels(memory, left as u32, y as u32, output)?;
        }
        self.surfaces.get_mut(&dst).unwrap().version += 1;
        self.stats.bytes_blitted +=
            u64::from(destination.format.row_bytes(width)) * (bottom - top) as u64;
        Ok(())
    }
}
