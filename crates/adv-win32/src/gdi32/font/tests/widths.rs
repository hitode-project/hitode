use super::*;

#[test]
#[cfg(windows)]
fn ansi_widths_match_native_and_dbcs_ranges_use_the_selected_charset() {
    let (mut machine, data, dc) = fixture();
    machine.ext.get_mut::<Win32>().codec =
        adv_os::Codec::new(unsafe { windows_sys::Win32::Globalization::GetACP() });
    let native = native_font(&machine, dc);
    let mut expected = [0; 256];
    assert_ne!(
        unsafe { gdi::GetCharWidth32A(native.dc, 0, 255, expected.as_mut_ptr()) },
        0
    );
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "GetCharWidth32A",
            &[dc, 0, 255, data + 512]
        ),
        1
    );
    for (index, width) in expected.into_iter().enumerate() {
        assert_eq!(
            machine.cpu.read_u32(data + 512 + index as u32 * 4).unwrap() as i32,
            width
        );
    }
    let mut raw = [0; 60];
    raw[..4].copy_from_slice(&(-24i32).to_le_bytes());
    raw[16..20].copy_from_slice(&400i32.to_le_bytes());
    raw[23] = 128;
    raw[28..38].copy_from_slice(b"MS Gothic\0");
    machine.cpu.write(data, &raw).unwrap();
    let font = invoke(&mut machine, data, "CreateFontIndirectA", &[data]);
    invoke(&mut machine, data, "SelectObject", &[dc, font]);
    machine.ext.get_mut::<Win32>().codec = adv_os::Codec::new(936);
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "GetCharWidth32A",
            &[dc, 0x82a0, 0x82a2, data + 512]
        ),
        1
    );
    let ansi: Vec<_> = (0..3)
        .map(|index| machine.cpu.read_u32(data + 512 + index * 4).unwrap())
        .collect();
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "GetCharWidth32W",
            &[dc, 0x3042, 0x3044, data + 512]
        ),
        1
    );
    let wide: Vec<_> = (0..3)
        .map(|index| machine.cpu.read_u32(data + 512 + index * 4).unwrap())
        .collect();
    assert_eq!(ansi, wide);
}

#[test]
#[cfg(windows)]
fn unicode_character_widths_match_native_selected_fonts_and_default_characters() {
    let (mut machine, data, dc) = fixture();
    let font_id = selected_font(&machine, dc);
    let output = data + 512;
    for (face, charset, italic) in [("Arial", 0, 1), ("MS Gothic", 128, 0), ("Terminal", 255, 0)] {
        let Some(GdiObj::Font(font)) = machine
            .ext
            .get_mut::<Win32>()
            .gdi
            .get_mut(&mut machine.sys.objects, font_id)
        else {
            panic!("selected font missing");
        };
        font.face = face.into();
        font.charset = charset;
        font.italic = italic != 0;
        font.height = -24;
        let native = native_font(&machine, dc);
        for (first, last) in [(0, 255), (0x3042, 0x3044), (0xffff, 0xffff)] {
            let mut expected = vec![-1; (last - first + 1) as usize];
            // SAFETY: the independently owned oracle DC and exact inclusive
            // output allocation are valid throughout the native call.
            let success =
                unsafe { gdi::GetCharWidth32W(native.dc, first, last, expected.as_mut_ptr()) };
            assert_ne!(success, 0);
            machine.cpu.write(output, &[0xcd; 1024]).unwrap();
            assert_eq!(
                invoke(
                    &mut machine,
                    data,
                    "GetCharWidth32W",
                    &[dc, first, last, output]
                ),
                1
            );
            for (index, expected) in expected.into_iter().enumerate() {
                assert_eq!(
                    machine.cpu.read_u32(output + index as u32 * 4).unwrap() as i32,
                    expected,
                    "face={face} character={:04x}",
                    first + index as u32
                );
            }
        }
    }
}

#[test]
fn unicode_width_output_is_atomic_for_bad_ranges_dc_and_page_boundaries() {
    let (mut machine, data, dc) = fixture();
    let output = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.write(output, &[0xcd; 4096]).unwrap();
    machine
        .cpu
        .protect(output + 4096, 4096, Prot::READ)
        .unwrap();
    for args in [
        [dc, 2, 1, output],
        [dc, 0, 65536, output],
        [0, 0, 0, output],
        [dc, 0, 0, 0],
        [dc, 0, 1, output + 4092],
    ] {
        assert_eq!(invoke(&mut machine, data, "GetCharWidth32W", &args), 0);
        let mut actual = [0; 4096];
        machine.cpu.read(output, &mut actual).unwrap();
        assert_eq!(actual, [0xcd; 4096]);
    }
    machine.cpu.protect(output, 4096, Prot::READ).unwrap();
    assert_eq!(
        invoke(&mut machine, data, "GetCharWidth32W", &[dc, 0, 1, output]),
        0
    );
    let mut actual = [0; 4096];
    machine.cpu.read(output, &mut actual).unwrap();
    assert_eq!(actual, [0xcd; 4096]);
}
