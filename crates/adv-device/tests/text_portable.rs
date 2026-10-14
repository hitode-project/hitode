//! Real portable font geometry/raster/ownership checks. The authored fixtures
//! keep the test independent of fonts installed on a developer's host.
#![cfg(not(windows))]
use adv_device::{
    surface::{PixelFormat, Rect, Snapshot, SurfaceMemory, Surfaces},
    text::*,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{borrow::Cow, path::PathBuf, sync::Arc};

const SINGLE: &[u8] = include_bytes!("fixtures/font-resource.ttf");
const COLLECTION: &[u8] = include_bytes!("fixtures/font-resource.ttc");

#[test]
// spec: WIN32-GDI-10
fn selected_face_spacing_defaults_and_unicode_coverage_share_the_font() {
    let mut text = Text::with_font_directories(&[]);
    text.add_file_resource(SINGLE, true).unwrap();
    let request = font("ADV Resource Fixture");
    let coverage = text.unicode_ranges(&request).unwrap();
    assert_eq!(
        coverage.glyphs,
        coverage
            .ranges
            .iter()
            .map(|&(_, count)| u32::from(count))
            .sum::<u32>()
    );
    assert!(
        coverage
            .ranges
            .iter()
            .any(|&(first, count)| u32::from(first) <= 65
                && 65 < u32::from(first) + u32::from(count))
    );
    let index = text.resolved_character(&request, 65).unwrap().glyph_index;
    let indices = [index as u16, 0, 65535];
    let widths = text
        .glyph_widths(&request, u32::MAX, 3, Some(&indices))
        .unwrap();
    let abc = text
        .glyph_abc_widths(&request, u32::MAX, 3, Some(&indices))
        .unwrap();
    for (width, abc) in widths.iter().zip(&abc) {
        assert_eq!(*width, abc.left + abc.ink as i32 + abc.right);
    }
    assert_eq!(widths[1], widths[2]);
    assert_eq!(abc[1], abc[2]);
    assert!(text.glyph_widths(&request, 0, 0, None).unwrap().is_empty());
    assert!(text.glyph_abc_widths(&request, 0, 0, None).is_err());
    assert!(text.glyph_widths(&request, 0, 2, Some(&indices)).is_err());
}

#[test]
fn extended_enumeration_filters_declared_repertoires_and_keeps_real_signatures() {
    let mut declared = SINGLE.to_vec();
    // The authored fixture's OS/2 v3 table starts at 400. Declare two code
    // pages in this in-memory variant; discovery must use the metadata rather
    // than guessing repertoire from an English family name.
    let codepages = 1u32 | (1 << 17);
    declared[478..482].copy_from_slice(&codepages.to_be_bytes());
    let mut text = Text::with_font_directories(&[]);
    text.add_file_resource(&declared, true).unwrap();
    let entries = text
        .enumerate(None, FontEnumeration::FamiliesEx(Encoding::Automatic))
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].font.encoding, Encoding::Latin);
    assert_eq!(entries[1].font.encoding, Encoding::Japanese);
    for (encoding, script) in [
        (Encoding::Latin, "Western"),
        (Encoding::Japanese, "Japanese"),
    ] {
        let entries = text
            .enumerate(
                Some("ADV Resource Fixture"),
                FontEnumeration::FamiliesEx(encoding),
            )
            .unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.font.encoding, encoding);
        assert_eq!(entry.metrics.encoding, encoding);
        assert_eq!(entry.script, script);
        assert_eq!(entry.signature.unwrap().codepages[0], codepages);
        assert_eq!(entry.font.face, "ADV Resource Fixture");
        assert!(
            entry
                .design
                .as_ref()
                .is_some_and(|design| design.em_units == 1000)
        );
    }
    assert!(
        text.enumerate(
            None,
            FontEnumeration::FamiliesEx(Encoding::SimplifiedChinese)
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        text.enumerate(
            Some("missing authored font"),
            FontEnumeration::FamiliesEx(Encoding::Japanese)
        )
        .unwrap()
        .is_empty()
    );
    // Legacy discovery remains a single physical face, independent of its
    // repertoire declarations.
    assert_eq!(
        text.enumerate(None, FontEnumeration::Families)
            .unwrap()
            .len(),
        1
    );
}

fn font(face: &str) -> FontRequest {
    FontRequest {
        default: None,
        face: face.into(),
        height: -40,
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
        quality: Quality::Antialiased,
        pitch: Pitch::Default,
        family: Family::Default,
    }
}
fn glyph(character: Character, format: GlyphFormat) -> GlyphRequest {
    GlyphRequest {
        character,
        format,
        hinted: false,
        transform: Transform::IDENTITY,
    }
}
fn loaded() -> (Text, FontRequest) {
    let mut text = Text::with_font_directories(&[]);
    text.add_resource(SINGLE).unwrap();
    (text, font("ADV Resource Fixture"))
}

