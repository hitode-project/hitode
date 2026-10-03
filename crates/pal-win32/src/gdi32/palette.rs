//! Logical palettes on the provider's 32-bit true-colour display. Entries are
//! real shared GDI objects; selecting a palette does not copy its contents.
use pal_cpu::{GuestExt, Prot};
use pal_machine::{Env, Outcome, Registry};

use super::{GDI32, Gdi, GdiObj, gdi, stock_object};
use crate::stdcall;

// Windows' immutable 20-entry DEFAULT_PALETTE, confirmed by native GDI.
const DEFAULT: [[u8; 4]; 20] = [
    [0, 0, 0, 0],
    [128, 0, 0, 0],
    [0, 128, 0, 0],
    [128, 128, 0, 0],
    [0, 0, 128, 0],
    [128, 0, 128, 0],
    [0, 128, 128, 0],
    [192, 192, 192, 0],
    [192, 220, 192, 0],
    [166, 202, 240, 0],
    [255, 251, 240, 0],
    [160, 160, 164, 0],
    [128, 128, 128, 0],
    [255, 0, 0, 0],
    [0, 255, 0, 0],
    [255, 255, 0, 0],
    [0, 0, 255, 0],
    [255, 0, 255, 0],
    [0, 255, 255, 0],
    [255, 255, 255, 0],
];
pub(super) fn entries(gdi: &Gdi, palette: u32) -> Option<&[[u8; 4]]> {
    if palette == stock_object(15) {
        return Some(&DEFAULT);
    }
    match gdi.get(palette)? {
        GdiObj::Palette { entries } => Some(entries),
        _ => None,
    }
}
fn create(env: &mut Env<'_>) -> Outcome {
    let ptr = env.arg(0);
    if ptr == 0 || !env.cpu.accessible(ptr, 4, Prot::READ) {
        return Outcome::Return(0);
    }
    let Ok(version) = env.cpu.read_u16(ptr) else {
        return Outcome::Return(0);
    };
    let Ok(count) = env.cpu.read_u16(ptr + 2) else {
        return Outcome::Return(0);
    };
    if version != 0x300 || count == 0 {
        return Outcome::Return(0);
    }
    let len = u32::from(count) * 4;
    let Some(data) = ptr.checked_add(4) else {
        return Outcome::Return(0);
    };
    if data.checked_add(len).is_none() || !env.cpu.accessible(data, len, Prot::READ) {
        return Outcome::Return(0);
    }
    let mut raw = vec![0; len as usize];
    if env.cpu.read(data, &mut raw).is_err() {
        return Outcome::Return(0);
    }
    let entries = raw
        .chunks_exact(4)
        .map(|entry| entry.try_into().unwrap())
        .collect();
    Outcome::Return(gdi(env).add(GdiObj::Palette { entries }))
}
fn get_entries(env: &mut Env<'_>) -> Outcome {
    let (palette, start, count, out) = (
        env.arg(0),
        env.arg(1) as usize,
        env.arg(2) as usize,
        env.arg(3),
    );
    let Some(palette) = entries(gdi(env), palette) else {
        return Outcome::Return(0);
    };
    if out == 0 {
        return Outcome::Return(palette.len() as u32);
    }
    let n = count.min(palette.len().saturating_sub(start));
    if n == 0 {
        return Outcome::Return(0);
    }
    let raw: Vec<_> = palette[start..start + n]
        .iter()
        .flatten()
        .copied()
        .collect();
    if !env.cpu.accessible(out, raw.len() as u32, Prot::WRITE) || env.cpu.write(out, &raw).is_err()
    {
        return Outcome::Return(0);
    }
    Outcome::Return(n as u32)
}
fn set_entries(env: &mut Env<'_>) -> Outcome {
    let (handle, start, count, ptr) = (
        env.arg(0),
        env.arg(1) as usize,
        env.arg(2) as usize,
        env.arg(3),
    );
    let Some(GdiObj::Palette { entries }) = gdi(env).get(handle) else {
        return Outcome::Return(0);
    };
    let n = count.min(entries.len().saturating_sub(start));
    if n == 0 || ptr == 0 || !env.cpu.accessible(ptr, (n * 4) as u32, Prot::READ) {
        return Outcome::Return(0);
    }
    let mut raw = vec![0; n * 4];
    if env.cpu.read(ptr, &mut raw).is_err() {
        return Outcome::Return(0);
    }
    let Some(GdiObj::Palette { entries }) = gdi(env).objects.get_mut(&handle) else {
        return Outcome::Return(0);
    };
    for (entry, bytes) in entries[start..start + n]
        .iter_mut()
        .zip(raw.chunks_exact(4))
    {
        entry.copy_from_slice(bytes);
    }
    Outcome::Return(n as u32)
}
fn select(env: &mut Env<'_>) -> Outcome {
    let (dc, palette) = (env.arg(0), env.arg(1));
    if entries(gdi(env), palette).is_none() {
        return Outcome::Return(0);
    }
    match gdi(env).objects.get_mut(&dc) {
        Some(GdiObj::Dc {
            palette: selected, ..
        }) => Outcome::Return(std::mem::replace(selected, palette)),
        _ => Outcome::Return(0),
    }
}
fn realize(env: &mut Env<'_>) -> Outcome {
    let dc = env.arg(0);
    // The device advertises no RC_PALETTE/SIZEPALETTE: no physical system
    // palette entries change on this true-colour display.
    Outcome::Return(if matches!(gdi(env).get(dc), Some(GdiObj::Dc { .. })) {
        0
    } else {
        u32::MAX
    })
}
fn resize(env: &mut Env<'_>) -> Outcome {
    let (palette, count) = (env.arg(0), env.arg(1) as usize);
    if count == 0 || count > 1024 {
        return Outcome::Return(0);
    }
    match gdi(env).objects.get_mut(&palette) {
        Some(GdiObj::Palette { entries }) => {
            entries.resize(count, [0; 4]);
            Outcome::Return(1)
        }
        _ => Outcome::Return(0),
    }
}
fn nearest(env: &mut Env<'_>) -> Outcome {
    let (palette, color) = (env.arg(0), env.arg(1));
    let Some(palette) = entries(gdi(env), palette) else {
        return Outcome::Return(u32::MAX);
    };
    if color >> 24 == 1 {
        let index = color & 0xffff;
        return Outcome::Return(if index < palette.len() as u32 {
            index
        } else {
            0
        });
    }
    if !matches!(color >> 24, 0 | 2) {
        return Outcome::Return(0);
    }
    let target = color.to_le_bytes();
    let index = palette
        .iter()
        .enumerate()
        .min_by_key(|(_, entry)| {
            (0..3)
                .map(|i| (i32::from(entry[i]) - i32::from(target[i])).pow(2))
                .sum::<i32>()
        })
        .map(|(index, _)| index as u32)
        .unwrap_or(u32::MAX);
    Outcome::Return(index)
}
fn current_object(env: &mut Env<'_>) -> Outcome {
    let (dc, kind) = (env.arg(0), env.arg(1));
    let Some(GdiObj::Dc {
        palette, selected, ..
    }) = gdi(env).get(dc)
    else {
        return Outcome::Return(0);
    };
    Outcome::Return(match kind {
        1 => selected[1],
        2 => selected[0],
        5 => *palette,
        6 => selected[2],
        7 => selected[3],
        _ => 0,
    })
}
pub(super) fn register(reg: &mut Registry) {
    for (name, argc, handler) in [
        ("CreatePalette", 1, create as pal_machine::Handler),
        ("SelectPalette", 3, select),
        ("RealizePalette", 1, realize),
        ("GetPaletteEntries", 4, get_entries),
        ("SetPaletteEntries", 4, set_entries),
        ("ResizePalette", 2, resize),
        ("GetNearestPaletteIndex", 2, nearest),
        ("GetCurrentObject", 2, current_object),
    ] {
        stdcall(reg, GDI32, name, argc, handler);
    }
}
