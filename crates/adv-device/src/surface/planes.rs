//! Window planes: top-level windows shown over the base (WINE-GFX-10). The
//! host shows each as a layer of its own (`frame::WindowLayer`). Without a
//! host compositor, a publication copies each damaged base row and then the
//! plane rows over it, bottom first, leaving the base untouched in guest memory.
use super::Rect;
#[cfg(not(feature = "gpu"))]
use super::{PixelFormat, Surface, SurfaceMemory, convert::decode_pixel, encode_pixel};

/// A window's guest `Bgra32` rows at a signed position in base pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
    /// The guest window.
    pub id: u32,
    pub pixels: u32,
    pub pitch: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Premultiplied `Bgra32` alpha instead of opaque fill.
    pub layered: bool,
}

impl Plane {
    /// The part of the plane inside a base of `extent`.
    pub fn visible(&self, extent: (u32, u32)) -> Rect {
        let clip = |at: i32, len: u32, bound: u32| {
            let start = at.clamp(0, bound as i32);
            let end = (i64::from(at) + i64::from(len)).clamp(0, i64::from(bound)) as i32;
            (start as u32, (end - start).max(0) as u32)
        };
        let (x, w) = clip(self.x, self.width, extent.0);
        let (y, h) = clip(self.y, self.height, extent.1);
        Rect { x, y, w, h }
    }
}

#[cfg(not(feature = "gpu"))]
/// Copy row `y` of `r` from the base `s` into `row`, which holds the whole
/// output row, then the planes over it, bottom first. Returns bytes copied.
pub(crate) fn compose_row(
    s: &Surface,
    planes: &[Plane],
    m: &dyn SurfaceMemory,
    y: u32,
    r: Rect,
    row: &mut [u8],
) -> Result<u64, String> {
    let start = s.format.byte_offset(r.x) as usize;
    let len = s.span(r.x, r.w) as usize;
    row[start..start + len].copy_from_slice(&m.read(s.row(0, y) + start as u32, len)?);
    let mut copied = len as u64;
    // Packed formats would need sub-byte edges; no window base uses them.
    if matches!(s.format, PixelFormat::Indexed4 | PixelFormat::Indexed1) {
        return Ok(copied);
    }
    let bytes = s.format.bytes() as usize;
    for plane in planes {
        let v = plane.visible((s.width, s.height));
        if y < v.y || y >= v.y + v.h {
            continue;
        }
        let (left, right) = (v.x.max(r.x), (v.x + v.w).min(r.x + r.w));
        if left >= right {
            continue;
        }
        let at = plane.pixels
            + (i64::from(y) - i64::from(plane.y)) as u32 * plane.pitch
            + (i64::from(left) - i64::from(plane.x)) as u32 * 4;
        let source = m.read(at, (right - left) as usize * 4)?;
        let out = &mut row[left as usize * bytes..right as usize * bytes];
        if s.format == PixelFormat::Bgra32 {
            if plane.layered {
                blend_premultiplied(out, &source);
            } else {
                out.copy_from_slice(&source);
            }
        } else {
            for (pixel, out) in source
                .as_chunks::<4>()
                .0
                .iter()
                .zip(out.chunks_exact_mut(bytes))
            {
                if plane.layered && pixel[3] == 0 {
                    continue;
                }
                let src = if plane.layered && pixel[3] < 255 {
                    let mut dst = decode_pixel(s.format, s.palette.as_deref(), out);
                    blend_pixel(&mut dst, *pixel);
                    dst
                } else {
                    *pixel
                };
                let encoded = encode_pixel(s.format, s.palette.as_deref(), src);
                out.copy_from_slice(&encoded[..bytes]);
            }
        }
        copied += source.len() as u64;
    }
    Ok(copied)
}

/// Premultiplied `Bgra32` source-over blend of one pixel into `dst`.
#[cfg(not(feature = "gpu"))]
fn blend_pixel(d: &mut [u8; 4], s: [u8; 4]) {
    let a = s[3];
    if a == 255 {
        *d = s;
    } else if a != 0 {
        let inv = 255 - u32::from(a);
        for (dc, sc) in d.iter_mut().zip(s) {
            *dc = sc.saturating_add(((u32::from(*dc) * inv + 127) / 255) as u8);
        }
    }
}

/// Premultiplied `Bgra32` source-over blend into `dst`.
#[cfg(not(feature = "gpu"))]
fn blend_premultiplied(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<4>().0)
    {
        blend_pixel(d, *s);
    }
}
