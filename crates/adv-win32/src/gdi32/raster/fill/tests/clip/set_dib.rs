//! Authored native memory-DC oracle for scan-band uploads, origins and clips.
use super::*;

fn header(width: u32, height: i32) -> [u32; 10] {
    [40, width, height as u32, 0x0020_0001, 0, 0, 0, 0, 0, 0]
}

#[test]
#[cfg(windows)]
fn device_dib_upload_matches_native_scan_bands_origins_and_clipping() {
    use windows_sys::Win32::Graphics::Gdi as gdi;
    let (mut m, data, dc, pixels) = fixture(6, 6, PixelFormat::Bgra32, false);
    let target = header(6, -6);
    let mut pointer = core::ptr::null_mut();
    // SAFETY: owned memory DC and DIB, with counted buffers for all inputs;
    // this test never obtains or draws into a desktop/window DC.
    let native_dc = unsafe { gdi::CreateCompatibleDC(core::ptr::null_mut()) };
    let bitmap = unsafe {
        gdi::CreateDIBSection(
            native_dc,
            target.as_ptr().cast(),
            0,
            &mut pointer,
            core::ptr::null_mut(),
            0,
        )
    };
    assert!(!bitmap.is_null() && !pointer.is_null());
    let old = unsafe { gdi::SelectObject(native_dc, bitmap) };
    let source: [u32; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    let info = data + 1024;
    let bits = data + 1100;
    m.cpu
        .write(
            bits,
            &source
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    for translated in [false, true] {
        if translated {
            assert_eq!(
                call(&mut m, data, "gdi32.dll!SetViewportOrgEx", &[dc, 1, 1, 0]),
                1
            );
            rect(&mut m, data, [1, 1, 5, 5]);
            assert_ne!(
                call(
                    &mut m,
                    data,
                    "gdi32.dll!IntersectClipRect",
                    &[dc, 1, 1, 5, 5]
                ),
                0
            );
            unsafe {
                gdi::SetViewportOrgEx(native_dc, 1, 1, core::ptr::null_mut());
                gdi::IntersectClipRect(native_dc, 1, 1, 5, 5);
            }
        }
        for height in [4, -4] {
            let input = header(2, height);
            m.cpu
                .write(
                    info,
                    &input
                        .into_iter()
                        .flat_map(u32::to_le_bytes)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            for (start, count, h, sy) in [
                (0, 4, 4, 0),
                (0, 2, 4, 0),
                (1, 2, 4, 0),
                (3, 1, 4, 0),
                (0, 4, 2, 0),
                (0, 4, 2, 1),
                (1, 2, 2, 1),
                (0, 4, 1, 2),
                (0, 0, 4, 0),
            ] {
                m.cpu.write(pixels, &[0; 144]).unwrap();
                unsafe {
                    core::ptr::write_bytes(pointer, 0, 144);
                }
                let expected = unsafe {
                    gdi::SetDIBitsToDevice(
                        native_dc,
                        2,
                        1,
                        2,
                        h,
                        0,
                        sy,
                        start,
                        count,
                        source.as_ptr().cast(),
                        input.as_ptr().cast(),
                        0,
                    )
                };
                unsafe {
                    gdi::GdiFlush();
                }
                assert_eq!(
                    call(
                        &mut m,
                        data,
                        "gdi32.dll!SetDIBitsToDevice",
                        &[dc, 2, 1, 2, h, 0, sy as u32, start, count, bits, info, 0]
                    ),
                    expected as u32,
                    "height={height} start={start} count={count} h={h} sy={sy}"
                );
                let mut actual = [0; 144];
                m.cpu.read(pixels, &mut actual).unwrap();
                let expected = unsafe { core::slice::from_raw_parts(pointer.cast::<u8>(), 144) };
                assert_eq!(
                    actual, expected,
                    "pixels: height={height} start={start} count={count} h={h} sy={sy}"
                );
            }
        }
    }
    // A rejected range or unreadable input must leave the destination intact.
    m.cpu.write(pixels, &[0xa5; 144]).unwrap();
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!SetDIBitsToDevice",
            &[dc, 0, 0, 2, 4, 0, 0, 3, 4, bits, info, 0]
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 50);
    assert_eq!(
        call(
            &mut m,
            data,
            "gdi32.dll!SetDIBitsToDevice",
            &[dc, 0, 0, 2, 4, 0, 0, 0, 4, u32::MAX - 3, info, 0]
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m)), 87);
    let mut actual = [0; 144];
    m.cpu.read(pixels, &mut actual).unwrap();
    assert_eq!(actual, [0xa5; 144]);
    unsafe {
        gdi::SelectObject(native_dc, old);
        gdi::DeleteObject(bitmap);
        gdi::DeleteDC(native_dc);
    }
}

#[test]
fn true_colour_palette_flag_creates_and_uploads_real_pixels_through_both_dib_apis() {
    for bpp in [16u32, 24, 32] {
        let (mut m, data, dc, pixels) = fixture(2, 2, PixelFormat::Bgra32, false);
        let info = data + 1024;
        let bits = data + 1100;
        let output = data + 1200;
        let header = [40u32, 2, (-2i32) as u32, (bpp << 16) | 1, 0, 0, 0, 0, 0, 0];
        m.cpu
            .write(
                info,
                &header
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let source: Vec<u8> = match bpp {
            16 => [0x7c00u16, 0x03e0, 0x001f, 0x7fff]
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect(),
            24 => vec![0, 0, 255, 0, 255, 0, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0],
            _ => vec![0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0],
        };
        m.cpu.write(bits, &source).unwrap();
        let bitmap = call(
            &mut m,
            data,
            "gdi32.dll!CreateDIBSection",
            &[dc, info, 1, output, 0, 0],
        );
        assert_ne!(bitmap, 0);
        assert_ne!(m.cpu.read_u32(output).unwrap(), 0);
        assert_eq!(call(&mut m, data, "gdi32.dll!DeleteObject", &[bitmap]), 1);
        for stretch in [true, false] {
            m.cpu.write(pixels, &[0; 16]).unwrap();
            let (api, args) = if stretch {
                (
                    "gdi32.dll!StretchDIBits",
                    vec![dc, 0, 0, 2, 2, 0, 0, 2, 2, bits, info, 1, 0xcc0020],
                )
            } else {
                (
                    "gdi32.dll!SetDIBitsToDevice",
                    vec![dc, 0, 0, 2, 2, 0, 0, 0, 2, bits, info, 1],
                )
            };
            assert_eq!(call(&mut m, data, api, &args), 2);
            assert_eq!(call(&mut m, data, "gdi32.dll!GetPixel", &[dc, 0, 0]), 0xff);
            assert_eq!(
                call(&mut m, data, "gdi32.dll!GetPixel", &[dc, 1, 0]),
                0xff00
            );
        }
        assert_eq!(
            call(
                &mut m,
                data,
                "gdi32.dll!CreateDIBSection",
                &[dc, info, 2, output, 0, 0]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(output).unwrap(), 0);
    }
}
