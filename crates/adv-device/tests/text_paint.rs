//! Native whole-run pixel oracle, independent of Win32 guest structures.
#![cfg(windows)]
use adv_device::{
    surface::{PixelFormat, Snapshot, SurfaceMemory, Surfaces},
    text::*,
};
use std::{
    borrow::Cow,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
struct Memory(Vec<u8>);
impl SurfaceMemory for Memory {
    fn read(&self, a: u32, n: usize) -> Result<Cow<'_, [u8]>, String> {
        Ok(Cow::Borrowed(&self.0[a as usize..a as usize + n]))
    }
    fn write(&mut self, a: u32, b: &[u8]) -> Result<(), String> {
        self.0[a as usize..a as usize + b.len()].copy_from_slice(b);
        Ok(())
    }
    fn writable(&self, a: u32, n: usize) -> Result<(), String> {
        if a as usize + n <= self.0.len() {
            Ok(())
        } else {
            Err("out of memory".into())
        }
    }
    fn copy(&mut self, s: u32, d: u32, n: usize) -> Result<(), String> {
        self.0.copy_within(s as usize..s as usize + n, d as usize);
        Ok(())
    }
}
fn font(style: u32) -> FontRequest {
    FontRequest {
        default: if style == 2 {
            Some(DefaultFont::System)
        } else {
            None
        },
        face: if style == 1 { "MS Gothic" } else { "Arial" }.into(),
        height: -19,
        width: 0,
        baseline_angle: 0,
        glyph_angle: 0,
        weight: 400,
        italic: style == 0,
        underline: style == 0,
        strikeout: false,
        encoding: if style == 1 {
            Encoding::Japanese
        } else {
            Encoding::Latin
        },
        precision: Precision::Default,
        clipping: Clipping {
            precision: ClipPrecision::Default,
            transform_rotation: false,
            force_outline: false,
            disable_association: false,
            embedded: false,
        },
        quality: if style == 0 {
            Quality::Subpixel
        } else {
            Quality::Monochrome
        },
        pitch: Pitch::Default,
        family: Family::Default,
    }
}
#[test]
fn full_run_pixels_match_native_for_formats_alignment_background_and_counted_strings() {
    use std::os::windows::process::CommandExt;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = root.join("target/gdi-text-paint-device");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-File"])
        .arg(root.join("tools/fixtures/build-gdi-text-paint.ps1"))
        .arg("-OutputDirectory")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let path = dir.join("gdi-text-paint.bin");
    if path.exists() {
        std::fs::remove_file(&path).unwrap();
    }
    let mut child = Command::new(dir.join("fixture_gdi_text_paint.exe"))
        .current_dir(&dir)
        .creation_flags(0x08000000)
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            assert_eq!(s.code(), Some(0));
            break;
        }
        if Instant::now() > end {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("owned paint oracle timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let file = std::fs::read(path).unwrap();
    let mut offset = 0;
    let mut cases = 0;
    let mut text = Text::default();
    let ansi = [b'W', b'i', b'A', b'V', 0, b'x', 0x82, 0xa0, 0x82];
    let wide = [
        b'W' as u16,
        b'i' as u16,
        0,
        b'x' as u16,
        0x3042,
        0x3044,
        0xd83d,
        0xde00,
    ];
    while offset < file.len() {
        let h: Vec<u32> = file[offset..offset + 40]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| u32::from_le_bytes(*p))
            .collect();
        offset += 40;
        let expected = &file[offset..offset + h[9] as usize];
        offset += h[9] as usize;
        let format = match h[0] {
            16 => PixelFormat::Rgb555,
            565 => PixelFormat::Rgb565,
            24 => PixelFormat::Bgr24,
            _ => PixelFormat::Bgra32,
        };
        let mut memory = Memory(vec![0; 65536]);
        let mut store = Surfaces::new(96, 56, 16, 60000, Snapshot::default()).unwrap();
        let id = store
            .create(96, 56, h[1] == 0, format, &mut memory)
            .unwrap();
        let s = store.get(id).unwrap().clone();
        for (i, p) in memory.0[s.pixels as usize..s.pixels as usize + h[9] as usize]
            .iter_mut()
            .enumerate()
        {
            *p = (i * 37 + 19) as u8;
        }
        let rect = TextBox {
            left: 13,
            top: 9,
            right: 76,
            bottom: 47,
        };
        let paint = Paint {
            input: if h[5] % 2 == 0 {
                TextInput::Encoded(&ansi[..h[6] as usize])
            } else {
                TextInput::Unicode(&wide)
            },
            origin: Point { x: 48, y: 28 },
            horizontal: match h[3] & 6 {
                2 => Horizontal::Right,
                6 => Horizontal::Center,
                _ => Horizontal::Left,
            },
            vertical: match h[3] & 24 {
                8 => Vertical::Bottom,
                24 => Vertical::Baseline,
                _ => Vertical::Top,
            },
            foreground: [193, 47, 91],
            background: [21, 117, 169],
            opaque_background: h[4] == 2,
            clip: if h[5] >= 2 { Some(rect) } else { None },
            fill: if h[5] == 2 { Some(rect) } else { None },
        };
        store
            .draw_text(id, &mut memory, &mut text, &font(h[2]), paint)
            .unwrap();
        let actual = &memory.0[s.pixels as usize..s.pixels as usize + h[9] as usize];
        if let Some(i) = actual.iter().zip(expected).position(|(a, b)| a != b) {
            panic!(
                "case {cases} {h:?} byte {i}: {} != {}",
                actual[i], expected[i]
            );
        }
        cases += 1;
    }
    assert_eq!(cases, 288);
    assert_eq!(text.stats().fonts, 4);
}

