//! Device text requests are independent of Win32 personality and guest memory.
#![cfg(windows)]
use adv_device::text::*;
use std::sync::Arc;

fn font() -> FontRequest {
    FontRequest {
        face: "Arial".into(),
        height: -24,
        width: 0,
        baseline_angle: 0,
        glyph_angle: 0,
        weight: 400,
        italic: false,
        underline: false,
        strikeout: false,
        encoding: Encoding::Latin,
        precision: Precision::Default,
        clipping: Clipping {
            precision: ClipPrecision::Default,
            transform_rotation: false,
            force_outline: false,
            disable_association: false,
            embedded: false,
        },
        quality: Quality::Default,
        pitch: Pitch::Default,
        family: Family::Default,
    }
}
fn request(character: Character, format: GlyphFormat) -> GlyphRequest {
    GlyphRequest {
        character,
        format,
        hinted: true,
        transform: Transform::IDENTITY,
    }
}
#[test]
fn neutral_metrics_bitmaps_outlines_and_cache_have_owned_results() {
    let mut text = Text::default();
    let font = font();
    let metrics = text.metrics(&font).unwrap();
    assert_eq!(metrics.height, metrics.ascent + metrics.descent);
    assert!(metrics.height >= 24 && metrics.average_width > 0);
    for format in [
        GlyphFormat::Metrics,
        GlyphFormat::Monochrome,
        GlyphFormat::QuadraticOutline,
        GlyphFormat::CubicOutline,
        GlyphFormat::Gray2,
        GlyphFormat::Gray4,
        GlyphFormat::Gray8,
    ] {
        let request = request(Character::Unicode(u32::from(b'A')), format);
        let glyph = text.glyph(&font, request).unwrap();
        assert!(glyph.metrics.width > 0 && glyph.metrics.height > 0);
        assert!(glyph.required_bytes > 0);
        match (&glyph.data, format) {
            (GlyphData::None, GlyphFormat::Metrics) => {}
            (
                GlyphData::Outline(contours),
                GlyphFormat::QuadraticOutline | GlyphFormat::CubicOutline,
            ) => {
                assert!(!contours.is_empty());
                assert!(contours.iter().any(|c| !c.curves.is_empty()));
            }
            (GlyphData::Bitmap(bytes), _) => {
                assert_eq!(bytes.len() as u32, glyph.required_bytes);
                assert!(bytes.iter().any(|&byte| byte != 0));
                if format == GlyphFormat::Gray8 {
                    assert!(bytes.iter().all(|&byte| byte <= 64));
                }
            }
            other => panic!("unexpected glyph data: {other:?}"),
        }
        let cached = text.glyph(&font, request).unwrap();
        assert!(Arc::ptr_eq(&glyph, &cached));
        drop(text);
        assert_eq!(
            glyph.metrics, cached.metrics,
            "results outlive the rasterizer"
        );
        text = Text::default();
    }
}
#[test]
fn request_cache_separates_font_transform_format_character_and_hinting() {
    let mut text = Text::default();
    let font = font();
    let first = request(Character::Unicode(u32::from(b'A')), GlyphFormat::Gray8);
    let glyph = text.glyph(&font, first).unwrap();
    assert!(Arc::ptr_eq(&glyph, &text.glyph(&font, first).unwrap()));
    assert_eq!(text.stats().hits, 1);
    for request in [
        GlyphRequest {
            character: Character::Encoded(u32::from(b'A')),
            ..first
        },
        GlyphRequest {
            transform: Transform {
                xx: 131072,
                ..Transform::IDENTITY
            },
            ..first
        },
        GlyphRequest {
            format: GlyphFormat::Monochrome,
            ..first
        },
        GlyphRequest {
            hinted: false,
            ..first
        },
    ] {
        text.glyph(&font, request).unwrap();
    }
    let mut other_font = font.clone();
    other_font.height = -32;
    text.glyph(&other_font, first).unwrap();
    assert_eq!(text.stats().fonts, 2);
    assert_eq!(text.stats().glyphs, 6);
    assert_eq!(text.stats().hits, 1);
    assert!(text.stats().bytes > 0 && text.stats().bytes <= 8 * 1024 * 1024);
    let mut invalid = font;
    invalid.height = i32::MIN;
    assert!(matches!(
        text.glyph(&invalid, first),
        Err(TextError::Font(_))
    ));
    assert_eq!(text.stats().glyphs, 6, "a failed request is never cached");
}
