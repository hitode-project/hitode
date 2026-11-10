//! Window planes: top-level windows shown over the software base (WINE-GFX-10).
//! A publication copies each damaged base row and then the plane rows over it,
//! bottom first, so a plane costs no pass of its own and the base beneath it
//! stays untouched in guest memory.
use super::{PixelFormat, Rect, Surface, SurfaceMemory, encode_pixel};

/// A window's guest `Bgra32` rows at a signed position in base pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
    pub pixels: u32,
    pub pitch: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
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
            out.copy_from_slice(&source);
        } else {
            for (pixel, out) in source
                .as_chunks::<4>()
                .0
                .iter()
                .zip(out.chunks_exact_mut(bytes))
            {
                let encoded = encode_pixel(s.format, s.palette.as_deref(), *pixel);
                out.copy_from_slice(&encoded[..bytes]);
            }
        }
        copied += source.len() as u64;
    }
    Ok(copied)
}
