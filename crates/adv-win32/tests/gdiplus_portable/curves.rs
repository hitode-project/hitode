//! Cardinal controls and pixels pass through the actual flat ABI in each CPU.
use super::*;
fn curves<B: TestCpu>(cpu: B, integer: bool) {
    let mut f = Fixture::new(cpu);
    let points: Vec<u8> = [1i32, 1, 2, 2, 3, 3]
        .into_iter()
        .flat_map(|v| {
            if integer {
                v.to_le_bytes()
            } else {
                (v as f32).to_le_bytes()
            }
        })
        .collect();
    f.machine.cpu.write(DATA + 512, &points).unwrap();
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    f.call("GdipCreatePath", &[0, DATA + 4], 0);
    let apis = if integer {
        [
            "GdipAddPathCurveI",
            "GdipAddPathCurve2I",
            "GdipAddPathCurve3I",
            "GdipAddPathClosedCurveI",
            "GdipAddPathClosedCurve2I",
        ]
    } else {
        [
            "GdipAddPathCurve",
            "GdipAddPathCurve2",
            "GdipAddPathCurve3",
            "GdipAddPathClosedCurve",
            "GdipAddPathClosedCurve2",
        ]
    };
    for (index, name) in apis.into_iter().enumerate() {
        f.call("GdipResetPath", &[REF | 4], 0);
        let count = if index < 3 { 2 } else { 3 };
        let mut args = vec![REF | 4, DATA + 512, count];
        if index == 2 {
            args.extend([0, 1]);
        }
        if index == 1 || index == 2 || index == 4 {
            args.push(0.5f32.to_bits());
        }
        f.call(name, &args, 0);
        f.call(
            "GdipGetPointCount",
            &[REF | 4, DATA + 800 + index as u32 * 4],
            0,
        );
        f.call(
            "GdipGetPathPoints",
            &[
                REF | 4,
                DATA + 900 + index as u32 * 80,
                if index < 3 { 4 } else { 10 },
            ],
            0,
        );
        f.call(
            "GdipGetPathTypes",
            &[
                REF | 4,
                DATA + 1400 + index as u32 * 12,
                if index < 3 { 4 } else { 10 },
            ],
            0,
        );
    }
    let name = if integer {
        "GdipAddPathCurve3I"
    } else {
        "GdipAddPathCurve3"
    };
    for (offset, segments) in [(0, 0), (0, u32::MAX), (1, 2), (u32::MAX, 1), (u32::MAX, 2)] {
        f.call(
            name,
            &[REF | 4, DATA + 512, 3, offset, segments, 0.5f32.to_bits()],
            2,
        );
    }
    f.call("GdipGetPointCount", &[REF | 4, DATA + 820], 0);
    let subset: Vec<_> = [0i32, 0, 6, 0, 6, 6, 12, 6]
        .into_iter()
        .flat_map(|v| {
            if integer {
                v.to_le_bytes()
            } else {
                (v as f32).to_le_bytes()
            }
        })
        .collect();
    f.machine.cpu.write(DATA + 560, &subset).unwrap();
    f.call("GdipResetPath", &[REF | 4], 0);
    f.call(name, &[REF | 4, DATA + 560, 4, 1, 1, 0.5f32.to_bits()], 0);
    f.call("GdipGetPathPoints", &[REF | 4, DATA + 1520, 4], 0);
    let m = f.run();
    for index in 0..5 {
        assert_eq!(
            m.cpu.read_u32(DATA + 800 + index * 4).unwrap(),
            if index < 3 { 4 } else { 10 }
        );
        let expected: &[f32] = if index < 3 {
            &[
                1.,
                1.,
                1.166_666_6,
                1.166_666_6,
                1.833_333_3,
                1.833_333_3,
                2.,
                2.,
            ]
        } else {
            &[
                1., 1., 0.8333333, 0.8333333, 1.6666667, 1.6666667, 2., 2., 2.3333333, 2.3333333,
                3.1666667, 3.1666667, 3., 3., 2.8333333, 2.8333333, 1.1666667, 1.1666667, 1., 1.,
            ]
        };
        for (i, &value) in expected.iter().enumerate() {
            let actual = f32::from_bits(
                m.cpu
                    .read_u32(DATA + 900 + index * 80 + i as u32 * 4)
                    .unwrap(),
            );
            assert!(
                (actual - value).abs() < 0.00001,
                "API {index} point {i}: {actual} vs {value}"
            );
        }
        let len = if index < 3 { 4 } else { 10 };
        for i in 0..len {
            assert_eq!(
                m.cpu.read_u8(DATA + 1400 + index * 12 + i).unwrap(),
                if i == 0 {
                    0
                } else if i == 9 {
                    0x83
                } else {
                    3
                }
            );
        }
    }
    assert_eq!(m.cpu.read_u32(DATA + 820).unwrap(), 10);
    for (index, value) in [6f32, 0., 7., 1., 5., 5., 6., 6.].into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + 1520 + index as u32 * 4).unwrap(),
            value.to_bits()
        );
    }
}
#[test]
// spec: DEV-IMAGE-13
fn cardinal_aliases_preserve_controls_closure_and_invalid_segment_state() {
    for integer in [false, true] {
        curves(JitCpu::new(PARK_ADDR, false).unwrap(), integer);
        curves(JitCpu::new(PARK_ADDR, true).unwrap(), integer);
        curves(UnicornCpu::new(PARK_ADDR).unwrap(), integer);
    }
}
