//! Mandatory native bodies at the font driver and the text entry points.
use super::*;

fn read_units(env: &Env<'_>, pointer: u32, count: u32) -> Vec<u16> {
    placement::units(placement::read(env, pointer, count * 2, false).unwrap())
}
fn write(env: &mut Env<'_>, pointer: u32, bytes: &[u8]) -> Outcome {
    if env.cpu.write(pointer, bytes).is_ok() {
        Outcome::Return(1)
    } else {
        set_last_error(env, 87);
        Outcome::Return(0)
    }
}
/// The tables Wine's `update_codepage` fills: win32u's own ANSI and OEM
/// conversions read them.
fn code_pages(env: &mut Env<'_>, module: u32) {
    let teb = env.sys.contexts[env.context as usize].teb;
    let peb = env.cpu.read_u32(teb + 0x30).unwrap();
    for (data, table) in [
        (0, layout::UTF8_CP),
        (env.cpu.read_u32(peb + 0x58).unwrap(), layout::ANSI_CP),
        (env.cpu.read_u32(peb + 0x5c).unwrap(), layout::OEM_CP),
    ] {
        let info = code_page_table(env, data);
        env.cpu.write(module + table, &info).unwrap();
    }
}

/// `RtlInitCodePageTable` over NLS data at `data`; zero means UTF-8.
fn code_page_table(env: &Env<'_>, data: u32) -> [u8; layout::cptable::SIZE] {
    use layout::cptable;
    let unit = |at: u32| env.cpu.read_u16(at).unwrap();
    let mut info = [0; cptable::SIZE];
    let mut put = |offset: usize, bytes: &[u8]| {
        info[offset..offset + bytes.len()].copy_from_slice(bytes);
    };
    if data == 0 || unit(data + 2) == 65001 {
        for (i, value) in [65001u16, 4, 0x3f, 0xfffd, 0x3f, 0x3f]
            .into_iter()
            .enumerate()
        {
            put(i * 2, &value.to_le_bytes());
        }
        return info;
    }
    for i in 0..6 {
        put(i * 2, &unit(data + 2 + i as u32 * 2).to_le_bytes());
    }
    let mut lead = [0; 12];
    env.cpu.read(data + 14, &mut lead).unwrap();
    put(cptable::LeadByte, &lead);
    let mut ptr = data + u32::from(unit(data)) * 2;
    put(
        cptable::WideCharTable,
        &(ptr + (u32::from(unit(ptr)) + 1) * 2).to_le_bytes(),
    );
    ptr += 2;
    put(cptable::MultiByteTable, &ptr.to_le_bytes());
    ptr += 256 * 2;
    let glyphs = unit(ptr) != 0;
    ptr += 2;
    if glyphs {
        ptr += 256 * 2;
    }
    put(cptable::DBCSRanges, &ptr.to_le_bytes());
    if unit(ptr) != 0 {
        put(12, &1u16.to_le_bytes());
        put(cptable::DBCSOffsets, &(ptr + 2).to_le_bytes());
    }
    info
}

