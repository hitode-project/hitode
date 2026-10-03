//! gdi32: the GDI object table (fonts, brushes, bitmaps, DCs) with enough behavior for
//! Game.exe's setup code. Actual drawing belongs to PAL's renderer, not GDI, so drawing
//! calls only keep state.

use std::collections::HashMap;

use adv_machine::{Env, Outcome, Registry};
use pal_cpu::{GuestExt, Prot};

use crate::util::{cstr, out_u32, zero};
use crate::{Win32, stdcall};

mod palette;

const GDI32: &str = "gdi32.dll";

/// Stock objects (GetStockObject index i) have fixed handles.
const STOCK_BASE: u32 = 0x0190_0010;
const STOCK_COUNT: u32 = 20;
const SYSTEM_FONT: u32 = 13;

#[derive(Clone, Debug)]
pub struct LogFont {
    pub height: i32,
    pub width: i32,
    pub weight: i32,
    pub italic: bool,
    pub charset: u8,
    pub face: String,
    /// The raw LOGFONTA for GetObject.
    pub raw: Vec<u8>,
}

#[derive(Clone, Debug)]
pub enum GdiObj {
    Font(LogFont),
    Brush {
        color: u32,
    },
    Pen {
        color: u32,
    },
    Bitmap {
        width: i32,
        height: i32,
        bpp: u16,
        bits: u32,
        stride: u32,
        owned: bool,
    },
    Palette {
        entries: Vec<[u8; 4]>,
    },
    Dc {
        hwnd: u32,
        selected: [u32; 4],
        palette: u32,
        text_color: u32,
        bk_color: u32,
        bk_mode: u32,
    },
    Region,
}

#[derive(Default)]
pub struct Gdi {
    objects: HashMap<u32, GdiObj>,
    next: u32,
}

impl Gdi {
    pub fn add(&mut self, o: GdiObj) -> u32 {
        // Handles carry a type tag in bits 16..23 on real GDI; keep them distinct from
        // stock handles and USER handles.
        self.next = self.next.max(0x0400_0000) + 4;
        self.objects.insert(self.next, o);
        self.next
    }
    pub fn get(&self, h: u32) -> Option<&GdiObj> {
        self.objects.get(&h)
    }

    /// Attach GDI metadata to a DC owned by another drawing provider. Repeated
    /// GetDC calls preserve its logical selections instead of resetting them.
    pub fn register_external_dc(&mut self, handle: u32, hwnd: u32) -> bool {
        if handle == 0 {
            return false;
        }
        if let Some(object) = self.objects.get(&handle) {
            return matches!(object, GdiObj::Dc { .. });
        }
        let selected = [
            stock_object(0),
            stock_object(7),
            stock_object(SYSTEM_FONT),
            0,
        ];
        self.objects.insert(
            handle,
            GdiObj::Dc {
                hwnd,
                selected,
                palette: stock_object(15),
                text_color: 0,
                bk_color: 0xff_ffff,
                bk_mode: 2,
            },
        );
        true
    }

    /// Retire the logical selections when the drawing provider destroys a DC.
    pub fn remove_external_dc(&mut self, handle: u32) -> bool {
        if !matches!(self.get(handle), Some(GdiObj::Dc { .. })) {
            return false;
        }
        self.objects.remove(&handle);
        true
    }

    /// Keep a drawing provider's externally stored selection visible to GDI queries.
    pub fn set_external_bitmap(&mut self, handle: u32, bitmap: u32) {
        if let Some(GdiObj::Dc { selected, .. }) = self.objects.get_mut(&handle) {
            selected[3] = bitmap;
        }
    }
}

pub fn stock_object(i: u32) -> u32 {
    STOCK_BASE + i
}

fn gdi<'a>(env: &'a mut Env<'_>) -> &'a mut Gdi {
    &mut env.ext.get_mut::<Win32>().gdi
}

