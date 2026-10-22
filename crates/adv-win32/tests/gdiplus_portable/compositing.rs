//! Authored coverage and byte-channel results match documented GDI+ compositing.
use super::*;
fn composition<B: TestCpu>(cpu: B, mode: u32) {
    let mut f = Fixture::new(cpu);
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    for (image, graphics, pixels) in [(4, 8, 512), (20, 24, 640)] {
        f.call(
            "GdipCreateBitmapFromScan0",
            &[3, 3, 12, 0x26200a, DATA + pixels, DATA + image],
            0,
        );
        f.call(
            "GdipGetImageGraphicsContext",
            &[REF | image, DATA + graphics],
            0,
        );
    }
    f.call("GdipCreateSolidFill", &[0xa0000000, DATA + 12], 0);
    f.call("GdipCreateSolidFill", &[0xa0ffffff, DATA + 16], 0);
    for (name, offset) in [
        ("GdipGetCompositingMode", 800),
        ("GdipGetSmoothingMode", 804),
        ("GdipGetTextRenderingHint", 808),
    ] {
        f.call(name, &[REF | 8, DATA + offset], 0);
        f.call(name, &[REF | 4, DATA + offset + 32], 2);
        f.call(name, &[REF | 8, CODE], 2);
        f.call(name, &[REF | 8, 0], 2);
    }
    f.call("GdipSetCompositingMode", &[REF | 8, mode], 0);
    f.call("GdipSetCompositingMode", &[REF | 8, 2], 2);
    f.call("GdipGetCompositingMode", &[REF | 8, DATA + 812], 0);
    f.call("GdipFillRectangleI", &[REF | 8, REF | 12, 0, 0, 2, 2], 0);
    f.call("GdipFillRectangleI", &[REF | 8, REF | 16, 1, 1, 2, 2], 0);
    f.call(
        "GdipDrawImageRect",
        &[REF | 24, REF | 4, 0, 0, 3f32.to_bits(), 3f32.to_bits()],
        0,
    );
    for y in 0..3 {
        for x in 0..3 {
            f.call(
                "GdipBitmapGetPixel",
                &[REF | 20, x, y, DATA + 960 + (y * 3 + x) * 4],
                0,
            );
        }
    }
    for (i, value) in [0, 1, 2, 3, 4, 5].into_iter().enumerate() {
        f.call("GdipSetSmoothingMode", &[REF | 8, value], 0);
        f.call(
            "GdipGetSmoothingMode",
            &[REF | 8, DATA + 864 + i as u32 * 4],
            0,
        );
    }
    f.call("GdipSetTextRenderingHint", &[REF | 8, 4], 0);
    f.call("GdipGetTextRenderingHint", &[REF | 8, DATA + 816], 0);
    f.call("GdipSetTextRenderingHint", &[REF | 8, 6], 2);
    f.call("GdipGetTextRenderingHint", &[REF | 8, DATA + 820], 0);
    let m = f.run();
    for (offset, expected) in [
        (800, 0),
        (804, 3),
        (808, 0),
        (812, mode),
        (816, 4),
        (820, 4),
    ] {
        assert_eq!(m.cpu.read_u32(DATA + offset).unwrap(), expected);
    }
    for (i, expected) in [3, 3, 4, 3, 4, 5].into_iter().enumerate() {
        assert_eq!(m.cpu.read_u32(DATA + 864 + i as u32 * 4).unwrap(), expected);
    }
    let overlap = if mode == 0 { 0xdcb9b9b9 } else { 0xa0ffffff };
    let expected = [
        0xa0000000, 0xa0000000, 0, 0xa0000000, overlap, 0xa0ffffff, 0, 0xa0ffffff, 0xa0ffffff,
    ];
    for (i, expected) in expected.into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + 960 + i as u32 * 4).unwrap(),
            expected,
            "mode {mode}, pixel {i}"
        );
    }
}
#[test]
// spec: DEV-IMAGE-5 DEV-IMAGE-16
fn source_copy_and_source_over_produce_known_straight_alpha_pixels_and_normalized_properties() {
    for mode in [0, 1] {
        composition(JitCpu::new(PARK_ADDR, false).unwrap(), mode);
        composition(JitCpu::new(PARK_ADDR, true).unwrap(), mode);
        composition(UnicornCpu::new(PARK_ADDR).unwrap(), mode);
    }
}
