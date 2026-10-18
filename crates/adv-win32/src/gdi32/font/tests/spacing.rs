use super::*;

#[test]
#[cfg(windows)]
// spec: WIN32-GDI-10
fn unicode_coverage_matches_native_selected_face_and_checks_complete_buffer() {
    let (mut machine, data, dc) = fixture();
    let font_id = selected_font(&machine, dc);
    for face in ["Arial", "MS Gothic", "Terminal"] {
        let Some(GdiObj::Font(font)) = machine
            .ext
            .get_mut::<Win32>()
            .gdi
            .get_mut(&mut machine.sys.objects, font_id)
        else {
            panic!("font");
        };
        font.face = face.into();
        let native = native_font(&machine, dc);
        assert_eq!(
            invoke(&mut machine, data, "GetFontLanguageInfo", &[dc]),
            unsafe { gdi::GetFontLanguageInfo(native.dc) }
        );
        let size = unsafe { gdi::GetFontUnicodeRanges(native.dc, std::ptr::null_mut()) };
        assert_eq!(
            invoke(&mut machine, data, "GetFontUnicodeRanges", &[dc, 0]),
            size
        );
        if size == 0 {
            continue;
        }
        let mut expected = vec![0u32; size as usize / 4];
        assert_eq!(
            unsafe { gdi::GetFontUnicodeRanges(native.dc, expected.as_mut_ptr().cast()) },
            size
        );
        let output = machine
            .sys
            .vm
            .alloc(&mut machine.cpu, size, Prot::RW)
            .unwrap();
        assert_eq!(
            invoke(&mut machine, data, "GetFontUnicodeRanges", &[dc, output]),
            size
        );
        for (index, word) in expected.into_iter().enumerate() {
            assert_eq!(
                machine.cpu.read_u32(output + index as u32 * 4).unwrap(),
                word,
                "{face} word {index}"
            );
        }
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
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetFontUnicodeRanges",
                &[dc, buffer + 4092]
            ),
            0
        );
        assert_eq!(machine.cpu.read_u32(buffer + 4092).unwrap(), 0xcdcdcdcd);
    }
}