/// A DC for a window (GetDC, BeginPaint).
pub fn window_dc(env: &mut Env<'_>, hwnd: u32) -> u32 {
    let selected = [
        stock_object(0),
        stock_object(7),
        stock_object(SYSTEM_FONT),
        0,
    ];
    gdi(env).add(GdiObj::Dc {
        hwnd,
        selected,
        palette: stock_object(15),
        text_color: 0,
        bk_color: 0xff_ffff,
        bk_mode: 2,
    })
}

pub fn release_dc(env: &mut Env<'_>, hdc: u32) -> bool {
    gdi(env).remove_external_dc(hdc)
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
    gdi(env).add(GdiObj::Font(f))
}

fn get_stock_object(env: &mut Env<'_>) -> Outcome {
    let i = env.arg(0);
    Outcome::Return(if i < STOCK_COUNT { stock_object(i) } else { 0 })
}

fn create_font_indirect_a(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let mut raw = vec![0u8; 60];
    if env.cpu.read(p, &mut raw).is_err() {
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
    gdi(env).add(GdiObj::Font(f))
}

fn create_font_a(env: &mut Env<'_>) -> Outcome {
    // CreateFontA's 14 arguments are LOGFONTA's fields in order.
    let mut raw = vec![0u8; 60];
    for i in 0..5 {
        let v = env.arg(i);
        raw[4 * i as usize..4 * i as usize + 4].copy_from_slice(&v.to_le_bytes());
    }
    for i in 0..8 {
        raw[20 + i] = env.arg(5 + i as u32) as u8;
    }
    let face = cstr(env.cpu, env.arg(13));
    let n = face.len().min(31);
    raw[28..28 + n].copy_from_slice(&face[..n]);
    Outcome::Return(add_logfont(env, raw))
}

fn create_solid_brush(env: &mut Env<'_>) -> Outcome {
    let color = env.arg(0);
    Outcome::Return(gdi(env).add(GdiObj::Brush { color }))
}

fn create_pen(env: &mut Env<'_>) -> Outcome {
    let color = env.arg(2);
    Outcome::Return(gdi(env).add(GdiObj::Pen { color }))
}

fn delete_object(env: &mut Env<'_>) -> Outcome {
    let h = env.arg(0);
    if (STOCK_BASE..STOCK_BASE + STOCK_COUNT).contains(&h) {
        return Outcome::Return(1);
    }
    if matches!(gdi(env).get(h), Some(GdiObj::Palette { .. }))
        && gdi(env)
            .objects
            .values()
            .any(|object| matches!(object, GdiObj::Dc { palette, .. } if *palette == h))
    {
        return Outcome::Return(0);
    }
    match gdi(env).objects.remove(&h) {
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
fn slot_of(g: &Gdi, h: u32) -> Option<usize> {
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
    match g.get(h)? {
        GdiObj::Brush { .. } => Some(0),
        GdiObj::Pen { .. } => Some(1),
        GdiObj::Font(_) => Some(2),
        GdiObj::Bitmap { .. } => Some(3),
        _ => None,
    }
}

fn select_object(env: &mut Env<'_>) -> Outcome {
    let (hdc, h) = (env.arg(0), env.arg(1));
    let g = gdi(env);
    let Some(slot) = slot_of(g, h) else {
        return Outcome::Return(0);
    };
    match g.objects.get_mut(&hdc) {
        Some(GdiObj::Dc { selected, .. }) => {
            Outcome::Return(std::mem::replace(&mut selected[slot], h))
        }
        _ => Outcome::Return(0),
    }
}

fn get_object_a(env: &mut Env<'_>) -> Outcome {
    let (h, size, out) = (env.arg(0), env.arg(1), env.arg(2));
    if let Some(entries) = palette::entries(gdi(env), h) {
        let count = entries.len() as u16;
        if out == 0 {
            return Outcome::Return(2);
        }
        if size < 2 || !env.cpu.accessible(out, 2, Prot::WRITE) {
            return Outcome::Return(0);
        }
        return Outcome::Return(if env.cpu.write(out, &count.to_le_bytes()).is_ok() {
            2
        } else {
            0
        });
    }
    let bytes: Vec<u8> = match gdi(env).get(h) {
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
    let _ = env.cpu.write(out, &bytes[..n]);
    Outcome::Return(n as u32)
}

fn new_bitmap(env: &mut Env<'_>, width: i32, height: i32, bpp: u16) -> Option<(u32, u32)> {
    let stride = (width.unsigned_abs() * u32::from(bpp)).div_ceil(32) * 4;
    let size = (stride * height.unsigned_abs()).max(4);
    let bits = env.sys.vm.alloc(env.cpu, size, Prot::RW).ok()?;
    let h = gdi(env).add(GdiObj::Bitmap {
        width,
        height,
        bpp,
        bits,
        stride,
        owned: true,
    });
    Some((h, bits))
}

fn create_bitmap(env: &mut Env<'_>) -> Outcome {
    let (w, h, bpp, init) = (
        env.arg(0) as i32,
        env.arg(1) as i32,
        env.arg(3) as u16,
        env.arg(4),
    );
    let Some((hbm, bits)) = new_bitmap(env, w, h, bpp.max(1)) else {
        return Outcome::Return(0);
    };
    if init != 0 {
        // Source rows are WORD-aligned; ours DWORD-aligned.
        let src_stride = (w.unsigned_abs() * u32::from(bpp)).div_ceil(16) * 2;
        let dst_stride = (w.unsigned_abs() * u32::from(bpp)).div_ceil(32) * 4;
        let mut row = vec![0u8; src_stride as usize];
        for y in 0..h.unsigned_abs() {
            if env.cpu.read(init + y * src_stride, &mut row).is_ok() {
                let _ = env.cpu.write(bits + y * dst_stride, &row);
            }
        }
    }
    Outcome::Return(hbm)
}

fn create_compatible_bitmap(env: &mut Env<'_>) -> Outcome {
    let (w, h) = (env.arg(1) as i32, env.arg(2) as i32);
    Outcome::Return(new_bitmap(env, w, h, 32).map_or(0, |(h, _)| h))
}

fn create_dib_section(env: &mut Env<'_>) -> Outcome {
    let (bmi, bits_out) = (env.arg(1), env.arg(3));
    let w = env.cpu.read_u32(bmi + 4).unwrap_or(0) as i32;
    let h = env.cpu.read_u32(bmi + 8).unwrap_or(0) as i32;
    let bpp = env.cpu.read_u16(bmi + 14).unwrap_or(32);
    let Some((hbm, bits)) = new_bitmap(env, w, h, bpp) else {
        return Outcome::Return(0);
    };
    out_u32(env.cpu, bits_out, bits);
    Outcome::Return(hbm)
}

fn create_compatible_dc(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(window_dc(env, 0))
}

fn delete_dc(env: &mut Env<'_>) -> Outcome {
    let hdc = env.arg(0);
    Outcome::Return(u32::from(release_dc(env, hdc)))
}

fn get_device_caps(env: &mut Env<'_>) -> Outcome {
    let (w, h) = env.ext.get::<Win32>().user.screen;
    Outcome::Return(match env.arg(1) {
        8 => w as u32,   // HORZRES
        10 => h as u32,  // VERTRES
        12 => 32,        // BITSPIXEL
        14 => 1,         // PLANES
        24 => u32::MAX,  // NUMCOLORS (true color)
        88 | 90 => 96,   // LOGPIXELSX/Y
        116 => 60,       // VREFRESH
        118 => w as u32, // DESKTOPHORZRES
        117 => h as u32, // DESKTOPVERTRES
        n => {
            log::debug!("GetDeviceCaps({n}) = 0");
            0
        }
    })
}

fn dc_field(env: &mut Env<'_>, f: impl FnOnce(&mut u32, &mut u32, &mut u32) -> u32) -> Outcome {
    let hdc = env.arg(0);
    match gdi(env).objects.get_mut(&hdc) {
        Some(GdiObj::Dc {
            text_color,
            bk_color,
            bk_mode,
            ..
        }) => Outcome::Return(f(text_color, bk_color, bk_mode)),
        _ => Outcome::Return(0),
    }
}

fn set_text_color(env: &mut Env<'_>) -> Outcome {
    let v = env.arg(1);
    dc_field(env, |t, _, _| std::mem::replace(t, v))
}

fn set_bk_color(env: &mut Env<'_>) -> Outcome {
    let v = env.arg(1);
    dc_field(env, |_, b, _| std::mem::replace(b, v))
}

fn set_bk_mode(env: &mut Env<'_>) -> Outcome {
    let v = env.arg(1);
    dc_field(env, |_, _, m| std::mem::replace(m, v))
}

/// Font height of the font selected into `hdc` (for text metrics).
fn dc_font_height(env: &mut Env<'_>, hdc: u32) -> i32 {
    let g = gdi(env);
    let font = match g.get(hdc) {
        Some(GdiObj::Dc { selected, .. }) => selected[2],
        _ => 0,
    };
    match g.get(font) {
        Some(GdiObj::Font(f)) if f.height != 0 => f.height.abs(),
        _ => 16,
    }
}

fn get_text_extent_point32_a(env: &mut Env<'_>) -> Outcome {
    let (hdc, s, n, out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let h = dc_font_height(env, hdc);
    // Monospace approximation: single-byte characters are half the height wide.
    let mut bytes = vec![0u8; n as usize];
    let _ = env.cpu.read(s, &mut bytes);
    let width: i32 = bytes.len() as i32 * h / 2;
    out_u32(env.cpu, out, width as u32);
    out_u32(env.cpu, out + 4, h as u32);
    Outcome::Return(1)
}

fn get_text_metrics_a(env: &mut Env<'_>) -> Outcome {
    let (hdc, out) = (env.arg(0), env.arg(1));
    let h = dc_font_height(env, hdc);
    // TEXTMETRICA: height, ascent, descent, internal leading, external leading,
    // ave char width, max char width, weight, ...
    zero(env.cpu, out, 56);
    let fields = [h, h * 7 / 8, h / 8, 0, 0, h / 2, h, 400];
    for (i, v) in fields.into_iter().enumerate() {
        out_u32(env.cpu, out + 4 * i as u32, v as u32);
    }
    let _ = env.cpu.write(out + 52, &[128]); // tmCharSet = SHIFTJIS_CHARSET
    Outcome::Return(1)
}

pub fn register(reg: &mut Registry) {
    palette::register(reg);
    let s = |reg: &mut Registry, name: &str, argc: u8, h| stdcall(reg, GDI32, name, argc, h);
    s(reg, "GetStockObject", 1, get_stock_object);
    s(reg, "CreateFontIndirectA", 1, create_font_indirect_a);
    s(reg, "CreateFontA", 14, create_font_a);
    s(reg, "CreateSolidBrush", 1, create_solid_brush);
    s(reg, "CreatePen", 3, create_pen);
    s(reg, "DeleteObject", 1, delete_object);
    s(reg, "SelectObject", 2, select_object);
    s(reg, "GetObjectA", 3, get_object_a);
    s(reg, "CreateBitmap", 5, create_bitmap);
    s(reg, "CreateCompatibleBitmap", 3, create_compatible_bitmap);
    s(reg, "CreateDIBSection", 6, create_dib_section);
    s(reg, "CreateCompatibleDC", 1, create_compatible_dc);
    s(reg, "DeleteDC", 1, delete_dc);
    s(reg, "GetDeviceCaps", 2, get_device_caps);
    s(reg, "SetTextColor", 2, set_text_color);
    s(reg, "SetBkColor", 2, set_bk_color);
    s(reg, "SetBkMode", 2, set_bk_mode);
    s(reg, "TextOutA", 5, |_| Outcome::Return(1));
    s(reg, "GetTextExtentPoint32A", 4, get_text_extent_point32_a);
    s(reg, "GetTextMetricsA", 2, get_text_metrics_a);
}
