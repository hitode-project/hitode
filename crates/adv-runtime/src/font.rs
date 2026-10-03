//! Guest font selection and Windows GDI glyph extraction for software DCs.
use crate::{Graphics, Snapshot};
use adv_machine::{Conv, Env, Handler, Outcome, Registry};
use adv_win32::{
    Win32,
    gdi32::{GdiObj, LogFont},
    util::set_last_error,
};
use std::collections::BTreeMap;

const STOCK_FONT: u32 = 0x0190_0010 + 13;
const GDI_ERROR: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct DcState {
    font: u32,
    map_mode: u32,
    poly_fill: u32,
}
impl Default for DcState {
    fn default() -> Self {
        Self {
            font: STOCK_FONT,
            map_mode: 1,
            poly_fill: 1,
        }
    }
}

pub(crate) struct Fonts {
    dcs: BTreeMap<u32, DcState>,
    select: Handler,
    delete: Handler,
    delete_dc: Handler,
    #[cfg(windows)]
    native: BTreeMap<FontSpec, NativeFont>,
    #[cfg(windows)]
    glyphs: BTreeMap<GlyphKey, std::sync::Arc<Glyph>>,
    #[cfg(windows)]
    glyph_bytes: usize,
    #[cfg(windows)]
    glyph_hits: u64,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FontSpec {
    raw: [u8; 28],
    face: String,
}
impl FontSpec {
    fn from_font(font: &LogFont) -> Self {
        Self {
            raw: font.raw[..28].try_into().unwrap(),
            face: font.face.clone(),
        }
    }
    fn stock() -> Self {
        let mut raw = [0; 28];
        raw[..4].copy_from_slice(&16i32.to_le_bytes());
        raw[16..20].copy_from_slice(&400i32.to_le_bytes());
        Self {
            raw,
            face: "Segoe UI".into(),
        }
    }
}

pub(crate) fn register(registry: &mut Registry) {
    for (name, argc) in [
        ("GetGlyphOutlineA", 7),
        ("GetGlyphOutlineW", 7),
        ("GetTextMetricsA", 2),
        ("SetMapMode", 2),
        ("GetMapMode", 1),
        ("SetPolyFillMode", 2),
        ("GetPolyFillMode", 1),
    ] {
        registry.add("gdi32.dll", name, Conv::Stdcall { argc }, font_call);
    }
    registry.add(
        "gdi32.dll",
        "SelectObject",
        Conv::Stdcall { argc: 2 },
        select_object,
    );
    registry.add(
        "gdi32.dll",
        "DeleteObject",
        Conv::Stdcall { argc: 1 },
        delete_object,
    );
    registry.add(
        "gdi32.dll",
        "DeleteDC",
        Conv::Stdcall { argc: 1 },
        delete_dc,
    );
}

pub(crate) fn install<B: pal_cpu::Backend>(machine: &mut adv_machine::Machine<B>) {
    let mut original = Registry::default();
    adv_win32::register(&mut original);
    machine.ext.insert(Fonts {
        dcs: BTreeMap::new(),
        select: original.get("gdi32.dll", "SelectObject").unwrap().handler,
        delete: original.get("gdi32.dll", "DeleteObject").unwrap().handler,
        delete_dc: original.get("gdi32.dll", "DeleteDC").unwrap().handler,
        #[cfg(windows)]
        native: BTreeMap::new(),
        #[cfg(windows)]
        glyphs: BTreeMap::new(),
        #[cfg(windows)]
        glyph_bytes: 0,
        #[cfg(windows)]
        glyph_hits: 0,
    });
}

fn valid_dc(env: &Env<'_>, dc: u32) -> bool {
    env.ext.get::<Graphics<Snapshot>>().owns_dc(dc)
        || matches!(env.ext.get::<Win32>().gdi.get(dc), Some(GdiObj::Dc { .. }))
}

pub(crate) fn retire_dc(env: &mut Env<'_>, dc: u32) {
    if let Some(fonts) = env.ext.try_get_mut::<Fonts>() {
        fonts.dcs.remove(&dc);
    }
}
fn select_object(env: &mut Env<'_>) -> Outcome {
    let (dc, object) = (env.arg(0), env.arg(1));
    let is_font = matches!(
        env.ext.get::<Win32>().gdi.get(object),
        Some(GdiObj::Font(_))
    ) || matches!(object, 0x0190_001a..=0x0190_001e | 0x0190_0020..=0x0190_0021);
    if is_font && valid_dc(env, dc) {
        if env.ext.get::<Graphics<Snapshot>>().owns_dc(dc) {
            env.ext.get_mut::<Win32>().gdi.register_external_dc(dc, 0);
        }
        let handler = env.ext.get::<Fonts>().select;
        let result = handler(env);
        if matches!(result, Outcome::Return(old) if old != 0) {
            env.ext.get_mut::<Fonts>().dcs.entry(dc).or_default().font = object;
        }
        return result;
    }
    if env.ext.get::<Graphics<Snapshot>>().owns_dc(dc) {
        crate::boot::graphics_call(env)
    } else {
        let handler = env.ext.get::<Fonts>().select;
        handler(env)
    }
}
fn delete_object(env: &mut Env<'_>) -> Outcome {
    let object = env.arg(0);
    if env.ext.get::<Graphics<Snapshot>>().owns_bitmap(object) {
        return crate::boot::graphics_call(env);
    }
    if env
        .ext
        .get::<Fonts>()
        .dcs
        .values()
        .any(|dc| dc.font == object)
    {
        set_last_error(env, 170);
        return Outcome::Return(0);
    }
    let handler = env.ext.get::<Fonts>().delete;
    handler(env)
}
fn delete_dc(env: &mut Env<'_>) -> Outcome {
    let dc = env.arg(0);
    let result = if env.ext.get::<Graphics<Snapshot>>().owns_dc(dc) {
        crate::boot::graphics_call(env)
    } else {
        let handler = env.ext.get::<Fonts>().delete_dc;
        handler(env)
    };
    if matches!(result, Outcome::Return(1)) {
        retire_dc(env, dc);
    }
    result
}

fn font_call(env: &mut Env<'_>) -> Outcome {
    let name = env.sys.thunks.get(env.thunk).name.clone();
    let dc = env.arg(0);
    if !valid_dc(env, dc) {
        set_last_error(env, 6);
        return Outcome::Return(if name.starts_with("GetGlyph") {
            GDI_ERROR
        } else {
            0
        });
    }
    if matches!(
        name.as_str(),
        "SetMapMode" | "GetMapMode" | "SetPolyFillMode" | "GetPolyFillMode"
    ) {
        let value = env.arg(1);
        if name == "SetMapMode" && value != 1 {
            set_last_error(env, if (2..=8).contains(&value) { 120 } else { 87 });
            return Outcome::Return(0);
        }
        if name == "SetPolyFillMode" && !matches!(value, 1 | 2) {
            set_last_error(env, 87);
            return Outcome::Return(0);
        }
        let state = env.ext.get_mut::<Fonts>().dcs.entry(dc).or_default();
        return Outcome::Return(match name.as_str() {
            "SetMapMode" => std::mem::replace(&mut state.map_mode, value),
            "GetMapMode" => state.map_mode,
            "SetPolyFillMode" => std::mem::replace(&mut state.poly_fill, value),
            "GetPolyFillMode" => state.poly_fill,
            _ => unreachable!(),
        });
    }
    let font = env
        .ext
        .get::<Fonts>()
        .dcs
        .get(&dc)
        .map(|s| s.font)
        .or_else(|| match env.ext.get::<Win32>().gdi.get(dc) {
            Some(GdiObj::Dc { selected, .. }) => Some(selected[2]),
            _ => None,
        })
        .unwrap_or(STOCK_FONT);
    let spec = match env.ext.get::<Win32>().gdi.get(font) {
        Some(GdiObj::Font(font)) => FontSpec::from_font(font),
        _ => FontSpec::stock(),
    };
    #[cfg(not(windows))]
    {
        let _ = spec;
        return Outcome::Fail(
            "Font metrics and glyphs require a Windows GDI backend on this host".into(),
        );
    }
    #[cfg(windows)]
    {
        let args: [u32; 7] = std::array::from_fn(|i| env.arg(i as u32));
        let fonts = env.ext.get_mut::<Fonts>();
        if !fonts.native.contains_key(&spec) {
            match NativeFont::new(&spec) {
                Ok(native) => {
                    fonts.native.insert(spec.clone(), native);
                }
                Err(_) => {
                    set_last_error(env, 87);
                    return Outcome::Return(if name == "GetTextMetricsA" {
                        0
                    } else {
                        GDI_ERROR
                    });
                }
            }
        }
        if name == "GetTextMetricsA" {
            let result = fonts.native[&spec].text_metrics();
            return match result {
                Ok(bytes) if env.cpu.write(args[1], &bytes).is_ok() => Outcome::Return(1),
                _ => {
                    set_last_error(env, 87);
                    Outcome::Return(0)
                }
            };
        }
        let format = args[2];
        if format & !0x1ff != 0 || !matches!(format & 0x7f, 0..=6) {
            set_last_error(env, 87);
            return Outcome::Return(GDI_ERROR);
        }
        let mut matrix = [0u8; 16];
        if args[6] == 0 || env.cpu.read(args[6], &mut matrix).is_err() {
            set_last_error(env, 87);
            return Outcome::Return(GDI_ERROR);
        }
        // The ANSI entry point performs the selected font's charset conversion,
        // including unmapped character handling, rather than the process ACP.
        let key = GlyphKey {
            font: spec.clone(),
            code: args[1],
            format,
            matrix,
            ansi: name.ends_with('A'),
        };
        let fonts = env.ext.get_mut::<Fonts>();
        // This cache is below the guest API thunk, so guest IAT/inline hooks still
        // execute. Cache immutable host results, never guest output pointers.
        let glyph = if let Some(glyph) = fonts.glyphs.get(&key) {
            fonts.glyph_hits += 1;
            Ok(std::sync::Arc::clone(glyph))
        } else {
            fonts.native[&spec]
                .glyph(key.code, format, &matrix, u32::MAX, true, key.ansi)
                .map(|glyph| {
                    let glyph = std::sync::Arc::new(glyph);
                    const CACHE_BYTES: usize = 8 * 1024 * 1024;
                    if glyph.bytes.len() <= CACHE_BYTES {
                        if fonts.glyphs.len() >= 4096
                            || fonts.glyph_bytes + glyph.bytes.len() > CACHE_BYTES
                        {
                            fonts.glyphs.clear();
                            fonts.glyph_bytes = 0;
                        }
                        fonts.glyph_bytes += glyph.bytes.len();
                        fonts.glyphs.insert(key, std::sync::Arc::clone(&glyph));
                    }
                    glyph
                })
        };
        match glyph {
            Ok(glyph) => {
                let fetch = args[5] != 0 && args[4] != 0 && format & 0x7f != 0;
                if fetch && args[4] < glyph.size {
                    set_last_error(env, 87);
                    return Outcome::Return(GDI_ERROR);
                }
                if env.cpu.write(args[3], &glyph.metrics).is_err()
                    || (fetch
                        && !glyph.bytes.is_empty()
                        && env.cpu.write(args[5], &glyph.bytes).is_err())
                {
                    set_last_error(env, 87);
                    return Outcome::Return(GDI_ERROR);
                }
                Outcome::Return(glyph.size)
            }
            Err(_) => {
                set_last_error(env, 87);
                Outcome::Return(GDI_ERROR)
            }
        }
    }
}

#[cfg(windows)]
use windows_sys::Win32::Graphics::Gdi::{self as gdi, HDC, HFONT, HGDIOBJ};
#[cfg(windows)]
struct NativeFont {
    dc: HDC,
    font: HFONT,
    old: HGDIOBJ,
}
#[cfg(windows)]
struct Glyph {
    metrics: [u8; 20],
    bytes: Vec<u8>,
    size: u32,
}

#[cfg(windows)]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GlyphKey {
    font: FontSpec,
    code: u32,
    format: u32,
    matrix: [u8; 16],
    ansi: bool,
}

