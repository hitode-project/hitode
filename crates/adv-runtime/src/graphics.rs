use adv_jit::{ApiCall, ApiMemory, ApiOutcome, ApiSpec, Convention, HostServices, Machine};
use std::borrow::Cow;
use std::collections::BTreeMap;

/// Memory needed by software surfaces. PAL guests with shared storage borrow pixels;
/// other CPU backends can supply an owned buffer at this boundary.
pub(crate) trait SurfaceMemory {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String>;
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String>;
    fn writable(&self, addr: u32, len: usize) -> Result<(), String>;
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String>;
    fn u32(&self, addr: u32) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.read(addr, 4)?.as_ref().try_into().unwrap(),
        ))
    }
    fn put_u32(&mut self, addr: u32, value: u32) -> Result<(), String> {
        self.write(addr, &value.to_le_bytes())
    }
}
impl SurfaceMemory for ApiMemory<'_> {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        ApiMemory::read(self, addr, len).map(Cow::Borrowed)
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        ApiMemory::write(self, addr, data)
    }
    fn writable(&self, addr: u32, len: usize) -> Result<(), String> {
        ApiMemory::writable(self, addr, len)
    }
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
        ApiMemory::copy(self, src, dst, len)
    }
}

pub(crate) struct GuestMemory<'a>(pub &'a mut dyn pal_cpu::Guest);
impl SurfaceMemory for GuestMemory<'_> {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        if let Some(bytes) = self.0.shared_bytes(addr, len) {
            return Ok(Cow::Borrowed(bytes));
        }
        let mut bytes = vec![0; len];
        self.0.read(addr, &mut bytes).map_err(|e| e.to_string())?;
        Ok(Cow::Owned(bytes))
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        self.0.write(addr, data).map_err(|e| e.to_string())
    }
    fn writable(&self, addr: u32, len: usize) -> Result<(), String> {
        if self.0.accessible(addr, len as u32, pal_cpu::Prot::WRITE) {
            Ok(())
        } else {
            Err(format!(
                "surface memory is not writable at {addr:#x} (+{len})"
            ))
        }
    }
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
        self.0.copy_memory(src, dst, len).map_err(|e| e.to_string())
    }
}

pub mod api {
    pub const GET_DC: u32 = 100;
    pub const CREATE_DC: u32 = 101;
    pub const CREATE_DIB: u32 = 102;
    pub const SELECT: u32 = 103;
    pub const BITBLT: u32 = 104;
    pub const DELETE_OBJECT: u32 = 105;
    pub const DELETE_DC: u32 = 106;
    pub const FLUSH: u32 = 107;
    pub const LAST_ERROR: u32 = 108;
    pub const RELEASE_DC: u32 = 109;
    pub const DD_CREATE: u32 = 200;
    pub const QUERY: u32 = 201;
    pub const ADDREF: u32 = 202;
    pub const RELEASE: u32 = 203;
    pub const CREATE_SURFACE: u32 = 204;
    pub const COOPERATIVE: u32 = 205;
    pub const LOCK: u32 = 206;
    pub const UNLOCK: u32 = 207;
    pub const BLT: u32 = 208;
    pub const GET_DESC: u32 = 209;
    pub const IS_LOST: u32 = 210;
    pub const DD_CREATE_OLD: u32 = 211;
    pub const DISPLAY_MODE: u32 = 212;
    pub const DISPLAY_MODE_OLD: u32 = 213;
    pub const RESTORE_MODE: u32 = 214;
    pub const GET_DISPLAY_MODE: u32 = 215;
    pub const GET_CAPS: u32 = 216;
    pub const BLT_FAST: u32 = 217;
    pub const FLIP: u32 = 218;
    pub const GET_ATTACHED: u32 = 219;
    pub const SURFACE_DC: u32 = 220;
    pub const SURFACE_RELEASE_DC: u32 = 221;
    pub const RESTORE: u32 = 222;
    pub const GET_PIXEL_FORMAT: u32 = 223;
    pub const UNIMPLEMENTED_COM: u32 = 224;
    pub const CREATE_CLIPPER: u32 = 225;
    pub const CLIPPER_SET_HWND: u32 = 226;
    pub const CLIPPER_GET_HWND: u32 = 227;
    pub const SET_CLIPPER: u32 = 228;
    pub const CLIPPER_INITIALIZE: u32 = 229;
}

pub(crate) const DEVICE_METHODS: &[&str] = &[
    "QueryInterface",
    "AddRef",
    "Release",
    "Compact",
    "CreateClipper",
    "CreatePalette",
    "CreateSurface",
    "DuplicateSurface",
    "EnumDisplayModes",
    "EnumSurfaces",
    "FlipToGDISurface",
    "GetCaps",
    "GetDisplayMode",
    "GetFourCCCodes",
    "GetGDISurface",
    "GetMonitorFrequency",
    "GetScanLine",
    "GetVerticalBlankStatus",
    "Initialize",
    "RestoreDisplayMode",
    "SetCooperativeLevel",
    "SetDisplayMode",
    "WaitForVerticalBlank",
    "GetAvailableVidMem",
    "GetSurfaceFromDC",
    "RestoreAllSurfaces",
    "TestCooperativeLevel",
    "GetDeviceIdentifier",
    "StartModeTest",
    "EvaluateMode",
];
pub(crate) const SURFACE_METHODS: &[&str] = &[
    "QueryInterface",
    "AddRef",
    "Release",
    "AddAttachedSurface",
    "AddOverlayDirtyRect",
    "Blt",
    "BltBatch",
    "BltFast",
    "DeleteAttachedSurface",
    "EnumAttachedSurfaces",
    "EnumOverlayZOrders",
    "Flip",
    "GetAttachedSurface",
    "GetBltStatus",
    "GetCaps",
    "GetClipper",
    "GetColorKey",
    "GetDC",
    "GetFlipStatus",
    "GetOverlayPosition",
    "GetPalette",
    "GetPixelFormat",
    "GetSurfaceDesc",
    "Initialize",
    "IsLost",
    "Lock",
    "ReleaseDC",
    "Restore",
    "SetClipper",
    "SetColorKey",
    "SetOverlayPosition",
    "SetPalette",
    "Unlock",
    "UpdateOverlay",
    "UpdateOverlayDisplay",
    "UpdateOverlayZOrder",
    "GetDDInterface",
    "PageLock",
    "PageUnlock",
    "SetSurfaceDesc",
    "SetPrivateData",
    "GetPrivateData",
    "FreePrivateData",
    "GetUniquenessValue",
    "ChangeUniquenessValue",
    "SetPriority",
    "GetPriority",
    "SetLOD",
    "GetLOD",
];