#[test]
// spec: WIN32-GDI-12
fn wide_metric_characters_follow_the_cmap_and_os2_first_character() {
    // The fixture maps U+0020 and U+0041..U+0042; OS/2 usFirstCharIndex
    // (offset 464) is 0x20. Native GDI takes break from that index, with
    // 0 and 1 shifted by two and values above 0xFF replaced by space.
    for (os2_first, characters) in [
        (0x20u16, [0x20, 0x42, 0x1f, 0x20]),
        (0x01, [0x20, 0x42, 0x02, 0x03]),
        (0x3000, [0x20, 0x42, 0x1f, 0x20]),
    ] {
        let mut bytes = SINGLE.to_vec();
        bytes[464..466].copy_from_slice(&os2_first.to_be_bytes());
        let mut text = Text::with_font_directories(&[]);
        text.add_resource(&bytes).unwrap();
        let request = font("ADV Resource Fixture");
        let (metrics, wide) = text.unicode_metrics(&request).unwrap();
        assert_eq!(wide, characters, "usFirstCharIndex {os2_first:#x}");
        assert_eq!(metrics.height, text.metrics(&request).unwrap().height);
    }
}

#[test]
fn outline_metrics_scale_the_design_tables_to_the_selected_em() {
    let (mut text, mut font) = loaded();
    // The fixture: 1000 units per em, typographic and hhea 800/-200, box
    // (50, 0)-(700, 700), smallest size 3 ppem, an upright caret.
    let m = text.outline_metrics(&font).unwrap();
    assert_eq!(m.em_units, 1000);
    assert_eq!((m.ascent, m.descent, m.line_gap), (32, -8, 0));
    assert_eq!((m.mac_ascent, m.mac_descent, m.mac_line_gap), (32, -8, 0));
    assert_eq!(
        (m.bounds.left, m.bounds.top, m.bounds.right, m.bounds.bottom),
        (2, 28, 28, 0)
    );
    assert_eq!((m.caret_rise, m.caret_run, m.italic_angle), (1, 0, 0));
    assert_eq!((m.minimum_ppem, m.selection, m.embedding), (3, 0, 0));
    assert_eq!(m.text.height, text.metrics(&font).unwrap().height);
    assert_eq!(
        m.names,
        [
            "ADV Resource Fixture",
            "ADV Resource Fixture Regular",
            "Regular",
            "ADV Resource Fixture-authored-20261004",
        ]
    );
    // Simulated styles show in the selection flags.
    font.italic = true;
    font.underline = true;
    font.weight = 700;
    assert_eq!(text.outline_metrics(&font).unwrap().selection, 0x23);
}

#[test]
fn authored_outline_gray_levels_mono_packing_and_transforms_are_real() {
    let (mut text, font) = loaded();
    let b = Character::Unicode(u32::from(b'B'));
    let metrics = text.glyph(&font, glyph(b, GlyphFormat::Metrics)).unwrap();
    assert_eq!(
        metrics.metrics,
        GlyphMetrics {
            width: 10,
            height: 28,
            origin_x: 4,
            origin_y: 28,
            advance_x: 20,
            advance_y: 0,
        }
    );
    assert_eq!(metrics.data, GlyphData::None);
    assert_eq!(metrics.required_bytes, 20);
    for (format, max) in [
        (GlyphFormat::Gray2, 4),
        (GlyphFormat::Gray4, 16),
        (GlyphFormat::Gray8, 64),
    ] {
        let request = glyph(b, format);
        let result = text.glyph(&font, request).unwrap();
        assert_eq!(result.metrics, metrics.metrics);
        let GlyphData::Bitmap(bytes) = &result.data else {
            panic!("missing raster");
        };
        assert_eq!(bytes.len(), 12 * 28, "DWORD-aligned byte rows");
        for row in bytes.chunks_exact(12) {
            assert!(row[..10].iter().all(|&n| n == max));
            assert_eq!(row[10..], [0, 0]);
        }
        assert!(Arc::ptr_eq(&result, &text.glyph(&font, request).unwrap()));
    }
    let mono = text
        .glyph(&font, glyph(b, GlyphFormat::Monochrome))
        .unwrap();
    let GlyphData::Bitmap(bytes) = &mono.data else {
        panic!("missing monochrome raster");
    };
    assert_eq!(bytes.len(), 4 * 28);
    assert!(bytes.chunks_exact(4).all(|row| row == [0xff, 0xc0, 0, 0]));
    for format in [GlyphFormat::QuadraticOutline, GlyphFormat::CubicOutline] {
        let result = text.glyph(&font, glyph(b, format)).unwrap();
        let GlyphData::Outline(contours) = &result.data else {
            panic!("missing outline");
        };
        assert_eq!(contours.len(), 1);
        assert!(contours[0].curves.len() >= 3);
        assert!(result.required_bytes >= 16 + 3 * 12);
    }
    let transformed = text
        .glyph(
            &font,
            GlyphRequest {
                transform: Transform {
                    xx: 2 * 65536,
                    ..Transform::IDENTITY
                },
                ..glyph(b, GlyphFormat::Gray8)
            },
        )
        .unwrap();
    assert_eq!(transformed.metrics.width, 20);
    assert_eq!(transformed.metrics.origin_x, 8);
    assert_eq!(transformed.metrics.advance_x, 40);
    let a = text
        .glyph(&font, glyph(Character::Unicode(65), GlyphFormat::Gray8))
        .unwrap();
    let GlyphData::Bitmap(bytes) = &a.data else {
        panic!("missing A raster");
    };
    assert!(
        bytes.iter().any(|&n| n > 0 && n < 64),
        "triangle must contain antialias coverage"
    );
}

