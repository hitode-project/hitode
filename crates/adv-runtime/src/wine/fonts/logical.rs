//! LOGFONT fields reach the common font matcher without reinterpretation.
use super::*;
pub(super) fn from_font(font: &LogFont) -> FontRequest {
    let raw = &font.raw;
    let integer = |at| i32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
    FontRequest {
        default: None,
        face: font.face.clone(),
        height: integer(0),
        width: integer(4),
        baseline_angle: integer(8),
        glyph_angle: integer(12),
        weight: integer(16),
        italic: raw[20] != 0,
        underline: raw[21] != 0,
        strikeout: raw[22] != 0,
        encoding: decode_encoding(raw[23]),
        precision: precision(raw[24]),
        clipping: Clipping {
            precision: match raw[25] & 15 {
                0 => ClipPrecision::Default,
                1 => ClipPrecision::Character,
                2 => ClipPrecision::Stroke,
                value => ClipPrecision::Reserved(value),
            },
            transform_rotation: raw[25] & 16 != 0,
            force_outline: raw[25] & 32 != 0,
            disable_association: raw[25] & 64 != 0,
            embedded: raw[25] & 128 != 0,
        },
        quality: match raw[26] {
            0 => Quality::Default,
            1 => Quality::Draft,
            2 => Quality::Proof,
            3 => Quality::Monochrome,
            4 => Quality::Antialiased,
            5 => Quality::Subpixel,
            6 => Quality::SubpixelCompatible,
            value => Quality::Reserved(value),
        },
        pitch: match raw[27] & 15 {
            0 => Pitch::Default,
            1 => Pitch::Fixed,
            2 => Pitch::Variable,
            value => Pitch::Reserved(value),
        },
        family: family(raw[27] >> 4),
    }
}
