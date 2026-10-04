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
