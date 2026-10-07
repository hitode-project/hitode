//! BMP export of authoritative CPU surface storage; never read a stale GPU shadow.
use super::*;

pub(crate) fn bmp(env: &mut Env<'_>, surface: u32, rect: Option<RECT>) -> Result<Vec<u8>, u32> {
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
            "BMP export requires CPU-readable A8R8G8B8/X8R8G8B8 surface storage",
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
    encode(width, height, &pixels).ok_or(INVALID)
}

fn encode(width: u32, height: u32, bgra: &[u8]) -> Option<Vec<u8>> {
    let stride = width.checked_mul(3)?.checked_add(3)? & !3;
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
    bytes[28..30].copy_from_slice(&24u16.to_le_bytes());
    for y in 0..height as usize {
        for x in 0..width as usize {
            let src = (y * width as usize + x) * 4;
            let dst = 54 + (height as usize - 1 - y) * stride as usize + x * 3;
            bytes[dst..dst + 3].copy_from_slice(&bgra[src..src + 3]);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bmp_has_bottom_up_bgr_rows_and_zero_padding() {
        let bytes = encode(1, 2, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        assert_eq!(bytes.len(), 62);
        assert_eq!(&bytes[..2], b"BM");
        assert_eq!(&bytes[54..], &[5, 6, 7, 0, 1, 2, 3, 0]);
        assert_eq!(u32::from_le_bytes(bytes[18..22].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(bytes[22..26].try_into().unwrap()), 2);
        assert!(encode(0, 2, &[]).is_none());
        assert!(encode(u32::MAX, 2, &[]).is_none());
    }
}
