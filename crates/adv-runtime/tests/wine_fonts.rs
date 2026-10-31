//! Wine consumes the portable backend's discovered files and missing-family
//! replacements, with real TrueType metadata and pixels from authored TrueType
//! and CFF fonts.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use adv_runtime::wine::session::{Config, Host, SessionOptions, Stop};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

// spec: WINE-SES-5, WINE-SES-8, DEV-FONT-1, WINE-UNIX-6
#[test]
fn discovered_fonts_and_aliases_survive_prefix_reuse() {
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    let child = std::env::var_os("ADV_FONT_PROBE_ROOT");
    let root = child.as_ref().map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir().join(format!("adv-wine-fonts-{}", std::process::id()))
    });
    let game = root.join("game");
    let nested = root.join("fonts/nested");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::copy(probes.join("fonts.exe"), game.join("fonts.exe")).unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../adv-device/tests/fixtures");
    std::fs::copy(
        fixtures.join("font-resource-japanese.ttf"),
        nested.join("japanese.ttf"),
    )
    .unwrap();
    std::fs::copy(
        fixtures.join("font-resource-japanese-cff.otf"),
        nested.join("japanese-cff.otf"),
    )
    .unwrap();
    std::fs::copy(fixtures.join("font-resource.ttf"), game.join("private.ttf")).unwrap();
    let fonts = format!(
        r"\??\unix{}",
        root.join("fonts").to_string_lossy().replace('/', "\\")
    );
    if child.is_none() {
        for pass in 0..2 {
            if pass == 0 {
                std::fs::write(game.join("choices.txt"), "explicit").unwrap();
            } else {
                std::fs::remove_file(game.join("choices.txt")).unwrap();
            }
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "discovered_fonts_and_aliases_survive_prefix_reuse",
                    "--nocapture",
                ])
                .env("ADV_FONT_PROBE_ROOT", &root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::fs::remove_dir_all(root).unwrap();
        return;
    }
    let initial = game.join("choices.txt").exists();
    let mut host = Host::default();
    for (pass, explicit) in [initial, !initial, initial].into_iter().enumerate() {
        if explicit {
            std::fs::write(game.join("choices.txt"), "explicit").unwrap();
        } else if game.join("choices.txt").exists() {
            std::fs::remove_file(game.join("choices.txt")).unwrap();
        }
        let output = root.join(format!("stdout-{}-{pass}", std::process::id()));
        let stdout = std::fs::File::create(&output).unwrap();
        let config = Config {
            dist: &dist,
            prefix: &root.join("prefix"),
            drive: None,
            locale: 0x411,
            fonts: &fonts,
            exe: &game.join("fonts.exe"),
            width: 640,
            height: 480,
            jit: true,
            debug: "",
            stdout: Some(std::os::fd::AsRawFd::as_raw_fd(&stdout)),
        };
        let mut fonts = SessionOptions::default();
        if explicit {
            fonts.font_files.push(fixtures.join("font-resource.ttf"));
            fonts
                .fonts
                .families
                .insert("ADV Host Choice".into(), "ADV Resource Fixture".into());
            fonts
                .fonts
                .families
                .insert("ADV Japanese Fixture".into(), "ADV Resource Fixture".into());
            fonts.fonts.antialiased = Some(false);
        }
        let mut session = host
            .start(&config, adv_sync::WakeEvent::new().unwrap(), &fonts, |_| {
                Ok(())
            })
            .unwrap();

        let stop = session.run(Instant::now() + Duration::from_secs(30));
        drop(session);
        drop(stdout);
        let text = std::fs::read_to_string(output).unwrap();
        assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
        assert_eq!(
            std::fs::read(game.join("font-child-result")).unwrap(),
            [1],
            "child did not inherit font choices"
        );
        let expected = if explicit {
            vec![
                "choices=1",
                "override=1 child=1",
                "aliases=1",
                "cff=1",
                "pixels=1",
                "private=1",
                "ansi=1",
            ]
        } else {
            vec![
                "defaults=1",
                "override=1 child=1",
                "aliases=1",
                "cff=1",
                "pixels=1",
                "private=1",
                "ansi=1",
            ]
        };
        assert_eq!(text.lines().collect::<Vec<_>>(), expected);
    }
}

fn probe(name: &str, draw: bool) -> Option<(Vec<u8>, String, Duration, String)> {
    let wine::Wine { dist, probes } = wine::wine()?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/font-probes")
        .join(format!("{}-{name}-{}", std::process::id(), u8::from(draw)));
    if std::env::var_os("ADV_FONT_PROBE_ROOT").is_none() {
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("task"), format!("{name}\n{}", u8::from(draw))).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "font_probe_child", "--nocapture"])
            .env("ADV_FONT_PROBE_ROOT", &root)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let time = std::fs::read_to_string(root.join("elapsed"))
            .unwrap()
            .parse::<f64>()
            .unwrap();
        return Some((
            std::fs::read(root.join("record")).unwrap(),
            std::fs::read_to_string(root.join("stdout")).unwrap(),
            Duration::from_secs_f64(time),
            std::fs::read_to_string(root.join("stats")).unwrap(),
        ));
    }
    let root = PathBuf::from(std::env::var_os("ADV_FONT_PROBE_ROOT").unwrap());
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(
        probes.join(format!("{name}.exe")),
        game.join(format!("{name}.exe")),
    )
    .unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../adv-device/tests/fixtures");
    for (source, target) in [
        ("font-resource.ttf", "font-resource.ttf"),
        ("font-resource-japanese-cff.otf", "cff.otf"),
    ] {
        std::fs::copy(fixtures.join(source), game.join(target)).unwrap();
    }
    if draw {
        std::fs::write(game.join("draw-mode"), []).unwrap();
    }
    let font_path = format!(r"\??\unix{}", fixtures.to_string_lossy().replace('/', "\\"));
    let output = root.join("stdout");
    let stdout = std::fs::File::create(&output).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x411,
        fonts: &font_path,
        exe: &game.join(format!("{name}.exe")),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(std::os::fd::AsRawFd::as_raw_fd(&stdout)),
    };
    let mut options = SessionOptions::default();
    for (alias, target) in [
        ("MS Gothic", "ADV Japanese CFF Fixture"),
        ("Arial", "ADV Resource Fixture"),
        ("Consolas", "ADV Resource Fixture"),
    ] {
        options.fonts.families.insert(alias.into(), target.into());
    }
    let start = Instant::now();
    let mut host = Host::default();
    let mut session = host
        .start(
            &config,
            adv_sync::WakeEvent::new().unwrap(),
            &options,
            |machine| {
                if name == "font-timing" {
                    return Ok(());
                }
                machine.sys.stats.services_enabled = true;
                let handler = machine.ext.get::<adv_nt::native::FontFrontend>().0;
                machine.ext.insert(FontHandler {
                    run: handler,
                    poisoned: Default::default(),
                });
                machine
                    .ext
                    .insert(adv_nt::native::FontFrontend(guard_fonts));
                Ok(())
            },
        )
        .unwrap();
    let stop = session.run(Instant::now() + Duration::from_secs(40));
    let elapsed = start.elapsed();
    let mut stats = Vec::new();
    session
        .machine
        .sys
        .stats
        .write_host_csv(
            &session.machine.cpu,
            &session.machine.sys.thunks,
            &mut stats,
        )
        .unwrap();
    assert!(
        session
            .machine
            .ext
            .get::<adv_unix::loader::Libraries>()
            .0
            .iter()
            .all(|lib| !lib.path.to_string_lossy().contains("freetype"))
    );
    drop(session);
    drop(stdout);
    let stdout = std::fs::read_to_string(output).unwrap();
    assert!(
        matches!(stop, Stop::Exited(0)),
        "{name}: {stop:?}: {stdout}"
    );
    let record =
        std::fs::read(game.join(format!("{}.bin", name.replace('_', "-")))).unwrap_or_default();
    Some((record, stdout, elapsed, String::from_utf8(stats).unwrap()))
}