#[test]
// spec: DEV-FONT-1
fn em_cell_sizing_counted_encoding_and_font_width_use_one_selected_font() {
    let (mut text, mut font) = loaded();
    let metrics = text.metrics(&font).unwrap();
    assert_eq!(
        (metrics.height, metrics.ascent, metrics.descent),
        (40, 32, 8)
    );
    let encoded = text.extent(&font, TextInput::Encoded(b"AB")).unwrap();
    let unicode = text.extent(&font, TextInput::Unicode(&[65, 66])).unwrap();
    assert_eq!(encoded, unicode);
    assert_eq!(
        encoded,
        TextExtent {
            width: 52,
            height: 40
        }
    );
    assert_eq!(
        text.extent(&font, TextInput::Encoded(&[])).unwrap(),
        TextExtent::default()
    );
    assert_eq!(
        text.extent(&font, TextInput::Unicode(&[])).unwrap(),
        TextExtent::default()
    );
    assert_eq!(
        text.extent(&font, TextInput::Unicode(&[65, 0, 66]))
            .unwrap(),
        TextExtent {
            width: 76,
            height: 40
        }
    );
    assert_eq!(
        text.extent(&font, TextInput::Encoded(&[65, 0, 66]))
            .unwrap(),
        TextExtent {
            width: 76,
            height: 40
        }
    );
    let nul = text
        .glyph(&font, glyph(Character::Unicode(0), GlyphFormat::Gray8))
        .unwrap();
    let index = text
        .glyph(&font, glyph(Character::GlyphIndex(0), GlyphFormat::Gray8))
        .unwrap();
    assert_eq!(nul.metrics, index.metrics);
    assert_eq!(nul.data, index.data);
    let a = text
        .glyph(&font, glyph(Character::Encoded(65), GlyphFormat::Gray8))
        .unwrap();
    for code in [0x01000041, 0x00123441, 0xffff3441] {
        let high_bits = text
            .glyph(&font, glyph(Character::Encoded(code), GlyphFormat::Gray8))
            .unwrap();
        assert_eq!(high_bits.metrics, a.metrics);
        assert_eq!(
            high_bits.data, a.data,
            "ANSI SBCS glyph selects only the low byte"
        );
    }
    let mut fixed_font = font.clone();
    fixed_font.pitch = Pitch::Fixed;
    assert_eq!(
        text.glyph(
            &fixed_font,
            glyph(Character::Unicode(0), GlyphFormat::Gray8)
        )
        .unwrap()
        .metrics
        .advance_x,
        text.glyph(
            &fixed_font,
            glyph(Character::GlyphIndex(0), GlyphFormat::Gray8)
        )
        .unwrap()
        .metrics
        .advance_x
    );
    font.height = 20;
    assert_eq!(
        text.metrics(&font).unwrap().height,
        20,
        "positive height is cell height"
    );
    assert_eq!(
        text.extent(&font, TextInput::Encoded(b"AB")).unwrap().width,
        26
    );
    font.height = -20;
    assert_eq!(
        text.metrics(&font).unwrap().height,
        20,
        "fixture em equals its cell"
    );
    let space = text
        .glyph(&font, glyph(Character::Unicode(32), GlyphFormat::Gray8))
        .unwrap();
    assert_eq!(space.metrics.advance_x, 6);
    assert_eq!(space.required_bytes, 0);
    assert_eq!(space.data, GlyphData::Bitmap(Vec::new()));
    let space_metrics = text
        .glyph(&font, glyph(Character::Unicode(32), GlyphFormat::Metrics))
        .unwrap();
    assert_eq!(space_metrics.required_bytes, 20);
    assert_eq!(space_metrics.data, GlyphData::None);
    assert!(text.extent(&font, TextInput::Unicode(&[0xd800])).is_err());
    font.encoding = Encoding::Japanese;
    assert!(
        text.extent(&font, TextInput::Encoded(&[0x82])).is_err(),
        "incomplete DBCS is an error"
    );
}

#[test]
fn non_symmetric_matrix_and_rotated_cell_advances_follow_the_glyph_contract() {
    let (mut text, font) = loaded();
    let query = glyph(Character::Unicode(66), GlyphFormat::Gray8);
    for (transform, expected) in [
        (
            Transform {
                xy: 65536,
                ..Transform::IDENTITY
            },
            GlyphMetrics {
                width: 10,
                height: 38,
                origin_x: 4,
                origin_y: 42,
                advance_x: 20,
                advance_y: -20,
            },
        ),
        (
            Transform {
                yx: 65536,
                ..Transform::IDENTITY
            },
            GlyphMetrics {
                width: 38,
                height: 28,
                origin_x: 4,
                origin_y: 28,
                advance_x: 20,
                advance_y: 0,
            },
        ),
        (
            Transform {
                xx: 0,
                xy: 65536,
                yx: -65536,
                yy: 0,
            },
            GlyphMetrics {
                width: 28,
                height: 10,
                origin_x: -28,
                origin_y: 14,
                advance_x: 0,
                advance_y: -20,
            },
        ),
    ] {
        let actual = text
            .glyph(&font, GlyphRequest { transform, ..query })
            .unwrap();
        assert_eq!(actual.metrics, expected);
        assert!(matches!(&actual.data, GlyphData::Bitmap(bytes) if bytes.iter().any(|&n| n != 0)));
    }
}

