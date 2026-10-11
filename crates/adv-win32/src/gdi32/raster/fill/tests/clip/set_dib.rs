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

#[test]
fn indexed_dib_creation_tables_upload_readback_and_clipped_bits_share_real_storage() {
    for bpp in [1u32, 4, 8] {
        let (mut m, data, dc, _) = fixture(9, 2, PixelFormat::Bgra32, false);
        let info = data + 1024;
        let output = data + 2200;
        let readback = data + 2300;
        let header = [40u32, 9, (-2i32) as u32, (bpp << 16) | 1, 0, 0, 0, 0, 0, 0];
        m.cpu
            .write(
                info,
                &header
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let indices: Vec<_> = (0..1u16 << bpp).flat_map(u16::to_le_bytes).collect();
        m.cpu.write(info + 40, &indices).unwrap();
        let bitmap = call(
            &mut m,
            data,
            "gdi32.dll!CreateDIBSection",
            &[dc, info, 1, output, 0, 0],
        );
        assert_ne!(bitmap, 0);
        let pixels = m.cpu.read_u32(output).unwrap();
        let surface = m.ext.get::<SurfaceStore>().get(bitmap).unwrap().clone();
        assert_eq!(surface.format.bits(), bpp);
        assert_eq!(surface.pitch, ((9 * bpp + 31) / 32) * 4);
        let old = call(&mut m, data, "gdi32.dll!SelectObject", &[dc, bitmap]);
        assert_eq!(
            call(
                &mut m,
                data,
                "gdi32.dll!GetDIBColorTable",
                &[dc, 0, 1 << bpp, readback]
            ),
            1 << bpp
        );
        let mut second = [0; 4];
        m.cpu.read(readback + 4, &mut second).unwrap();
        assert_eq!(second, [0, 0, 128, 0]);
        // Change the selected DIB's real table, independently of its creation palette.
        m.cpu
            .write(readback, &[0, 0, 0, 0, 255, 255, 255, 0])
            .unwrap();
        assert_eq!(
            call(
                &mut m,
                data,
                "gdi32.dll!SetDIBColorTable",
                &[dc, 0, 2, readback]
            ),
            2
        );
        m.cpu
            .write(pixels, &vec![0xa5; surface.pitch as usize * 2])
            .unwrap();
        assert_ne!(
            call(
                &mut m,
                data,
                "gdi32.dll!IntersectClipRect",
                &[dc, 2, 0, 7, 1]
            ),
            0
        );
        assert_eq!(
            call(
                &mut m,
                data,
                "gdi32.dll!PatBlt",
                &[dc, 0, 0, 9, 2, 0xff0062]
            ),
            1
        );
        let row = surface
            .read_pixels(&memory(&mut m.cpu), 0, 0, 9)
            .unwrap()
            .into_owned();
        assert!(row[2..7].iter().all(|&i| u32::from(i) == (1 << bpp) - 1));
        let untouched = surface
            .read_pixels(&memory(&mut m.cpu), 0, 1, 9)
            .unwrap()
            .into_owned();
        assert_eq!(&row[..2], &untouched[..2]);
        assert_eq!(&row[7..], &untouched[7..]);
        call(&mut m, data, "gdi32.dll!SelectObject", &[dc, old]);
        // Read back packed storage, with a bounded RGBQUAD description.
        m.cpu.write(readback, &[0; 128]).unwrap();
        assert_eq!(
            call(
                &mut m,
                data,
                "gdi32.dll!GetDIBits",
                &[dc, bitmap, 0, 2, readback, info, 0]
            ),
            2
        );
        let mut actual = vec![0; surface.pitch as usize * 2];
        m.cpu.read(readback, &mut actual).unwrap();
        let mut original = vec![0; surface.pitch as usize * 2];
        m.cpu.read(pixels, &mut original).unwrap();
        if !(9 * bpp).is_multiple_of(8) {
            for y in 0..2 {
                original[y * surface.pitch as usize + ((9 * bpp + 7) / 8) as usize - 1] &=
                    255 << (8 - (9 * bpp) % 8);
            }
        }
        // Padding is not part of the returned pixel range.
        let bytes = ((9 * bpp + 7) / 8) as usize;
        for y in 0..2 {
            assert_eq!(
                &actual[y * surface.pitch as usize..y * surface.pitch as usize + bytes],
                &original[y * surface.pitch as usize..y * surface.pitch as usize + bytes]
            );
        }
        // Both outputs remain untouched when the packed row output crosses
        // a protected second page, even though its header is writable.
        m.cpu
            .write(
                info,
                &header
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let guard_output = m.sys.vm.alloc(&mut m.cpu, 8192, Prot::RW).unwrap();
        m.cpu.write(guard_output + 4092, &[0xa5; 24]).unwrap();
        m.cpu
            .protect(guard_output + 4096, 4096, Prot::READ)
            .unwrap();
        assert_eq!(
            call(
                &mut m,
                data,
                "gdi32.dll!GetDIBits",
                &[dc, bitmap, 0, 2, guard_output + 4092, info, 0]
            ),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut m)), 87);
        let mut actual_header = [0; 40];
        m.cpu.read(info, &mut actual_header).unwrap();
        assert_eq!(
            actual_header.as_slice(),
            header
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>()
        );
        let mut guard = [0; 24];
        m.cpu.read(guard_output + 4092, &mut guard).unwrap();
        assert_eq!(guard, [0xa5; 24]);
        assert_eq!(call(&mut m, data, "gdi32.dll!DeleteObject", &[bitmap]), 1);
    }
}

