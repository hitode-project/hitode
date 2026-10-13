//! Counted text output on the same selected DC and device surface as GDI/DDraw.
use super::*;
use adv_cpu::backend::Prot;
use adv_device::{
    surface::SurfaceStore,
    text::{Horizontal, Paint, Point, TextBox, TextInput, Vertical},
};

fn unavailable(env: &mut Env<'_>, reason: &str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    set_last_error(env, 120);
    Outcome::Return(0)
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        set_align_ABI,
        get_align_ABI,
        output_a_ABI,
        output_w_ABI,
        extended_a_ABI,
        extended_w_ABI,
    ] {
        registry.register(api);
    }
}
#[win32(gdi32,name="SetTextAlign",level=state)]
fn set_align(env: &mut Env<'_>, dc: Handle, flags: u32) -> u32 {
    if reject_dc(env, dc.0) {
        return GDI_ERROR;
    }
    if flags & !0x1f != 0 || !matches!(flags & 6, 0 | 2 | 6) || !matches!(flags & 24, 0 | 8 | 24) {
        set_last_error(env, 87);
        return GDI_ERROR;
    }
    // UPDATECP needs current-position state, which this slice does not implement.
    if flags & 1 != 0 {
        let _ = unavailable(
            env,
            "TA_UPDATECP current-position text alignment is not supported",
        );
        return GDI_ERROR;
    }
    std::mem::replace(&mut modes(env, dc.0).text_align, flags)
}
#[win32(gdi32,name="GetTextAlign",level=state)]
fn get_align(env: &mut Env<'_>, dc: Handle) -> u32 {
    if reject_dc(env, dc.0) {
        return GDI_ERROR;
    }
    modes(env, dc.0).text_align
}
#[win32(gdi32,name="TextOutA",level=implement)]
fn output_a(env: &mut Env<'_>, dc: Handle, x: i32, y: i32, input: AnsiStr, count: i32) -> Outcome {
    output(
        env,
        dc.0,
        x,
        y,
        0,
        GuestPtr::new(0),
        input.0.addr(),
        u32::try_from(count).ok(),
        GuestPtr::new(0),
        true,
    )
}
#[win32(gdi32,name="TextOutW",level=implement)]
fn output_w(env: &mut Env<'_>, dc: Handle, x: i32, y: i32, input: WideStr, count: i32) -> Outcome {
    output(
        env,
        dc.0,
        x,
        y,
        0,
        GuestPtr::new(0),
        input.0.addr(),
        u32::try_from(count).ok(),
        GuestPtr::new(0),
        false,
    )
}
#[win32(gdi32,name="ExtTextOutA",level=implement)]
#[allow(clippy::too_many_arguments)]
fn extended_a(
    env: &mut Env<'_>,
    dc: Handle,
    x: i32,
    y: i32,
    options: u32,
    rect: GuestPtr<[i32; 4]>,
    input: AnsiStr,
    count: u32,
    dx: GuestPtr<i32>,
) -> Outcome {
    output(
        env,
        dc.0,
        x,
        y,
        options,
        rect,
        input.0.addr(),
        Some(count),
        dx,
        true,
    )
}
#[win32(gdi32,name="ExtTextOutW",level=implement)]
#[allow(clippy::too_many_arguments)]
fn extended_w(
    env: &mut Env<'_>,
    dc: Handle,
    x: i32,
    y: i32,
    options: u32,
    rect: GuestPtr<[i32; 4]>,
    input: WideStr,
    count: u32,
    dx: GuestPtr<i32>,
) -> Outcome {
    output(
        env,
        dc.0,
        x,
        y,
        options,
        rect,
        input.0.addr(),
        Some(count),
        dx,
        false,
    )
}
fn color(env: &Env<'_>, palette: u32, value: u32) -> Option<[u8; 3]> {
    match value >> 24 {
        0 => Some([value as u8, (value >> 8) as u8, (value >> 16) as u8]),
        1 => super::super::palette::entries(env, palette)?
            .get((value & 0xffff) as usize)
            .map(|p| [p[0], p[1], p[2]]),
        _ => None,
    }
}
#[allow(clippy::too_many_arguments)]
fn output(
    env: &mut Env<'_>,
    dc: u32,
    x: i32,
    y: i32,
    options: u32,
    rect: GuestPtr<[i32; 4]>,
    input: u32,
    count: Option<u32>,
    dx: GuestPtr<i32>,
    ansi: bool,
) -> Outcome {
    let fail = |env: &mut Env<'_>, code| {
        set_last_error(env, code);
        Outcome::Return(0)
    };
    if reject_dc(env, dc) {
        return Outcome::Return(0);
    }
    if options & !6 != 0 || dx.addr() != 0 {
        return unavailable(
            env,
            "text flags or explicit character spacing are not supported",
        );
    }
    let Some(count) = count.filter(|&n| n as usize <= text::MAX_PAINT_UNITS) else {
        return fail(env, 87);
    };
    let length = count * if ansi { 1 } else { 2 };
    let r = if options != 0 {
        if adv_abi::guest::check_range(env.cpu, rect.addr(), 16, Prot::READ).is_err() {
            return fail(env, 87);
        }
        let Ok([left, top, right, bottom]) = rect.read(env.cpu) else {
            return fail(env, 87);
        };
        Some(TextBox {
            left,
            top,
            right,
            bottom,
        })
    } else {
        None
    };
    if length != 0 && adv_abi::guest::check_range(env.cpu, input, length, Prot::READ).is_err() {
        return fail(env, 87);
    }
    let mut bytes = vec![0; length as usize];
    if length != 0 && env.cpu.read(input, &mut bytes).is_err() {
        return fail(env, 87);
    }
    paint(env, dc, x, y, options, r, &bytes, ansi, 1, None)
}

