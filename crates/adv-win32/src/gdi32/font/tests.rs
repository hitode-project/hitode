use super::*;
#[path = "tests/outline_metrics.rs"]
mod outline_metrics;
#[path = "tests/pairs.rs"]
mod pairs;
#[path = "tests/placement.rs"]
mod placement;
#[path = "tests/spacing.rs"]
mod spacing;
#[path = "tests/widths.rs"]
mod widths;
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
        context: machine.sys.current,
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
fn set_di_bits_copies_rows_to_bitmap() {
    let (mut machine, data, _dc) = fixture();
    let hbm = invoke(&mut machine, data, "CreateBitmap", &[2, 2, 1, 32, 0]);
    assert_ne!(hbm, 0);

    let info = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    let mut header = [0u8; 40];
    header[0..4].copy_from_slice(&40u32.to_le_bytes());
    header[4..8].copy_from_slice(&2i32.to_le_bytes());
    header[8..12].copy_from_slice(&2i32.to_le_bytes()); // biHeight positive = bottom-up
    header[12..14].copy_from_slice(&1u16.to_le_bytes());
    header[14..16].copy_from_slice(&24u16.to_le_bytes()); // 24 bpp
    machine.cpu.write(info, &header).unwrap();

    let bits = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    let dib_data = [
        0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0x00, 0x00, // bottom row (Red, Green, 2 pad)
        0xff, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, // top row (Blue, White, 2 pad)
    ];
    machine.cpu.write(bits, &dib_data).unwrap();

    let result = invoke(
        &mut machine,
        data,
        "SetDIBits",
        &[0, hbm, 0, 2, bits, info, 0],
    );
    assert_eq!(result, 2);

    let Some(GdiObj::Bitmap {
        bits: dst_bits,
        stride,
        ..
    }) = machine
        .ext
        .get::<Win32>()
        .gdi
        .get(&machine.sys.objects, hbm)
    else {
        panic!("bitmap missing");
    };
    let mut top_row = [0u8; 8];
    machine.cpu.read(*dst_bits, &mut top_row).unwrap();
    assert_eq!(&top_row[0..4], &[0xff, 0x00, 0x00, 0xff]); // Blue
    assert_eq!(&top_row[4..8], &[0xff, 0xff, 0xff, 0xff]); // White

    let mut bottom_row = [0u8; 8];
    machine
        .cpu
        .read(*dst_bits + stride, &mut bottom_row)
        .unwrap();
    assert_eq!(&bottom_row[0..4], &[0x00, 0x00, 0xff, 0xff]); // Red
    assert_eq!(&bottom_row[4..8], &[0x00, 0xff, 0x00, 0xff]); // Green

    assert_eq!(invoke(&mut machine, data, "DeleteObject", &[hbm]), 1);
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
fn restore_dc_returns_selections_colors_and_the_bitmap_by_level() {
    let (mut machine, data, dc) = fixture();
    let font = selected_font(&machine, dc);
    let bitmap = invoke(&mut machine, data, "CreateCompatibleBitmap", &[dc, 4, 4]);
    assert_ne!(bitmap, 0);
    invoke(&mut machine, data, "SetTextColor", &[dc, 0x11]);
    assert_eq!(invoke(&mut machine, data, "SaveDC", &[dc]), 1);
    let stock_bitmap = invoke(&mut machine, data, "SelectObject", &[dc, bitmap]);
    invoke(&mut machine, data, "SelectObject", &[dc, STOCK_FONT]);
    invoke(&mut machine, data, "SetTextColor", &[dc, 0x22]);
    invoke(&mut machine, data, "SetBkMode", &[dc, 1]);
    assert_eq!(invoke(&mut machine, data, "SaveDC", &[dc]), 2);
    invoke(&mut machine, data, "SetTextColor", &[dc, 0x33]);
    assert_eq!(invoke(&mut machine, data, "RestoreDC", &[dc, 0]), 0);
    assert_eq!(invoke(&mut machine, data, "RestoreDC", &[dc, 3]), 0);
    assert_eq!(
        invoke(&mut machine, data, "RestoreDC", &[dc, (-3i32) as u32]),
        0
    );
    // -1 brings back the newest state and pops it.
    assert_eq!(invoke(&mut machine, data, "RestoreDC", &[dc, u32::MAX]), 1);
    assert_eq!(
        invoke(&mut machine, data, "SetTextColor", &[dc, 0x22]),
        0x22
    );
    assert_eq!(invoke(&mut machine, data, "SetBkMode", &[dc, 1]), 1);
    assert_eq!(invoke(&mut machine, data, "RestoreDC", &[dc, 2]), 0);
    assert_eq!(invoke(&mut machine, data, "RestoreDC", &[dc, 1]), 1);
    assert_eq!(
        invoke(&mut machine, data, "SetTextColor", &[dc, 0x11]),
        0x11
    );
    assert_eq!(invoke(&mut machine, data, "SetBkMode", &[dc, 2]), 2);
    assert_eq!(selected_font(&machine, dc), font);
    assert_eq!(
        invoke(&mut machine, data, "SelectObject", &[dc, stock_bitmap]),
        stock_bitmap
    );
    assert_eq!(invoke(&mut machine, data, "RestoreDC", &[dc, u32::MAX]), 0);
    assert_eq!(invoke(&mut machine, data, "SaveDC", &[0x1234]), 0);
}

fn invoke_draw_text(machine: &mut Machine<JitCpu>, data: u32, wide: bool, args: &[u32]) -> u32 {
    let address = machine
        .resolve_import("user32.dll", if wide { "DrawTextW" } else { "DrawTextA" })
        .unwrap();
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
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: index,
    });
    let Outcome::Return(value) = result else {
        panic!("DrawText measurement must return")
    };
    value
}

