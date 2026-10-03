//! GDI font semantics and guest layouts. Matching, rasterization and immutable
//! glyph caching are owned by adv-device; drawing providers only lend DCs.
use super::{GdiObj, LogFont};
use crate::{Win32, util::set_last_error};
use adv_abi::{Env, GuestPtr, Handle, Handler, Outcome, Registry, win32};
use adv_device::text::{
    self, Character, ClipPrecision, Clipping, Curve, Encoding, Family, FontRequest, GlyphData,
    GlyphFormat, GlyphRequest, Pitch, Precision, Quality, Text, TextError, Transform,
};
use adv_machine::Machine;
use std::{borrow::Cow, collections::BTreeMap};

#[cfg(test)]
#[path = "font/tests.rs"]
mod tests;

const STOCK_FONT: u32 = 0x0190_0010 + 13;
const GDI_ERROR: u32 = u32::MAX;

/// The assembly supplies access to its drawing front. Font semantics remain
/// here; the callbacks only query ownership or dispatch a bound raster call.
#[derive(Clone, Copy)]
pub struct DrawingProvider {
    pub contains_dc: fn(&Env<'_>, u32) -> bool,
    pub contains_bitmap: fn(&Env<'_>, u32) -> bool,
    pub call: Handler,
}
#[derive(Clone, Copy)]
struct DcState {
    map_mode: i32,
    poly_fill: i32,
}
impl Default for DcState {
    fn default() -> Self {
        Self {
            map_mode: 1,
            poly_fill: 1,
        }
    }
}
#[derive(Default)]
struct Fonts {
    dcs: BTreeMap<u32, DcState>,
    drawing: Option<DrawingProvider>,
    text: Text,
}

pub fn register(registry: &mut Registry) {
    for api in [
        glyph_a_ABI,
        glyph_w_ABI,
        metrics_a_ABI,
        set_map_mode_ABI,
        get_map_mode_ABI,
        set_poly_fill_ABI,
        get_poly_fill_ABI,
    ] {
        registry.register(api);
    }
    for api in [select_object_ABI, delete_object_ABI, delete_dc_ABI] {
        let original = registry
            .get(api.module, api.name)
            .expect("GDI registered before fonts");
        assert_eq!(
            original.conv, api.conv,
            "font decorator must preserve the original convention"
        );
        registry.register_with_data(api, original.data);
        registry.set_trace(api.module, api.name, original.trace);
    }
}
pub fn install<B: adv_cpu::backend::Backend>(machine: &mut Machine<B>) {
    machine.ext.insert(Fonts::default());
}
pub fn set_drawing_provider<B: adv_cpu::backend::Backend>(
    machine: &mut Machine<B>,
    provider: DrawingProvider,
) {
    machine.ext.get_mut::<Fonts>().drawing = Some(provider);
}
pub fn cache_stats<B: adv_cpu::backend::Backend>(machine: &Machine<B>) -> text::CacheStats {
    machine.ext.get::<Fonts>().text.stats()
}
pub fn retire_dc(env: &mut Env<'_>, dc: u32) {
    if let Some(fonts) = env.ext.try_get_mut::<Fonts>() {
        fonts.dcs.remove(&dc);
    }
}
fn drawing(env: &Env<'_>) -> Option<DrawingProvider> {
    env.ext.get::<Fonts>().drawing
}
fn drawing_dc(env: &Env<'_>, dc: u32) -> bool {
    drawing(env).is_some_and(|p| (p.contains_dc)(env, dc))
}
fn valid_dc(env: &Env<'_>, dc: u32) -> bool {
    drawing_dc(env, dc) || matches!(env.ext.get::<Win32>().gdi.get(dc), Some(GdiObj::Dc { .. }))
}

#[win32(gdi32, name = "SelectObject", level = implement)]
fn select_object(env: &mut Env<'_>, dc: Handle, object: Handle) -> Outcome {
    let is_font = matches!(
        env.ext.get::<Win32>().gdi.get(object.0),
        Some(GdiObj::Font(_))
    ) || matches!(object.0, 0x0190_001a..=0x0190_001e | 0x0190_0020..=0x0190_0021);
    if is_font && valid_dc(env, dc.0) {
        if drawing_dc(env, dc.0) {
            env.ext.get_mut::<Win32>().gdi.register_external_dc(dc.0, 0);
        }
        return super::select_object(env);
    }
    if drawing_dc(env, dc.0) {
        (drawing(env).unwrap().call)(env)
    } else {
        super::select_object(env)
    }
}
#[win32(gdi32, name = "DeleteObject", level = implement)]
fn delete_object(env: &mut Env<'_>, object: Handle) -> Outcome {
    if drawing(env).is_some_and(|p| (p.contains_bitmap)(env, object.0)) {
        return (drawing(env).unwrap().call)(env);
    }
    if env.ext.get::<Win32>().gdi.font_selected(object.0) {
        set_last_error(env, 170);
        return Outcome::Return(0);
    }
    super::delete_object(env)
}
#[win32(gdi32, name = "DeleteDC", level = implement)]
fn delete_dc(env: &mut Env<'_>, dc: Handle) -> Outcome {
    let result = if drawing_dc(env, dc.0) {
        (drawing(env).unwrap().call)(env)
    } else {
        super::delete_dc(env)
    };
    if matches!(result, Outcome::Return(1)) {
        retire_dc(env, dc.0);
    }
    result
}
fn reject_dc(env: &mut Env<'_>, dc: u32) -> bool {
    if !valid_dc(env, dc) {
        set_last_error(env, 6);
        true
    } else {
        false
    }
}
#[win32(gdi32, name = "SetMapMode", level = state)]
fn set_map_mode(env: &mut Env<'_>, dc: Handle, mode: i32) -> i32 {
    if reject_dc(env, dc.0) {
        return 0;
    }
    if mode != 1 {
        set_last_error(env, if (2..=8).contains(&mode) { 120 } else { 87 });
        return 0;
    }
    std::mem::replace(
        &mut env
            .ext
            .get_mut::<Fonts>()
            .dcs
            .entry(dc.0)
            .or_default()
            .map_mode,
        mode,
    )
}
#[win32(gdi32, name = "GetMapMode", level = state)]
fn get_map_mode(env: &mut Env<'_>, dc: Handle) -> i32 {
    if reject_dc(env, dc.0) {
        return 0;
    }
    env.ext
        .get_mut::<Fonts>()
        .dcs
        .entry(dc.0)
        .or_default()
        .map_mode
}
#[win32(gdi32, name = "SetPolyFillMode", level = state)]
fn set_poly_fill(env: &mut Env<'_>, dc: Handle, mode: i32) -> i32 {
    if reject_dc(env, dc.0) {
        return 0;
    }
    if !matches!(mode, 1 | 2) {
        set_last_error(env, 87);
        return 0;
    }
    std::mem::replace(
        &mut env
            .ext
            .get_mut::<Fonts>()
            .dcs
            .entry(dc.0)
            .or_default()
            .poly_fill,
        mode,
    )
}
#[win32(gdi32, name = "GetPolyFillMode", level = state)]
fn get_poly_fill(env: &mut Env<'_>, dc: Handle) -> i32 {
    if reject_dc(env, dc.0) {
        return 0;
    }
    env.ext
        .get_mut::<Fonts>()
        .dcs
        .entry(dc.0)
        .or_default()
        .poly_fill
}
fn font_request(env: &Env<'_>, dc: u32) -> FontRequest {
    let gdi = &env.ext.get::<Win32>().gdi;
    let font = match gdi.get(dc) {
        Some(GdiObj::Dc { selected, .. }) => selected[2],
        _ => STOCK_FONT,
    };
    if let Some(GdiObj::Font(font)) = gdi.get(font) {
        from_font(font)
    } else {
        FontRequest {
            face: "Segoe UI".into(),
            height: 16,
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
}

#[win32(gdi32, name = "GetTextMetricsA", level = implement)]
fn metrics_a(env: &mut Env<'_>, dc: Handle, output: GuestPtr<u8>) -> Outcome {
    if reject_dc(env, dc.0) {
        return Outcome::Return(0);
    }
    let request = font_request(env, dc.0);
    match env.ext.get_mut::<Fonts>().text.metrics(&request) {
        Ok(metrics)
            if adv_abi::guest::check_range(
                env.cpu,
                output.addr(),
                56,
                adv_cpu::backend::Prot::WRITE,
            )
            .is_ok()
                && env
                    .cpu
                    .write(output.addr(), &pack_metrics(&metrics))
                    .is_ok() =>
        {
            Outcome::Return(1)
        }
        Err(TextError::Unavailable) => Outcome::Fail(TextError::Unavailable.to_string()),
        _ => {
            set_last_error(env, 87);
            Outcome::Return(0)
        }
    }
}
#[win32(gdi32, name = "GetGlyphOutlineA", level = implement)]
#[expect(
    clippy::too_many_arguments,
    reason = "typed parameters follow the generated Win32Metadata ABI"
)]
fn glyph_a(
    env: &mut Env<'_>,
    dc: Handle,
    character: u32,
    format: u32,
    metrics: GuestPtr<[u8; 20]>,
    capacity: u32,
    buffer: GuestPtr<u8>,
    matrix: GuestPtr<[u8; 16]>,
) -> Outcome {
    glyph(
        env, dc.0, character, format, metrics, capacity, buffer, matrix, true,
    )
}
#[win32(gdi32, name = "GetGlyphOutlineW", level = implement)]
#[expect(
    clippy::too_many_arguments,
    reason = "typed parameters follow the generated Win32Metadata ABI"
)]
fn glyph_w(
    env: &mut Env<'_>,
    dc: Handle,
    character: u32,
    format: u32,
    metrics: GuestPtr<[u8; 20]>,
    capacity: u32,
    buffer: GuestPtr<u8>,
    matrix: GuestPtr<[u8; 16]>,
) -> Outcome {
    glyph(
        env, dc.0, character, format, metrics, capacity, buffer, matrix, false,
    )
}
#[expect(
    clippy::too_many_arguments,
    reason = "shared A/W API behavior; typed adapters match the generated signature"
)]
fn glyph(
    env: &mut Env<'_>,
    dc: u32,
    character: u32,
    format: u32,
    metrics: GuestPtr<[u8; 20]>,
    capacity: u32,
    buffer: GuestPtr<u8>,
    matrix: GuestPtr<[u8; 16]>,
    ansi: bool,
) -> Outcome {
    if reject_dc(env, dc) {
        return Outcome::Return(GDI_ERROR);
    }
    let fail = |env: &mut Env<'_>| {
        set_last_error(env, 87);
        Outcome::Return(GDI_ERROR)
    };
    if format & !0x1ff != 0 || !matches!(format & 0x7f, 0..=6) {
        return fail(env);
    }
    let Ok(matrix) = matrix.read(env.cpu) else {
        return fail(env);
    };
    let fixed = |at| i32::from_le_bytes(matrix[at..at + 4].try_into().unwrap());
    let request = GlyphRequest {
        character: if format & 0x80 != 0 {
            Character::GlyphIndex(character)
        } else if ansi {
            Character::Encoded(character)
        } else {
            Character::Unicode(character)
        },
        format: match format & 0x7f {
            0 => GlyphFormat::Metrics,
            1 => GlyphFormat::Monochrome,
            2 => GlyphFormat::QuadraticOutline,
            3 => GlyphFormat::CubicOutline,
            4 => GlyphFormat::Gray2,
            5 => GlyphFormat::Gray4,
            _ => GlyphFormat::Gray8,
        },
        hinted: format & 0x100 == 0,
        transform: Transform {
            xx: fixed(0),
            xy: fixed(4),
            yx: fixed(8),
            yy: fixed(12),
        },
    };
    let font = font_request(env, dc);
    match env.ext.get_mut::<Fonts>().text.glyph(&font, request) {
        Ok(glyph) => {
            let fetch = !buffer.is_null() && capacity != 0 && format & 0x7f != 0;
            if fetch && capacity < glyph.required_bytes {
                return fail(env);
            }
            // Validate before writing either output, so protected or wrapping
            // buffers cannot partially publish metrics while returning failure.
            if adv_abi::guest::check_range(
                env.cpu,
                metrics.addr(),
                20,
                adv_cpu::backend::Prot::WRITE,
            )
            .is_err()
                || (fetch
                    && adv_abi::guest::check_range(
                        env.cpu,
                        buffer.addr(),
                        glyph.required_bytes,
                        adv_cpu::backend::Prot::WRITE,
                    )
                    .is_err())
            {
                return fail(env);
            }
            let bytes = if fetch {
                pack_glyph_data(&glyph.data)
            } else {
                Cow::Borrowed(&[][..])
            };
            if metrics
                .write(env.cpu, pack_glyph_metrics(glyph.metrics))
                .is_err()
                || (fetch && !bytes.is_empty() && env.cpu.write(buffer.addr(), &bytes).is_err())
            {
                return fail(env);
            }
            Outcome::Return(glyph.required_bytes)
        }
        Err(TextError::Unavailable) => Outcome::Fail(TextError::Unavailable.to_string()),
        Err(_) => fail(env),
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
fn encode_encoding(value: Encoding) -> u8 {
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
fn precision(value: u8) -> Precision {
    match value {
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
    }
}
fn family(value: u8) -> Family {
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
fn encode_family(value: Family) -> u8 {
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
fn from_font(font: &LogFont) -> FontRequest {
    let raw = &font.raw;
    let integer = |at| i32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
    FontRequest {
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
fn pack_metrics(metrics: &text::TextMetrics) -> [u8; 56] {
    let mut bytes = [0; 56];
    for (i, value) in [
        metrics.height,
        metrics.ascent,
        metrics.descent,
        metrics.internal_leading,
        metrics.external_leading,
        metrics.average_width,
        metrics.maximum_width,
        metrics.weight,
        metrics.overhang,
        metrics.aspect_x,
        metrics.aspect_y,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[44..53].copy_from_slice(&[
        metrics.first_character,
        metrics.last_character,
        metrics.default_character,
        metrics.break_character,
        // Native TEXTMETRICA uses 0xff for an enabled style, although LOGFONT
        // accepts any nonzero byte. Keep that ABI encoding out of device types.
        if metrics.italic { 0xff } else { 0 },
        if metrics.underline { 0xff } else { 0 },
        if metrics.strikeout { 0xff } else { 0 },
        u8::from(metrics.variable_pitch)
            | (u8::from(metrics.vector) << 1)
            | (u8::from(metrics.outline) << 2)
            | (u8::from(metrics.device) << 3)
            | (encode_family(metrics.family) << 4),
        encode_encoding(metrics.encoding),
    ]);
    bytes
}
fn pack_glyph_metrics(metrics: text::GlyphMetrics) -> [u8; 20] {
    let mut bytes = [0; 20];
    for (i, value) in [
        metrics.width,
        metrics.height,
        metrics.origin_x as u32,
        metrics.origin_y as u32,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[16..18].copy_from_slice(&metrics.advance_x.to_le_bytes());
    bytes[18..20].copy_from_slice(&metrics.advance_y.to_le_bytes());
    bytes
}
fn pack_glyph_data(data: &GlyphData) -> Cow<'_, [u8]> {
    match data {
        GlyphData::None => Cow::Borrowed(&[]),
        GlyphData::Bitmap(bytes) => Cow::Borrowed(bytes),
        GlyphData::Outline(contours) => {
            let mut bytes = Vec::new();
            for contour in contours {
                let start = bytes.len();
                bytes.extend_from_slice(&[0; 4]);
                bytes.extend_from_slice(&24u32.to_le_bytes());
                bytes.extend_from_slice(&contour.start.x.to_le_bytes());
                bytes.extend_from_slice(&contour.start.y.to_le_bytes());
                for curve in &contour.curves {
                    let (kind, points) = match curve {
                        Curve::Line(p) => (1u16, p),
                        Curve::Quadratic(p) => (2, p),
                        Curve::Cubic(p) => (3, p),
                    };
                    bytes.extend_from_slice(&kind.to_le_bytes());
                    bytes.extend_from_slice(&(points.len() as u16).to_le_bytes());
                    for point in points {
                        bytes.extend_from_slice(&point.x.to_le_bytes());
                        bytes.extend_from_slice(&point.y.to_le_bytes());
                    }
                }
                let length = (bytes.len() - start) as u32;
                bytes[start..start + 4].copy_from_slice(&length.to_le_bytes());
            }
            Cow::Owned(bytes)
        }
    }
}
