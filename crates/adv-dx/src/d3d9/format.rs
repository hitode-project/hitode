//! SDK pixel layouts: raw staging for stored formats, BGRA8 expansion otherwise.
use super::UploadKey;
use adv_device::{gpu::Image, surface::Rect};
use std::sync::Arc;
use xxhash_rust::xxh3::Xxh3;

// D3DFORMAT names run from the most significant channel to the least:
// https://learn.microsoft.com/en-us/windows/win32/direct3d9/d3dformat

pub(super) const L8: u32 = 50;

pub(super) fn bytes(format: u32) -> Option<u32> {
    match format {
        21 | 22 => Some(4),
        23 | 26 => Some(2),
        28 | L8 => Some(1),
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

fn texel(format: u32) -> Option<adv_device::gpu::TexelFormat> {
    use adv_device::gpu::TexelFormat as T;
    Some(match format {
        21 => T::Bgra8,
        22 => T::Bgrx8,
        23 => T::B5g6r5,
        26 => T::Bgra4,
        28 => T::A8,
        _ => return None,
    })
}

pub(super) fn texture_storage(
    gpu: &adv_device::gpu::Gpu,
    format: u32,
) -> adv_device::gpu::TexelFormat {
    texel(format)
        .filter(|&texel| gpu.stores(texel))
        .unwrap_or(adv_device::gpu::TexelFormat::Bgra8)
}

pub(super) fn covers(rectangle: Rect, image: &Image) -> bool {
    rectangle.x == 0
        && rectangle.y == 0
        && rectangle.w == image.width()
        && rectangle.h == image.height()
}

/// Sparse content probe over whole uploads larger than 4 KiB; matching probes
/// gate the full `xxh3` pass in `dirty::next` without risking false matches.
pub(super) fn sample_probe(
    cpu: &dyn adv_cpu::backend::Guest,
    scratch: &mut Vec<u8>,
    (format, address, pitch): (u32, u32, u32),
    (width, height): (u32, u32),
) -> Option<u128> {
    let row_bytes = self::pitch(format, width)?;
    if row_bytes.checked_mul(height)? <= 4096 || height == 0 {
        return None;
    }
    let slice_len = row_bytes.min(32);
    let mut hash = Xxh3::new();
    use std::hash::Hash;
    (format, row_bytes, height).hash(&mut hash);
    for k in 0..8u32 {
        let y = if height > 1 { k * (height - 1) / 7 } else { 0 };
        let x = k * row_bytes.saturating_sub(slice_len) / 7;
        let at = address + y * pitch + x;
        let slice = if let Some(slice) = cpu.shared_bytes(at, slice_len as usize) {
            slice
        } else {
            scratch.resize(slice_len as usize, 0);
            cpu.read(at, scratch).ok()?;
            scratch.as_slice()
        };
        hash.update(slice);
    }
    Some(hash.digest128())
}

/// Snapshot admitted guest rows for `destination`, staging them unconverted
/// when its image stores the guest format and expanding to BGRA8 otherwise.
/// The caller has checked that the complete source rectangle is readable.
pub(super) fn snapshot(
    cpu: &dyn adv_cpu::backend::Guest,
    gpu: &mut adv_device::gpu::Gpu,
    admission: &mut u64,
    scratch: &mut Vec<u8>,
    source: (u32, u32, u32),
    size: (u32, u32),
    destination: (&Image, Rect),
) -> Option<(super::Pixels, u32)> {
    snapshot_with_hash(
        cpu,
        gpu,
        admission,
        scratch,
        source,
        size,
        destination,
        true,
    )
}

pub(super) fn snapshot_with_hash(
    cpu: &dyn adv_cpu::backend::Guest,
    gpu: &mut adv_device::gpu::Gpu,
    admission: &mut u64,
    scratch: &mut Vec<u8>,
    source: (u32, u32, u32),
    size: (u32, u32),
    destination: (&Image, Rect),
    hash_whole: bool,
) -> Option<(super::Pixels, u32)> {
    let (format, address, pitch) = source;
    let (width, height) = size;
    let whole = hash_whole && covers(destination.1, destination.0);
    let mut key = |hash: Option<Xxh3>| match hash {
        Some(hash) => UploadKey::Content(hash.digest128()),
        None => {
            *admission += 1;
            UploadKey::Admission(*admission)
        }
    };
    let mut hash = whole.then(Xxh3::new);
    if Some(destination.0.texel_format()) == texel(format) {
        let row_bytes = self::pitch(format, width)?;
        if pitch == row_bytes
            && let Some(total) = row_bytes.checked_mul(height)
            && let Some(all) = cpu.shared_bytes(address, total as usize)
        {
            let staged = if let Some(hash) = &mut hash {
                gpu.stage_bytes_with(row_bytes, height, all, |chunk| hash.update(chunk))
                    .ok()?
            } else {
                gpu.stage_bytes(row_bytes, height, all).ok()?
            };
            let pitch = staged.pitch();
            return Some((super::Pixels::Staged(staged, key(hash)), pitch));
        }
        let staged = gpu
            .stage(row_bytes, height, |write| {
                for y in 0..height {
                    let at = address + y * pitch;
                    let row = if let Some(row) = cpu.shared_bytes(at, row_bytes as usize) {
                        row
                    } else {
                        scratch.resize(row_bytes as usize, 0);
                        cpu.read(at, scratch).map_err(drop)?;
                        scratch.as_slice()
                    };
                    if let Some(hash) = &mut hash {
                        hash.update(row);
                    }
                    write(y, row);
                }
                Ok::<(), ()>(())
            })
            .ok()?
            .ok()?;
        let pitch = staged.pitch();
        return Some((super::Pixels::Staged(staged, key(hash)), pitch));
    }
    let row = self::pitch(format, width)?;
    let mut pixels = vec![0; ((height - 1) * pitch + row) as usize];
    cpu.read(address, &mut pixels).ok()?;
    let (pixels, pitch) = upload(format, pixels, pitch, width, height)?;
    if let Some(hash) = &mut hash {
        for row in pixels.chunks(pitch as usize).take(height as usize) {
            hash.update(&row[..width as usize * 4]);
        }
    }
    Some((super::Pixels::Host(Arc::new(pixels), key(hash)), pitch))
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
    if matches!(format, 23 | 26 | 28 | L8) {
        let output_pitch = width.checked_mul(4)?;
        let mut output = vec![0; output_pitch.checked_mul(height)? as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let destination = y * output_pitch as usize + x * 4;
                if format == 28 {
                    // D3DFMT_A8 is the documented undefined-channel exception:
                    // sampled RGB is zero, and the one guest byte is alpha.
                    output[destination + 3] = pixels[y * pitch as usize + x];
                } else if format == L8 {
                    let luminance = pixels[y * pitch as usize + x];
                    output[destination..destination + 4]
                        .copy_from_slice(&[luminance, luminance, luminance, 255]);
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

/// One texel of `format` holding the D3DCOLOR `color`, channels truncated.
pub(super) fn pack(format: u32, color: u32) -> Option<Vec<u8>> {
    let [b, g, r, a] = color.to_le_bytes().map(u16::from);
    Some(match format {
        21 | 22 => color.to_le_bytes().to_vec(),
        23 => (r >> 3 << 11 | g >> 2 << 5 | b >> 3).to_le_bytes().to_vec(),
        26 => (a >> 4 << 12 | r >> 4 << 8 | g >> 4 << 4 | b >> 4)
            .to_le_bytes()
            .to_vec(),
        28 => vec![a as u8],
        _ => return None,
    })
}

/// Explicit staging copies convert back to the declared guest pixel layout.
pub(super) fn readback(format: u32, pixels: Vec<u8>) -> Option<Vec<u8>> {
    bytes(format)?;
    if !pixels.len().is_multiple_of(4) {
        return None;
    }
    if matches!(format, 28 | L8) {
        let channel = if format == L8 { 0 } else { 3 };
        return Some(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[channel])
                .collect(),
        );
    }
    if format == 23 {
        let mut output = Vec::with_capacity(pixels.len() / 2);
        for pixel in pixels.as_chunks::<4>().0 {
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
    for pixel in pixels.as_chunks::<4>().0 {
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

    // spec: GFX-REC-7
    #[test]
    fn snapshots_identify_admissions_and_clones_without_hashing_pixels() {
        let mut fixture = crate::d3d9::stretch::tests::Fixture::new();
        let address = fixture.packet([1u8, 2, 3, 4]);
        let state = fixture.m.ext.get_mut::<super::super::State>();
        let gpu = state.gpu.as_mut().unwrap();
        let image = gpu
            .texture(2, 1, 1, adv_device::gpu::TexelFormat::Bgra8)
            .unwrap();
        let part = Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        };
        let mut capture = || {
            snapshot(
                &fixture.m.cpu,
                gpu,
                &mut state.upload_admission,
                &mut state.upload_scratch,
                (21, address, 4),
                (1, 1),
                (&image, part),
            )
            .unwrap()
            .0
        };
        let first = capture();
        let second = capture();
        use super::super::recording::hash;
        assert_eq!(hash(&first), hash(first.clone()));
        assert_ne!(hash(&first), hash(second));
        let super::super::Pixels::Staged(_, admission) = first else {
            panic!("BGRA staging");
        };
        assert_eq!(
            hash(super::super::Pixels::Host(Arc::new(vec![0; 4]), admission)),
            hash(super::super::Pixels::Host(
                Arc::new(vec![255; 4]),
                admission
            ))
        );
    }

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

    // spec: GFX-SAMPLER-5
    #[test]
    fn l8_expands_to_opaque_luminance_and_skips_guest_padding() {
        let (expanded, pitch) = upload(L8, vec![7, 121, 254], 2, 1, 2).unwrap();
        assert_eq!(pitch, 4);
        assert_eq!(expanded, [7, 7, 7, 255, 254, 254, 254, 255]);
        assert_eq!(readback(L8, expanded).unwrap(), [7, 254]);
        assert_eq!(length(L8, 3, 2), Some(6));
    }
}
