use super::*;

#[test]
#[cfg(windows)]
// spec: WIN32-GDI-11
fn placement_arrays_and_partial_results_match_native_for_both_encodings() {
    let (mut machine, data, dc) = fixture();
    let font_id = selected_font(&machine, dc);
    for (face, charset, codepage) in [
        ("Arial", 0, unsafe {
            windows_sys::Win32::Globalization::GetACP()
        }),
        ("MS Gothic", 128, 932),
        ("Microsoft YaHei", 134, 936),
    ] {
        let Some(GdiObj::Font(font)) = machine
            .ext
            .get_mut::<Win32>()
            .gdi
            .get_mut(&mut machine.sys.objects, font_id)
        else {
            panic!("font")
        };
        font.face = face.into();
        font.raw[23] = charset;
        machine.ext.get_mut::<Win32>().codec = adv_os::Codec::new(codepage);
        let native = native_font(&machine, dc);
        for wide in [false, true] {
            for (input, flags, maximum) in [
                ("AV fi", 0, 100),
                ("AV fi", 8, 100),
                ("AV fi", 0x100000, 30),
                ("abc", 2, 100),
                ("あいう", 0, 100),
                ("中文", 0, 100),
                ("אבג abc", 2, 100),
                ("سلام", 2 | 0x20, 100),
            ] {
                if !wide && charset == 0 && !input.is_ascii() {
                    continue;
                }
                let utf16: Vec<_> = input.encode_utf16().collect();
                let encoded: Vec<_> = utf16.iter().flat_map(|unit| unit.to_le_bytes()).collect();
                let ansi_input = machine.ext.get::<Win32>().codec.encode(input);
                let count = if wide { utf16.len() } else { ansi_input.len() };
                machine
                    .cpu
                    .write(data + 128, if wide { &encoded } else { &ansi_input })
                    .unwrap();
                for capacity in [0, 2, 16] {
                    let mut text = [0xcdcdu16; 16];
                    let mut ansi = [0xcdu8; 16];
                    let mut order = [0xcdcdcdcdu32; 16];
                    let mut advances = [0xcdcdcdcdu32 as i32; 16];
                    let mut caret = [0xcdcdcdcdu32 as i32; 16];
                    let mut classes = [0xcdu8; 16];
                    let mut glyphs = [0xcdcdu16; 16];
                    let mut result = gdi::GCP_RESULTSW {
                        lStructSize: std::mem::size_of::<gdi::GCP_RESULTSW>() as u32,
                        lpOutString: if wide {
                            text.as_mut_ptr()
                        } else {
                            ansi.as_mut_ptr().cast()
                        },
                        lpOrder: order.as_mut_ptr(),
                        lpDx: advances.as_mut_ptr(),
                        lpCaretPos: caret.as_mut_ptr(),
                        lpClass: classes.as_mut_ptr(),
                        lpGlyphs: glyphs.as_mut_ptr(),
                        nGlyphs: capacity,
                        nMaxFit: 0x12345678,
                    };
                    let extent = unsafe {
                        if wide {
                            gdi::GetCharacterPlacementW(
                                native.dc,
                                utf16.as_ptr(),
                                utf16.len() as i32,
                                maximum,
                                &mut result,
                                flags,
                            )
                        } else {
                            gdi::GetCharacterPlacementA(
                                native.dc,
                                ansi_input.as_ptr(),
                                ansi_input.len() as i32,
                                maximum,
                                (&mut result as *mut gdi::GCP_RESULTSW).cast(),
                                flags,
                            )
                        }
                    };
                    machine.cpu.write(data + 576, &[0xcd; 352]).unwrap();
                    let fields = [
                        36,
                        data + 576,
                        data + 640,
                        data + 704,
                        data + 768,
                        data + 832,
                        data + 896,
                        capacity,
                        0x12345678,
                    ];
                    for (index, word) in fields.into_iter().enumerate() {
                        machine
                            .cpu
                            .write_u32(data + 512 + index as u32 * 4, word)
                            .unwrap();
                    }
                    let got = invoke(
                        &mut machine,
                        data,
                        if wide {
                            "GetCharacterPlacementW"
                        } else {
                            "GetCharacterPlacementA"
                        },
                        &[
                            dc,
                            data + 128,
                            count as u32,
                            maximum as u32,
                            data + 512,
                            flags,
                        ],
                    );
                    assert_eq!(got, extent, "wide={wide} capacity={capacity} flags={flags}");
                    assert_eq!(machine.cpu.read_u32(data + 540).unwrap(), result.nGlyphs);
                    assert_eq!(
                        machine.cpu.read_u32(data + 544).unwrap(),
                        result.nMaxFit as u32
                    );
                    let string: Vec<_> = if wide {
                        text.into_iter().flat_map(u16::to_le_bytes).collect()
                    } else {
                        ansi.to_vec()
                    };
                    for (pointer, expected) in [
                        (data + 576, string),
                        (
                            data + 640,
                            order.into_iter().flat_map(u32::to_le_bytes).collect(),
                        ),
                        (
                            data + 704,
                            advances.into_iter().flat_map(i32::to_le_bytes).collect(),
                        ),
                        (
                            data + 768,
                            caret.into_iter().flat_map(i32::to_le_bytes).collect(),
                        ),
                        (data + 832, classes.to_vec()),
                        (
                            data + 896,
                            glyphs.into_iter().flat_map(u16::to_le_bytes).collect(),
                        ),
                    ] {
                        let mut actual = vec![0; expected.len()];
                        machine.cpu.read(pointer, &mut actual).unwrap();
                        assert_eq!(
                            actual, expected,
                            "wide={wide} capacity={capacity} flags={flags} buffer={pointer}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
// spec: WIN32-GDI-11
fn placement_rejects_bad_optional_arrays_before_any_output_changes() {
    let (mut machine, data, dc) = fixture();
    machine.cpu.write(data + 128, &65u16.to_le_bytes()).unwrap();
    let buffer = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.write_u32(buffer + 4092, 0xcdcdcdcd).unwrap();
    machine
        .cpu
        .protect(buffer + 4096, 4096, Prot::READ)
        .unwrap();
    let fields = [36, 0, data + 640, buffer + 4092, 0, 0, 0, 2, 0x12345678];
    machine.cpu.write(data + 640, &[0xcd; 8]).unwrap();
    for (index, word) in fields.into_iter().enumerate() {
        machine
            .cpu
            .write_u32(data + 512 + index as u32 * 4, word)
            .unwrap();
    }
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "GetCharacterPlacementW",
            &[dc, data + 128, 1, 100, data + 512, 0]
        ),
        0
    );
    assert_eq!(machine.cpu.read_u32(data + 640).unwrap(), 0xcdcdcdcd);
    assert_eq!(machine.cpu.read_u32(data + 540).unwrap(), 2);
    assert_eq!(machine.cpu.read_u32(buffer + 4092).unwrap(), 0xcdcdcdcd);
}
