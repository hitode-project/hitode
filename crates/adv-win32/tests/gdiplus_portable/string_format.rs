//! Independently authored flat calls check native format defaults and array copies.
use super::*;

fn settings<B: Backend>(cpu: B) {
    let mut f = Fixture::new(cpu);
    for (i, value) in [1f32, 2.5, f32::INFINITY, f32::NAN, f32::NEG_INFINITY]
        .into_iter()
        .enumerate()
    {
        f.machine
            .cpu
            .write_u32(DATA + 1000 + i as u32 * 4, value.to_bits())
            .unwrap();
    }
    f.machine
        .cpu
        .write_u32(DATA + 1040, (-1i32) as u32)
        .unwrap();
    f.machine
        .cpu
        .write_u32(DATA + 1044, (-1i32) as u32)
        .unwrap();
    f.machine.cpu.write_u32(DATA + 1048, 10).unwrap();
    f.machine.cpu.write_u32(DATA + 1052, 3).unwrap();
    for at in [700, 704, 708, 712, 716, 720, 724, 728, 732] {
        f.machine.cpu.write_u32(DATA + at, 0xcccccccc).unwrap();
    }
    f.call("GdiplusStartup", &[DATA, DATA + 256, 0], 0);
    f.call("GdipCreateStringFormat", &[u32::MAX, 0x409, DATA + 4], 0);
    f.call("GdipStringFormatGetGenericDefault", &[DATA + 8], 0);
    f.call("GdipStringFormatGetGenericTypographic", &[DATA + 12], 0);
    f.call("GdipGetStringFormatFlags", &[REF | 4, DATA + 600], 0);
    f.call(
        "GdipGetStringFormatDigitSubstitution",
        &[REF | 4, DATA + 700, DATA + 604],
        0,
    );
    f.call("GdipGetStringFormatHotkeyPrefix", &[REF | 4, DATA + 608], 0);
    f.call("GdipGetStringFormatTrimming", &[REF | 4, DATA + 612], 0);
    f.call("GdipGetStringFormatFlags", &[REF | 8, DATA + 616], 0);
    f.call("GdipGetStringFormatTrimming", &[REF | 8, DATA + 620], 0);
    f.call("GdipGetStringFormatFlags", &[REF | 12, DATA + 624], 0);
    f.call("GdipGetStringFormatTrimming", &[REF | 12, DATA + 628], 0);
    f.call(
        "GdipGetStringFormatMeasurableCharacterRangeCount",
        &[REF | 4, DATA + 632],
        0,
    );
    f.call("GdipGetStringFormatTabStopCount", &[REF | 4, DATA + 636], 0);
    f.call(
        "GdipSetStringFormatDigitSubstitution",
        &[REF | 4, u32::MAX, u32::MAX],
        0,
    );
    f.call("GdipSetStringFormatHotkeyPrefix", &[REF | 4, 2], 0);
    f.call("GdipSetStringFormatTrimming", &[REF | 4, 5], 0);
    f.call(
        "GdipSetStringFormatTabStops",
        &[REF | 4, 10f32.to_bits(), 4, DATA + 1000],
        0,
    );
    f.call(
        "GdipSetStringFormatMeasurableCharacterRanges",
        &[REF | 4, 2, DATA + 1040],
        0,
    );
    f.call("GdipCloneStringFormat", &[REF | 4, DATA + 16], 0);
    f.call("GdipSetStringFormatHotkeyPrefix", &[REF | 4, 0], 0);
    f.call("GdipSetStringFormatTrimming", &[REF | 4, 0], 0);
    f.call("GdipSetStringFormatDigitSubstitution", &[REF | 4, 0, 0], 0);
    f.call(
        "GdipSetStringFormatTabStops",
        &[REF | 4, 0, 0, DATA + 1000],
        0,
    );
    f.call(
        "GdipSetStringFormatMeasurableCharacterRanges",
        &[REF | 4, 0, 0],
        0,
    );
    f.call(
        "GdipGetStringFormatDigitSubstitution",
        &[REF | 16, DATA + 704, 0],
        0,
    );
    f.call(
        "GdipGetStringFormatDigitSubstitution",
        &[REF | 16, 0, DATA + 640],
        0,
    );
    f.call(
        "GdipGetStringFormatHotkeyPrefix",
        &[REF | 16, DATA + 644],
        0,
    );
    f.call("GdipGetStringFormatTrimming", &[REF | 16, DATA + 648], 0);
    f.call(
        "GdipGetStringFormatTabStopCount",
        &[REF | 16, DATA + 652],
        0,
    );
    f.call(
        "GdipGetStringFormatMeasurableCharacterRangeCount",
        &[REF | 16, DATA + 656],
        0,
    );
    f.call(
        "GdipGetStringFormatTabStops",
        &[REF | 16, 4, DATA + 660, DATA + 1100],
        0,
    );
    // Native NotImplemented for backwards stops is an argument-driven result.
    f.call(
        "GdipSetStringFormatTabStops",
        &[REF | 16, 0, 1, DATA + 1016],
        6,
    );
    f.call(
        "GdipSetStringFormatTabStops",
        &[REF | 16, (-1f32).to_bits(), 0, DATA + 1000],
        2,
    );
    f.call("GdipSetStringFormatTabStops", &[REF | 16, 0, 0, 0], 2);
    f.call(
        "GdipGetStringFormatTabStops",
        &[REF | 16, 3, DATA + 708, DATA + 712],
        2,
    );
    f.call(
        "GdipGetStringFormatTabStops",
        &[REF | 16, 4, DATA + 708, DATA + 4092],
        2,
    );
    f.call(
        "GdipSetStringFormatMeasurableCharacterRanges",
        &[REF | 16, 32, DATA + 1300],
        0,
    );
    f.call(
        "GdipSetStringFormatMeasurableCharacterRanges",
        &[REF | 16, 33, 0],
        11,
    );
    f.call(
        "GdipGetStringFormatMeasurableCharacterRangeCount",
        &[REF | 16, DATA + 664],
        0,
    );
    f.call(
        "GdipSetStringFormatMeasurableCharacterRanges",
        &[REF | 16, u32::MAX, 0],
        2,
    );
    f.call(
        "GdipSetStringFormatMeasurableCharacterRanges",
        &[REF | 16, 1, 0],
        2,
    );
    f.call("GdipSetStringFormatHotkeyPrefix", &[REF | 16, 3], 2);
    f.call("GdipSetStringFormatTrimming", &[REF | 16, 6], 2);
    f.call("GdipGetStringFormatHotkeyPrefix", &[0, DATA + 728], 2);
    f.call("GdipGetStringFormatTrimming", &[REF | 16, 0], 2);
    f.call("GdipSetStringFormatDigitSubstitution", &[0, 0, 0], 2);
    f.call(
        "GdipGetStringFormatDigitSubstitution",
        &[REF | 16, DATA + 732, DATA + 4094],
        2,
    );
    f.call("GdipGetStringFormatTabStopCount", &[REF | 4, DATA + 668], 0);
    f.call(
        "GdipGetStringFormatMeasurableCharacterRangeCount",
        &[REF | 4, DATA + 672],
        0,
    );
    for slot in [4, 8, 12, 16] {
        f.call("GdipDeleteStringFormat", &[REF | slot], 0);
    }
    f.call("GdiplusShutdown", &[REF], 0);
    let m = f.run();
    for (at, expected) in [
        (600, u32::MAX),
        (604, 0),
        (608, 0),
        (612, 1),
        (616, 0),
        (620, 1),
        (624, 0x6004),
        (628, 0),
        (632, 0),
        (636, 0),
        (640, u32::MAX),
        (644, 2),
        (648, 5),
        (652, 4),
        (656, 2),
        (660, 10f32.to_bits()),
        (664, 32),
        (668, 0),
        (672, 0),
        (700, 0xcccc0000),
        (704, 0xccccffff),
        (708, 0xcccccccc),
        (712, 0xcccccccc),
        (728, 0xcccccccc),
        (732, 0xcccccccc),
    ] {
        assert_eq!(m.cpu.read_u32(DATA + at).unwrap(), expected, "output {at}");
    }
    for (i, expected) in [1f32, 2.5, f32::INFINITY, f32::NAN].into_iter().enumerate() {
        let actual = f32::from_bits(m.cpu.read_u32(DATA + 1100 + i as u32 * 4).unwrap());
        assert!(actual == expected || actual.is_nan() && expected.is_nan());
    }
}
#[test]
// spec: DEV-IMAGE-6 DEV-IMAGE-11
fn string_format_defaults_and_mutable_layout_arrays() {
    settings(JitCpu::new(PARK_ADDR, false).unwrap());
    settings(JitCpu::new(PARK_ADDR, true).unwrap());
    settings(UnicornCpu::new(PARK_ADDR).unwrap());
}
