//! Owned icon/list pixels composite through the existing surface and clip memory.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpriteAlpha {
    Ignore,
    Straight,
    Premultiplied,
}
#[derive(Clone, Copy)]
pub enum SpriteMode {
    Image {
        alpha: SpriteAlpha,
    },
    Color {
        alpha: SpriteAlpha,
        masked: bool,
    },
    Mask {
        foreground: [u8; 4],
        background: Option<[u8; 4]>,
    },
}
#[derive(Clone, Copy)]
pub struct Sprite<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
    pub mask: Option<&'a [u8]>,
    pub mode: SpriteMode,
    pub background: Option<[u8; 4]>,
    pub preserve_alpha: bool,
}
impl<P: Presenter> Surfaces<P> {
    pub fn sprite(
        &mut self,
        destination: u32,
        source: Sprite<'_>,
        sr: Rect,
        dr: BlitRect,
        memory: &mut dyn SurfaceMemory,
    ) -> Result<Rect, CompositeError> {
        let d = self
            .get(destination)
            .ok_or("unknown sprite destination")?
            .clone();
        let count = u64::from(source.width) * u64::from(source.height);
        if count > usize::MAX as u64 / 4
            || source.rgba.len() != count as usize * 4
            || source.mask.is_some_and(|mask| mask.len() != count as usize)
            || sr
                .x
                .checked_add(sr.w)
                .is_none_or(|right| right > source.width)
            || sr
                .y
                .checked_add(sr.h)
                .is_none_or(|bottom| bottom > source.height)
        {
            return Err("invalid sprite storage or rectangle".into());
        }
        let left = dr.x.clamp(0, i64::from(d.width)) as u32;
        let top = dr.y.clamp(0, i64::from(d.height)) as u32;
        let right =
            dr.x.saturating_add(i64::from(dr.w))
                .clamp(0, i64::from(d.width)) as u32;
        let bottom =
            dr.y.saturating_add(i64::from(dr.h))
                .clamp(0, i64::from(d.height)) as u32;
        let bounds = Rect {
            x: left,
            y: top,
            w: right.saturating_sub(left),
            h: bottom.saturating_sub(top),
        };
        if bounds.empty() || sr.empty() || dr.w == 0 || dr.h == 0 {
            return Ok(Rect::default());
        }
        let mut rows = Vec::new();
        rows.try_reserve_exact(bounds.h as usize)
            .map_err(|_| CompositeError::Allocation)?;
        for y in top..bottom {
            memory.writable(d.row(left, y), d.span(left, bounds.w) as usize)?;
            rows.push(d.read_pixels(memory, left, y, bounds.w)?.into_owned());
        }
        let scale = |value: u8, alpha: u8| (u32::from(value) * u32::from(alpha) + 127) / 255;
        let mut changed = false;
        for (row, y) in rows.iter_mut().zip(top..bottom) {
            let sy =
                sr.y + ((i64::from(y) - dr.y) as u64 * u64::from(sr.h) / u64::from(dr.h)) as u32;
            for (offset, target) in row.chunks_exact_mut(d.format.bytes() as usize).enumerate() {
                let x = left + offset as u32;
                let sx = sr.x
                    + ((i64::from(x) - dr.x) as u64 * u64::from(sr.w) / u64::from(dr.w)) as u32;
                let at = (u64::from(sy) * u64::from(source.width) + u64::from(sx)) as usize;
                let p = &source.rgba[at * 4..at * 4 + 4];
                let src = [p[2], p[1], p[0], p[3]];
                let and = source.mask.is_some_and(|mask| mask[at] != 0);
                let original = d.decode_pixel(target);
                let dst = source.background.unwrap_or(original);
                if let SpriteMode::Color {
                    alpha: SpriteAlpha::Ignore,
                    masked,
                } = source.mode
                {
                    let base = source.background.map(|color| d.encode_pixel(color));
                    let mut packed = d.encode_pixel([src[0], src[1], src[2], 0]);
                    if masked && and {
                        for (i, value) in packed[..target.len()].iter_mut().enumerate() {
                            *value ^= base.as_ref().map_or(target[i], |base| base[i]);
                        }
                    }
                    if source.preserve_alpha && d.format == PixelFormat::Bgra32 {
                        packed[3] = target[3];
                    }
                    if !target.iter().zip(&packed).all(|(a, b)| a == b) {
                        target.copy_from_slice(&packed[..target.len()]);
                        changed = true;
                    }
                    continue;
                }
                let mut pixel = match source.mode {
                    SpriteMode::Image { alpha } => {
                        let mut color = src;
                        if alpha == SpriteAlpha::Ignore {
                            color[3] = 0;
                        } else if alpha == SpriteAlpha::Straight {
                            for value in &mut color[..3] {
                                *value = scale(*value, src[3]) as u8;
                            }
                        }
                        color
                    }
                    SpriteMode::Mask {
                        foreground,
                        background,
                    } => {
                        if and {
                            if let Some(color) = background {
                                color
                            } else {
                                continue;
                            }
                        } else {
                            foreground
                        }
                    }
                    SpriteMode::Color { alpha, .. } => std::array::from_fn(|c| {
                        let source = if c == 3 {
                            u32::from(src[3])
                        } else if alpha == SpriteAlpha::Straight {
                            scale(src[c], src[3])
                        } else {
                            u32::from(src[c])
                        };
                        (source + scale(dst[c], 255 - src[3])).min(255) as u8
                    }),
                };
                if source.preserve_alpha {
                    pixel[3] = original[3];
                }
                let packed = d.encode_pixel(pixel);
                if !target.iter().zip(&packed).all(|(a, b)| a == b) {
                    target.copy_from_slice(&packed[..target.len()]);
                    changed = true;
                }
            }
        }
        if changed {
            for (row, y) in rows.iter().zip(top..bottom) {
                d.write_pixels(memory, left, y, row)?;
            }
            let version = self.get(destination).unwrap().version.wrapping_add(1);
            self.get_mut(destination).unwrap().version = version;
            Ok(bounds)
        } else {
            Ok(Rect::default())
        }
    }
}
