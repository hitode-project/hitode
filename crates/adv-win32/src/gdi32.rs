//! GDI object semantics over the process object table. Raster and text commands
//! translate into the shared device implementation.

use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};

use crate::Win32;

pub mod font;
mod palette;
pub mod raster;

#[cfg(test)]
mod object_tests {
    use super::*;
    use adv_machine::{ObjectLimits, Objects};

    #[test]
    fn common_ownership_keeps_external_dc_selection_and_kernel_handles_separate() {
        let mut objects = Objects::with_limits(ObjectLimits {
            references: 1,
            ..ObjectLimits::default()
        });
        let mut gdi = Gdi::default();
        let kernel = objects.insert_host(42_u32);
        assert!(gdi.register_external_dc(&mut objects, kernel, 7));
        gdi.set_external_bitmap(&mut objects, kernel, 123);
        assert!(gdi.register_external_dc(&mut objects, kernel, 8));
        assert!(
            matches!(gdi.get(&objects, kernel), Some(GdiObj::Dc { hwnd: 7, selected, .. }) if selected[3] == 123)
        );
        let brush = gdi.add(&mut objects, GdiObj::Brush { color: 0xff });
        assert_eq!(brush, 0x04000004);
        assert!(objects.close(brush).is_none());
        assert!(!gdi.register_external_dc(&mut objects, brush, 9));
        assert!(gdi.remove_external_dc(&mut objects, kernel));
        assert_eq!(objects.host::<u32>(kernel), Some(&42));
        assert_eq!(objects.all_hosts::<GdiObj>().count(), 1);
        assert!(matches!(
            gdi.remove(&mut objects, brush),
            Some(GdiObj::Brush { color: 0xff })
        ));
        assert!(!gdi.remove_external_dc(&mut objects, kernel));
        assert_eq!(objects.object_count(), 1);
    }

    #[test]
    fn exhausted_gdi_ownership_does_not_publish_a_handle() {
        let mut objects = Objects::with_limits(ObjectLimits {
            objects: 0,
            ..ObjectLimits::default()
        });
        let mut gdi = Gdi::default();
        assert_eq!(gdi.add(&mut objects, GdiObj::Region), 0);
        assert!(!gdi.register_external_dc(&mut objects, 1, 7));
        assert_eq!(objects.object_count(), 0);
    }
}

const GDI32: &str = "gdi32.dll"; // Shared by the palette frontend.

/// Stock objects (GetStockObject index i) have fixed handles.
const STOCK_BASE: u32 = 0x0190_0010;
const STOCK_COUNT: u32 = 20;
const SYSTEM_FONT: u32 = 13;

pub use adv_os::gdi::{Gdi, GdiObj, LogFont, stock_object};

fn add(env: &mut Env<'_>, object: GdiObj) -> u32 {
    env.ext
        .get_mut::<Win32>()
        .gdi
        .add(&mut env.sys.objects, object)
}
fn get<'a>(env: &'a Env<'_>, handle: u32) -> Option<&'a GdiObj> {
    env.ext.get::<Win32>().gdi.get(&env.sys.objects, handle)
}
fn get_mut<'a>(env: &'a mut Env<'_>, handle: u32) -> Option<&'a mut GdiObj> {
    env.ext
        .get::<Win32>()
        .gdi
        .get_mut(&mut env.sys.objects, handle)
}

/// A DC for a window (GetDC, BeginPaint).
pub fn window_dc(env: &mut Env<'_>, hwnd: u32) -> u32 {
    let selected = [
        stock_object(0),
        stock_object(7),
        stock_object(SYSTEM_FONT),
        0,
    ];
    add(
        env,
        GdiObj::Dc {
            raster: false,
            hwnd,
            selected,
            palette: stock_object(15),
            text_color: 0,
            bk_color: 0xff_ffff,
            bk_mode: 2,
            modes: adv_os::gdi::DcModes::default(),
        },
    )
}

pub fn release_dc(env: &mut Env<'_>, hdc: u32) -> bool {
    env.ext
        .get_mut::<Win32>()
        .gdi
        .remove_external_dc(&mut env.sys.objects, hdc)
}

/// A font object with the given height and face (for dialog fonts).
pub fn create_font(env: &mut Env<'_>, height: i32, face: &str) -> u32 {
    let mut raw = vec![0u8; 60];
    raw[0..4].copy_from_slice(&height.to_le_bytes());
    raw[16..20].copy_from_slice(&400i32.to_le_bytes());
    let bytes = env.ext.get::<Win32>().codec.encode(face);
    let n = bytes.len().min(31);
    raw[28..28 + n].copy_from_slice(&bytes[..n]);
    let f = LogFont {
        height,
        width: 0,
        weight: 400,
        italic: false,
        charset: 1,
        face: face.into(),
        raw,
    };
    add(env, GdiObj::Font(f))
}