#[test]
fn draw_text_measurement_rejects_unwritable_rectangles_and_unsupported_layouts() {
    let (mut m, data, dc) = fixture();
    let rect = data + 128;
    let original = [3i32, 4, 100, 100]
        .into_iter()
        .flat_map(i32::to_le_bytes)
        .collect::<Vec<_>>();
    m.cpu.write(rect, &original).unwrap();
    m.cpu.write(data, b"A\0").unwrap();
    for (hdc, pointer, count, rectangle, flags) in [
        (0xdeadbeef, data, 1, rect, 0x440),
        (dc, data, -2i32 as u32, rect, 0x440),
        (dc, 0xdeadbeef, 1, rect, 0x440),
        (dc, data, 1, 0xdeadbeef, 0x440),
        (dc, data, 1, rect, 0),
        (dc, data, 1, rect, 0x410),
    ] {
        assert_eq!(
            invoke_draw_text(
                &mut m,
                data,
                false,
                &[hdc, pointer, count, rectangle, flags]
            ),
            0
        );
        let mut actual = vec![0; 16];
        m.cpu.read(rect, &mut actual).unwrap();
        assert_eq!(actual, original);
    }
    let protected = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
    m.cpu.write(protected, &original).unwrap();
    m.cpu.protect(protected, 4096, Prot::READ).unwrap();
    assert_eq!(
        invoke_draw_text(&mut m, data, false, &[dc, data, 1, protected, 0x440]),
        0
    );
    let mut actual = vec![0; 16];
    m.cpu.read(protected, &mut actual).unwrap();
    assert_eq!(actual, original);
}

