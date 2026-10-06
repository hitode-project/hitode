//! FillRect is a USER32 front over the existing GDI brush and device surface.
use crate::{Win32, util::set_last_error};
use adv_abi::{Env, GuestPtr, Handle, Outcome, Registry, types::RECT, win32};
use adv_cpu::backend::Prot;
use adv_device::surface::{PixelFormat, Rect, SurfaceStore};
use adv_os::gdi::{GdiObj, WINDOW_DC, stock_object};

pub(super) fn register(registry: &mut Registry) {
    registry.register(fill_ABI);
    registry.register(frame_ABI);
    registry.register(set_pixel_ABI);
    registry.register(set_pixel_v_ABI);
    registry.register(get_pixel_ABI);
    registry.register(pat_blt_ABI);
}
fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
#[win32(user32,name="FillRect",level=implement)]
fn fill(env: &mut Env<'_>, dc: Handle, rectangle: GuestPtr<RECT>, brush: Handle) -> Outcome {
    if !matches!(
        env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0),
        Some(GdiObj::Dc { .. })
    ) {
        return fail(env, 6);
    }
    if adv_abi::guest::check_range(env.cpu, rectangle.addr(), 16, Prot::READ).is_err() {
        return fail(env, 87);
    }
    let Ok(r) = rectangle.read(env.cpu) else {
        return fail(env, 87);
    };
    paint_rectangles(
        env,
        dc,
        brush,
        &[[
            i64::from(r.left),
            i64::from(r.top),
            i64::from(r.right),
            i64::from(r.bottom),
        ]],
        "user32.dll!FillRect",
    )
}

#[win32(user32,name="FrameRect",level=implement)]
fn frame(env: &mut Env<'_>, dc: Handle, rectangle: GuestPtr<RECT>, brush: Handle) -> Outcome {
    if !matches!(
        env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0),
        Some(GdiObj::Dc { .. })
    ) {
        return fail(env, 6);
    }
    if adv_abi::guest::check_range(env.cpu, rectangle.addr(), 16, Prot::READ).is_err() {
        return fail(env, 87);
    }
    let Ok(r) = rectangle.read(env.cpu) else {
        return fail(env, 87);
    };
    if r.right < r.left || r.bottom < r.top {
        return Outcome::Return(0);
    }
    let [l, t, r, b] = [
        i64::from(r.left),
        i64::from(r.top),
        i64::from(r.right),
        i64::from(r.bottom),
    ];
    // USER draws four logical-unit PatBlt strips. Normalizing the side strip's
    // negative height preserves native zero-width/height border pixels.
    let top = (t + 1).min(b - 1);
    let bottom = (t + 1).max(b - 1);
    paint_rectangles(
        env,
        dc,
        brush,
        &[
            [l, t, r, t + 1],
            [l, b - 1, r, b],
            [l, top, l + 1, bottom],
            [r - 1, top, r, bottom],
        ],
        "user32.dll!FrameRect",
    )
}

