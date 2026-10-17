//! SDK guest pixel layouts over the shared BGRA8 GPU image representation.
// D3DFORMAT names run from the most significant channel to the least:
// https://learn.microsoft.com/en-us/windows/win32/direct3d9/d3dformat

pub(super) fn bytes(format: u32) -> Option<u32> {
    match format {
        21 | 22 => Some(4),
        23 | 26 => Some(2),
        28 => Some(1),
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
    if matches!(format, 23 | 26 | 28) {
        let output_pitch = width.checked_mul(4)?;
        let mut output = vec![0; output_pitch.checked_mul(height)? as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let destination = y * output_pitch as usize + x * 4;
                if format == 28 {
                    // D3DFMT_A8 is the documented undefined-channel exception:
                    // sampled RGB is zero, and the one guest byte is alpha.
                    output[destination + 3] = pixels[y * pitch as usize + x];
                } else if format == 23 {
                    // R5G6B5 replicates high bits into the low ones, so a
                    // readback truncates to the same bytes; alpha is one.
                    let at = y * pitch as usize + x * 2;
                    let packed = u16::from_le_bytes(pixels[at..at + 2].try_into().ok()?);
                    for (channel, (shift, bits)) in
                        [(0, 5), (5, 6), (11, 5)].into_iter().enumerate()
                    {
                        let value = (packed >> shift) as u8 & ((1 << bits) - 1);
                        output[destination + channel] =
                            value << (8 - bits) | value >> (2 * bits - 8);
                    }
                    output[destination + 3] = 255;
                } else {
                    let at = y * pitch as usize + x * 2;
                    let packed = u16::from_le_bytes(pixels[at..at + 2].try_into().ok()?);
                    for channel in 0..4 {
                        output[destination + channel] = ((packed >> (channel * 4)) & 15) as u8 * 17;
                    }
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
    if format == 28 {
        return Some(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[3])
                .collect(),
        );
    }
    if format == 23 {
        let mut output = Vec::with_capacity(pixels.len() / 2);
        for pixel in pixels.chunks_exact(4) {
            let packed = u16::from(pixel[0] >> 3)
                | u16::from(pixel[1] >> 2) << 5
                | u16::from(pixel[2] >> 3) << 11;
            output.extend_from_slice(&packed.to_le_bytes());
        }
        return Some(output);
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

    #[test]
    fn r5g6b5_expands_opaque_and_round_trips_every_value() {
        let packed: Vec<u8> = (0..=u16::MAX).flat_map(u16::to_le_bytes).collect();
        let (expanded, pitch) = upload(23, packed.clone(), 512, 256, 256).unwrap();
        assert_eq!(pitch, 1024);
        // 0xf81f: red and blue full, green zero.
        assert_eq!(&expanded[0xf81f * 4..0xf81f * 4 + 4], &[255, 0, 255, 255]);
        // 0x0841: the lowest step of each channel.
        assert_eq!(&expanded[0x0841 * 4..0x0841 * 4 + 4], &[8, 8, 8, 255]);
        assert_eq!(readback(23, expanded).unwrap(), packed);
        assert_eq!(length(23, 3, 2), Some(12));
    }
}
