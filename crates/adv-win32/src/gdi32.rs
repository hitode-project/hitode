//! GDI object semantics over the process object table. Raster and text commands
//! translate into the shared device implementation.

use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_device::surface::SurfaceStore;
use adv_machine::{Env, Outcome};

use crate::Win32;

pub mod font;
mod batch;
mod palette;
mod pen;
mod readback;
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
            clip: Default::default(),
            saved: Vec::new(),
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
    let face_end = raw[28..]
        .iter()
        .position(|&b| b == 0)
        .map_or(60, |n| 28 + n);
    let face = env.ext.get::<Win32>().codec.decode(&raw[28..face_end]);
    add_logfont_with_face(env, raw, face)
}

// Both creation encodings publish the same logical font payload. `raw` remains
// LOGFONTA for GetObjectA; `face` owns the original decoded name for realization.
fn add_logfont_with_face(env: &mut Env<'_>, raw: Vec<u8>, face: String) -> u32 {
    let i32_at = |o: usize| i32::from_le_bytes(raw[o..o + 4].try_into().unwrap());
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
#[win32(gdi32, name = "CreateBrushIndirect", level = implement)]
fn create_brush_indirect(env: &mut Env<'_>, plbrush: GuestPtr<()>) -> Outcome {
    if plbrush.is_null() {
        return Outcome::Return(0);
    }
    if adv_abi::guest::check_range(env.cpu, plbrush.addr(), 12, adv_cpu::backend::Prot::READ).is_err() {
        return Outcome::Return(0);
    }
    let style = env.cpu.read_u32(plbrush.addr()).unwrap_or(0);
    let color = env.cpu.read_u32(plbrush.addr() + 4).unwrap_or(0);
    if style == 1 {
        // BS_HOLLOW / BS_NULL
        return Outcome::Return(stock_object(5));
    }
    Outcome::Return(add(env, GdiObj::Brush { color }))
}
#[win32(gdi32, name = "CreatePen", level = implement)]
fn create_pen(env: &mut Env<'_>, style: i32, width: i32, color: u32) -> Outcome {
    pen::create(env, style as u32, width, color)
}
#[win32(gdi32, name = "UnrealizeObject", level = implement)]
fn unrealize_object(_env: &mut Env<'_>, _h: Handle) -> Outcome {
    Outcome::Return(1)
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
    if let Some(bytes) = pen::describe(env, h) {
        if out == 0 {
            return Outcome::Return(16);
        }
        if size < 16 || adv_abi::guest::check_range(env.cpu, out, 16, Prot::WRITE).is_err() {
            return Outcome::Return(0);
        }
        return Outcome::Return(if env.cpu.write(out, &bytes).is_ok() {
            16
        } else {
            0
        });
    }
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
            mapping_view: None,
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
fn get_device_caps(env: &mut Env<'_>, dc: Handle, index: i32) -> Outcome {
    if !matches!(get(env, dc.0), Some(GdiObj::Dc { .. })) {
        return Outcome::Return(0);
    }
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

#[win32(gdi32, name = "SaveDC", level = implement)]
fn save_dc(env: &mut Env<'_>, dc: Handle) -> Outcome {
    if dc.0 == adv_os::gdi::WINDOW_DC {
        env.ext
            .get_mut::<Win32>()
            .gdi
            .register_external_dc(&mut env.sys.objects, dc.0, 0);
    }
    match get_mut(env, dc.0) {
        Some(GdiObj::Dc {
            selected,
            palette,
            text_color,
            bk_color,
            bk_mode,
            modes,
            saved,
            clip,
            ..
        }) => {
            saved.push(adv_os::gdi::DcState {
                selected: *selected,
                palette: *palette,
                text_color: *text_color,
                bk_color: *bk_color,
                bk_mode: *bk_mode,
                modes: *modes,
                clip: clip.clone(),
            });
            Outcome::Return(saved.len() as u32)
        }
        _ => Outcome::Return(0),
    }
}

/// A positive level restores the state that SaveDC numbered so; a negative one counts back
/// from the newest. Either way the restored state and every newer one leave the stack.
#[win32(gdi32, name = "RestoreDC", level = implement)]
fn restore_dc(env: &mut Env<'_>, dc: Handle, level: i32) -> Outcome {
    let (state, raster, bitmap) = match get_mut(env, dc.0) {
        Some(GdiObj::Dc {
            saved,
            raster,
            selected,
            ..
        }) => {
            let depth = saved.len() as i64;
            let index = if level > 0 {
                i64::from(level) - 1
            } else {
                depth + i64::from(level)
            };
            if level == 0 || !(0..depth).contains(&index) {
                return Outcome::Return(0);
            }
            saved.truncate(index as usize + 1);
            (saved.pop().unwrap(), *raster, selected[3])
        }
        _ => return Outcome::Return(0),
    };
    if raster && state.selected[3] != bitmap {
        raster::front::reselect_bitmap(env, dc.0, state.selected[3]);
    }
    if let Some(GdiObj::Dc {
        selected,
        palette,
        text_color,
        bk_color,
        bk_mode,
        modes,
        clip,
        ..
    }) = get_mut(env, dc.0)
    {
        let kept = selected[3];
        *selected = state.selected;
        if raster {
            selected[3] = kept;
        }
        *palette = state.palette;
        *text_color = state.text_color;
        *bk_color = state.bk_color;
        *bk_mode = state.bk_mode;
        *modes = state.modes;
        *clip = state.clip;
    }
    Outcome::Return(1)
}

#[win32(gdi32, name = "MoveToEx", level = implement)]
fn move_to_ex(
    env: &mut Env<'_>,
    dc: Handle,
    _x: i32,
    _y: i32,
    lppt: GuestPtr<adv_abi::types::POINT>,
) -> Outcome {
    let Some(GdiObj::Dc { .. }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0) else {
        return Outcome::Return(0);
    };
    if !lppt.is_null() {
        if adv_abi::guest::check_range(env.cpu, lppt.addr(), 8, adv_cpu::backend::Prot::WRITE).is_err() {
            return Outcome::Return(0);
        }
        let _ = lppt.write(env.cpu, adv_abi::types::POINT { x: 0, y: 0 });
    }
    Outcome::Return(1)
}

#[win32(gdi32, name = "LineTo", level = implement)]
fn line_to(env: &mut Env<'_>, dc: Handle, _x: i32, _y: i32) -> Outcome {
    let Some(GdiObj::Dc { .. }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0) else {
        return Outcome::Return(0);
    };
    Outcome::Return(1)
}

#[win32(gdi32, name = "GetCurrentPositionEx", level = implement)]
fn get_current_position_ex(
    env: &mut Env<'_>,
    dc: Handle,
    lppt: GuestPtr<adv_abi::types::POINT>,
) -> Outcome {
    let Some(GdiObj::Dc { .. }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0) else {
        return Outcome::Return(0);
    };
    if !lppt.is_null() {
        if adv_abi::guest::check_range(env.cpu, lppt.addr(), 8, adv_cpu::backend::Prot::WRITE).is_err() {
            return Outcome::Return(0);
        }
        let _ = lppt.write(env.cpu, adv_abi::types::POINT { x: 0, y: 0 });
    }
    Outcome::Return(1)
}

#[win32(gdi32, name = "SetDIBits", level = implement)]
fn set_di_bits(
    env: &mut Env<'_>,
    _dc: Handle,
    hbm: Handle,
    start: u32,
    lines: u32,
    bits: GuestPtr<()>,
    info: GuestPtr<()>,
    _usage: u32,
) -> Outcome {
    if hbm.0 == 0 || lines == 0 || bits.is_null() || info.is_null() {
        return Outcome::Return(0);
    }
    let mut header = [0u8; 40];
    if adv_abi::guest::check_range(env.cpu, info.addr(), 40, Prot::READ).is_err()
        || env.cpu.read(info.addr(), &mut header).is_err()
    {
        return Outcome::Return(0);
    }
    let bi_size = u32::from_le_bytes(header[0..4].try_into().unwrap());
    if bi_size < 40 {
        return Outcome::Return(0);
    }
    let bi_width = i32::from_le_bytes(header[4..8].try_into().unwrap());
    let bi_height = i32::from_le_bytes(header[8..12].try_into().unwrap());
    let bi_planes = u16::from_le_bytes(header[12..14].try_into().unwrap());
    let bi_bit_count = u16::from_le_bytes(header[14..16].try_into().unwrap());
    let bi_compression = u32::from_le_bytes(header[16..20].try_into().unwrap());
    let bi_clr_used = u32::from_le_bytes(header[32..36].try_into().unwrap());

    if bi_width <= 0 || bi_height == 0 || bi_planes != 1 {
        return Outcome::Return(0);
    }

    let src_w = bi_width as u32;
    let src_h = bi_height.unsigned_abs();
    let src_bottom_up = bi_height > 0;
    let src_bpp = bi_bit_count;

    if start >= src_h {
        return Outcome::Return(0);
    }
    let count = lines.min(src_h - start);
    if count == 0 {
        return Outcome::Return(0);
    }

    let Some(src_row_bits) = src_w.checked_mul(src_bpp as u32) else {
        return Outcome::Return(0);
    };
    let src_pitch = (src_row_bits + 31) / 32 * 4;
    let Some(src_bytes) = src_pitch.checked_mul(count) else {
        return Outcome::Return(0);
    };
    if adv_abi::guest::check_range(env.cpu, bits.addr(), src_bytes, Prot::READ).is_err() {
        return Outcome::Return(0);
    }

    let mut palette = [[0u8; 4]; 256];
    if src_bpp <= 8 {
        let num_colors = if bi_clr_used > 0 && bi_clr_used <= 256 {
            bi_clr_used as usize
        } else {
            1usize << src_bpp
        };
        let pal_addr = info.addr() + bi_size;
        let mut pal_bytes = vec![0u8; num_colors * 4];
        if env.cpu.read(pal_addr, &mut pal_bytes).is_ok() {
            for (entry, chunk) in palette.iter_mut().zip(pal_bytes.chunks_exact(4)) {
                *entry = [chunk[0], chunk[1], chunk[2], chunk[3]];
            }
        }
    }

    let bitmap_meta = match env.ext.get::<Win32>().gdi.get(&env.sys.objects, hbm.0) {
        Some(GdiObj::Bitmap {
            width,
            height,
            bpp,
            bits: dst_bits,
            stride: dst_pitch,
            ..
        }) => Some((*width, *height, *bpp, *dst_bits, *dst_pitch)),
        _ => None,
    };

    let (dst_w, dst_h, dst_bpp, dst_bits, dst_pitch) = if let Some(meta) = bitmap_meta {
        meta
    } else if let Some(store) = env.ext.try_get::<SurfaceStore>() {
        if let Some(surf) = store.get(hbm.0) {
            (
                surf.width as i32,
                if surf.bottom_up {
                    surf.height as i32
                } else {
                    -(surf.height as i32)
                },
                (surf.format.bytes() * 8) as u16,
                surf.pixels,
                surf.pitch,
            )
        } else {
            return Outcome::Return(0);
        }
    } else {
        return Outcome::Return(0);
    };

    let dest_h = dst_h.unsigned_abs();
    let actual_lines = count.min(dest_h.saturating_sub(start));

    for l in 0..actual_lines {
        let src_line_addr = bits.addr() + l * src_pitch;
        let dst_y = if src_bottom_up {
            dest_h - 1 - (start + l)
        } else {
            start + l
        };
        if dst_y >= dest_h {
            continue;
        }
        let dst_line_addr = dst_bits + dst_y * dst_pitch;

        if src_bpp == dst_bpp {
            let row_len = (src_w * (src_bpp as u32) / 8).min(dst_pitch);
            let mut row = vec![0u8; row_len as usize];
            if env.cpu.read(src_line_addr, &mut row).is_ok() {
                let _ = env.cpu.write(dst_line_addr, &row);
            }
        } else if src_bpp == 24 && dst_bpp == 32 {
            let copy_w = src_w.min(dst_w as u32);
            let mut src_pixels = vec![0u8; (copy_w * 3) as usize];
            if env.cpu.read(src_line_addr, &mut src_pixels).is_ok() {
                let mut dst_pixels = vec![0u8; (copy_w * 4) as usize];
                for (s_pix, d_pix) in src_pixels.chunks_exact(3).zip(dst_pixels.chunks_exact_mut(4)) {
                    d_pix[0] = s_pix[0];
                    d_pix[1] = s_pix[1];
                    d_pix[2] = s_pix[2];
                    d_pix[3] = 0xff;
                }
                let write_len = ((copy_w * 4).min(dst_pitch)) as usize;
                let _ = env.cpu.write(dst_line_addr, &dst_pixels[..write_len]);
            }
        } else if src_bpp == 8 && dst_bpp == 32 {
            let copy_w = src_w.min(dst_w as u32);
            let mut src_indices = vec![0u8; copy_w as usize];
            if env.cpu.read(src_line_addr, &mut src_indices).is_ok() {
                let mut dst_pixels = vec![0u8; (copy_w * 4) as usize];
                for (idx, d_pix) in src_indices.iter().zip(dst_pixels.chunks_exact_mut(4)) {
                    let pal = palette[*idx as usize];
                    d_pix[0] = pal[0];
                    d_pix[1] = pal[1];
                    d_pix[2] = pal[2];
                    d_pix[3] = 0xff;
                }
                let write_len = ((copy_w * 4).min(dst_pitch)) as usize;
                let _ = env.cpu.write(dst_line_addr, &dst_pixels[..write_len]);
            }
        } else if src_bpp == 16 && dst_bpp == 32 {
            let copy_w = src_w.min(dst_w as u32);
            let mut src_words = vec![0u8; (copy_w * 2) as usize];
            if env.cpu.read(src_line_addr, &mut src_words).is_ok() {
                let mut dst_pixels = vec![0u8; (copy_w * 4) as usize];
                let is_565 = bi_compression == 3;
                for (s_pix, d_pix) in src_words.chunks_exact(2).zip(dst_pixels.chunks_exact_mut(4)) {
                    let w = u16::from_le_bytes([s_pix[0], s_pix[1]]);
                    let (r, g, b) = if is_565 {
                        (((w >> 11) & 0x1f) * 255 / 31, ((w >> 5) & 0x3f) * 255 / 63, (w & 0x1f) * 255 / 31)
                    } else {
                        (((w >> 10) & 0x1f) * 255 / 31, ((w >> 5) & 0x1f) * 255 / 31, (w & 0x1f) * 255 / 31)
                    };
                    d_pix[0] = b as u8;
                    d_pix[1] = g as u8;
                    d_pix[2] = r as u8;
                    d_pix[3] = 0xff;
                }
                let write_len = ((copy_w * 4).min(dst_pitch)) as usize;
                let _ = env.cpu.write(dst_line_addr, &dst_pixels[..write_len]);
            }
        } else if src_bpp == 32 && dst_bpp == 24 {
            let copy_w = src_w.min(dst_w as u32);
            let mut src_pixels = vec![0u8; (copy_w * 4) as usize];
            if env.cpu.read(src_line_addr, &mut src_pixels).is_ok() {
                let mut dst_pixels = vec![0u8; (copy_w * 3) as usize];
                for (s_pix, d_pix) in src_pixels.chunks_exact(4).zip(dst_pixels.chunks_exact_mut(3)) {
                    d_pix[0] = s_pix[0];
                    d_pix[1] = s_pix[1];
                    d_pix[2] = s_pix[2];
                }
                let write_len = ((copy_w * 3).min(dst_pitch)) as usize;
                let _ = env.cpu.write(dst_line_addr, &dst_pixels[..write_len]);
            }
        }
    }

    if let Some(store) = env.ext.try_get_mut::<SurfaceStore>() {
        if let Some(surf) = store.get_mut(hbm.0) {
            surf.version = surf.version.wrapping_add(1);
        }
    }

    Outcome::Return(actual_lines)
}

#[win32(gdi32, name = "CreateDIBitmap", level = implement)]
fn create_di_bitmap(
    env: &mut Env<'_>,
    hdc: Handle,
    pbmih: GuestPtr<()>,
    fl_init: u32,
    pj_bits: GuestPtr<()>,
    pbmi: GuestPtr<()>,
    usage: u32,
) -> Outcome {
    if pbmih.is_null() {
        return Outcome::Return(0);
    }
    let mut header = [0u8; 40];
    if adv_abi::guest::check_range(env.cpu, pbmih.addr(), 40, Prot::READ).is_err()
        || env.cpu.read(pbmih.addr(), &mut header).is_err()
    {
        return Outcome::Return(0);
    }
    let width = i32::from_le_bytes(header[4..8].try_into().unwrap());
    let height = i32::from_le_bytes(header[8..12].try_into().unwrap());
    let hbm = new_bitmap(env, width.abs(), height.abs(), 32).map_or(0, |(h, _)| h);
    if hbm == 0 {
        return Outcome::Return(0);
    }
    // CBM_INIT = 4
    if (fl_init & 4) != 0 && !pj_bits.is_null() && !pbmi.is_null() {
        let lines = height.unsigned_abs();
        set_di_bits(env, hdc, Handle(hbm), 0, lines, pj_bits, pbmi, usage);
    }
    Outcome::Return(hbm)
}

pub fn register(reg: &mut Registry) {
    batch::register(reg);
    palette::register(reg);
    pen::register(reg);
    readback::register(reg);
    for api in [
        get_stock_object_ABI,
        create_font_indirect_a_ABI,
        create_font_a_ABI,
        create_solid_brush_ABI,
        create_brush_indirect_ABI,
        create_pen_ABI,
        get_object_a_ABI,
        create_bitmap_ABI,
        create_compatible_bitmap_ABI,
        get_device_caps_ABI,
        set_text_color_ABI,
        set_bk_color_ABI,
        set_bk_mode_ABI,
        save_dc_ABI,
        restore_dc_ABI,
        move_to_ex_ABI,
        line_to_ABI,
        get_current_position_ex_ABI,
        unrealize_object_ABI,
        set_di_bits_ABI,
        create_di_bitmap_ABI,
    ] {
        reg.register(api);
    }
    font::register(reg);
    raster::register(reg);
}
