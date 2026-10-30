//! GDI font semantics and guest layouts, restored over Wine's DC objects.
//! Matching, rasterization and glyph caching belong to adv-device's Text.
use adv_abi::{AnsiStr, Env, GuestPtr, Handle, Outcome, Registry, WideStr, win32};
use adv_cpu::backend::GuestExt;
use adv_device::text::{
    self, Character, ClipPrecision, Clipping, Curve, Encoding, Family, FontRequest, GlyphData,
    GlyphFormat, GlyphRequest, Pitch, Precision, Quality, Text, TextError, Transform,
};
use std::borrow::Cow;

mod charset;
mod driver;
mod dwrite;
mod enumeration;
mod eudc;
mod kerning;
mod layout;
mod logical;
mod paint;
mod placement;
mod rectangle;
mod resource;
mod spacing;
mod widths;
use logical::from_font;
const GDI_ERROR: u32 = u32::MAX;

struct LogFont {
    raw: [u8; 92],
    face: String,
}
pub(super) struct Fonts {
    pub text: Text,
    codec: adv_os::nls::Codec,
    locale: u32,
    resources: std::collections::BTreeMap<(String, u32), resource::Reference>,
    memory: std::collections::BTreeMap<u32, text::FontResourceId>,
    next_memory: u32,
    eudc: Option<text::FontResourceId>,
    apis: Registry,
    dwrite: std::collections::BTreeMap<u32, (text::FontResourceId, u32)>,
    realizations: std::collections::BTreeMap<u32, FontRequest>,
}

pub(super) fn install<B: adv_cpu::backend::Backend>(
    machine: &mut adv_machine::Machine<B>,
    text: Text,
    locale: u32,
) {
    let acp = match locale {
        0x411 => 932,
        0x804 => 936,
        _ => 1252,
    };
    let mut apis = Registry::default();
    register(&mut apis);
    machine.ext.insert(Fonts {
        text,
        apis,
        codec: adv_os::nls::Codec::new(acp),
        locale,
        resources: Default::default(),
        memory: Default::default(),
        next_memory: 0,
        eudc: None,
        dwrite: Default::default(),
        realizations: Default::default(),
    });
    machine
        .ext
        .insert(adv_nt::native::FontFrontend(driver::run));
}
fn set_last_error(env: &mut Env<'_>, value: u32) {
    let _ = env
        .cpu
        .write_u32(env.ctx().teb + adv_machine::layout::teb::LAST_ERROR, value);
}
fn module(env: &Env<'_>) -> Option<u32> {
    env.ext
        .try_get::<adv_nt::identity::Images>()?
        .0
        .values()
        .find(|m| m.name == "win32u.so")
        .map(|m| m.base)
}
fn object(env: &Env<'_>, handle: u32, kind: u8) -> Option<u32> {
    let base = module(env)?;
    let shared = env.cpu.read_u32(base + layout::GDI_SHARED).ok()?;
    let entry = shared + (handle & 0xffff) * layout::handle::SIZE;
    if env.cpu.read_u8(entry + layout::handle::Type).ok()? != kind {
        return None;
    }
    let unique = env.cpu.read_u16(entry + 12).ok()?;
    if handle >> 16 != 0 && handle >> 16 != u32::from(unique) {
        return None;
    }
    env.cpu.read_u32(entry).ok().filter(|&p| p != 0)
}
fn dc_pointer(env: &Env<'_>, dc: u32) -> Option<u32> {
    object(env, dc, 1)
}
fn reject_dc(env: &mut Env<'_>, dc: u32) -> bool {
    if dc_pointer(env, dc).is_some() {
        false
    } else {
        set_last_error(env, 6);
        true
    }
}
fn font_request(env: &Env<'_>, dc: u32) -> FontRequest {
    let dc = dc_pointer(env, dc).expect("validated DC");
    let font = env.cpu.read_u32(dc + layout::dc::hFont).unwrap();
    let mut request = request_for_handle(env, font);
    let attr = env.cpu.read_u32(dc + layout::dc::attr).unwrap();
    if env
        .cpu
        .read_u32(attr + layout::attr::graphics_mode)
        .unwrap()
        != 2
    {
        request.glyph_angle = request.baseline_angle;
    }
    request
}
fn request_for_handle(env: &Env<'_>, font: u32) -> FontRequest {
    if font & 0xffff >= 42 && font & 0xffff <= 49 {
        let mut request = from_font(&LogFont {
            raw: [0; 92],
            face: String::new(),
        });
        request.default = Some(match (font & 0xffff) - 32 {
            10 => text::DefaultFont::Console,
            11 => text::DefaultFont::Fixed,
            12 => text::DefaultFont::Variable,
            14 => text::DefaultFont::Device,
            16 => text::DefaultFont::SystemFixed,
            17 => text::DefaultFont::Interface,
            _ => text::DefaultFont::System,
        });
        return request;
    }

    let pointer = object(env, font, 10).expect("Wine font object");
    let mut raw = [0; 92];
    env.cpu
        .read(pointer + layout::font::logfont, &mut raw)
        .unwrap();
    let units: Vec<_> = raw[28..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|u| u16::from_le_bytes(*u))
        .take_while(|&u| u != 0)
        .collect();
    from_font(&LogFont {
        raw,
        face: String::from_utf16_lossy(&units),
    })
}

