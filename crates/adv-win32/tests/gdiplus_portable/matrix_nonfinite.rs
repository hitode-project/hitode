//! Native matrix storage accepts IEEE values even when inversion cannot proceed.
use super::*;

fn check<B: TestCpu>(cpu: B, coefficients: [f32; 6]) {
    let mut f = Fixture::new(cpu);
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    let mut args = coefficients.map(f32::to_bits).to_vec();
    args.push(DATA + 4);
    f.call("GdipCreateMatrix2", &args, 0);
    f.call("GdipIsMatrixInvertible", &[REF | 4, DATA + 800], 0);
    f.call("GdipIsMatrixIdentity", &[REF | 4, DATA + 804], 0);
    f.call("GdipInvertMatrix", &[REF | 4], 2);
    f.call("GdipGetMatrixElements", &[REF | 4, DATA + 808], 0);
    f.call("GdipCloneMatrix", &[REF | 4, DATA + 8], 0);
    args[6] = 0;
    args.insert(0, REF | 8);
    args.pop();
    f.call("GdipSetMatrixElements", &args, 0);
    f.call("GdipGetMatrixElements", &[REF | 8, DATA + 832], 0);
    let m = f.run();
    assert_eq!(m.cpu.read_u32(DATA + 800).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 804).unwrap(), 0);
    for at in [808, 832] {
        for (i, expected) in coefficients.iter().enumerate() {
            let actual = f32::from_bits(m.cpu.read_u32(DATA + at + i as u32 * 4).unwrap());
            assert!(actual == *expected || actual.is_nan() && expected.is_nan());
        }
    }
}

#[test]
// spec: DEV-IMAGE-8
fn nonfinite_matrix_elements_survive_failed_inversion() {
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for at in 0..6 {
            let mut elements = [1., 0., 0., 1., 0., 0.];
            elements[at] = value;
            check(JitCpu::new(PARK_ADDR, false).unwrap(), elements);
            check(JitCpu::new(PARK_ADDR, true).unwrap(), elements);
            check(UnicornCpu::new(PARK_ADDR).unwrap(), elements);
        }
    }
}
