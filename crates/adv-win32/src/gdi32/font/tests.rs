use super::*;
use crate as adv_win32;
use adv_cpu::JitCpu;
use adv_cpu::backend::{Guest, GuestExt, Prot, Reg};
use adv_machine::{Clock, Machine};
#[cfg(windows)]
use windows_sys::Win32::Graphics::Gdi as gdi;

fn selected_font(machine: &Machine<JitCpu>, dc: u32) -> u32 {
    match machine.ext.get::<Win32>().gdi.get(&machine.sys.objects, dc) {
        Some(GdiObj::Dc { selected, .. }) => selected[2],
        _ => panic!("font fixture DC missing"),
    }
}

#[cfg(windows)]
struct NativeOracle {
    dc: gdi::HDC,
    font: gdi::HFONT,
    old: gdi::HGDIOBJ,
}
#[cfg(windows)]
fn native_font(machine: &Machine<JitCpu>, dc: u32) -> NativeOracle {
    let Some(GdiObj::Font(font)) = machine
        .ext
        .get::<Win32>()
        .gdi
        .get(&machine.sys.objects, selected_font(machine, dc))
    else {
        panic!("font")
    };
    let raw = &font.raw;
    let integer = |at| i32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
    // Native oracle consumes the original guest LOGFONT attributes, rather
    // than the front's neutral request or the device's cached native font.
    let mut logical = gdi::LOGFONTW {
        lfHeight: integer(0),
        lfWidth: integer(4),
        lfEscapement: integer(8),
        lfOrientation: integer(12),
        lfWeight: integer(16),
        lfItalic: raw[20],
        lfUnderline: raw[21],
        lfStrikeOut: raw[22],
        lfCharSet: raw[23],
        lfOutPrecision: raw[24],
        lfClipPrecision: raw[25],
        lfQuality: raw[26],
        lfPitchAndFamily: raw[27],
        ..Default::default()
    };
    for (slot, unit) in logical
        .lfFaceName
        .iter_mut()
        .take(31)
        .zip(font.face.encode_utf16())
    {
        *slot = unit;
    }
    // SAFETY: valid host LOGFONT and owned native objects; assertions ensure
    // that no null handle reaches the subsequent native differential calls.
    unsafe {
        let dc = gdi::CreateCompatibleDC(std::ptr::null_mut());
        assert!(!dc.is_null());
        let font = gdi::CreateFontIndirectW(&logical);
        assert!(!font.is_null());
        let old = gdi::SelectObject(dc, font);
        assert!(!old.is_null());
        NativeOracle { dc, font, old }
    }
}
#[cfg(windows)]
impl NativeOracle {
    fn outline(
        &self,
        ansi: bool,
        code: u32,
        format: u32,
        matrix: &[u8; 16],
    ) -> (u32, [u8; 20], Vec<u8>) {
        let fixed = |at| gdi::FIXED {
            fract: u16::from_le_bytes(matrix[at..at + 2].try_into().unwrap()),
            value: i16::from_le_bytes(matrix[at + 2..at + 4].try_into().unwrap()),
        };
        let transform = gdi::MAT2 {
            eM11: fixed(0),
            eM12: fixed(4),
            eM21: fixed(8),
            eM22: fixed(12),
        };
        let call = if ansi {
            gdi::GetGlyphOutlineA
        } else {
            gdi::GetGlyphOutlineW
        };
        let mut metrics = gdi::GLYPHMETRICS::default();
        // SAFETY: owned DC, initialized host outputs and null query buffer.
        let size = unsafe {
            call(
                self.dc,
                code,
                format,
                &mut metrics,
                0,
                std::ptr::null_mut(),
                &transform,
            )
        };
        let mut data = Vec::new();
        if size != GDI_ERROR && format & 0x7f != 0 {
            data.resize(size as usize, 0);
            // SAFETY: data covers the exact size returned by the native query.
            assert_eq!(
                unsafe {
                    call(
                        self.dc,
                        code,
                        format,
                        &mut metrics,
                        size,
                        data.as_mut_ptr().cast(),
                        &transform,
                    )
                },
                size
            );
        }
        let mut packed = [0; 20];
        for (i, value) in [
            metrics.gmBlackBoxX,
            metrics.gmBlackBoxY,
            metrics.gmptGlyphOrigin.x as u32,
            metrics.gmptGlyphOrigin.y as u32,
        ]
        .into_iter()
        .enumerate()
        {
            packed[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        packed[16..18].copy_from_slice(&metrics.gmCellIncX.to_le_bytes());
        packed[18..20].copy_from_slice(&metrics.gmCellIncY.to_le_bytes());
        (size, packed, data)
    }
    fn text_metrics(&self) -> [u8; 56] {
        let mut m = gdi::TEXTMETRICA::default();
        // SAFETY: owned DC and initialized writable host output.
        assert_ne!(unsafe { gdi::GetTextMetricsA(self.dc, &mut m) }, 0);
        let mut bytes = [0; 56];
        for (i, value) in [
            m.tmHeight,
            m.tmAscent,
            m.tmDescent,
            m.tmInternalLeading,
            m.tmExternalLeading,
            m.tmAveCharWidth,
            m.tmMaxCharWidth,
            m.tmWeight,
            m.tmOverhang,
            m.tmDigitizedAspectX,
            m.tmDigitizedAspectY,
        ]
        .into_iter()
        .enumerate()
        {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[44..53].copy_from_slice(&[
            m.tmFirstChar,
            m.tmLastChar,
            m.tmDefaultChar,
            m.tmBreakChar,
            m.tmItalic,
            m.tmUnderlined,
            m.tmStruckOut,
            m.tmPitchAndFamily,
            m.tmCharSet,
        ]);
        bytes
    }
}

#[test]
#[cfg(windows)]
fn native_bitmap_outline_formats_fixed_transforms_and_guest_layouts_match() {
    let (mut machine, data, dc) = fixture();
    let native = native_font(&machine, dc);
    for transform in [[65536i32, 0, 0, 65536], [98304, 16384, -32768, 65536]] {
        let matrix: [u8; 16] = transform
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        machine.cpu.write(data + 128, &matrix).unwrap();
        for hinted in [true, false] {
            for format in 0..=6 {
                let format = format | if hinted { 0 } else { 0x100 };
                let (size, metrics, pixels) =
                    native.outline(false, u32::from(b'g'), format, &matrix);
                assert_ne!(size, GDI_ERROR, "native format={format:#x}");
                assert_eq!(
                    invoke(
                        &mut machine,
                        data,
                        "GetGlyphOutlineW",
                        &[dc, u32::from(b'g'), format, data + 160, 0, 0, data + 128]
                    ),
                    size
                );
                let mut actual_metrics = [0; 20];
                machine.cpu.read(data + 160, &mut actual_metrics).unwrap();
                assert_eq!(actual_metrics, metrics);
                assert_eq!(
                    invoke(
                        &mut machine,
                        data,
                        "GetGlyphOutlineW",
                        &[
                            dc,
                            u32::from(b'g'),
                            format,
                            data + 160,
                            size,
                            data + 256,
                            data + 128
                        ]
                    ),
                    size
                );
                let mut actual = vec![0; pixels.len()];
                machine.cpu.read(data + 256, &mut actual).unwrap();
                assert_eq!(actual, pixels, "format={format:#x} transform={transform:?}");
            }
        }
    }
}

#[test]
#[cfg(windows)]
fn font_matching_attributes_and_reserved_hints_keep_native_metrics() {
    let (mut machine, data, dc) = fixture();
    for (offset, values) in [
        (20usize, [0, 1, 7, 255]),
        (21, [0, 1, 7, 255]),
        (22, [0, 1, 7, 255]),
        (23, [0, 1, 128, 42]),
        (24, [0, 4, 8, 255]),
        (25, [0, 16, 64, 255]),
        (26, [0, 3, 6, 255]),
        (27, [0, 17, 49, 255]),
    ] {
        for value in values {
            let mut raw = [0; 60];
            raw[..4].copy_from_slice(&(-24i32).to_le_bytes());
            raw[16..20].copy_from_slice(&400i32.to_le_bytes());
            raw[28..34].copy_from_slice(b"Arial\0");
            raw[offset] = value;
            machine.cpu.write(data, &raw).unwrap();
            let font = invoke(&mut machine, data, "CreateFontIndirectA", &[data]);
            assert_ne!(font, 0);
            invoke(&mut machine, data, "SelectObject", &[dc, font]);
            let expected = native_font(&machine, dc).text_metrics();
            assert_eq!(
                invoke(&mut machine, data, "GetTextMetricsA", &[dc, data + 128]),
                1
            );
            let mut actual = [0; 56];
            machine.cpu.read(data + 128, &mut actual).unwrap();
            assert_eq!(actual, expected, "LOGFONT offset={offset} value={value}");
        }
    }
}

#[test]
#[cfg(windows)]
fn invalid_guest_outputs_do_not_partially_publish_metrics() {
    let (mut machine, data, dc) = fixture();
    let matrix: Vec<_> = [65536i32, 0, 0, 65536]
        .into_iter()
        .flat_map(i32::to_le_bytes)
        .collect();
    machine.cpu.write(data + 128, &matrix).unwrap();
    let size = invoke(
        &mut machine,
        data,
        "GetGlyphOutlineW",
        &[dc, 65, 6, data + 160, 0, 0, data + 128],
    );
    assert!(size > 0 && size != GDI_ERROR);
    let unmapped = (1..64)
        .map(|i| data + i * 65536)
        .find(|&address| !machine.cpu.accessible(address, size, Prot::WRITE))
        .unwrap();
    for output in [0, u32::MAX - 2, unmapped - 6] {
        machine.cpu.write(data + 160, &[0xa5; 20]).unwrap();
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetGlyphOutlineW",
                &[dc, 65, 6, data + 160, size, output, data + 128]
            ),
            if output == 0 { size } else { GDI_ERROR }
        );
        if output != 0 {
            let mut metrics = [0; 20];
            machine.cpu.read(data + 160, &mut metrics).unwrap();
            assert_eq!(metrics, [0xa5; 20]);
        }
    }
    let readonly = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::READ)
        .unwrap();
    machine.cpu.write(data + 160, &[0xa5; 20]).unwrap();
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "GetGlyphOutlineW",
            &[dc, 65, 6, data + 160, size, readonly, data + 128]
        ),
        GDI_ERROR
    );
    let mut metrics = [0; 20];
    machine.cpu.read(data + 160, &mut metrics).unwrap();
    assert_eq!(metrics, [0xa5; 20]);
    assert_eq!(
        invoke(&mut machine, data, "GetTextMetricsA", &[dc, readonly]),
        0
    );
}
#[cfg(windows)]
impl Drop for NativeOracle {
    fn drop(&mut self) {
        // SAFETY: restores selection before releasing owned oracle handles.
        unsafe {
            gdi::SelectObject(self.dc, self.old);
            gdi::DeleteObject(self.font);
            gdi::DeleteDC(self.dc);
        }
    }
}

