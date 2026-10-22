//! Authored pixel masks verify primitives through shared scan storage.
use super::*;
fn fills<B: TestCpu>(cpu: B, integer: bool) {
    let mut f = Fixture::new(cpu);
    let n = |v: i32| {
        if integer {
            v as u32
        } else {
            (v as f32).to_bits()
        }
    };
    let points: Vec<_> = [1, 1, 3, 1, 3, 3, 1, 3]
        .into_iter()
        .flat_map(|v| n(v).to_le_bytes())
        .collect();
    f.machine.cpu.write(DATA + 600, &points).unwrap();
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    f.call(
        "GdipCreateBitmapFromScan0",
        &[4, 4, 16, 0x26200a, DATA + 512, DATA + 4],
        0,
    );
    f.call("GdipGetImageGraphicsContext", &[REF | 4, DATA + 8], 0);
    f.call("GdipSetPixelOffsetMode", &[REF | 8, 4], 0);
    f.call("GdipCreateSolidFill", &[0xff306090, DATA + 12], 0);
    let cases = [
        (
            "GdipFillRectangle",
            "GdipFillRectangleI",
            vec![n(1), n(1), n(2), n(2)],
            0x0660u16,
        ),
        (
            "GdipFillEllipse",
            "GdipFillEllipseI",
            vec![n(0), n(0), n(4), n(4)],
            0x6ff6,
        ),
        (
            "GdipFillPie",
            "GdipFillPieI",
            vec![n(0), n(0), n(4), n(4), 0, 90f32.to_bits()],
            0x4c00,
        ),
        (
            "GdipFillPolygon",
            "GdipFillPolygonI",
            vec![DATA + 600, 4, 1],
            0x0660,
        ),
        (
            "GdipFillPolygon2",
            "GdipFillPolygon2I",
            vec![DATA + 600, 4],
            0x0660,
        ),
        (
            "GdipFillClosedCurve",
            "GdipFillClosedCurveI",
            vec![DATA + 600, 4],
            0x0660,
        ),
        (
            "GdipFillClosedCurve2",
            "GdipFillClosedCurve2I",
            vec![DATA + 600, 4, 0, 0],
            0x0660,
        ),
    ];
    let mut expected = Vec::new();
    for (index, (float, int, args, mask)) in cases.into_iter().enumerate() {
        f.call("GdipGraphicsClear", &[REF | 8, 0], 0);
        let mut arguments = vec![REF | 8, REF | 12];
        arguments.extend(args);
        f.call(if integer { int } else { float }, &arguments, 0);
        for p in 0..16 {
            f.code
                .mov_eax_mem(DATA + 512 + p * 4)
                .mov_mem_eax(DATA + 900 + index as u32 * 64 + p * 4);
        }
        expected.push((float, mask));
    }
    let m = f.run();
    for (index, (api, mask)) in expected.into_iter().enumerate() {
        for p in 0..16 {
            assert_eq!(
                m.cpu
                    .read_u32(DATA + 900 + index as u32 * 64 + p * 4)
                    .unwrap(),
                if mask & (1 << p) != 0 { 0xff306090 } else { 0 },
                "{api} pixel {p} integer={integer}"
            );
        }
    }
}
#[test]
// spec: DEV-IMAGE-5 DEV-IMAGE-13
fn filled_primitive_aliases_write_the_expected_scan_pixels() {
    for integer in [false, true] {
        fills(JitCpu::new(PARK_ADDR, false).unwrap(), integer);
        fills(JitCpu::new(PARK_ADDR, true).unwrap(), integer);
        fills(UnicornCpu::new(PARK_ADDR).unwrap(), integer);
    }
}