#[test]
fn malformed_public_pixel_storage_is_rejected_before_native_access() {
    let mut text = Text::default();
    let request = font(0);
    for (bounds, pitch) in [
        (
            adv_device::surface::Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 4,
            },
            1,
        ),
        (
            adv_device::surface::Rect {
                x: 0,
                y: 0,
                w: 4,
                h: u32::MAX,
            },
            16,
        ),
        (
            adv_device::surface::Rect {
                x: u32::MAX,
                y: 0,
                w: 4,
                h: 1,
            },
            16,
        ),
        (
            adv_device::surface::Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 1,
            },
            0,
        ),
    ] {
        let mut bytes = [0xcdu8; 16];
        let paint = Paint {
            input: TextInput::Encoded(b"AV"),
            origin: Point { x: 0, y: 0 },
            horizontal: Horizontal::Left,
            vertical: Vertical::Top,
            foreground: [0; 3],
            background: [255; 3],
            opaque_background: false,
            clip: None,
            fill: None,
        };
        assert!(
            text.paint(
                &request,
                paint,
                Pixels {
                    bounds,
                    pitch,
                    format: PixelFormat::Bgra32,
                    bytes: &mut bytes
                }
            )
            .is_err()
        );
        assert_eq!(bytes, [0xcd; 16]);
    }
    assert_eq!(text.stats().fonts, 0);
}

#[test]
fn consecutive_long_short_runs_reuse_the_bounded_patch_and_cached_font() {
    let mut memory = Memory(vec![0; 65536]);
    let mut store = Surfaces::new(128, 56, 16, 60000, Snapshot::default()).unwrap();
    let id = store
        .create(128, 56, false, PixelFormat::Bgr24, &mut memory)
        .unwrap();
    let mut text = Text::default();
    let request = font(3);
    for input in [
        &b"A long warm run"[..],
        &b"W"[..],
        &b""[..],
        &b"AV"[..],
        &b"short"[..],
    ] {
        let paint = Paint {
            input: TextInput::Encoded(input),
            origin: Point { x: 0, y: 2 },
            horizontal: Horizontal::Left,
            vertical: Vertical::Top,
            foreground: [193, 47, 91],
            background: [21, 117, 169],
            opaque_background: true,
            clip: None,
            fill: None,
        };
        store
            .draw_text(id, &mut memory, &mut text, &request, paint)
            .unwrap();
        assert_eq!(
            store.stats.text_scratch_growths, 1,
            "shorter patches must not allocate again"
        );
        assert_eq!(text.stats().fonts, 1);
    }
}