#[test]
#[cfg(windows)]
// spec: WIN32-GDI-10
fn glyph_spacing_matches_native_ranges_arrays_defaults_and_bitmap_fonts() {
    let (mut machine, data, dc) = fixture();
    let font_id = selected_font(&machine, dc);
    for face in ["Arial", "MS Gothic", "Terminal"] {
        for height in [-12, -25, -38] {
            for italic in [false, true] {
                let Some(GdiObj::Font(font)) = machine
                    .ext
                    .get_mut::<Win32>()
                    .gdi
                    .get_mut(&mut machine.sys.objects, font_id)
                else {
                    panic!("font");
                };
                font.face = face.into();
                font.height = height;
                font.italic = italic;
                font.raw[..4].copy_from_slice(&height.to_le_bytes());
                font.raw[20] = u8::from(italic);
                let native = native_font(&machine, dc);
                let mut indices = [0, 0, 0, 0, 65535];
                let units: Vec<_> = "Afx?".encode_utf16().collect();
                unsafe {
                    gdi::GetGlyphIndicesW(native.dc, units.as_ptr(), 4, indices.as_mut_ptr(), 0);
                }
                for (i, index) in indices.into_iter().enumerate() {
                    machine
                        .cpu
                        .write(data + 512 + i as u32 * 2, &index.to_le_bytes())
                        .unwrap();
                }
                for (first, count, array) in [
                    (0, 4, false),
                    (u32::MAX, 5, true),
                    (65535, 1, false),
                    (65537, 1, false),
                    (0, 0, false),
                ] {
                    let mut expected_widths = [0xcdcdcdcdu32 as i32; 5];
                    let mut expected_abc = [gdi::ABC {
                        abcA: 0xcdcdcdcdu32 as i32,
                        abcB: 0xcdcdcdcd,
                        abcC: 0xcdcdcdcdu32 as i32,
                    }; 5];
                    let pointer = if array {
                        indices.as_mut_ptr()
                    } else {
                        std::ptr::null_mut()
                    };
                    let width_ok = unsafe {
                        gdi::GetCharWidthI(
                            native.dc,
                            first,
                            count,
                            pointer,
                            expected_widths.as_mut_ptr(),
                        )
                    };
                    let abc_ok = unsafe {
                        gdi::GetCharABCWidthsI(
                            native.dc,
                            first,
                            count,
                            pointer,
                            expected_abc.as_mut_ptr(),
                        )
                    };
                    let guest_pointer = if array { data + 512 } else { 0 };
                    machine.cpu.write(data + 1024, &[0xcd; 64]).unwrap();
                    assert_eq!(
                        invoke(
                            &mut machine,
                            data,
                            "GetCharWidthI",
                            &[dc, first, count, guest_pointer, data + 1024]
                        ),
                        u32::from(width_ok != 0),
                        "{face} {height} {italic} width"
                    );
                    for (i, expected) in expected_widths.iter().enumerate() {
                        assert_eq!(
                            machine.cpu.read_u32(data + 1024 + i as u32 * 4).unwrap() as i32,
                            *expected,
                            "{face} width {i}"
                        );
                    }
                    machine.cpu.write(data + 1024, &[0xcd; 64]).unwrap();
                    assert_eq!(
                        invoke(
                            &mut machine,
                            data,
                            "GetCharABCWidthsI",
                            &[dc, first, count, guest_pointer, data + 1024]
                        ),
                        u32::from(abc_ok != 0),
                        "{face} {height} {italic} abc"
                    );
                    for (i, abc) in expected_abc.into_iter().enumerate() {
                        let actual: [u32; 3] = std::array::from_fn(|part| {
                            machine
                                .cpu
                                .read_u32(data + 1024 + i as u32 * 12 + part as u32 * 4)
                                .unwrap()
                        });
                        assert_eq!(
                            actual,
                            [abc.abcA as u32, abc.abcB, abc.abcC as u32],
                            "{face} abc {i}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
#[cfg(windows)]
fn abc_character_ranges_preserve_native_bearings_and_dbcs_charset() {
    let (mut machine, data, dc) = fixture();
    machine.ext.get_mut::<Win32>().codec =
        adv_os::Codec::new(unsafe { windows_sys::Win32::Globalization::GetACP() });
    for wide in [false, true] {
        let native = native_font(&machine, dc);
        let mut expected = [gdi::ABC {
            abcA: 0,
            abcB: 0,
            abcC: 0,
        }; 96];
        let ok = unsafe {
            if wide {
                gdi::GetCharABCWidthsW(native.dc, 32, 127, expected.as_mut_ptr())
            } else {
                gdi::GetCharABCWidthsA(native.dc, 32, 127, expected.as_mut_ptr())
            }
        };
        assert_ne!(ok, 0);
        assert_eq!(
            invoke(
                &mut machine,
                data,
                if wide {
                    "GetCharABCWidthsW"
                } else {
                    "GetCharABCWidthsA"
                },
                &[dc, 32, 127, data + 512]
            ),
            1
        );
        for (i, abc) in expected.into_iter().enumerate() {
            let actual: [u32; 3] = std::array::from_fn(|part| {
                machine
                    .cpu
                    .read_u32(data + 512 + i as u32 * 12 + part as u32 * 4)
                    .unwrap()
            });
            assert_eq!(actual, [abc.abcA as u32, abc.abcB, abc.abcC as u32]);
        }
    }
}

#[test]
fn spacing_checks_entire_outputs_and_reads_overlapping_index_arrays_first() {
    let (mut machine, data, dc) = fixture();
    let buffer = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.write(buffer, &[0xcd; 4096]).unwrap();
    machine
        .cpu
        .protect(buffer + 4096, 4096, Prot::READ)
        .unwrap();
    for args in [
        [dc, 0, 2, 0, buffer + 4092],
        [dc, 0, 65537, 0, buffer],
        [dc, 0, 1, u32::MAX, buffer],
        [0, 0, 1, 0, buffer],
        [dc, 0, 1, 0, 0],
    ] {
        assert_eq!(
            invoke(&mut machine, data, "GetCharABCWidthsI", &args),
            0,
            "{args:?}"
        );
        assert_eq!(machine.cpu.read_u32(buffer).unwrap(), 0xcdcdcdcd);
        assert_eq!(machine.cpu.read_u32(buffer + 4092).unwrap(), 0xcdcdcdcd);
    }
    machine.cpu.write(buffer, &[0, 0, 1, 0, 2, 0]).unwrap();
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "GetCharABCWidthsI",
            &[dc, 0, 3, buffer, data + 512]
        ),
        1
    );
    let mut expected = [0; 36];
    machine.cpu.read(data + 512, &mut expected).unwrap();
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "GetCharABCWidthsI",
            &[dc, 0, 3, buffer, buffer]
        ),
        1
    );
    let mut actual = [0; 36];
    machine.cpu.read(buffer, &mut actual).unwrap();
    assert_eq!(actual, expected);
}
