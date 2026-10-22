use super::*;

#[test]
// spec: DEV-IMAGE-1
fn authored_dds_mip_levels_keep_distinct_pixels_and_require_complete_payloads() {
    let mut bytes = vec![0; 128];
    bytes[..4].copy_from_slice(b"DDS ");
    for (offset, value) in [
        (4, 124u32),
        (8, 8),
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
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    let levels = [
        [10, 20, 30, 255].repeat(16),
        [40, 50, 60, 128].repeat(4),
        vec![70, 80, 90, 0],
    ];
    for level in &levels {
        bytes.extend_from_slice(level);
    }
    assert_eq!(decode_mipmaps(&bytes, FileFormat::Dds).unwrap(), levels);
    bytes.pop();
    assert!(matches!(
        decode_mipmaps(&bytes, FileFormat::Dds),
        Err(Error::Invalid)
    ));
    assert_eq!(decode(&bytes, FileFormat::Dds).unwrap(), levels[0]);
}

fn png_bytes(color: png::ColorType, samples: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 3, 1);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        if color == png::ColorType::Indexed {
            encoder.set_palette(vec![0, 0, 0, 40, 80, 120]);
        }
        encoder
            .write_header()
            .unwrap()
            .write_image_data(samples)
            .unwrap();
    }
    bytes
}
fn dds(format: u32, block: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 128];
    bytes[..4].copy_from_slice(b"DDS ");
    for (offset, value) in [
        (4, 124),
        (8, 0x1007),
        (12, 4),
        (16, 4),
        (28, 1),
        (76, 32),
        (80, 4),
        (84, format),
        (108, 0x1000),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&u32::to_le_bytes(value));
    }
    bytes.extend_from_slice(block);
    bytes
}

#[test]
// spec: DEV-IMAGE-1
fn image_metadata_and_bgra_decode_match_authored_native_samples_and_reject_truncation() {
    // D3DX9_43 native evidence is ignored target/.../d3dx-texture-native/native.jsonl:
    // RGB22/RGBA21/P8=41/L8=50/A8L8=51, source metadata stays unexpanded.
    for (color, samples, format, bgra) in [
        (
            png::ColorType::Rgb,
            [40, 80, 120].repeat(3),
            Format::Rgb8,
            [120, 80, 40, 255],
        ),
        (
            png::ColorType::Rgba,
            [40, 80, 120, 160].repeat(3),
            Format::Rgba8,
            [120, 80, 40, 160],
        ),
        (
            png::ColorType::Grayscale,
            vec![40; 3],
            Format::Luma8,
            [40, 40, 40, 255],
        ),
        (
            png::ColorType::GrayscaleAlpha,
            [40, 160].repeat(3),
            Format::LumaAlpha8,
            [40, 40, 40, 160],
        ),
        (
            png::ColorType::Indexed,
            vec![1; 3],
            Format::Indexed8,
            [120, 80, 40, 255],
        ),
    ] {
        let bytes = png_bytes(color, &samples);
        let image = inspect(&bytes).unwrap();
        assert_eq!(
            (image.width, image.height, image.format, image.file_format),
            (3, 1, format, FileFormat::Png)
        );
        assert_eq!(decode(&bytes, image.file_format).unwrap(), bgra.repeat(3));
        assert!(inspect(&bytes[..16]).is_err());
    }
    // BC1 transparent code3, BC2 explicit nibble alpha, BC3 endpoints and alpha
    // selectors. A block also exercises edge clipping in a non-multiple-of-4 image.
    let color = [0x00, 0xf8, 0xe0, 0x07, 0, 0, 0, 0];
    for (format, block, alpha) in [
        (0x31545844, color.to_vec(), 255),
        (0x33545844, [vec![0x88; 8], color.to_vec()].concat(), 136),
        (
            0x35545844,
            [vec![160, 40, 0, 0, 0, 0, 0, 0], color.to_vec()].concat(),
            160,
        ),
    ] {
        let bytes = dds(format, &block);
        let image = inspect(&bytes).unwrap();
        assert_eq!(
            decode(&bytes, image.file_format).unwrap(),
            [0, 0, 255, alpha].repeat(16)
        );
        assert!(inspect(&bytes[..bytes.len() - 1]).is_err());
    }
    let transparent = dds(0x31545844, &[0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    let image = inspect(&transparent).unwrap();
    assert_eq!(
        decode(&transparent, image.file_format).unwrap(),
        vec![0; 64]
    );
    let mut oversized = dds(0x31545844, &color);
    oversized[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    oversized[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(inspect(&oversized).is_err());
    assert!(matches!(
        inspect(b"unsupported"),
        Err(Error::Unsupported(_))
    ));
}