#[test]
fn coverage_budget_includes_unpacked_monochrome_raster_storage() {
    let (mut text, mut font) = loaded();
    font.height = -16384;
    // Packed monochrome alone fits under 16 MiB, but its rasterizer's full
    // coverage buffer does not. Reject before allocating that intermediate.
    assert!(matches!(
        text.glyph(&font, glyph(Character::Unicode(66), GlyphFormat::Monochrome)),
        Err(TextError::Glyph(message)) if message.contains("allocation limit")
    ));
}

#[test]
fn missing_unicode_and_invalid_dbcs_glyphs_use_visible_default_outlines() {
    let (mut text, mut request) = loaded();
    let index = text
        .glyph(
            &request,
            glyph(Character::GlyphIndex(0), GlyphFormat::Gray8),
        )
        .unwrap();
    for code in [0x0378, 0xffff, u32::from(b'?')] {
        let missing = text
            .glyph(
                &request,
                glyph(Character::Unicode(code), GlyphFormat::Gray8),
            )
            .unwrap();
        assert_eq!(missing.metrics, index.metrics);
        assert_eq!(missing.data, index.data);
        assert!(
            missing.required_bytes > 0,
            "missing coverage is a visible default glyph"
        );
    }
    assert!(
        text.glyph(
            &request,
            glyph(Character::Unicode(0x110000), GlyphFormat::Gray8)
        )
        .is_err()
    );
    request.encoding = Encoding::SimplifiedChinese;
    for code in [0xb6ff, 0x00ffb6ff, 0x817f] {
        let invalid = text
            .glyph(
                &request,
                glyph(Character::Encoded(code), GlyphFormat::Gray8),
            )
            .unwrap();
        assert_eq!(invalid.metrics, index.metrics);
        assert_eq!(
            invalid.data, index.data,
            "invalid ANSI glyph resolves '?' then the fixture's default"
        );
    }
    assert!(
        text.extent(&request, TextInput::Encoded(&[0xb6, 0xff]))
            .is_err(),
        "counted runs keep strict decoding"
    );
    request.encoding = Encoding::Reserved(99);
    assert!(
        text.glyph(&request, glyph(Character::Encoded(65), GlyphFormat::Gray8))
            .is_err()
    );

    // This authored SFNT has OS/2 v3 at byte 400; usDefaultChar is its
    // documented byte 90 field. Change only that test resource's default to A.
    let mut explicit_default = SINGLE.to_vec();
    assert_eq!(&explicit_default[400..402], &[0, 3]);
    assert_eq!(&explicit_default[490..492], &[0, 0]);
    explicit_default[490..492].copy_from_slice(&65u16.to_be_bytes());
    let mut text = Text::with_font_directories(&[]);
    text.add_resource(&explicit_default).unwrap();
    let mut request = font("ADV Resource Fixture");
    request.pitch = Pitch::Fixed;
    let missing = text
        .glyph(
            &request,
            glyph(Character::Unicode(0x0378), GlyphFormat::Gray8),
        )
        .unwrap();
    let named_default = text
        .glyph(
            &request,
            glyph(Character::GlyphIndex(2), GlyphFormat::Gray8),
        )
        .unwrap();
    assert_eq!(
        missing.metrics, named_default.metrics,
        "OS/2 default metadata selects the real glyph advance"
    );
    assert_eq!(missing.data, named_default.data);
}

#[test]
fn hidden_resources_still_render_and_leave_other_font_owners_unchanged() {
    let mut text = Text::with_font_directories(&[]);
    let mut other = Text::with_font_directories(&[]);
    let request = font("ADV Collection Two");
    let (id, count) = text.add_file_resource(COLLECTION, false).unwrap();
    assert_eq!(count, 2);
    assert!(
        text.enumerate(None, FontEnumeration::Families)
            .unwrap()
            .is_empty()
    );
    assert!(
        text.enumerate(Some("ADV Collection Two"), FontEnumeration::Families)
            .unwrap()
            .is_empty()
    );
    let held = text
        .glyph(&request, glyph(Character::Unicode(66), GlyphFormat::Gray8))
        .unwrap();
    assert_eq!(held.metrics.advance_x, 36);
    assert!(other.metrics(&request).is_err());
    for bad in [&[][..], &[0x5a; 128][..]] {
        assert!(text.add_file_resource(bad, false).is_err());
    }
    assert!(Arc::ptr_eq(
        &held,
        &text
            .glyph(&request, glyph(Character::Unicode(66), GlyphFormat::Gray8))
            .unwrap()
    ));
    assert!(text.remove_resource(id).unwrap());
    assert_eq!(text.resource_stats(), (0, 0));
    assert!(text.metrics(&request).is_err());
    assert_eq!(held.metrics.advance_x, 36);
}

