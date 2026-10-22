//! Bounded PNG/BMP/DDS decoding and resizing of straight-alpha BGRA8 pixels.
mod decode;
mod resize;

pub use decode::{decode, decode_mipmaps, inspect};
pub use resize::pixels as resize;

pub const LIMIT: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub enum Error {
    Invalid,
    Unsupported(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileFormat {
    Bmp,
    Png,
    Dds,
}

/// Encoded source samples, before expansion into BGRA8 pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Rgb8,
    Rgbx8,
    Rgba8,
    Indexed8,
    Luma8,
    LumaAlpha8,
    Rgb565,
    Rgba4444,
    Bc1,
    Bc2,
    Bc3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub mips: u32,
    pub format: Format,
    pub file_format: FileFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// Copy the overlapping rectangle and leave the rest transparent black.
    Copy,
    /// Select integer source cells.
    Point,
    /// Interpolate pixel centers with wrapping at both edges.
    LinearWrap,
    /// Scale a tent reconstruction kernel, wrapping taps beyond the source.
    TriangleWrap,
    /// Average a 2x2 box for a half-sized mip level (unit axes repeat).
    Box,
}