#[test]
fn draw_text_ansi_measurement_uses_guest_acp_before_font_formatting() {
    let (mut m, data, dc) = fixture();
    let rect = data + 129;
    let initial = [3i32, 4, 150, 150]
        .into_iter()
        .flat_map(i32::to_le_bytes)
        .collect::<Vec<_>>();
    for (cp, raw) in [
        (932, vec![0xb1]),
        (936, vec![0xb1]),
        (1252, vec![0xb1]),
        (932, vec![0x82, 0xa0]),
        (936, vec![0xd6, 0xd0]),
    ] {
        m.ext.get_mut::<Win32>().codec = adv_os::Codec::new(cp);
        let units = m.ext.get::<Win32>().codec.to_utf16(0, &raw);
        m.cpu
            .write(data + 512, &[raw.as_slice(), &[0]].concat())
            .unwrap();
        m.cpu
            .write(
                data + 640,
                &units
                    .iter()
                    .copied()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        for (ansi_count, wide_count) in
            [(raw.len() as u32, units.len() as u32), (u32::MAX, u32::MAX)]
        {
            m.cpu.write(rect, &initial).unwrap();
            let a = invoke_draw_text(
                &mut m,
                data,
                false,
                &[dc, data + 512, ansi_count, rect, 0x440],
            );
            let mut a_rect = [0; 16];
            m.cpu.read(rect, &mut a_rect).unwrap();
            m.cpu.write(rect, &initial).unwrap();
            let w = invoke_draw_text(
                &mut m,
                data,
                true,
                &[dc, data + 640, wide_count, rect, 0x440],
            );
            let mut w_rect = [0; 16];
            m.cpu.read(rect, &mut w_rect).unwrap();
            assert_eq!((a, a_rect), (w, w_rect), "guest cp={cp}");
        }
    }
}

#[test]
#[cfg(windows)]
fn draw_text_calc_rect_matches_native_selected_font_tabs_prefixes_and_line_breaks() {
    use core::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn DrawTextA(
            dc: *mut c_void,
            input: *const u8,
            count: i32,
            rect: *mut i32,
            flags: u32,
        ) -> i32;
        fn DrawTextW(
            dc: *mut c_void,
            input: *const u16,
            count: i32,
            rect: *mut i32,
            flags: u32,
        ) -> i32;
    }
    let (mut m, data, dc) = fixture();
    let native = native_font(&m, dc);
    let before = m
        .ext
        .try_get::<adv_device::surface::SurfaceStore>()
        .map(|s| s.stats.presentations);
    for wide in [false, true] {
        for input in ["", "A", "A\n", "A\r\nB", "A\tB", "\t", "&A&&B", "A\0B"] {
            for flags in [0x400, 0x440, 0xc40, 0x460, 0x640] {
                let mut rectangle = [3i32, 4, 150, 150];
                let units = input.encode_utf16().chain([0]).collect::<Vec<_>>();
                let raw = input
                    .as_bytes()
                    .iter()
                    .copied()
                    .chain([0])
                    .collect::<Vec<_>>();
                // SAFETY: counted input and a complete mutable RECT; the oracle owns its DC.
                let expected = unsafe {
                    if wide {
                        DrawTextW(
                            native.dc,
                            units.as_ptr(),
                            input.encode_utf16().count() as i32,
                            rectangle.as_mut_ptr(),
                            flags,
                        )
                    } else {
                        DrawTextA(
                            native.dc,
                            raw.as_ptr(),
                            input.len() as i32,
                            rectangle.as_mut_ptr(),
                            flags,
                        )
                    }
                };
                m.cpu
                    .write(
                        data,
                        &if wide {
                            units
                                .iter()
                                .flat_map(|u| u.to_le_bytes())
                                .collect::<Vec<_>>()
                        } else {
                            raw
                        },
                    )
                    .unwrap();
                let rect = data + 129; // guest RECT need not be aligned
                m.cpu
                    .write(
                        rect,
                        &[3i32, 4, 150, 150]
                            .into_iter()
                            .flat_map(i32::to_le_bytes)
                            .collect::<Vec<_>>(),
                    )
                    .unwrap();
                let actual = invoke_draw_text(
                    &mut m,
                    data,
                    wide,
                    &[dc, data, input.len() as u32, rect, flags],
                );
                assert_eq!(
                    actual, expected as u32,
                    "input={input:?} flags={flags:x} wide={wide}"
                );
                let mut actual_rect = [0; 16];
                m.cpu.read(rect, &mut actual_rect).unwrap();
                assert_eq!(
                    actual_rect.to_vec(),
                    rectangle
                        .into_iter()
                        .flat_map(i32::to_le_bytes)
                        .collect::<Vec<_>>(),
                    "input={input:?} flags={flags:x} wide={wide}"
                );
            }
        }
    }
    assert_eq!(
        m.ext
            .try_get::<adv_device::surface::SurfaceStore>()
            .map(|s| s.stats.presentations),
        before
    );
}