#[test]
fn resources_enumerate_match_invalidate_and_keep_immutable_glyphs_alive() {
    let mut text = Text::with_font_directories(&[]);
    let request = font("ADV Collection Two");
    assert!(text.metrics(&request).is_err());
    let (id, count) = text.add_file_resource(COLLECTION, true).unwrap();
    assert_eq!(count, 2);
    assert_eq!(text.resource_stats(), (1, COLLECTION.len()));
    let list = text.enumerate(None, FontEnumeration::Families).unwrap();
    assert_eq!(list.len(), 2);
    assert!(
        list.iter()
            .all(|f| f.kind == FontKind::TrueType && f.design.as_ref().unwrap().em_units == 1000)
    );
    let filtered = text
        .enumerate(Some("adv collection two"), FontEnumeration::Families)
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].full_name, "ADV Collection Two Regular");
    let query = glyph(Character::Unicode(66), GlyphFormat::Gray8);
    let held = text.glyph(&request, query).unwrap();
    assert_eq!(held.metrics.advance_x, 36);
    for bad in [&[][..], &[0x5a; 128][..]] {
        assert!(text.add_resource(bad).is_err());
    }
    assert_eq!(text.resource_stats(), (1, COLLECTION.len()));
    assert!(
        Arc::ptr_eq(&held, &text.glyph(&request, query).unwrap()),
        "failed registration preserves owners"
    );
    assert!(text.remove_resource(id).unwrap());
    assert_eq!(text.stats().fonts, 0);
    assert_eq!(text.stats().glyphs, 0);
    assert_eq!(text.resource_stats(), (0, 0));
    assert!(!text.remove_resource(id).unwrap());
    assert!(text.metrics(&request).is_err());
    assert_eq!(held.metrics.advance_x, 36);
    assert!(matches!(&held.data, GlyphData::Bitmap(bytes) if bytes.iter().any(|&n| n != 0)));
}

#[test]
fn explicit_directories_discover_fixture_faces_without_host_font_dependencies() {
    let directory = FixtureDirectory::new();
    let mut text = Text::with_font_directories(std::slice::from_ref(&directory.0));
    let discovered = text.enumerate(None, FontEnumeration::Families).unwrap();
    assert_eq!(discovered.len(), 3);
    let request = font("ADV Resource Fixture");
    let query = glyph(Character::Unicode(66), GlyphFormat::Gray8);
    let before = text.glyph(&request, query).unwrap();
    let (id, _) = text.add_resource(SINGLE).unwrap();
    let loaded = text.glyph(&request, query).unwrap();
    assert!(!Arc::ptr_eq(&before, &loaded));
    assert_eq!(before.data, loaded.data);
    text.remove_resource(id).unwrap();
    assert_eq!(text.glyph(&request, query).unwrap().data, before.data);
}

/// Tests may execute on another host than the compiler (notably Android).
/// Materialize the authored input on that host and retire only our own files.
struct FixtureDirectory(PathBuf);
impl FixtureDirectory {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("adv-fonts-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        let directory = Self(path);
        std::fs::write(directory.0.join("single.ttf"), SINGLE).unwrap();
        std::fs::write(directory.0.join("collection.ttc"), COLLECTION).unwrap();
        directory
    }
}
impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0.join("single.ttf"));
        let _ = std::fs::remove_file(self.0.join("collection.ttc"));
        let _ = std::fs::remove_file(self.0.join("profile.json"));
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn fixture_profile() -> serde_json::Value {
    json!({
        "version": 1,
        "faces": [{
            "id": "two", "path": "collection.ttc",
            "sha256": format!("{:x}", Sha256::digest(COLLECTION)),
            "index": 1, "family": "ADV Collection Two"
        }],
        "rules": [
            {"face": "ADV Resource Fixture", "faces": ["two"]},
            {"face": "Reference", "faces": ["two"]},
            {"default": "interface", "faces": ["two"]}
        ]
    })
}
fn write_profile(directory: &FixtureDirectory, value: &serde_json::Value) -> PathBuf {
    let path = directory.0.join("profile.json");
    std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    path
}

