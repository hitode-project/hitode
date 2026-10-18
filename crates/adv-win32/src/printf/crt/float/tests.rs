//! VC80 8.0.50727.9680 authored-oracle outputs, independent of the implementation.
use super::super::{Error, narrow, narrow_with_float_profile, wide, wide_with_float_profile};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, Prot},
};
use adv_machine::layout::PARK_ADDR;
use adv_unicorn::UnicornCpu;

const VALUES: u32 = 0x400000;

fn slots(cpu: &mut dyn Guest, words: &[u32]) {
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    cpu.write(VALUES, &bytes).unwrap();
}

fn oracle<B: Backend>(mut cpu: B) {
    cpu.map(VALUES, 4096, Prot::RW).unwrap();
    let cases: &[(&str, f64, &str)] = &[
        ("%.0f", 2.5, "3"),
        ("%.0f", -2.5, "-3"),
        ("%.2f", 0.125, "0.13"),
        ("%.2f", 2.675, "2.67"),
        ("%f", 1.25, "1.250000"),
        ("%#.0f", 1.25, "1."),
        ("%f", -0.0, "-0.000000"),
        ("%+010.2f", -0.0, "-000000.00"),
        ("%+010.2f", 1.25, "+000001.25"),
        ("%-010.2f", 1.25, "1.25      "),
        ("% 10.2f", 1.25, "      1.25"),
        ("%.2e", 12345.0, "1.23e+004"),
        ("%#.0E", 9.5, "1.E+001"),
        ("%e", 0.00125, "1.250000e-003"),
        ("%e", 0.0, "0.000000e+000"),
        ("%g", 12345.0, "12345"),
        ("%.0g", 9.5, "1e+001"),
        ("%.6g", 12.5, "12.5"),
        ("%#.6g", 12.5, "12.5000"),
        ("%#.0g", 0.0, "0.0"),
        ("%#.6g", 0.0, "0.000000"),
        ("%#.0g", 1.0, "1."),
        ("%.3g", 0.00009996, "0.0001"),
        ("%.3g", 0.00009994, "9.99e-005"),
        ("%.3g", 999.5, "1e+003"),
        ("%.3g", 999.4, "999"),
        ("%.3G", 999.5, "1E+003"),
        ("%.17g", 0.1, "0.10000000000000001"),
        ("%.25f", 0.1, "0.1000000000000000100000000"),
        ("%.25e", 0.1, "1.0000000000000001000000000e-001"),
        ("%.25g", 0.1, "0.10000000000000001"),
        ("%.14g", 1.2345678901234567, "1.2345678901235"),
        ("%.2lf", 1.25, "1.25"),
        ("%.2Lf", 1.25, "1.25"),
        (
            "%.17g",
            0.000_000_029_802_322_387_695_313,
            "2.9802322387695313e-008",
        ),
        ("%.14g", f64::from_bits(1), "4.9406564584125e-324"),
        ("%.14g", f64::MAX, "1.7976931348623e+308"),
        ("%.14g", 1.0, "1"),
    ];
    for &(format, value, expected) in cases {
        let bits = value.to_bits();
        slots(&mut cpu, &[bits as u32, (bits >> 32) as u32]);
        for cp in [0, 932, 936, 1252] {
            let (output, encoding_error) =
                narrow_with_float_profile(&cpu, cp, format.as_bytes(), VALUES, None).unwrap();
            assert_eq!(output, expected.as_bytes(), "{format} {value:?}, cp{cp}");
            assert!(!encoding_error);
        }
        let format_w: Vec<u16> = format.encode_utf16().collect();
        let (output, encoding_error) =
            wide_with_float_profile(&cpu, 0, &format_w, VALUES, None).unwrap();
        assert_eq!(output, expected.encode_utf16().collect::<Vec<_>>());
        assert!(!encoding_error);
    }
}

#[test]
fn vc80_float_oracle_over_checked_guest_slots() {
    oracle(JitCpu::new(PARK_ADDR, false).unwrap());
    oracle(JitCpu::new(PARK_ADDR, true).unwrap());
    oracle(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn boundaries<B: Backend>(mut cpu: B) {
    cpu.map(VALUES, 4096, Prot::RW).unwrap();
    let a = 2.5f64.to_bits();
    let b = 1.0f64.to_bits();
    slots(
        &mut cpu,
        &[
            (-12i32) as u32,
            2,
            a as u32,
            (a >> 32) as u32,
            b as u32,
            (b >> 32) as u32,
            7,
        ],
    );
    let (output, _) = narrow_with_float_profile(&cpu, 0, b"%*.*f|%.14g|%d", VALUES, None).unwrap();
    assert_eq!(output, b"2.50        |1|7");
    slots(&mut cpu, &[(-1i32) as u32, a as u32, (a >> 32) as u32]);
    let (output, _) = narrow_with_float_profile(&cpu, 0, b"%.*f", VALUES, None).unwrap();
    assert_eq!(output, b"2.500000");
    for value in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        let bits = value.to_bits();
        slots(&mut cpu, &[bits as u32, (bits >> 32) as u32]);
        assert!(matches!(
            narrow_with_float_profile(&cpu, 0, b"%g", VALUES, None),
            Err(Error::Unsupported(_))
        ));
    }
    slots(&mut cpu, &[a as u32, (a >> 32) as u32]);
    assert!(matches!(
        narrow_with_float_profile(&cpu, 0, b"%.65f", VALUES, None),
        Err(Error::Unsupported(_))
    ));
    for format in [b"%a".as_slice(), b"%F", b"%g"] {
        assert!(matches!(
            narrow_with_float_profile(&cpu, 0, format, 0, Some("non-nearest or non-dot profile")),
            Err(Error::Unsupported(_))
        ));
    }
    assert!(matches!(
        narrow(&cpu, 0, b"%g", 0),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        wide(&cpu, 0, &[b'%' as u16, b'g' as u16], 0),
        Err(Error::Unsupported(_))
    ));
    slots(&mut cpu, &[42]);
    let (output, _) =
        narrow_with_float_profile(&cpu, 0, b"%d", VALUES, Some("non-dot profile")).unwrap();
    assert_eq!(output, b"42");
    let boundary = VALUES + 4092;
    cpu.write(boundary, &(a as u32).to_le_bytes()).unwrap();
    assert!(
        matches!(narrow_with_float_profile(&cpu, 0, b"%f", boundary, None),
                     Err(Error::Memory(error)) if error.addr == VALUES + 4096)
    );
    cpu.protect(VALUES, 4096, Prot::WRITE).unwrap();
    assert!(matches!(
        narrow_with_float_profile(&cpu, 0, b"%f", VALUES, None),
        Err(Error::Memory(_))
    ));
}

#[test]
fn float_fields_double_slots_and_rejection_boundaries() {
    boundaries(JitCpu::new(PARK_ADDR, false).unwrap());
    boundaries(JitCpu::new(PARK_ADDR, true).unwrap());
    boundaries(UnicornCpu::new(PARK_ADDR).unwrap());
}