#[test]
fn software_dc_preserves_rop2_per_context_and_normalizes_the_native_byte() {
    let (mut machine, data, dc) = fixture();
    let other = invoke(&mut machine, data, "CreateCompatibleDC", &[0]);
    assert_ne!(other, 0);
    assert_eq!(invoke(&mut machine, data, "GetROP2", &[dc]), 13);
    let mut previous = 13;
    for mode in 1..=16 {
        assert_eq!(invoke(&mut machine, data, "SetROP2", &[dc, mode]), previous);
        assert_eq!(invoke(&mut machine, data, "GetROP2", &[dc]), mode);
        assert_eq!(invoke(&mut machine, data, "GetROP2", &[other]), 13);
        previous = mode;
    }
    for (input, actual) in [(0, 0), (17, 17), (u32::MAX, 255), (256, 0), (0x100000d, 13)] {
        assert_eq!(
            invoke(&mut machine, data, "SetROP2", &[dc, input]),
            previous
        );
        assert_eq!(invoke(&mut machine, data, "GetROP2", &[dc]), actual);
        assert_eq!(invoke(&mut machine, data, "GetROP2", &[other]), 13);
        previous = actual;
    }
    assert_eq!(invoke(&mut machine, data, "DeleteDC", &[other]), 1);
    assert_eq!(invoke(&mut machine, data, "GetROP2", &[other]), 0);
    assert_eq!(invoke(&mut machine, data, "SetROP2", &[other, 13]), 0);
    assert_eq!(invoke(&mut machine, data, "GetROP2", &[0xdeadbeef]), 0);
}

#[test]
#[cfg(windows)]
fn rop2_state_matches_native_memory_dc() {
    use core::ffi::c_void;
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateCompatibleDC(source: *mut c_void) -> *mut c_void;
        fn DeleteDC(dc: *mut c_void) -> i32;
        fn GetROP2(dc: *mut c_void) -> i32;
        fn SetROP2(dc: *mut c_void, mode: i32) -> i32;
    }
    let (mut machine, data, dc) = fixture();
    // SAFETY: the native DC is owned by this test and retired after comparison.
    let native = unsafe { CreateCompatibleDC(core::ptr::null_mut()) };
    assert!(!native.is_null());
    for mode in [13, 7, 1, 16, 0, 17, -1, 2, 256, i32::MIN, i32::MAX] {
        assert_eq!(
            invoke(&mut machine, data, "SetROP2", &[dc, mode as u32]),
            unsafe { SetROP2(native, mode) } as u32
        );
        assert_eq!(
            invoke(&mut machine, data, "GetROP2", &[dc]),
            unsafe { GetROP2(native) } as u32
        );
    }
    assert_eq!(unsafe { DeleteDC(native) }, 1);
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
fn glyph_cache_shares_decoded_characters_and_separates_transform_and_font() {
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
    assert_eq!(machine.ext.get::<Fonts>().text.stats().glyphs, 1);
    transform[2..4].copy_from_slice(&2i16.to_le_bytes());
    machine.cpu.write(matrix, &transform).unwrap();
    assert!(query(&mut machine, "GetGlyphOutlineW") > original);
    assert_eq!(machine.ext.get::<Fonts>().text.stats().glyphs, 2);
    // Use a stock outline font. SYSTEM_FONT is a bitmap font and native
    // GetGlyphOutline correctly fails for it.
    invoke(&mut machine, data, "SelectObject", &[dc, 0x0190_0010 + 17]);
    assert_ne!(query(&mut machine, "GetGlyphOutlineW"), GDI_ERROR);
    assert_eq!(machine.ext.get::<Fonts>().text.stats().glyphs, 3);
    assert_eq!(machine.ext.get::<Fonts>().text.stats().hits, 2);
}