#[test]
fn strict_profile_pins_collection_faces_rules_sizes_and_guest_precedence() {
    let directory = FixtureDirectory::new();
    let path = write_profile(&directory, &fixture_profile());
    let mut text = Text::with_font_profile(&path).unwrap();
    let faces = text.enumerate(None, FontEnumeration::Families).unwrap();
    assert_eq!(
        faces.len(),
        1,
        "unlisted TTC faces and directory fonts stay isolated"
    );
    assert_eq!(faces[0].font.face, "ADV Collection Two");
    let mut request = font("Reference");
    request.height = 22;
    let resolved = text.resolved_character(&request, 66).unwrap();
    assert_eq!(resolved.family, "ADV Collection Two");
    assert_eq!(resolved.face_index, 1);
    assert_eq!(resolved.sha256, format!("{:x}", Sha256::digest(COLLECTION)));
    assert_eq!(resolved.font_bytes, COLLECTION.len());
    assert_eq!(resolved.em_pixels, 22.0);
    assert_eq!(resolved.cell_pixels, 22);
    assert_eq!(resolved.backend, "portable/skrifa+zeno");
    assert!(!resolved.default_glyph);
    assert_eq!(
        resolved.profile_sha256,
        Some(format!(
            "{:x}",
            Sha256::digest(std::fs::read(&path).unwrap())
        ))
    );
    assert!(
        text.metrics(&font("ADV Collection Two")).is_err(),
        "even loaded families require a declared request rule"
    );
    assert!(text.metrics(&font("missing requested family")).is_err());
    request.default = Some(DefaultFont::Interface);
    assert_eq!(text.resolved_character(&request, 66).unwrap(), resolved);
    request.default = Some(DefaultFont::Console);
    assert!(
        text.metrics(&request).is_err(),
        "default selectors are explicit"
    );

    let request = font("ADV Resource Fixture");
    let before = text.resolved_character(&request, 66).unwrap();
    assert_eq!(before.family, "ADV Collection Two");
    let (id, _) = text.add_resource(SINGLE).unwrap();
    let guest = text.resolved_character(&request, 66).unwrap();
    assert_eq!(guest.family, "ADV Resource Fixture");
    assert_eq!(guest.source, "guest resource");
    assert_eq!(guest.sha256, format!("{:x}", Sha256::digest(SINGLE)));
    assert_eq!(guest.face_index, 0);
    assert!(text.remove_resource(id).unwrap());
    assert_eq!(text.resolved_character(&request, 66).unwrap(), before);

    // Missing coverage keeps the GDI default-glyph contract, while the
    // reference API makes it possible to require coverage for a specific run.
    let missing = text.resolved_character(&request, 0x0378).unwrap();
    assert!(missing.default_glyph);
    assert_eq!(missing.family, "ADV Collection Two");
    assert_eq!(missing.glyph_index, 0);
}

#[test]
fn strict_profile_owns_verified_bytes_across_file_changes_and_cache_invalidation() {
    let directory = FixtureDirectory::new();
    let path = write_profile(&directory, &fixture_profile());
    let mut text = Text::with_font_profile(&path).unwrap();
    std::fs::write(directory.0.join("collection.ttc"), SINGLE).unwrap();
    let request = font("Reference");
    let query = glyph(Character::Unicode(66), GlyphFormat::Gray8);
    let before = text.glyph(&request, query).unwrap();
    assert_eq!(before.metrics.advance_x, 36);
    let (id, _) = text.add_resource(SINGLE).unwrap();
    text.remove_resource(id).unwrap();
    assert_eq!(text.glyph(&request, query).unwrap().data, before.data);
    let result = Text::with_font_profile(&path);
    assert!(
        matches!(result, Err(TextError::Font(message)) if message.contains("SHA-256 mismatch"))
    );
}

#[test]
fn strict_profile_rejects_missing_or_changed_fonts_and_invalid_replacement_rules() {
    let directory = FixtureDirectory::new();
    for (field, value, error) in [
        ("path", json!("missing.ttf"), "missing.ttf"),
        ("sha256", json!("0".repeat(64)), "SHA-256 mismatch"),
        ("index", json!(99), "collection index"),
        ("family", json!("Wrong family"), "family"),
    ] {
        let mut profile = fixture_profile();
        profile["faces"][0][field] = value;
        let path = write_profile(&directory, &profile);
        assert!(
            matches!(Text::with_font_profile(&path), Err(TextError::Font(message)) if message.contains(error)),
            "bad {field} must fail"
        );
    }
    for (value, error) in [
        (
            json!({"face":"Reference", "faces":["not-listed"]}),
            "not declared",
        ),
        (
            json!({"face":"Reference", "default":"interface", "faces":["two"]}),
            "exactly one",
        ),
        (json!({"face":"Reference", "faces":[]}), "ordered faces"),
        (
            json!({"face":"Reference", "faces":["two"], "typo":true}),
            "unknown",
        ),
    ] {
        let mut profile = fixture_profile();
        profile["rules"] = json!([value]);
        let path = write_profile(&directory, &profile);
        assert!(
            matches!(Text::with_font_profile(&path), Err(TextError::Font(message)) if message.contains(error))
        );
    }
}