#[test]
fn indexed_text_preserves_gray_antialiasing_clip_and_unchanged_duplicate_indices() {
    let mut request = font(3);
    request.quality = Quality::Antialiased;
    let paint = Paint {
        input: TextInput::Encoded(b"AW"),
        origin: Point { x: 6, y: 4 },
        horizontal: Horizontal::Left,
        vertical: Vertical::Top,
        foreground: [255; 3],
        background: [0; 3],
        opaque_background: false,
        clip: Some(TextBox {
            left: 4,
            top: 2,
            right: 52,
            bottom: 30,
        }),
        fill: None,
    };
    for bottom_up in [false, true] {
        let mut memory = Memory(vec![0; 65536]);
        let mut store = Surfaces::new(64, 32, 16, 60000, Snapshot::default()).unwrap();
        let indexed = store
            .create(64, 32, bottom_up, PixelFormat::Indexed8, &mut memory)
            .unwrap();
        let bgra = store
            .create(64, 32, false, PixelFormat::Bgra32, &mut memory)
            .unwrap();
        let mut palette: [[u8; 4]; 256] = std::array::from_fn(|i| [i as u8, i as u8, i as u8, 0]);
        palette[50] = palette[49];
        store.set_palette(indexed, 0, &palette).unwrap();
        let indices = store.get(indexed).unwrap().clone();
        let direct = store.get(bgra).unwrap().clone();
        memory.0
            [indices.pixels as usize..(indices.pixels + indices.pitch * indices.height) as usize]
            .fill(50);
        for pixel in memory.0
            [direct.pixels as usize..(direct.pixels + direct.pitch * direct.height) as usize]
            .as_chunks_mut::<4>()
            .0
        {
            *pixel = [49, 49, 49, 0];
        }
        let mut text = Text::default();
        assert!(
            store
                .draw_text(bgra, &mut memory, &mut text, &request, paint)
                .unwrap()
        );
        assert!(
            store
                .draw_text(indexed, &mut memory, &mut text, &request, paint)
                .unwrap()
        );
        let mut antialiased = false;
        for y in 0..32 {
            for x in 0..64 {
                let index = memory.0[indices.row(x, y) as usize];
                let address = direct.row(x, y) as usize;
                let color: [u8; 4] = memory.0[address..address + 4].try_into().unwrap();
                if color[..3] == [49; 3] {
                    assert_eq!(index, 50, "unchanged duplicate index at {x},{y}");
                } else {
                    assert_eq!(
                        palette[index as usize][..3],
                        color[..3],
                        "coverage at {x},{y}"
                    );
                    antialiased |= color[0] > 49 && color[0] < 255;
                }
                if !(4..52).contains(&x) || !(2..30).contains(&y) {
                    assert_eq!(index, 50, "clip exterior at {x},{y}");
                }
            }
        }
        assert!(
            antialiased,
            "font rasterizer must produce intermediate coverage"
        );
        let growths = store.stats.text_scratch_growths;
        store
            .draw_text(indexed, &mut memory, &mut text, &request, paint)
            .unwrap();
        assert_eq!(
            store.stats.text_scratch_growths, growths,
            "repeated indexed output reuses scratch"
        );
    }
}

