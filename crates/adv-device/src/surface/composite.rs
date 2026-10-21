//! CPU compositing over shared surface storage, clipping and format conversion.
use super::*;

#[derive(Clone, Copy)]
pub enum Composite {
    /// RGB is premultiplied when source_alpha is true.
    Alpha { constant: u8, source_alpha: bool },
    /// BGR comparison ignores the source's high byte.
    ColorKey([u8; 3]),
}

#[derive(Debug)]
pub enum CompositeError {
    Allocation,
    Storage(String),
}
impl From<String> for CompositeError {
    fn from(value: String) -> Self {
        Self::Storage(value)
    }
}
impl From<&str> for CompositeError {
    fn from(value: &str) -> Self {
        Self::Storage(value.into())
    }
}

impl<P: Presenter> Surfaces<P> {
    #[allow(clippy::too_many_arguments)]
    pub fn composite(
        &mut self,
        source: u32,
        sr: Rect,
        destination: u32,
        dr: BlitRect,
        operation: Composite,
        memory: &mut dyn SurfaceMemory,
    ) -> Result<Rect, CompositeError> {
        let s = self.get(source).ok_or("unknown source surface")?.clone();
        let d = self
            .get(destination)
            .ok_or("unknown destination surface")?
            .clone();
        let operation = match operation {
            Composite::ColorKey([b, g, r]) => {
                let raw = s.encode_pixel([b, g, r, 0]);
                let color = s.decode_pixel(&raw[..s.format.bytes() as usize]);
                Composite::ColorKey([color[0], color[1], color[2]])
            }
            alpha => alpha,
        };
        if sr.w == 0
            || sr.h == 0
            || dr.w == 0
            || dr.h == 0
            || sr.x.checked_add(sr.w).is_none_or(|x| x > s.width)
            || sr.y.checked_add(sr.h).is_none_or(|y| y > s.height)
        {
            return Err("invalid composite source rectangle".into());
        }
        let left = dr.x.max(0).min(i64::from(d.width)) as u32;
        let top = dr.y.max(0).min(i64::from(d.height)) as u32;
        let right =
            dr.x.saturating_add(i64::from(dr.w))
                .max(0)
                .min(i64::from(d.width)) as u32;
        let bottom =
            dr.y.saturating_add(i64::from(dr.h))
                .max(0)
                .min(i64::from(d.height)) as u32;
        let bounds = Rect {
            x: left,
            y: top,
            w: right.saturating_sub(left),
            h: bottom.saturating_sub(top),
        };
        if bounds.empty() {
            return Ok(bounds);
        }
        // Read every source row and validate all destination ranges before the
        // first write. A failed transfer cannot leave a partially painted DIB.
        let count = (sr.w as usize)
            .checked_mul(sr.h as usize)
            .ok_or(CompositeError::Allocation)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(count)
            .map_err(|_| CompositeError::Allocation)?;
        for y in sr.y..sr.y + sr.h {
            let row = s.read_pixels(memory, sr.x, y, sr.w)?;
            pixels.extend(
                row.chunks_exact(s.format.bytes() as usize)
                    .map(|p| s.decode_pixel(p)),
            );
        }
        for y in top..bottom {
            memory.writable(d.row(left, y), d.span(left, bounds.w) as usize)?;
            d.read_pixels(memory, left, y, bounds.w)?;
        }
        let bytes = d.format.bytes() as usize;
        for y in top..bottom {
            let sy = ((i64::from(y) - dr.y) as u64 * u64::from(sr.h) / u64::from(dr.h)) as usize;
            let mut row = d.read_pixels(memory, left, y, bounds.w)?.into_owned();
            for (offset, target) in row.chunks_exact_mut(bytes).enumerate() {
                let x = left + offset as u32;
                let sx =
                    ((i64::from(x) - dr.x) as u64 * u64::from(sr.w) / u64::from(dr.w)) as usize;
                let src = pixels[sy * sr.w as usize + sx];
                let result = match operation {
                    Composite::ColorKey(key) => {
                        if src[..3] == key {
                            continue;
                        }
                        src
                    }
                    Composite::Alpha {
                        constant,
                        source_alpha,
                    } => {
                        let dst = d.decode_pixel(target);
                        let scale = |v: u8, a: u8| (u32::from(v) * u32::from(a) + 127) / 255;
                        let alpha = if source_alpha {
                            scale(src[3], constant) as u8
                        } else {
                            constant
                        };
                        std::array::from_fn(|channel| {
                            let source = scale(src[channel], constant);
                            (source + scale(dst[channel], 255 - alpha)).min(255) as u8
                        })
                    }
                };
                target.copy_from_slice(&d.encode_pixel(result)[..bytes]);
            }
            d.write_pixels(memory, left, y, &row)?;
        }
        self.get_mut(destination).unwrap().version += 1;
        Ok(bounds)
    }
}