#[win32(gdi32, name = "GetStockObject", level = state)]
fn get_stock_object(_env: &mut Env<'_>, index: i32) -> Outcome {
    let i = index as u32;
    Outcome::Return(if i < STOCK_COUNT { stock_object(i) } else { 0 })
}

#[win32(gdi32, name = "CreateFontIndirectA", level = implement)]
fn create_font_indirect_a(env: &mut Env<'_>, pointer: GuestPtr<[u8; 60]>) -> Outcome {
    let mut raw = vec![0; 60];
    if adv_abi::guest::check_range(env.cpu, pointer.addr(), 60, Prot::READ).is_err()
        || env.cpu.read(pointer.addr(), &mut raw).is_err()
    {
        return Outcome::Return(0);
    }
    Outcome::Return(add_logfont(env, raw))
}

fn add_logfont(env: &mut Env<'_>, raw: Vec<u8>) -> u32 {
    let i32_at = |o: usize| i32::from_le_bytes(raw[o..o + 4].try_into().unwrap());
    let face_end = raw[28..]
        .iter()
        .position(|&b| b == 0)
        .map_or(60, |n| 28 + n);
    let face = env.ext.get::<Win32>().codec.decode(&raw[28..face_end]);
    let f = LogFont {
        height: i32_at(0),
        width: i32_at(4),
        weight: i32_at(16),
        italic: raw[20] != 0,
        charset: raw[23],
        face,
        raw,
    };
    log::debug!("CreateFont({} h{} w{})", f.face, f.height, f.weight);
    add(env, GdiObj::Font(f))
}

#[win32(gdi32, name = "CreateFontA", level = implement)]
#[expect(clippy::too_many_arguments, reason = "SDK CreateFontA fields")]
fn create_font_a(
    env: &mut Env<'_>,
    height: i32,
    width: i32,
    escapement: i32,
    orientation: i32,
    weight: i32,
    italic: u32,
    underline: u32,
    strikeout: u32,
    charset: u32,
    precision: u32,
    clip: u32,
    quality: u32,
    pitch: u32,
    face: AnsiStr,
) -> Outcome {
    let mut raw = vec![0; 60];
    for (slot, value) in raw[..20].as_chunks_mut::<4>().0.iter_mut().zip([
        height,
        width,
        escapement,
        orientation,
        weight,
    ]) {
        *slot = value.to_le_bytes();
    }
    for (slot, value) in raw[20..28].iter_mut().zip([
        italic, underline, strikeout, charset, precision, clip, quality, pitch,
    ]) {
        *slot = value as u8;
    }
    if face.0.addr() != 0 {
        let Ok(face) = face.read(env.cpu, 4096) else {
            return Outcome::Return(0);
        };
        let length = face.len().min(31);
        raw[28..28 + length].copy_from_slice(&face[..length]);
    }
    Outcome::Return(add_logfont(env, raw))
}
#[win32(gdi32, name = "CreateSolidBrush", level = implement)]
fn create_solid_brush(env: &mut Env<'_>, color: u32) -> Outcome {
    Outcome::Return(add(env, GdiObj::Brush { color }))
}
#[win32(gdi32, name = "CreatePen", level = implement)]
fn create_pen(env: &mut Env<'_>, _style: i32, _width: i32, color: u32) -> Outcome {
    Outcome::Return(add(env, GdiObj::Pen { color }))
}

pub(super) fn delete_object_value(env: &mut Env<'_>, h: u32) -> Outcome {
    if (STOCK_BASE..STOCK_BASE + STOCK_COUNT).contains(&h)
        || crate::user32::system_colors::is_system_brush(h)
    {
        return Outcome::Return(1);
    }
    if env.sys.objects.all_hosts::<GdiObj>()
        .any(|(_, object)| matches!(object, GdiObj::Dc { selected, palette, .. } if selected.contains(&h) || *palette == h))
    {
        return Outcome::Return(0);
    }
    match env.ext.get::<Win32>().gdi.remove(&mut env.sys.objects, h) {
        Some(GdiObj::Bitmap {
            bits, owned: true, ..
        }) => {
            let _ = env.sys.vm.free(env.cpu, bits);
            Outcome::Return(1)
        }
        Some(_) => Outcome::Return(1),
        None => Outcome::Return(0),
    }
}