fn paint_rectangles(
    env: &mut Env<'_>,
    dc: Handle,
    brush: Handle,
    rectangles: &[[i64; 4]],
    api: &str,
) -> Outcome {
    let Some(GdiObj::Dc {
        selected,
        palette,
        modes,
        clip,
        ..
    }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0)
    else {
        return fail(env, 6);
    };
    if modes.map != 1 {
        adv_abi::warn_unavailable(env, api, "only MM_TEXT brush drawing is available");
        return fail(env, 50);
    }
    let (target, palette, clip, modes) = (selected[3], *palette, clip.clone(), *modes);
    let stock = brush.0.checked_sub(stock_object(0));
    let color = match stock {
        Some(0) => 0xffffff,
        Some(1) => 0xc0c0c0,
        Some(2) => 0x808080,
        Some(3) => 0x404040,
        Some(4) => 0,
        Some(5) => return Outcome::Return(1),
        _ => match env.ext.get::<Win32>().gdi.get(&env.sys.objects, brush.0) {
            Some(GdiObj::Brush { color }) => *color,
            _ => return fail(env, 6),
        },
    };
    let rgb = match color >> 24 {
        0 => [color as u8, (color >> 8) as u8, (color >> 16) as u8],
        1 => {
            let Some(entry) = super::super::palette::entries(env, palette)
                .and_then(|e| e.get((color & 0xffff) as usize))
            else {
                return fail(env, 87);
            };
            [entry[0], entry[1], entry[2]]
        }
        _ => {
            adv_abi::warn_unavailable(env, api, "palette-relative brushes are unavailable");
            return fail(env, 50);
        }
    };
    let Some(provider) = super::front::provider(env.ext) else {
        adv_abi::warn_unavailable(env, api, "the CPU raster provider is unavailable");
        return fail(env, 50);
    };
    let Some(surface) = env
        .ext
        .try_get::<SurfaceStore>()
        .and_then(|s| s.get(target))
        .cloned()
    else {
        return fail(env, 6);
    };
    if surface.pixels == 0 {
        adv_abi::warn_unavailable(env, api, "the selected bitmap has no CPU backing");
        return fail(env, 50);
    }
    let rectangles = rectangles
        .iter()
        .filter_map(|&[l, t, r, b]| {
            let [l, t] = modes.to_device(l, t);
            let [r, b] = modes.to_device(r, b);
            let l = l.max(0).min(i64::from(surface.width)) as u32;
            let t = t.max(0).min(i64::from(surface.height)) as u32;
            let r = r.max(0).min(i64::from(surface.width)) as u32;
            let b = b.max(0).min(i64::from(surface.height)) as u32;
            (l < r && t < b).then(|| Rect {
                x: l,
                y: t,
                w: r - l,
                h: b - t,
            })
        })
        .collect::<Vec<_>>();
    if rectangles.is_empty() {
        return Outcome::Return(1);
    }
    let bytes = surface.format.bytes();
    {
        let mut raw = (provider.memory)(env.cpu);
        let memory = super::clip::ClippedMemory::new(&mut raw, surface.clone(), &clip);
        // All visible border/fill ranges are checked before the first write.
        for r in &rectangles {
            for y in r.y..r.y + r.h {
                let row = if surface.bottom_up {
                    surface.height - y - 1
                } else {
                    y
                };
                let Some(address) = row
                    .checked_mul(surface.pitch)
                    .and_then(|o| surface.pixels.checked_add(o))
                    .and_then(|a| a.checked_add(r.x * bytes))
                else {
                    return fail(env, 87);
                };
                if adv_device::surface::SurfaceMemory::writable(
                    &memory,
                    address,
                    (r.w * bytes) as usize,
                )
                .is_err()
                {
                    return fail(env, 87);
                }
            }
        }
    }
    let value = match surface.format {
        PixelFormat::Rgb555 => {
            u32::from(rgb[0] >> 3) << 10 | u32::from(rgb[1] >> 3) << 5 | u32::from(rgb[2] >> 3)
        }
        PixelFormat::Rgb565 => {
            u32::from(rgb[0] >> 3) << 11 | u32::from(rgb[1] >> 2) << 5 | u32::from(rgb[2] >> 3)
        }
        _ => u32::from_le_bytes(surface.encode_pixel([rgb[2], rgb[1], rgb[0], 0])),
    };
    let result = {
        let store = env.ext.get_mut::<SurfaceStore>();
        let mut raw = (provider.memory)(env.cpu);
        let mut memory = super::clip::ClippedMemory::new(&mut raw, surface, &clip);
        let mut result = Ok(());
        for r in rectangles {
            if let Err(e) = store.fill_raw(target, r, value, &mut memory) {
                result = Err(e);
                break;
            }
        }
        result.and_then(|()| {
            if dc.0 == WINDOW_DC {
                store.present(target, &memory)
            } else {
                Ok(())
            }
        })
    };
    match result {
        Ok(()) => {
            if dc.0 == WINDOW_DC {
                let serial = env.ext.get::<SurfaceStore>().stats.presentations;
                if let Some(o) = (provider.completed)(env, serial) {
                    return o;
                }
            }
            Outcome::Return(1)
        }
        Err(error) => Outcome::Fail(format!("{api}: {error}")),
    }
}