/// ANSI/default text belongs to the guest locale, never the host process ACP.
/// Explicit supported DBCS repertoires also remain independent of host locale.
/// Symbol and other explicit repertoires retain their existing backend mapping.
fn ansi_codepage(env: &Env<'_>, request: &FontRequest) -> Option<u32> {
    Some(match request.encoding {
        Encoding::Automatic | Encoding::Latin | Encoding::Oem => env.ext.get::<Fonts>().codec.acp,
        Encoding::Japanese => 932,
        Encoding::SimplifiedChinese => 936,
        _ => return None,
    })
}

fn ansi_units(env: &Env<'_>, request: &FontRequest, bytes: &[u8]) -> Option<(Vec<u16>, bool)> {
    Some(
        env.ext
            .get::<Fonts>()
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
        return Some(env.ext.get::<Fonts>().codec.to_utf16(1252, bytes));
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
        .get::<Fonts>()
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
        Err(TextError::Unavailable) => unavailable_font(env, 0),
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
                bytes[44 + index * 2..46 + index * 2].copy_from_slice(&character.to_le_bytes());
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
    const OUTLINE_SIZE: usize = 212;
    if reject_dc(env, dc.0) {
        return Outcome::Return(0);
    }
    if size == 0 && !output.is_null() {
        return Outcome::Return(0);
    }
    let request = font_request(env, dc.0);
    let metrics = match env.ext.get_mut::<Fonts>().text.outline_metrics(&request) {
        Ok(metrics) => metrics,
        Err(TextError::Unavailable) => {
            adv_abi::warn_unavailable(
                env,
                "gdi32.dll!GetOutlineTextMetricsA",
                "selected-font outline metrics are unavailable on this text backend",
            );
            set_last_error(env, 50);
            return Outcome::Return(0);
        }
        Err(_) => return Outcome::Return(0),
    };
    let mut bytes = pack_outline_metrics(env, &metrics, None);
    let required = bytes.len() as u32;
    if output.is_null() {
        return Outcome::Return(required);
    }
    // Native GDI supports a prefix of the fixed structure, with zero name
    // offsets. A buffer extending past that structure must fit all names;
    // otherwise it publishes only the fixed fields with otmSize=0 and fails.
    let failed_names = size as usize > OUTLINE_SIZE && size < required;
    let copied = if size < required {
        bytes[196..OUTLINE_SIZE].fill(0);
        (size as usize).min(OUTLINE_SIZE)
    } else {
        bytes.len()
    };
    if copied >= 4 {
        let returned = if failed_names { 0 } else { copied as u32 };
        bytes[..4].copy_from_slice(&returned.to_le_bytes());
    } else {
        // For a sub-DWORD prefix the native ANSI thunk exposes the underlying
        // Unicode query size; it cannot replace a complete otmSize field.
        let wide_required = 216u32
            + metrics
                .names
                .iter()
                .map(|name| (name.encode_utf16().count() as u32 + 1) * 2)
                .sum::<u32>();
        bytes[..4].copy_from_slice(&wide_required.to_le_bytes());
    }
    if adv_abi::guest::check_range(
        env.cpu,
        output.addr(),
        copied as u32,
        adv_cpu::backend::Prot::WRITE,
    )
    .is_err()
        || env.cpu.write(output.addr(), &bytes[..copied]).is_err()
    {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    if failed_names {
        set_last_error(env, 1003);
        Outcome::Return(0)
    } else {
        Outcome::Return(copied as u32)
    }
}

#[win32(gdi32, name = "GetOutlineTextMetricsW", level = implement)]
fn outline_metrics_w(env: &mut Env<'_>, dc: Handle, size: u32, output: GuestPtr<u8>) -> Outcome {
    if reject_dc(env, dc.0) || (size == 0 && !output.is_null()) {
        return Outcome::Return(0);
    }
    let request = font_request(env, dc.0);
    let result = {
        let provider = &mut env.ext.get_mut::<Fonts>().text;
        provider.outline_metrics(&request).and_then(|metrics| {
            provider
                .unicode_metrics(&request)
                .map(|(_, chars)| (metrics, chars))
        })
    };
    let (metrics, chars) = match result {
        Ok(value) => value,
        Err(error) => {
            adv_abi::warn_unavailable(env, "gdi32.dll!GetOutlineTextMetricsW", &error.to_string());
            set_last_error(env, 50);
            return Outcome::Return(0);
        }
    };
    let mut bytes = pack_outline_metrics(env, &metrics, Some(chars));
    let required = bytes.len() as u32;
    if output.is_null() {
        return Outcome::Return(required);
    }
    // The W front keeps full otmSize in every prefix. Beyond the fixed x86
    // structure, insufficient name storage fails without writes or LastError.
    if size > 216 && size < required {
        return Outcome::Return(0);
    }
    if size < required {
        bytes[200..216].fill(0);
    }
    let copied = size.min(required) as usize;
    if adv_abi::guest::check_range(
        env.cpu,
        output.addr(),
        copied as u32,
        adv_cpu::backend::Prot::WRITE,
    )
    .is_err()
        || env.cpu.write(output.addr(), &bytes[..copied]).is_err()
    {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    Outcome::Return(copied as u32)
}

fn pack_outline_metrics(
    env: &Env<'_>,
    metrics: &text::OutlineMetrics,
    characters: Option<[u16; 4]>,
) -> Vec<u8> {
    // SDK x86 OUTLINETEXTMETRICA: the 56-byte TEXTMETRICA occupies 4..60,
    // PANOSE follows its filler, and typographic ascent starts at byte 96.
    let shift = usize::from(characters.is_some()) * 4;
    let mut bytes = vec![0; 212 + shift];
    let common = pack_metrics(&metrics.text);
    if let Some(chars) = characters {
        bytes[4..48].copy_from_slice(&common[..44]);
        for (index, unit) in chars.into_iter().enumerate() {
            bytes[48 + index * 2..50 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        bytes[56..61].copy_from_slice(&common[48..53]);
    } else {
        bytes[4..60].copy_from_slice(&common);
    }
    bytes[61 + shift..71 + shift].copy_from_slice(&metrics.panose);
    for (index, value) in [
        metrics.selection,
        metrics.embedding,
        metrics.caret_rise as u32,
        metrics.caret_run as u32,
        metrics.italic_angle as u32,
        metrics.em_units,
        metrics.ascent as u32,
        metrics.descent as u32,
        metrics.line_gap,
        metrics.cap_height,
        metrics.x_height,
        metrics.bounds.left as u32,
        metrics.bounds.top as u32,
        metrics.bounds.right as u32,
        metrics.bounds.bottom as u32,
        metrics.mac_ascent as u32,
        metrics.mac_descent as u32,
        metrics.mac_line_gap,
        metrics.minimum_ppem,
        metrics.subscript_size.x as u32,
        metrics.subscript_size.y as u32,
        metrics.subscript_offset.x as u32,
        metrics.subscript_offset.y as u32,
        metrics.superscript_size.x as u32,
        metrics.superscript_size.y as u32,
        metrics.superscript_offset.x as u32,
        metrics.superscript_offset.y as u32,
        metrics.strikeout_size,
        metrics.strikeout_position as u32,
        metrics.underline_size as u32,
        metrics.underline_position as u32,
    ]
    .into_iter()
    .enumerate()
    {
        let offset = 72 + shift + index * 4;
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    let codec = &env.ext.get::<Fonts>().codec;
    for (index, name) in metrics.names.iter().enumerate() {
        let offset = bytes.len() as u32;
        bytes[196 + shift + index * 4..200 + shift + index * 4]
            .copy_from_slice(&offset.to_le_bytes());
        if characters.is_some() {
            bytes.extend(name.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
        } else {
            bytes.extend(codec.encode(name));
            bytes.push(0);
        }
    }
    let required = bytes.len() as u32;
    bytes[..4].copy_from_slice(&required.to_le_bytes());
    bytes
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
#[win32(gdi32,name="GetTextExtentPointA",level=implement)]
fn legacy_extent_a(
    env: &mut Env<'_>,
    dc: Handle,
    input: AnsiStr,
    count: i32,
    output: GuestPtr<[i32; 2]>,
) -> Outcome {
    extent_a(env, dc, input, count, output)
}
#[win32(gdi32,name="GetTextExtentPointW",level=implement)]
fn legacy_extent_w(
    env: &mut Env<'_>,
    dc: Handle,
    input: WideStr,
    count: i32,
    output: GuestPtr<[i32; 2]>,
) -> Outcome {
    extent_w(env, dc, input, count, output)
}
fn unavailable_font(env: &mut Env<'_>, failure: u32) -> Outcome {
    let api = format!("gdi32.dll!{}", env.sys.thunks.get(env.thunk).name);
    adv_abi::warn_unavailable(env, &api, "no font backend is available on this host");
    set_last_error(env, 50);
    Outcome::Return(failure)
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
    let measured = env.ext.get_mut::<Fonts>().text.extent(&request, input);
    let measured = measured.and_then(|mut size| {
        let dc = dc_pointer(env, dc).unwrap();
        let attr = env.cpu.read_u32(dc + layout::dc::attr).unwrap();
        let extra = env.cpu.read_u32(attr + layout::attr::char_extra).unwrap() as i32;
        let justify = env.cpu.read_u32(dc + layout::dc::breakExtra).unwrap() as i32;
        let rem = env.cpu.read_u32(dc + layout::dc::breakRem).unwrap() as i32;
        if extra != 0 || justify != 0 || rem != 0 {
            let decoded = match input {
                text::TextInput::Unicode(units) => units.to_vec(),
                text::TextInput::Encoded(bytes) => env.ext.get::<Fonts>().codec.to_utf16(0, bytes),
            };
            size.width = driver::positions(env, &request, &decoded, false, extra, justify, rem)?.1;
        }
        Ok(size)
    });
    match measured {
        Ok(size) if output.write(env.cpu, [size.width, size.height]).is_ok() => Outcome::Return(1),
        Err(TextError::Unavailable) => unavailable_font(env, 0),
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
        Err(TextError::Unavailable) => unavailable_font(env, GDI_ERROR),
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

pub fn register(registry: &mut Registry) {
    eudc::register(registry);
    paint::register(registry);
    resource::register(registry);
    enumeration::register(registry);
    rectangle::register(registry);
    widths::register(registry);
    spacing::register(registry);
    placement::register(registry);
    charset::register(registry);
    kerning::register(registry);
    for api in [
        glyph_a_ABI,
        glyph_w_ABI,
        metrics_a_ABI,
        metrics_w_ABI,
        outline_metrics_a_ABI,
        outline_metrics_w_ABI,
        extent_a_ABI,
        legacy_extent_a_ABI,
        legacy_extent_w_ABI,
        extent_w_ABI,
    ] {
        registry.register(api);
    }
}

#[cfg(test)]
mod tests;
