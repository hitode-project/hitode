//! Authored source levels are copied or resized before missing levels are generated.
use super::*;
use adv_device::image::{self, FileFormat};

pub(super) fn pixels(
    bytes: &[u8],
    image: ImageInfo,
    target: [u32; 2],
    count: u32,
    filter: u32,
    mip_filter: u32,
    key: u32,
) -> Result<Vec<Vec<u8>>, decode::Error> {
    let mut source = if count == 1 {
        vec![decode::decode(bytes, image)?]
    } else {
        let format = match image.file_format {
            0 => FileFormat::Bmp,
            3 => FileFormat::Png,
            4 => FileFormat::Dds,
            _ => return Err(decode::Error::Invalid),
        };
        image::decode_mipmaps(bytes, format)?
    };
    if key != 0 {
        for pixels in &mut source {
            for pixel in pixels.as_chunks_mut::<4>().0.iter_mut() {
                if u32::from_le_bytes(*pixel) == key {
                    pixel.fill(0);
                }
            }
        }
    }
    let mut result: Vec<Vec<u8>> = Vec::new();
    for level in 0..count {
        let [width, height] = target.map(|size| (size >> level).max(1));
        let pixels = if let Some(pixels) = source.get(level as usize) {
            let sw = (image.width >> level).max(1);
            let sh = (image.height >> level).max(1);
            if (sw, sh) == (width, height) {
                pixels.clone()
            } else {
                resize::pixels(pixels, sw, sh, width, height, filter)
            }
        } else {
            let sw = (target[0] >> (level - 1)).max(1);
            let sh = (target[1] >> (level - 1)).max(1);
            resize::pixels(result.last().unwrap(), sw, sh, width, height, mip_filter)
        };
        result.push(pixels);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // spec: DEV-IMAGE-1
    fn authored_dds_levels_are_preserved_by_memory_texture_preparation() {
        let mut bytes = vec![0; 128];
        bytes[..4].copy_from_slice(b"DDS ");
        for (at, value) in [
            (4, 124u32),
            (8, 0x2100f),
            (12, 4),
            (16, 4),
            (20, 16),
            (28, 3),
            (76, 32),
            (80, 0x41),
            (88, 32),
            (92, 0xff0000),
            (96, 0xff00),
            (100, 0xff),
            (104, 0xff000000),
            (108, 0x401008),
        ] {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let authored = [
            [10, 20, 30, 255].repeat(16),
            [40, 50, 60, 128].repeat(4),
            vec![70, 80, 90, 64],
        ];
        for level in &authored {
            bytes.extend(level);
        }
        let info = decode::inspect(&bytes).unwrap();
        assert_eq!(pixels(&bytes, info, [4, 4], 3, 4, 5, 0).unwrap(), authored);
        let enlarged = pixels(&bytes, info, [8, 8], 4, 4, 5, 0).unwrap();
        for (index, color) in [
            [10, 20, 30, 255],
            [40, 50, 60, 128],
            [70, 80, 90, 64],
            [70, 80, 90, 64],
        ]
        .into_iter()
        .enumerate()
        {
            let size = 8 >> index;
            assert_eq!(enlarged[index], color.repeat(size * size));
        }
    }
}
