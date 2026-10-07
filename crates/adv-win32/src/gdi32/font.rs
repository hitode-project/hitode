//! GDI font semantics and guest layouts. Matching, rasterization and immutable
//! glyph caching are owned by adv-device; DCs have one OS object payload.
use super::{GdiObj, LogFont};
use crate::{Win32, util::set_last_error};
use adv_abi::{AnsiStr, Env, GuestPtr, Handle, Outcome, Registry, WideStr, win32};
use adv_device::text::{
    self, Character, ClipPrecision, Clipping, Curve, Encoding, Family, FontRequest, GlyphData,
    GlyphFormat, GlyphRequest, Pitch, Precision, Quality, Text, TextError, Transform,
};
use adv_machine::Machine;
use std::borrow::Cow;

mod enumeration;
mod eudc;
mod rectangle;
mod paint;
mod resource;
mod widths;
mod charset;
mod logical;
mod kerning;
#[cfg(test)]
#[path = "font/tests.rs"]
mod tests;

const STOCK_FONT: u32 = 0x0190_0010 + 13;
const GDI_ERROR: u32 = u32::MAX;

#[derive(Default)]
struct Fonts {
    text: Text,
    resources: std::collections::BTreeMap<(String, u32), resource::Reference>,
}

pub fn register(registry: &mut Registry) {
    eudc::register(registry);
    paint::register(registry);
    resource::register(registry);
    enumeration::register(registry);
    rectangle::register(registry);
    widths::register(registry);
    charset::register(registry);
    logical::register(registry);
    kerning::register(registry);
    for api in [
        glyph_a_ABI,
        glyph_w_ABI,
        metrics_a_ABI,
        metrics_w_ABI,
        outline_metrics_a_ABI,
        set_map_mode_ABI,
        get_map_mode_ABI,
        set_poly_fill_ABI,
        get_poly_fill_ABI,
        set_rop2_ABI,
        get_rop2_ABI,
        extent_a_ABI,
        extent_w_ABI,
    ] {
        registry.register(api);
    }
}

pub fn install<B: adv_cpu::backend::Backend>(machine: &mut Machine<B>) {
    machine.ext.insert(Fonts::default());
}
pub fn cache_stats<B: adv_cpu::backend::Backend>(machine: &Machine<B>) -> text::CacheStats {
    machine.ext.get::<Fonts>().text.stats()
}

/// The caller runs this only after DLL detach/CRT cleanup has completed.
pub(crate) fn detach_process(env: &mut Env<'_>) -> Result<(), String> {
    let Some(fonts) = env.ext.try_get_mut::<Fonts>() else {
        return Ok(());
    };
    while let Some((path, reference)) = fonts.resources.first_key_value() {
        let (path, id) = (path.clone(), reference.id);
        if !fonts
            .text
            .remove_resource(id)
            .map_err(|error| error.to_string())?
        {
            return Err("font resource owner missing during process cleanup".into());
        }
        fonts.resources.remove(&path);
    }
    Ok(())
}

fn reject_dc(env: &mut Env<'_>, dc: u32) -> bool {
    if matches!(
        env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc),
        Some(GdiObj::Dc { .. })
    ) {
        false
    } else {
        set_last_error(env, 6);
        true
    }
}
fn modes<'a>(env: &'a mut Env<'_>, dc: u32) -> &'a mut adv_os::gdi::DcModes {
    let Some(GdiObj::Dc { modes, .. }) =
        env.ext.get::<Win32>().gdi.get_mut(&mut env.sys.objects, dc)
    else {
        unreachable!("validated DC")
    };
    modes
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
    std::mem::replace(&mut modes(env, dc.0).map, mode)
}
#[win32(gdi32, name = "GetMapMode", level = state)]
fn get_map_mode(env: &mut Env<'_>, dc: Handle) -> i32 {
    if reject_dc(env, dc.0) {
        return 0;
    }
    modes(env, dc.0).map
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
    std::mem::replace(&mut modes(env, dc.0).poly_fill, mode)
}
#[win32(gdi32, name = "GetPolyFillMode", level = state)]
fn get_poly_fill(env: &mut Env<'_>, dc: Handle) -> i32 {
    if reject_dc(env, dc.0) {
        return 0;
    }
    modes(env, dc.0).poly_fill
}

#[win32(gdi32, name = "SetROP2", level = state)]
fn set_rop2(env: &mut Env<'_>, dc: Handle, mode: i32) -> i32 {
    if reject_dc(env, dc.0) {
        return 0;
    }
    // Native GDI stores the low byte, including values outside the documented
    // R2_BLACK..R2_WHITE range; state queries must preserve that normalization.
    std::mem::replace(&mut modes(env, dc.0).rop2, i32::from(mode as u8))
}
#[win32(gdi32, name = "GetROP2", level = state)]
fn get_rop2(env: &mut Env<'_>, dc: Handle) -> i32 {
    if reject_dc(env, dc.0) {
        return 0;
    }
    modes(env, dc.0).rop2
}

