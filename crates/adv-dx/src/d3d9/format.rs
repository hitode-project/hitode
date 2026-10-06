//! SDK guest pixel layouts over the shared BGRA8 GPU image representation.
// D3DFORMAT names run from the most significant channel to the least:
// https://learn.microsoft.com/en-us/windows/win32/direct3d9/d3dformat

pub(super) fn bytes(format: u32) -> Option<u32> {
    match format {
        21 | 22 => Some(4),
        26 => Some(2),
        _ => None,
    }
}

pub(super) fn pitch(format: u32, width: u32) -> Option<u32> {
    width.checked_mul(bytes(format)?)
}

pub(super) fn length(format: u32, width: u32, height: u32) -> Option<u32> {
    pitch(format, width)?
        .checked_mul(height)
        .filter(|&n| n > 0 && n <= 64 * 1024 * 1024)
}

/// The GPU receives BGRA8, while a packed texture retains its actual SDK bytes.
/// A partial lock can retain the full guest pitch without uploading its padding.
pub(super) fn upload(
    format: u32,
    mut pixels: Vec<u8>,
    pitch: u32,
    width: u32,
    height: u32,
) -> Option<(Vec<u8>, u32)> {
    let row = self::pitch(format, width)?;
    let required = height
        .checked_sub(1)?
        .checked_mul(pitch)?
        .checked_add(row)?;
    if width == 0 || pitch < row || pixels.len() < required as usize {
        return None;
    }
    if format == 26 {
        let output_pitch = width.checked_mul(4)?;
        let mut output = vec![0; output_pitch.checked_mul(height)? as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let at = y * pitch as usize + x * 2;
                let packed = u16::from_le_bytes(pixels[at..at + 2].try_into().ok()?);
                let at = y * output_pitch as usize + x * 4;
                for channel in 0..4 {
                    output[at + channel] = ((packed >> (channel * 4)) & 15) as u8 * 17;
                }
            }
        }
        Some((output, output_pitch))
    } else {
        if format == 22 {
            for y in 0..height as usize {
                for x in 0..width as usize {
                    pixels[y * pitch as usize + x * 4 + 3] = 255;
                }
            }
        }
        Some((pixels, pitch))
    }
}

/// Explicit staging copies convert back to the declared guest pixel layout.
pub(super) fn readback(format: u32, pixels: Vec<u8>) -> Option<Vec<u8>> {
    bytes(format)?;
    if pixels.len() % 4 != 0 {
        return None;
    }
    if format != 26 {
        return Some(pixels);
    }
    let mut output = Vec::with_capacity(pixels.len() / 2);
    for pixel in pixels.chunks_exact(4) {
        let packed = (0..4).fold(0u16, |value, channel| {
            value | u16::from(pixel[channel] >> 4) << (channel * 4)
        });
        output.extend_from_slice(&packed.to_le_bytes());
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_argb_round_trips_every_channel_value_and_respects_guest_row_padding() {
        let packed: Vec<u8> = (0..=u16::MAX).flat_map(u16::to_le_bytes).collect();
        let (expanded, pitch) = upload(26, packed.clone(), 512, 256, 256).unwrap();
        assert_eq!(pitch, 1024);
        assert_eq!(&expanded[0x8123 * 4..0x8123 * 4 + 4], &[51, 34, 17, 136]);
        assert_eq!(readback(26, expanded).unwrap(), packed);
        assert_eq!(
            upload(26, vec![0x23, 0x81, 0xaa, 0xbb, 0xff, 0x0f], 4, 1, 2).unwrap(),
            (vec![51, 34, 17, 136, 255, 255, 255, 0], 4)
        );
        assert!(upload(26, vec![0; 5], 4, 1, 2).is_none());
        assert!(upload(26, vec![0; 4], 1, 1, 2).is_none());
        assert_eq!(length(26, 3, 2), Some(12));
        assert_eq!(length(21, 3, 2), Some(24));
        assert!(length(26, u32::MAX, 2).is_none());
        assert!(length(26, 0, 2).is_none());
    }
}