const CLR_INVALID: u32 = 0xffff_ffff;

#[win32(gdi32, name = "SetPixel", level = implement)]
fn set_pixel(env: &mut Env<'_>, dc: Handle, x: i32, y: i32, color: u32) -> Outcome {
    let Some(GdiObj::Dc {
        selected,
        palette,
        modes,
        ..
    }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0)
    else {
        return Outcome::Return(CLR_INVALID);
    };
    if modes.map != 1 {
        return Outcome::Return(CLR_INVALID);
    }
    let Some([x, y]) = modes.device_point(x, y) else {
        return Outcome::Return(CLR_INVALID);
    };
    if !super::clip::snapshot(env, dc.0).contains(x, y) {
        return Outcome::Return(CLR_INVALID);
    }
    let (target, palette) = (selected[3], *palette);
    let rgb = match color >> 24 {
        0 => [color as u8, (color >> 8) as u8, (color >> 16) as u8],
        1 => {
            let Some(entry) = super::super::palette::entries(env, palette)
                .and_then(|entries| entries.get((color & 0xffff) as usize))
            else {
                return Outcome::Return(CLR_INVALID);
            };
            [entry[0], entry[1], entry[2]]
        }
        _ => return Outcome::Return(CLR_INVALID),
    };
    let Some(provider) = super::front::provider(env.ext) else {
        return Outcome::Return(color & 0x00ff_ffff);
    };
    let Some(store) = env.ext.try_get::<SurfaceStore>() else {
        return Outcome::Return(color & 0x00ff_ffff);
    };
    let Some(surface) = store.get(target) else {
        return Outcome::Return(CLR_INVALID);
    };
    if surface.pixels == 0 {
        return Outcome::Return(CLR_INVALID);
    }
    if x < 0 || x >= surface.width as i32 || y < 0 || y >= surface.height as i32 {
        return Outcome::Return(CLR_INVALID);
    }
    let (ux, uy) = (x as u32, y as u32);
    let row = if surface.bottom_up {
        surface.height - uy - 1
    } else {
        uy
    };
    let bytes = surface.format.bytes();
    let Some(address) = row
        .checked_mul(surface.pitch)
        .and_then(|offset| surface.pixels.checked_add(offset))
        .and_then(|address| address.checked_add(ux * bytes))
    else {
        return Outcome::Return(CLR_INVALID);
    };
    if adv_abi::guest::check_range(env.cpu, address, bytes, Prot::WRITE).is_err() {
        return Outcome::Return(CLR_INVALID);
    }
    let value = match surface.format {
        PixelFormat::Rgb555 => {
            u32::from(rgb[0] >> 3) << 10 | u32::from(rgb[1] >> 3) << 5 | u32::from(rgb[2] >> 3)
        }
        PixelFormat::Rgb565 => {
            u32::from(rgb[0] >> 3) << 11 | u32::from(rgb[1] >> 2) << 5 | u32::from(rgb[2] >> 3)
        }
        _ => u32::from_le_bytes(surface.encode_pixel([rgb[2], rgb[1], rgb[0], 0])),
    };
    let result = {
        let store = env.ext.get_mut::<SurfaceStore>();
        let mut memory = (provider.memory)(env.cpu);
        store
            .fill_raw(
                target,
                Rect {
                    x: ux,
                    y: uy,
                    w: 1,
                    h: 1,
                },
                value,
                &mut memory,
            )
            .and_then(|()| {
                if dc.0 == WINDOW_DC {
                    store.present(target, &memory)
                } else {
                    Ok(())
                }
            })
    };
    match result {
        Ok(()) => {
            if dc.0 == WINDOW_DC {
                let serial = env.ext.get::<SurfaceStore>().stats.presentations;
                if let Some(outcome) = (provider.completed)(env, serial) {
                    return outcome;
                }
            }
            Outcome::Return((rgb[0] as u32) | ((rgb[1] as u32) << 8) | ((rgb[2] as u32) << 16))
        }
        Err(_) => Outcome::Return(CLR_INVALID),
    }
}