#[test]
fn font_probe_child() {
    let Some(root) = std::env::var_os("ADV_FONT_PROBE_ROOT").map(PathBuf::from) else {
        return;
    };
    let task = std::fs::read_to_string(root.join("task")).unwrap();
    let (name, draw) = task.split_once('\n').unwrap();
    let (record, _, elapsed, stats) = probe(name, draw == "1").unwrap();
    std::fs::write(root.join("record"), record).unwrap();
    std::fs::write(root.join("elapsed"), elapsed.as_secs_f64().to_string()).unwrap();
    std::fs::write(root.join("stats"), stats).unwrap();
}

// spec: WINE-UNIX-6, WIN32-GDI-10, WIN32-GDI-12, DEV-FONT-1
#[test]
fn font_stack_records_use_native_glyphs_and_composition() {
    let Some((bytes, _, _, stats)) = probe("font-stack", false) else {
        return;
    };
    assert!(bytes.len() > 300_000);
    assert!(stats.contains("gdi32.dll!ExtTextOutW"), "{stats}");
    use adv_device::text::{Character, GlyphFormat, GlyphRequest, TextInput, Transform};
    let mut direct = direct_text();
    let request = request("ADV Japanese CFF Fixture", -24);
    let mut at = 0;
    fn u32_at(bytes: &[u8], at: &mut usize) -> u32 {
        let value = u32::from_le_bytes(bytes[*at..*at + 4].try_into().unwrap());
        *at += 4;
        value
    }
    assert_eq!(u32_at(&bytes, &mut at) & 4, 4);
    assert_ne!(u32_at(&bytes, &mut at) & 4, 0);
    assert_eq!(u32_at(&bytes, &mut at), 128);
    assert_eq!(u32_at(&bytes, &mut at), 1);
    at += 60;
    assert_eq!(u32_at(&bytes, &mut at), 1);
    let _fit = u32_at(&bytes, &mut at);
    let width = u32_at(&bytes, &mut at);
    let height = u32_at(&bytes, &mut at);
    assert_eq!(height, direct.metrics(&request).unwrap().height as u32);
    let paragraph = [
        0x3042u16, 65, 32, 0x3042, 66, 32, 38, 67, 9, 0x3042, 68, 32, 0x3042, 69,
    ];
    let positions: Vec<_> = (0..paragraph.len())
        .map(|_| u32_at(&bytes, &mut at))
        .collect();
    assert_eq!(*positions.last().unwrap(), width);
    assert_eq!(
        width,
        direct
            .extent(&request, TextInput::Unicode(&paragraph))
            .unwrap()
            .width as u32
    );
    let mut previous = 0;
    for (&code, position) in paragraph.iter().zip(positions) {
        let w = u32_at(&bytes, &mut at);
        let a = u32_at(&bytes, &mut at) as i32;
        let b = u32_at(&bytes, &mut at);
        let c = u32_at(&bytes, &mut at) as i32;
        let advance = u32_at(&bytes, &mut at);
        assert_eq!(w, position - previous);
        assert_eq!(a + b as i32 + c, w as i32);
        assert_eq!(advance, w);
        let expected = direct
            .character_abc_widths(&request, &[Character::Unicode(u32::from(code))])
            .unwrap()[0];
        assert_eq!((a, b, c), (expected.left, expected.ink, expected.right));
        previous = position;
    }
    for format in [2, 3, 6] {
        assert_eq!(u32_at(&bytes, &mut at), format);
        let size = u32_at(&bytes, &mut at) as usize;
        let metrics = &bytes[at..at + 20];
        at += 20;
        let glyph = direct
            .glyph(
                &request,
                GlyphRequest {
                    character: Character::Unicode(0x3042),
                    format: match format {
                        2 => GlyphFormat::QuadraticOutline,
                        3 => GlyphFormat::CubicOutline,
                        _ => GlyphFormat::Gray8,
                    },
                    hinted: true,
                    transform: Transform::IDENTITY,
                },
            )
            .unwrap();
        assert_eq!(metrics, glyph_metrics(glyph.metrics));
        assert_eq!(size, glyph.required_bytes as usize);
        assert!(size > 0);
        let data = &bytes[at..at + size];
        assert_eq!(data, glyph_bytes(&glyph.data));
        if format == 2 || format == 3 {
            let mut contour = 0;
            while contour < data.len() {
                let length =
                    u32::from_le_bytes(data[contour..contour + 4].try_into().unwrap()) as usize;
                let mut curve = contour + 16;
                while curve < contour + length {
                    let kind = u16::from_le_bytes(data[curve..curve + 2].try_into().unwrap());
                    let points =
                        u16::from_le_bytes(data[curve + 2..curve + 4].try_into().unwrap()) as usize;
                    assert!(kind == 1 || kind == if format == 2 { 2 } else { 3 });
                    if format == 2 {
                        for p in data[curve + 4..curve + 4 + points * 8]
                            .as_chunks::<8>()
                            .0
                            .iter()
                        {
                            let x = i32::from_le_bytes(p[..4].try_into().unwrap()) as f64 / 65536.0;
                            let y = i32::from_le_bytes(p[4..].try_into().unwrap()) as f64 / 65536.0;
                            let radius = (x - 12.0).hypot(y - 9.12);
                            assert!((radius - 10.08).abs().min((radius - 5.28).abs()) <= 1.0);
                        }
                    }
                    curve += 4 + points * 8;
                }
                assert_eq!(curve, contour + length);
                contour += length;
            }
        } else {
            assert!(data.iter().any(|&b| b != 0));
        }
        at += size;
    }
    for test in 0..10 {
        assert_eq!(u32_at(&bytes, &mut at), 1);
        let cp = [u32_at(&bytes, &mut at), u32_at(&bytes, &mut at)];
        assert_eq!(
            cp,
            if test == 8 || test == 9 {
                [60, 14]
            } else {
                [0, 0]
            }
        );
        let bpp = u32_at(&bytes, &mut at);
        assert!([1, 4, 8, 16, 24, 32].contains(&bpp));
        let size = u32_at(&bytes, &mut at) as usize;
        assert!(bytes[at..at + size].iter().any(|&b| b != 0x23));
        if test != 6 {
            let expected = painted_reference(&mut direct, &request, test, bpp);
            assert_pixels(
                &bytes[at..at + size],
                &expected,
                &format!("DIB {test}/{bpp}"),
            );
        }
        at += size;
    }
    for _ in 0..5 {
        let _flags = u32_at(&bytes, &mut at);
        assert!(u32_at(&bytes, &mut at) > 0);
        at += 16 + 128 * 96 * 4;
    }
    assert_eq!(at, bytes.len());
}