#[test]
fn clipping_background_alignment_and_pixel_formats_preserve_storage() {
    let (mut text, request) = loaded();
    for format in [
        PixelFormat::Bgra32,
        PixelFormat::Bgr24,
        PixelFormat::Rgb565,
        PixelFormat::Rgb555,
    ] {
        let pitch = (64 * format.bytes() + 3) & !3;
        let mut bytes = vec![0; pitch as usize * 48 + 16];
        bytes[pitch as usize * 48..].fill(0xcd);
        let paint = Paint {
            input: TextInput::Unicode(&[65, 66]),
            origin: Point { x: 60, y: 36 },
            horizontal: Horizontal::Right,
            vertical: Vertical::Baseline,
            foreground: [255, 255, 255],
            background: [30, 80, 150],
            opaque_background: true,
            clip: Some(TextBox {
                left: 14,
                top: 8,
                right: 56,
                bottom: 36,
            }),
            fill: None,
        };
        text.paint(
            &request,
            paint,
            Pixels {
                bounds: Rect {
                    x: 0,
                    y: 0,
                    w: 64,
                    h: 48,
                },
                pitch,
                format,
                bytes: &mut bytes,
            },
        )
        .unwrap();
        let n = format.bytes() as usize;
        assert!(bytes[..pitch as usize * 48].iter().any(|&b| b != 0));
        for y in 0..48 {
            for x in 0..64 {
                let at = y * pitch as usize + x * n;
                if !(14..56).contains(&x) || !(8..36).contains(&y) {
                    assert!(
                        bytes[at..at + n].iter().all(|&b| b == 0),
                        "clip exterior changed at {x},{y}"
                    );
                }
            }
        }
        assert_eq!(&bytes[pitch as usize * 48..], &[0xcd; 16]);
    }
    let mut bytes = [0x22; 64 * 48 * 4];
    let before = bytes;
    let paint = Paint {
        input: TextInput::Unicode(&[0x0627]),
        origin: Point { x: 0, y: 0 },
        horizontal: Horizontal::Left,
        vertical: Vertical::Top,
        foreground: [255; 3],
        background: [0; 3],
        opaque_background: true,
        clip: None,
        fill: Some(TextBox {
            left: 0,
            top: 0,
            right: 64,
            bottom: 48,
        }),
    };
    assert!(
        text.paint(
            &request,
            paint,
            Pixels {
                bounds: Rect {
                    x: 0,
                    y: 0,
                    w: 64,
                    h: 48
                },
                pitch: 256,
                format: PixelFormat::Bgra32,
                bytes: &mut bytes
            }
        )
        .is_err()
    );
    assert_eq!(
        bytes, before,
        "unsupported run cannot partly paint its background"
    );
}

struct Memory(Vec<u8>);
impl SurfaceMemory for Memory {
    fn read(&self, at: u32, length: usize) -> Result<Cow<'_, [u8]>, String> {
        Ok(Cow::Borrowed(&self.0[at as usize..at as usize + length]))
    }
    fn write(&mut self, at: u32, data: &[u8]) -> Result<(), String> {
        self.0[at as usize..at as usize + data.len()].copy_from_slice(data);
        Ok(())
    }
    fn writable(&self, at: u32, length: usize) -> Result<(), String> {
        if at as usize + length <= self.0.len() {
            Ok(())
        } else {
            Err("out of memory".into())
        }
    }
    fn copy(&mut self, source: u32, destination: u32, length: usize) -> Result<(), String> {
        self.0.copy_within(
            source as usize..source as usize + length,
            destination as usize,
        );
        Ok(())
    }
}
#[test]
fn indexed_surface_text_preserves_coverage_and_reuses_scratch_and_glyphs() {
    let (mut text, request) = loaded();
    let mut memory = Memory(vec![0; 65536]);
    let mut store = Surfaces::new(64, 48, 16, 60000, Snapshot::default()).unwrap();
    let id = store
        .create(64, 48, true, PixelFormat::Indexed8, &mut memory)
        .unwrap();
    let palette: [[u8; 4]; 256] = std::array::from_fn(|i| [i as u8, i as u8, i as u8, 0]);
    store.set_palette(id, 0, &palette).unwrap();
    let paint = Paint {
        input: TextInput::Encoded(b"AB"),
        origin: Point { x: 0, y: 0 },
        horizontal: Horizontal::Left,
        vertical: Vertical::Top,
        foreground: [255; 3],
        background: [0; 3],
        opaque_background: false,
        clip: None,
        fill: None,
    };
    assert!(
        store
            .draw_text(id, &mut memory, &mut text, &request, paint)
            .unwrap()
    );
    let surface = store.get(id).unwrap();
    let mut intermediate = false;
    for y in 0..48 {
        for x in 0..64 {
            let coverage = memory.0[surface.row(x, y) as usize];
            intermediate |= coverage > 0 && coverage < 255;
        }
    }
    assert!(intermediate);
    let growths = store.stats.text_scratch_growths;
    let hits = text.stats().hits;
    store
        .draw_text(id, &mut memory, &mut text, &request, paint)
        .unwrap();
    assert_eq!(store.stats.text_scratch_growths, growths);
    assert!(text.stats().hits >= hits + 2);
}

