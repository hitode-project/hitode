//! ABC bearings and glyph-index widths share the selected device font.
use super::*;
use adv_cpu::backend::Prot;

pub(super) fn register(registry: &mut Registry) {
    for api in [
        abc_a_ABI,
        abc_w_ABI,
        abc_i_ABI,
        width_i_ABI,
        ranges_ABI,
        language_ABI,
    ] {
        registry.register(api);
    }
}

#[win32(gdi32, name = "GetFontLanguageInfo", level = implement)]
pub(super) fn language(env: &mut Env<'_>, dc: Handle) -> Outcome {
    if reject_dc(env, dc.0) {
        return Outcome::Return(0x8000);
    }
    let request = font_request(env, dc.0);
    match env.ext.get_mut::<Fonts>().text.language_info(&request) {
        Ok(info) => Outcome::Return(info),
        Err(error) => {
            adv_abi::warn_unavailable(env, "gdi32.dll!GetFontLanguageInfo", &error.to_string());
            set_last_error(env, 50);
            Outcome::Return(0x8000)
        }
    }
}

#[win32(gdi32, name = "GetFontUnicodeRanges", level = implement)]
pub(super) fn ranges(env: &mut Env<'_>, dc: Handle, output: GuestPtr<u8>) -> Outcome {
    if reject_dc(env, dc.0) {
        return Outcome::Return(0);
    }
    let request = font_request(env, dc.0);
    let coverage = match env.ext.get_mut::<Fonts>().text.unicode_ranges(&request) {
        Ok(coverage) => coverage,
        Err(error) => {
            adv_abi::warn_unavailable(env, "gdi32.dll!GetFontUnicodeRanges", &error.to_string());
            set_last_error(env, 50);
            return Outcome::Return(0);
        }
    };
    let size = 16 + coverage.ranges.len() as u32 * 4;
    if !output.is_null() {
        if adv_abi::guest::check_range(env.cpu, output.addr(), size, Prot::WRITE).is_err() {
            return invalid(env);
        }
        let mut bytes: Vec<_> = [
            size,
            coverage.flags,
            coverage.glyphs,
            coverage.ranges.len() as u32,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
        for (first, count) in coverage.ranges {
            bytes.extend(first.to_le_bytes());
            bytes.extend(count.to_le_bytes());
        }
        if env.cpu.write(output.addr(), &bytes).is_err() {
            return invalid(env);
        }
    }
    Outcome::Return(size)
}

#[win32(gdi32, name = "GetCharABCWidthsI", level = implement)]
pub(super) fn abc_i(
    env: &mut Env<'_>,
    dc: Handle,
    first: u32,
    count: u32,
    indices: GuestPtr<u16>,
    output: GuestPtr<u8>,
) -> Outcome {
    glyphs(env, dc.0, first, count, indices.addr(), output.addr(), true)
}
#[win32(gdi32, name = "GetCharWidthI", level = implement)]
pub(super) fn width_i(
    env: &mut Env<'_>,
    dc: Handle,
    first: u32,
    count: u32,
    indices: GuestPtr<u16>,
    output: GuestPtr<u8>,
) -> Outcome {
    glyphs(
        env,
        dc.0,
        first,
        count,
        indices.addr(),
        output.addr(),
        false,
    )
}
#[win32(gdi32, name = "GetCharABCWidthsA", level = implement)]
pub(super) fn abc_a(
    env: &mut Env<'_>,
    dc: Handle,
    first: u32,
    last: u32,
    output: GuestPtr<u8>,
) -> Outcome {
    characters(env, dc.0, first, last, output.addr(), false)
}
#[win32(gdi32, name = "GetCharABCWidthsW", level = implement)]
pub(super) fn abc_w(
    env: &mut Env<'_>,
    dc: Handle,
    first: u32,
    last: u32,
    output: GuestPtr<u8>,
) -> Outcome {
    characters(env, dc.0, first, last, output.addr(), true)
}

pub(super) fn invalid(env: &mut Env<'_>) -> Outcome {
    set_last_error(env, 87);
    Outcome::Return(0)
}
pub(super) fn output_valid(env: &Env<'_>, output: u32, count: u32, stride: u32) -> bool {
    output != 0
        && count <= 65536
        && (count == 0
            || adv_abi::guest::check_range(env.cpu, output, count * stride, Prot::WRITE).is_ok())
}
pub(super) fn finish(
    env: &mut Env<'_>,
    result: Result<Vec<u8>, TextError>,
    output: u32,
    api: &str,
) -> Outcome {
    match result {
        Ok(bytes) => {
            if bytes.is_empty() || env.cpu.write(output, &bytes).is_ok() {
                Outcome::Return(1)
            } else {
                invalid(env)
            }
        }
        Err(error) => {
            adv_abi::warn_unavailable(env, api, &error.to_string());
            set_last_error(env, 50);
            Outcome::Return(0)
        }
    }
}
pub(super) fn pack(values: Vec<text::AbcWidths>) -> Vec<u8> {
    values
        .into_iter()
        .flat_map(|value| [value.left as u32, value.ink, value.right as u32])
        .flat_map(u32::to_le_bytes)
        .collect()
}

pub(super) fn glyphs(
    env: &mut Env<'_>,
    dc: u32,
    first: u32,
    count: u32,
    pointer: u32,
    output: u32,
    abc: bool,
) -> Outcome {
    if reject_dc(env, dc) {
        return Outcome::Return(0);
    }
    if !output_valid(env, output, count, if abc { 12 } else { 4 }) || (abc && count == 0) {
        return invalid(env);
    }
    let indices = if pointer == 0 {
        None
    } else {
        let mut bytes = vec![0; count as usize * 2];
        if count != 0
            && (adv_abi::guest::check_range(env.cpu, pointer, count * 2, Prot::READ).is_err()
                || env.cpu.read(pointer, &mut bytes).is_err())
        {
            return invalid(env);
        }
        Some(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect::<Vec<_>>(),
        )
    };
    let request = font_request(env, dc);
    let provider = &mut env.ext.get_mut::<Fonts>().text;
    let result = if abc {
        provider
            .glyph_abc_widths(&request, first, count, indices.as_deref())
            .map(pack)
    } else {
        provider
            .glyph_widths(&request, first, count, indices.as_deref())
            .map(|values| values.into_iter().flat_map(i32::to_le_bytes).collect())
    };
    finish(
        env,
        result,
        output,
        if abc {
            "gdi32.dll!GetCharABCWidthsI"
        } else {
            "gdi32.dll!GetCharWidthI"
        },
    )
}

pub(super) fn characters(
    env: &mut Env<'_>,
    dc: u32,
    first: u32,
    last: u32,
    output: u32,
    wide: bool,
) -> Outcome {
    if reject_dc(env, dc) {
        return Outcome::Return(0);
    }
    if first > last || last > 65535 || !output_valid(env, output, last - first + 1, 12) {
        return invalid(env);
    }
    let request = font_request(env, dc);
    let characters: Vec<_> = (first..=last)
        .map(|code| {
            if wide {
                Character::Unicode(code)
            } else {
                ansi_character(env, &request, code)
            }
        })
        .collect();
    let result = env
        .ext
        .get_mut::<Fonts>()
        .text
        .character_abc_widths(&request, &characters)
        .map(pack);
    finish(
        env,
        result,
        output,
        if wide {
            "gdi32.dll!GetCharABCWidthsW"
        } else {
            "gdi32.dll!GetCharABCWidthsA"
        },
    )
}
