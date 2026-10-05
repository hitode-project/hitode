//! FillRect is a USER32 front over the existing GDI brush and device surface.
use crate::{Win32, util::set_last_error};
use adv_abi::{Env, GuestPtr, Handle, Outcome, Registry, types::RECT, win32};
use adv_cpu::backend::Prot;
use adv_device::surface::{PixelFormat, Rect, SurfaceStore};
use adv_os::gdi::{GdiObj, WINDOW_DC, stock_object};

pub(super) fn register(registry: &mut Registry) {
    registry.register(fill_ABI);
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
    if adv_abi::guest::check_range(env.cpu, rectangle.addr(), 16, Prot::READ).is_err() {
        return fail(env, 87);
    }
    let Ok(rectangle) = rectangle.read(env.cpu) else {
        return fail(env, 87);
    };
    let stock = brush.0.checked_sub(stock_object(0));
    let color = match stock {
        Some(0) => 0xffffff,
        Some(1) => 0xc0c0c0,
        Some(2) => 0x808080,
        Some(3) => 0x404040,
        Some(4) => 0,
        Some(5) => return Outcome::Return(1), // NULL_BRUSH is a valid no-op.
        _ => match env.ext.get::<Win32>().gdi.get(&env.sys.objects, brush.0) {
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
    let left = rectangle.left.max(0).min(surface.width as i32) as u32;
    let top = rectangle.top.max(0).min(surface.height as i32) as u32;
    let right = rectangle.right.max(0).min(surface.width as i32) as u32;
    let bottom = rectangle.bottom.max(0).min(surface.height as i32) as u32;
    if right <= left || bottom <= top {
        return Outcome::Return(1);
    }
    let bytes = surface.format.bytes();
    // Validate every touched row before the first mutation, including bottom-up
    // DIBs and padding. No read permission or unrelated prefix is required.
    for y in top..bottom {
        let row = if surface.bottom_up {
            surface.height - y - 1
        } else {
            y
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
    // GDI converts COLORREF into 16-bit DIB channels by bit truncation. The
    // neutral device conversion uses scaled channels for other frontends.
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
        Err(error) => Outcome::Fail(format!("FillRect: {error}")),
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
        selected,
        modes,
        ..
    }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0)
    else {
        return Outcome::Return(CLR_INVALID);
    };
    if modes.map != 1 {
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
fn pat_blt(
    env: &mut Env<'_>,
    dc: Handle,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    rop: u32,
) -> Outcome {
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
    let brush_handle = match rop {
        BLACKNESS => stock_object(4), // BLACK_BRUSH
        WHITENESS => stock_object(0), // WHITE_BRUSH
        _ => selected[0],            // Selected brush in the DC
    };
    let stock = brush_handle.checked_sub(stock_object(0));
    let color = match stock {
        Some(0) => 0xffffff,
        Some(1) => 0xc0c0c0,
        Some(2) => 0x808080,
        Some(3) => 0x404040,
        Some(4) => 0,
        Some(5) => return Outcome::Return(1), // NULL_BRUSH is a valid no-op.
        _ => match env.ext.get::<Win32>().gdi.get(&env.sys.objects, brush_handle) {
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
    let left = x.max(0).min(surface.width as i32) as u32;
    let top = y.max(0).min(surface.height as i32) as u32;
    let right = (x.saturating_add(w)).max(0).min(surface.width as i32) as u32;
    let bottom = (y.saturating_add(h)).max(0).min(surface.height as i32) as u32;
    if right <= left || bottom <= top {
        return Outcome::Return(1);
    }
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
        let mut memory = (provider.memory)(env.cpu);
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
