//! Native rasterization adapter. Every GDI handle is private and owned here;
//! request/result types carry neither Win32 handles nor packed ABI structures.
use super::*;
use windows_sys::Win32::Graphics::Gdi::{self as gdi, HDC, HFONT, HGDIOBJ};

pub(super) struct Font {
    dc: HDC,
    font: HFONT,
    old: HGDIOBJ,
}

fn encoding(value: Encoding) -> u8 {
    match value {
        Encoding::Latin => 0,
        Encoding::Automatic => 1,
        Encoding::Symbol => 2,
        Encoding::MacRoman => 77,
        Encoding::Japanese => 128,
        Encoding::Korean => 129,
        Encoding::Johab => 130,
        Encoding::SimplifiedChinese => 134,
        Encoding::TraditionalChinese => 136,
        Encoding::Greek => 161,
        Encoding::Turkish => 162,
        Encoding::Vietnamese => 163,
        Encoding::Hebrew => 177,
        Encoding::Arabic => 178,
        Encoding::Baltic => 186,
        Encoding::Cyrillic => 204,
        Encoding::Thai => 222,
        Encoding::EasternEuropean => 238,
        Encoding::Oem => 255,
        Encoding::Reserved(value) => value,
    }
}
fn precision(value: Precision) -> u8 {
    match value {
        Precision::Default => 0,
        Precision::String => 1,
        Precision::Character => 2,
        Precision::Stroke => 3,
        Precision::TrueType => 4,
        Precision::Device => 5,
        Precision::Raster => 6,
        Precision::PreferTrueType => 7,
        Precision::Outline => 8,
        Precision::ScreenOutline => 9,
        Precision::PostScript => 10,
        Precision::Reserved(value) => value,
    }
}
fn quality(value: Quality) -> u8 {
    match value {
        Quality::Default => 0,
        Quality::Draft => 1,
        Quality::Proof => 2,
        Quality::Monochrome => 3,
        Quality::Antialiased => 4,
        Quality::Subpixel => 5,
        Quality::SubpixelCompatible => 6,
        Quality::Reserved(value) => value,
    }
}
fn pitch(value: Pitch) -> u8 {
    match value {
        Pitch::Default => 0,
        Pitch::Fixed => 1,
        Pitch::Variable => 2,
        Pitch::Reserved(value) => value,
    }
}
fn clip_precision(value: ClipPrecision) -> u8 {
    match value {
        ClipPrecision::Default => 0,
        ClipPrecision::Character => 1,
        ClipPrecision::Stroke => 2,
        ClipPrecision::Reserved(value) => value,
    }
}
fn family(value: Family) -> u8 {
    match value {
        Family::Default => 0,
        Family::Serif => 1,
        Family::SansSerif => 2,
        Family::Monospace => 3,
        Family::Script => 4,
        Family::Decorative => 5,
        Family::Reserved(value) => value,
    }
}
fn decode_encoding(value: u8) -> Encoding {
    match value {
        0 => Encoding::Latin,
        1 => Encoding::Automatic,
        2 => Encoding::Symbol,
        77 => Encoding::MacRoman,
        128 => Encoding::Japanese,
        129 => Encoding::Korean,
        130 => Encoding::Johab,
        134 => Encoding::SimplifiedChinese,
        136 => Encoding::TraditionalChinese,
        161 => Encoding::Greek,
        162 => Encoding::Turkish,
        163 => Encoding::Vietnamese,
        177 => Encoding::Hebrew,
        178 => Encoding::Arabic,
        186 => Encoding::Baltic,
        204 => Encoding::Cyrillic,
        222 => Encoding::Thai,
        238 => Encoding::EasternEuropean,
        255 => Encoding::Oem,
        value => Encoding::Reserved(value),
    }
}
fn decode_family(value: u8) -> Family {
    match value {
        0 => Family::Default,
        1 => Family::Serif,
        2 => Family::SansSerif,
        3 => Family::Monospace,
        4 => Family::Script,
        5 => Family::Decorative,
        value => Family::Reserved(value),
    }
}
impl Font {
    pub(super) fn new(request: &FontRequest) -> Result<Self, TextError> {
        if request.height.unsigned_abs() > 16384 || request.width.unsigned_abs() > 16384 {
            return Err(TextError::Font("invalid font dimensions".into()));
        }
        let clip = request.clipping;
        let mut logical = gdi::LOGFONTW {
            lfHeight: request.height,
            lfWidth: request.width,
            lfEscapement: request.baseline_angle,
            lfOrientation: request.glyph_angle,
            lfWeight: request.weight,
            lfItalic: u8::from(request.italic),
            lfUnderline: u8::from(request.underline),
            lfStrikeOut: u8::from(request.strikeout),
            lfCharSet: encoding(request.encoding),
            lfOutPrecision: precision(request.precision),
            lfClipPrecision: clip_precision(clip.precision)
                | (u8::from(clip.transform_rotation) << 4)
                | (u8::from(clip.force_outline) << 5)
                | (u8::from(clip.disable_association) << 6)
                | (u8::from(clip.embedded) << 7),
            lfQuality: quality(request.quality),
            lfPitchAndFamily: pitch(request.pitch) | (family(request.family) << 4),
            ..Default::default()
        };
        for (slot, unit) in logical
            .lfFaceName
            .iter_mut()
            .take(31)
            .zip(request.face.encode_utf16())
        {
            *slot = unit;
        }
        // SAFETY: initialized host LOGFONT; each acquired handle is released on
        // failure or by Drop after restoring the original DC selection.
        unsafe {
            let dc = gdi::CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return Err(TextError::Font("CreateCompatibleDC failed".into()));
            }
            let font = gdi::CreateFontIndirectW(&logical);
            if font.is_null() {
                gdi::DeleteDC(dc);
                return Err(TextError::Font("CreateFontIndirectW failed".into()));
            }
            let old = gdi::SelectObject(dc, font);
            if old.is_null() || old as usize == usize::MAX {
                gdi::DeleteObject(font);
                gdi::DeleteDC(dc);
                return Err(TextError::Font("SelectObject failed".into()));
            }
            Ok(Self { dc, font, old })
        }
    }
    pub(super) fn metrics(&self) -> Result<TextMetrics, TextError> {
        let mut m = gdi::TEXTMETRICA::default();
        // SAFETY: self owns the DC; m is initialized writable host storage.
        if unsafe { gdi::GetTextMetricsA(self.dc, &mut m) } == 0 {
            return Err(TextError::Font("GetTextMetricsA failed".into()));
        }
        Ok(TextMetrics {
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
            first_character: m.tmFirstChar,
            last_character: m.tmLastChar,
            default_character: m.tmDefaultChar,
            break_character: m.tmBreakChar,
            italic: m.tmItalic != 0,
            underline: m.tmUnderlined != 0,
            strikeout: m.tmStruckOut != 0,
            variable_pitch: m.tmPitchAndFamily & 1 != 0,
            vector: m.tmPitchAndFamily & 2 != 0,
            outline: m.tmPitchAndFamily & 4 != 0,
            device: m.tmPitchAndFamily & 8 != 0,
            family: decode_family(m.tmPitchAndFamily >> 4),
            encoding: decode_encoding(m.tmCharSet),
        })
    }
    pub(super) fn extent(&self, input: TextInput<'_>) -> Result<TextExtent, TextError> {
        let mut size = windows_sys::Win32::Foundation::SIZE::default();
        let result = match input {
            TextInput::Encoded(bytes) => {
                let count = i32::try_from(bytes.len())
                    .map_err(|_| TextError::Font("encoded run length overflow".into()))?;
                // SAFETY: the counted input covers every byte GDI may consume;
                // no terminator scan. The selected private font defines its repertoire.
                unsafe { gdi::GetTextExtentPoint32A(self.dc, bytes.as_ptr(), count, &mut size) }
            }
            TextInput::Unicode(units) => {
                let count = i32::try_from(units.len())
                    .map_err(|_| TextError::Font("UTF-16 run length overflow".into()))?;
                // SAFETY: same owned DC and explicit count of initialized UTF-16 units.
                unsafe { gdi::GetTextExtentPoint32W(self.dc, units.as_ptr(), count, &mut size) }
            }
        };
        if result == 0 {
            return Err(TextError::Font("native text extent query failed".into()));
        }
        Ok(TextExtent {
            width: size.cx,
            height: size.cy,
        })
    }
    pub(super) fn glyph(&self, request: GlyphRequest) -> Result<Glyph, TextError> {
        let (code, ansi, index) = match request.character {
            Character::Unicode(code) => (code, false, false),
            Character::Encoded(code) => (code, true, false),
            Character::GlyphIndex(code) => (code, false, true),
        };
        let mut format = match request.format {
            GlyphFormat::Metrics => 0,
            GlyphFormat::Monochrome => 1,
            GlyphFormat::QuadraticOutline => 2,
            GlyphFormat::CubicOutline => 3,
            GlyphFormat::Gray2 => 4,
            GlyphFormat::Gray4 => 5,
            GlyphFormat::Gray8 => 6,
        };
        if index {
            format |= 0x80;
        }
        if !request.hinted {
            format |= 0x100;
        }
        let fixed = |value: i32| gdi::FIXED {
            fract: value as u16,
            value: (value >> 16) as i16,
        };
        let transform = gdi::MAT2 {
            eM11: fixed(request.transform.xx),
            eM12: fixed(request.transform.xy),
            eM21: fixed(request.transform.yx),
            eM22: fixed(request.transform.yy),
        };
        let mut metrics = gdi::GLYPHMETRICS::default();
        let get_outline = if ansi {
            gdi::GetGlyphOutlineA
        } else {
            gdi::GetGlyphOutlineW
        };
        // SAFETY: owned DC and initialized host metrics/transform; query has no output buffer.
        let size = unsafe {
            get_outline(
                self.dc,
                code,
                format,
                &mut metrics,
                0,
                std::ptr::null_mut(),
                &transform,
            )
        };
        if size == u32::MAX || size > 16 * 1024 * 1024 {
            return Err(TextError::Glyph("GetGlyphOutline failed".into()));
        }
        let mut bytes = Vec::new();
        if request.format != GlyphFormat::Metrics && size != 0 {
            bytes.resize(size as usize, 0);
            // SAFETY: bytes covers exactly the size returned by this same native query.
            if unsafe {
                get_outline(
                    self.dc,
                    code,
                    format,
                    &mut metrics,
                    size,
                    bytes.as_mut_ptr().cast(),
                    &transform,
                )
            } == u32::MAX
            {
                return Err(TextError::Glyph("GetGlyphOutline data failed".into()));
            }
        }
        let data = match request.format {
            GlyphFormat::Metrics => GlyphData::None,
            GlyphFormat::QuadraticOutline | GlyphFormat::CubicOutline => {
                GlyphData::Outline(contours(&bytes)?)
            }
            _ => GlyphData::Bitmap(bytes),
        };
        Ok(Glyph {
            metrics: GlyphMetrics {
                width: metrics.gmBlackBoxX,
                height: metrics.gmBlackBoxY,
                origin_x: metrics.gmptGlyphOrigin.x,
                origin_y: metrics.gmptGlyphOrigin.y,
                advance_x: metrics.gmCellIncX,
                advance_y: metrics.gmCellIncY,
            },
            required_bytes: size,
            data,
        })
    }
}
fn contours(bytes: &[u8]) -> Result<Vec<Contour>, TextError> {
    let invalid = || TextError::Glyph("invalid native outline result".into());
    let u32_at = |at: usize| -> Result<u32, TextError> {
        Ok(u32::from_le_bytes(
            bytes
                .get(at..at + 4)
                .ok_or_else(invalid)?
                .try_into()
                .unwrap(),
        ))
    };
    let point = |at: usize| -> Result<Point, TextError> {
        Ok(Point {
            x: u32_at(at)? as i32,
            y: u32_at(at + 4)? as i32,
        })
    };
    let mut output = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let end = at
            .checked_add(u32_at(at)? as usize)
            .filter(|&end| end <= bytes.len() && end >= at + 16)
            .ok_or_else(invalid)?;
        if u32_at(at + 4)? != 24 {
            return Err(invalid());
        }
        let mut contour = Contour {
            start: point(at + 8)?,
            curves: Vec::new(),
        };
        at += 16;
        while at < end {
            let kind = u16::from_le_bytes(
                bytes
                    .get(at..at + 2)
                    .ok_or_else(invalid)?
                    .try_into()
                    .unwrap(),
            );
            let count = u16::from_le_bytes(
                bytes
                    .get(at + 2..at + 4)
                    .ok_or_else(invalid)?
                    .try_into()
                    .unwrap(),
            ) as usize;
            let next = at + 4 + count * 8;
            if next > end {
                return Err(invalid());
            }
            let points = (0..count)
                .map(|i| point(at + 4 + i * 8))
                .collect::<Result<Vec<_>, _>>()?;
            contour.curves.push(match kind {
                1 => Curve::Line(points),
                2 => Curve::Quadratic(points),
                3 => Curve::Cubic(points),
                _ => return Err(invalid()),
            });
            at = next;
        }
        output.push(contour);
    }
    Ok(output)
}
impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: restore the owned DC's previous selection before releasing its font.
        unsafe {
            gdi::SelectObject(self.dc, self.old);
            gdi::DeleteObject(self.font);
            gdi::DeleteDC(self.dc);
        }
    }
}
