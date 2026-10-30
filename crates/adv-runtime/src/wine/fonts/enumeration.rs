//! Font enumeration is a snapshot of neutral device data. Every user callback
//! runs in the guest CPU, with transient guest structures on its own stack.
use super::*;
use adv_cpu::backend::{Prot, Reg};
use adv_device::text::{EnumeratedFont, FontEnumeration, FontKind};
use std::collections::VecDeque;

pub(super) fn register(registry: &mut Registry) {
    for api in [
        families_a_ABI,
        families_w_ABI,
        families_ex_a_ABI,
        families_ex_w_ABI,
        fonts_a_ABI,
        fonts_w_ABI,
    ] {
        registry.register(api);
    }
}
#[win32(gdi32, name = "EnumFontFamiliesA", level = implement)]
pub(super) fn families_a(
    env: &mut Env<'_>,
    dc: Handle,
    face: AnsiStr,
    callback: GuestPtr<()>,
    param: u32,
) -> Outcome {
    start_a(
        env,
        dc.0,
        face,
        callback.addr(),
        param,
        FontEnumeration::Families,
    )
}
#[win32(gdi32, name = "EnumFontFamiliesW", level = implement)]
pub(super) fn families_w(
    env: &mut Env<'_>,
    dc: Handle,
    face: WideStr,
    callback: GuestPtr<()>,
    param: u32,
) -> Outcome {
    start_w(
        env,
        dc.0,
        face,
        callback.addr(),
        param,
        FontEnumeration::Families,
    )
}
#[win32(gdi32, name = "EnumFontFamiliesExA", level = implement)]
pub(super) fn families_ex_a(
    env: &mut Env<'_>,
    dc: Handle,
    logfont: GuestPtr<()>,
    callback: GuestPtr<()>,
    param: u32,
    _flags: u32,
) -> Outcome {
    let Ok(logical) = GuestPtr::<[u8; 60]>::new(logfont.addr()).read(env.cpu) else {
        set_last_error(env, 87);
        return Outcome::Return(0);
    };
    let face = {
        let face_bytes = &logical[28..];
        let len = face_bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(face_bytes.len());
        if len == 0 {
            None
        } else {
            Some(env.ext.get::<Fonts>().codec.decode(&face_bytes[..len]))
        }
    };
    start(
        env,
        dc.0,
        face,
        callback.addr(),
        param,
        FontEnumeration::FamiliesEx(decode_encoding(logical[23])),
        false,
    )
}
#[win32(gdi32, name = "EnumFontFamiliesExW", level = implement)]
pub(super) fn families_ex_w(
    env: &mut Env<'_>,
    dc: Handle,
    logfont: GuestPtr<()>,
    callback: GuestPtr<()>,
    param: u32,
    _flags: u32,
) -> Outcome {
    let Ok(logical) = GuestPtr::<[u8; 92]>::new(logfont.addr()).read(env.cpu) else {
        set_last_error(env, 87);
        return Outcome::Return(0);
    };
    let face = {
        let mut units = [0u16; 32];
        let bytes = &logical[28..];
        for i in 0..32 {
            units[i] = u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]);
        }
        let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        if len == 0 {
            None
        } else {
            let Ok(face) = String::from_utf16(&units[..len]) else {
                set_last_error(env, 87);
                return Outcome::Return(0);
            };
            Some(face)
        }
    };
    start(
        env,
        dc.0,
        face,
        callback.addr(),
        param,
        FontEnumeration::FamiliesEx(decode_encoding(logical[23])),
        true,
    )
}