#[derive(Clone, Copy)]
pub struct GradientVertex {
    pub x: i32,
    pub y: i32,
    pub color: [u16; 3],
}

impl<P: Presenter> Surfaces<P> {
    fn shade(
        &mut self,
        destination: u32,
        bounds: [i32; 4],
        memory: &mut dyn SurfaceMemory,
        mut color: impl FnMut(i32, i32) -> Option<[u8; 4]>,
    ) -> Result<(), String> {
        let d = self
            .get(destination)
            .ok_or("unknown gradient target")?
            .clone();
        let [left, top, right, bottom] = bounds;
        let left = left.max(0).min(d.width as i32) as u32;
        let top = top.max(0).min(d.height as i32) as u32;
        let right = right.max(0).min(d.width as i32) as u32;
        let bottom = bottom.max(0).min(d.height as i32) as u32;
        if left >= right || top >= bottom {
            return Ok(());
        }
        for y in top..bottom {
            memory.writable(d.row(left, y), d.span(left, right - left) as usize)?;
            d.read_pixels(memory, left, y, right - left)?;
        }
        let bytes = d.format.bytes() as usize;
        for y in top..bottom {
            let mut row = d.read_pixels(memory, left, y, right - left)?.into_owned();
            for (offset, target) in row.chunks_exact_mut(bytes).enumerate() {
                if let Some(pixel) = color((left + offset as u32) as i32, y as i32) {
                    target.copy_from_slice(&d.encode_pixel(pixel)[..bytes]);
                }
            }
            d.write_pixels(memory, left, y, &row)?;
        }
        self.get_mut(destination).unwrap().version += 1;
        Ok(())
    }
    pub fn gradient_rectangle(
        &mut self,
        destination: u32,
        vertices: [GradientVertex; 2],
        horizontal: bool,
        memory: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        let [a, b] = vertices;
        self.shade(destination, [a.x, a.y, b.x, b.y], memory, |x, y| {
            let (at, span) = if horizontal {
                (
                    i64::from(x) - i64::from(a.x),
                    i64::from(b.x) - i64::from(a.x),
                )
            } else {
                (
                    i64::from(y) - i64::from(a.y),
                    i64::from(b.y) - i64::from(a.y),
                )
            };
            if span <= 0 {
                return None;
            }
            let mut pixel = [0; 4];
            for (channel, output) in pixel[..3].iter_mut().enumerate() {
                let value = (i64::from(a.color[channel]) * (span - at)
                    + i64::from(b.color[channel]) * at)
                    / span;
                *output = (value >> 8) as u8;
            }
            Some(pixel)
        })
    }
    pub fn gradient_triangle(
        &mut self,
        destination: u32,
        mut vertices: [GradientVertex; 3],
        memory: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        let edge = |a: GradientVertex, b: GradientVertex, x: i32, y: i32| {
            (i128::from(b.x) - i128::from(a.x)) * (i128::from(y) - i128::from(a.y))
                - (i128::from(b.y) - i128::from(a.y)) * (i128::from(x) - i128::from(a.x))
        };
        let mut area = edge(vertices[0], vertices[1], vertices[2].x, vertices[2].y);
        if area == 0 {
            return Ok(());
        }
        if area < 0 {
            vertices.swap(1, 2);
            area = -area;
        }
        let [a, b, c] = vertices;
        let top_left =
            |a: GradientVertex, b: GradientVertex| b.y < a.y || (b.y == a.y && b.x > a.x);
        let inclusive = [top_left(b, c), top_left(c, a), top_left(a, b)];
        self.shade(
            destination,
            [
                a.x.min(b.x).min(c.x),
                a.y.min(b.y).min(c.y),
                a.x.max(b.x).max(c.x),
                a.y.max(b.y).max(c.y),
            ],
            memory,
            |x, y| {
                let weights = [edge(b, c, x, y), edge(c, a, x, y), edge(a, b, x, y)];
                if weights
                    .iter()
                    .zip(inclusive)
                    .any(|(&w, inclusive)| w < 0 || (w == 0 && !inclusive))
                {
                    return None;
                }
                let mut pixel = [0; 4];
                for (channel, output) in pixel[..3].iter_mut().enumerate() {
                    let value = (i128::from(a.color[channel]) * weights[0]
                        + i128::from(b.color[channel]) * weights[1]
                        + i128::from(c.color[channel]) * weights[2])
                        / area;
                    *output = (value >> 8) as u8;
                }
                Some(pixel)
            },
        )
    }
}