#[test]
fn installed_japanese_fonts_produce_nonempty_unicode_and_dbcs_glyphs_when_available() {
    let mut text = Text::default();
    let mut request = font("MS Gothic");
    request.height = -24;
    request.encoding = Encoding::Japanese;
    if text.metrics(&request).is_err() {
        return;
    }
    let unicode: Vec<_> = "日本語".encode_utf16().collect();
    let Ok(extent) = text.extent(&request, TextInput::Unicode(&unicode)) else {
        eprintln!("host has no Japanese font coverage; deterministic fixture tests still apply");
        return;
    };
    let dbcs = [0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea];
    assert_eq!(
        text.extent(&request, TextInput::Encoded(&dbcs)).unwrap(),
        extent
    );
    for c in "日本語".chars() {
        let result = text
            .glyph(
                &request,
                glyph(Character::Unicode(u32::from(c)), GlyphFormat::Gray8),
            )
            .unwrap();
        assert!(result.metrics.advance_x > 0);
        assert!(matches!(&result.data, GlyphData::Bitmap(bytes) if bytes.iter().any(|&n| n != 0)));
    }
}

#[test]
fn installed_chinese_dbcs_glyphs_ignore_upper_word_when_available() {
    let mut text = Text::default();
    let mut request = font("黑体");
    request.height = 22;
    request.encoding = Encoding::SimplifiedChinese;
    if text.metrics(&request).is_err() {
        return;
    }
    let query = glyph(Character::Encoded(0xa1ad), GlyphFormat::Gray8);
    let Ok(low_word) = text.glyph(&request, query) else {
        eprintln!("host has no Chinese font coverage; deterministic fixture tests still apply");
        return;
    };
    let unicode = text
        .glyph(
            &request,
            GlyphRequest {
                character: Character::Unicode(0x2026),
                ..query
            },
        )
        .unwrap();
    assert_eq!(low_word.metrics, unicode.metrics);
    assert_eq!(
        low_word.data, unicode.data,
        "CP936 A1AD decodes to horizontal ellipsis"
    );
    for code in [0x00ada1ad, 0x0001a1ad, 0x7fffa1ad, 0xdeada1ad, 0xffffa1ad] {
        let high_word = text
            .glyph(
                &request,
                GlyphRequest {
                    character: Character::Encoded(code),
                    ..query
                },
            )
            .unwrap();
        assert_eq!(low_word.metrics, high_word.metrics);
        assert_eq!(low_word.data, high_word.data);
    }
    let question = text
        .glyph(
            &request,
            GlyphRequest {
                character: Character::Unicode(u32::from(b'?')),
                ..query
            },
        )
        .unwrap();
    for code in [0xb6ff, 0x00ffb6ff, 0x817f] {
        let invalid = text
            .glyph(
                &request,
                GlyphRequest {
                    character: Character::Encoded(code),
                    ..query
                },
            )
            .unwrap();
        assert_eq!(invalid.metrics, question.metrics);
        assert_eq!(
            invalid.data, question.data,
            "GDI substitutes '?' for invalid packed DBCS glyphs"
        );
    }
}

// spec: DEV-FONT-1
#[test]
fn explicit_host_substitution_updates_metrics_and_cached_glyphs() {
    let (mut text, request) = loaded();
    text.add_file_resource(COLLECTION, false).unwrap();
    let query = glyph(Character::Unicode(66), GlyphFormat::Gray8);
    let before = text.glyph(&request, query).unwrap();
    assert_eq!(before.metrics.advance_x, 20);
    text.set_overrides(Overrides {
        families: [(request.face.clone(), "ADV Collection Two".into())].into(),
        antialiased: Some(false),
    });
    assert_eq!(
        text.resolved_character(&request, 66).unwrap().family,
        "ADV Collection Two"
    );
    let after = text.glyph(&request, query).unwrap();
    assert_eq!(after.metrics.advance_x, 36);
    assert_eq!(
        text.extent(&request, TextInput::Unicode(&[66]))
            .unwrap()
            .width,
        36
    );
    assert_ne!(before.data, after.data);
    text.set_overrides(Overrides::default());
    assert_eq!(text.glyph(&request, query).unwrap().data, before.data);
}

// spec: DEV-FONT-1
#[test]
fn host_smoothing_changes_paint_without_changing_outline_requests() {
    let (mut text, mut request) = loaded();
    request.quality = Quality::Monochrome;
    let mut images = Vec::new();
    for smooth in [false, true] {
        text.set_overrides(Overrides {
            antialiased: Some(smooth),
            ..Overrides::default()
        });
        let mut bytes = vec![0; 64 * 48 * 4];
        text.paint(
            &request,
            Paint {
                input: TextInput::Unicode(&[65]),
                origin: Point { x: 4, y: 40 },
                horizontal: Horizontal::Left,
                vertical: Vertical::Baseline,
                foreground: [255; 3],
                background: [0; 3],
                opaque_background: false,
                clip: None,
                fill: None,
            },
            Pixels {
                bounds: Rect {
                    x: 0,
                    y: 0,
                    w: 64,
                    h: 48,
                },
                pitch: 256,
                format: PixelFormat::Bgra32,
                bytes: &mut bytes,
            },
        )
        .unwrap();
        let partial = bytes
            .chunks_exact(4)
            .any(|pixel| pixel[0] > 0 && pixel[0] < 255);
        assert_eq!(partial, smooth);
        images.push(
            text.glyph(
                &request,
                glyph(Character::Unicode(65), GlyphFormat::Monochrome),
            )
            .unwrap()
            .data
            .clone(),
        );
    }
    assert_eq!(images[0], images[1]);
}