#[win32(gdi32, name = "SetPixelV", level = implement)]
fn set_pixel_v(env: &mut Env<'_>, dc: Handle, x: i32, y: i32, color: u32) -> Outcome {
    match set_pixel(env, dc, x, y, color) {
        Outcome::Return(v) if v != CLR_INVALID => Outcome::Return(1),
        Outcome::Return(_) => Outcome::Return(0),
        other => other,
    }
}

#[win32(gdi32, name = "GetPixel", level = implement)]
fn get_pixel(env: &mut Env<'_>, dc: Handle, x: i32, y: i32) -> Outcome {
    let Some(GdiObj::Dc {
        selected, modes, ..
    }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0)
    else {
        return Outcome::Return(CLR_INVALID);
    };
    if modes.map != 1 {
        return Outcome::Return(CLR_INVALID);
    }
    let Some([x, y]) = modes.device_point(x, y) else {
        return Outcome::Return(CLR_INVALID);
    };
    if !super::clip::snapshot(env, dc.0).contains(x, y) {
        return Outcome::Return(CLR_INVALID);
    }
    let target = selected[3];
    let Some(_provider) = super::front::provider(env.ext) else {
        return Outcome::Return(CLR_INVALID);
    };
    let Some(store) = env.ext.try_get::<SurfaceStore>() else {
        return Outcome::Return(CLR_INVALID);
    };
    let Some(surface) = store.get(target) else {
        return Outcome::Return(CLR_INVALID);
    };
    if surface.pixels == 0 {
        return Outcome::Return(CLR_INVALID);
    }
    if x < 0 || x >= surface.width as i32 || y < 0 || y >= surface.height as i32 {
        return Outcome::Return(CLR_INVALID);
    }
    let (ux, uy) = (x as u32, y as u32);
    let row = if surface.bottom_up {
        surface.height - uy - 1
    } else {
        uy
    };
    let bytes = surface.format.bytes();
    let Some(address) = row
        .checked_mul(surface.pitch)
        .and_then(|offset| surface.pixels.checked_add(offset))
        .and_then(|address| address.checked_add(ux * bytes))
    else {
        return Outcome::Return(CLR_INVALID);
    };
    if adv_abi::guest::check_range(env.cpu, address, bytes, Prot::READ).is_err() {
        return Outcome::Return(CLR_INVALID);
    }
    let mut raw = [0u8; 4];
    if env.cpu.read(address, &mut raw[..bytes as usize]).is_err() {
        return Outcome::Return(CLR_INVALID);
    }
    let [b, g, r, _a] = surface.decode_pixel(&raw[..bytes as usize]);
    Outcome::Return((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

const BLACKNESS: u32 = 0x00000042;
const WHITENESS: u32 = 0x00FF0062;

#[win32(gdi32, name = "PatBlt", level = implement)]
fn pat_blt(env: &mut Env<'_>, dc: Handle, x: i32, y: i32, w: i32, h: i32, rop: u32) -> Outcome {
    if w <= 0 || h <= 0 {
        return Outcome::Return(1);
    }
    let Some(GdiObj::Dc {
        selected,
        palette,
        modes,
        ..
    }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0)
    else {
        return fail(env, 6);
    };
    if modes.map != 1 {
        return fail(env, 120);
    }
    let (target, palette) = (selected[3], *palette);
    let [x, y] = modes.to_device(i64::from(x), i64::from(y));
    let brush_handle = match rop {
        BLACKNESS => stock_object(4), // BLACK_BRUSH
        WHITENESS => stock_object(0), // WHITE_BRUSH
        _ => selected[0],             // Selected brush in the DC
    };
    let stock = brush_handle.checked_sub(stock_object(0));
    let color = match stock {
        Some(0) => 0xffffff,
        Some(1) => 0xc0c0c0,
        Some(2) => 0x808080,
        Some(3) => 0x404040,
        Some(4) => 0,
        Some(5) => return Outcome::Return(1), // NULL_BRUSH is a valid no-op.
        _ => match env
            .ext
            .get::<Win32>()
            .gdi
            .get(&env.sys.objects, brush_handle)
        {
            Some(GdiObj::Brush { color }) => *color,
            _ => return fail(env, 6),
        },
    };
    let rgb = match color >> 24 {
        0 => [color as u8, (color >> 8) as u8, (color >> 16) as u8],
        1 => {
            let Some(entry) = super::super::palette::entries(env, palette)
                .and_then(|entries| entries.get((color & 0xffff) as usize))
            else {
                return fail(env, 87);
            };
            [entry[0], entry[1], entry[2]]
        }
        _ => return fail(env, 120),
    };
    let Some(provider) = super::front::provider(env.ext) else {
        return fail(env, 120);
    };
    let Some(store) = env.ext.try_get::<SurfaceStore>() else {
        return fail(env, 120);
    };
    let Some(surface) = store.get(target) else {
        return fail(env, 6);
    };
    if surface.pixels == 0 {
        return fail(env, 120);
    }
    let left = x.max(0).min(i64::from(surface.width)) as u32;
    let top = y.max(0).min(i64::from(surface.height)) as u32;
    let right = (x + i64::from(w)).max(0).min(i64::from(surface.width)) as u32;
    let bottom = (y + i64::from(h)).max(0).min(i64::from(surface.height)) as u32;
    if right <= left || bottom <= top {
        return Outcome::Return(1);
    }
    let clip = super::clip::snapshot(env, dc.0);
    let surface = surface.clone();
    let bytes = surface.format.bytes();
    for row_y in top..bottom {
        let row = if surface.bottom_up {
            surface.height - row_y - 1
        } else {
            row_y
        };
        let Some(address) = row
            .checked_mul(surface.pitch)
            .and_then(|offset| surface.pixels.checked_add(offset))
            .and_then(|address| address.checked_add(left * bytes))
        else {
            return fail(env, 87);
        };
        if adv_abi::guest::check_range(env.cpu, address, (right - left) * bytes, Prot::WRITE)
            .is_err()
        {
            return fail(env, 87);
        }
    }
    let value = match surface.format {
        PixelFormat::Rgb555 => {
            u32::from(rgb[0] >> 3) << 10 | u32::from(rgb[1] >> 3) << 5 | u32::from(rgb[2] >> 3)
        }
        PixelFormat::Rgb565 => {
            u32::from(rgb[0] >> 3) << 11 | u32::from(rgb[1] >> 2) << 5 | u32::from(rgb[2] >> 3)
        }
        _ => u32::from_le_bytes(surface.encode_pixel([rgb[2], rgb[1], rgb[0], 0])),
    };
    let result = {
        let store = env.ext.get_mut::<SurfaceStore>();
        let mut raw = (provider.memory)(env.cpu);
        let mut memory = super::clip::ClippedMemory::new(&mut raw, surface, &clip);
        store
            .fill_raw(
                target,
                Rect {
                    x: left,
                    y: top,
                    w: right - left,
                    h: bottom - top,
                },
                value,
                &mut memory,
            )
            .and_then(|()| {
                if dc.0 == WINDOW_DC {
                    store.present(target, &memory)
                } else {
                    Ok(())
                }
            })
    };
    match result {
        Ok(()) => {
            if dc.0 == WINDOW_DC {
                let serial = env.ext.get::<SurfaceStore>().stats.presentations;
                if let Some(outcome) = (provider.completed)(env, serial) {
                    return outcome;
                }
            }
            Outcome::Return(1)
        }
        Err(error) => Outcome::Fail(format!("PatBlt: {error}")),
    }
}

#[cfg(test)]
mod tests;
