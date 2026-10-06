//! Snapshot installed fonts through a private native measurement DC. Callbacks
//! copy data synchronously and never retain provider-owned pointers or handles.
use super::*;

struct Collection {
    mode: FontEnumeration,
    fonts: Vec<EnumeratedFont>,
    exhausted: bool,
}
const MAX_ENUMERATED_FONTS: usize = 16384;
fn string(units: &[u16]) -> String {
    String::from_utf16_lossy(
        &units[..units
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(units.len())],
    )
}
fn request(font: &gdi::LOGFONTW) -> FontRequest {
    FontRequest {
        default: None,
        face: string(&font.lfFaceName),
        height: font.lfHeight,
        width: font.lfWidth,
        baseline_angle: font.lfEscapement,
        glyph_angle: font.lfOrientation,
        weight: font.lfWeight,
        italic: font.lfItalic != 0,
        underline: font.lfUnderline != 0,
        strikeout: font.lfStrikeOut != 0,
        encoding: decode_encoding(font.lfCharSet),
        precision: match font.lfOutPrecision {
            0 => Precision::Default,
            1 => Precision::String,
            2 => Precision::Character,
            3 => Precision::Stroke,
            4 => Precision::TrueType,
            5 => Precision::Device,
            6 => Precision::Raster,
            7 => Precision::PreferTrueType,
            8 => Precision::Outline,
            9 => Precision::ScreenOutline,
            10 => Precision::PostScript,
            value => Precision::Reserved(value),
        },
        clipping: Clipping {
            precision: match font.lfClipPrecision & 15 {
                0 => ClipPrecision::Default,
                1 => ClipPrecision::Character,
                2 => ClipPrecision::Stroke,
                value => ClipPrecision::Reserved(value),
            },
            transform_rotation: font.lfClipPrecision & 16 != 0,
            force_outline: font.lfClipPrecision & 32 != 0,
            disable_association: font.lfClipPrecision & 64 != 0,
            embedded: font.lfClipPrecision & 128 != 0,
        },
        quality: match font.lfQuality {
            0 => Quality::Default,
            1 => Quality::Draft,
            2 => Quality::Proof,
            3 => Quality::Monochrome,
            4 => Quality::Antialiased,
            5 => Quality::Subpixel,
            6 => Quality::SubpixelCompatible,
            value => Quality::Reserved(value),
        },
        pitch: match font.lfPitchAndFamily & 15 {
            0 => Pitch::Default,
            1 => Pitch::Fixed,
            2 => Pitch::Variable,
            value => Pitch::Reserved(value),
        },
        family: decode_family(font.lfPitchAndFamily >> 4),
    }
}
fn metrics(m: &gdi::TEXTMETRICW) -> TextMetrics {
    TextMetrics {
        height: m.tmHeight,
        ascent: m.tmAscent,
        descent: m.tmDescent,
        internal_leading: m.tmInternalLeading,
        external_leading: m.tmExternalLeading,
        average_width: m.tmAveCharWidth,
        maximum_width: m.tmMaxCharWidth,
        weight: m.tmWeight,
        overhang: m.tmOverhang,
        aspect_x: m.tmDigitizedAspectX,
        aspect_y: m.tmDigitizedAspectY,
        // Unicode character fields are copied separately, never narrowed here.
        first_character: 0,
        last_character: 0,
        default_character: 0,
        break_character: 0,
        italic: m.tmItalic != 0,
        underline: m.tmUnderlined != 0,
        strikeout: m.tmStruckOut != 0,
        variable_pitch: m.tmPitchAndFamily & 1 != 0,
        vector: m.tmPitchAndFamily & 2 != 0,
        outline: m.tmPitchAndFamily & 4 != 0,
        device: m.tmPitchAndFamily & 8 != 0,
        family: decode_family(m.tmPitchAndFamily >> 4),
        encoding: decode_encoding(m.tmCharSet),
    }
}
unsafe extern "system" fn collect(
    font: *const gdi::LOGFONTW,
    metric: *const gdi::TEXTMETRICW,
    kind: u32,
    param: isize,
) -> i32 {
    // SAFETY: EnumFont*W invokes synchronously with valid structures. param
    // points at enumerate's exclusively borrowed collection for the whole call.
    let (collection, logical, m) = unsafe { (&mut *(param as *mut Collection), &*font, &*metric) };
    if collection.fonts.len() >= MAX_ENUMERATED_FONTS {
        collection.exhausted = true;
        return 0;
    }
    let kind_value = if kind & gdi::TRUETYPE_FONTTYPE != 0 {
        FontKind::TrueType
    } else if kind & gdi::RASTER_FONTTYPE != 0 {
        FontKind::Raster
    } else {
        FontKind::Vector
    };
    let extended = matches!(collection.mode, FontEnumeration::FamiliesEx(_));
    let (mut full_name, mut style, design) =
        if collection.mode != FontEnumeration::Faces && kind_value == FontKind::TrueType {
            // SAFETY: EnumFontFamiliesW supplies ENUMLOGFONTW, and TrueType font
            // metrics have NEWTEXTMETRICW's additional design fields.
            let extended = unsafe { &*font.cast::<gdi::ENUMLOGFONTW>() };
            let design = if kind_value == FontKind::TrueType {
                let extended = unsafe { &*metric.cast::<gdi::NEWTEXTMETRICW>() };
                Some(FontDesign {
                    italic: extended.ntmFlags & 1 != 0,
                    bold: extended.ntmFlags & 32 != 0,
                    regular: extended.ntmFlags & 64 != 0,
                    nonnegative_spacing: extended.ntmFlags & 0x10000 != 0,
                    postscript_outline: extended.ntmFlags & 0x20000 != 0,
                    truetype_outline: extended.ntmFlags & 0x40000 != 0,
                    multiple_master: extended.ntmFlags & 0x80000 != 0,
                    type1: extended.ntmFlags & 0x100000 != 0,
                    digital_signature: extended.ntmFlags & 0x200000 != 0,
                    em_units: extended.ntmSizeEM,
                    cell_height: extended.ntmCellHeight,
                    average_width: extended.ntmAvgWidth,
                })
            } else {
                None
            };
            (
                string(&extended.elfFullName),
                string(&extended.elfStyle),
                design,
            )
        } else {
            (String::new(), String::new(), None)
        };
    let (script, signature) = if extended {
        // SAFETY: EnumFontFamiliesExW supplies ENUMLOGFONTEXW. Only TrueType
        // callbacks promise NEWTEXTMETRICEXW and its repertoire signature.
        let logical = unsafe { &*font.cast::<gdi::ENUMLOGFONTEXW>() };
        full_name = string(&logical.elfFullName);
        style = string(&logical.elfStyle);
        let signature = (kind_value == FontKind::TrueType).then(|| {
            let metric =
                unsafe { &*metric.cast::<windows_sys::Win32::Globalization::NEWTEXTMETRICEXW>() };
            FontSignature {
                unicode: metric.ntmFontSig.fsUsb,
                codepages: metric.ntmFontSig.fsCsb,
            }
        });
        (string(&logical.elfScript), signature)
    } else {
        (String::new(), None)
    };
    collection.fonts.push(EnumeratedFont {
        font: request(logical),
        metrics: metrics(m),
        characters: [m.tmFirstChar, m.tmLastChar, m.tmDefaultChar, m.tmBreakChar],
        kind: kind_value,
        device: kind & gdi::DEVICE_FONTTYPE != 0,
        full_name,
        style,
        script,
        design,
        signature,
    });
    1
}
pub(in crate::text) fn enumerate(
    face: Option<&str>,
    mode: FontEnumeration,
) -> Result<Vec<EnumeratedFont>, TextError> {
    if face.is_some_and(|face| face.contains('\0') || face.encode_utf16().count() > 32767) {
        return Err(TextError::Font("invalid enumeration face name".into()));
    }
    let face = face.map(|face| face.encode_utf16().chain(Some(0)).collect::<Vec<_>>());
    let pointer = face.as_ref().map_or(std::ptr::null(), |face| face.as_ptr());
    let mut collection = Collection {
        mode,
        fonts: Vec::new(),
        exhausted: false,
    };
    // SAFETY: private screen-compatible DC, valid optional terminated face,
    // callback only copies native data while the collection is exclusively owned.
    let dc = unsafe { gdi::CreateCompatibleDC(std::ptr::null_mut()) };
    if dc.is_null() {
        return Err(TextError::Font(
            "font enumeration DC creation failed".into(),
        ));
    }
    unsafe {
        match mode {
            FontEnumeration::Families => {
                gdi::EnumFontFamiliesW(dc, pointer, Some(collect), &raw mut collection as isize)
            }
            FontEnumeration::Faces => {
                gdi::EnumFontsW(dc, pointer, Some(collect), &raw mut collection as isize)
            }
            FontEnumeration::FamiliesEx(encoding) => {
                let mut query: gdi::LOGFONTW = std::mem::zeroed();
                query.lfCharSet = super::encoding(encoding);
                if let Some(face) = &face {
                    for (destination, source) in query.lfFaceName.iter_mut().take(31).zip(face) {
                        *destination = *source;
                    }
                }
                gdi::EnumFontFamiliesExW(dc, &query, Some(collect), &raw mut collection as isize, 0)
            }
        };
        gdi::DeleteDC(dc);
    }
    if collection.exhausted {
        return Err(TextError::Font("font enumeration budget exceeded".into()));
    }
    Ok(collection.fonts)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_families_and_face_filter_preserve_native_data() {
        let fonts = enumerate(None, FontEnumeration::Families).unwrap();
        assert!(!fonts.is_empty());
        let font = fonts
            .iter()
            .find(|font| font.kind == FontKind::TrueType)
            .unwrap();
        assert!(
            font.design
                .as_ref()
                .is_some_and(|design| design.em_units > 0)
        );
        let filtered = enumerate(Some(&font.font.face), FontEnumeration::Families).unwrap();
        assert!(!filtered.is_empty());
        assert!(
            filtered
                .iter()
                .all(|entry| entry.font.face.eq_ignore_ascii_case(&font.font.face))
        );
        assert!(
            enumerate(Some("__adv_absent_font_42d9a__"), FontEnumeration::Faces)
                .unwrap()
                .is_empty()
        );
    }
}