#[test]
#[cfg(windows)]
fn explicit_dbcs_charsets_share_unicode_glyphs_across_guest_codepages() {
    for (charset, guest_codepage, face, characters) in [
        (
            128,
            936,
            "MS Gothic",
            vec![(0x82a0, 'あ'), (0xad, 'ｭ'), (0x1234, '4')],
        ),
        (
            134,
            932,
            "SimSun",
            vec![(0xd6d0, '中'), (0xcec4, '文'), (0x1234, '4')],
        ),
    ] {
        let (mut machine, data, dc) = fixture();
        machine.ext.get_mut::<Win32>().codec = adv_os::Codec::new(guest_codepage);
        let mut raw = [0u8; 60];
        raw[..4].copy_from_slice(&(-24i32).to_le_bytes());
        raw[16..20].copy_from_slice(&400i32.to_le_bytes());
        raw[23] = charset;
        raw[28..28 + face.len()].copy_from_slice(face.as_bytes());
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
        for (packed, character) in characters {
            let mut outputs = Vec::new();
            let before = machine.ext.get::<Fonts>().text.stats();
            for (api, code) in [
                ("GetGlyphOutlineA", packed),
                ("GetGlyphOutlineW", character as u32),
            ] {
                let size = invoke(
                    &mut machine,
                    data,
                    api,
                    &[dc, code, 6, metrics, 0, 0, matrix],
                );
                assert_ne!(size, GDI_ERROR);
                assert!(size > 0);
                assert_eq!(
                    invoke(
                        &mut machine,
                        data,
                        api,
                        &[dc, code, 6, metrics, size, buffer, matrix]
                    ),
                    size
                );
                let mut pixels = vec![0; size as usize];
                let mut layout = [0; 20];
                machine.cpu.read(buffer, &mut pixels).unwrap();
                machine.cpu.read(metrics, &mut layout).unwrap();
                outputs.push((layout, pixels));
            }
            assert_eq!(
                outputs[0], outputs[1],
                "charset={charset} packed={packed:04x}"
            );
            assert_eq!(
                machine.ext.get::<Fonts>().text.stats().glyphs,
                before.glyphs + 1,
                "ANSI and Unicode must identify the same cached glyph"
            );
        }
    }
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
fn default_charset_glyphs_use_guest_codepage_and_keep_wide_pixels() {
    let (mut machine, data, dc) = fixture();
    let matrix = data + 128;
    let metrics = data + 160;
    let buffer = data + 256;
    let mut transform = [0u8; 16];
    transform[2..4].copy_from_slice(&1i16.to_le_bytes());
    transform[14..16].copy_from_slice(&1i16.to_le_bytes());
    machine.cpu.write(matrix, &transform).unwrap();
    for charset in [0, 1] {
        let mut raw = [0u8; 60];
        raw[..4].copy_from_slice(&(-24i32).to_le_bytes());
        raw[16..20].copy_from_slice(&400i32.to_le_bytes());
        raw[23] = charset;
        raw[28..38].copy_from_slice(b"MS Gothic\0");
        machine.cpu.write(data, &raw).unwrap();
        let font = invoke(&mut machine, data, "CreateFontIndirectA", &[data]);
        invoke(&mut machine, data, "SelectObject", &[dc, font]);
        for (codepage, text) in [(932, "あｭ"), (936, "中文")] {
            machine.ext.get_mut::<Win32>().codec = adv_os::Codec::new(codepage);
            for character in text.chars() {
                let codec = adv_os::Codec::new(codepage);
                let (bytes, lossy) = codec.from_utf16(0, &[character as u16]);
                assert!(!lossy);
                let packed = bytes.iter().fold(0u32, |n, b| (n << 8) | u32::from(*b));
                let mut outputs = Vec::new();
                for (api, code) in [
                    ("GetGlyphOutlineA", packed),
                    ("GetGlyphOutlineW", character as u32),
                ] {
                    let size = invoke(
                        &mut machine,
                        data,
                        api,
                        &[dc, code, 6, metrics, 0, 0, matrix],
                    );
                    assert_ne!(size, GDI_ERROR);
                    assert_eq!(
                        invoke(
                            &mut machine,
                            data,
                            api,
                            &[dc, code, 6, metrics, size, buffer, matrix]
                        ),
                        size
                    );
                    let mut pixels = vec![0; size as usize];
                    machine.cpu.read(buffer, &mut pixels).unwrap();
                    let mut layout = [0; 20];
                    machine.cpu.read(metrics, &mut layout).unwrap();
                    outputs.push((layout, pixels));
                }
                assert_eq!(outputs[0], outputs[1], "cp={codepage} charset={charset}");
            }
        }
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
    for character in [0xad, 0x82a0, 0xecff, 0x00ff_ecff, 0xffff_82a0] {
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
        assert_eq!(size, expected, "packed character={character:08x}");
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