/// DrawText supplies already formatted Unicode, the same selected font/DC and
/// a logical clipping rectangle. No temporary guest buffer or second provider.
pub(super) fn formatted(
    env: &mut Env<'_>,
    dc: u32,
    rectangle: [i32; 4],
    lines: &[Vec<u16>],
    line_height: i32,
    clip: bool,
    height: u32,
) -> Outcome {
    let [left, top, right, bottom] = rectangle;
    paint(
        env,
        dc,
        left,
        top,
        if clip { 4 } else { 0 },
        Some(TextBox {
            left,
            top,
            right,
            bottom,
        }),
        &[],
        false,
        height,
        Some((lines, line_height)),
    )
}

#[allow(clippy::too_many_arguments)]
fn paint(
    env: &mut Env<'_>,
    dc: u32,
    x: i32,
    y: i32,
    options: u32,
    r: Option<TextBox>,
    bytes: &[u8],
    ansi: bool,
    success: u32,
    formatted: Option<(&[Vec<u16>], i32)>,
) -> Outcome {
    if super::super::raster::front::pixels_unavailable(env, dc) {
        let thunk = env.sys.thunks.get(env.thunk);
        let api = format!("{}!{}", thunk.module, thunk.name);
        super::super::raster::front::unavailable_pixels(env, dc, &api);
        return Outcome::Return(0);
    }
    let fail = |env: &mut Env<'_>, code| {
        set_last_error(env, code);
        Outcome::Return(0)
    };
    let Some(GdiObj::Dc {
        selected,
        palette,
        text_color,
        bk_color,
        bk_mode,
        modes,
        ..
    }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc)
    else {
        unreachable!()
    };
    let target = selected[3];
    let align = modes.text_align;
    if *text_color >> 24 == 2 || *bk_color >> 24 == 2 {
        return unavailable(env, "palette-relative text colors are not supported");
    }
    let Some(foreground) = color(env, *palette, *text_color) else {
        return fail(env, 87);
    };
    let Some(background) = color(env, *palette, *bk_color) else {
        return fail(env, 87);
    };
    if !matches!(*bk_mode, 1 | 2) || modes.map != 1 {
        return unavailable(
            env,
            "text background or coordinate mapping mode is not supported",
        );
    }
    let opaque_background = *bk_mode == 2;
    let modes = *modes;
    let Some([x, y]) = modes.device_point(x, y) else {
        return unavailable(
            env,
            "translated text coordinates exceed the supported device range",
        );
    };
    let r = match r {
        Some(r) => {
            let (Some([left, top]), Some([right, bottom])) = (
                modes.device_point(r.left, r.top),
                modes.device_point(r.right, r.bottom),
            ) else {
                return unavailable(
                    env,
                    "translated text rectangle exceeds the supported device range",
                );
            };
            Some(TextBox {
                left,
                top,
                right,
                bottom,
            })
        }
        None => None,
    };
    let clip = super::super::raster::clip::snapshot(env, dc);
    let font = font_request(env, dc);
    if font.baseline_angle != 0 || font.glyph_angle != 0 || font.face.starts_with('@') {
        return unavailable(env, "rotated or vertical text is not supported");
    }
    let Some(provider) = super::super::raster::front::provider(env.ext) else {
        return unavailable(env, "text raster provider is not installed");
    };
    if env.ext.try_get::<SurfaceStore>().is_none() {
        return unavailable(env, "text surface store is not installed");
    }
    let converted = ansi.then(|| ansi_text(env, &font, bytes));
    let units;
    let input = if let Some(converted) = &converted {
        match converted {
            Some(units) => TextInput::Unicode(units),
            None => TextInput::Encoded(bytes),
        }
    } else {
        units = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|u| u16::from_le_bytes(*u))
            .collect::<Vec<_>>();
        TextInput::Unicode(&units)
    };
    let paint = Paint {
        input,
        origin: Point { x, y },
        horizontal: match align & 6 {
            2 => Horizontal::Right,
            6 => Horizontal::Center,
            _ => Horizontal::Left,
        },
        vertical: match align & 24 {
            8 => Vertical::Bottom,
            24 => Vertical::Baseline,
            _ => Vertical::Top,
        },
        foreground,
        background,
        opaque_background,
        clip: if options & 4 != 0 { r } else { None },
        fill: if options & 2 != 0 { r } else { None },
    };
    let window = env
        .ext
        .get::<Win32>()
        .gdi
        .is_window_dc(&env.sys.objects, dc);
    let (result, presented, changed) = {
        let (fonts, store) = env.ext.get2_installed_mut::<Fonts, SurfaceStore>();
        let Some(surface) = store.get(target) else {
            return unavailable(env, "selected bitmap has no text-renderable surface");
        };
        let Some(size) = surface.pitch.checked_mul(surface.height) else {
            return fail(env, 87);
        };
        if surface.pixels == 0 {
            return unavailable(env, "selected bitmap has no CPU text backing");
        }
        if adv_abi::guest::check_range(env.cpu, surface.pixels, size, Prot::READ | Prot::WRITE)
            .is_err()
        {
            return fail(env, 87);
        }
        let surface = surface.clone();
        let mut raw = (provider.memory)(env.cpu);
        let mut memory = super::super::raster::clip::ClippedMemory::new(&mut raw, surface, &clip);
        // Paint the entire paragraph before presenting or yielding to the host.
        let result = (|| {
            if let Some((lines, line_height)) = formatted {
                let mut changed = false;
                for (index, line) in lines.iter().enumerate() {
                    let mut line_paint = paint;
                    line_paint.input = TextInput::Unicode(line);
                    line_paint.origin.y = y
                        .checked_add(index as i32 * line_height)
                        .ok_or("translated paragraph exceeds the device coordinate range")?;
                    changed |=
                        store.draw_text(target, &mut memory, &mut fonts.text, &font, line_paint)?;
                }
                Ok(changed)
            } else {
                store.draw_text(target, &mut memory, &mut fonts.text, &font, paint)
            }
        })();
        let before = store.stats.presentations;
        let result = result.and_then(|changed| {
            if changed && window {
                store.present(target, &memory)?;
            }
            Ok(())
        });
        (
            result,
            store.stats.presentations,
            store.stats.presentations != before,
        )
    };
    match result {
        Ok(()) => {
            if changed && let Some(outcome) = (provider.completed)(env, presented) {
                outcome
            } else {
                Outcome::Return(success)
            }
        }
        Err(error) => unavailable(env, &error),
    }
}
