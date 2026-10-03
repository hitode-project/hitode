//! DirectDraw: `DirectDrawCreate(Ex)` and the IDirectDraw, IDirectDrawSurface and
//! IDirectDrawClipper objects. Objects and vtables live in guest memory from the surface
//! arena, so a guest can read or patch them; every vtable slot points at a registry thunk.
//! Pixels belong to the shared surface store; a surface lends GDI a DC from the shared table.
use std::collections::BTreeMap;

use adv_device::surface::{self, PixelFormat, Presenter, Rect, SurfaceMemory, Surfaces};
use adv_os::gdi::DeviceContexts;

pub mod api {
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

pub const DEVICE_METHODS: &[&str] = &[
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
pub const SURFACE_METHODS: &[&str] = &[
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

/// (id, module, name, argc) of every DirectDraw call; COM methods are named
/// `Interface::Method`.
pub const API_SPECS: &[(u32, &str, &str, u8)] = &[
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

pub struct DirectDraw {
    objects: BTreeMap<u32, Com>,
    dd_vtable: Option<u32>,
    old_dd_vtable: Option<u32>,
    surface_vtable: Option<u32>,
    clipper_vtable: Option<u32>,
    clipper_windows: BTreeMap<u32, u32>,
    surface_clippers: BTreeMap<u32, u32>,
    clipper_origins: BTreeMap<u32, (i32, i32)>,
    /// Call id -> guest entry; 400 + slot and 500 + slot are the device and surface
    /// vtables' diagnostic entries.
    entries: BTreeMap<u32, u32>,
    attached: BTreeMap<u32, u32>,
}

impl DirectDraw {
    pub fn new(entries: BTreeMap<u32, u32>) -> Self {
        Self {
            objects: BTreeMap::new(),
            dd_vtable: None,
            old_dd_vtable: None,
            surface_vtable: None,
            clipper_vtable: None,
            clipper_windows: BTreeMap::new(),
            surface_clippers: BTreeMap::new(),
            clipper_origins: BTreeMap::new(),
            entries,
            attached: BTreeMap::new(),
        }
    }

    /// Screen origin of the window a clipper names; primary-surface blits are relative to it.
    pub fn configure_clipper(&mut self, clipper: u32, origin: (i32, i32)) {
        self.clipper_origins.insert(clipper, origin);
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

impl DirectDraw {
    fn table<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
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
        let p = surfaces.alloc(slots * 4, m)?;
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
    fn com_version<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        kind: Kind,
        legacy: bool,
        m: &mut dyn SurfaceMemory,
    ) -> Result<u32, String> {
        let vtable = if matches!(kind, Kind::Clipper) {
            if let Some(table) = self.clipper_vtable {
                table
            } else {
                let table = surfaces.alloc(9 * 4, m)?;
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
            self.table(surfaces, matches!(kind, Kind::Surface(_)), legacy, m)?
        };
        let p = surfaces.alloc(16, m)?;
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
}

fn desc<P: Presenter>(surfaces: &Surfaces<P>, id: u32, pixels: u32) -> [u8; 124] {
    let s = &surfaces[id];
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
            if surfaces.primary_id() == Some(id) {
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

fn rect<P: Presenter>(
    surfaces: &Surfaces<P>,
    id: u32,
    p: u32,
    m: &dyn SurfaceMemory,
) -> Result<Rect, String> {
    let s = &surfaces[id];
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

impl DirectDraw {
    /// Run DirectDraw call `id` with arguments `a`; COM methods take the object first.
    pub fn call<P: Presenter>(
        &mut self,
        id: u32,
        a: &[u32],
        surfaces: &mut Surfaces<P>,
        dcs: &mut DeviceContexts,
        m: &mut dyn SurfaceMemory,
    ) -> Result<u32, String> {
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
            let p = self.com_version(surfaces, Kind::Device, id == api::DD_CREATE_OLD, m)?;
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
                    let object = self.com_version(surfaces, kind, !legacy, m)?;
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
                    surfaces.release(a[0], 16);
                    if let Kind::Surface(id) = kind
                        && surfaces.primary_id() != Some(id)
                        && !self
                            .objects
                            .values()
                            .any(|o| matches!(o.kind,Kind::Surface(other) if other==id))
                        && !self.attached.values().any(|&other| other == id)
                    {
                        surfaces.delete(id);
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
                let clipper = self.com_version(surfaces, Kind::Clipper, false, m)?;
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
                    || surface::size(a[1], a[2]).is_err()
                    || !matches!(a[3], 16 | 32)
                {
                    return Ok(INVALID);
                }
                if surfaces.primary_id().is_some() {
                    return Ok(BUSY);
                }
                surfaces.width = a[1];
                surfaces.height = a[2];
                surfaces.display_format = if a[3] == 16 {
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
                let sid = surfaces.primary(m)?;
                let mut desc = desc(surfaces, sid, 0);
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
                    let primary = surfaces.primary(m)?;
                    if caps & 0x18 == 0x18 && flags & 0x20 != 0 && m.u32(a[1] + 20)? > 0 {
                        let back = surfaces.create(
                            surfaces.width,
                            surfaces.height,
                            false,
                            surfaces.display_format,
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
                        surfaces.display_format
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
                    if surface::size(w, h).is_err() {
                        return Ok(INVALID);
                    }
                    surfaces.create(w, h, false, format, m)?
                };
                let p = self.com_version(surfaces, Kind::Surface(sid), legacy, m)?;
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
                if surfaces[sid].locked.is_some() {
                    return Ok(BUSY);
                }
                let Ok(rect) = rect(surfaces, sid, a[1], m) else {
                    return Ok(INVALID);
                };
                let p = surfaces[sid].row(rect.x, rect.y);
                let mut desc = desc(surfaces, sid, p);
                desc[..4].copy_from_slice(&len.to_le_bytes());
                m.write(a[2], &desc[..len as usize])?;
                surfaces.get_mut(sid).unwrap().locked = Some((rect, a[3] & 0x10 != 0));
                surfaces.stats.locks += 1;
                0
            }
            api::UNLOCK => {
                let Some(sid) = self.surface_id(a[0]) else {
                    return Ok(INVALID);
                };
                let Some((locked, readonly)) = surfaces[sid].locked else {
                    return Ok(NOT_LOCKED);
                };
                if a[1] != 0
                    && a[1] != surfaces[sid].row(locked.x, locked.y)
                    && rect(surfaces, sid, a[1], m).ok() != Some(locked)
                {
                    return Ok(INVALID);
                }
                let s = surfaces.get_mut(sid).unwrap();
                s.locked = None;
                if !readonly {
                    s.version += 1;
                }
                surfaces.stats.unlocks += 1;
                if surfaces.primary_id() == Some(sid) {
                    surfaces.present(sid, m)?;
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
                let mut desc = desc(surfaces, sid, 0);
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
                m.write(a[1], &desc(surfaces, sid, 0)[72..104])?;
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
                let p = self.com_version(surfaces, Kind::Surface(back), legacy, m)?;
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
                if surfaces[dst].locked.is_some() || surfaces[src].locked.is_some() {
                    return Ok(BUSY);
                }
                let front = surfaces[dst].pixels;
                surfaces.get_mut(dst).unwrap().pixels = surfaces[src].pixels;
                surfaces.get_mut(src).unwrap().pixels = front;
                surfaces.present(dst, m)?;
                0
            }
            api::SURFACE_DC => {
                let Some(sid) = self.surface_id(a[0]) else {
                    return Ok(INVALID);
                };
                if surfaces[sid].locked.is_some() {
                    return Ok(BUSY);
                }
                let dc = surfaces.new_id();
                dcs.insert(dc, sid);
                m.put_u32(a[1], dc)?;
                0
            }
            api::SURFACE_RELEASE_DC => {
                let Some(sid) = self.surface_id(a[0]) else {
                    return Ok(INVALID);
                };
                if dcs.get(a[1]) != Some(sid) {
                    return Ok(INVALID);
                }
                dcs.remove(a[1]);
                if surfaces.primary_id() == Some(sid) {
                    surfaces.present(sid, m)?;
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
                let Ok(sr) = rect(surfaces, src, a[4], m) else {
                    return Ok(INVALID);
                };
                let dr = Rect {
                    x: a[1],
                    y: a[2],
                    ..sr
                };
                let d = &surfaces[dst];
                if dr.x.checked_add(dr.w).is_none_or(|r| r > d.width)
                    || dr.y.checked_add(dr.h).is_none_or(|b| b > d.height)
                {
                    return Ok(INVALID);
                }
                if d.locked.is_some() || surfaces[src].locked.is_some() {
                    return Ok(BUSY);
                }
                surfaces.copy(src, sr, dst, dr, m)?;
                if surfaces.primary_id() == Some(dst) {
                    surfaces.present(dst, m)?;
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
                    let Ok(dr) = rect(surfaces, dst, a[1], m) else {
                        return Ok(INVALID);
                    };
                    let s = &surfaces[dst];
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
                    if surfaces.primary_id() == Some(dst) {
                        surfaces.present(dst, m)?;
                    }
                    return Ok(0);
                }
                let (Some(dst), Some(src)) = (self.surface_id(a[0]), self.surface_id(a[2])) else {
                    return Ok(INVALID);
                };
                if a[4] & !0x01000000 != 0 || a[5] != 0 {
                    return Ok(UNSUPPORTED);
                }
                if surfaces[dst].locked.is_some() || surfaces[src].locked.is_some() {
                    return Ok(BUSY);
                }
                let Ok(mut sr) = rect(surfaces, src, a[3], m) else {
                    return Ok(INVALID);
                };
                let origin = if surfaces.primary_id() == Some(dst) {
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
                    let clipped_right = right.min(i64::from(surfaces[dst].width));
                    let clipped_bottom = bottom.min(i64::from(surfaces[dst].height));
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
                    let Ok(dr) = rect(surfaces, dst, a[1], m) else {
                        return Ok(INVALID);
                    };
                    dr
                };
                if dr.w != sr.w || dr.h != sr.h {
                    return Ok(UNSUPPORTED);
                }
                surfaces.copy(src, sr, dst, dr, m)?;
                if surfaces.primary_id() == Some(dst) {
                    surfaces.present(dst, m)?;
                }
                0
            }
            _ => return Err(format!("unknown DirectDraw API {id}")),
        })
    }
}
