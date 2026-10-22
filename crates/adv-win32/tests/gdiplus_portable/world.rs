//! World matrices change path/image pixels while queries copy independent values.
use super::*;
fn transforms<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    f.call(
        "GdipCreateBitmapFromScan0",
        &[4, 4, 16, 0x26200a, DATA + 512, DATA + 4],
        0,
    );
    f.call("GdipGetImageGraphicsContext", &[REF | 4, DATA + 8], 0);
    f.call("GdipCreateSolidFill", &[0xffff0000, DATA + 12], 0);
    f.call("GdipCreatePath", &[0, DATA + 16], 0);
    for p in [[0, 0, 1, 0], [1, 0, 1, 1], [1, 1, 0, 1]] {
        f.call("GdipAddPathLineI", &[REF | 16, p[0], p[1], p[2], p[3]], 0);
    }
    f.call("GdipClosePathFigure", &[REF | 16], 0);
    f.call("GdipCreateMatrix", &[DATA + 20], 0);
    f.call(
        "GdipTranslateWorldTransform",
        &[REF | 8, 1f32.to_bits(), 1f32.to_bits(), 1],
        0,
    );
    f.call("GdipFillPath", &[REF | 8, REF | 12, REF | 16], 0);
    f.call("GdipBitmapGetPixel", &[REF | 4, 1, 1, DATA + 800], 0);
    f.call("GdipBitmapGetPixel", &[REF | 4, 0, 0, DATA + 804], 0);
    f.call("GdipGetWorldTransform", &[REF | 8, REF | 20], 0);
    f.call("GdipGetMatrixElements", &[REF | 20, DATA + 808], 0);
    f.call("GdipResetWorldTransform", &[REF | 8], 0);
    f.call(
        "GdipScaleWorldTransform",
        &[REF | 8, 2f32.to_bits(), 2f32.to_bits(), 1],
        0,
    );
    f.call("GdipGraphicsClear", &[REF | 8, 0xff0000ff], 0);
    // Clear ignores world geometry, so this bottom-right pixel is blue too.
    f.call("GdipBitmapGetPixel", &[REF | 4, 3, 3, DATA + 832], 0);
    f.call("GdipFillPath", &[REF | 8, REF | 12, REF | 16], 0);
    f.call("GdipBitmapGetPixel", &[REF | 4, 1, 1, DATA + 836], 0);
    f.call("GdipBitmapGetPixel", &[REF | 4, 2, 2, DATA + 840], 0);
    // GetWorldTransform did not leave a live reference to the Graphics matrix.
    f.call("GdipGetMatrixElements", &[REF | 20, DATA + 844], 0);
    f.call("GdipSetWorldTransform", &[REF | 8, REF | 20], 0);
    f.call("GdipTranslateMatrix", &[REF | 20, 1f32.to_bits(), 0, 1], 0);
    f.call("GdipGetWorldTransform", &[REF | 8, REF | 20], 0);
    f.call("GdipGetMatrixElements", &[REF | 20, DATA + 868], 0);
    f.call("GdipMultiplyWorldTransform", &[REF | 8, REF | 20, 1], 0);
    f.call("GdipGetWorldTransform", &[REF | 8, REF | 20], 0);
    f.call("GdipGetMatrixElements", &[REF | 20, DATA + 892], 0);
    f.call("GdipResetWorldTransform", &[REF | 8], 0);
    f.call(
        "GdipRotateWorldTransform",
        &[REF | 8, 90f32.to_bits(), 1],
        0,
    );
    f.call("GdipGetWorldTransform", &[REF | 8, REF | 20], 0);
    f.call("GdipGetMatrixElements", &[REF | 20, DATA + 916], 0);
    f.call(
        "GdipScaleWorldTransform",
        &[REF | 8, 2f32.to_bits(), 2f32.to_bits(), 2],
        2,
    );
    f.call("GdipGetWorldTransform", &[REF | 8, REF | 12], 2);
    f.call("GdipSetWorldTransform", &[REF | 8, REF | 12], 2);
    f.call("GdipCreateMatrix2", &[0, 0, 0, 0, 0, 0, DATA + 28], 0);
    f.call("GdipSetWorldTransform", &[REF | 8, REF | 28], 2);
    f.call("GdipMultiplyWorldTransform", &[REF | 8, REF | 28, 0], 2);
    f.call("GdipScaleWorldTransform", &[REF | 8, 0, 0, 1], 2);
    f.call("GdipGetWorldTransform", &[REF | 8, REF | 20], 0);
    f.call("GdipGetMatrixElements", &[REF | 20, DATA + 944], 0);
    // A one-pixel source is translated through the same device matrix.
    f.machine.cpu.write_u32(DATA + 1000, 0xff00ff00).unwrap();
    f.call(
        "GdipCreateBitmapFromScan0",
        &[1, 1, 4, 0x26200a, DATA + 1000, DATA + 24],
        0,
    );
    f.call("GdipGraphicsClear", &[REF | 8, 0], 0);
    f.call("GdipResetWorldTransform", &[REF | 8], 0);
    f.call(
        "GdipTranslateWorldTransform",
        &[REF | 8, 2f32.to_bits(), 1f32.to_bits(), 1],
        0,
    );
    f.call("GdipDrawImage", &[REF | 8, REF | 24, 0, 0], 0);
    let m = f.run();
    for (offset, color) in [
        (800, 0xffff0000),
        (804, 0),
        (832, 0xff0000ff),
        (836, 0xffff0000),
        (840, 0xff0000ff),
    ] {
        assert_eq!(
            m.cpu.read_u32(DATA + offset).unwrap(),
            color,
            "pixel output {offset}"
        );
    }
    for at in [808, 844, 868] {
        for (i, value) in [1f32, 0., 0., 1., 1., 1.].into_iter().enumerate() {
            assert_eq!(
                m.cpu.read_u32(DATA + at + i as u32 * 4).unwrap(),
                value.to_bits()
            );
        }
    }
    for (at, expected) in [
        (892, &[1f32, 0., 0., 1., 2., 2.][..]),
        (916, &[0., 1., -1., 0., 0., 0.][..]),
        (944, &[0., 1., -1., 0., 0., 0.][..]),
    ] {
        for (i, expected) in expected.iter().enumerate() {
            let value = f32::from_bits(m.cpu.read_u32(DATA + at + i as u32 * 4).unwrap());
            assert!((value - *expected).abs() < 1e-6, "matrix {at}/{i}: {value}");
        }
    }
    for y in 0..4 {
        for x in 0..4 {
            let expected = if x == 2 && y == 1 { 0xff00ff00 } else { 0 };
            assert_eq!(
                m.cpu.read_u32(DATA + 512 + (y * 4 + x) * 4).unwrap(),
                expected,
                "transformed image {x}/{y}"
            );
        }
    }
}
#[test]
// spec: DEV-IMAGE-5 DEV-IMAGE-10
fn world_transform_queries_and_rendering_use_the_same_matrix() {
    transforms(JitCpu::new(PARK_ADDR, false).unwrap());
    transforms(JitCpu::new(PARK_ADDR, true).unwrap());
    transforms(UnicornCpu::new(PARK_ADDR).unwrap());
}
