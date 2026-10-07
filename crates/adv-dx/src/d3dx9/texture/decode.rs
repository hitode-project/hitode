//! Bounded still-image decoding. Pixels are ordinary BGRA8, never GPU objects.
use super::ImageInfo;

pub(super) const LIMIT: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub(super) enum Error {
    Invalid,
    Unsupported(&'static str),
}
type Result<T> = std::result::Result<T, Error>;

fn u16_at(bytes: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(at..at + 2)
            .ok_or(Error::Invalid)?
            .try_into()
            .unwrap(),
    ))
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(Error::Invalid)?
            .try_into()
            .unwrap(),
    ))
}
fn extent(width: u32, height: u32) -> Result<usize> {
    let size = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::Invalid)?;
    if width == 0 || height == 0 || size > LIMIT as u64 {
        return Err(Error::Invalid);
    }
    Ok(size as usize)
}
fn info(width: u32, height: u32, format: u32, file: u32, mips: u32) -> Result<ImageInfo> {
    extent(width, height)?;
    Ok(ImageInfo {
        width,
        height,
        depth: 1,
        mips,
        format,
        resource_type: 3,
        file_format: file,
    })
}

pub(super) fn inspect(bytes: &[u8]) -> Result<ImageInfo> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let reader = png_reader(bytes)?;
        let i = reader.info();
        if i.bit_depth == png::BitDepth::Sixteen {
            return Err(Error::Unsupported(
                "16-bit PNG source metadata/conversion is outside the measured loader profile",
            ));
        }
        // Source metadata describes encoded samples, even when the requested
        // destination expands them. Authored D3DX9_43 native fixture: P8/L8/A8L8.
        let format = match i.color_type {
            png::ColorType::Indexed => 41,
            png::ColorType::Grayscale => {
                if i.trns.is_some() {
                    51
                } else {
                    50
                }
            }
            png::ColorType::GrayscaleAlpha => 51,
            png::ColorType::Rgb => {
                if i.trns.is_some() {
                    21
                } else {
                    22
                }
            }
            png::ColorType::Rgba => 21,
        };
        info(i.width, i.height, format, 3, 1)
    } else if bytes.starts_with(b"BM") {
        bmp_header(bytes).map(|b| b.info)
    } else if bytes.starts_with(b"DDS ") {
        dds_header(bytes).map(|d| d.info)
    } else {
        Err(Error::Unsupported(
            "encoded image is not a supported PNG/BMP/DDS container",
        ))
    }
}

pub(super) fn decode(bytes: &[u8], image: ImageInfo) -> Result<Vec<u8>> {
    match image.file_format {
        3 => png_pixels(bytes),
        0 => bmp_pixels(bytes),
        4 => dds_pixels(bytes),
        _ => Err(Error::Invalid),
    }
}

fn png_reader(bytes: &[u8]) -> Result<png::Reader<std::io::Cursor<&[u8]>>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_limits(png::Limits { bytes: LIMIT });
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let reader = decoder.read_info().map_err(|_| Error::Invalid)?;
    extent(reader.info().width, reader.info().height)?;
    if reader.output_buffer_size() > LIMIT {
        return Err(Error::Invalid);
    }
    Ok(reader)
}
fn png_pixels(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut reader = png_reader(bytes)?;
    let mut raw = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut raw).map_err(|_| Error::Invalid)?;
    let mut pixels = vec![0; extent(frame.width, frame.height)?];
    let channels = frame.color_type.samples();
    for (src, dst) in raw[..frame.buffer_size()]
        .chunks_exact(channels)
        .zip(pixels.as_chunks_mut::<4>().0.iter_mut())
    {
        let (r, g, b, a) = match frame.color_type {
            png::ColorType::Rgb => (src[0], src[1], src[2], 255),
            png::ColorType::Rgba => (src[0], src[1], src[2], src[3]),
            png::ColorType::Grayscale => (src[0], src[0], src[0], 255),
            png::ColorType::GrayscaleAlpha => (src[0], src[0], src[0], src[1]),
            _ => return Err(Error::Invalid),
        };
        dst.copy_from_slice(&[b, g, r, a]);
    }
    Ok(pixels)
}