#[cfg(windows)]
impl NativeFont {
    fn new(spec: &FontSpec) -> Result<Self, String> {
        let raw = &spec.raw;
        let integer = |offset| i32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap());
        if integer(0).unsigned_abs() > 16384 || integer(4).unsigned_abs() > 16384 {
            return Err("invalid font dimensions".into());
        }
        let mut logical = gdi::LOGFONTW {
            lfHeight: integer(0),
            lfWidth: integer(4),
            lfEscapement: integer(8),
            lfOrientation: integer(12),
            lfWeight: integer(16),
            lfItalic: raw[20],
            lfUnderline: raw[21],
            lfStrikeOut: raw[22],
            lfCharSet: raw[23],
            lfOutPrecision: raw[24],
            lfClipPrecision: raw[25],
            lfQuality: raw[26],
            lfPitchAndFamily: raw[27],
            ..Default::default()
        };
        for (slot, value) in logical
            .lfFaceName
            .iter_mut()
            .take(31)
            .zip(spec.face.encode_utf16())
        {
            *slot = value;
        }
        // SAFETY: all GDI handles are owned here and all pointers refer to valid host objects.
        unsafe {
            let dc = gdi::CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return Err("CreateCompatibleDC failed".into());
            }
            let font = gdi::CreateFontIndirectW(&logical);
            if font.is_null() {
                gdi::DeleteDC(dc);
                return Err("CreateFontIndirectW failed".into());
            }
            let old = gdi::SelectObject(dc, font);
            if old.is_null() || old as usize == usize::MAX {
                gdi::DeleteObject(font);
                gdi::DeleteDC(dc);
                return Err("SelectObject failed".into());
            }
            Ok(Self { dc, font, old })
        }
    }
    fn text_metrics(&self) -> Result<[u8; 56], String> {
        let mut metrics = gdi::TEXTMETRICA::default();
        // SAFETY: the DC is owned by this font and the output is a valid host struct.
        if unsafe { gdi::GetTextMetricsA(self.dc, &mut metrics) } == 0 {
            return Err("GetTextMetricsA failed".into());
        }
        let mut packed = [0; 56];
        for (i, value) in [
            metrics.tmHeight,
            metrics.tmAscent,
            metrics.tmDescent,
            metrics.tmInternalLeading,
            metrics.tmExternalLeading,
            metrics.tmAveCharWidth,
            metrics.tmMaxCharWidth,
            metrics.tmWeight,
            metrics.tmOverhang,
            metrics.tmDigitizedAspectX,
            metrics.tmDigitizedAspectY,
        ]
        .into_iter()
        .enumerate()
        {
            packed[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        packed[44..53].copy_from_slice(&[
            metrics.tmFirstChar,
            metrics.tmLastChar,
            metrics.tmDefaultChar,
            metrics.tmBreakChar,
            metrics.tmItalic,
            metrics.tmUnderlined,
            metrics.tmStruckOut,
            metrics.tmPitchAndFamily,
            metrics.tmCharSet,
        ]);
        Ok(packed)
    }
    fn glyph(
        &self,
        code: u32,
        format: u32,
        matrix: &[u8; 16],
        capacity: u32,
        fetch: bool,
        ansi: bool,
    ) -> Result<Glyph, String> {
        let fixed = |offset| gdi::FIXED {
            fract: u16::from_le_bytes(matrix[offset..offset + 2].try_into().unwrap()),
            value: i16::from_le_bytes(matrix[offset + 2..offset + 4].try_into().unwrap()),
        };
        let transform = gdi::MAT2 {
            eM11: fixed(0),
            eM12: fixed(4),
            eM21: fixed(8),
            eM22: fixed(12),
        };
        let mut metrics = gdi::GLYPHMETRICS::default();
        let get_outline = if ansi {
            gdi::GetGlyphOutlineA
        } else {
            gdi::GetGlyphOutlineW
        };
        // SAFETY: transform/metrics are valid host structs; the output buffer is sized by GDI's query.
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
        if size == GDI_ERROR || size > 16 * 1024 * 1024 {
            return Err("GetGlyphOutline failed".into());
        }
        let mut bytes = Vec::new();
        if fetch && capacity != 0 && format & 0x7f != 0 {
            if capacity < size {
                return Err("glyph buffer is too small".into());
            }
            bytes.resize(size as usize, 0);
            let result = unsafe {
                get_outline(
                    self.dc,
                    code,
                    format,
                    &mut metrics,
                    size,
                    bytes.as_mut_ptr().cast(),
                    &transform,
                )
            };
            if result == GDI_ERROR {
                return Err("GetGlyphOutline bitmap failed".into());
            }
        }
        let mut packed = [0; 20];
        for (offset, value) in [
            (0, metrics.gmBlackBoxX),
            (4, metrics.gmBlackBoxY),
            (8, metrics.gmptGlyphOrigin.x as u32),
            (12, metrics.gmptGlyphOrigin.y as u32),
        ] {
            packed[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        packed[16..18].copy_from_slice(&metrics.gmCellIncX.to_le_bytes());
        packed[18..20].copy_from_slice(&metrics.gmCellIncY.to_le_bytes());
        Ok(Glyph {
            metrics: packed,
            bytes,
            size,
        })
    }
}
#[cfg(windows)]
impl Drop for NativeFont {
    fn drop(&mut self) {
        // SAFETY: restore the previous selection before releasing the owned font and DC.
        unsafe {
            gdi::SelectObject(self.dc, self.old);
            gdi::DeleteObject(self.font);
            gdi::DeleteDC(self.dc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_jit::pal::JitCpu;
    use adv_machine::{Clock, Machine};
    use pal_cpu::{Guest, GuestExt, Prot, Reg};

    fn fixture() -> (Machine<JitCpu>, u32, u32) {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        crate::boot::register_graphics(&mut registry);
        register(&mut registry);
        let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        adv_win32::Win32::install(
            &mut machine,
            adv_win32::Win32Config::new(".", "font-test.exe", "target/font-test-overlay"),
        )
        .unwrap();
        crate::boot::install_graphics(&mut machine, 4, 2).unwrap();
        install(&mut machine);
        let data = machine
            .sys
            .vm
            .alloc(&mut machine.cpu, 4096, Prot::RW)
            .unwrap();
        machine
            .spawn("font test", adv_machine::layout::PARK_ADDR, &[], 65536)
            .unwrap();
        let dc = invoke(&mut machine, data, "CreateCompatibleDC", &[0]);
        let mut raw = [0u8; 60];
        raw[..4].copy_from_slice(&(-24i32).to_le_bytes());
        raw[16..20].copy_from_slice(&400i32.to_le_bytes());
        raw[28..34].copy_from_slice(b"Arial\0");
        machine.cpu.write(data, &raw).unwrap();
        let font = invoke(&mut machine, data, "CreateFontIndirectA", &[data]);
        assert_ne!(font, 0);
        assert_eq!(
            invoke(&mut machine, data, "SelectObject", &[dc, font]),
            STOCK_FONT
        );
        (machine, data, dc)
    }
    fn invoke(machine: &mut Machine<JitCpu>, data: u32, name: &str, args: &[u32]) -> u32 {
        let address = machine.resolve_import("gdi32.dll", name).unwrap();
        let index = (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
        let handler = machine.sys.thunks.get(index).f.handler;
        machine.cpu.set_reg(Reg::Esp, data + 2048);
        for (i, &arg) in args.iter().enumerate() {
            machine
                .cpu
                .write_u32(data + 2052 + i as u32 * 4, arg)
                .unwrap();
        }
        let result = handler(&mut Env {
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: index,
        });
        let Outcome::Return(value) = result else {
            panic!("font call must return")
        };
        value
    }
    #[test]
    fn software_dc_preserves_font_and_mapping_state() {
        let (mut machine, data, dc) = fixture();
        assert_eq!(invoke(&mut machine, data, "SetMapMode", &[dc, 1]), 1);
        assert_eq!(invoke(&mut machine, data, "SetMapMode", &[dc, 2]), 0);
        assert_eq!(invoke(&mut machine, data, "GetMapMode", &[dc]), 1);
        assert_eq!(invoke(&mut machine, data, "SetPolyFillMode", &[dc, 2]), 1);
        assert_eq!(invoke(&mut machine, data, "GetPolyFillMode", &[dc]), 2);
        assert_eq!(invoke(&mut machine, data, "SetPolyFillMode", &[dc, 0]), 0);
        let font = machine.ext.get::<Fonts>().dcs[&dc].font;
        assert_eq!(invoke(&mut machine, data, "DeleteObject", &[font]), 0);
        assert_eq!(
            invoke(&mut machine, data, "SelectObject", &[dc, STOCK_FONT]),
            font
        );
        assert_eq!(invoke(&mut machine, data, "DeleteObject", &[font]), 1);
        assert!(machine.ext.get::<Win32>().gdi.get(font).is_none());
        assert_eq!(invoke(&mut machine, data, "DeleteDC", &[dc]), 1);
        assert_eq!(invoke(&mut machine, data, "GetMapMode", &[dc]), 0);
    }
    #[test]
    #[cfg(windows)]
    fn glyph_metrics_bitmap_gray8_and_index_use_real_font_pixels() {
        let (mut machine, data, dc) = fixture();
        let matrix = data + 128;
        let metrics = data + 160;
        let buffer = data + 256;
        let mut identity = [0u8; 16];
        identity[2..4].copy_from_slice(&1i16.to_le_bytes());
        identity[14..16].copy_from_slice(&1i16.to_le_bytes());
        machine.cpu.write(matrix, &identity).unwrap();
        for format in [0u32, 1, 6] {
            let size = invoke(
                &mut machine,
                data,
                "GetGlyphOutlineA",
                &[dc, u32::from(b'A'), format, metrics, 0, 0, matrix],
            );
            assert_ne!(size, GDI_ERROR);
            let width = machine.cpu.read_u32(metrics).unwrap();
            let height = machine.cpu.read_u32(metrics + 4).unwrap();
            assert!(width > 0 && height > 0);
            if format == 0 {
                // GDI returns its glyph data size even when only metrics are requested.
                assert!(size > 0);
                continue;
            }
            let pitch = if format == 1 {
                width.div_ceil(32) * 4
            } else {
                (width + 3) & !3
            };
            assert_eq!(size, pitch * height);
            assert_eq!(
                invoke(
                    &mut machine,
                    data,
                    "GetGlyphOutlineA",
                    &[dc, u32::from(b'A'), format, metrics, size, buffer, matrix]
                ),
                size
            );
            let mut bytes = vec![0; size as usize];
            machine.cpu.read(buffer, &mut bytes).unwrap();
            assert!(bytes.iter().any(|&b| b != 0));
            let hits = machine.ext.get::<Fonts>().glyph_hits;
            machine
                .cpu
                .write(buffer, &vec![0xa5; size as usize])
                .unwrap();
            // A cached size query must not write the caller's zero-capacity buffer.
            assert_eq!(
                invoke(
                    &mut machine,
                    data,
                    "GetGlyphOutlineA",
                    &[dc, u32::from(b'A'), format, metrics, 0, buffer, matrix]
                ),
                size
            );
            let mut queried = vec![0; size as usize];
            machine.cpu.read(buffer, &mut queried).unwrap();
            assert!(queried.iter().all(|&byte| byte == 0xa5));
            assert_eq!(
                invoke(
                    &mut machine,
                    data,
                    "GetGlyphOutlineA",
                    &[dc, u32::from(b'A'), format, metrics, size, buffer, matrix]
                ),
                size
            );
            machine.cpu.read(buffer, &mut queried).unwrap();
            assert_eq!(queried, bytes);
            assert_eq!(machine.ext.get::<Fonts>().glyph_hits, hits + 2);
            if format == 6 {
                assert!(bytes.iter().all(|&b| b <= 64));
            }
            assert_eq!(
                invoke(
                    &mut machine,
                    data,
                    "GetGlyphOutlineA",
                    &[
                        dc,
                        u32::from(b'A'),
                        format,
                        metrics,
                        size - 1,
                        buffer,
                        matrix
                    ]
                ),
                GDI_ERROR
            );
        }
        let font = machine.ext.get::<Fonts>().dcs[&dc].font;
        let spec = match machine.ext.get::<Win32>().gdi.get(font) {
            Some(GdiObj::Font(font)) => FontSpec::from_font(font),
            _ => panic!("font"),
        };
        let native = &machine.ext.get::<Fonts>().native[&spec];
        let mut index = 0u16;
        // SAFETY: an owned native DC, a single UTF-16 character and one output glyph index.
        assert_eq!(
            unsafe { gdi::GetGlyphIndicesW(native.dc, &u16::from(b'A'), 1, &mut index, 0) },
            1
        );
        assert_ne!(
            invoke(
                &mut machine,
                data,
                "GetGlyphOutlineA",
                &[dc, u32::from(index), 6 | 0x80, metrics, 0, 0, matrix]
            ),
            GDI_ERROR
        );
    }

    #[test]
    #[cfg(windows)]
    fn glyph_cache_separates_transform_encoding_and_selected_font() {
        let (mut machine, data, dc) = fixture();
        let matrix = data + 128;
        let mut transform = [0u8; 16];
        transform[2..4].copy_from_slice(&1i16.to_le_bytes());
        transform[14..16].copy_from_slice(&1i16.to_le_bytes());
        machine.cpu.write(matrix, &transform).unwrap();
        let query = |machine: &mut Machine<JitCpu>, name: &str| {
            invoke(
                machine,
                data,
                name,
                &[dc, u32::from(b'A'), 6, data + 160, 0, 0, matrix],
            )
        };
        let original = query(&mut machine, "GetGlyphOutlineW");
        assert_ne!(original, GDI_ERROR);
        assert_eq!(query(&mut machine, "GetGlyphOutlineW"), original);
        assert_eq!(machine.ext.get::<Fonts>().glyph_hits, 1);
        assert_eq!(query(&mut machine, "GetGlyphOutlineA"), original);
        assert_eq!(machine.ext.get::<Fonts>().glyphs.len(), 2);
        transform[2..4].copy_from_slice(&2i16.to_le_bytes());
        machine.cpu.write(matrix, &transform).unwrap();
        assert!(query(&mut machine, "GetGlyphOutlineW") > original);
        assert_eq!(machine.ext.get::<Fonts>().glyphs.len(), 3);
        invoke(&mut machine, data, "SelectObject", &[dc, STOCK_FONT]);
        assert_ne!(query(&mut machine, "GetGlyphOutlineW"), GDI_ERROR);
        assert_eq!(machine.ext.get::<Fonts>().glyphs.len(), 4);
        assert_eq!(machine.ext.get::<Fonts>().glyph_hits, 1);
    }

    #[test]
    #[cfg(windows)]
    fn gbk_packed_characters_match_unicode_glyph_pixels() {
        let (mut machine, data, dc) = fixture();
        let win32 = machine.ext.get_mut::<Win32>();
        // The selected GB2312 font overrides a different process ACP.
        win32.cfg.codepage = 932;
        win32.codec = adv_win32::Codec::new(932);
        let mut raw = [0u8; 60];
        raw[..4].copy_from_slice(&(-24i32).to_le_bytes());
        raw[16..20].copy_from_slice(&400i32.to_le_bytes());
        raw[23] = 134; // GB2312_CHARSET
        raw[28..35].copy_from_slice(b"SimSun\0");
        machine.cpu.write(data, &raw).unwrap();
        let font = invoke(&mut machine, data, "CreateFontIndirectA", &[data]);
        assert_ne!(font, 0);
        invoke(&mut machine, data, "SelectObject", &[dc, font]);
        let matrix = data + 128;
        let metrics = data + 160;
        let buffer = data + 256;
        let mut identity = [0u8; 16];
        identity[2..4].copy_from_slice(&1i16.to_le_bytes());
        identity[14..16].copy_from_slice(&1i16.to_le_bytes());
        machine.cpu.write(matrix, &identity).unwrap();
        for character in ['中', '文'] {
            let codec = adv_win32::Codec::new(936);
            let (encoded, lossy) = codec.from_utf16(0, &[character as u16]);
            assert!(!lossy);
            assert_eq!(encoded.len(), 2);
            let packed = (u32::from(encoded[0]) << 8) | u32::from(encoded[1]);
            let count = invoke(
                &mut machine,
                data,
                "GetGlyphOutlineA",
                &[dc, packed, 6, metrics, 0, 0, matrix],
            );
            assert_ne!(count, GDI_ERROR);
            assert!(count > 0);
            assert_eq!(
                invoke(
                    &mut machine,
                    data,
                    "GetGlyphOutlineA",
                    &[dc, packed, 6, metrics, count, buffer, matrix]
                ),
                count
            );
            let mut ansi = vec![0; count as usize];
            machine.cpu.read(buffer, &mut ansi).unwrap();
            assert_eq!(
                invoke(
                    &mut machine,
                    data,
                    "GetGlyphOutlineW",
                    &[dc, character as u32, 6, metrics, count, buffer, matrix]
                ),
                count
            );
            let mut unicode = vec![0; count as usize];
            machine.cpu.read(buffer, &mut unicode).unwrap();
            assert_eq!(ansi, unicode);
            assert!(ansi.iter().any(|&alpha| alpha > 0));
            assert!(ansi.iter().all(|&alpha| alpha <= 64));
        }
    }

    #[test]
    #[cfg(windows)]
    fn selected_charset_and_metrics_match_native_gdi_across_process_acp() {
        let (mut machine, data, dc) = fixture();
        let win32 = machine.ext.get_mut::<Win32>();
        win32.cfg.codepage = 936;
        win32.codec = adv_win32::Codec::new(936);
        let mut raw = [0u8; 60];
        raw[..4].copy_from_slice(&(-24i32).to_le_bytes());
        raw[16..20].copy_from_slice(&400i32.to_le_bytes());
        raw[23] = 128; // SHIFTJIS_CHARSET, even though ACP is GBK.
        raw[28..38].copy_from_slice(b"MS Gothic\0");
        machine.cpu.write(data, &raw).unwrap();
        let font = invoke(&mut machine, data, "CreateFontIndirectA", &[data]);
        invoke(&mut machine, data, "SelectObject", &[dc, font]);
        let matrix = data + 128;
        let metrics = data + 160;
        let buffer = data + 256;
        let mut identity = [0u8; 16];
        identity[2..4].copy_from_slice(&1i16.to_le_bytes());
        identity[14..16].copy_from_slice(&1i16.to_le_bytes());
        machine.cpu.write(matrix, &identity).unwrap();
        for character in [0xad, 0x82a0, 0x00ff_ecff] {
            let size = invoke(
                &mut machine,
                data,
                "GetGlyphOutlineA",
                &[dc, character, 6, metrics, 0, 0, matrix],
            );
            assert_ne!(size, GDI_ERROR);
            let spec = match machine.ext.get::<Win32>().gdi.get(font) {
                Some(GdiObj::Font(font)) => FontSpec::from_font(font),
                _ => panic!("font"),
            };
            let native = &machine.ext.get::<Fonts>().native[&spec];
            let mut native_metrics = gdi::GLYPHMETRICS::default();
            let transform = gdi::MAT2 {
                eM11: gdi::FIXED { value: 1, fract: 0 },
                eM22: gdi::FIXED { value: 1, fract: 0 },
                ..Default::default()
            };
            // SAFETY: an owned native DC, initialized metrics/transform and a null query buffer.
            let expected = unsafe {
                gdi::GetGlyphOutlineA(
                    native.dc,
                    character,
                    6,
                    &mut native_metrics,
                    0,
                    std::ptr::null_mut(),
                    &transform,
                )
            };
            assert_eq!(size, expected);
            assert_eq!(
                machine.cpu.read_u32(metrics).unwrap(),
                native_metrics.gmBlackBoxX
            );
            assert_eq!(
                machine.cpu.read_u32(metrics + 4).unwrap(),
                native_metrics.gmBlackBoxY
            );
            assert_eq!(
                invoke(
                    &mut machine,
                    data,
                    "GetGlyphOutlineA",
                    &[dc, character, 6, metrics, size, buffer, matrix]
                ),
                size
            );
        }
        assert_eq!(
            invoke(&mut machine, data, "GetTextMetricsA", &[dc, metrics]),
            1
        );
        let height = machine.cpu.read_u32(metrics).unwrap();
        let ascent = machine.cpu.read_u32(metrics + 4).unwrap();
        let descent = machine.cpu.read_u32(metrics + 8).unwrap();
        assert_eq!(height, ascent + descent);
        assert!(height >= 24);
        let spec = match machine.ext.get::<Win32>().gdi.get(font) {
            Some(GdiObj::Font(font)) => FontSpec::from_font(font),
            _ => panic!("font"),
        };
        let expected = machine.ext.get::<Fonts>().native[&spec]
            .text_metrics()
            .unwrap();
        let mut actual = [0u8; 56];
        machine.cpu.read(metrics, &mut actual).unwrap();
        assert_eq!(actual, expected);
    }
}