pub(super) fn run(env: &mut Env<'_>, name: &'static str) -> Outcome {
    let (module, name) = name.split_once('!').expect("font body identity");
    let args: [u32; 10] = std::array::from_fn(|i| env.arg(i as u32));
    let a = args.as_slice();
    if module == "dwrite.so" {
        return super::dwrite::run(env, name, a);
    }
    if module == "user32.dll" || name == "DrawTextW" {
        return rectangle::dispatch(env, name, a);
    }
    if module == "gdi32.dll"
        && matches!(
            name,
            "TextOutA" | "TextOutW" | "ExtTextOutA" | "ExtTextOutW"
        )
    {
        return paint::pe_output(env, name, a);
    }
    if module == "gdi32.dll" {
        let f = env
            .ext
            .get::<Fonts>()
            .apis
            .get(module, name)
            .expect("restored font entry");
        return (f.handler)(env);
    }
    match name {
        "font_init" => {
            let user = adv_nt::environment::user_key(env).unwrap();
            let key = adv_nt::registry::open_for_reading(env, 0, &user).unwrap();
            let module = super::module(env).unwrap();
            env.cpu.write_u32(module + layout::HKCU_KEY, key).unwrap();
            code_pages(env, module);
            return Outcome::Return(96);
        }
        "init_freetype_lib" => return Outcome::Return(0),
        "font_CreateDC" => return create_dc(env, a[0]),
        "font_DeleteDC" => {
            (super::super::allocator().free)(env, a[0]);
            return Outcome::Return(1);
        }
        "NtGdiAddFontResourceW" => {
            let path = resource::wide(env, WideStr(GuestPtr::new(a[0])));
            return Outcome::Return(path.map_or(0, |p| resource::add(env, &p, a[3])) as u32);
        }
        "NtGdiRemoveFontResourceW" => {
            let path = resource::wide(env, WideStr(GuestPtr::new(a[0])));
            return Outcome::Return(path.map_or(0, |p| resource::remove(env, &p, a[3])) as u32);
        }
        "NtGdiAddFontMemResourceEx" => {
            return Outcome::Return(
                resource::add_memory(
                    env,
                    GuestPtr::new(a[0]),
                    a[1],
                    GuestPtr::new(a[2]),
                    GuestPtr::new(a[4]),
                )
                .0,
            );
        }
        "NtGdiRemoveFontMemResourceEx" => {
            return Outcome::Return(resource::remove_memory(env, Handle(a[0])) as u32);
        }
        "NtGdiGetRasterizerCaps" => {
            let bytes: Vec<_> = [6u16, 3, 0]
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect();
            return write(env, a[0], &bytes[..(a[1] as usize).min(6)]);
        }
        "NtGdiGetFontFileInfo" | "NtGdiGetFontFileData" => return file_info(env, name, a),
        "NtGdiMakeFontDir" => return resource::make_dir(env, a),
        "NtGdiExtTextOutW" => return paint::output(env, a),
        "dibdrv_ExtTextOut" => return paint::driver_output(env, a),
        "NtGdiGetTextExtentExW" => return extent(env, a),
        "NtGdiSetTextJustification" => {
            let Some(dc) = dc_pointer(env, a[0]) else {
                return Outcome::Return(0);
            };
            let (extra, breaks) = (a[1] as i32, a[2] as i32);
            let (q, r) = if breaks > 0 {
                (extra / breaks, extra % breaks)
            } else {
                (0, 0)
            };
            let _ = env.cpu.write_u32(dc + layout::dc::breakExtra, q as u32);
            let _ = env.cpu.write_u32(dc + layout::dc::breakRem, r as u32);
            return Outcome::Return(1);
        }
        "NtGdiPolyTextOutW" => return paint::poly(env, a),
        _ => {}
    }
    let hdc = env.cpu.read_u32(a[0] + layout::physdev::hdc).unwrap();
    let dc = Handle(hdc);
    if name == "dibdrv_SelectFont" {
        let mut next = env.cpu.read_u32(a[0] + layout::physdev::next).unwrap();
        loop {
            let funcs = env.cpu.read_u32(next).unwrap();
            let function = env
                .cpu
                .read_u32(funcs + layout::funcs::pSelectFont)
                .unwrap();
            if function != 0 {
                return Outcome::call_aligned(env, function, [next, a[1], a[2]], |_, result| {
                    Outcome::Return(result.value())
                });
            }
            next = env.cpu.read_u32(next + layout::physdev::next).unwrap();
        }
    }
    if name == "font_SelectFont" {
        let request = request_for_handle(env, a[1]);
        let quality = env
            .ext
            .get_mut::<Fonts>()
            .text
            .logical_font(&request)
            .map_or(request.quality, |font| font.quality);
        let flags = if quality == Quality::Monochrome { 1 } else { 6 };
        let _ = env.cpu.write_u32(a[2], flags);
        env.ext
            .get_mut::<Fonts>()
            .realizations
            .insert(a[1], request);
        return Outcome::Return(a[1]);
    }
    if name == "font_EnumFonts" {
        return enumeration::driver(env, hdc, a);
    }
    let request = font_request(env, hdc);
    match name {
        "font_FontIsLinked" => Outcome::Return(u32::from(
            env.ext
                .get_mut::<Fonts>()
                .text
                .font_is_linked(&request)
                .unwrap_or(false),
        )),
        "font_GetTextMetrics" => Outcome::Return(metrics_w(env, dc, GuestPtr::new(a[1])) as u32),
        "font_GetOutlineTextMetrics" => outline_metrics_w(env, dc, a[1], GuestPtr::new(a[2])),
        "font_GetFontUnicodeRanges" => spacing::ranges(env, dc, GuestPtr::new(a[1])),
        "font_GetKerningPairs" => {
            Outcome::Return(kerning::pairs_w(env, dc, a[1], GuestPtr::new(a[2])))
        }
        "font_GetTextCharsetInfo" => {
            Outcome::Return(charset::text_charset_info(env, dc, GuestPtr::new(a[1]), a[2]) as u32)
        }
        "font_GetGlyphOutline" => {
            let matrix = if a[6] == 0 {
                let sp = env.cpu.reg(adv_cpu::backend::Reg::Esp) - 32;
                let bytes: Vec<_> = [65536i32, 0, 0, 65536]
                    .into_iter()
                    .flat_map(i32::to_le_bytes)
                    .collect();
                env.cpu.write(sp, &bytes).unwrap();
                sp
            } else {
                a[6]
            };
            glyph(
                env,
                hdc,
                a[1],
                a[2],
                GuestPtr::new(a[3]),
                a[4],
                GuestPtr::new(a[5]),
                GuestPtr::new(matrix),
                false,
            )
        }
        "font_GetCharABCWidthsI" => spacing::glyphs(env, hdc, a[1], a[2], a[3], a[4], true),
        "font_GetCharABCWidths" | "font_GetCharWidth" => {
            let characters: Vec<_> = if a[3] != 0 {
                read_units(env, a[3], a[2])
                    .into_iter()
                    .map(|c| Character::Unicode(u32::from(c)))
                    .collect()
            } else {
                (a[1]..a[1] + a[2]).map(Character::Unicode).collect()
            };
            let result = env
                .ext
                .get_mut::<Fonts>()
                .text
                .character_abc_widths(&request, &characters);
            let result = result.map(|values| {
                if name == "font_GetCharABCWidths" {
                    spacing::pack(values)
                } else {
                    values
                        .into_iter()
                        .flat_map(|v| (v.left + v.ink as i32 + v.right).to_le_bytes())
                        .collect()
                }
            });
            spacing::finish(env, result, a[4], name)
        }
        "font_GetCharWidthInfo" => {
            match env.ext.get_mut::<Fonts>().text.char_width_info(&request) {
                Ok([l, r]) => write(
                    env,
                    a[1],
                    &[l, r, 0]
                        .into_iter()
                        .flat_map(i32::to_le_bytes)
                        .collect::<Vec<_>>(),
                ),
                Err(_) => Outcome::Return(0),
            }
        }
        "font_GetGlyphIndices" => {
            let units = read_units(env, a[1], a[2]);
            match env.ext.get_mut::<Fonts>().text.glyph_indices(
                &request,
                &units,
                if a[4] & 1 != 0 { 0xffff } else { 0 },
            ) {
                Ok(indices) => {
                    let bytes: Vec<_> = indices.into_iter().flat_map(u16::to_le_bytes).collect();
                    let _ = env.cpu.write(a[3], &bytes);
                    Outcome::Return(a[2])
                }
                Err(_) => Outcome::Return(GDI_ERROR),
            }
        }
        "font_GetTextExtentExPoint" | "font_GetTextExtentExPointI" => {
            let units = read_units(env, a[1], a[2]);
            match positions(env, &request, &units, name.ends_with('I'), 0, 0, 0) {
                Ok((dx, _)) => write(
                    env,
                    a[3],
                    &dx.into_iter()
                        .flat_map(i32::to_le_bytes)
                        .collect::<Vec<_>>(),
                ),
                Err(_) => Outcome::Return(0),
            }
        }
        "font_GetTextFace" => match env.ext.get_mut::<Fonts>().text.logical_font(&request) {
            Ok(font) => {
                let mut units: Vec<_> = font.face.encode_utf16().chain([0]).collect();
                let count = units.len() as u32;
                if a[2] == 0 {
                    return Outcome::Return(count);
                }
                units.truncate(a[1] as usize);
                if let Some(last) = units.last_mut() {
                    *last = 0;
                }
                let copied = units.len() as u32;
                let _ = env.cpu.write(
                    a[2],
                    &units
                        .into_iter()
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<_>>(),
                );
                Outcome::Return(copied)
            }
            Err(_) => Outcome::Return(0),
        },
        "font_GetFontData" => font_data(env, &request, a),
        "font_GetFontRealizationInfo" => {
            let size = env.cpu.read_u32(a[1]).unwrap();
            let handle = env
                .cpu
                .read_u32(dc_pointer(env, hdc).unwrap() + layout::dc::hFont)
                .unwrap();
            let text = &mut env.ext.get_mut::<Fonts>().text;
            let index = text.font_file(&request).map_or(0, |f| f.index);
            let simulations = text.simulations(&request).unwrap_or(0);
            env.ext
                .get_mut::<Fonts>()
                .realizations
                .insert(handle, request);
            let values = [size, 3, 1, handle, 1, index, simulations];
            write(
                env,
                a[1],
                &values
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .take(size as usize)
                    .collect::<Vec<_>>(),
            )
        }
        _ => Outcome::Fail(format!("unrecognized font body {name}")),
    }
}
fn create_dc(env: &mut Env<'_>, slot: u32) -> Outcome {
    let allocator = super::super::allocator();
    let Some(dev) = (allocator.allocate)(env, 16) else {
        return Outcome::Return(0);
    };
    let table = module(env).unwrap() + layout::FONT_DRIVER;
    let priority = env.cpu.read_u32(table + layout::funcs::priority).unwrap();
    let mut slot = slot;
    loop {
        let next = env.cpu.read_u32(slot).unwrap();
        if next == 0 {
            break;
        }
        let funcs = env.cpu.read_u32(next).unwrap();
        if env.cpu.read_u32(funcs + layout::funcs::priority).unwrap() <= priority {
            break;
        }
        slot = next + layout::physdev::next;
    }
    let next = env.cpu.read_u32(slot).unwrap();
    let hdc = if next != 0 {
        env.cpu.read_u32(next + layout::physdev::hdc).unwrap()
    } else {
        0
    };
    let bytes: Vec<_> = [table, next, hdc, 0]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    env.cpu.write(dev, &bytes).unwrap();
    env.cpu.write_u32(slot, dev).unwrap();
    Outcome::Return(1)
}
fn font_data(env: &mut Env<'_>, request: &FontRequest, a: &[u32]) -> Outcome {
    let Ok(file) = env.ext.get_mut::<Fonts>().text.font_file(request) else {
        return Outcome::Return(GDI_ERROR);
    };
    let all = file.data.as_ref().as_ref();
    let bytes = if a[1] == 0 {
        Some(&all[file.item_offset()..])
    } else if a[1] == u32::from_le_bytes(*b"ttcf") {
        all.starts_with(b"ttcf").then_some(all)
    } else {
        file.table(a[1].to_le_bytes())
    };
    let Some(bytes) = bytes.and_then(|b| b.get(a[2] as usize..)) else {
        return Outcome::Return(GDI_ERROR);
    };
    if a[3] == 0 {
        return Outcome::Return(bytes.len() as u32);
    }
    let count = bytes.len().min(a[4] as usize);
    if env.cpu.write(a[3], &bytes[..count]).is_err() {
        return Outcome::Return(GDI_ERROR);
    }
    Outcome::Return(count as u32)
}
pub(super) fn positions(
    env: &mut Env<'_>,
    request: &FontRequest,
    units: &[u16],
    indices: bool,
    extra: i32,
    justify: i32,
    mut rem: i32,
) -> Result<(Vec<i32>, i32), TextError> {
    let widths = if indices {
        env.ext
            .get_mut::<Fonts>()
            .text
            .glyph_widths(request, 0, units.len() as u32, Some(units))?
    } else {
        let mut horizontal = request.clone();
        horizontal.baseline_angle = 0;
        horizontal.glyph_angle = 0;
        let mut widths = Vec::with_capacity(units.len());
        for scalar in char::decode_utf16(units.iter().copied()) {
            let scalar = scalar.map_err(|e| TextError::Font(e.to_string()))?;
            let mut encoded = [0; 2];
            let encoded = scalar.encode_utf16(&mut encoded);
            if encoded.len() == 2 {
                widths.push(0);
            }
            widths.push(
                env.ext
                    .get_mut::<Fonts>()
                    .text
                    .extent(&horizontal, text::TextInput::Unicode(encoded))?
                    .width,
            );
        }
        widths
    };
    let mut total = 0;
    let mut dx = Vec::with_capacity(units.len());
    for (&c, w) in units.iter().zip(widths) {
        total += w + extra;
        if c == 32 {
            total += justify;
            if rem != 0 {
                total += rem.signum();
                rem -= rem.signum();
            }
        }
        dx.push(total);
    }
    Ok((dx, total))
}
fn extent(env: &mut Env<'_>, a: &[u32]) -> Outcome {
    if reject_dc(env, a[0]) || (a[2] as i32) < 0 {
        return Outcome::Return(0);
    }
    let request = font_request(env, a[0]);
    let dc = dc_pointer(env, a[0]).unwrap();
    let attr = env.cpu.read_u32(dc + layout::dc::attr).unwrap();
    let extra = env.cpu.read_u32(attr + layout::attr::char_extra).unwrap() as i32;
    let justify = env.cpu.read_u32(dc + layout::dc::breakExtra).unwrap() as i32;
    let rem = env.cpu.read_u32(dc + layout::dc::breakRem).unwrap() as i32;
    let units = read_units(env, a[1], a[2]);
    let Ok((dx, width)) = positions(env, &request, &units, a[7] != 0, extra, justify, rem) else {
        return Outcome::Return(0);
    };
    let height = if units.is_empty() {
        0
    } else {
        env.ext
            .get_mut::<Fonts>()
            .text
            .metrics(&request)
            .unwrap()
            .height
    };
    if a[4] != 0 {
        let fit = dx.iter().take_while(|&&x| x <= a[3] as i32).count();
        let _ = env.cpu.write_u32(a[4], fit as u32);
    }
    if a[5] != 0 {
        let _ = env.cpu.write(
            a[5],
            &dx.into_iter()
                .flat_map(i32::to_le_bytes)
                .collect::<Vec<_>>(),
        );
    }
    write(
        env,
        a[6],
        &[width, height]
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect::<Vec<_>>(),
    )
}
fn file_info(env: &mut Env<'_>, name: &str, a: &[u32]) -> Outcome {
    let Some(request) = env.ext.get::<Fonts>().realizations.get(&a[0]).cloned() else {
        return Outcome::Return(0);
    };
    if a[1] != 0 {
        return Outcome::Return(0);
    }
    let Ok(file) = env.ext.get_mut::<Fonts>().text.font_file(&request) else {
        return Outcome::Return(0);
    };
    let bytes = file.data.as_ref().as_ref();
    if name == "NtGdiGetFontFileData" {
        let mut offset = [0; 8];
        if env.cpu.read(a[2], &mut offset).is_err() {
            return Outcome::Return(0);
        }
        let off = u64::from_le_bytes(offset) as usize;
        let Some(data) = bytes.get(off..off.saturating_add(a[4] as usize)) else {
            set_last_error(env, 87);
            return Outcome::Return(0);
        };
        write(env, a[3], data)
    } else {
        let required = 24;
        if a[4] != 0 {
            let _ = env.cpu.write_u32(a[4], required);
        }
        if a[3] < required {
            set_last_error(env, 122);
            return Outcome::Return(0);
        }
        let mut info = [0; 24];
        info[8..16].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
        write(env, a[2], &info)
    }
}