struct Bitmap {
    info: ImageInfo,
    offset: usize,
    bits: u32,
    stride: usize,
    top_down: bool,
}
fn bmp_header(bytes: &[u8]) -> Result<Bitmap> {
    let header = u32_at(bytes, 14)?;
    if !matches!(header, 40 | 108 | 124) {
        return Err(Error::Unsupported("BMP header type is unsupported"));
    }
    bytes.get(14..14 + header as usize).ok_or(Error::Invalid)?;
    let width = u32_at(bytes, 18)? as i32;
    let height = u32_at(bytes, 22)? as i32;
    if width <= 0 || height == i32::MIN || u16_at(bytes, 26)? != 1 {
        return Err(Error::Invalid);
    }
    let bits = u16_at(bytes, 28)? as u32;
    if !matches!(bits, 24 | 32) || u32_at(bytes, 30)? != 0 {
        return Err(Error::Unsupported(
            "BMP supports uncompressed 24/32-bit RGB only",
        ));
    }
    let image = info(
        width as u32,
        height.unsigned_abs(),
        if bits == 32 { 21 } else { 20 },
        0,
        1,
    )?;
    let stride = ((u64::from(image.width) * u64::from(bits)).div_ceil(32) * 4) as usize;
    let offset = u32_at(bytes, 10)? as usize;
    if offset < 14 + header as usize
        || bytes
            .get(offset..offset + stride * image.height as usize)
            .is_none()
    {
        return Err(Error::Invalid);
    }
    Ok(Bitmap {
        info: image,
        offset,
        bits,
        stride,
        top_down: height < 0,
    })
}
fn bmp_pixels(bytes: &[u8]) -> Result<Vec<u8>> {
    let bitmap = bmp_header(bytes)?;
    let image = bitmap.info;
    let mut pixels = vec![0; extent(image.width, image.height)?];
    for y in 0..image.height as usize {
        let sy = if bitmap.top_down {
            y
        } else {
            image.height as usize - 1 - y
        };
        let src = &bytes[bitmap.offset + sy * bitmap.stride..];
        for x in 0..image.width as usize {
            let at = x * (bitmap.bits as usize / 8);
            pixels[(y * image.width as usize + x) * 4..][..4].copy_from_slice(&[
                src[at],
                src[at + 1],
                src[at + 2],
                if bitmap.bits == 32 { src[at + 3] } else { 255 },
            ]);
        }
    }
    Ok(pixels)
}