fn font_request(env: &Env<'_>, dc: u32) -> FontRequest {
    let gdi = &env.ext.get::<Win32>().gdi;
    let font = match gdi.get(&env.sys.objects, dc) {
        Some(GdiObj::Dc { selected, .. }) => selected[2],
        _ => STOCK_FONT,
    };
    request_for_handle(env, font)
}

fn request_for_handle(env: &Env<'_>, font: u32) -> FontRequest {
    let gdi = &env.ext.get::<Win32>().gdi;
    if let Some(GdiObj::Font(font)) = gdi.get(&env.sys.objects, font) {
        from_font(font)
    } else {
        FontRequest {
            default: Some(match font.wrapping_sub(0x0190_0010) {
                10 => text::DefaultFont::Console,
                11 => text::DefaultFont::Fixed,
                12 => text::DefaultFont::Variable,
                14 => text::DefaultFont::Device,
                16 => text::DefaultFont::SystemFixed,
                17 => text::DefaultFont::Interface,
                _ => text::DefaultFont::System,
            }),
            face: String::new(),
            height: 0,
            width: 0,
            baseline_angle: 0,
            glyph_angle: 0,
            weight: 0,
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
            quality: Quality::Default,
            pitch: Pitch::Default,
            family: Family::Default,
        }
    }
}

/// USER control geometry measures the same logical font without creating a DC.
pub(crate) fn metrics_for_logical_font(
    env: &mut Env<'_>,
    font: u32,
) -> Result<text::TextMetrics, TextError> {
    let request = request_for_handle(env, font);
    env.ext.get_mut::<Fonts>().text.metrics(&request)
}

/// ANSI/default text belongs to the guest locale, never the host process ACP.
/// Explicit supported DBCS repertoires also remain independent of host locale.
/// Symbol and other explicit repertoires retain their existing backend mapping.
fn ansi_codepage(env: &Env<'_>, request: &FontRequest) -> Option<u32> {
    Some(match request.encoding {
        Encoding::Automatic | Encoding::Latin | Encoding::Oem => env.ext.get::<Win32>().codec.acp,
        Encoding::Japanese => 932,
        Encoding::SimplifiedChinese => 936,
        _ => return None,
    })
}

fn ansi_units(env: &Env<'_>, request: &FontRequest, bytes: &[u8]) -> Option<(Vec<u16>, bool)> {
    Some(
        env.ext
            .get::<Win32>()
            .codec
            .to_utf16_checked(ansi_codepage(env, request)?, bytes),
    )
}

/// A counted ANSI string for text output and extents.
fn ansi_text(env: &Env<'_>, request: &FontRequest, bytes: &[u8]) -> Option<Vec<u16>> {
    // Native GDI decodes a string of one byte in an ANSI_CHARSET font as
    // cp1252, even where the ACP is a DBCS code page; longer strings use the
    // ACP. Text drawn a byte at a time therefore shows Latin-1 glyphs.
    if matches!(request.encoding, Encoding::Latin) && bytes.len() == 1 {
        return Some(env.ext.get::<Win32>().codec.to_utf16(1252, bytes));
    }
    ansi_units(env, request, bytes).map(|(units, _)| units)
}

