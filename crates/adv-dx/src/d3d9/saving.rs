//! BMP/PNG export of authoritative CPU surface storage; never read a stale GPU shadow.
use super::*;

pub(crate) fn image(
    env: &mut Env<'_>,
    surface: u32,
    rect: Option<RECT>,
    file_format: u32,
) -> Result<Vec<u8>, u32> {
    if env.ext.try_get::<State>().is_none() {
        return Err(INVALID);
    }
    let r = resources::resource(env, surface).ok_or(INVALID)?;
    if r.kind != resources::Kind::Surface || resources::locked(env, r) {
        return Err(INVALID);
    }
    let desc = r.desc;
    if desc.Pool == 0 || desc.Usage & 3 != 0 || !matches!(desc.Format, 21 | 22) {
        adv_abi::warn_unavailable(
            env,
            "d3dx9_43.dll!D3DXSaveSurfaceToFileInMemory",
            "image export requires CPU-readable A8R8G8B8/X8R8G8B8 surface storage",
        );
        return Err(INVALID);
    }
    let storage = resources::storage(env, r).ok_or(INVALID)?;
    let area = rect.unwrap_or(RECT {
        left: 0,
        top: 0,
        right: desc.Width as i32,
        bottom: desc.Height as i32,
    });
    if area.left < 0
        || area.top < 0
        || area.right <= area.left
        || area.bottom <= area.top
        || area.right as u32 > desc.Width
        || area.bottom as u32 > desc.Height
    {
        return Err(INVALID);
    }
    let width = (area.right - area.left) as u32;
    let height = (area.bottom - area.top) as u32;
    let source_pitch = desc.Width.checked_mul(4).ok_or(INVALID)?;
    let length = format::length(desc.Format, desc.Width, desc.Height).ok_or(INVALID)?;
    adv_abi::guest::check_range(env.cpu, storage, length, Prot::READ).map_err(|_| INVALID)?;
    let mut pixels = vec![0; (width * height * 4) as usize];
    for y in 0..height {
        let offset = (area.top as u32 + y) * source_pitch + area.left as u32 * 4;
        let row = &mut pixels[(y * width * 4) as usize..((y + 1) * width * 4) as usize];
        env.cpu.read(storage + offset, row).map_err(|_| INVALID)?;
    }
    if file_format == 3 {
        encode_png(width, height, &pixels, desc.Format == 21).ok_or(INVALID)
    } else {
        encode(width, height, &pixels, desc.Format == 21).ok_or(INVALID)
    }
}

fn encode_png(width: u32, height: u32, bgra: &[u8], alpha: bool) -> Option<Vec<u8>> {
    let mut pixels = Vec::with_capacity(bgra.len());
    for p in bgra.chunks_exact(4) {
        pixels.extend_from_slice(&[p[2], p[1], p[0]]);
        if alpha {
            pixels.push(p[3]);
        }
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(if alpha {
            png::ColorType::Rgba
        } else {
            png::ColorType::Rgb
        });
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .ok()?
            .write_image_data(&pixels)
            .ok()?;
    }
    Some(bytes)
}

fn encode(width: u32, height: u32, bgra: &[u8], alpha: bool) -> Option<Vec<u8>> {
    let pixel_size = if alpha { 4 } else { 3 };
    let stride = width.checked_mul(pixel_size)?.checked_add(3)? & !3;
    let size = stride.checked_mul(height)?.checked_add(54)?;
    if width == 0
        || height == 0
        || size > 64 * 1024 * 1024
        || bgra.len() != width as usize * height as usize * 4
    {
        return None;
    }
    let mut bytes = vec![0; size as usize];
    bytes[..2].copy_from_slice(b"BM");
    for (at, value) in [
        (2, size),
        (10, 54),
        (14, 40),
        (18, width),
        (22, height),
        (34, size - 54),
    ] {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
    bytes[28..30].copy_from_slice(&((pixel_size * 8) as u16).to_le_bytes());
    for y in 0..height as usize {
        for x in 0..width as usize {
            let src = (y * width as usize + x) * 4;
            let dst = 54 + (height as usize - 1 - y) * stride as usize + x * pixel_size as usize;
            bytes[dst..dst + pixel_size as usize]
                .copy_from_slice(&bgra[src..src + pixel_size as usize]);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bmp_has_bottom_up_bgr_rows_and_zero_padding() {
        let bytes = encode(1, 2, &[1, 2, 3, 4, 5, 6, 7, 8], false).unwrap();
        assert_eq!(bytes.len(), 62);
        assert_eq!(&bytes[..2], b"BM");
        assert_eq!(&bytes[54..], &[5, 6, 7, 0, 1, 2, 3, 0]);
        assert_eq!(u32::from_le_bytes(bytes[18..22].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(bytes[22..26].try_into().unwrap()), 2);
        let alpha = encode(1, 2, &[1, 2, 3, 4, 5, 6, 7, 8], true).unwrap();
        assert_eq!(&alpha[54..], &[5, 6, 7, 8, 1, 2, 3, 4]);
        assert!(encode(0, 2, &[], false).is_none());
        assert!(encode(u32::MAX, 2, &[], false).is_none());
    }
}
