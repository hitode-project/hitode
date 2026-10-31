//! Font ownership is supplied by the runtime assembly over the device Text.
use adv_machine::{Conv, Env, Outcome};

/// The session's font frontend, including guest continuations for enumeration.
pub struct FontFrontend(pub fn(&mut Env<'_>, &'static str) -> Outcome);

fn run(env: &mut Env<'_>) -> Outcome {
    let index = env.sys.thunks.get(env.thunk).f.data as usize;
    let name = super::body_at(index).name.as_ref();
    match env.ext.try_get::<FontFrontend>() {
        Some(frontend) => (frontend.0)(env, name),
        None => Outcome::Fail("font frontend is not installed".into()),
    }
}
macro_rules! bodies {
    ($($module:literal, $conv:ident, $argc:literal, $name:literal;)*) => {
        pub(super) const BODIES: &[super::Body] = &[$(super::Body {
            name: std::borrow::Cow::Borrowed(concat!($module,"!",$name)),
            conv: Conv::$conv { argc: $argc }, body: None, handler: run, direct: None,
        }),*];
    }
}
bodies! {
    "win32u.so", Cdecl, 4, "font_CreateDC";
    "win32u.so", Cdecl, 1, "font_DeleteDC";
    "win32u.so", Cdecl, 4, "font_EnumFonts";
    "win32u.so", Cdecl, 1, "font_FontIsLinked";
    "win32u.so", Cdecl, 5, "font_GetCharABCWidths";
    "win32u.so", Cdecl, 5, "font_GetCharABCWidthsI";
    "win32u.so", Cdecl, 5, "font_GetCharWidth";
    "win32u.so", Cdecl, 2, "font_GetCharWidthInfo";
    "win32u.so", Cdecl, 5, "font_GetFontData";
    "win32u.so", Cdecl, 2, "font_GetFontRealizationInfo";
    "win32u.so", Cdecl, 2, "font_GetFontUnicodeRanges";
    "win32u.so", Cdecl, 5, "font_GetGlyphIndices";
    "win32u.so", Cdecl, 7, "font_GetGlyphOutline";
    "win32u.so", Cdecl, 3, "font_GetKerningPairs";
    "win32u.so", Cdecl, 3, "font_GetOutlineTextMetrics";
    "win32u.so", Cdecl, 3, "font_GetTextCharsetInfo";
    "win32u.so", Cdecl, 4, "font_GetTextExtentExPoint";
    "win32u.so", Cdecl, 4, "font_GetTextExtentExPointI";
    "win32u.so", Cdecl, 3, "font_GetTextFace";
    "win32u.so", Cdecl, 2, "font_GetTextMetrics";
    "win32u.so", Cdecl, 3, "font_SelectFont";
    "win32u.so", Cdecl, 0, "init_freetype_lib";
    "win32u.so", Cdecl, 0, "font_init";
    "win32u.so", Cdecl, 8, "dibdrv_ExtTextOut";
    "win32u.so", Cdecl, 3, "dibdrv_SelectFont";
    "win32u.so", Cdecl, 6, "NtGdiAddFontResourceW";
    "win32u.so", Cdecl, 6, "NtGdiRemoveFontResourceW";
    "win32u.so", Cdecl, 5, "NtGdiAddFontMemResourceEx";
    "win32u.so", Cdecl, 1, "NtGdiRemoveFontMemResourceEx";
    "win32u.so", Cdecl, 5, "NtGdiGetFontFileInfo";
    "win32u.so", Cdecl, 5, "NtGdiGetFontFileData";
    "win32u.so", Cdecl, 2, "NtGdiGetRasterizerCaps";
    "win32u.so", Cdecl, 5, "NtGdiMakeFontDir";
    "win32u.so", Cdecl, 9, "NtGdiExtTextOutW";
    "win32u.so", Cdecl, 8, "NtGdiGetTextExtentExW";
    "win32u.so", Cdecl, 3, "NtGdiSetTextJustification";
    "win32u.so", Cdecl, 5, "DrawTextW";
    "gdi32.dll", Stdcall, 1, "EnableEUDC";
    "gdi32.dll", Stdcall, 1, "AddFontResourceA";
    "gdi32.dll", Stdcall, 1, "AddFontResourceW";
    "gdi32.dll", Stdcall, 1, "RemoveFontResourceA";
    "gdi32.dll", Stdcall, 1, "RemoveFontResourceW";
    "gdi32.dll", Stdcall, 3, "AddFontResourceExA";
    "gdi32.dll", Stdcall, 3, "AddFontResourceExW";
    "gdi32.dll", Stdcall, 3, "RemoveFontResourceExA";
    "gdi32.dll", Stdcall, 3, "RemoveFontResourceExW";
    "gdi32.dll", Stdcall, 4, "AddFontMemResourceEx";
    "gdi32.dll", Stdcall, 1, "RemoveFontMemResourceEx";
    "gdi32.dll", Stdcall, 4, "GetCharWidth32W";
    "gdi32.dll", Stdcall, 4, "GetCharWidth32A";
    "gdi32.dll", Stdcall, 3, "GetKerningPairsW";
    "gdi32.dll", Stdcall, 3, "GetKerningPairsA";
    "gdi32.dll", Stdcall, 4, "EnumFontFamiliesA";
    "gdi32.dll", Stdcall, 4, "EnumFontFamiliesW";
    "gdi32.dll", Stdcall, 5, "EnumFontFamiliesExA";
    "gdi32.dll", Stdcall, 5, "EnumFontFamiliesExW";
    "gdi32.dll", Stdcall, 4, "EnumFontsA";
    "gdi32.dll", Stdcall, 4, "EnumFontsW";
    "gdi32.dll", Stdcall, 1, "GetFontLanguageInfo";
    "gdi32.dll", Stdcall, 2, "GetFontUnicodeRanges";
    "gdi32.dll", Stdcall, 5, "GetCharABCWidthsI";
    "gdi32.dll", Stdcall, 5, "GetCharWidthI";
    "gdi32.dll", Stdcall, 4, "GetCharABCWidthsA";
    "gdi32.dll", Stdcall, 4, "GetCharABCWidthsW";
    "gdi32.dll", Stdcall, 2, "GetTextMetricsA";
    "gdi32.dll", Stdcall, 2, "GetTextMetricsW";
    "gdi32.dll", Stdcall, 3, "GetOutlineTextMetricsA";
    "gdi32.dll", Stdcall, 3, "GetOutlineTextMetricsW";
    "gdi32.dll", Stdcall, 4, "GetTextExtentPoint32A";
    "gdi32.dll", Stdcall, 4, "GetTextExtentPoint32W";
    "gdi32.dll", Stdcall, 4, "GetTextExtentPointA";
    "gdi32.dll", Stdcall, 4, "GetTextExtentPointW";
    "gdi32.dll", Stdcall, 7, "GetGlyphOutlineA";
    "gdi32.dll", Stdcall, 7, "GetGlyphOutlineW";
    "gdi32.dll", Stdcall, 6, "GetCharacterPlacementA";
    "gdi32.dll", Stdcall, 6, "GetCharacterPlacementW";
    "gdi32.dll", Stdcall, 1, "GetTextCharset";
    "gdi32.dll", Stdcall, 3, "GetTextCharsetInfo";
    "user32.dll", Stdcall, 5, "DrawTextA";
    "user32.dll", Stdcall, 5, "DrawTextW";
    "user32.dll", Stdcall, 6, "DrawTextExW";
    "user32.dll", Stdcall, 6, "DrawTextExA";
    "user32.dll", Stdcall, 8, "TabbedTextOutW";
    "user32.dll", Stdcall, 8, "TabbedTextOutA";
    "user32.dll", Stdcall, 5, "GetTabbedTextExtentW";
    "user32.dll", Stdcall, 5, "GetTabbedTextExtentA";
    "win32u.so", Cdecl, 4, "NtGdiPolyTextOutW";
    "gdi32.dll", Stdcall, 5, "TextOutA";
    "gdi32.dll", Stdcall, 5, "TextOutW";
    "gdi32.dll", Stdcall, 8, "ExtTextOutA";
    "gdi32.dll", Stdcall, 8, "ExtTextOutW";
    "dwrite.so", Cdecl, 1, "process_attach";
    "dwrite.so", Cdecl, 1, "process_detach";
    "dwrite.so", Cdecl, 1, "create_font_object";
    "dwrite.so", Cdecl, 1, "release_font_object";
    "dwrite.so", Cdecl, 1, "get_glyph_outline";
    "dwrite.so", Cdecl, 1, "get_glyph_count";
    "dwrite.so", Cdecl, 1, "get_glyph_advance";
    "dwrite.so", Cdecl, 1, "get_glyph_bbox";
    "dwrite.so", Cdecl, 1, "get_glyph_bitmap";
    "dwrite.so", Cdecl, 1, "get_design_glyph_metrics";
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{
        Cpu,
        backend::{Guest, GuestExt, Prot},
    };
    use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};

    // spec: WINE-UNIX-6, NAT-BODY-1
    #[test]
    fn native_font_services_replace_original_in_both_execution_modes() {
        for jit in [false, true] {
            for limited in [false, true] {
                let mut machine = Machine::new(
                    Cpu::new(PARK_ADDR, jit).unwrap(),
                    crate::dispatch::Platform,
                    Clock::virtual_time(),
                )
                .unwrap();
                const CODE: u32 = 0x400000;
                const DATA: u32 = 0x410000;
                machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
                machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
                let entry = CODE + 256;
                let mut caller = vec![0xe8];
                caller.extend((entry - CODE - 5).to_le_bytes());
                caller.push(0xa3);
                caller.extend(DATA.to_le_bytes());
                caller.push(0xc3);
                machine.cpu.write(CODE, &caller).unwrap();
                machine.cpu.write(entry, &[0x0f, 0x0b]).unwrap();
                machine.ext.insert(FontFrontend(|_, name| {
                    assert_eq!(name, "win32u.so!init_freetype_lib");
                    Outcome::Return(0x12345678)
                }));
                super::super::bind_one(
                    &mut machine.cpu,
                    &mut machine.sys,
                    &mut machine.ext,
                    "win32u.so!init_freetype_lib",
                    entry,
                    2,
                )
                .unwrap();
                assert!(machine.ext.try_get::<super::super::Trampolines>().is_none());
                if limited {
                    machine.sys.call_limit = Some(100);
                }
                machine.spawn("native font", CODE, &[], 65536).unwrap();
                assert!(matches!(machine.run(), RunResult::Exited(_)));
                assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0x12345678);
                assert_eq!(super::super::declines(&machine.ext), 0);
            }
        }
    }
}
