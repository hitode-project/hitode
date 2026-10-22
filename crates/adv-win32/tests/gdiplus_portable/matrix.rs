//! Authored affine examples cover every flat matrix operation on each CPU.
use super::*;

fn floats<B: Backend>(fixture: &mut Fixture<B>, at: u32, values: &[f32]) {
    for (i, value) in values.iter().enumerate() {
        fixture
            .machine
            .cpu
            .write_u32(DATA + at + i as u32 * 4, value.to_bits())
            .unwrap();
    }
}
fn ints<B: Backend>(fixture: &mut Fixture<B>, at: u32, values: &[i32]) {
    for (i, value) in values.iter().enumerate() {
        fixture
            .machine
            .cpu
            .write_u32(DATA + at + i as u32 * 4, *value as u32)
            .unwrap();
    }
}
fn example<B: Backend>(cpu: B) {
    let mut f = Fixture::new(cpu);
    floats(&mut f, 1000, &[2., -3., -1., 0.5]);
    floats(&mut f, 1024, &[2., -3.]);
    ints(&mut f, 1040, &[2, -3, -1, 1]);
    ints(&mut f, 1064, &[2, -3]);
    floats(&mut f, 1080, &[14., 8., 8., 22.]);
    ints(&mut f, 1104, &[-1, -1, 0, 0]);
    floats(&mut f, 1128, &[1., 0.]);
    floats(&mut f, 1160, &[1., 1., f32::MAX, f32::MAX]);
    ints(&mut f, 1200, &[10, 20, 2, 4]);
    ints(&mut f, 1224, &[100, 200, 104, 202, 96, 212]);
    floats(&mut f, 1264, &[10., 20., 2., 4.]);
    floats(&mut f, 1288, &[100., 200., 104., 202., 96., 212.]);
    f.machine.cpu.write_u32(DATA + 1376, 0x12345678).unwrap();
    f.machine.cpu.write_u32(DATA + 4092, 0x12345678).unwrap();
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    f.call("GdipCreateMatrix", &[DATA + 4], 0);
    f.call("GdipIsMatrixIdentity", &[REF | 4, DATA + 800], 0);
    f.call("GdipIsMatrixInvertible", &[REF | 4, DATA + 804], 0);
    f.call(
        "GdipCreateMatrix2",
        &[
            2f32.to_bits(),
            0,
            0,
            4f32.to_bits(),
            10f32.to_bits(),
            20f32.to_bits(),
            DATA + 8,
        ],
        0,
    );
    f.call("GdipCloneMatrix", &[REF | 8, DATA + 12], 0);
    f.call(
        "GdipScaleMatrix",
        &[REF | 8, 2f32.to_bits(), 3f32.to_bits(), 1],
        0,
    );
    f.call("GdipGetMatrixElements", &[REF | 8, DATA + 808], 0);
    f.call("GdipGetMatrixElements", &[REF | 12, DATA + 832], 0);
    f.call("GdipTransformMatrixPoints", &[REF | 12, DATA + 1000, 2], 0);
    f.call(
        "GdipVectorTransformMatrixPoints",
        &[REF | 12, DATA + 1024, 1],
        0,
    );
    f.call("GdipTransformMatrixPointsI", &[REF | 12, DATA + 1040, 2], 0);
    f.call(
        "GdipVectorTransformMatrixPointsI",
        &[REF | 12, DATA + 1064, 1],
        0,
    );
    f.call("GdipInvertMatrix", &[REF | 12], 0);
    f.call("GdipTransformMatrixPoints", &[REF | 12, DATA + 1080, 2], 0);
    f.call(
        "GdipCreateMatrix2",
        &[
            2f32.to_bits(),
            0,
            0,
            4f32.to_bits(),
            10f32.to_bits(),
            20f32.to_bits(),
            DATA + 16,
        ],
        0,
    );
    f.call("GdipMultiplyMatrix", &[REF | 12, REF | 16, 1], 0);
    f.call("GdipIsMatrixIdentity", &[REF | 12, DATA + 856], 0);
    f.call("GdipIsMatrixEqual", &[REF | 12, REF | 4, DATA + 860], 0);
    f.call("GdipIsMatrixEqual", &[REF | 8, REF | 4, DATA + 864], 0);
    f.call(
        "GdipTranslateMatrix",
        &[REF | 8, 5f32.to_bits(), (-2f32).to_bits(), 0],
        0,
    );
    f.call("GdipGetMatrixElements", &[REF | 8, DATA + 868], 0);
    f.call("GdipRotateMatrix", &[REF | 4, 90f32.to_bits(), 1], 0);
    f.call("GdipShearMatrix", &[REF | 4, 2f32.to_bits(), 0, 1], 0);
    f.call(
        "GdipVectorTransformMatrixPoints",
        &[REF | 4, DATA + 1128, 1],
        0,
    );
    f.call(
        "GdipSetMatrixElements",
        &[
            REF | 4,
            1f32.to_bits(),
            0,
            0,
            1f32.to_bits(),
            (-0.5f32).to_bits(),
            0.5f32.to_bits(),
        ],
        0,
    );
    f.call("GdipTransformMatrixPointsI", &[REF | 4, DATA + 1104, 2], 0);
    f.call(
        "GdipSetMatrixElements",
        &[
            REF | 8,
            1f32.to_bits(),
            2f32.to_bits(),
            2f32.to_bits(),
            4f32.to_bits(),
            10f32.to_bits(),
            20f32.to_bits(),
        ],
        0,
    );
    f.call("GdipIsMatrixInvertible", &[REF | 8, DATA + 924], 0);
    f.call("GdipInvertMatrix", &[REF | 8], 2);
    f.call(
        "GdipScaleMatrix",
        &[REF | 8, 2f32.to_bits(), 2f32.to_bits(), 2],
        2,
    );
    f.call("GdipGetMatrixElements", &[REF | 8, DATA + 900], 0);
    f.call(
        "GdipCreateMatrix3I",
        &[DATA + 1200, DATA + 1224, DATA + 20],
        0,
    );
    f.call(
        "GdipCreateMatrix3",
        &[DATA + 1264, DATA + 1288, DATA + 24],
        0,
    );
    f.call("GdipGetMatrixElements", &[REF | 20, DATA + 940], 0);
    f.call("GdipIsMatrixEqual", &[REF | 20, REF | 24, DATA + 964], 0);
    f.call("GdipTransformMatrixPoints", &[REF | 4, DATA + 4092, 1], 2);
    f.call("GdipTransformMatrixPoints", &[REF | 4, DATA + 1128, 0], 2);
    f.call(
        "GdipSetMatrixElements",
        &[REF | 4, 2f32.to_bits(), 0, 0, 2f32.to_bits(), 0, 0],
        0,
    );
    f.call("GdipTransformMatrixPoints", &[REF | 4, DATA + 1160, 2], 11);
    f.call("GdipCreateSolidFill", &[0xffff0000, DATA + 28], 0);
    f.call("GdipGetMatrixElements", &[REF | 28, DATA + 1376], 2);
    f.call("GdipDeleteBrush", &[REF | 28], 0);
    for slot in [4, 8, 12, 16, 20, 24] {
        f.call("GdipDeleteMatrix", &[REF | slot], 0);
    }
    f.call("GdipDeleteMatrix", &[REF | 4], 2);
    f.call("GdiplusShutdown", &[REF], 0);
    let m = f.run();
    for (offset, expected) in [
        (800, 1),
        (804, 1),
        (856, 1),
        (860, 1),
        (864, 0),
        (924, 0),
        (964, 1),
        (1376, 0x12345678),
        (4092, 0x12345678),
    ] {
        assert_eq!(
            m.cpu.read_u32(DATA + offset).unwrap(),
            expected,
            "output {offset}"
        );
    }
    for (at, expected) in [
        (808, &[4., 0., 0., 12., 20., 60.][..]),
        (832, &[2., 0., 0., 4., 10., 20.][..]),
        (868, &[4., 0., 0., 12., 40., 36.][..]),
        (900, &[1., 2., 2., 4., 10., 20.][..]),
        (940, &[2., 1., -1., 3., 100., 130.][..]),
        (1000, &[14., 8., 8., 22.][..]),
        (1024, &[4., -12.][..]),
        (1080, &[2., -3., -1., 0.5][..]),
        (1128, &[2., 1.][..]),
        (1160, &[1., 1., f32::MAX, f32::MAX][..]),
    ] {
        for (i, expected) in expected.iter().enumerate() {
            let actual = f32::from_bits(m.cpu.read_u32(DATA + at + i as u32 * 4).unwrap());
            assert!(
                (actual - *expected).abs() <= 1e-6,
                "float {at}/{i}: {actual} != {expected}"
            );
        }
    }
    for (at, expected) in [
        (1040, &[14, 8, 8, 24][..]),
        (1064, &[4, -12][..]),
        (1104, &[-1, 0, 0, 1][..]),
    ] {
        for (i, expected) in expected.iter().enumerate() {
            assert_eq!(
                m.cpu.read_u32(DATA + at + i as u32 * 4).unwrap() as i32,
                *expected,
                "integer {at}/{i}"
            );
        }
    }
}
#[test]
// spec: DEV-IMAGE-8
fn affine_composition_inverse_and_point_transforms() {
    example(JitCpu::new(PARK_ADDR, false).unwrap());
    example(JitCpu::new(PARK_ADDR, true).unwrap());
    example(UnicornCpu::new(PARK_ADDR).unwrap());
}