/// Slot in a DC's selection for an object: brush, pen, font, bitmap.
fn slot_of(env: &Env<'_>, h: u32) -> Option<usize> {
    if (STOCK_BASE..STOCK_BASE + STOCK_COUNT).contains(&h) {
        if h == stock_object(15) {
            return None;
        }
        return Some(match h - STOCK_BASE {
            0..=5 | 18 => 0,
            6..=8 | 19 => 1,
            _ => 2,
        });
    }
    match get(env, h)? {
        GdiObj::Brush { .. } => Some(0),
        GdiObj::Pen { .. } => Some(1),
        GdiObj::Font(_) => Some(2),
        GdiObj::Bitmap { .. } => Some(3),
        _ => None,
    }
}

pub(super) fn select_object_value(env: &mut Env<'_>, hdc: u32, h: u32) -> Outcome {
    let Some(slot) = slot_of(env, h) else {
        return Outcome::Return(0);
    };
    match get_mut(env, hdc) {
        Some(GdiObj::Dc { selected, .. }) => {
            Outcome::Return(std::mem::replace(&mut selected[slot], h))
        }
        _ => Outcome::Return(0),
    }
}

#[win32(gdi32, name = "GetObjectA", level = implement)]
fn get_object_a(env: &mut Env<'_>, object: Handle, count: i32, output: GuestPtr<()>) -> Outcome {
    let (h, size, out) = (object.0, count.max(0) as u32, output.addr());
    if let Some(entries) = palette::entries(env, h) {
        let count = entries.len() as u16;
        if out == 0 {
            return Outcome::Return(2);
        }
        if size < 2 || adv_abi::guest::check_range(env.cpu, out, 2, Prot::WRITE).is_err() {
            return Outcome::Return(0);
        }
        return Outcome::Return(if env.cpu.write(out, &count.to_le_bytes()).is_ok() {
            2
        } else {
            0
        });
    }
    let bytes: Vec<u8> = match get(env, h) {
        Some(GdiObj::Font(f)) => f.raw.clone(),
        Some(GdiObj::Bitmap {
            width,
            height,
            bpp,
            bits,
            stride,
            ..
        }) => {
            // BITMAP: bmType, bmWidth, bmHeight, bmWidthBytes, bmPlanes, bmBitsPixel, bmBits.
            let mut b = Vec::with_capacity(24);
            for v in [0u32, *width as u32, height.unsigned_abs(), *stride] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b.extend_from_slice(&1u16.to_le_bytes());
            b.extend_from_slice(&bpp.to_le_bytes());
            b.extend_from_slice(&bits.to_le_bytes());
            b
        }
        Some(GdiObj::Brush { color }) => [0u32, *color, 0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect(),
        _ => return Outcome::Return(0),
    };
    if out == 0 {
        return Outcome::Return(bytes.len() as u32);
    }
    let n = bytes.len().min(size as usize);
    Outcome::Return(
        if adv_abi::guest::check_range(env.cpu, out, n as u32, Prot::WRITE).is_ok()
            && env.cpu.write(out, &bytes[..n]).is_ok()
        {
            n as u32
        } else {
            0
        },
    )
}

fn new_bitmap(env: &mut Env<'_>, width: i32, height: i32, bpp: u16) -> Option<(u32, u32)> {
    if width < 0 || height < 0 {
        return None;
    }
    let (width, height, bpp) = if width == 0 || height == 0 {
        (1, 1, 1)
    } else {
        (width, height, bpp)
    };
    let stride = (width as u32)
        .checked_mul(u32::from(bpp))?
        .div_ceil(32)
        .checked_mul(4)?;
    let size = stride.checked_mul(height as u32)?;
    let bits = env.sys.vm.alloc(env.cpu, size, Prot::RW).ok()?;
    let h = add(
        env,
        GdiObj::Bitmap {
            width,
            height,
            bpp,
            bits,
            stride,
            owned: true,
            dib_colors: 0,
        },
    );
    if h == 0 {
        let _ = env.sys.vm.free(env.cpu, bits);
        return None;
    }
    Some((h, bits))
}

#[win32(gdi32, name = "CreateBitmap", level = implement)]
fn create_bitmap(
    env: &mut Env<'_>,
    w: i32,
    h: i32,
    planes: u32,
    depth: u32,
    input: GuestPtr<()>,
) -> Outcome {
    if planes != 1 || !matches!(depth, 1 | 4 | 8 | 16 | 24 | 32) || w < 0 || h < 0 {
        return Outcome::Return(0);
    }
    let (bpp, init) = (depth as u16, input.addr());
    if w == 0 || h == 0 {
        // Native returns a 1x1 monochrome bitmap. There are no counted source
        // rows to inspect, even when the caller supplies a non-null pointer.
        return Outcome::Return(new_bitmap(env, w, h, bpp).map_or(0, |(bitmap, _)| bitmap));
    }
    let Some(row_bits) = (w as u32).checked_mul(depth) else {
        return Outcome::Return(0);
    };
    let Some(src_stride) = row_bits.div_ceil(16).checked_mul(2) else {
        return Outcome::Return(0);
    };
    let Some(dst_stride) = row_bits.div_ceil(32).checked_mul(4) else {
        return Outcome::Return(0);
    };
    let Some(source_length) = src_stride.checked_mul(h as u32) else {
        return Outcome::Return(0);
    };
    // Guest::read may bypass page permissions on a host-backed CPU. Validate
    // every source row as one region before allocating or publishing the bitmap.
    if init != 0
        && (init.checked_add(source_length).is_none()
            || adv_abi::guest::check_range(env.cpu, init, source_length, Prot::READ).is_err())
    {
        return Outcome::Return(0);
    }
    let Some((hbm, bits)) = new_bitmap(env, w, h, bpp) else {
        return Outcome::Return(0);
    };
    if init != 0 {
        // Source rows are WORD-aligned; ours DWORD-aligned.
        let mut row = vec![0; src_stride as usize];
        for y in 0..h.unsigned_abs() {
            let source = init
                .checked_add(y * src_stride)
                .expect("validated counted source");
            if env.cpu.read(source, &mut row).is_err()
                || env.cpu.write(bits + y * dst_stride, &row).is_err()
            {
                delete_object_value(env, hbm);
                return Outcome::Return(0);
            }
        }
    }
    Outcome::Return(hbm)
}
#[win32(gdi32, name = "CreateCompatibleBitmap", level = implement)]
fn create_compatible_bitmap(env: &mut Env<'_>, _dc: Handle, w: i32, h: i32) -> Outcome {
    Outcome::Return(new_bitmap(env, w, h, 32).map_or(0, |(h, _)| h))
}
#[win32(gdi32, name = "GetDeviceCaps", level = state)]
fn get_device_caps(env: &mut Env<'_>, _dc: Handle, index: i32) -> Outcome {
    let (w, h) = env.ext.get::<Win32>().user.screen;
    Outcome::Return(match index {
        8 => w as u32,
        10 => h as u32,
        12 => 32,
        14 => 1,
        24 => u32::MAX,
        88 | 90 => 96,
        116 => 60,
        118 => w as u32,
        117 => h as u32,
        n => {
            log::debug!("GetDeviceCaps({n}) = 0");
            0
        }
    })
}

fn dc_field(
    env: &mut Env<'_>,
    hdc: u32,
    f: impl FnOnce(&mut u32, &mut u32, &mut u32) -> u32,
) -> Outcome {
    match get_mut(env, hdc) {
        Some(GdiObj::Dc {
            text_color,
            bk_color,
            bk_mode,
            ..
        }) => Outcome::Return(f(text_color, bk_color, bk_mode)),
        _ => Outcome::Return(0),
    }
}

#[win32(gdi32, name = "SetTextColor", level = state)]
fn set_text_color(env: &mut Env<'_>, dc: Handle, color: u32) -> Outcome {
    dc_field(env, dc.0, |t, _, _| std::mem::replace(t, color))
}
#[win32(gdi32, name = "SetBkColor", level = state)]
fn set_bk_color(env: &mut Env<'_>, dc: Handle, color: u32) -> Outcome {
    dc_field(env, dc.0, |_, b, _| std::mem::replace(b, color))
}
#[win32(gdi32, name = "SetBkMode", level = state)]
fn set_bk_mode(env: &mut Env<'_>, dc: Handle, mode: i32) -> Outcome {
    // Native GDI preserves even nonstandard integer modes. Drawing validates
    // the supported behavior separately instead of changing this state contract.
    dc_field(env, dc.0, |_, _, m| std::mem::replace(m, mode as u32))
}

pub fn register(reg: &mut Registry) {
    palette::register(reg);
    for api in [
        get_stock_object_ABI,
        create_font_indirect_a_ABI,
        create_font_a_ABI,
        create_solid_brush_ABI,
        create_pen_ABI,
        get_object_a_ABI,
        create_bitmap_ABI,
        create_compatible_bitmap_ABI,
        get_device_caps_ABI,
        set_text_color_ABI,
        set_bk_color_ABI,
        set_bk_mode_ABI,
    ] {
        reg.register(api);
    }
    font::register(reg);
    raster::register(reg);
}
