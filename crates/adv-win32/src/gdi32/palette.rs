//! Logical palettes on the provider's 32-bit true-colour display. Entries are
//! real shared GDI objects; selecting a palette does not copy its contents.
use adv_abi::{Handle, Registry, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};

use super::{GDI32, GdiObj, add, get, get_mut, stock_object};
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

#[win32(gdi32, name = "CreateHalftonePalette", level = implement)]
fn create_halftone(env: &mut Env<'_>, _dc: Handle) -> Handle {
    // This true-colour provider uses a device-independent 256-entry palette.
    // Interior ordering is device-specific; the static system colours occupy
    // the first and last ten slots, with a colour cube and greys between them.
    // Native Windows also accepts NULL/stale DCs for this independent palette.
    let mut entries = Vec::with_capacity(256);
    entries.extend_from_slice(&DEFAULT[..10]);
    for blue in 0..6 {
        for green in 0..6 {
            for red in 0..6 {
                entries.push([red * 51, green * 51, blue * 51, 4]);
            }
        }
    }
    for shade in 1..=20 {
        let value = (shade * 255 / 21) as u8;
        entries.push([value, value, value, 4]);
    }
    entries.extend_from_slice(&DEFAULT[10..]);
    Handle(add(env, GdiObj::Palette { entries }))
}
pub(super) fn entries<'a>(env: &'a Env<'_>, palette: u32) -> Option<&'a [[u8; 4]]> {
    if palette == stock_object(15) {
        return Some(&DEFAULT);
    }
    match get(env, palette)? {
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
    let entries = raw.as_chunks::<4>().0.to_vec();
    Outcome::Return(add(env, GdiObj::Palette { entries }))
}
fn get_entries(env: &mut Env<'_>) -> Outcome {
    let (palette, start, count, out) = (
        env.arg(0),
        env.arg(1) as usize,
        env.arg(2) as usize,
        env.arg(3),
    );
    let Some(palette) = entries(env, palette) else {
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
    let Some(GdiObj::Palette { entries }) = get(env, handle) else {
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
    let Some(GdiObj::Palette { entries }) = get_mut(env, handle) else {
        return Outcome::Return(0);
    };
    for (entry, bytes) in entries[start..start + n]
        .iter_mut()
        .zip(raw.as_chunks::<4>().0)
    {
        *entry = *bytes;
    }
    Outcome::Return(n as u32)
}
fn select(env: &mut Env<'_>) -> Outcome {
    let (dc, palette) = (env.arg(0), env.arg(1));
    if entries(env, palette).is_none() {
        return Outcome::Return(0);
    }
    match get_mut(env, dc) {
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
    Outcome::Return(if matches!(get(env, dc), Some(GdiObj::Dc { .. })) {
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
    match get_mut(env, palette) {
        Some(GdiObj::Palette { entries }) => {
            entries.resize(count, [0; 4]);
            Outcome::Return(1)
        }
        _ => Outcome::Return(0),
    }
}
fn nearest(env: &mut Env<'_>) -> Outcome {
    let (palette, color) = (env.arg(0), env.arg(1));
    let Some(palette) = entries(env, palette) else {
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
    }) = get(env, dc)
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
    reg.register(create_halftone_ABI);
    for (name, argc, handler) in [
        ("CreatePalette", 1, create as adv_machine::Handler),
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

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, Reg},
    };
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateHalftonePalette(dc: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        fn GetPaletteEntries(
            palette: *mut core::ffi::c_void,
            start: u32,
            count: u32,
            output: *mut u8,
        ) -> u32;
        fn DeleteObject(object: *mut core::ffi::c_void) -> i32;
    }

    #[test]
    fn halftone_palette_has_native_static_colours_and_shared_object_lifetime() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/halftone-tests");
        crate::Win32::install(
            &mut machine,
            crate::Win32Config::new(&root, "authored.exe", &root),
        )
        .unwrap();
        machine.cpu.map(0x410000, 4096, Prot::RW).unwrap();
        machine.cpu.set_reg(Reg::Esp, 0x410000);
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        for dc in [0, 0xdeadbeef] {
            // SAFETY: Windows accepts these DC values; the returned palette is
            // inspected in a 256-entry buffer and released before leaving.
            let native = unsafe { CreateHalftonePalette(dc as usize as *mut _) };
            assert!(!native.is_null());
            let mut native_entries = [[0u8; 4]; 256];
            assert_eq!(
                unsafe { GetPaletteEntries(native, 0, 256, native_entries.as_mut_ptr().cast()) },
                256
            );
            let palette = create_halftone(&mut env, Handle(dc));
            let actual = entries(&env, palette.0).unwrap();
            assert_eq!(actual.len(), 256);
            assert_eq!(&actual[..10], &native_entries[..10]);
            assert_eq!(&actual[246..], &native_entries[246..]);
            assert_eq!(unsafe { DeleteObject(native) }, 1);
            let other = create_halftone(&mut env, Handle(dc));
            assert_ne!(palette.0, other.0);
            let private_dc = super::super::window_dc(&mut env, 0);
            for (index, argument) in [private_dc, palette.0, 0].into_iter().enumerate() {
                env.cpu
                    .write_u32(0x410004 + index as u32 * 4, argument)
                    .unwrap();
            }
            assert!(
                matches!(select(&mut env), Outcome::Return(value) if value == stock_object(15))
            );
            env.cpu.write_u32(0x410004, palette.0).unwrap();
            assert!(matches!(
                super::super::delete_object_value(&mut env, palette.0),
                Outcome::Return(0)
            ));
            env.cpu.write_u32(0x410004, private_dc).unwrap();
            env.cpu.write_u32(0x410008, stock_object(15)).unwrap();
            assert!(matches!(select(&mut env), Outcome::Return(value) if value == palette.0));
            env.cpu.write_u32(0x410004, palette.0).unwrap();
            assert!(matches!(
                super::super::delete_object_value(&mut env, palette.0),
                Outcome::Return(1)
            ));
            assert!(entries(&env, palette.0).is_none());
            assert!(entries(&env, other.0).is_some());
        }
    }
}