#[test]
fn packed_text_quantizes_into_actual_bits_and_keeps_clipped_edges_and_padding() {
    let request = font(3);
    let paint = Paint {
        input: TextInput::Encoded(b"AW"),
        origin: Point { x: 3, y: 2 },
        horizontal: Horizontal::Left,
        vertical: Vertical::Top,
        foreground: [255; 3],
        background: [0; 3],
        opaque_background: false,
        clip: Some(TextBox {
            left: 3,
            top: 2,
            right: 35,
            bottom: 23,
        }),
        fill: None,
    };
    for format in [PixelFormat::Indexed1, PixelFormat::Indexed4] {
        for bottom_up in [false, true] {
            let mut memory = Memory(vec![0; 65536]);
            let mut store = Surfaces::new(41, 25, 16, 60000, Snapshot::default()).unwrap();
            let id = store
                .create(41, 25, bottom_up, format, &mut memory)
                .unwrap();
            let palette: Vec<_> = (0..format.colors())
                .map(|i| {
                    let v = (i * 255 / (format.colors() - 1)) as u8;
                    [v, v, v, 0]
                })
                .collect();
            store.set_palette(id, 0, &palette).unwrap();
            let surface = store.get(id).unwrap().clone();
            for y in 0..25 {
                let at = surface.row(0, y) as usize;
                memory.0[at + format.row_bytes(41) as usize..at + surface.pitch as usize]
                    .fill(0xa5);
            }
            let mut text = Text::default();
            assert!(
                store
                    .draw_text(id, &mut memory, &mut text, &request, paint)
                    .unwrap()
            );
            let mut ink = 0;
            for y in 0..25 {
                let indices = surface.read_pixels(&memory, 0, y, 41).unwrap();
                for (x, &index) in indices.iter().enumerate() {
                    if !(3..35).contains(&x) || !(2..23).contains(&y) {
                        assert_eq!(index, 0);
                    }
                    ink += usize::from(index != 0);
                }
                let at = surface.row(0, y) as usize;
                assert!(
                    memory.0[at + format.row_bytes(41) as usize..at + surface.pitch as usize]
                        .iter()
                        .all(|&b| b == 0xa5)
                );
            }
            assert!(ink > 0);
        }
    }
}