fn fixture() -> (Machine<JitCpu>, u32, u32) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    register(&mut registry);
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    adv_win32::Win32::install(
        &mut machine,
        adv_win32::Win32Config::new(".", "font-test.exe", "target/font-test-overlay"),
    )
    .unwrap();
    install(&mut machine);
    let data = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    machine
        .spawn("font test", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let dc = invoke(&mut machine, data, "CreateCompatibleDC", &[0]);
    let mut raw = [0u8; 60];
    raw[..4].copy_from_slice(&(-24i32).to_le_bytes());
    raw[16..20].copy_from_slice(&400i32.to_le_bytes());
    raw[28..34].copy_from_slice(b"Arial\0");
    machine.cpu.write(data, &raw).unwrap();
    let font = invoke(&mut machine, data, "CreateFontIndirectA", &[data]);
    assert_ne!(font, 0);
    assert_eq!(
        invoke(&mut machine, data, "SelectObject", &[dc, font]),
        STOCK_FONT
    );
    (machine, data, dc)
}
fn invoke(machine: &mut Machine<JitCpu>, data: u32, name: &str, args: &[u32]) -> u32 {
    let address = machine.resolve_import("gdi32.dll", name).unwrap();
    let index = (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let handler = machine.sys.thunks.get(index).f.handler;
    machine.cpu.set_reg(Reg::Esp, data + 2048);
    for (i, &arg) in args.iter().enumerate() {
        machine
            .cpu
            .write_u32(data + 2052 + i as u32 * 4, arg)
            .unwrap();
    }
    let result = handler(&mut Env {
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: index,
    });
    let Outcome::Return(value) = result else {
        panic!("font call must return")
    };
    value
}

#[test]
#[cfg(windows)]
fn ansi_extent_uses_guest_locale_instead_of_host_acp() {
    let (mut machine, data, dc) = fixture();
    for (codepage, bytes, unicode) in [
        (932, vec![0xb1, 0x41], vec![0xff71u16, 0x41]),
        (936, vec![0xd6, 0xd0, 0x41], vec![0x4e2du16, 0x41]),
    ] {
        machine.ext.get_mut::<Win32>().codec = adv_os::Codec::new(codepage);
        machine.cpu.write(data + 512, &bytes).unwrap();
        let wide: Vec<_> = unicode.iter().flat_map(|unit| unit.to_le_bytes()).collect();
        machine.cpu.write(data + 640, &wide).unwrap();
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetTextExtentPoint32A",
                &[dc, data + 512, bytes.len() as u32, data + 1024]
            ),
            1
        );
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetTextExtentPoint32W",
                &[dc, data + 640, unicode.len() as u32, data + 1040]
            ),
            1
        );
        let mut ansi_size = [0; 8];
        let mut wide_size = [0; 8];
        machine.cpu.read(data + 1024, &mut ansi_size).unwrap();
        machine.cpu.read(data + 1040, &mut wide_size).unwrap();
        assert_eq!(ansi_size, wide_size, "guest code page {codepage}");
    }
}
#[test]
fn text_extent_rejects_short_readonly_and_overflowing_ranges_without_partial_size() {
    let (mut machine, data, dc) = fixture();
    let output = data + 1024;
    machine.cpu.write(data + 512, b"AV\0x").unwrap();
    for (pointer, count) in [
        (u32::MAX - 1, 4),
        (data + 512, u32::MAX),
        (data + 512, 2 * 1024 * 1024),
    ] {
        machine.cpu.write(output, &[0xcc; 8]).unwrap();
        for name in ["GetTextExtentPoint32A", "GetTextExtentPoint32W"] {
            assert_eq!(
                invoke(&mut machine, data, name, &[dc, pointer, count, output]),
                0
            );
            let mut actual = [0; 8];
            machine.cpu.read(output, &mut actual).unwrap();
            assert_eq!(actual, [0xcc; 8]);
        }
    }
    let read_only = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    machine.cpu.write(read_only, &[0xcd; 8]).unwrap();
    machine.cpu.protect(read_only, 4096, Prot::READ).unwrap();
    for name in ["GetTextExtentPoint32A", "GetTextExtentPoint32W"] {
        assert_eq!(
            invoke(&mut machine, data, name, &[dc, data + 512, 2, read_only]),
            0
        );
        let mut actual = [0; 8];
        machine.cpu.read(read_only, &mut actual).unwrap();
        assert_eq!(actual, [0xcd; 8]);
    }
    assert_eq!(
        machine.ext.get::<Fonts>().text.stats().fonts,
        0,
        "invalid outputs fail before native font matching"
    );
}
#[test]
#[cfg(windows)]
fn measurements_bound_native_font_owners_without_invalidating_cached_glyph_results() {
    let (mut machine, _data, dc) = fixture();
    let Some(GdiObj::Font(font)) = machine
        .ext
        .get::<Win32>()
        .gdi
        .get(&machine.sys.objects, selected_font(&machine, dc))
    else {
        panic!("font");
    };
    let original = from_font(font);
    let request = GlyphRequest {
        character: Character::Unicode(u32::from(b'W')),
        format: GlyphFormat::Gray8,
        hinted: true,
        transform: Transform::IDENTITY,
    };
    let text = &mut machine.ext.get_mut::<Fonts>().text;
    let retained = text.glyph(&original, request).unwrap();
    let pixels = retained.data.clone();
    for width in 1..=text::FONT_CACHE_CAPACITY + 8 {
        let mut font = original.clone();
        font.width = width as i32;
        let extent = text
            .extent(&font, text::TextInput::Unicode(&[u16::from(b'W')]))
            .unwrap();
        assert!(extent.width > 0);
        assert!(text.stats().fonts <= text::FONT_CACHE_CAPACITY);
    }
    let hits = text.stats().hits;
    let reused = text.glyph(&original, request).unwrap();
    assert!(std::sync::Arc::ptr_eq(&retained, &reused));
    assert_eq!(reused.data, pixels);
    assert_eq!(text.stats().hits, hits + 1);
}
#[test]
fn bitmap_source_and_palette_outputs_obey_permissions_and_never_wrap_or_leak() {
    let (mut machine, data, _dc) = fixture();
    let source = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::WRITE)
        .unwrap();
    machine.cpu.write(source, &[0x37; 16]).unwrap();
    let objects_before = machine.sys.objects.object_count();
    let first_free = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    machine.sys.vm.free(&mut machine.cpu, first_free).unwrap();
    for pointer in [u32::MAX - 3, source] {
        for _ in 0..8 {
            assert_eq!(
                invoke(&mut machine, data, "CreateBitmap", &[2, 2, 1, 32, pointer]),
                0
            );
            assert_eq!(machine.sys.objects.object_count(), objects_before);
        }
    }
    let after = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    assert_eq!(
        after, first_free,
        "rejected source validation must not leave a guest allocation"
    );
    machine.sys.vm.free(&mut machine.cpu, after).unwrap();
    let zero = invoke(&mut machine, data, "CreateBitmap", &[0, 7, 1, 32, 0]);
    assert_ne!(zero, 0);
    assert!(matches!(
        machine
            .ext
            .get::<Win32>()
            .gdi
            .get(&machine.sys.objects, zero),
        Some(GdiObj::Bitmap {
            width: 1,
            height: 1,
            bpp: 1,
            ..
        })
    ));
    assert_eq!(invoke(&mut machine, data, "DeleteObject", &[zero]), 1);
    assert_eq!(machine.sys.objects.object_count(), objects_before);

    // DEFAULT_PALETTE returns only its two-byte entry count. The entire count
    // must be writable even if the backend's host write can bypass guest protection.
    let output = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    machine.cpu.write(output, &[0xcd; 2]).unwrap();
    machine.cpu.protect(output, 4096, Prot::READ).unwrap();
    assert_eq!(
        invoke(&mut machine, data, "GetObjectA", &[0x0190_001f, 2, output]),
        0
    );
    assert_eq!(machine.cpu.read_u16(output).unwrap(), 0xcdcd);
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "GetObjectA",
            &[0x0190_001f, 2, u32::MAX]
        ),
        0
    );
}
#[test]
fn software_dc_preserves_font_and_mapping_state() {
    let (mut machine, data, dc) = fixture();
    assert_eq!(invoke(&mut machine, data, "SetMapMode", &[dc, 1]), 1);
    assert_eq!(invoke(&mut machine, data, "SetMapMode", &[dc, 2]), 0);
    assert_eq!(invoke(&mut machine, data, "GetMapMode", &[dc]), 1);
    assert_eq!(invoke(&mut machine, data, "SetPolyFillMode", &[dc, 2]), 1);
    assert_eq!(invoke(&mut machine, data, "GetPolyFillMode", &[dc]), 2);
    assert_eq!(invoke(&mut machine, data, "SetPolyFillMode", &[dc, 0]), 0);
    let font = selected_font(&machine, dc);
    assert_eq!(invoke(&mut machine, data, "DeleteObject", &[font]), 0);
    assert_eq!(
        invoke(&mut machine, data, "SelectObject", &[dc, STOCK_FONT]),
        font
    );
    assert_eq!(invoke(&mut machine, data, "DeleteObject", &[font]), 1);
    assert!(
        machine
            .ext
            .get::<Win32>()
            .gdi
            .get(&machine.sys.objects, font)
            .is_none()
    );
    assert_eq!(invoke(&mut machine, data, "DeleteDC", &[dc]), 1);
    assert_eq!(invoke(&mut machine, data, "GetMapMode", &[dc]), 0);
}
#[test]
#[cfg(windows)]
fn glyph_metrics_bitmap_gray8_and_index_use_real_font_pixels() {
    let (mut machine, data, dc) = fixture();
    let matrix = data + 128;
    let metrics = data + 160;
    let buffer = data + 256;
    let mut identity = [0u8; 16];
    identity[2..4].copy_from_slice(&1i16.to_le_bytes());
    identity[14..16].copy_from_slice(&1i16.to_le_bytes());
    machine.cpu.write(matrix, &identity).unwrap();
    for format in [0u32, 1, 6] {
        let size = invoke(
            &mut machine,
            data,
            "GetGlyphOutlineA",
            &[dc, u32::from(b'A'), format, metrics, 0, 0, matrix],
        );
        assert_ne!(size, GDI_ERROR);
        let width = machine.cpu.read_u32(metrics).unwrap();
        let height = machine.cpu.read_u32(metrics + 4).unwrap();
        assert!(width > 0 && height > 0);
        if format == 0 {
            // GDI returns its glyph data size even when only metrics are requested.
            assert!(size > 0);
            continue;
        }
        let pitch = if format == 1 {
            width.div_ceil(32) * 4
        } else {
            (width + 3) & !3
        };
        assert_eq!(size, pitch * height);
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetGlyphOutlineA",
                &[dc, u32::from(b'A'), format, metrics, size, buffer, matrix]
            ),
            size
        );
        let mut bytes = vec![0; size as usize];
        machine.cpu.read(buffer, &mut bytes).unwrap();
        assert!(bytes.iter().any(|&b| b != 0));
        let hits = machine.ext.get::<Fonts>().text.stats().hits;
        machine
            .cpu
            .write(buffer, &vec![0xa5; size as usize])
            .unwrap();
        // A cached size query must not write the caller's zero-capacity buffer.
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetGlyphOutlineA",
                &[dc, u32::from(b'A'), format, metrics, 0, buffer, matrix]
            ),
            size
        );
        let mut queried = vec![0; size as usize];
        machine.cpu.read(buffer, &mut queried).unwrap();
        assert!(queried.iter().all(|&byte| byte == 0xa5));
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetGlyphOutlineA",
                &[dc, u32::from(b'A'), format, metrics, size, buffer, matrix]
            ),
            size
        );
        machine.cpu.read(buffer, &mut queried).unwrap();
        assert_eq!(queried, bytes);
        assert_eq!(machine.ext.get::<Fonts>().text.stats().hits, hits + 2);
        if format == 6 {
            assert!(bytes.iter().all(|&b| b <= 64));
        }
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetGlyphOutlineA",
                &[
                    dc,
                    u32::from(b'A'),
                    format,
                    metrics,
                    size - 1,
                    buffer,
                    matrix
                ]
            ),
            GDI_ERROR
        );
    }
    let native = native_font(&machine, dc);
    let mut index = 0u16;
    // SAFETY: an owned native DC, a single UTF-16 character and one output glyph index.
    assert_eq!(
        unsafe { gdi::GetGlyphIndicesW(native.dc, &u16::from(b'A'), 1, &mut index, 0) },
        1
    );
    assert_ne!(
        invoke(
            &mut machine,
            data,
            "GetGlyphOutlineA",
            &[dc, u32::from(index), 6 | 0x80, metrics, 0, 0, matrix]
        ),
        GDI_ERROR
    );
}

