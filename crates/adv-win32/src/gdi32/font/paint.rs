//! Counted text output on the same selected DC and device surface as GDI/DDraw.
use super::*;
use adv_cpu::backend::Prot;
use adv_device::{
    surface::SurfaceStore,
    text::{Horizontal, Paint, Point, TextBox, TextInput, Vertical},
};

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
        set_last_error(env, 120);
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
        return fail(env, 120);
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
        return fail(env, 120);
    }
    let Some(foreground) = color(env, *palette, *text_color) else {
        return fail(env, 87);
    };
    let Some(background) = color(env, *palette, *bk_color) else {
        return fail(env, 87);
    };
    if !matches!(*bk_mode, 1 | 2) || modes.map != 1 {
        return fail(env, 120);
    }
    let opaque_background = *bk_mode == 2;
    let font = font_request(env, dc);
    if font.baseline_angle != 0 || font.glyph_angle != 0 || font.face.starts_with('@') {
        return fail(env, 120);
    }
    let Some(provider) = super::super::raster::front::provider(env.ext) else {
        return fail(env, 120);
    };
    if env.ext.try_get::<SurfaceStore>().is_none() {
        return fail(env, 120);
    }
    let units;
    let input = if ansi {
        TextInput::Encoded(&bytes)
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
    let (result, presented, changed) = {
        let (fonts, store) = env.ext.get2_installed_mut::<Fonts, SurfaceStore>();
        let Some(surface) = store.get(target) else {
            return fail(env, 120);
        };
        let Some(size) = surface.pitch.checked_mul(surface.height) else {
            return fail(env, 87);
        };
        if surface.pixels == 0 {
            return fail(env, 120);
        }
        if adv_abi::guest::check_range(env.cpu, surface.pixels, size, Prot::READ | Prot::WRITE)
            .is_err()
        {
            return fail(env, 87);
        }
        let mut memory = (provider.memory)(env.cpu);
        let result = store.draw_text(target, &mut memory, &mut fonts.text, &font, paint);
        let before = store.stats.presentations;
        let result = result.and_then(|changed| {
            if changed && dc == adv_os::gdi::WINDOW_DC {
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
                Outcome::Return(1)
            }
        }
        Err(_) => fail(env, 120),
    }
}