#[test]
// spec: WIN32-GDI-7
fn monochrome_text_matches_native_duplicate_palettes_colours_quality_and_partial_clips() {
    use windows_sys::Win32::Graphics::Gdi as gdi;
    let input = [b'A' as u16, b'W' as u16, 0x3042];
    let clip = TextBox {
        left: 3,
        top: 2,
        right: 35,
        bottom: 23,
    };
    let mut text = Text::default();
    for style in [0, 1, 2] {
        let mut request = font(style);
        if style == 2 {
            request.default = None;
            request.face = "System".into();
            request.height = 136;
            request.encoding = Encoding::Automatic;
            request.quality = Quality::Default;
        }
        let mut logical = gdi::LOGFONTW {
            lfHeight: request.height,
            lfWeight: 400,
            lfItalic: u8::from(request.italic),
            lfUnderline: u8::from(request.underline),
            lfCharSet: match style {
                1 => 128,
                2 => 1,
                _ => 0,
            },
            lfQuality: match style {
                0 => 5,
                1 => 3,
                _ => 0,
            },
            ..Default::default()
        };
        for (target, unit) in logical
            .lfFaceName
            .iter_mut()
            .zip(request.face.encode_utf16())
        {
            *target = unit;
        }
        // SAFETY: fonts, DIBs, DCs and all buffers are owned by this test.
        // No desktop/window DC is obtained, drawn into or otherwise changed.
        let dc = unsafe { gdi::CreateCompatibleDC(core::ptr::null_mut()) };
        let native_font = unsafe { gdi::CreateFontIndirectW(&logical) };
        assert!(!dc.is_null() && !native_font.is_null());
        let old_font = unsafe { gdi::SelectObject(dc, native_font) };
        for palette in [
            [[0; 4], [0; 4]],
            [[255, 255, 255, 0], [255, 255, 255, 0]],
            [[0; 4], [255, 255, 255, 0]],
            [[255, 255, 255, 0], [0; 4]],
        ] {
            for foreground in [[0; 3], [255; 3], [255, 0, 0]] {
                for opaque in [false, true] {
                    for bottom_up in [false, true] {
                        let mut memory = Memory(vec![0; 65536]);
                        let mut store =
                            Surfaces::new(41, 25, 16, 60000, Snapshot::default()).unwrap();
                        let id = store
                            .create(41, 25, bottom_up, PixelFormat::Indexed1, &mut memory)
                            .unwrap();
                        store.set_palette(id, 0, &palette).unwrap();
                        let surface = store.get(id).unwrap().clone();
                        let length = (surface.pitch * surface.height) as usize;
                        memory.0[surface.pixels as usize..surface.pixels as usize + length]
                            .fill(0xa5);
                        let mut info = [0u32; 12];
                        info[..10].copy_from_slice(&[
                            40,
                            41,
                            if bottom_up { 25 } else { (-25i32) as u32 },
                            0x10001,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                        ]);
                        info[10] = u32::from_le_bytes(palette[0]);
                        info[11] = u32::from_le_bytes(palette[1]);
                        let mut pointer = core::ptr::null_mut();
                        let bitmap = unsafe {
                            gdi::CreateDIBSection(
                                dc,
                                info.as_ptr().cast(),
                                0,
                                &mut pointer,
                                core::ptr::null_mut(),
                                0,
                            )
                        };
                        assert!(!bitmap.is_null() && !pointer.is_null());
                        let previous = unsafe { gdi::SelectObject(dc, bitmap) };
                        let saved = unsafe { gdi::SaveDC(dc) };
                        unsafe {
                            core::ptr::write_bytes(pointer, 0xa5, length);
                            gdi::IntersectClipRect(
                                dc,
                                clip.left,
                                clip.top,
                                clip.right,
                                clip.bottom,
                            );
                            gdi::SetTextColor(
                                dc,
                                u32::from(foreground[0])
                                    | u32::from(foreground[1]) << 8
                                    | u32::from(foreground[2]) << 16,
                            );
                            gdi::SetBkColor(dc, 0xffffff);
                            gdi::SetBkMode(dc, if opaque { 2 } else { 1 });
                            assert_ne!(
                                gdi::TextOutW(dc, 3, 2, input.as_ptr(), input.len() as i32),
                                0
                            );
                            gdi::GdiFlush();
                        }
                        store
                            .draw_text(
                                id,
                                &mut memory,
                                &mut text,
                                &request,
                                Paint {
                                    input: TextInput::Unicode(&input),
                                    origin: Point { x: 3, y: 2 },
                                    horizontal: Horizontal::Left,
                                    vertical: Vertical::Top,
                                    foreground,
                                    background: [255; 3],
                                    opaque_background: opaque,
                                    clip: Some(clip),
                                    fill: None,
                                },
                            )
                            .unwrap();
                        let expected =
                            unsafe { core::slice::from_raw_parts(pointer.cast::<u8>(), length) };
                        let actual =
                            &memory.0[surface.pixels as usize..surface.pixels as usize + length];
                        assert_eq!(
                            actual, expected,
                            "style={style} palette={palette:?} foreground={foreground:?} opaque={opaque} bottom_up={bottom_up}"
                        );
                        unsafe {
                            assert_ne!(gdi::RestoreDC(dc, saved), 0);
                            gdi::SelectObject(dc, previous);
                            assert_ne!(gdi::DeleteObject(bitmap), 0);
                        }
                    }
                }
            }
        }
        unsafe {
            gdi::SelectObject(dc, old_font);
            assert_ne!(gdi::DeleteObject(native_font), 0);
            assert_ne!(gdi::DeleteDC(dc), 0);
        }
    }
}