#[win32(gdi32, name = "EnumFontsA", level = implement)]
pub(super) fn fonts_a(
    env: &mut Env<'_>,
    dc: Handle,
    face: AnsiStr,
    callback: GuestPtr<()>,
    param: u32,
) -> Outcome {
    start_a(
        env,
        dc.0,
        face,
        callback.addr(),
        param,
        FontEnumeration::Faces,
    )
}
#[win32(gdi32, name = "EnumFontsW", level = implement)]
pub(super) fn fonts_w(
    env: &mut Env<'_>,
    dc: Handle,
    face: WideStr,
    callback: GuestPtr<()>,
    param: u32,
) -> Outcome {
    start_w(
        env,
        dc.0,
        face,
        callback.addr(),
        param,
        FontEnumeration::Faces,
    )
}
pub(super) fn start_a(
    env: &mut Env<'_>,
    dc: u32,
    face: AnsiStr,
    callback: u32,
    param: u32,
    mode: FontEnumeration,
) -> Outcome {
    let face = if face.0.is_null() {
        None
    } else {
        let Ok(bytes) = face.read(env.cpu, 32768) else {
            set_last_error(env, 87);
            return Outcome::Return(0);
        };
        Some(env.ext.get::<Fonts>().codec.decode(&bytes))
    };
    start(env, dc, face, callback, param, mode, false)
}
pub(super) fn start_w(
    env: &mut Env<'_>,
    dc: u32,
    face: WideStr,
    callback: u32,
    param: u32,
    mode: FontEnumeration,
) -> Outcome {
    let face = if face.0.is_null() {
        None
    } else {
        let Ok(units) = face.read(env.cpu, 32768) else {
            set_last_error(env, 87);
            return Outcome::Return(0);
        };
        let Ok(face) = String::from_utf16(&units) else {
            set_last_error(env, 87);
            return Outcome::Return(0);
        };
        Some(face)
    };
    start(env, dc, face, callback, param, mode, true)
}
/// A face name fits LF_FACESIZE both in UTF-16 and in the guest's ANSI code page.
fn listed(codec: &adv_os::nls::Codec, face: &str) -> bool {
    face.encode_utf16().count() < 32 && codec.encode(face).len() < 32
}
const SCRATCH: u32 = 512;
const METRICS_OFFSET: u32 = 352;
struct Enumeration {
    fonts: VecDeque<EnumeratedFont>,
    callback: u32,
    param: u32,
    scratch: u32,
    wide: bool,
    mode: FontEnumeration,
}
pub(super) fn start(
    env: &mut Env<'_>,
    dc: u32,
    face: Option<String>,
    callback: u32,
    param: u32,
    mode: FontEnumeration,
    wide: bool,
) -> Outcome {
    if reject_dc(env, dc) {
        return Outcome::Return(0);
    }
    if callback == 0 || !env.cpu.accessible(callback, 1, Prot::EXEC) {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let language = match env.ext.get::<Fonts>().locale {
        0x804 => "zh-Hans",
        _ => "ja-JP",
    };
    let text = &mut env.ext.get_mut::<Fonts>().text;
    text.set_language(language);
    let api = match (mode, wide) {
        (FontEnumeration::Families, false) => "gdi32.dll!EnumFontFamiliesA",
        (FontEnumeration::Families, true) => "gdi32.dll!EnumFontFamiliesW",
        (FontEnumeration::Faces, false) => "gdi32.dll!EnumFontsA",
        (FontEnumeration::Faces, true) => "gdi32.dll!EnumFontsW",
        (FontEnumeration::FamiliesEx(_), false) => "gdi32.dll!EnumFontFamiliesExA",
        (FontEnumeration::FamiliesEx(_), true) => "gdi32.dll!EnumFontFamiliesExW",
    };
    let fonts = match text.enumerate(face.as_deref(), mode) {
        Ok(fonts) => fonts,
        Err(error) => {
            adv_abi::warn_unavailable(env, api, &error.to_string());
            set_last_error(
                env,
                if error == TextError::Unavailable {
                    120
                } else {
                    87
                },
            );
            return Outcome::Return(0);
        }
    };
    let mut fonts = fonts;
    if face.as_deref().is_none_or(str::is_empty) {
        // GDI names fit LF_FACESIZE even in the ANSI code page, and engines
        // copy them into fixed buffers. Longer host names stay selectable but
        // are not listed.
        let codec = &env.ext.get::<Fonts>().codec;
        fonts.retain(|font| listed(codec, &font.font.face));
    }
    if fonts.is_empty() {
        return Outcome::Return(1);
    }
    if fonts[0].substitute {
        adv_abi::warn_unavailable(
            env,
            api,
            &format!(
                "{:?} is not installed; enumerating its substitute {:?} under that name",
                face.as_deref().unwrap_or_default(),
                fonts[0].full_name
            ),
        );
    }
    let Some(scratch) = env
        .cpu
        .reg(Reg::Esp)
        .checked_sub(SCRATCH + 15)
        .map(|sp| sp & !15)
    else {
        set_last_error(env, 8);
        return Outcome::Return(0);
    };
    if scratch < env.ctx().stack_lo || !env.cpu.accessible(scratch, SCRATCH, Prot::RW) {
        set_last_error(env, 8);
        return Outcome::Return(0);
    }
    next(
        env,
        Enumeration {
            fonts: fonts.into(),
            callback,
            param,
            scratch,
            wide,
            mode,
        },
        1,
    )
}

fn next(env: &mut Env<'_>, mut enumeration: Enumeration, previous: u32) -> Outcome {
    let Some(font) = enumeration.fonts.pop_front() else {
        return Outcome::Return(previous);
    };
    let bytes = pack(env, &font, enumeration.wide, enumeration.mode);
    if let Err(error) = env.cpu.write(enumeration.scratch, &bytes) {
        return Outcome::Fail(error.to_string());
    }
    let kind = match font.kind {
        FontKind::Raster => 1,
        FontKind::Vector => 0,
        FontKind::TrueType => 4,
    } | if font.device { 2 } else { 0 };
    Outcome::call_on_stack(
        enumeration.callback,
        [
            enumeration.scratch,
            enumeration.scratch + METRICS_OFFSET,
            kind,
            enumeration.param,
        ],
        enumeration.scratch,
        move |env, result| {
            let value = result.value();
            if value == 0 {
                Outcome::Return(0)
            } else {
                next(env, enumeration, value)
            }
        },
    )
}
pub(super) fn put_string(codec: adv_os::nls::Codec, bytes: &mut [u8], text: &str, wide: bool) {
    let mut at = 0;
    for character in text.chars() {
        let encoded = if wide {
            character
                .encode_utf16(&mut [0; 2])
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect::<Vec<_>>()
        } else {
            codec.encode(&character.to_string())
        };
        if at + encoded.len() + (if wide { 2 } else { 1 }) > bytes.len() {
            break;
        }
        bytes[at..at + encoded.len()].copy_from_slice(&encoded);
        at += encoded.len();
    }
}
pub(super) fn pack_logfont(env: &Env<'_>, request: &FontRequest, wide: bool) -> [u8; 92] {
    let mut bytes = [0; 92];
    for (i, value) in [
        request.height,
        request.width,
        request.baseline_angle,
        request.glyph_angle,
        request.weight,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[20] = u8::from(request.italic);
    bytes[21] = u8::from(request.underline);
    bytes[22] = u8::from(request.strikeout);
    bytes[23] = encode_encoding(request.encoding);
    bytes[24] = match request.precision {
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
    };
    bytes[25] = match request.clipping.precision {
        ClipPrecision::Default => 0,
        ClipPrecision::Character => 1,
        ClipPrecision::Stroke => 2,
        ClipPrecision::Reserved(value) => value,
    } | (u8::from(request.clipping.transform_rotation) << 4)
        | (u8::from(request.clipping.force_outline) << 5)
        | (u8::from(request.clipping.disable_association) << 6)
        | (u8::from(request.clipping.embedded) << 7);
    bytes[26] = match request.quality {
        Quality::Default => 0,
        Quality::Draft => 1,
        Quality::Proof => 2,
        Quality::Monochrome => 3,
        Quality::Antialiased => 4,
        Quality::Subpixel => 5,
        Quality::SubpixelCompatible => 6,
        Quality::Reserved(value) => value,
    };
    bytes[27] = match request.pitch {
        Pitch::Default => 0,
        Pitch::Fixed => 1,
        Pitch::Variable => 2,
        Pitch::Reserved(value) => value,
    } | (encode_family(request.family) << 4);
    let codec = env.ext.get::<Fonts>().codec;
    let unit = if wide { 2 } else { 1 };
    put_string(codec, &mut bytes[28..28 + 32 * unit], &request.face, wide);
    bytes
}
pub(super) fn pack(
    env: &Env<'_>,
    font: &EnumeratedFont,
    wide: bool,
    mode: FontEnumeration,
) -> [u8; SCRATCH as usize] {
    let mut bytes = [0; SCRATCH as usize];
    let logical = pack_logfont(env, &font.font, wide);
    let codec = env.ext.get::<Fonts>().codec;
    let unit = if wide { 2 } else { 1 };
    bytes[..28 + 32 * unit].copy_from_slice(&logical[..28 + 32 * unit]);
    if mode != FontEnumeration::Faces {
        put_string(
            codec,
            &mut bytes[28 + 32 * unit..28 + 96 * unit],
            &font.full_name,
            wide,
        );
        put_string(
            codec,
            &mut bytes[28 + 96 * unit..28 + 128 * unit],
            &font.style,
            wide,
        );
        if matches!(mode, FontEnumeration::FamiliesEx(_)) {
            put_string(
                codec,
                &mut bytes[28 + 128 * unit..28 + 160 * unit],
                &font.script,
                wide,
            );
        }
    }
    let metrics = METRICS_OFFSET as usize;
    let packed = pack_metrics(&font.metrics);
    bytes[metrics..metrics + 44].copy_from_slice(&packed[..44]);
    if wide {
        for (i, character) in font.characters.iter().enumerate() {
            bytes[metrics + 44 + i * 2..metrics + 46 + i * 2]
                .copy_from_slice(&character.to_le_bytes());
        }
        bytes[metrics + 52..metrics + 57].copy_from_slice(&packed[48..53]);
    } else {
        for (i, character) in font.characters.iter().enumerate() {
            bytes[metrics + 44 + i] = codec
                .from_utf16(codec.acp, &[*character])
                .0
                .first()
                .copied()
                .unwrap_or(b'?');
        }
        bytes[metrics + 48..metrics + 53].copy_from_slice(&packed[48..53]);
    }
    if let Some(design) = &font.design {
        let offset = metrics + if wide { 60 } else { 56 };
        let flags = u32::from(design.italic)
            | (u32::from(design.bold) << 5)
            | (u32::from(design.regular) << 6)
            | (u32::from(design.nonnegative_spacing) << 16)
            | (u32::from(design.postscript_outline) << 17)
            | (u32::from(design.truetype_outline) << 18)
            | (u32::from(design.multiple_master) << 19)
            | (u32::from(design.type1) << 20)
            | (u32::from(design.digital_signature) << 21);
        for (i, value) in [
            flags,
            design.em_units,
            design.cell_height,
            design.average_width,
        ]
        .into_iter()
        .enumerate()
        {
            bytes[offset + i * 4..offset + i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    if matches!(mode, FontEnumeration::FamiliesEx(_))
        && let Some(signature) = font.signature
    {
        let offset = metrics + if wide { 76 } else { 72 };
        for (i, value) in signature
            .unicode
            .into_iter()
            .chain(signature.codepages)
            .enumerate()
        {
            bytes[offset + i * 4..offset + i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    bytes
}

pub(super) fn driver(env: &mut Env<'_>, _dc: u32, a: &[u32]) -> Outcome {
    let mut raw = [0; 92];
    if env.cpu.read(a[1], &mut raw).is_err() {
        return Outcome::Return(0);
    }
    let face = String::from_utf16_lossy(
        &raw[28..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|u| u16::from_le_bytes(*u))
            .take_while(|&u| u != 0)
            .collect::<Vec<_>>(),
    );
    let mode = FontEnumeration::FamiliesEx(decode_encoding(raw[23]));
    let language = if env.ext.get::<Fonts>().locale == 0x804 {
        "zh-Hans"
    } else {
        "ja-JP"
    };
    let text = &mut env.ext.get_mut::<Fonts>().text;
    text.set_language(language);
    let Ok(fonts) = text.enumerate((!face.is_empty()).then_some(face.as_str()), mode) else {
        return Outcome::Return(0);
    };
    let allocator = super::super::allocator();
    let Some(scratch) = (allocator.allocate)(env, SCRATCH) else {
        return Outcome::Return(0);
    };
    driver_next(env, fonts.into(), a[2], a[3], scratch, mode)
}
fn driver_next(
    env: &mut Env<'_>,
    mut fonts: VecDeque<EnumeratedFont>,
    callback: u32,
    param: u32,
    scratch: u32,
    mode: FontEnumeration,
) -> Outcome {
    let Some(font) = fonts.pop_front() else {
        (super::super::allocator().free)(env, scratch);
        return Outcome::Return(1);
    };
    let bytes = pack(env, &font, true, mode);
    env.cpu.write(scratch, &bytes).unwrap();
    Outcome::call_aligned(
        env,
        callback,
        [scratch, scratch + METRICS_OFFSET, 4, param],
        move |env, result| {
            if result.value() == 0 {
                (super::super::allocator().free)(env, scratch);
                Outcome::Return(0)
            } else {
                driver_next(env, fonts, callback, param, scratch, mode)
            }
        },
    )
}

#[cfg(test)]
mod tests;
