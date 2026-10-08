use adv_device::image::{self, FileFormat, Filter, Format};

fn bmp(bits: u16, height: i32, rows: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 54];
    bytes[..2].copy_from_slice(b"BM");
    bytes[10..14].copy_from_slice(&54u32.to_le_bytes());
    bytes[14..18].copy_from_slice(&40u32.to_le_bytes());
    bytes[18..22].copy_from_slice(&1u32.to_le_bytes());
    bytes[22..26].copy_from_slice(&height.to_le_bytes());
    bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
    bytes[28..30].copy_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(rows);
    bytes
}

fn dds(width: u32, height: u32, fields: &[(usize, u32)], data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 128];
    bytes[..4].copy_from_slice(b"DDS ");
    for (offset, value) in [(4, 124), (12, height), (16, width), (28, 1), (76, 32)]
        .into_iter()
        .chain(fields.iter().copied())
    {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(data);
    bytes
}

#[test]
fn bmp_row_direction_padding_and_encoded_alpha_are_preserved() {
    for (bits, format, alpha) in [(24, Format::Rgb8, 255), (32, Format::Rgba8, 7)] {
        let top = [10, 20, 30, 7];
        let bottom = [40, 50, 60, 9];
        for (height, rows) in [(2, [bottom, top].concat()), (-2, [top, bottom].concat())] {
            let bytes = bmp(bits, height, &rows);
            let info = image::inspect(&bytes).unwrap();
            assert_eq!((info.width, info.height, info.format), (1, 2, format));
            assert_eq!(info.file_format, FileFormat::Bmp);
            assert_eq!(
                image::decode(&bytes, info.file_format).unwrap(),
                [
                    10,
                    20,
                    30,
                    alpha,
                    40,
                    50,
                    60,
                    if bits == 24 { 255 } else { 9 }
                ]
            );
            assert!(image::inspect(&bytes[..bytes.len() - 1]).is_err());
        }
    }
}

#[test]
fn dds_rgb_masks_pitch_and_channel_rounding_are_preserved() {
    for (bits, masks, format, sample, pixel) in [
        (
            24,
            [0xff0000, 0xff00, 0xff, 0],
            Format::Rgb8,
            vec![11, 22, 33],
            [11, 22, 33, 255],
        ),
        (
            32,
            [0xff0000, 0xff00, 0xff, 0],
            Format::Rgbx8,
            vec![11, 22, 33, 44],
            [11, 22, 33, 255],
        ),
        (
            32,
            [0xff0000, 0xff00, 0xff, 0xff000000],
            Format::Rgba8,
            vec![11, 22, 33, 44],
            [11, 22, 33, 44],
        ),
        (
            16,
            [0xf800, 0x7e0, 0x1f, 0],
            Format::Rgb565,
            0x7befu16.to_le_bytes().to_vec(),
            [123, 125, 123, 255],
        ),
        (
            16,
            [0xf00, 0xf0, 0xf, 0xf000],
            Format::Rgba4444,
            0x8421u16.to_le_bytes().to_vec(),
            [17, 34, 68, 136],
        ),
    ] {
        let mut row = sample;
        row.resize(8, 0xcd);
        let bytes = dds(
            1,
            2,
            &[
                (8, 8),
                (20, 8),
                (80, 0x40),
                (88, bits),
                (92, masks[0]),
                (96, masks[1]),
                (100, masks[2]),
                (104, masks[3]),
            ],
            &row.repeat(2),
        );
        let info = image::inspect(&bytes).unwrap();
        assert_eq!(info.format, format);
        assert_eq!(
            image::decode(&bytes, info.file_format).unwrap(),
            pixel.repeat(2)
        );
        assert!(image::inspect(&bytes[..bytes.len() - 1]).is_err());
    }
}

#[test]
fn bc3_alpha_interpolation_and_partial_block_clipping_are_preserved() {
    let selectors = (0..16).fold(0u64, |bits, i| bits | ((i % 8) as u64) << (16 + i * 3));
    for (a, b, alpha) in [
        (160, 40, [160, 40, 142, 108, 91, 74]),
        (40, 160, [40, 160, 64, 112, 136, 0]),
    ] {
        let mut block = (selectors | a | b << 8).to_le_bytes().to_vec();
        block.extend_from_slice(&[0, 0xf8, 0xe0, 7, 0, 0, 0, 0]);
        let bytes = dds(3, 2, &[(80, 4), (84, 0x35545844)], &block);
        let info = image::inspect(&bytes).unwrap();
        let expected: Vec<_> = alpha.into_iter().flat_map(|a| [0, 0, 255, a]).collect();
        assert_eq!(image::decode(&bytes, info.file_format).unwrap(), expected);
    }
}

#[test]
fn resizing_interpolates_straight_alpha_and_wraps_single_row_edges() {
    let source = [0, 20, 40, 0, 100, 120, 140, 255];
    assert_eq!(
        image::resize(&source, 2, 1, 4, 1, Filter::LinearWrap),
        [
            25, 45, 65, 64, 25, 45, 65, 64, 75, 95, 115, 191, 75, 95, 115, 191
        ]
    );
    assert_eq!(
        image::resize(&source, 2, 1, 3, 2, Filter::Copy),
        [source.to_vec(), vec![0; 16]].concat()
    );
}