#[test]
#[cfg(windows)]
fn glyph_cache_separates_transform_encoding_and_selected_font() {
    let (mut machine, data, dc) = fixture();
    let matrix = data + 128;
    let mut transform = [0u8; 16];
    transform[2..4].copy_from_slice(&1i16.to_le_bytes());
    transform[14..16].copy_from_slice(&1i16.to_le_bytes());
    machine.cpu.write(matrix, &transform).unwrap();
    let query = |machine: &mut Machine<JitCpu>, name: &str| {
        invoke(
            machine,
            data,
            name,
            &[dc, u32::from(b'A'), 6, data + 160, 0, 0, matrix],
        )
    };
    let original = query(&mut machine, "GetGlyphOutlineW");
    assert_ne!(original, GDI_ERROR);
    assert_eq!(query(&mut machine, "GetGlyphOutlineW"), original);
    assert_eq!(machine.ext.get::<Fonts>().text.stats().hits, 1);
    assert_eq!(query(&mut machine, "GetGlyphOutlineA"), original);
    assert_eq!(machine.ext.get::<Fonts>().text.stats().glyphs, 2);
    transform[2..4].copy_from_slice(&2i16.to_le_bytes());
    machine.cpu.write(matrix, &transform).unwrap();
    assert!(query(&mut machine, "GetGlyphOutlineW") > original);
    assert_eq!(machine.ext.get::<Fonts>().text.stats().glyphs, 3);
    // Use a stock outline font. SYSTEM_FONT is a bitmap font and native
    // GetGlyphOutline correctly fails for it.
    invoke(&mut machine, data, "SelectObject", &[dc, 0x0190_0010 + 17]);
    assert_ne!(query(&mut machine, "GetGlyphOutlineW"), GDI_ERROR);
    assert_eq!(machine.ext.get::<Fonts>().text.stats().glyphs, 4);
    assert_eq!(machine.ext.get::<Fonts>().text.stats().hits, 1);
}

