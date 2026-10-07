use super::*;

#[cfg(windows)]
impl NativeOracle {
    fn outline_metrics_x86(&self) -> Vec<u8> {
        // SAFETY: an owned font DC; a NULL output queries native buffer size.
        let size = unsafe { gdi::GetOutlineTextMetricsA(self.dc, 0, std::ptr::null_mut()) };
        assert!(size >= std::mem::size_of::<gdi::OUTLINETEXTMETRICA>() as u32);
        let mut native = vec![0u64; (size as usize).div_ceil(8)];
        let pointer = native.as_mut_ptr().cast::<gdi::OUTLINETEXTMETRICA>();
        // SAFETY: aligned initialized storage covers the native query result.
        assert_eq!(
            unsafe { gdi::GetOutlineTextMetricsA(self.dc, size, pointer) },
            size
        );
        // SAFETY: GDI initialized the complete native structure and its names.
        let metrics = unsafe { &*pointer };
        let bytes =
            unsafe { std::slice::from_raw_parts(native.as_ptr().cast::<u8>(), size as usize) };
        // The fixed numeric fields precede the pointer-width-dependent names.
        // Read the independent SDK result directly, never the device adapter.
        let mut expected = vec![0; 212];
        expected[..196].copy_from_slice(&bytes[..196]);
        for (index, offset) in [
            metrics.otmpFamilyName,
            metrics.otmpFaceName,
            metrics.otmpStyleName,
            metrics.otmpFullName,
        ]
        .into_iter()
        .enumerate()
        {
            let start = offset as usize;
            let length = bytes[start..].iter().position(|&byte| byte == 0).unwrap() + 1;
            let guest_offset = expected.len() as u32;
            expected[196 + index * 4..200 + index * 4].copy_from_slice(&guest_offset.to_le_bytes());
            expected.extend_from_slice(&bytes[start..start + length]);
        }
        let required = expected.len() as u32;
        expected[..4].copy_from_slice(&required.to_le_bytes());
        expected
    }
}