// spec: WINE-UNIX-6, WIN32-GDI-14, WIN32-GDI-15
#[test]
fn restored_font_probe_records() {
    for name in [
        "gdi_text_query",
        "gdi_text_paint",
        "drawtext-lines",
        "font-resource-private",
    ] {
        let Some((bytes, _, _, _)) = probe(name, false) else {
            return;
        };
        assert!(!bytes.is_empty(), "{name}");
        match name {
            "gdi_text_query" => assert_pixels(&bytes, &query_reference(), "restored queries"),
            "gdi_text_paint" => old_paint_reference(&bytes),
            "drawtext-lines" => draw_lines_reference(&bytes),
            "font-resource-private" => private_resource_reference(&bytes),
            _ => unreachable!(),
        }
    }
}

// spec: WINE-UNIX-6, WINE-SES-5
#[test]
fn font_loading_and_drawing_timings() {
    let mut loading = Vec::new();
    let mut drawing = Vec::new();
    for _ in 0..5 {
        let Some((_, _, time, _)) = probe("font-timing", false) else {
            return;
        };
        loading.push(time.as_secs_f64());
        let (_, stdout, _, _) = probe("font-timing", true).unwrap();
        drawing.push(stdout.trim().parse::<f64>().unwrap());
    }
    loading.sort_by(f64::total_cmp);
    drawing.sort_by(f64::total_cmp);
    println!(
        "font loading median {:.9}s; drawing median {:.9}s",
        loading[2], drawing[2]
    );
}

// spec: WINE-UNIX-6, DEV-FONT-1
#[test]
fn directwrite_uses_the_host_cff_glyphs_without_freetype() {
    let Some((bytes, _, _, stats)) = probe("font-dwrite", false) else {
        return;
    };
    assert!(bytes.len() > 28);
    assert!(bytes[28..].iter().any(|&b| b != 0));
    assert!(stats.contains("dwrite.so!get_glyph_outline"), "{stats}");
    assert!(stats.contains("dwrite.so!get_glyph_bitmap"), "{stats}");
}

// spec: DEV-FONT-6
#[test]
fn wine_fonts_fill_missing_families_but_never_link_symbols() {
    if wine::dist().is_none() {
        return;
    }
    let mut text = direct_text();
    let mut latin = request("ADV Resource Fixture", -19);
    latin.encoding = adv_device::text::Encoding::Latin;
    // The fixture has no "W". Symbol sorts before Tahoma and maps it into
    // its private range, but only a selected Symbol draws it.
    let missing = text.resolved_character(&latin, 'W'.into()).unwrap();
    assert_eq!(
        (missing.family.as_str(), missing.default_glyph),
        ("Tahoma", false)
    );
    latin.face = "Symbol".into();
    let symbol = text.resolved_character(&latin, 'W'.into()).unwrap();
    assert_eq!(
        (symbol.family.as_str(), symbol.default_glyph),
        ("Symbol", false)
    );
}

