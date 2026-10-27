//! One-bit text retains pixel indices even when both palette colours are equal.

use super::*;

pub(super) fn paint(
    text: &mut Text,
    request: &FontRequest,
    mut paint: Paint<'_>,
    pixels: Pixels<'_>,
    palette: [[u8; 4]; 2],
) -> Result<(), TextError> {
    let pitch = pixels
        .bounds
        .w
        .checked_mul(4)
        .ok_or_else(|| TextError::Font("monochrome text row overflow".into()))?;
    let length = pitch
        .checked_mul(pixels.bounds.h)
        .filter(|&n| n <= 32 * 1024 * 1024)
        .ok_or_else(|| TextError::Font("monochrome text patch exceeds limit".into()))?;
    let mut bytes = vec![0; length as usize];
    expand(
        pixels.bytes,
        pixels.pitch,
        &mut bytes,
        pixels.bounds.w,
        pixels.bounds.h,
    );
    let colour = |rgb: [u8; 3]| {
        let distances = palette.map(|entry| {
            (0..3)
                .map(|i| {
                    let delta = i32::from(rgb[i]) - i32::from(entry[2 - i]);
                    delta * delta
                })
                .sum::<i32>()
        });
        [if distances[1] < distances[0] { 255 } else { 0 }; 3]
    };
    paint.foreground = colour(paint.foreground);
    paint.background = colour(paint.background);
    let mut request = request.clone();
    request.quality = Quality::Monochrome;
    super::portable::paint(
        text,
        &request,
        paint,
        Pixels {
            bounds: pixels.bounds,
            pitch,
            format: crate::surface::PixelFormat::Bgra32,
            bytes: &mut bytes,
        },
    )?;
    pack(
        &bytes,
        pixels.bytes,
        pixels.pitch,
        pixels.bounds.w,
        pixels.bounds.h,
    );
    Ok(())
}

fn expand(input: &[u8], pitch: u32, output: &mut [u8], width: u32, height: u32) {
    for y in 0..height {
        for x in 0..width {
            let index = (input[(y * pitch + x / 8) as usize] >> (7 - x % 8)) & 1;
            output[((y * width + x) * 4) as usize..][..4].copy_from_slice(&[
                index * 255,
                index * 255,
                index * 255,
                0,
            ]);
        }
    }
}

fn pack(input: &[u8], output: &mut [u8], pitch: u32, width: u32, height: u32) {
    for y in 0..height {
        for x in 0..width {
            let value = u8::from(input[((y * width + x) * 4) as usize] >= 128);
            let bit = 7 - x % 8;
            let byte = &mut output[(y * pitch + x / 8) as usize];
            *byte = (*byte & !(1 << bit)) | (value << bit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: WIN32-GDI-7
    #[test]
    fn coverage_keeps_duplicate_indices_distinct_and_preserves_partial_bits_and_padding() {
        let original = [0xff, 0xad, 0xcc, 0xdd, 0x00, 0x55, 0xee, 0xff];
        let mut rgb = vec![0; 9 * 2 * 4];
        expand(&original, 4, &mut rgb, 9, 2);
        rgb[3 * 4..4 * 4].fill(0);
        rgb[(9 + 4) * 4..(9 + 5) * 4].fill(255);
        let mut actual = original;
        pack(&rgb, &mut actual, 4, 9, 2);
        assert_eq!(actual, [0xef, 0xad, 0xcc, 0xdd, 0x08, 0x55, 0xee, 0xff]);
    }
}