pub const IID_DDRAW7: [u8; 16] = [
    0xc0, 0x5e, 0xe6, 0x15, 0x9c, 0x3b, 0xd2, 0x11, 0xb9, 0x2f, 0, 0x60, 0x97, 0x97, 0xea, 0x5b,
];
pub const IID_SURFACE7: [u8; 16] = [
    0x80, 0x5a, 0x67, 0x06, 0x9b, 0x3b, 0xd2, 0x11, 0xb9, 0x2f, 0, 0x60, 0x97, 0x97, 0xea, 0x5b,
];
const IID_UNKNOWN: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
const IID_DDRAW: [u8; 16] = [
    0x80, 0xdb, 0x14, 0x6c, 0x33, 0xa7, 0xce, 0x11, 0xa5, 0x21, 0, 0x20, 0xaf, 0x0b, 0xe5, 0x60,
];
const IID_DDRAW2: [u8; 16] = [
    0xe0, 0xf3, 0xa6, 0xb3, 0x43, 0x2b, 0xcf, 0x11, 0xa2, 0xde, 0, 0xaa, 0, 0xb9, 0x33, 0x56,
];
const IID_DDRAW4: [u8; 16] = [
    0x9a, 0x50, 0x59, 0x9c, 0xbd, 0x39, 0xd1, 0x11, 0x8c, 0x4a, 0, 0xc0, 0x4f, 0xd9, 0x30, 0xc5,
];
const IID_SURFACE: [u8; 16] = [
    0x81, 0xdb, 0x14, 0x6c, 0x33, 0xa7, 0xce, 0x11, 0xa5, 0x21, 0, 0x20, 0xaf, 0x0b, 0xe5, 0x60,
];
const IID_SURFACE2: [u8; 16] = [
    0x85, 0x58, 0x80, 0x57, 0xec, 0x6e, 0xcf, 0x11, 0x94, 0x41, 0xa8, 0x23, 3, 0xc1, 0x0e, 0x27,
];
const IID_SURFACE3: [u8; 16] = [
    0, 0x4e, 0x04, 0xda, 0xb2, 0x69, 0xd0, 0x11, 0xa1, 0xd5, 0, 0xaa, 0, 0xb8, 0xdf, 0xbb,
];
const IID_SURFACE4: [u8; 16] = [
    0x30, 0x86, 0x2b, 0x0b, 0x35, 0xad, 0xd0, 0x11, 0x8e, 0xa6, 0, 0x60, 0x97, 0x97, 0xea, 0x5b,
];
const IID_CLIPPER: [u8; 16] = [
    0x85, 0xdb, 0x14, 0x6c, 0x33, 0xa7, 0xce, 0x11, 0xa5, 0x21, 0, 0x20, 0xaf, 0x0b, 0xe5, 0x60,
];
const INVALID: u32 = 0x80070057;
const UNSUPPORTED: u32 = 0x80004001;
const BUSY: u32 = 0x887601ae;
const NOT_LOCKED: u32 = 0x88760248;
const WINDOW_DC: u32 = 1;
const STOCK_BITMAP: u32 = 2;

pub(crate) const API_SPECS: &[(u32, &str, &str, u8)] = &[
    (
        api::CREATE_CLIPPER,
        "ddraw.dll",
        "IDirectDraw7::CreateClipper",
        4,
    ),
    (
        api::CLIPPER_SET_HWND,
        "ddraw.dll",
        "IDirectDrawClipper::SetHWnd",
        3,
    ),
    (
        api::CLIPPER_GET_HWND,
        "ddraw.dll",
        "IDirectDrawClipper::GetHWnd",
        2,
    ),
    (
        api::SET_CLIPPER,
        "ddraw.dll",
        "IDirectDrawSurface7::SetClipper",
        2,
    ),
    (
        api::CLIPPER_INITIALIZE,
        "ddraw.dll",
        "IDirectDrawClipper::Initialize",
        3,
    ),
    (
        api::UNIMPLEMENTED_COM,
        "ddraw.dll",
        "UnsupportedCOMMethod",
        0,
    ),
    (api::GET_DC, "user32.dll", "GetDC", 1),
    (api::CREATE_DC, "gdi32.dll", "CreateCompatibleDC", 1),
    (api::CREATE_DIB, "gdi32.dll", "CreateDIBSection", 6),
    (api::SELECT, "gdi32.dll", "SelectObject", 2),
    (api::BITBLT, "gdi32.dll", "BitBlt", 9),
    (api::DELETE_OBJECT, "gdi32.dll", "DeleteObject", 1),
    (api::DELETE_DC, "gdi32.dll", "DeleteDC", 1),
    (api::FLUSH, "gdi32.dll", "GdiFlush", 0),
    (api::LAST_ERROR, "kernel32.dll", "GetLastError", 0),
    (api::RELEASE_DC, "user32.dll", "ReleaseDC", 2),
    (api::DD_CREATE, "ddraw.dll", "DirectDrawCreateEx", 4),
    (api::QUERY, "ddraw.dll", "QueryInterface", 3),
    (api::ADDREF, "ddraw.dll", "AddRef", 1),
    (api::RELEASE, "ddraw.dll", "Release", 1),
    (
        api::CREATE_SURFACE,
        "ddraw.dll",
        "IDirectDraw7::CreateSurface",
        4,
    ),
    (
        api::COOPERATIVE,
        "ddraw.dll",
        "IDirectDraw7::SetCooperativeLevel",
        3,
    ),
    (api::LOCK, "ddraw.dll", "IDirectDrawSurface7::Lock", 5),
    (api::UNLOCK, "ddraw.dll", "IDirectDrawSurface7::Unlock", 2),
    (api::BLT, "ddraw.dll", "IDirectDrawSurface7::Blt", 6),
    (
        api::GET_DESC,
        "ddraw.dll",
        "IDirectDrawSurface7::GetSurfaceDesc",
        2,
    ),
    (api::IS_LOST, "ddraw.dll", "IDirectDrawSurface7::IsLost", 1),
    (api::DD_CREATE_OLD, "ddraw.dll", "DirectDrawCreate", 3),
    (
        api::DISPLAY_MODE,
        "ddraw.dll",
        "IDirectDraw7::SetDisplayMode",
        6,
    ),
    (
        api::DISPLAY_MODE_OLD,
        "ddraw.dll",
        "IDirectDraw::SetDisplayMode",
        4,
    ),
    (
        api::RESTORE_MODE,
        "ddraw.dll",
        "IDirectDraw7::RestoreDisplayMode",
        1,
    ),
    (
        api::GET_DISPLAY_MODE,
        "ddraw.dll",
        "IDirectDraw7::GetDisplayMode",
        2,
    ),
    (api::GET_CAPS, "ddraw.dll", "IDirectDraw7::GetCaps", 3),
    (
        api::BLT_FAST,
        "ddraw.dll",
        "IDirectDrawSurface7::BltFast",
        6,
    ),
    (api::FLIP, "ddraw.dll", "IDirectDrawSurface7::Flip", 3),
    (
        api::GET_ATTACHED,
        "ddraw.dll",
        "IDirectDrawSurface7::GetAttachedSurface",
        3,
    ),
    (
        api::SURFACE_DC,
        "ddraw.dll",
        "IDirectDrawSurface7::GetDC",
        2,
    ),
    (
        api::SURFACE_RELEASE_DC,
        "ddraw.dll",
        "IDirectDrawSurface7::ReleaseDC",
        2,
    ),
    (api::RESTORE, "ddraw.dll", "IDirectDrawSurface7::Restore", 1),
    (
        api::GET_PIXEL_FORMAT,
        "ddraw.dll",
        "IDirectDrawSurface7::GetPixelFormat",
        2,
    ),
];