#[test]
#[cfg(windows)]
fn gbk_packed_characters_match_unicode_glyph_pixels() {
    let (mut machine, data, dc) = fixture();
    let win32 = machine.ext.get_mut::<Win32>();
    // The selected GB2312 font overrides a different process ACP.
    win32.cfg.os.codepage = 932;
    win32.codec = adv_os::Codec::new(932);
    let mut raw = [0u8; 60];
    raw[..4].copy_from_slice(&(-24i32).to_le_bytes());
    raw[16..20].copy_from_slice(&400i32.to_le_bytes());
    raw[23] = 134; // GB2312_CHARSET
    raw[28..35].copy_from_slice(b"SimSun\0");
    machine.cpu.write(data, &raw).unwrap();
    let font = invoke(&mut machine, data, "CreateFontIndirectA", &[data]);
    assert_ne!(font, 0);
    invoke(&mut machine, data, "SelectObject", &[dc, font]);
    let matrix = data + 128;
    let metrics = data + 160;
    let buffer = data + 256;
    let mut identity = [0u8; 16];
    identity[2..4].copy_from_slice(&1i16.to_le_bytes());
    identity[14..16].copy_from_slice(&1i16.to_le_bytes());
    machine.cpu.write(matrix, &identity).unwrap();
    for character in ['中', '文'] {
        let codec = adv_os::Codec::new(936);
        let (encoded, lossy) = codec.from_utf16(0, &[character as u16]);
        assert!(!lossy);
        assert_eq!(encoded.len(), 2);
        let packed = (u32::from(encoded[0]) << 8) | u32::from(encoded[1]);
        let count = invoke(
            &mut machine,
            data,
            "GetGlyphOutlineA",
            &[dc, packed, 6, metrics, 0, 0, matrix],
        );
        assert_ne!(count, GDI_ERROR);
        assert!(count > 0);
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetGlyphOutlineA",
                &[dc, packed, 6, metrics, count, buffer, matrix]
            ),
            count
        );
        let mut ansi = vec![0; count as usize];
        machine.cpu.read(buffer, &mut ansi).unwrap();
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetGlyphOutlineW",
                &[dc, character as u32, 6, metrics, count, buffer, matrix]
            ),
            count
        );
        let mut unicode = vec![0; count as usize];
        machine.cpu.read(buffer, &mut unicode).unwrap();
        assert_eq!(ansi, unicode);
        assert!(ansi.iter().any(|&alpha| alpha > 0));
        assert!(ansi.iter().all(|&alpha| alpha <= 64));
    }
}