struct Dds {
    info: ImageInfo,
    bits: u32,
    masks: [u32; 4],
    block: u32,
    pitch: usize,
}
fn dds_header(bytes: &[u8]) -> Result<Dds> {
    if u32_at(bytes, 4)? != 124 || u32_at(bytes, 76)? != 32 {
        return Err(Error::Invalid);
    }
    if u32_at(bytes, 112)? & (0xfe00 | 0x200000) != 0 || u32_at(bytes, 24)? > 1 {
        return Err(Error::Unsupported(
            "DDS cube/volume textures are unsupported by this 2D loader",
        ));
    }
    let width = u32_at(bytes, 16)?;
    let height = u32_at(bytes, 12)?;
    let mips = u32_at(bytes, 28)?.max(1);
    if width == 0 || height == 0 || mips > 32 - width.max(height).leading_zeros() {
        return Err(Error::Invalid);
    }
    let flags = u32_at(bytes, 80)?;
    let code = u32_at(bytes, 84)?;
    let bits = u32_at(bytes, 88)?;
    let masks = [
        u32_at(bytes, 92)?,
        u32_at(bytes, 96)?,
        u32_at(bytes, 100)?,
        u32_at(bytes, 104)?,
    ];
    let (format, block) = if flags & 4 != 0 {
        match code {
            0x31545844 => (code, 8),
            0x33545844 | 0x35545844 => (code, 16),
            _ => {
                return Err(Error::Unsupported(
                    "DDS FourCC supports DXT1/DXT3/DXT5 only",
                ));
            }
        }
    } else if flags & 0x40 != 0 {
        let format = match (bits, masks) {
            (32, [0xff0000, 0xff00, 0xff, 0xff000000]) => 21,
            (32, [0xff0000, 0xff00, 0xff, 0]) => 22,
            (24, [0xff0000, 0xff00, 0xff, 0]) => 20,
            (16, [0xf800, 0x7e0, 0x1f, 0]) => 23,
            (16, [0xf00, 0xf0, 0xf, 0xf000]) => 26,
            _ => return Err(Error::Unsupported("DDS RGB channel masks are unsupported")),
        };
        (format, 0)
    } else {
        return Err(Error::Unsupported("DDS pixel encoding is unsupported"));
    };
    let image = info(width, height, format, 4, mips)?;
    let row = if block != 0 {
        width.div_ceil(4) as usize * block as usize
    } else {
        width as usize * (bits as usize / 8)
    };
    let pitch = if block == 0 && u32_at(bytes, 8)? & 8 != 0 {
        u32_at(bytes, 20)? as usize
    } else {
        row
    };
    let rows = if block != 0 {
        height.div_ceil(4)
    } else {
        height
    } as usize;
    let size = pitch
        .checked_mul(rows)
        .filter(|&n| n <= LIMIT)
        .ok_or(Error::Invalid)?;
    if pitch < row || bytes.get(128..128 + size).is_none() {
        return Err(Error::Invalid);
    }
    Ok(Dds {
        info: image,
        bits,
        masks,
        block,
        pitch,
    })
}
fn channel(value: u32, mask: u32, absent: u8) -> u8 {
    if mask == 0 {
        return absent;
    }
    let shift = mask.trailing_zeros();
    let maximum = mask >> shift;
    (((value & mask) >> shift) * 255 / maximum) as u8
}
fn dds_pixels(bytes: &[u8]) -> Result<Vec<u8>> {
    let dds = dds_header(bytes)?;
    let image = dds.info;
    let mut pixels = vec![0; extent(image.width, image.height)?];
    if dds.block == 0 {
        for y in 0..image.height as usize {
            for x in 0..image.width as usize {
                let at = 128 + y * dds.pitch + x * (dds.bits as usize / 8);
                let mut word = [0; 4];
                word[..dds.bits as usize / 8]
                    .copy_from_slice(&bytes[at..at + dds.bits as usize / 8]);
                let value = u32::from_le_bytes(word);
                pixels[(y * image.width as usize + x) * 4..][..4].copy_from_slice(&[
                    channel(value, dds.masks[2], 0),
                    channel(value, dds.masks[1], 0),
                    channel(value, dds.masks[0], 0),
                    channel(value, dds.masks[3], 255),
                ]);
            }
        }
    } else {
        for by in 0..image.height.div_ceil(4) {
            for bx in 0..image.width.div_ceil(4) {
                let at = 128 + by as usize * dds.pitch + bx as usize * dds.block as usize;
                let block = &bytes[at..at + dds.block as usize];
                let decoded = dxt(block, image.format);
                for y in 0..4 {
                    for x in 0..4 {
                        let px = bx * 4 + x;
                        let py = by * 4 + y;
                        if px < image.width && py < image.height {
                            pixels[((py * image.width + px) * 4) as usize..][..4]
                                .copy_from_slice(&decoded[(y * 4 + x) as usize]);
                        }
                    }
                }
            }
        }
    }
    Ok(pixels)
}
fn dxt(block: &[u8], format: u32) -> [[u8; 4]; 16] {
    let color = if format == 0x31545844 {
        block
    } else {
        &block[8..]
    };
    let a = u16::from_le_bytes(color[..2].try_into().unwrap());
    let b = u16::from_le_bytes(color[2..4].try_into().unwrap());
    let rgb = |v: u16| {
        [
            ((v & 31) << 3 | (v & 31) >> 2) as u8,
            (((v >> 5) & 63) << 2 | ((v >> 5) & 63) >> 4) as u8,
            (((v >> 11) & 31) << 3 | ((v >> 11) & 31) >> 2) as u8,
            255,
        ]
    };
    let mut palette = [rgb(a), rgb(b), [0, 0, 0, 255], [0; 4]];
    for c in 0..3 {
        if a > b || format != 0x31545844 {
            palette[2][c] = ((2 * u16::from(palette[0][c]) + u16::from(palette[1][c])) / 3) as u8;
            palette[3][c] = ((u16::from(palette[0][c]) + 2 * u16::from(palette[1][c])) / 3) as u8;
            palette[3][3] = 255;
        } else {
            palette[2][c] = ((u16::from(palette[0][c]) + u16::from(palette[1][c])) / 2) as u8;
        }
    }
    let indices = u32::from_le_bytes(color[4..8].try_into().unwrap());
    let mut pixels = [[0; 4]; 16];
    let mut alpha = [0u8; 8];
    alpha[0] = block[0];
    alpha[1] = block[1];
    if format == 0x35545844 {
        let denominator = if alpha[0] > alpha[1] { 7 } else { 5 };
        for i in 1..denominator {
            alpha[i + 1] = (((denominator - i) as u16 * u16::from(alpha[0])
                + i as u16 * u16::from(alpha[1]))
                / denominator as u16) as u8;
        }
        if denominator == 5 {
            alpha[6] = 0;
            alpha[7] = 255;
        }
    }
    let alpha_bits = u64::from_le_bytes(block[..8].try_into().unwrap());
    for (i, pixel) in pixels.iter_mut().enumerate() {
        *pixel = palette[((indices >> (i * 2)) & 3) as usize];
        if format == 0x33545844 {
            pixel[3] = ((alpha_bits >> (i * 4)) & 15) as u8 * 17;
        }
        if format == 0x35545844 {
            pixel[3] = alpha[((alpha_bits >> (16 + i * 3)) & 7) as usize];
        }
    }
    pixels
}

#[cfg(test)]
mod tests;