fn ansi_character(env: &Env<'_>, font: &FontRequest, character: u32) -> Character {
    // Preserve native handling of values outside the packed ANSI low word.
    if character > u32::from(u16::MAX) {
        return Character::Encoded(character);
    }
    let Some(codepage) = ansi_codepage(env, font) else {
        return Character::Encoded(character);
    };
    // GGO packs a DBCS lead byte in the high byte, unlike a counted A string.
    // Resolve the guest repertoire before entering a host font backend, whose
    // default charset may have been selected under a different system locale.
    let packed = character as u16;
    let bytes = packed.to_be_bytes();
    let bytes = if env
        .ext
        .get::<Win32>()
        .codec
        .is_lead_byte(codepage, bytes[0])
    {
        &bytes[..]
    } else {
        &bytes[1..]
    };
    match ansi_units(env, font, bytes) {
        // A replacement produced by the codec is not a decoded ANSI character.
        // Keep the backend's native invalid-sequence behavior in that case.
        Some((units, false)) if units.len() == 1 => Character::Unicode(u32::from(units[0])),
        _ => Character::Encoded(character),
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

#[win32(gdi32, name = "GetTextMetricsW", level = implement)]
fn metrics_w(env: &mut Env<'_>, dc: Handle, output: GuestPtr<u8>) -> i32 {
    // Authored native memory-DC oracle: NULL HDC or NULL output returns FALSE
    // without changing LastError or publishing any metrics.
    if dc.0 == 0 {
        return 0;
    }
    if reject_dc(env, dc.0) {
        return 0;
    }
    if output.is_null() {
        return 0;
    }
    // Preflight all 60 SDK bytes before realizing a font or publishing data.
    // Non-NULL inaccessible guest output follows this GDI personality's
    // checked failure contract; it does not claim native AV equivalence.
    if adv_abi::guest::check_range(env.cpu, output.addr(), 60, adv_cpu::backend::Prot::WRITE)
        .is_err()
    {
        set_last_error(env, 87);
        return 0;
    }
    let request = font_request(env, dc.0);
    match env.ext.get_mut::<Fonts>().text.unicode_metrics(&request) {
        Ok((metrics, characters)) => {
            let common = pack_metrics(&metrics);
            let mut bytes = [0; 60];
            bytes[..44].copy_from_slice(&common[..44]);
            for (index, character) in characters.into_iter().enumerate() {
                bytes[44 + index * 2..46 + index * 2]
                    .copy_from_slice(&character.to_le_bytes());
            }
            bytes[52..57].copy_from_slice(&common[48..53]);
            if env.cpu.write(output.addr(), &bytes).is_err() {
                set_last_error(env, 87);
                return 0;
            }
            1
        }
        Err(error) => {
            adv_abi::warn_unavailable(
                env,
                "gdi32.dll!GetTextMetricsW",
                &format!("selected font Unicode metrics unavailable: {error}"),
            );
            set_last_error(env, 50);
            0
        }
    }
}

#[win32(gdi32, name = "GetOutlineTextMetricsA", level = implement)]
fn outline_metrics_a(env: &mut Env<'_>, dc: Handle, size: u32, output: GuestPtr<u8>) -> Outcome {
    const OUTLINE_SIZE: u32 = 212;
    if reject_dc(env, dc.0) {
        return Outcome::Return(0);
    }
    if size == 0 || output.is_null() {
        return Outcome::Return(OUTLINE_SIZE);
    }
    let request = font_request(env, dc.0);
    match env.ext.get_mut::<Fonts>().text.metrics(&request) {
        Ok(metrics) => {
            let mut buf = vec![0u8; size as usize];
            let packed = pack_metrics(&metrics);
            let size_bytes = size.to_le_bytes();
            let buf_len = buf.len();
            let header_len = 4.min(buf_len);
            buf[..header_len].copy_from_slice(&size_bytes[..header_len]);
            if buf_len > 4 {
                let copy_len = packed.len().min(buf_len - 4);
                buf[4..4 + copy_len].copy_from_slice(&packed[..copy_len]);
            }
            if env.cpu.write(output.addr(), &buf).is_ok() {
                Outcome::Return(size)
            } else {
                set_last_error(env, 87);
                Outcome::Return(0)
            }
        }
        _ => {
            set_last_error(env, 87);
            Outcome::Return(0)
        }
    }
}

#[win32(gdi32, name = "GetTextExtentPoint32A", level = implement)]
fn extent_a(
    env: &mut Env<'_>,
    dc: Handle,
    input: AnsiStr,
    count: i32,
    output: GuestPtr<[i32; 2]>,
) -> Outcome {
    extent(env, dc.0, input.0.addr(), count, output, true)
}
#[win32(gdi32, name = "GetTextExtentPoint32W", level = implement)]
fn extent_w(
    env: &mut Env<'_>,
    dc: Handle,
    input: WideStr,
    count: i32,
    output: GuestPtr<[i32; 2]>,
) -> Outcome {
    extent(env, dc.0, input.0.addr(), count, output, false)
}
fn extent(
    env: &mut Env<'_>,
    dc: u32,
    input: u32,
    count: i32,
    output: GuestPtr<[i32; 2]>,
    ansi: bool,
) -> Outcome {
    if reject_dc(env, dc) {
        return Outcome::Return(0);
    }
    // Counted strings may contain NUL. Bound work and check the whole output
    // before touching it; a short output must never publish half a SIZE.
    let length = usize::try_from(count)
        .ok()
        .and_then(|n| n.checked_mul(if ansi { 1 } else { 2 }));
    let Some(length) = length.filter(|&n| n <= 1024 * 1024) else {
        set_last_error(env, 87);
        return Outcome::Return(0);
    };
    if adv_abi::guest::check_range(env.cpu, output.addr(), 8, adv_cpu::backend::Prot::WRITE)
        .is_err()
        || (length != 0
            && adv_abi::guest::check_range(
                env.cpu,
                input,
                length as u32,
                adv_cpu::backend::Prot::READ,
            )
            .is_err())
    {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let mut bytes = vec![0; length];
    if length != 0 && env.cpu.read(input, &mut bytes).is_err() {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let request = font_request(env, dc);
    let ansi = ansi.then(|| ansi_text(env, &request, &bytes));
    let units;
    let input = if let Some(converted) = &ansi {
        match converted {
            Some(units) => text::TextInput::Unicode(units),
            None => text::TextInput::Encoded(&bytes),
        }
    } else {
        units = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|unit| u16::from_le_bytes(*unit))
            .collect::<Vec<_>>();
        text::TextInput::Unicode(&units)
    };
    match env.ext.get_mut::<Fonts>().text.extent(&request, input) {
        Ok(size) if output.write(env.cpu, [size.width, size.height]).is_ok() => Outcome::Return(1),
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
    let font = font_request(env, dc);
    let request = GlyphRequest {
        character: if format & 0x80 != 0 {
            Character::GlyphIndex(character)
        } else if ansi {
            ansi_character(env, &font, character)
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