#[test]
#[cfg(windows)]
fn selected_outline_fields_names_and_baseline_match_native() {
    let (mut machine, data, dc) = fixture();
    for (face, height) in [("Arial", -24i32), ("Tahoma", 24), ("MS Gothic", -24)] {
        let mut raw = [0; 60];
        raw[..4].copy_from_slice(&height.to_le_bytes());
        raw[16..20].copy_from_slice(&400i32.to_le_bytes());
        raw[28..28 + face.len()].copy_from_slice(face.as_bytes());
        machine.cpu.write(data, &raw).unwrap();
        let font = invoke(&mut machine, data, "CreateFontIndirectA", &[data]);
        assert_ne!(font, 0);
        let previous = invoke(&mut machine, data, "SelectObject", &[dc, font]);
        let native = native_font(&machine, dc);
        let expected = native.outline_metrics_x86();
        let required = expected.len() as u32;
        // Independent native W query explains the ANSI thunk's sub-DWORD
        // prefix. Adjust only the host pointer-width-dependent fixed header.
        // SAFETY: the independent oracle owns this font DC; output is NULL.
        let wide_required =
            unsafe { gdi::GetOutlineTextMetricsW(native.dc, 0, std::ptr::null_mut()) }
                - std::mem::size_of::<gdi::OUTLINETEXTMETRICW>() as u32
                + 216;
        let output = data + 512;
        let last_error = machine.sys.contexts[machine.sys.current as usize].teb
            + adv_machine::layout::teb::LAST_ERROR;
        let sentinel = vec![0xa5; expected.len() + 32];
        for capacity in [0, 212, u32::MAX] {
            assert_eq!(
                invoke(
                    &mut machine,
                    data,
                    "GetOutlineTextMetricsA",
                    &[dc, capacity, 0]
                ),
                required,
                "NULL output face={face}"
            );
        }
        for capacity in [
            0,
            1,
            2,
            3,
            4,
            56,
            96,
            196,
            212,
            required - 1,
            required,
            required + 16,
            u32::MAX,
        ] {
            machine.cpu.write(output, &sentinel).unwrap();
            machine.cpu.write_u32(last_error, 0x12345678).unwrap();
            let failed_names = capacity > 212 && capacity < required;
            let copied = if capacity < required {
                capacity.min(212)
            } else {
                required
            };
            let returned = if failed_names { 0 } else { copied };
            let mut wanted = expected.clone();
            if capacity < required {
                wanted[196..212].fill(0);
            }
            if copied >= 4 {
                wanted[..4].copy_from_slice(&returned.to_le_bytes());
            } else {
                wanted[..4].copy_from_slice(&wide_required.to_le_bytes());
            }
            let mut actual = sentinel.clone();
            assert_eq!(
                invoke(
                    &mut machine,
                    data,
                    "GetOutlineTextMetricsA",
                    &[dc, capacity, output]
                ),
                returned,
                "face={face} capacity={capacity}"
            );
            machine.cpu.read(output, &mut actual).unwrap();
            assert_eq!(
                machine.cpu.read_u32(last_error).unwrap(),
                if failed_names { 1003 } else { 0x12345678 }
            );
            assert_eq!(
                &actual[..copied as usize],
                &wanted[..copied as usize],
                "face={face} capacity={capacity}"
            );
            assert!(actual[copied as usize..].iter().all(|&byte| byte == 0xa5));
        }
        let scalar = |at| i32::from_le_bytes(expected[at..at + 4].try_into().unwrap());
        assert!(scalar(96) > 0);
        if face == "Arial" {
            assert_ne!(
                scalar(8),
                scalar(96),
                "cell ascent is not typographic ascent"
            );
        }
        let matrix: [u8; 16] = [65536i32, 0, 0, 65536]
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let (_, native_glyph, _) = native.outline(false, u32::from(b'A'), 0, &matrix);
        machine.cpu.write(data + 128, &matrix).unwrap();
        assert_ne!(
            invoke(
                &mut machine,
                data,
                "GetGlyphOutlineW",
                &[dc, u32::from(b'A'), 0, data + 160, 0, 0, data + 128]
            ),
            GDI_ERROR
        );
        let origin = machine.cpu.read_u32(data + 172).unwrap() as i32;
        let native_origin = i32::from_le_bytes(native_glyph[12..16].try_into().unwrap());
        // A caller positioning a glyph below a top origin gets the same
        // baseline-relative top as a native outline-font query.
        assert_eq!(scalar(96) - origin, scalar(96) - native_origin);
        invoke(&mut machine, data, "SelectObject", &[dc, previous]);
        assert_eq!(invoke(&mut machine, data, "DeleteObject", &[font]), 1);
    }
}

#[test]
#[cfg(windows)]
fn outline_metrics_preflight_prevents_partial_guest_writes() {
    let (mut machine, data, dc) = fixture();
    let pages = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    let readonly = pages + 4096;
    machine.cpu.protect(readonly, 4096, Prot::READ).unwrap();
    let output = readonly - 96;
    for pointer in [readonly, u32::MAX - 3, output] {
        machine.cpu.write(output, &[0xa5; 96]).unwrap();
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetOutlineTextMetricsA",
                &[dc, 212, pointer]
            ),
            0
        );
        let mut unchanged = [0; 96];
        machine.cpu.read(output, &mut unchanged).unwrap();
        assert_eq!(unchanged, [0xa5; 96]);
    }
}

#[test]
#[cfg(not(windows))]
fn unavailable_outline_backend_does_not_publish_zero_success_metrics() {
    let (mut machine, data, dc) = fixture();
    machine.cpu.write(data + 512, &[0xa5; 212]).unwrap();
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "GetOutlineTextMetricsA",
            &[dc, 212, data + 512]
        ),
        0
    );
    let mut bytes = [0; 212];
    machine.cpu.read(data + 512, &mut bytes).unwrap();
    assert_eq!(bytes, [0xa5; 212]);
    assert!(
        machine
            .ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|key| key.0 == "gdi32.dll!GetOutlineTextMetricsA")
    );
}
