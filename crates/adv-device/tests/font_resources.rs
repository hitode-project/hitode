//! Entirely authored font data, loaded only into this test process's memory table.
#![cfg(windows)]
use adv_device::text::*;
use std::sync::{Arc, Mutex};

const SINGLE: &[u8] = include_bytes!("fixtures/font-resource.ttf");
const COLLECTION: &[u8] = include_bytes!("fixtures/font-resource.ttc");
static RESOURCES: Mutex<()> = Mutex::new(());

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
        encoding: Encoding::Automatic,
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
fn glyph() -> GlyphRequest {
    GlyphRequest {
        character: Character::Unicode(u32::from(b'B')),
        format: GlyphFormat::Gray8,
        hinted: true,
        transform: Transform::IDENTITY,
    }
}

#[test]
fn loaded_faces_change_real_matching_and_cache_then_retire_with_selected_fonts() {
    let _lock = RESOURCES.lock().unwrap();
    let mut text = Text::default();
    let request = font("ADV Resource Fixture");
    let before = text.glyph(&request, glyph()).unwrap();
    let (id, faces) = text.add_resource(SINGLE).unwrap();
    assert_eq!(faces, 1);
    assert_eq!(text.stats().fonts, 0);
    assert_eq!(text.stats().glyphs, 0);
    let loaded = text.glyph(&request, glyph()).unwrap();
    assert_eq!(
        loaded.metrics.advance_x, 20,
        "authored 500-unit B at 40 pixels"
    );
    assert_ne!(before.metrics, loaded.metrics);
    assert_ne!(before.data, loaded.data);
    assert!(loaded.required_bytes > 0);
    assert!(Arc::ptr_eq(
        &loaded,
        &text.glyph(&request, glyph()).unwrap()
    ));
    // The glyph query has selected a native HFONT in its cached measurement DC.
    assert!(text.remove_resource(id).unwrap());
    assert_eq!(text.resource_stats(), (0, 0));
    assert_eq!(text.stats().fonts, 0);
    assert_eq!(text.stats().glyphs, 0);
    let restored = text.glyph(&request, glyph()).unwrap();
    assert_eq!(restored.metrics, before.metrics);
    assert_eq!(restored.data, before.data);
    assert_eq!(
        loaded.metrics.advance_x, 20,
        "retained immutable result is still valid"
    );
    assert!(!text.remove_resource(id).unwrap());
    assert!(Arc::ptr_eq(
        &restored,
        &text.glyph(&request, glyph()).unwrap()
    ));
}

#[test]
fn collection_count_is_real_and_drop_releases_both_authored_faces() {
    let _lock = RESOURCES.lock().unwrap();
    let request = font("ADV Collection Two");
    let baseline = Text::default().glyph(&request, glyph()).unwrap();
    {
        let mut text = Text::default();
        let (_, faces) = text.add_resource(COLLECTION).unwrap();
        assert_eq!(faces, 2);
        let loaded = text.glyph(&request, glyph()).unwrap();
        assert_eq!(loaded.metrics.advance_x, 36, "authored 900-unit B");
        assert_ne!(baseline.metrics, loaded.metrics);
    }
    let after = Text::default().glyph(&request, glyph()).unwrap();
    assert_eq!(baseline.metrics, after.metrics);
    assert_eq!(baseline.data, after.data);
}

#[test]
fn corrupt_and_over_budget_resources_preserve_live_glyph_owners_and_registration() {
    let _lock = RESOURCES.lock().unwrap();
    let mut text = Text::default();
    let request = font("ADV Resource Fixture");
    let (id, _) = text.add_resource(SINGLE).unwrap();
    let loaded = text.glyph(&request, glyph()).unwrap();
    let state = text.resource_stats();
    let glyphs = text.stats().glyphs;
    for bad in [&[][..], &[0x5a; 128][..]] {
        assert!(text.add_resource(bad).is_err());
    }
    let over_remaining = vec![0; MAX_FONT_RESOURCE_BYTES];
    assert!(text.add_resource(&over_remaining).is_err());
    assert_eq!(text.resource_stats(), state);
    assert_eq!(text.stats().glyphs, glyphs);
    assert!(Arc::ptr_eq(
        &loaded,
        &text.glyph(&request, glyph()).unwrap()
    ));
    assert!(text.remove_resource(id).unwrap());
}

#[test]
fn registration_capacity_rejects_without_retiring_existing_resources() {
    let _lock = RESOURCES.lock().unwrap();
    let mut text = Text::default();
    let mut ids = Vec::new();
    for _ in 0..MAX_FONT_RESOURCES {
        ids.push(text.add_resource(SINGLE).unwrap().0);
    }
    assert_eq!(
        text.resource_stats(),
        (MAX_FONT_RESOURCES, MAX_FONT_RESOURCES * SINGLE.len())
    );
    assert!(text.add_resource(SINGLE).is_err());
    for id in ids {
        assert!(text.remove_resource(id).unwrap());
    }
    assert_eq!(text.resource_stats(), (0, 0));
}