#[test]
#[cfg(windows)]
fn indexed_readback_matches_native_palette_mapping_queries_and_partial_bytes() {
    use windows_sys::Win32::Graphics::Gdi as gdi;
    for bpp in [1u32, 4, 8] {
        for custom in [false, true] {
            let (mut m, data, dc, _) = fixture(9, 2, PixelFormat::Bgra32, false);
            let info = data + 1024;
            let output = data + 2200;
            let readback = data + 2300;
            let mut initial = [0u32; 266];
            initial[..10].copy_from_slice(&[
                40,
                9,
                (-2i32) as u32,
                (bpp << 16) | 1,
                0,
                0,
                0,
                0,
                0,
                0,
            ]);
            let logical = super::super::super::super::super::palette::dc_entries(&env(&mut m), dc);
            for i in 0..1usize << bpp {
                let color = if custom {
                    let value = 255 - i as u8;
                    [value, value, value, 0]
                } else {
                    let color = logical[i % logical.len()];
                    [color[2], color[1], color[0], 0]
                };
                initial[10 + i] = u32::from_le_bytes(color);
            }
            let encoded: Vec<_> = initial.into_iter().flat_map(u32::to_le_bytes).collect();
            m.cpu.write(info, &encoded).unwrap();
            let bitmap = call(
                &mut m,
                data,
                "gdi32.dll!CreateDIBSection",
                &[dc, info, 0, output, 0, 0],
            );
            assert_ne!(bitmap, 0);
            let pitch = ((9 * bpp + 31) / 32) * 4;
            m.cpu
                .write(
                    m.cpu.read_u32(output).unwrap(),
                    &vec![0xa5; (pitch * 2) as usize],
                )
                .unwrap();
            // SAFETY: both native handles and all counted storage are owned here;
            // no desktop/window DC is obtained or modified.
            let native_dc = unsafe { gdi::CreateCompatibleDC(core::ptr::null_mut()) };
            let mut pointer = core::ptr::null_mut();
            let native = unsafe {
                gdi::CreateDIBSection(
                    native_dc,
                    initial.as_ptr().cast(),
                    0,
                    &mut pointer,
                    core::ptr::null_mut(),
                    0,
                )
            };
            assert!(!native.is_null() && !pointer.is_null());
            unsafe { core::ptr::write_bytes(pointer, 0xa5, (pitch * 2) as usize) };
            for usage in [0, 1] {
                for (attributes, height, start, count) in
                    [(true, -2, 0, 0), (false, -2, 0, 2), (false, 2, 1, 1)]
                {
                    let mut description = [0xccccccccu32; 266];
                    description[..10].copy_from_slice(&initial[..10]);
                    description[2] = height as u32;
                    if attributes {
                        description[3] = 1;
                    }
                    m.cpu
                        .write(
                            info,
                            &description
                                .into_iter()
                                .flat_map(u32::to_le_bytes)
                                .collect::<Vec<_>>(),
                        )
                        .unwrap();
                    m.cpu.write(readback, &[0xa5; 128]).unwrap();
                    let mut expected = [0xa5; 128];
                    let result = unsafe {
                        gdi::GetDIBits(
                            native_dc,
                            native,
                            start,
                            count,
                            if attributes {
                                core::ptr::null_mut()
                            } else {
                                expected.as_mut_ptr().cast()
                            },
                            description.as_mut_ptr().cast(),
                            usage,
                        )
                    };
                    assert_eq!(
                        call(
                            &mut m,
                            data,
                            "gdi32.dll!GetDIBits",
                            &[
                                dc,
                                bitmap,
                                start,
                                count,
                                if attributes { 0 } else { readback },
                                info,
                                usage
                            ],
                        ),
                        result as u32,
                    );
                    let mut actual = [0; 128];
                    m.cpu.read(readback, &mut actual).unwrap();
                    assert_eq!(
                        actual, expected,
                        "pixels bpp={bpp} custom={custom} usage={usage} attributes={attributes}"
                    );
                    let mut actual = vec![0; 1064];
                    m.cpu.read(info, &mut actual).unwrap();
                    let expected: Vec<_> =
                        description.into_iter().flat_map(u32::to_le_bytes).collect();
                    let differences: Vec<_> = actual
                        .iter()
                        .zip(&expected)
                        .enumerate()
                        .filter(|(_, (a, b))| a != b)
                        .collect();
                    assert!(
                        differences.is_empty(),
                        "description bpp={bpp} custom={custom} usage={usage} attributes={attributes}: {differences:?}"
                    );
                }
            }
            assert_eq!(unsafe { gdi::DeleteObject(native) }, 1);
            assert_eq!(unsafe { gdi::DeleteDC(native_dc) }, 1);
            assert_eq!(call(&mut m, data, "gdi32.dll!DeleteObject", &[bitmap]), 1);
        }
    }
}