fn request(face: &str, height: i32) -> adv_device::text::FontRequest {
    use adv_device::text::*;
    FontRequest {
        default: None,
        face: face.into(),
        height,
        width: 0,
        baseline_angle: 0,
        glyph_angle: 0,
        weight: 400,
        italic: false,
        underline: false,
        strikeout: false,
        encoding: Encoding::Japanese,
        precision: Precision::PreferTrueType,
        clipping: Clipping {
            precision: ClipPrecision::Default,
            transform_rotation: false,
            force_outline: false,
            disable_association: false,
            embedded: false,
        },
        quality: Quality::Antialiased,
        pitch: Pitch::Default,
        family: Family::Default,
    }
}
/// The session's font environment: the fixtures, and Wine's fonts for the
/// families they lack.
fn direct_text() -> adv_device::text::Text {
    adv_device::text::Text::with_font_directories(&[
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../adv-device/tests/fixtures")
    ])
    .with_windows_fonts(&wine::dist().unwrap().join("share/wine/fonts"))
}
fn glyph_metrics(metrics: adv_device::text::GlyphMetrics) -> Vec<u8> {
    [
        metrics.width,
        metrics.height,
        metrics.origin_x as u32,
        metrics.origin_y as u32,
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .chain(metrics.advance_x.to_le_bytes())
    .chain(metrics.advance_y.to_le_bytes())
    .collect()
}
fn glyph_bytes(data: &adv_device::text::GlyphData) -> Vec<u8> {
    use adv_device::text::*;
    match data {
        GlyphData::None => Vec::new(),
        GlyphData::Bitmap(b) => b.clone(),
        GlyphData::Outline(contours) => {
            let mut bytes = Vec::new();
            for contour in contours {
                let at = bytes.len();
                bytes.extend(
                    [0u32, 24, contour.start.x as u32, contour.start.y as u32]
                        .into_iter()
                        .flat_map(u32::to_le_bytes),
                );
                for curve in &contour.curves {
                    let (kind, points) = match curve {
                        Curve::Line(p) => (1u16, p),
                        Curve::Quadratic(p) => (2, p),
                        Curve::Cubic(p) => (3, p),
                    };
                    bytes.extend(kind.to_le_bytes());
                    bytes.extend((points.len() as u16).to_le_bytes());
                    for p in points {
                        bytes.extend(p.x.to_le_bytes());
                        bytes.extend(p.y.to_le_bytes());
                    }
                }
                let size = (bytes.len() - at) as u32;
                bytes[at..at + 4].copy_from_slice(&size.to_le_bytes());
            }
            bytes
        }
    }
}
fn painted_reference(
    text: &mut adv_device::text::Text,
    request: &adv_device::text::FontRequest,
    test: usize,
    bpp: u32,
) -> Vec<u8> {
    use adv_device::{
        surface::{PixelFormat, Rect},
        text::*,
    };
    let pitch = (128 * bpp).div_ceil(32) * 4;
    let mut original = vec![0x23; pitch as usize * 96];
    let mut request = request.clone();
    if test == 7 {
        request.face.insert(0, '@');
    }
    let paint = Paint {
        input: TextInput::Unicode(&[0x3042, 0x3042]),
        origin: if test == 8 {
            Point { x: 12, y: 14 }
        } else {
            Point { x: 24, y: 32 }
        },
        horizontal: Horizontal::Left,
        vertical: Vertical::Top,
        foreground: [220, 180, 140],
        background: [30, 50, 70],
        opaque_background: false,
        clip: (test == 9).then_some(TextBox {
            left: 5,
            top: 6,
            right: 100,
            bottom: 80,
        }),
        fill: (test == 9).then_some(TextBox {
            left: 5,
            top: 6,
            right: 100,
            bottom: 80,
        }),
    };
    let bounds = Rect {
        x: 0,
        y: 0,
        w: 128,
        h: 96,
    };
    if bpp == 1 {
        text.paint_monochrome(
            &request,
            paint,
            Pixels {
                bounds,
                pitch,
                format: PixelFormat::Indexed1,
                bytes: &mut original,
            },
            [[0, 0, 0, 0], [255, 255, 255, 0]],
        )
        .unwrap();
        return original;
    }
    if bpp == 4 || bpp == 8 {
        // spec: WIN32-GDI-16 Windows draws text on palette DIBs without antialiasing.
        request.quality = Quality::Monochrome;
        let max = (1u32 << bpp) - 1;
        let mut pixels = vec![0; 128 * 96 * 4];
        for (i, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let value = if bpp == 8 {
                0x23
            } else if i % 2 == 0 {
                2
            } else {
                3
            };
            let v = (value * 255 / max) as u8;
            pixel[..3].fill(v);
        }
        text.paint(
            &request,
            paint,
            Pixels {
                bounds,
                pitch: 512,
                format: PixelFormat::Bgra32,
                bytes: &mut pixels,
            },
        )
        .unwrap();
        for (i, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
            let nearest = (0..=max)
                .min_by_key(|&v| {
                    pixel[..3]
                        .iter()
                        .map(|&c| {
                            let delta = i32::from(c) - (v * 255 / max) as i32;
                            delta * delta
                        })
                        .sum::<i32>()
                })
                .unwrap() as u8;
            if bpp == 8 {
                original[i] = nearest
            } else if i % 2 == 0 {
                original[i / 2] = (original[i / 2] & 15) | (nearest << 4)
            } else {
                original[i / 2] = (original[i / 2] & 240) | nearest
            }
        }
    } else {
        let format = match bpp {
            16 => PixelFormat::Rgb555,
            24 => PixelFormat::Bgr24,
            _ => PixelFormat::Bgra32,
        };
        text.paint(
            &request,
            paint,
            Pixels {
                bounds,
                pitch,
                format,
                bytes: &mut original,
            },
        )
        .unwrap();
    }
    original
}

fn assert_pixels(actual: &[u8], expected: &[u8], label: &str) {
    assert_eq!(actual.len(), expected.len(), "{label}");
    if let Some(i) = actual.iter().zip(expected).position(|(a, b)| a != b) {
        panic!("{label}: byte {i}: {} != {}", actual[i], expected[i]);
    }
}

struct FontHandler {
    run: fn(&mut adv_machine::Env<'_>, &'static str) -> adv_machine::Outcome,
    poisoned: std::collections::BTreeSet<u32>,
}
fn guard_fonts(env: &mut adv_machine::Env<'_>, body: &'static str) -> adv_machine::Outcome {
    use adv_cpu::backend::GuestExt;
    let name = &env.sys.thunks.get(env.thunk).name;
    if name.contains("@font-dormant:") {
        return adv_machine::Outcome::Fail(format!("dormant font code entered: {name}"));
    }
    let images: Vec<_> = env
        .ext
        .get::<adv_nt::identity::Images>()
        .0
        .values()
        .filter(|m| matches!(m.name.as_str(), "win32u.so" | "dwrite.so"))
        .cloned()
        .collect();
    for module in images {
        if module.name == "win32u.so" {
            assert_eq!(
                env.cpu.read_u32(module.base + 0x1f1884).unwrap(),
                0,
                "Wine font_funcs was initialized"
            );
        }
        if !env
            .ext
            .get_mut::<FontHandler>()
            .poisoned
            .insert(module.base)
        {
            continue;
        }
        let table = module.table().unwrap();
        for function in &table.functions {
            let entry = module.base + function.rva;
            if env.sys.required_redirects.contains(&entry) {
                continue;
            }
            let forbidden = function.name.starts_with("freetype_")
                || function.name.starts_with("opentype_")
                || DORMANT.contains(&function.name.as_ref());
            if !forbidden {
                continue;
            }
            let alias = format!(
                "win32u.so!font_init@font-dormant:{}!{}",
                module.name, function.name
            );
            let target = env
                .sys
                .thunks
                .publish(env.cpu, adv_nt::native::MODULE, &alias)
                .unwrap();
            let mut bytes = vec![0; function.size as usize];
            env.cpu.read(entry, &mut bytes).unwrap();
            env.sys.redirects.insert(entry, (target, bytes.into()));
            env.sys.required_redirects.insert(entry);
        }
    }
    (env.ext.get::<FontHandler>().run)(env, body)
}
const DORMANT: &[&str] = &[
    "add_cached_font",
    "add_child_font",
    "add_family_replacement",
    "add_font_resource",
    "add_gdi_face",
    "add_system_font_resource",
    "alloc_gdi_font",
    "cache_gdi_font",
    "cache_glyph_bitmap",
    "create_face",
    "create_family",
    "create_gdi_font",
    "dump_gdi_font_list",
    "enum_fallbacks",
    "face_full_name_compare",
    "family_find_face_from_filename",
    "family_matches",
    "family_name_compare",
    "family_second_name_compare",
    "find_any_face",
    "find_best_matching_face",
    "find_cached_gdi_font",
    "find_face_from_filename",
    "find_face_from_full_name",
    "find_family_from_any_name",
    "find_family_from_font_links",
    "find_family_from_name",
    "find_matching_face",
    "find_matching_face_by_name",
    "free_font_handle",
    "free_gdi_font",
    "get_GSUB_vert_feature",
    "get_cached_glyph",
    "get_face_enum_data",
    "get_font_from_handle",
    "get_gdi_font_glyph_metrics",
    "get_glyph_index",
    "get_glyph_index_linked",
    "get_glyph_index_symbol",
    "get_glyph_outline",
    "insert_face_in_family_list",
    "load_directory_fonts",
    "load_face_from_cache",
    "load_font_list_from_cache",
    "load_system_links",
    "populate_system_links",
    "release_family",
    "release_gdi_font",
    "remove_font",
    "select_font",
    "set_default_family",
    "set_gdi_font_glyph_metrics",
    "unix_face_create",
    "unix_face_destroy",
    "update_external_font_keys",
];

fn query_reference() -> Vec<u8> {
    use adv_device::text::*;
    let mut text = direct_text();
    let codec = adv_os::Codec::new(932);
    let latin = b"WiAV\0xy";
    let dbcs = b"\x82\xa0\x82\xa2A\0x";
    let wide = [87, 105, 0, 120, 0x3042, 0x3044, 0xd83d, 0xde00];
    let mut words = Vec::<u32>::new();
    let mut extent =
        |words: &mut Vec<u32>, request: &FontRequest, units: Option<Vec<u16>>| match units
            .and_then(|u| text.extent(request, TextInput::Unicode(&u)).ok())
        {
            Some(size) => words.extend([1, size.width as u32, size.height as u32]),
            None => words.extend([0, 0xcccccccc, 0xdddddddd]),
        };
    for face in 0..3 {
        for charset in 0..3 {
            let mut request = request(
                if face == 2 {
                    "ADV Japanese CFF Fixture"
                } else {
                    "ADV Resource Fixture"
                },
                -24,
            );
            request.weight = if face == 1 { 700 } else { 400 };
            request.quality = Quality::Default;
            request.precision = Precision::Default;
            request.encoding = [
                Encoding::Latin,
                Encoding::Japanese,
                Encoding::SimplifiedChinese,
            ][charset];
            words.extend([1, 1]);
            for source in [latin.as_slice(), dbcs.as_slice()] {
                for n in 0..=7 {
                    // GDI decodes one byte as cp1252 in an ANSI font, and a
                    // lone lead byte in any font.
                    let cp = if n == 1 && (charset == 0 || codec.is_lead_byte(932, source[0])) {
                        1252
                    } else if charset == 2 {
                        936
                    } else {
                        932
                    };
                    extent(&mut words, &request, Some(codec.to_utf16(cp, &source[..n])));
                }
            }
            for n in 0..=8 {
                extent(&mut words, &request, Some(wide[..n].to_vec()));
            }
            extent(&mut words, &request, None);
            extent(&mut words, &request, None);
            extent(&mut words, &request, Some(vec![]));
            extent(&mut words, &request, Some(vec![]));
            words.extend([
                60,
                (-24i32) as u32,
                request.weight as u32,
                [0, 128, 134][charset],
                1,
                1,
            ]);
        }
    }
    let mut request = request("ADV Resource Fixture", -19);
    request.width = 7;
    request.weight = 500;
    request.underline = true;
    request.quality = Quality::Monochrome;
    request.pitch = Pitch::Fixed;
    request.precision = Precision::Default;
    request.encoding = Encoding::Latin;
    words.push(1);
    extent(&mut words, &request, Some(codec.to_utf16(932, latin)));
    extent(&mut words, &request, Some(wide.to_vec()));
    words.extend([
        1, 1, 0, 0x814120, 0xffffff, 0x492513, 2, 1, 0, 2, 1, 24, 1, 1, 1, 1, 1, 1,
    ]);
    words.into_iter().flat_map(u32::to_le_bytes).collect()
}
fn old_paint_reference(bytes: &[u8]) {
    use adv_device::{
        surface::{PixelFormat, Rect},
        text::*,
    };
    let mut text = direct_text();
    let codec = adv_os::Codec::new(932);
    let ansi = b"WiAV\0x\x82\xa0\x82";
    let wide = [87, 105, 0, 120, 0x3042, 0x3044, 0xd83d, 0xde00];
    let mut at = 0;
    while at < bytes.len() {
        let header: [u32; 10] = std::array::from_fn(|i| {
            u32::from_le_bytes(bytes[at + i * 4..at + i * 4 + 4].try_into().unwrap())
        });
        at += 40;
        let [format, up, style, align, bg, operation, count, _, _, size] = header;
        let mut request = request(
            if style == 1 {
                "ADV Japanese CFF Fixture"
            } else {
                "ADV Resource Fixture"
            },
            -19,
        );
        request.encoding = if style == 1 {
            Encoding::Japanese
        } else {
            Encoding::Latin
        };
        request.precision = Precision::Default;
        request.quality = if style == 0 {
            Quality::Subpixel
        } else {
            Quality::Monochrome
        };
        request.italic = style == 0;
        request.underline = style == 0;
        if style == 2 {
            request.default = Some(DefaultFont::System);
            request.face.clear();
            request.height = 0;
            request.weight = 0;
            request.quality = Quality::Default;
        }
        let units = if operation == 0 || operation == 2 {
            codec.to_utf16(
                if style == 1 || count != 1 { 932 } else { 1252 },
                &ansi[..count as usize],
            )
        } else {
            wide.to_vec()
        };
        let mut pixels: Vec<u8> = (0..size).map(|i| (i * 37 + 19) as u8).collect();
        let pitch = size as usize / 56;
        if up == 0 {
            pixels = pixels.chunks(pitch).rev().flatten().copied().collect();
        }
        let r = TextBox {
            left: 13,
            top: 9,
            right: 76,
            bottom: 47,
        };
        text.paint(
            &request,
            Paint {
                input: TextInput::Unicode(&units),
                origin: Point { x: 48, y: 28 },
                horizontal: match align & 6 {
                    2 => Horizontal::Right,
                    6 => Horizontal::Center,
                    _ => Horizontal::Left,
                },
                vertical: match align & 24 {
                    8 => Vertical::Bottom,
                    24 => Vertical::Baseline,
                    _ => Vertical::Top,
                },
                foreground: [193, 47, 91],
                background: [21, 117, 169],
                opaque_background: bg == 2,
                clip: (operation >= 2).then_some(r),
                fill: (operation == 2).then_some(r),
            },
            Pixels {
                bounds: Rect {
                    x: 0,
                    y: 0,
                    w: 96,
                    h: 56,
                },
                pitch: pitch as u32,
                format: match format {
                    16 => PixelFormat::Rgb555,
                    565 => PixelFormat::Rgb565,
                    24 => PixelFormat::Bgr24,
                    _ => PixelFormat::Bgra32,
                },
                bytes: &mut pixels,
            },
        )
        .unwrap();
        if up == 0 {
            pixels = pixels.chunks(pitch).rev().flatten().copied().collect();
        }
        assert_pixels(
            &bytes[at..at + size as usize],
            &pixels,
            &format!("restored paint {header:?}"),
        );
        at += size as usize;
    }
    assert_eq!(at, bytes.len());
}
fn draw_lines_reference(bytes: &[u8]) {
    use adv_device::{
        surface::{PixelFormat, Rect},
        text::*,
    };
    let mut text = direct_text();
    let mut request = request("ADV Resource Fixture", -16);
    request.encoding = Encoding::Automatic;
    request.quality = Quality::Monochrome;
    request.precision = Precision::Default;
    let metrics = text.metrics(&request).unwrap();
    let inputs = [
        "AB",
        "AB\r\nCD",
        "AB\nCD",
        "AB\rCD",
        "AB\r\nCD\r\n",
        "AB&&CD\r\nEF",
        "AB\r\nCD",
        "AB\r\nCD\r\n\r\n",
        "AB\r\n",
        "\r\n",
        "AB\r\nCD",
    ];
    let mut expected = Vec::<u32>::new();
    for _ in 0..2 {
        for (n, input) in inputs.iter().enumerate() {
            for clipped in [false, true] {
                let r = TextBox {
                    left: 3,
                    top: 4,
                    right: 90,
                    bottom: if clipped { 18 } else { 60 },
                };
                let line_height =
                    metrics.height + if n == 6 { metrics.external_leading } else { 0 };
                let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
                let lines: Vec<_> = normalized.split('\n').collect();
                let visible = if n == 10 {
                    lines.len()
                } else {
                    lines
                        .len()
                        .min(((r.bottom - r.top - 1) / line_height).max(0) as usize + 1)
                };
                let extra = usize::from(lines.len() > visible && lines.last() == Some(&""));
                let mut pixels = [0x77, 0x55, 0x33, 0].repeat(96 * 64);
                for (i, line) in lines.iter().take(visible).enumerate() {
                    text.paint(
                        &request,
                        Paint {
                            input: TextInput::Unicode(&line.encode_utf16().collect::<Vec<_>>()),
                            origin: Point {
                                x: 3,
                                y: 4 + i as i32 * line_height,
                            },
                            horizontal: Horizontal::Left,
                            vertical: Vertical::Top,
                            foreground: [240, 220, 200],
                            background: [255; 3],
                            opaque_background: false,
                            clip: (n != 10).then_some(r),
                            fill: None,
                        },
                        Pixels {
                            bounds: Rect {
                                x: 0,
                                y: 0,
                                w: 96,
                                h: 64,
                            },
                            pitch: 384,
                            format: PixelFormat::Bgra32,
                            bytes: &mut pixels,
                        },
                    )
                    .unwrap();
                }
                let mut hash = 2166136261u32;
                let mut changed = 0;
                for pixel in pixels.as_chunks::<4>().0 {
                    let rgb = u32::from_le_bytes(*pixel) & 0xffffff;
                    hash = (hash ^ rgb).wrapping_mul(16777619);
                    changed += u32::from(rgb != 0x335577);
                }
                expected.extend([
                    (visible + extra) as u32 * line_height as u32,
                    3,
                    4,
                    90,
                    r.bottom as u32,
                    hash,
                    changed,
                ]);
            }
        }
    }
    assert_pixels(
        bytes,
        &expected
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>(),
        "restored DrawText lines",
    );
}
fn private_resource_reference(bytes: &[u8]) {
    use adv_device::text::*;
    let mut text = direct_text();
    let request = request("ADV Resource Fixture", -40);
    let glyph = text
        .glyph(
            &request,
            GlyphRequest {
                character: Character::Unicode(66),
                format: GlyphFormat::Metrics,
                hinted: true,
                transform: Transform::IDENTITY,
            },
        )
        .unwrap();
    let advance = i32::from(glyph.metrics.advance_x) as u32;
    let enumerated = text
        .enumerate(
            Some("ADV Resource Fixture"),
            FontEnumeration::FamiliesEx(Encoding::Automatic),
        )
        .unwrap()
        .len() as u32;
    let expected = [
        1,
        1,
        enumerated,
        glyph.required_bytes,
        advance,
        0,
        1,
        glyph.required_bytes,
        advance,
        1,
        0,
        0,
        0xf0170030,
    ];
    assert_pixels(
        bytes,
        &expected
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>(),
        "private font lifetime",
    );
}
// spec: WINE-UNIX-6, WIN32-GDI-10, WIN32-GDI-11, WIN32-GDI-12
#[test]
fn restored_native_query_format_cases() {
    use adv_device::text::*;
    let Some((bytes, _, _, _)) = probe("font-query-formats", false) else {
        return;
    };
    let mut text = direct_text();
    let mut overrides = Overrides::default();
    for (aliases, targets) in text.replacement_fonts().unwrap().families {
        if let Some(target) = targets.first() {
            for alias in aliases {
                overrides.families.insert(alias, target.clone());
            }
        }
    }
    overrides
        .families
        .insert("Arial".into(), "ADV Resource Fixture".into());
    overrides
        .families
        .insert("MS Gothic".into(), "ADV Japanese CFF Fixture".into());
    text.set_overrides(overrides);
    let mut record = Records {
        bytes: &bytes,
        at: 0,
    };
    loop {
        match record.word() {
            0 => break,
            1 => {
                let flags = record.word();
                let matrix: [i32; 4] = std::array::from_fn(|_| record.word() as i32);
                let size = record.word() as usize;
                let actual_metrics = record.take(20);
                let actual = record.take(size);
                let mut request = request("Arial", -24);
                request.encoding = Encoding::Latin;
                request.precision = Precision::Default;
                request.quality = Quality::Default;
                let formats = [
                    GlyphFormat::Metrics,
                    GlyphFormat::Monochrome,
                    GlyphFormat::QuadraticOutline,
                    GlyphFormat::CubicOutline,
                    GlyphFormat::Gray2,
                    GlyphFormat::Gray4,
                    GlyphFormat::Gray8,
                ];
                let glyph = text
                    .glyph(
                        &request,
                        GlyphRequest {
                            character: Character::Unicode(103),
                            format: formats[(flags & 7) as usize],
                            hinted: flags & 0x100 == 0,
                            transform: Transform {
                                xx: matrix[0],
                                xy: matrix[1],
                                yx: matrix[2],
                                yy: matrix[3],
                            },
                        },
                    )
                    .unwrap();
                assert_eq!(size, glyph.required_bytes as usize);
                assert_eq!(actual_metrics, glyph_metrics(glyph.metrics));
                let expected = if flags & 7 == 0 {
                    vec![0; size]
                } else {
                    glyph_bytes(&glyph.data)
                };
                assert_pixels(actual, &expected, "restored outline formats");
            }
            2 => {
                let offset = record.word();
                let value = record.word() as u8;
                assert_eq!(record.word(), 1);
                let mut request = request("Arial", -24);
                request.encoding = Encoding::Latin;
                request.precision = Precision::Default;
                request.quality = Quality::Default;
                match offset {
                    20 => request.italic = value != 0,
                    21 => request.underline = value != 0,
                    22 => request.strikeout = value != 0,
                    23 => {
                        request.encoding = match value {
                            0 => Encoding::Latin,
                            1 => Encoding::Automatic,
                            128 => Encoding::Japanese,
                            n => Encoding::Reserved(n),
                        }
                    }
                    24 => {
                        request.precision = match value {
                            0 => Precision::Default,
                            4 => Precision::TrueType,
                            8 => Precision::Outline,
                            n => Precision::Reserved(n),
                        }
                    }
                    25 => {
                        request.clipping = Clipping {
                            precision: if value & 15 == 0 {
                                ClipPrecision::Default
                            } else {
                                ClipPrecision::Reserved(value & 15)
                            },
                            transform_rotation: value & 16 != 0,
                            force_outline: value & 32 != 0,
                            disable_association: value & 64 != 0,
                            embedded: value & 128 != 0,
                        }
                    }
                    26 => {
                        request.quality = match value {
                            0 => Quality::Default,
                            3 => Quality::Monochrome,
                            6 => Quality::SubpixelCompatible,
                            n => Quality::Reserved(n),
                        }
                    }
                    27 => {
                        request.pitch = match value & 15 {
                            0 => Pitch::Default,
                            1 => Pitch::Fixed,
                            n => Pitch::Reserved(n),
                        };
                        request.family = match value >> 4 {
                            0 => Family::Default,
                            1 => Family::Serif,
                            3 => Family::Monospace,
                            n => Family::Reserved(n),
                        };
                    }
                    _ => unreachable!(),
                }
                let metrics = text.metrics(&request).unwrap();
                assert_pixels(
                    record.take(56),
                    &text_metrics_bytes(&metrics),
                    &format!("restored LOGFONT {offset}/{value}"),
                );
            }
            3 => {
                let face = record.word() as usize;
                let height = record.word() as i32;
                let italic = record.word() != 0;
                let test = record.word() as usize;
                let indices: Vec<u16> = record
                    .take(10)
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|p| u16::from_le_bytes(*p))
                    .collect();
                let mut request = request(["Arial", "MS Gothic", "Terminal"][face], height);
                request.encoding = Encoding::Latin;
                request.precision = Precision::Default;
                request.quality = Quality::Default;
                request.italic = italic;
                let (first, count, array) = [
                    (0, 4, false),
                    (u32::MAX, 5, true),
                    (65535, 1, false),
                    (65537, 1, false),
                    (0, 0, false),
                ][test];
                let widths =
                    text.glyph_widths(&request, first, count, array.then_some(indices.as_slice()));
                let ok = record.word();
                let actual = record.take(20);
                let mut expected = vec![0xcd; 20];
                match widths {
                    Ok(values) => {
                        assert_eq!(ok, 1);
                        for (i, v) in values.iter().enumerate() {
                            expected[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                        }
                    }
                    Err(_) => assert_eq!(ok, 0),
                }
                assert_eq!(actual, expected);
                let abc = text.glyph_abc_widths(
                    &request,
                    first,
                    count,
                    array.then_some(indices.as_slice()),
                );
                let ok = record.word();
                let actual = record.take(60);
                let mut expected = vec![0xcd; 60];
                match abc {
                    Ok(values) => {
                        assert_eq!(ok, 1);
                        for (i, v) in values.iter().enumerate() {
                            let packed: Vec<_> = [v.left as u32, v.ink, v.right as u32]
                                .into_iter()
                                .flat_map(u32::to_le_bytes)
                                .collect();
                            expected[i * 12..i * 12 + 12].copy_from_slice(&packed);
                        }
                    }
                    Err(_) => assert_eq!(ok, 0),
                }
                assert_eq!(actual, expected);
            }
            4 => {
                let face = record.word() as usize;
                let wide = record.word() != 0;
                let test = record.word() as usize;
                let capacity = record.word();
                let input = [
                    "AV fi",
                    "AV fi",
                    "AV fi",
                    "abc",
                    "あいう",
                    "中文",
                    "אבג abc",
                    "سلام",
                ][test];
                let flags = [0, 8, 0x100000, 2, 0, 0, 2, 2 | 0x20][test];
                let maximum = if test == 2 { 30 } else { 100 };
                let mut request = request(["Arial", "MS Gothic", "Microsoft YaHei"][face], -24);
                request.encoding = [
                    Encoding::Latin,
                    Encoding::Japanese,
                    Encoding::SimplifiedChinese,
                ][face];
                request.precision = Precision::Default;
                request.quality = Quality::Default;
                let cp = if face == 2 { 936 } else { 932 };
                let codec = adv_os::Codec::new(cp);
                let encoded = codec.encode(input);
                let units: Vec<_> = input.encode_utf16().collect();
                let result = text
                    .placement(
                        &request,
                        if wide {
                            TextInput::Unicode(&units)
                        } else {
                            TextInput::Encoded(&encoded)
                        },
                        maximum,
                        flags,
                        Some(PlacementBuffers {
                            text: wide.then(|| vec![0xcdcd; 16]),
                            encoded_text: (!wide).then(|| vec![0xcd; 16]),
                            order: Some(vec![0xcdcdcdcd; 16]),
                            advances: Some(vec![0xcdcdcdcdu32 as i32; 16]),
                            caret: Some(vec![0xcdcdcdcdu32 as i32; 16]),
                            classes: Some(vec![0xcd; 16]),
                            glyphs: Some(vec![0xcdcd; 16]),
                            capacity,
                            max_fit: 0x12345678,
                        }),
                        932,
                    )
                    .unwrap();
                assert_eq!(record.word(), result.extent);
                let buffers = result.buffers.unwrap();
                assert_eq!(record.word(), buffers.capacity);
                assert_eq!(record.word(), buffers.max_fit as u32);
                let packed = if wide {
                    buffers
                        .text
                        .unwrap()
                        .into_iter()
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<_>>()
                } else {
                    buffers.encoded_text.unwrap()
                };
                assert_eq!(record.take(packed.len()), packed);
                for values in [
                    buffers.order.unwrap(),
                    buffers
                        .advances
                        .unwrap()
                        .into_iter()
                        .map(|n| n as u32)
                        .collect(),
                    buffers
                        .caret
                        .unwrap()
                        .into_iter()
                        .map(|n| n as u32)
                        .collect(),
                ] {
                    let packed: Vec<_> = values.into_iter().flat_map(u32::to_le_bytes).collect();
                    assert_eq!(record.take(64), packed);
                }
                assert_eq!(record.take(16), buffers.classes.unwrap());
                let packed: Vec<_> = buffers
                    .glyphs
                    .unwrap()
                    .into_iter()
                    .flat_map(u16::to_le_bytes)
                    .collect();
                assert_eq!(record.take(32), packed);
            }
            tag => panic!("unknown font query record {tag}"),
        }
    }
    assert_eq!(record.at, bytes.len());
}
struct Records<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Records<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        let at = self.at;
        self.at += n;
        &self.bytes[at..self.at]
    }
    fn word(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().unwrap())
    }
}
fn text_metrics_bytes(m: &adv_device::text::TextMetrics) -> Vec<u8> {
    use adv_device::text::*;
    let mut out: Vec<_> = [
        m.height,
        m.ascent,
        m.descent,
        m.internal_leading,
        m.external_leading,
        m.average_width,
        m.maximum_width,
        m.weight,
        m.overhang,
        m.aspect_x,
        m.aspect_y,
    ]
    .into_iter()
    .flat_map(i32::to_le_bytes)
    .collect();
    let family = match m.family {
        Family::Default => 0,
        Family::Serif => 1,
        Family::SansSerif => 2,
        Family::Monospace => 3,
        Family::Script => 4,
        Family::Decorative => 5,
        Family::Reserved(n) => n,
    };
    let charset = match m.encoding {
        Encoding::Latin => 0,
        Encoding::Automatic => 1,
        Encoding::Japanese => 128,
        Encoding::SimplifiedChinese => 134,
        Encoding::Oem => 255,
        Encoding::Reserved(n) => n,
        _ => 0,
    };
    out.extend([
        m.first_character,
        m.last_character,
        m.default_character,
        m.break_character,
        if m.italic { 255 } else { 0 },
        if m.underline { 255 } else { 0 },
        if m.strikeout { 255 } else { 0 },
        u8::from(m.variable_pitch)
            | (u8::from(m.vector) << 1)
            | (u8::from(m.outline) << 2)
            | (u8::from(m.device) << 3)
            | (family << 4),
        charset,
        0,
        0,
        0,
    ]);
    out
}