/// Returns id -> guest entry. COM vtables use these exact same API thunks.
pub fn install(machine: &mut Machine, base: u32) -> Result<BTreeMap<u32, u32>, String> {
    let mut entries = BTreeMap::new();
    for (index, (id, module, name, argc)) in API_SPECS.iter().copied().enumerate() {
        let entry = base
            .checked_add(index as u32 * 16)
            .ok_or("API address overflow")?;
        machine.register_api(ApiSpec::new(
            id,
            module,
            name,
            entry,
            Convention::Stdcall(argc),
        ))?;
        entries.insert(id, entry);
    }
    Ok(entries)
}

pub struct FrameView<'a> {
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bgra: &'a [u8],
    pub serial: u64,
}
pub trait Presenter {
    fn present(&mut self, frame: FrameView<'_>) -> Result<(), String>;
}

#[derive(Default)]
pub struct Snapshot {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub frames: u64,
}
impl Presenter for Snapshot {
    fn present(&mut self, f: FrameView<'_>) -> Result<(), String> {
        self.width = f.width;
        self.height = f.height;
        self.frames = f.serial;
        self.bgra.resize((f.width * f.height * 4) as usize, 0);
        for y in 0..f.height as usize {
            let n = f.width as usize * 4;
            self.bgra[y * n..(y + 1) * n]
                .copy_from_slice(&f.bgra[y * f.pitch as usize..y * f.pitch as usize + n]);
        }
        Ok(())
    }
}

#[derive(Default, Debug)]
pub struct GraphicsStats {
    pub presentations: u64,
    pub bytes_blitted: u64,
    pub locks: u64,
    pub unlocks: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}
#[derive(Clone)]
struct Surface {
    pixels: u32,
    width: u32,
    height: u32,
    pitch: u32,
    bottom_up: bool,
    locked: Option<(Rect, bool)>,
    version: u64,
    format: PixelFormat,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum PixelFormat {
    Bgra32,
    Rgb565,
    Rgb555,
}
impl PixelFormat {
    fn bytes(self) -> u32 {
        if self == Self::Bgra32 { 4 } else { 2 }
    }
    fn masks(self) -> (u32, u32, u32) {
        match self {
            Self::Bgra32 => (0xff0000, 0xff00, 0xff),
            Self::Rgb565 => (0xf800, 0x7e0, 0x1f),
            Self::Rgb555 => (0x7c00, 0x3e0, 0x1f),
        }
    }
    fn decode(self, p: &[u8]) -> [u8; 4] {
        if self == Self::Bgra32 {
            return p[..4].try_into().unwrap();
        }
        let v = u32::from(u16::from_le_bytes(p[..2].try_into().unwrap()));
        let (r, g, b) = self.masks();
        let channel = |mask: u32| {
            let bits = mask >> mask.trailing_zeros();
            ((v & mask) >> mask.trailing_zeros()) * 255 / bits
        };
        [channel(b) as u8, channel(g) as u8, channel(r) as u8, 0]
    }
    fn encode(self, p: [u8; 4]) -> Vec<u8> {
        if self == Self::Bgra32 {
            return p.to_vec();
        }
        let (r, g, b) = self.masks();
        let channel = |value: u8, mask: u32| {
            (u32::from(value) * (mask >> mask.trailing_zeros()) / 255) << mask.trailing_zeros()
        };
        ((channel(p[2], r) | channel(p[1], g) | channel(p[0], b)) as u16)
            .to_le_bytes()
            .to_vec()
    }
}
impl Surface {
    fn rect(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        }
    }
    fn row(&self, x: u32, y: u32) -> u32 {
        self.pixels
            + (if self.bottom_up {
                self.height - 1 - y
            } else {
                y
            }) * self.pitch
            + x * self.format.bytes()
    }
}
#[derive(Clone, Copy)]
enum Kind {
    Device,
    Surface(u32),
    Clipper,
}
struct Com {
    kind: Kind,
    refs: u32,
    legacy: bool,
}

struct Arena {
    free: Vec<(u32, u32)>,
}
impl Arena {
    fn new(start: u32, len: u32) -> Result<Self, String> {
        start.checked_add(len).ok_or("arena overflow")?;
        if !start.is_multiple_of(16) || len == 0 {
            return Err("arena must be aligned and nonempty".into());
        }
        Ok(Self {
            free: vec![(start, len)],
        })
    }
    fn alloc(&mut self, n: u32, m: &mut dyn SurfaceMemory) -> Result<u32, String> {
        let n = n.checked_add(15).ok_or("allocation overflow")? & !15;
        let index = self
            .free
            .iter()
            .position(|(_, len)| *len >= n)
            .ok_or("surface arena exhausted")?;
        let (p, len) = self.free[index];
        m.writable(p, n as usize)?;
        self.free[index] = (p + n, len - n);
        if len == n {
            self.free.remove(index);
        }
        Ok(p)
    }
    fn release(&mut self, p: u32, n: u32) {
        self.free.push((p, (n + 15) & !15));
        self.free.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::new();
        for &(p, n) in &self.free {
            if let Some(last) = merged.last_mut()
                && last.0 + last.1 == p
            {
                last.1 += n;
                continue;
            }
            merged.push((p, n));
        }
        self.free = merged;
    }
}

pub struct Graphics<P: Presenter> {
    pub presenter: P,
    pub stats: GraphicsStats,
    width: u32,
    height: u32,
    arena: Arena,
    next: u32,
    surfaces: BTreeMap<u32, Surface>,
    dcs: BTreeMap<u32, u32>,
    objects: BTreeMap<u32, Com>,
    primary: Option<u32>,
    dd_vtable: Option<u32>,
    old_dd_vtable: Option<u32>,
    surface_vtable: Option<u32>,
    clipper_vtable: Option<u32>,
    clipper_windows: BTreeMap<u32, u32>,
    surface_clippers: BTreeMap<u32, u32>,
    clipper_origins: BTreeMap<u32, (i32, i32)>,
    entries: BTreeMap<u32, u32>,
    last_error: u32,
    display_format: PixelFormat,
    attached: BTreeMap<u32, u32>,
}

impl<P: Presenter> Graphics<P> {
    pub(crate) fn handle(
        &mut self,
        id: u32,
        args: &[u32],
        memory: &mut dyn SurfaceMemory,
    ) -> Result<u32, String> {
        if id < 200 {
            self.gdi(id, args, memory)
        } else {
            self.ddraw(id, args, memory)
        }
    }
    pub fn new(
        width: u32,
        height: u32,
        arena: u32,
        len: u32,
        entries: BTreeMap<u32, u32>,
        presenter: P,
    ) -> Result<Self, String> {
        size(width, height)?;
        Ok(Self {
            presenter,
            stats: GraphicsStats::default(),
            width,
            height,
            arena: Arena::new(arena, len)?,
            next: 16,
            surfaces: BTreeMap::new(),
            dcs: BTreeMap::new(),
            objects: BTreeMap::new(),
            primary: None,
            dd_vtable: None,
            old_dd_vtable: None,
            surface_vtable: None,
            clipper_vtable: None,
            clipper_windows: BTreeMap::new(),
            surface_clippers: BTreeMap::new(),
            clipper_origins: BTreeMap::new(),
            entries,
            last_error: 0,
            display_format: PixelFormat::Bgra32,
            attached: BTreeMap::new(),
        })
    }
    pub fn live_surfaces(&self) -> usize {
        self.surfaces.len()
    }
    pub(crate) fn owns_dc(&self, dc: u32) -> bool {
        dc == WINDOW_DC || self.dcs.contains_key(&dc)
    }
    pub(crate) fn owns_bitmap(&self, object: u32) -> bool {
        self.surfaces.contains_key(&object)
    }
    pub(crate) fn configure_window(
        &mut self,
        width: u32,
        height: u32,
        m: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        size(width, height)?;
        self.width = width;
        self.height = height;
        let Some(primary) = self.primary else {
            return Ok(());
        };
        let old = self.surfaces[&primary].clone();
        if old.width == width && old.height == height {
            return Ok(());
        }
        if old.locked.is_some() {
            return Err("primary surface is locked during window resize".into());
        }
        let new = self.new_surface_format(width, height, false, old.format, m)?;
        let rect = Rect {
            x: 0,
            y: 0,
            w: old.width.min(width),
            h: old.height.min(height),
        };
        self.copy(primary, rect, new, rect, m)?;
        let surface = self.surfaces.remove(&new).unwrap();
        self.surfaces.insert(primary, surface);
        self.arena.release(old.pixels, old.pitch * old.height);
        Ok(())
    }
    pub(crate) fn configure_clipper(&mut self, clipper: u32, origin: (i32, i32)) {
        self.clipper_origins.insert(clipper, origin);
    }
    fn new_surface_format(
        &mut self,
        w: u32,
        h: u32,
        bottom_up: bool,
        format: PixelFormat,
        m: &mut dyn SurfaceMemory,
    ) -> Result<u32, String> {
        size(w, h)?;
        let pitch = (w * format.bytes() + 3) & !3;
        let bytes = pitch.checked_mul(h).ok_or("surface overflow")?;
        let p = self.arena.alloc(bytes, m)?;
        // Reused storage must not expose pixels from a previously deleted object.
        let zero = [0u8; 4096];
        let mut offset = 0;
        while offset < bytes {
            let n = (bytes - offset).min(4096);
            m.write(p + offset, &zero[..n as usize])?;
            offset += n;
        }
        let id = self.next;
        self.next += 1;
        self.surfaces.insert(
            id,
            Surface {
                pixels: p,
                width: w,
                height: h,
                pitch,
                bottom_up,
                locked: None,
                version: 0,
                format,
            },
        );
        Ok(id)
    }
    fn primary(&mut self, m: &mut dyn SurfaceMemory) -> Result<u32, String> {
        if let Some(id) = self.primary {
            return Ok(id);
        }
        let id = self.new_surface_format(self.width, self.height, false, self.display_format, m)?;
        self.primary = Some(id);
        Ok(id)
    }
    fn present(&mut self, id: u32, m: &dyn SurfaceMemory) -> Result<(), String> {
        let s = &self.surfaces[&id];
        self.stats.presentations += 1;
        let pixels = m.read(s.pixels, (s.pitch * s.height) as usize)?;
        if s.format != PixelFormat::Bgra32 {
            // Conversion is one output buffer; the guest pixel store remains shared.
            let mut converted = vec![0; s.width as usize * s.height as usize * 4];
            for y in 0..s.height as usize {
                for x in 0..s.width as usize {
                    let offset = y * s.pitch as usize + x * s.format.bytes() as usize;
                    converted[(y * s.width as usize + x) * 4..(y * s.width as usize + x + 1) * 4]
                        .copy_from_slice(&s.format.decode(&pixels[offset..]));
                }
            }
            return self.presenter.present(FrameView {
                width: s.width,
                height: s.height,
                pitch: s.width * 4,
                bgra: &converted,
                serial: self.stats.presentations,
            });
        }
        self.presenter.present(FrameView {
            width: s.width,
            height: s.height,
            pitch: s.pitch,
            bgra: &pixels,
            serial: self.stats.presentations,
        })
    }
    fn copy(
        &mut self,
        src: u32,
        sr: Rect,
        dst: u32,
        dr: Rect,
        m: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        let s = self
            .surfaces
            .get(&src)
            .ok_or("unknown source surface")?
            .clone();
        let d = self
            .surfaces
            .get(&dst)
            .ok_or("unknown destination surface")?
            .clone();
        if sr.w != dr.w || sr.h != dr.h {
            return Err("scaling not implemented".into());
        }
        // Copy bottom to top for downward overlapping same-surface transfers.
        for row in 0..sr.h {
            let y = if src == dst && dr.y > sr.y {
                sr.h - 1 - row
            } else {
                row
            };
            if s.format == d.format {
                m.copy(
                    s.row(sr.x, sr.y + y),
                    d.row(dr.x, dr.y + y),
                    (sr.w * s.format.bytes()) as usize,
                )?;
            } else {
                let input = m
                    .read(s.row(sr.x, sr.y + y), (sr.w * s.format.bytes()) as usize)?
                    .into_owned();
                let output: Vec<u8> = input
                    .chunks_exact(s.format.bytes() as usize)
                    .flat_map(|p| d.format.encode(s.format.decode(p)))
                    .collect();
                m.write(d.row(dr.x, dr.y + y), &output)?;
            }
        }
        self.surfaces.get_mut(&dst).unwrap().version += 1;
        self.stats.bytes_blitted += u64::from(sr.w) * u64::from(sr.h) * u64::from(d.format.bytes());
        Ok(())
    }
    fn fail(&mut self, code: u32) -> u32 {
        self.last_error = code;
        0
    }
    fn gdi(&mut self, id: u32, a: &[u32], m: &mut dyn SurfaceMemory) -> Result<u32, String> {
        Ok(match id {
            api::GET_DC => {
                self.primary(m)?;
                WINDOW_DC
            }
            api::RELEASE_DC => u32::from(a[1] == WINDOW_DC),
            api::LAST_ERROR => self.last_error,
            api::FLUSH => 1,
            api::CREATE_DC => {
                let id = self.next;
                self.next += 1;
                self.dcs.insert(id, STOCK_BITMAP);
                id
            }
            api::CREATE_DIB => {
                m.put_u32(a[3], 0)?;
                let h = m.read(a[1], 40)?;
                let w = i32::from_le_bytes(h[4..8].try_into().unwrap());
                let height = i32::from_le_bytes(h[8..12].try_into().unwrap());
                if u32::from_le_bytes(h[..4].try_into().unwrap()) != 40
                    || w <= 0
                    || height == 0
                    || height == i32::MIN
                    || h[12..14] != 1u16.to_le_bytes()
                    || !matches!(u16::from_le_bytes(h[14..16].try_into().unwrap()), 16 | 32)
                    || h[16..20] != [0; 4]
                    || a[2] != 0
                    || a[4] != 0
                {
                    return Ok(self.fail(87));
                }
                if size(w as u32, height.unsigned_abs()).is_err() {
                    return Ok(self.fail(87));
                }
                let format = if h[14..16] == 16u16.to_le_bytes() {
                    PixelFormat::Rgb555
                } else {
                    PixelFormat::Bgra32
                };
                let bitmap = self.new_surface_format(
                    w as u32,
                    height.unsigned_abs(),
                    height > 0,
                    format,
                    m,
                )?;
                m.put_u32(a[3], self.surfaces[&bitmap].pixels)?;
                bitmap
            }
            api::SELECT => {
                if !self.dcs.contains_key(&a[0])
                    || (a[1] != STOCK_BITMAP && !self.surfaces.contains_key(&a[1]))
                {
                    return Ok(self.fail(6));
                }
                if a[1] != STOCK_BITMAP
                    && self
                        .dcs
                        .iter()
                        .any(|(&dc, &bitmap)| dc != a[0] && bitmap == a[1])
                {
                    return Ok(self.fail(87));
                }
                self.dcs.insert(a[0], a[1]).unwrap()
            }
            api::DELETE_DC => u32::from(self.dcs.remove(&a[0]).is_some()),
            api::DELETE_OBJECT => {
                if self.dcs.values().any(|&id| id == a[0]) || self.primary == Some(a[0]) {
                    return Ok(self.fail(170));
                }
                if let Some(s) = self.surfaces.remove(&a[0]) {
                    self.arena.release(s.pixels, s.pitch * s.height);
                    1
                } else {
                    self.fail(6)
                }
            }
            api::BITBLT => {
                if a[8] != 0x00cc0020 {
                    return Ok(self.fail(87));
                }
                let Some(&src) = self.dcs.get(&a[5]) else {
                    return Ok(self.fail(6));
                };
                let dst = if a[0] == WINDOW_DC {
                    self.primary(m)?
                } else {
                    *self.dcs.get(&a[0]).unwrap_or(&0)
                };
                let (Some(s), Some(d)) = (self.surfaces.get(&src), self.surfaces.get(&dst)) else {
                    return Ok(self.fail(6));
                };
                let (x, y, w, h, sx, sy) = (
                    a[1] as i32 as i64,
                    a[2] as i32 as i64,
                    a[3] as i32 as i64,
                    a[4] as i32 as i64,
                    a[6] as i32 as i64,
                    a[7] as i32 as i64,
                );
                if w < 0 || h < 0 {
                    return Ok(self.fail(87));
                }
                let (left, top, right, bottom) = (
                    x.max(0),
                    y.max(0),
                    (x + w).min(i64::from(d.width)),
                    (y + h).min(i64::from(d.height)),
                );
                if right <= left || bottom <= top {
                    return Ok(1);
                }
                let (sx, sy) = (sx + left - x, sy + top - y);
                if sx < 0
                    || sy < 0
                    || sx + right - left > i64::from(s.width)
                    || sy + bottom - top > i64::from(s.height)
                {
                    return Ok(self.fail(87));
                }
                let sr = Rect {
                    x: sx as u32,
                    y: sy as u32,
                    w: (right - left) as u32,
                    h: (bottom - top) as u32,
                };
                let dr = Rect {
                    x: left as u32,
                    y: top as u32,
                    ..sr
                };
                self.copy(src, sr, dst, dr, m)?;
                if a[0] == WINDOW_DC {
                    self.present(dst, m)?;
                }
                1
            }
            _ => return Err(format!("unknown GDI API {id}")),
        })
    }
}

fn size(w: u32, h: u32) -> Result<u32, String> {
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return Err("invalid surface dimensions".into());
    }
    w.checked_mul(h)
        .and_then(|n| n.checked_mul(4))
        .ok_or("surface overflow".into())
}