#[test]
#[cfg(windows)]
fn selected_charset_and_metrics_match_native_gdi_across_process_acp() {
    let (mut machine, data, dc) = fixture();
    let win32 = machine.ext.get_mut::<Win32>();
    win32.cfg.os.codepage = 936;
    win32.codec = adv_os::Codec::new(936);
    let mut raw = [0u8; 60];
    raw[..4].copy_from_slice(&(-24i32).to_le_bytes());
    raw[16..20].copy_from_slice(&400i32.to_le_bytes());
    raw[23] = 128; // SHIFTJIS_CHARSET, even though ACP is GBK.
    raw[28..38].copy_from_slice(b"MS Gothic\0");
    machine.cpu.write(data, &raw).unwrap();
    let font = invoke(&mut machine, data, "CreateFontIndirectA", &[data]);
    invoke(&mut machine, data, "SelectObject", &[dc, font]);
    let matrix = data + 128;
    let metrics = data + 160;
    let buffer = data + 256;
    let mut identity = [0u8; 16];
    identity[2..4].copy_from_slice(&1i16.to_le_bytes());
    identity[14..16].copy_from_slice(&1i16.to_le_bytes());
    machine.cpu.write(matrix, &identity).unwrap();
    for character in [0xad, 0x82a0, 0x00ff_ecff] {
        let size = invoke(
            &mut machine,
            data,
            "GetGlyphOutlineA",
            &[dc, character, 6, metrics, 0, 0, matrix],
        );
        assert_ne!(size, GDI_ERROR);
        let native = native_font(&machine, dc);
        let mut native_metrics = gdi::GLYPHMETRICS::default();
        let transform = gdi::MAT2 {
            eM11: gdi::FIXED { value: 1, fract: 0 },
            eM22: gdi::FIXED { value: 1, fract: 0 },
            ..Default::default()
        };
        // SAFETY: an owned native DC, initialized metrics/transform and a null query buffer.
        let expected = unsafe {
            gdi::GetGlyphOutlineA(
                native.dc,
                character,
                6,
                &mut native_metrics,
                0,
                std::ptr::null_mut(),
                &transform,
            )
        };
        assert_eq!(size, expected);
        assert_eq!(
            machine.cpu.read_u32(metrics).unwrap(),
            native_metrics.gmBlackBoxX
        );
        assert_eq!(
            machine.cpu.read_u32(metrics + 4).unwrap(),
            native_metrics.gmBlackBoxY
        );
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "GetGlyphOutlineA",
                &[dc, character, 6, metrics, size, buffer, matrix]
            ),
            size
        );
    }
    assert_eq!(
        invoke(&mut machine, data, "GetTextMetricsA", &[dc, metrics]),
        1
    );
    let height = machine.cpu.read_u32(metrics).unwrap();
    let ascent = machine.cpu.read_u32(metrics + 4).unwrap();
    let descent = machine.cpu.read_u32(metrics + 8).unwrap();
    assert_eq!(height, ascent + descent);
    assert!(height >= 24);
    let expected = native_font(&machine, dc).text_metrics();
    let mut actual = [0u8; 56];
    machine.cpu.read(metrics, &mut actual).unwrap();
    assert_eq!(actual, expected);
}
