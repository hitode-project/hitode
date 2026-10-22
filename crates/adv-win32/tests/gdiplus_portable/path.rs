//! Authored path arrays and pixels check the shared flat geometry family.
use super::*;
fn write_numbers<B: TestCpu>(f: &mut Fixture<B>, pointer: u32, values: &[i32], integer: bool) {
    let bytes: Vec<_> = values
        .iter()
        .flat_map(|&value| {
            if integer {
                value.to_le_bytes()
            } else {
                (value as f32).to_le_bytes()
            }
        })
        .collect();
    f.machine.cpu.write(pointer, &bytes).unwrap();
}
fn geometry<B: TestCpu>(cpu: B, integer: bool) {
    let mut f = Fixture::new(cpu);
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    f.call(
        "GdipCreateBitmapFromScan0",
        &[4, 4, 16, 0x26200a, DATA + 512, DATA + 4],
        0,
    );
    f.call("GdipGetImageGraphicsContext", &[REF | 4, DATA + 8], 0);
    f.call("GdipSetPixelOffsetMode", &[REF | 8, 4], 0);
    f.call("GdipCreateSolidFill", &[0xff306090, DATA + 12], 0);
    f.call("GdipCreatePath", &[0, DATA + 16], 0);
    let n = |value: i32| {
        if integer {
            value as u32
        } else {
            (value as f32).to_bits()
        }
    };
    let shapes = [
        (
            "GdipAddPathRectangle",
            "GdipAddPathRectangleI",
            vec![n(1), n(1), n(2), n(2)],
            4,
        ),
        (
            "GdipAddPathEllipse",
            "GdipAddPathEllipseI",
            vec![n(0), n(0), n(4), n(4)],
            13,
        ),
        (
            "GdipAddPathPie",
            "GdipAddPathPieI",
            vec![n(0), n(0), n(4), n(4), 0f32.to_bits(), 90f32.to_bits()],
            5,
        ),
        (
            "GdipAddPathBezier",
            "GdipAddPathBezierI",
            vec![n(0), n(2), n(0), n(0), n(4), n(0), n(4), n(2)],
            4,
        ),
    ];
    for (index, (float, int, args, count)) in shapes.into_iter().enumerate() {
        f.call("GdipResetPath", &[REF | 16], 0);
        f.call("GdipGraphicsClear", &[REF | 8, 0], 0);
        let mut arguments = vec![REF | 16];
        arguments.extend(args);
        f.call(if integer { int } else { float }, &arguments, 0);
        f.call(
            "GdipGetPointCount",
            &[REF | 16, DATA + 800 + index as u32 * 4],
            0,
        );
        f.call("GdipFillPath", &[REF | 8, REF | 12, REF | 16], 0);
        for y in 0..4 {
            for x in 0..4 {
                f.call(
                    "GdipBitmapGetPixel",
                    &[
                        REF | 4,
                        x,
                        y,
                        DATA + 900 + index as u32 * 64 + (y * 4 + x) * 4,
                    ],
                    0,
                );
            }
        }
        assert!(count > 0);
    }
    let m = f.run();
    let masks = [0x0660u16, 0x6ff6, 0x4c00, 0x00f0];
    for (index, (mask, count)) in masks.into_iter().zip([4, 13, 5, 4]).enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + 800 + index as u32 * 4).unwrap(),
            count
        );
        for p in 0..16 {
            assert_eq!(
                m.cpu
                    .read_u32(DATA + 900 + index as u32 * 64 + p * 4)
                    .unwrap(),
                if mask & (1 << p) != 0 { 0xff306090 } else { 0 },
                "shape {index} pixel {p} integer={integer}"
            );
        }
    }
}
fn arrays<B: TestCpu>(cpu: B, integer: bool) {
    let mut f = Fixture::new(cpu);
    write_numbers(&mut f, DATA + 512, &[1, 1, 2, 1, 2, 2, 1, 2], integer);
    f.machine.cpu.write(DATA + 560, &[0, 3, 3, 0xa3]).unwrap();
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    f.call(
        if integer {
            "GdipCreatePath2I"
        } else {
            "GdipCreatePath2"
        },
        &[DATA + 512, DATA + 560, 4, 1, DATA + 4],
        0,
    );
    f.call("GdipGetPathTypes", &[REF | 4, DATA + 800, 4], 0);
    f.call("GdipGetPathLastPoint", &[REF | 4, DATA + 808], 0);
    f.call("GdipClearPathMarkers", &[REF | 4], 0);
    f.call("GdipGetPathTypes", &[REF | 4, DATA + 804, 4], 0);
    f.call("GdipSetPathMarker", &[REF | 4], 0);
    f.call("GdipClonePath", &[REF | 4, DATA + 8], 0);
    f.call(
        "GdipCreateMatrix2",
        &[
            1f32.to_bits(),
            0,
            0,
            1f32.to_bits(),
            10f32.to_bits(),
            20f32.to_bits(),
            DATA + 12,
        ],
        0,
    );
    f.call("GdipTransformPath", &[REF | 4, REF | 12], 0);
    f.call(
        if integer {
            "GdipGetPathPointsI"
        } else {
            "GdipGetPathPoints"
        },
        &[REF | 4, DATA + 832, 4],
        0,
    );
    f.call("GdipGetPathPoints", &[REF | 8, DATA + 864, 4], 0);
    f.call("GdipGetPathPoints", &[REF | 8, DATA + 896, 3], 5);
    for (at, value) in [
        (DATA + 600, 4),
        (DATA + 604, DATA + 900),
        (DATA + 608, DATA + 932),
    ] {
        f.machine.cpu.write_u32(at, value).unwrap();
    }
    f.call("GdipGetPathData", &[REF | 8, DATA + 600], 0);
    f.call("GdipStartPathFigure", &[REF | 8], 0);
    f.call(
        if integer {
            "GdipAddPathLine2I"
        } else {
            "GdipAddPathLine2"
        },
        &[REF | 8, DATA + 512, 2],
        0,
    );
    f.call("GdipClosePathFigures", &[REF | 8], 0);
    f.call("GdipGetPathTypes", &[REF | 8, DATA + 940, 6], 0);
    for (index, (float, int, count, expected)) in [
        ("GdipAddPathLine2", "GdipAddPathLine2I", 4, 4),
        ("GdipAddPathPolygon", "GdipAddPathPolygonI", 4, 4),
        ("GdipAddPathBeziers", "GdipAddPathBeziersI", 4, 4),
        ("GdipAddPathRectangles", "GdipAddPathRectanglesI", 2, 8),
    ]
    .into_iter()
    .enumerate()
    {
        f.call("GdipResetPath", &[REF | 4], 0);
        f.call(
            if integer { int } else { float },
            &[REF | 4, DATA + 512, count],
            0,
        );
        f.call(
            "GdipGetPointCount",
            &[REF | 4, DATA + 960 + index as u32 * 4],
            0,
        );
        assert!(expected > 0);
    }
    f.call("GdipAddPathBeziers", &[REF | 4, DATA + 512, 3], 2);
    f.call("GdipGetPointCount", &[REF | 4, DATA + 976], 0);
    f.call("GdipGetPathData", &[REF | 4, 0], 2);
    f.call("GdipTransformPath", &[REF | 4, 0], 0);
    f.call("GdipTransformPath", &[REF | 12, REF | 12], 2);
    let m = f.run();
    assert_eq!(m.cpu.read_u32(DATA + 800).unwrap(), 0xa3030300);
    assert_eq!(m.cpu.read_u32(DATA + 804).unwrap(), 0x83030300);
    assert_eq!(m.cpu.read_u32(DATA + 808).unwrap(), 1f32.to_bits());
    assert_eq!(m.cpu.read_u32(DATA + 812).unwrap(), 2f32.to_bits());
    for (i, original) in [1, 1, 2, 1, 2, 2, 1, 2].into_iter().enumerate() {
        let actual = m.cpu.read_u32(DATA + 832 + i as u32 * 4).unwrap();
        let expected = original + if i % 2 == 0 { 10 } else { 20 };
        assert_eq!(
            actual,
            if integer {
                expected
            } else {
                (expected as f32).to_bits()
            }
        );
        assert_eq!(
            m.cpu.read_u32(DATA + 864 + i as u32 * 4).unwrap(),
            (original as f32).to_bits()
        );
        assert_eq!(
            m.cpu.read_u32(DATA + 900 + i as u32 * 4).unwrap(),
            (original as f32).to_bits()
        );
    }
    assert_eq!(m.cpu.read_u32(DATA + 932).unwrap(), 0xa3030300);
    assert_eq!(m.cpu.read_u32(DATA + 940).unwrap(), 0xa3030300);
    assert_eq!(m.cpu.read_u16(DATA + 944).unwrap(), 0x8100);
    for (index, expected) in [4, 4, 4, 8, 8].into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + 960 + index as u32 * 4).unwrap(),
            expected
        );
    }
}
#[test]
// spec: DEV-IMAGE-13
fn path_shapes_rasterize_expected_pixels() {
    for integer in [false, true] {
        geometry(JitCpu::new(PARK_ADDR, false).unwrap(), integer);
        geometry(JitCpu::new(PARK_ADDR, true).unwrap(), integer);
        geometry(UnicornCpu::new(PARK_ADDR).unwrap(), integer);
    }
}
#[test]
// spec: DEV-IMAGE-13 DEV-IMAGE-6
fn path_arrays_clone_markers_figures_and_transforms() {
    for integer in [false, true] {
        arrays(JitCpu::new(PARK_ADDR, false).unwrap(), integer);
        arrays(JitCpu::new(PARK_ADDR, true).unwrap(), integer);
        arrays(UnicornCpu::new(PARK_ADDR).unwrap(), integer);
    }
}