impl<P: Presenter> HostServices for Graphics<P> {
    fn call(&mut self, c: ApiCall<'_>, m: &mut ApiMemory<'_>) -> Result<ApiOutcome, String> {
        let v = if c.spec.id < 200 {
            self.gdi(c.spec.id, c.args, m)?
        } else {
            self.ddraw(c.spec.id, c.args, m)?
        };
        Ok(ApiOutcome::Return(v))
    }
}

impl<P: Presenter> Graphics<P> {
    fn table(
        &mut self,
        surface: bool,
        legacy: bool,
        m: &mut dyn SurfaceMemory,
    ) -> Result<u32, String> {
        if let Some(v) = if surface {
            self.surface_vtable
        } else if legacy {
            self.old_dd_vtable
        } else {
            self.dd_vtable
        } {
            return Ok(v);
        }
        let slots = if surface { 49 } else { 30 };
        let p = self.arena.alloc(slots * 4, m)?;
        for slot in 0..slots {
            let diagnostic_id = if surface { 500 + slot } else { 400 + slot };
            let thunk = self
                .entries
                .get(&diagnostic_id)
                .or_else(|| self.entries.get(&api::UNIMPLEMENTED_COM))
                .ok_or("missing COM diagnostic thunk")?;
            m.put_u32(p + slot * 4, *thunk)?;
        }
        let mut methods = vec![(0, api::QUERY), (1, api::ADDREF), (2, api::RELEASE)];
        if surface {
            methods.extend([
                (5, api::BLT),
                (7, api::BLT_FAST),
                (11, api::FLIP),
                (12, api::GET_ATTACHED),
                (17, api::SURFACE_DC),
                (21, api::GET_PIXEL_FORMAT),
                (22, api::GET_DESC),
                (24, api::IS_LOST),
                (25, api::LOCK),
                (26, api::SURFACE_RELEASE_DC),
                (27, api::RESTORE),
                (28, api::SET_CLIPPER),
                (32, api::UNLOCK),
            ]);
        } else {
            methods.extend([
                (4, api::CREATE_CLIPPER),
                (6, api::CREATE_SURFACE),
                (11, api::GET_CAPS),
                (12, api::GET_DISPLAY_MODE),
                (19, api::RESTORE_MODE),
                (20, api::COOPERATIVE),
                (
                    21,
                    if legacy {
                        api::DISPLAY_MODE_OLD
                    } else {
                        api::DISPLAY_MODE
                    },
                ),
            ]);
        }
        for (slot, id) in methods {
            m.put_u32(
                p + slot * 4,
                *self.entries.get(&id).ok_or("missing COM thunk")?,
            )?;
        }
        if surface {
            self.surface_vtable = Some(p);
        } else if legacy {
            self.old_dd_vtable = Some(p);
        } else {
            self.dd_vtable = Some(p);
        }
        Ok(p)
    }
    fn com_version(
        &mut self,
        kind: Kind,
        legacy: bool,
        m: &mut dyn SurfaceMemory,
    ) -> Result<u32, String> {
        let vtable = if matches!(kind, Kind::Clipper) {
            if let Some(table) = self.clipper_vtable {
                table
            } else {
                let table = self.arena.alloc(9 * 4, m)?;
                for slot in 0..9 {
                    m.put_u32(table + slot * 4, self.entries[&api::UNIMPLEMENTED_COM])?;
                }
                for (slot, id) in [
                    (0, api::QUERY),
                    (1, api::ADDREF),
                    (2, api::RELEASE),
                    (4, api::CLIPPER_GET_HWND),
                    (5, api::CLIPPER_INITIALIZE),
                    (8, api::CLIPPER_SET_HWND),
                ] {
                    m.put_u32(table + slot * 4, self.entries[&id])?;
                }
                self.clipper_vtable = Some(table);
                table
            }
        } else {
            self.table(matches!(kind, Kind::Surface(_)), legacy, m)?
        };
        let p = self.arena.alloc(16, m)?;
        m.put_u32(p, vtable)?;
        self.objects.insert(
            p,
            Com {
                kind,
                refs: 1,
                legacy,
            },
        );
        Ok(p)
    }
    fn surface_id(&self, object: u32) -> Option<u32> {
        match self.objects.get(&object)?.kind {
            Kind::Surface(id) => Some(id),
            Kind::Device | Kind::Clipper => None,
        }
    }
    fn desc(&self, id: u32, pixels: u32) -> [u8; 124] {
        let s = &self.surfaces[&id];
        let mut d = [0u8; 124];
        let (r, g, b) = s.format.masks();
        for (offset, v) in [
            (0, 124),
            (4, if pixels == 0 { 0x100f } else { 0x180f }),
            (8, s.height),
            (12, s.width),
            (16, s.pitch),
            (36, pixels),
            (72, 32),
            (76, 0x40),
            (84, s.format.bytes() * 8),
            (88, r),
            (92, g),
            (96, b),
            (
                104,
                if self.primary == Some(id) {
                    0x200
                } else {
                    0x40
                },
            ),
        ] {
            d[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
        }
        d
    }
    fn rect(&self, id: u32, p: u32, m: &dyn SurfaceMemory) -> Result<Rect, String> {
        let s = &self.surfaces[&id];
        if p == 0 {
            return Ok(s.rect());
        }
        let (x, y, r, b) = (
            m.u32(p)? as i32,
            m.u32(p + 4)? as i32,
            m.u32(p + 8)? as i32,
            m.u32(p + 12)? as i32,
        );
        if x < 0 || y < 0 || r <= x || b <= y || r as u32 > s.width || b as u32 > s.height {
            return Err("invalid surface rectangle".into());
        }
        Ok(Rect {
            x: x as u32,
            y: y as u32,
            w: (r - x) as u32,
            h: (b - y) as u32,
        })
    }
    fn ddraw(&mut self, id: u32, a: &[u32], m: &mut dyn SurfaceMemory) -> Result<u32, String> {
        if id == api::UNIMPLEMENTED_COM {
            return Err("unimplemented DirectDraw COM method".into());
        }
        if id == api::DD_CREATE || id == api::DD_CREATE_OLD {
            m.put_u32(a[1], 0)?;
            if a[0] != 0
                || (id == api::DD_CREATE && (a[3] != 0 || m.read(a[2], 16)?.as_ref() != IID_DDRAW7))
                || (id == api::DD_CREATE_OLD && a[2] != 0)
            {
                return Ok(INVALID);
            }
            let p = self.com_version(Kind::Device, id == api::DD_CREATE_OLD, m)?;
            m.put_u32(a[1], p)?;
            return Ok(0);
        }
        let Some(object) = self.objects.get(&a[0]) else {
            return Ok(INVALID);
        };
        let kind = object.kind;
        let legacy = object.legacy;
        Ok(match id {
            api::QUERY => {
                m.put_u32(a[2], 0)?;
                let iid = m.read(a[1], 16)?;
                let newer_device = [IID_DDRAW2, IID_DDRAW4, IID_DDRAW7]
                    .iter()
                    .any(|id| iid.as_ref() == id);
                if matches!(kind, Kind::Device)
                    && ((legacy && newer_device) || (!legacy && iid.as_ref() == IID_DDRAW))
                {
                    let object = self.com_version(kind, !legacy, m)?;
                    m.put_u32(a[2], object)?;
                    return Ok(0);
                }
                if iid.as_ref() != IID_UNKNOWN
                    && !(matches!(kind, Kind::Device) && newer_device)
                    && !(matches!(kind, Kind::Surface(_))
                        && [IID_SURFACE2, IID_SURFACE3, IID_SURFACE4]
                            .iter()
                            .any(|id| iid.as_ref() == id))
                    && iid.as_ref()
                        != match kind {
                            Kind::Device => &IID_DDRAW,
                            Kind::Surface(_) => &IID_SURFACE,
                            Kind::Clipper => &IID_CLIPPER,
                        }
                    && iid.as_ref()
                        != match kind {
                            Kind::Device => &IID_DDRAW7,
                            Kind::Surface(_) => &IID_SURFACE7,
                            Kind::Clipper => &IID_CLIPPER,
                        }
                {
                    return Ok(0x80004002);
                }
                self.objects.get_mut(&a[0]).unwrap().refs += 1;
                m.put_u32(a[2], a[0])?;
                0
            }
            api::ADDREF => {
                let o = self.objects.get_mut(&a[0]).unwrap();
                o.refs += 1;
                o.refs
            }
            api::RELEASE => {
                let o = self.objects.get_mut(&a[0]).unwrap();
                o.refs -= 1;
                let count = o.refs;
                if count == 0 {
                    self.objects.remove(&a[0]);
                    self.arena.release(a[0], 16);
                    if let Kind::Surface(id) = kind
                        && self.primary != Some(id)
                        && !self
                            .objects
                            .values()
                            .any(|o| matches!(o.kind,Kind::Surface(other) if other==id))
                        && !self.attached.values().any(|&other| other == id)
                        && let Some(s) = self.surfaces.remove(&id)
                    {
                        self.arena.release(s.pixels, s.pitch * s.height);
                    }
                }
                count
            }
            api::COOPERATIVE => {
                if matches!(kind, Kind::Device) && (a[2] == 8 || a[2] & 0x11 == 0x11) {
                    0
                } else {
                    UNSUPPORTED
                }
            }
            api::CREATE_CLIPPER => {
                m.put_u32(a[2], 0)?;
                if !matches!(kind, Kind::Device) || a[1] != 0 || a[3] != 0 {
                    return Ok(INVALID);
                }
                let clipper = self.com_version(Kind::Clipper, false, m)?;
                self.clipper_windows.insert(clipper, 0);
                m.put_u32(a[2], clipper)?;
                0
            }
            api::CLIPPER_INITIALIZE => {
                if matches!(kind, Kind::Clipper) {
                    0
                } else {
                    INVALID
                }
            }
            api::CLIPPER_SET_HWND => {
                if !matches!(kind, Kind::Clipper) || a[1] != 0 {
                    return Ok(INVALID);
                }
                self.clipper_windows.insert(a[0], a[2]);
                0
            }
            api::CLIPPER_GET_HWND => {
                if !matches!(kind, Kind::Clipper) {
                    return Ok(INVALID);
                }
                m.put_u32(a[1], *self.clipper_windows.get(&a[0]).unwrap_or(&0))?;
                0
            }
            api::SET_CLIPPER => {
                let Some(sid) = self.surface_id(a[0]) else {
                    return Ok(INVALID);
                };
                if a[1] != 0
                    && !self
                        .objects
                        .get(&a[1])
                        .is_some_and(|o| matches!(o.kind, Kind::Clipper))
                {
                    return Ok(INVALID);
                }
                if let Some(old) = self.surface_clippers.remove(&sid)
                    && let Some(object) = self.objects.get_mut(&old)
                {
                    object.refs = object.refs.saturating_sub(1);
                }
                if a[1] != 0 {
                    self.objects.get_mut(&a[1]).unwrap().refs += 1;
                    self.surface_clippers.insert(sid, a[1]);
                }
                0
            }
            api::DISPLAY_MODE | api::DISPLAY_MODE_OLD => {
                if !matches!(kind, Kind::Device)
                    || size(a[1], a[2]).is_err()
                    || !matches!(a[3], 16 | 32)
                {
                    return Ok(INVALID);
                }
                if self.primary.is_some() {
                    return Ok(BUSY);
                }
                self.width = a[1];
                self.height = a[2];
                self.display_format = if a[3] == 16 {
                    PixelFormat::Rgb565
                } else {
                    PixelFormat::Bgra32
                };
                0
            }
            api::RESTORE_MODE => {
                if matches!(kind, Kind::Device) {
                    0
                } else {
                    INVALID
                }
            }
            api::GET_DISPLAY_MODE => {
                if !matches!(kind, Kind::Device) {
                    return Ok(INVALID);
                }
                let len = m.u32(a[1])?;
                if !matches!(len, 108 | 124) {
                    return Ok(INVALID);
                }
                let sid = self.primary(m)?;
                let mut desc = self.desc(sid, 0);
                desc[..4].copy_from_slice(&len.to_le_bytes());
                m.write(a[1], &desc[..len as usize])?;
                0
            }
            api::GET_CAPS => {
                if !matches!(kind, Kind::Device) {
                    return Ok(INVALID);
                }
                for &p in &a[1..3] {
                    if p != 0 {
                        let len = m.u32(p)?;
                        if !(4..=380).contains(&len) {
                            return Ok(INVALID);
                        }
                        let mut caps = vec![0; len as usize];
                        caps[..4].copy_from_slice(&len.to_le_bytes());
                        for (offset, value) in [
                            (4usize, 0x00400040u32),
                            (88, 64 * 1024 * 1024),
                            (92, 32 * 1024 * 1024),
                        ] {
                            if offset + 4 <= caps.len() {
                                caps[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
                            }
                        }
                        m.write(p, &caps)?;
                    }
                }
                0
            }
            api::CREATE_SURFACE => {
                m.put_u32(a[2], 0)?;
                let desc_len = m.u32(a[1])?;
                if !matches!(kind, Kind::Device) || a[3] != 0 || !matches!(desc_len, 108 | 124) {
                    return Ok(INVALID);
                }
                let flags = m.u32(a[1] + 4)?;
                let caps = m.u32(a[1] + 104)?;
                if flags & 1 == 0
                    || flags & !0x1027 != 0
                    || (desc_len == 124 && m.read(a[1] + 108, 12)?.iter().any(|&b| b != 0))
                {
                    return Ok(UNSUPPORTED);
                }
                let sid = if caps & 0x200 != 0 {
                    let primary = self.primary(m)?;
                    if caps & 0x18 == 0x18 && flags & 0x20 != 0 && m.u32(a[1] + 20)? > 0 {
                        let back = self.new_surface_format(
                            self.width,
                            self.height,
                            false,
                            self.display_format,
                            m,
                        )?;
                        self.attached.insert(primary, back);
                    }
                    primary
                } else {
                    if caps & !(0x40 | 0x800 | 0x4000 | 0x1000 | 4 | 0x10000000) != 0
                        || flags & 7 != 7
                    {
                        return Ok(UNSUPPORTED);
                    }
                    let format = if flags & 0x1000 == 0 {
                        self.display_format
                    } else {
                        match (
                            m.u32(a[1] + 84)?,
                            m.u32(a[1] + 88)?,
                            m.u32(a[1] + 92)?,
                            m.u32(a[1] + 96)?,
                        ) {
                            (32, 0xff0000, 0xff00, 0xff) => PixelFormat::Bgra32,
                            (16, 0xf800, 0x7e0, 0x1f) => PixelFormat::Rgb565,
                            (16, 0x7c00, 0x3e0, 0x1f) => PixelFormat::Rgb555,
                            _ => return Ok(UNSUPPORTED),
                        }
                    };
                    let (w, h) = (m.u32(a[1] + 12)?, m.u32(a[1] + 8)?);
                    if size(w, h).is_err() {
                        return Ok(INVALID);
                    }
                    self.new_surface_format(w, h, false, format, m)?
                };
                let p = self.com_version(Kind::Surface(sid), legacy, m)?;
                m.put_u32(a[2], p)?;
                0
            }
            api::LOCK => {
                let Some(sid) = self.surface_id(a[0]) else {
                    return Ok(INVALID);
                };
                let len = m.u32(a[2])?;
                if !matches!(len, 108 | 124) || a[4] != 0 {
                    return Ok(INVALID);
                }
                if a[3] & !(0x11 | 0x800 | 0x2000 | 0x4000) != 0 {
                    return Ok(UNSUPPORTED);
                }
                if self.surfaces[&sid].locked.is_some() {
                    return Ok(BUSY);
                }
                let Ok(rect) = self.rect(sid, a[1], m) else {
                    return Ok(INVALID);
                };
                let p = self.surfaces[&sid].row(rect.x, rect.y);
                let mut desc = self.desc(sid, p);
                desc[..4].copy_from_slice(&len.to_le_bytes());
                m.write(a[2], &desc[..len as usize])?;
                self.surfaces.get_mut(&sid).unwrap().locked = Some((rect, a[3] & 0x10 != 0));
                self.stats.locks += 1;
                0
            }
            api::UNLOCK => {
                let Some(sid) = self.surface_id(a[0]) else {
                    return Ok(INVALID);
                };
                let Some((locked, readonly)) = self.surfaces[&sid].locked else {
                    return Ok(NOT_LOCKED);
                };
                if a[1] != 0
                    && a[1] != self.surfaces[&sid].row(locked.x, locked.y)
                    && self.rect(sid, a[1], m).ok() != Some(locked)
                {
                    return Ok(INVALID);
                }
                let s = self.surfaces.get_mut(&sid).unwrap();
                s.locked = None;
                if !readonly {
                    s.version += 1;
                }
                self.stats.unlocks += 1;
                if self.primary == Some(sid) {
                    self.present(sid, m)?;
                }
                0
            }
            api::GET_DESC => {
                let Some(sid) = self.surface_id(a[0]) else {
                    return Ok(INVALID);
                };
                let len = m.u32(a[1])?;
                if !matches!(len, 108 | 124) {
                    return Ok(INVALID);
                }
                let mut desc = self.desc(sid, 0);
                desc[..4].copy_from_slice(&len.to_le_bytes());
                m.write(a[1], &desc[..len as usize])?;
                0
            }
            api::IS_LOST | api::RESTORE => {
                if matches!(kind, Kind::Surface(_)) {
                    0
                } else {
                    INVALID
                }
            }
            api::GET_PIXEL_FORMAT => {
                let Some(sid) = self.surface_id(a[0]) else {
                    return Ok(INVALID);
                };
                m.write(a[1], &self.desc(sid, 0)[72..104])?;
                0
            }
            api::GET_ATTACHED => {
                m.put_u32(a[2], 0)?;
                let Some(sid) = self.surface_id(a[0]) else {
                    return Ok(INVALID);
                };
                let Some(&back) = self.attached.get(&sid) else {
                    return Ok(0x887600ff);
                };
                let p = self.com_version(Kind::Surface(back), legacy, m)?;
                m.put_u32(a[2], p)?;
                0
            }
            api::FLIP => {
                let Some(dst) = self.surface_id(a[0]) else {
                    return Ok(INVALID);
                };
                let src = if a[1] != 0 {
                    self.surface_id(a[1])
                } else {
                    self.attached.get(&dst).copied()
                };
                let Some(src) = src else { return Ok(INVALID) };
                if self.surfaces[&dst].locked.is_some() || self.surfaces[&src].locked.is_some() {
                    return Ok(BUSY);
                }
                let front = self.surfaces[&dst].pixels;
                self.surfaces.get_mut(&dst).unwrap().pixels = self.surfaces[&src].pixels;
                self.surfaces.get_mut(&src).unwrap().pixels = front;
                self.present(dst, m)?;
                0
            }
            api::SURFACE_DC => {
                let Some(sid) = self.surface_id(a[0]) else {
                    return Ok(INVALID);
                };
                if self.surfaces[&sid].locked.is_some() {
                    return Ok(BUSY);
                }
                let dc = self.next;
                self.next += 1;
                self.dcs.insert(dc, sid);
                m.put_u32(a[1], dc)?;
                0
            }
            api::SURFACE_RELEASE_DC => {
                let Some(sid) = self.surface_id(a[0]) else {
                    return Ok(INVALID);
                };
                if self.dcs.remove(&a[1]) != Some(sid) {
                    return Ok(INVALID);
                }
                if self.primary == Some(sid) {
                    self.present(sid, m)?;
                }
                0
            }
            api::BLT_FAST => {
                let (Some(dst), Some(src)) = (self.surface_id(a[0]), self.surface_id(a[3])) else {
                    return Ok(INVALID);
                };
                if a[5] & !0x10 != 0 {
                    return Ok(UNSUPPORTED);
                }
                let Ok(sr) = self.rect(src, a[4], m) else {
                    return Ok(INVALID);
                };
                let dr = Rect {
                    x: a[1],
                    y: a[2],
                    ..sr
                };
                let d = &self.surfaces[&dst];
                if dr.x.checked_add(dr.w).is_none_or(|r| r > d.width)
                    || dr.y.checked_add(dr.h).is_none_or(|b| b > d.height)
                {
                    return Ok(INVALID);
                }
                if d.locked.is_some() || self.surfaces[&src].locked.is_some() {
                    return Ok(BUSY);
                }
                self.copy(src, sr, dst, dr, m)?;
                if self.primary == Some(dst) {
                    self.present(dst, m)?;
                }
                0
            }
            api::BLT => {
                if a[4] & 0x400 != 0 {
                    let Some(dst) = self.surface_id(a[0]) else {
                        return Ok(INVALID);
                    };
                    if a[5] == 0 || a[4] & !(0x400 | 0x01000000) != 0 {
                        return Ok(UNSUPPORTED);
                    }
                    let Ok(dr) = self.rect(dst, a[1], m) else {
                        return Ok(INVALID);
                    };
                    let s = &self.surfaces[&dst];
                    if s.locked.is_some() {
                        return Ok(BUSY);
                    }
                    let color = m.u32(a[5] + 80)?.to_le_bytes();
                    let row: Vec<u8> = (0..dr.w)
                        .flat_map(|_| color[..s.format.bytes() as usize].iter().copied())
                        .collect();
                    for y in dr.y..dr.y + dr.h {
                        m.write(s.row(dr.x, y), &row)?;
                    }
                    if self.primary == Some(dst) {
                        self.present(dst, m)?;
                    }
                    return Ok(0);
                }
                let (Some(dst), Some(src)) = (self.surface_id(a[0]), self.surface_id(a[2])) else {
                    return Ok(INVALID);
                };
                if a[4] & !0x01000000 != 0 || a[5] != 0 {
                    return Ok(UNSUPPORTED);
                }
                if self.surfaces[&dst].locked.is_some() || self.surfaces[&src].locked.is_some() {
                    return Ok(BUSY);
                }
                let Ok(mut sr) = self.rect(src, a[3], m) else {
                    return Ok(INVALID);
                };
                let origin = if self.primary == Some(dst) {
                    self.surface_clippers
                        .get(&dst)
                        .and_then(|clipper| self.clipper_origins.get(clipper))
                        .copied()
                } else {
                    None
                };
                let dr = if let Some((ox, oy)) = origin.filter(|_| a[1] != 0) {
                    let (left, top, right, bottom) = (
                        i64::from(m.u32(a[1])? as i32) - i64::from(ox),
                        i64::from(m.u32(a[1] + 4)? as i32) - i64::from(oy),
                        i64::from(m.u32(a[1] + 8)? as i32) - i64::from(ox),
                        i64::from(m.u32(a[1] + 12)? as i32) - i64::from(oy),
                    );
                    if right - left != i64::from(sr.w) || bottom - top != i64::from(sr.h) {
                        return Ok(UNSUPPORTED);
                    }
                    let clipped_left = left.max(0);
                    let clipped_top = top.max(0);
                    let clipped_right = right.min(i64::from(self.surfaces[&dst].width));
                    let clipped_bottom = bottom.min(i64::from(self.surfaces[&dst].height));
                    if clipped_right <= clipped_left || clipped_bottom <= clipped_top {
                        return Ok(0);
                    }
                    sr.x += (clipped_left - left) as u32;
                    sr.y += (clipped_top - top) as u32;
                    sr.w = (clipped_right - clipped_left) as u32;
                    sr.h = (clipped_bottom - clipped_top) as u32;
                    Rect {
                        x: clipped_left as u32,
                        y: clipped_top as u32,
                        w: sr.w,
                        h: sr.h,
                    }
                } else {
                    let Ok(dr) = self.rect(dst, a[1], m) else {
                        return Ok(INVALID);
                    };
                    dr
                };
                if dr.w != sr.w || dr.h != sr.h {
                    return Ok(UNSUPPORTED);
                }
                self.copy(src, sr, dst, dr, m)?;
                if self.primary == Some(dst) {
                    self.present(dst, m)?;
                }
                0
            }
            _ => return Err(format!("unknown DirectDraw API {id}")),
        })
    }
}
