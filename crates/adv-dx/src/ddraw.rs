//! DirectDraw: `DirectDrawCreate(Ex)` and the IDirectDraw, IDirectDrawSurface and
//! IDirectDrawClipper objects. Objects and vtables live in guest memory from the surface
//! arena, so a guest can read or patch them; every vtable slot points at a registry thunk.
//! Pixels belong to the shared surface store; assembly lends a memory DC over them.
use crate::argument::{Arguments, CallError};
use std::collections::BTreeMap;

use adv_abi::com::{Com as CommonCom, ComIdentity, QueryError};
use adv_device::surface::{
    self, Access, BlitRect, Blt, BltError, BltFailure, BltSurface, MemoryClass, PixelFormat,
    Presenter, Rect, SurfaceMemory, Surfaces,
};
use adv_machine::Objects;

mod creation;
pub mod d3d;
pub mod front;
pub use front::{Provider, WindowGeometry, install, register};

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
    pub const ENUM_MODES: u32 = 230;
    pub const INITIALIZE: u32 = 231;
    pub const SET_COLOR_KEY: u32 = 232;
    pub const GET_COLOR_KEY: u32 = 233;
    pub const GET_CLIPPER: u32 = 234;
    pub const CLIPPER_SET_CLIP_LIST: u32 = 235;
    pub const GET_BLT_STATUS: u32 = 236;
    pub const GET_FLIP_STATUS: u32 = 237;
    pub const FLIP_TO_GDI: u32 = 238;
    pub const COMPACT: u32 = 239;
    pub const RESTORE_ALL_SURFACES: u32 = 240;
    pub const TEST_COOPERATIVE_LEVEL: u32 = 241;
    pub const DD_CREATE_CLIPPER: u32 = 242;
    pub const GET_GDI_SURFACE: u32 = 243;
    pub const GET_DD_INTERFACE: u32 = 244;
    pub const PAGE_LOCK: u32 = 245;
    pub const PAGE_UNLOCK: u32 = 246;
    pub const GET_UNIQUENESS: u32 = 247;
    pub const CHANGE_UNIQUENESS: u32 = 248;
    pub const SURFACE_CAPS: u32 = 249;
    pub const SURFACE_INITIALIZE: u32 = 250;
    pub const ADD_ATTACHED: u32 = 251;
    pub const DELETE_ATTACHED: u32 = 252;
    pub const SET_SURFACE_DESC: u32 = 253;
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
/// Offscreen surfaces: plain, back buffer, texture, in system, local or
/// nonlocal video memory, which share one storage.
const SURFACE_CAPS: u32 = 0x40 | 4 | 0x1000 | 0x800 | 0x4000 | 0x1000_0000 | 0x2000_0000;
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
const INVALID_RECT: u32 = 0x88760096;
const NO_COLOR_KEY_HW: u32 = 0x887600dc;
const UNSUPPORTED: u32 = 0x80004001;
const BUSY: u32 = 0x887601ae;
const NOT_FOUND: u32 = 0x887600ff;
const NO_EXCLUSIVE_MODE: u32 = 0x887600e1;
const EXCLUSIVE_MODE_ALREADY_SET: u32 = 0x88760245;
const SURFACE_LOST: u32 = 0x887601c2;
const WRONG_MODE: u32 = 0x8876024b;
const NO_CLIPPER_ATTACHED: u32 = 0x88760238;
const INVALID_CLIP_LIST: u32 = 0x8876006e;
const CLIPPER_IS_USING_HWND: u32 = 0x88760237;
const BLT_FAST_CANT_CLIP: u32 = 0x8876023e;
const NO_COLOR_KEY: u32 = 0x887600d7;
const NO_CLIP_LIST: u32 = 0x887600cd;
const REGION_TOO_SMALL: u32 = 0x88760236;
const ALREADY_INITIALIZED: u32 = 0x88760005;
/// DDCKEY_DESTBLT, DDCKEY_DESTOVERLAY, DDCKEY_SRCBLT and DDCKEY_SRCOVERLAY.
const KEY_KINDS: [u32; 4] = [2, 4, 8, 0x10];
const NOT_LOCKED: u32 = 0x88760248;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Loss {
    Mode,
    Activation,
}

pub struct DirectDraw {
    pub(super) context: u32,
    submitted: Option<u32>,
    com: Option<CommonCom>,
    pending_guest: Vec<ComIdentity<ComState>>,
    #[cfg(feature = "gpu")]
    d3d: Option<d3d::State>,
    #[cfg(feature = "gpu")]
    d3d_surface_devices: BTreeMap<u32, d3d::object::Device>,
    /// Non-owning index; all payload and reference ownership lives in Objects.
    surface_objects: BTreeMap<u32, ComIdentity<ComState>>,
    /// Non-owning index from device_id to live device identity for GetDDInterface.
    device_objects: BTreeMap<u64, ComIdentity<ComState>>,
    /// Leased GDI memory DC -> surface id for IDirectDraw7::GetSurfaceFromDC.
    pub(super) surface_dcs: BTreeMap<u32, u32>,
    /// Active PageLock counts per surface id.
    page_locks: BTreeMap<u32, u32>,
    dd_vtable: Option<u32>,
    old_dd_vtable: Option<u32>,
    surface_vtable: Option<u32>,
    clipper_vtable: Option<u32>,
    clipper_windows: BTreeMap<u32, u32>,
    surface_clippers: BTreeMap<u32, u32>,
    /// (surface, DDCKEY kind) -> inclusive raw pixel range.
    color_keys: BTreeMap<(u32, u32), (u32, u32)>,
    /// The completed descriptor each surface was created with.
    described: BTreeMap<u32, creation::Described>,
    /// The DirectDraw4/7 interface each surface created through it holds a
    /// reference on until it is destroyed.
    creators: BTreeMap<u32, u32>,
    clipper_origins: BTreeMap<u32, (i32, i32)>,
    /// Explicit clip lists by clipper; Blt writes only inside these rectangles.
    clip_lists: BTreeMap<u32, Vec<Rect>>,
    /// Call id -> guest entry; 400 + slot and 500 + slot are the device and surface
    /// vtables' diagnostic entries.
    entries: BTreeMap<u32, u32>,
    /// Each surface's implicit chain successors: the next flip buffer or
    /// mipmap level, created with the chain's root.
    complex: BTreeMap<u32, Vec<u32>>,
    /// Chain member -> its root and the interface the root holds a reference on.
    members: BTreeMap<u32, (u32, u32)>,
    /// Explicit attachments: each surface and the interface it was attached
    /// through, which the parent holds a reference on.
    attachments: BTreeMap<u32, Vec<(u32, u32)>>,
    /// The live DirectDraw object (its modern interface) that created the
    /// primary surface; none once that object is gone.
    primary_owner: Option<u32>,
    /// Monotonic id of the live device holding exclusive fullscreen mode.
    exclusive_owner: Option<u64>,
    next_device_id: u64,
    /// Owning device id for each surface sid at creation time.
    surface_owners: BTreeMap<u32, u64>,
    /// Current screen origin of the window-backed primary for this operation.
    primary_origin: Option<(i32, i32)>,
    display: Option<front::modes::DisplayModes>,
    palette_table: u32,
    surface_palettes: BTreeMap<u32, u32>,
    creation_modes: BTreeMap<u32, front::modes::DisplayMode>,
    /// Surfaces lost to an exclusive or display mode change.
    lost: BTreeMap<u32, Loss>,
    /// Primary flip chains -> the member holding GDI's memory, which flips
    /// move along the chain.
    gdi: BTreeMap<u32, u32>,
}

impl DirectDraw {
    pub fn new(entries: BTreeMap<u32, u32>) -> Self {
        Self {
            context: 0,
            submitted: None,
            com: None,
            pending_guest: Vec::new(),
            #[cfg(feature = "gpu")]
            d3d: None,
            #[cfg(feature = "gpu")]
            d3d_surface_devices: BTreeMap::new(),
            surface_objects: BTreeMap::new(),
            device_objects: BTreeMap::new(),
            surface_dcs: BTreeMap::new(),
            page_locks: BTreeMap::new(),
            dd_vtable: None,
            old_dd_vtable: None,
            surface_vtable: None,
            clipper_vtable: None,
            clipper_windows: BTreeMap::new(),
            surface_clippers: BTreeMap::new(),
            color_keys: BTreeMap::new(),
            described: BTreeMap::new(),
            creators: BTreeMap::new(),
            clipper_origins: BTreeMap::new(),
            clip_lists: BTreeMap::new(),
            entries,
            complex: BTreeMap::new(),
            members: BTreeMap::new(),
            attachments: BTreeMap::new(),
            primary_owner: None,
            exclusive_owner: None,
            next_device_id: 1,
            surface_owners: BTreeMap::new(),
            primary_origin: None,
            display: None,
            palette_table: 0,
            surface_palettes: BTreeMap::new(),
            creation_modes: BTreeMap::new(),
            lost: BTreeMap::new(),
            gdi: BTreeMap::new(),
        }
    }

    fn present<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        sid: u32,
        m: &mut dyn SurfaceMemory,
        damage: Rect,
    ) -> Result<(), CallError> {
        if !damage.empty() {
            surfaces.cpu_access(sid, damage, Access::Read, m)?;
        }
        surfaces.present_from(self.context, sid, m, damage)?;
        if !damage.clipped(surfaces[sid].rect()).empty() {
            self.submitted = Some(sid);
        }
        Ok(())
    }

    /// Move the memory of each flip `chain` surface to the one before it and
    /// the front's memory to the last, then present the front.
    fn rotate<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        m: &mut dyn SurfaceMemory,
        chain: &[u32],
    ) -> Result<u32, CallError> {
        if chain.iter().any(|&sid| surfaces[sid].locked.is_some()) {
            return Ok(BUSY);
        }
        let head = &surfaces[chain[0]];
        let layout = (head.width, head.height, head.pitch, head.format);
        if chain[1..].iter().any(|&sid| {
            let s = &surfaces[sid];
            (s.width, s.height, s.pitch, s.format) != layout
        }) {
            return Err("DirectDraw flip chain surfaces have mismatched geometry".into());
        }
        surfaces.flush(m, surface::FlushReason::Immediate)?;
        let front = surfaces[chain[0]].pixels;
        for pair in chain.windows(2) {
            surfaces.get_mut(pair[0]).unwrap().pixels = surfaces[pair[1]].pixels;
        }
        surfaces.get_mut(*chain.last().unwrap()).unwrap().pixels = front;
        if let Some(at) = self.gdi.get_mut(&chain[0])
            && let Some(i) = chain.iter().position(|sid| sid == at)
        {
            *at = chain[(i + chain.len() - 1) % chain.len()];
        }
        self.present(surfaces, chain[0], m, surfaces[chain[0]].rect())?;
        Ok(0)
    }

    /// Create the chain `root` was described with: back buffers leading back
    /// to the root, or smaller mipmap levels. False when an object could not
    /// be made; the root's release then ends what was built.
    #[allow(clippy::too_many_arguments)]
    fn build_chain<P: Presenter>(
        &mut self,
        objects: &mut Objects,
        surfaces: &mut Surfaces<P>,
        m: &mut dyn SurfaceMemory,
        root: u32,
        d: creation::Described,
        version: u8,
        owner: u64,
    ) -> Result<bool, CallError> {
        use creation::*;
        let (width, height, format) = {
            let s = &surfaces[root];
            (s.width, s.height, s.format)
        };
        let flip = d.caps[0] & FLIP != 0;
        let mut levels = Vec::new();
        if flip {
            for i in 0..d.backbuffers {
                let mut member = d;
                member.caps[0] &= !(VISIBLE | PRIMARY | FRONTBUFFER | BACKBUFFER);
                if i == 0 {
                    member.caps[0] |= BACKBUFFER;
                }
                member.backbuffers = 0;
                levels.push((width, height, member));
            }
        } else if d.caps[0] & MIPMAP != 0 {
            for j in 1..d.mipmaps {
                let mut member = d;
                (member.width, member.height) = ((width >> j).max(1), (height >> j).max(1));
                member.mipmaps = d.mipmaps - j;
                member.caps[1] |= MIPMAP_SUBLEVEL;
                levels.push((member.width, member.height, member));
            }
        }
        let mut parent = root;
        for (w, h, member) in levels {
            let sid = surfaces.create(w, h, false, format, m)?;
            #[cfg(feature = "gpu")]
            if self.d3d.is_some() {
                surfaces.set_packed_color(sid, member.packed_render_masks());
            }
            self.described.insert(sid, member);
            let result = self.com_version(objects, surfaces, Kind::Surface(sid), version, m);
            let Ok(Some(p)) = result else {
                self.described.remove(&sid);
                surfaces.delete(sid);
                result?;
                return Ok(false);
            };
            self.surface_owners.insert(sid, owner);
            if self.display.is_some() {
                self.creation_modes
                    .insert(sid, front::modes::current(surfaces));
            }
            self.members.insert(sid, (root, p));
            self.complex.entry(parent).or_default().push(sid);
            parent = sid;
        }
        if flip {
            self.complex.entry(parent).or_default().push(root);
        }
        Ok(true)
    }

    /// The capabilities surface `sid` reports.
    fn caps(&self, sid: u32) -> [u32; 4] {
        self.described
            .get(&sid)
            .map_or([creation::OFFSCREEN_PLAIN, 0, 0, 0], |d| d.caps)
    }

    fn blt_surface(&self, id: u32, rect: Rect) -> BltSurface {
        let memory = if self.caps(id)[0] & creation::VIDEO_MEMORY != 0 {
            MemoryClass::Video
        } else {
            MemoryClass::System
        };
        BltSurface { id, rect, memory }
    }

    fn execute_blt<P: Presenter>(
        surfaces: &mut Surfaces<P>,
        destination: BltSurface,
        operation: Blt,
        m: &mut dyn SurfaceMemory,
    ) -> Result<Result<Rect, u32>, CallError> {
        match surfaces.blt(destination, operation, m) {
            Ok(damage) => Ok(Ok(damage)),
            Err(BltFailure::Storage(error)) => Err(error.into()),
            Err(BltFailure::Rule(error)) => Ok(Err(match error {
                BltError::InvalidRect => INVALID_RECT,
                BltError::UnsupportedConversion => UNSUPPORTED,
                BltError::ConversionFailed => 0x80004005,
                BltError::NoColorKeyHardware => NO_COLOR_KEY_HW,
                BltError::InvalidArgument => INVALID,
            })),
        }
    }

    /// `front` and the surfaces its flip chain reaches before coming back.
    fn flip_chain(&self, front: u32) -> Vec<u32> {
        let mut chain = vec![front];
        loop {
            let at = *chain.last().unwrap();
            let next = self.complex.get(&at).and_then(|next| {
                next.iter()
                    .copied()
                    .find(|&sid| self.caps(sid)[0] & creation::FLIP != 0)
            });
            match next {
                Some(next) if next == front => return chain,
                Some(next) if !chain.contains(&next) => chain.push(next),
                _ => return vec![front],
            }
        }
    }

    /// Surfaces attached to `sid`: its chain successors, then explicit ones.
    fn attached(&self, sid: u32) -> Vec<u32> {
        let implicit = self.complex.get(&sid).into_iter().flatten().copied();
        let explicit = self.attachments.get(&sid).into_iter().flatten();
        implicit.chain(explicit.map(|&(sid, _)| sid)).collect()
    }

    /// Surface `sid`'s descriptor as `len` bytes, with its pixels at `pixels`
    /// when locked and its current color keys.
    fn describe<P: Presenter>(
        &self,
        surfaces: &Surfaces<P>,
        sid: u32,
        len: u32,
        pixels: u32,
    ) -> Vec<u8> {
        let s = &surfaces[sid];
        let mut d = self
            .described
            .get(&sid)
            .copied()
            .unwrap_or_else(|| creation::Described {
                flags: creation::DDSD_CAPS | 0x1006,
                caps: [0x40, 0, 0, 0],
                format: creation::pixel_format(s.format),
                ..Default::default()
            });
        (d.width, d.height) = (s.width, s.height);
        for (i, &(flag, key)) in creation::KEYS.iter().enumerate() {
            match self.color_keys.get(&(sid, key)) {
                Some(&value) => {
                    d.flags |= flag;
                    d.keys[i] = value;
                }
                None => {
                    d.flags &= !flag;
                    d.keys[i] = (0, 0);
                }
            }
        }
        d.bytes(len, s.pitch, pixels)
    }

    /// Screen origin of the window a clipper names; primary-surface blits are relative to it.
    pub fn configure_clipper(&mut self, clipper: u32, origin: (i32, i32)) {
        self.clipper_origins.insert(clipper, origin);
    }

    /// Whether `surface` belongs to the live exclusive-mode device rather than
    /// sharing window content with GDI (WINE-GFX-11).
    pub(super) fn is_exclusive_surface(&self, surface: u32) -> bool {
        self.exclusive_owner
            .is_some_and(|owner| self.surface_owners.get(&surface) == Some(&owner))
    }

    /// Whether `surface` belongs to a multi-buffer flip chain.
    pub(super) fn is_flippable(&self, surface: u32) -> bool {
        self.flip_chain(surface).len() > 1
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Device,
    Surface(u32),
    Clipper,
}
struct ComState {
    kind: Kind,
    initialized: bool,
    device_id: u64,
    was_exclusive: bool,
    changed_exclusive_mode: bool,
    focus_window: u32,
    windowed: bool,
    /// SetCooperativeLevel has set a normal or exclusive level.
    cooperative: bool,
    old: u32,
    modern: u32,
    #[cfg(feature = "gpu")]
    d3d_device_faces: [u32; 2],
    /// Each interface version's pointer, oldest first.
    versions: Vec<(u8, u32)>,
}

impl ComState {
    /// The interface version `pointer` names.
    fn version(&self, pointer: u32) -> u8 {
        self.versions
            .iter()
            .find(|&&(_, p)| p == pointer)
            .map_or(7, |&(version, _)| version)
    }
    /// The pointer of interface `version`, or of the newest older one.
    fn pointer(&self, version: u8) -> u32 {
        self.versions
            .iter()
            .rev()
            .find(|&&(v, _)| v <= version)
            .map_or(self.old, |&(_, p)| p)
    }
}

/// The surface version a DirectDraw interface version creates and returns.
fn surface_version(device: u8) -> u8 {
    match device {
        1 | 2 => 1,
        4 => 4,
        _ => 7,
    }
}

impl DirectDraw {
    fn table<P: Presenter>(
        &mut self,
        surfaces: &mut Surfaces<P>,
        surface: bool,
        legacy: bool,
        m: &mut dyn SurfaceMemory,
    ) -> Result<u32, CallError> {
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
        // Production tables have a generated, correctly named thunk per slot.
        // Standalone domain fixtures can still supply the common entries.
        let mut methods = if self
            .entries
            .contains_key(&(if surface { 500 } else { 400 }))
        {
            Vec::new()
        } else {
            vec![(0, api::QUERY), (1, api::ADDREF), (2, api::RELEASE)]
        };
        if surface {
            methods.extend([
                (5, api::BLT),
                (7, api::BLT_FAST),
                (11, api::FLIP),
                (12, api::GET_ATTACHED),
                (13, api::GET_BLT_STATUS),
                (14, api::SURFACE_CAPS),
                (17, api::SURFACE_DC),
                (18, api::GET_FLIP_STATUS),
                (21, api::GET_PIXEL_FORMAT),
                (22, api::GET_DESC),
                (23, api::SURFACE_INITIALIZE),
                (24, api::IS_LOST),
                (25, api::LOCK),
                (26, api::SURFACE_RELEASE_DC),
                (27, api::RESTORE),
                (15, api::GET_CLIPPER),
                (16, api::GET_COLOR_KEY),
                (28, api::SET_CLIPPER),
                (29, api::SET_COLOR_KEY),
                (32, api::UNLOCK),
                (36, api::GET_DD_INTERFACE),
                (37, api::PAGE_LOCK),
                (38, api::PAGE_UNLOCK),
                (43, api::GET_UNIQUENESS),
                (44, api::CHANGE_UNIQUENESS),
            ]);
        } else {
            methods.extend([
                (3, api::COMPACT),
                (4, api::CREATE_CLIPPER),
                (6, api::CREATE_SURFACE),
                (8, api::ENUM_MODES),
                (10, api::FLIP_TO_GDI),
                (11, api::GET_CAPS),
                (12, api::GET_DISPLAY_MODE),
                (14, api::GET_GDI_SURFACE),
                (18, api::INITIALIZE),
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
                (25, api::RESTORE_ALL_SURFACES),
                (26, api::TEST_COOPERATIVE_LEVEL),
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
        objects: &mut Objects,
        surfaces: &mut Surfaces<P>,
        kind: Kind,
        version: u8,
        m: &mut dyn SurfaceMemory,
    ) -> Result<Option<u32>, CallError> {
        let legacy = version < 4;
        if self.com.is_none() {
            self.com = CommonCom::new(objects);
        }
        let Some(com) = self.com else {
            return Ok(None);
        };
        if let Kind::Surface(sid) = kind
            && let Some(&identity) = self.surface_objects.get(&sid)
            && let Some(value) = com.get_identity(objects, identity)
        {
            let pointer = value.pointer(version);
            return Ok(com.add_ref::<ComState>(objects, pointer).map(|_| pointer));
        }
        let vtable = if matches!(kind, Kind::Clipper) {
            if let Some(table) = self.clipper_vtable {
                table
            } else {
                let table = surfaces.alloc(9 * 4, m)?;
                for slot in 0..9 {
                    let entry = self
                        .entries
                        .get(&(600 + slot))
                        .or_else(|| self.entries.get(&api::UNIMPLEMENTED_COM))
                        .ok_or("missing generated clipper thunk")?;
                    m.put_u32(table + slot * 4, *entry)?;
                }
                for (slot, id) in [
                    (0, api::QUERY),
                    (1, api::ADDREF),
                    (2, api::RELEASE),
                    (4, api::CLIPPER_GET_HWND),
                    (5, api::CLIPPER_INITIALIZE),
                    (7, api::CLIPPER_SET_CLIP_LIST),
                    (8, api::CLIPPER_SET_HWND),
                ] {
                    if slot >= 3 || !self.entries.contains_key(&600) {
                        m.put_u32(table + slot * 4, self.entries[&id])?;
                    }
                }
                self.clipper_vtable = Some(table);
                table
            }
        } else {
            self.table(surfaces, matches!(kind, Kind::Surface(_)), legacy, m)?
        };
        // Like Windows, every interface version has its own pointer and
        // reference count; all share the canonical IUnknown and the object.
        let mut layouts = match kind {
            Kind::Clipper => vec![(1, IID_CLIPPER, vtable)],
            Kind::Surface(_) => vec![
                (1, IID_SURFACE, vtable),
                (2, IID_SURFACE2, vtable),
                (3, IID_SURFACE3, vtable),
                (4, IID_SURFACE4, vtable),
                (7, IID_SURFACE7, vtable),
            ],
            Kind::Device => {
                let modern = self.table(surfaces, false, false, m)?;
                vec![
                    (1, IID_DDRAW, self.table(surfaces, false, true, m)?),
                    (2, IID_DDRAW2, modern),
                    (4, IID_DDRAW4, modern),
                    (7, IID_DDRAW7, modern),
                ]
            }
        };
        let mut pointers = Vec::new();
        for (_, _, table) in &mut layouts {
            let result = surfaces.alloc(16, m).and_then(|p| {
                pointers.push(p);
                m.put_u32(p, *table)?;
                m.put_u32(p + 4, 0)?;
                Ok(p)
            });
            match result {
                Ok(p) => *table = p,
                Err(error) => {
                    for p in pointers {
                        surfaces.release(p, 16);
                    }
                    return Err(error.into());
                }
            }
        }
        let old = pointers[0];
        let modern = *pointers.last().unwrap();
        let &(_, initial_iid, canonical) = layouts
            .iter()
            .rev()
            .find(|&&(v, _, _)| v <= version)
            .unwrap_or(&layouts[0]);
        let device_id = if matches!(kind, Kind::Device) {
            let id = self.next_device_id;
            self.next_device_id += 1;
            id
        } else {
            0
        };
        let value = ComState {
            kind,
            initialized: true,
            device_id,
            was_exclusive: false,
            changed_exclusive_mode: false,
            focus_window: 0,
            windowed: false,
            cooperative: false,
            old,
            modern,
            #[cfg(feature = "gpu")]
            d3d_device_faces: [0; 2],
            versions: layouts.iter().map(|&(v, _, p)| (v, p)).collect(),
        };
        let Some(identity) = com.create(objects, canonical, initial_iid, value) else {
            for p in pointers {
                surfaces.release(p, 16);
            }
            return Ok(None);
        };
        if !layouts
            .into_iter()
            .filter(|&(_, _, p)| p != canonical)
            .all(|(_, iid, p)| com.register_separate(objects, identity, iid, p))
        {
            com.release_identity(objects, identity);
            for p in pointers {
                surfaces.release(p, 16);
            }
            return Ok(None);
        }
        #[cfg(feature = "gpu")]
        if let Some(state) = &self.d3d {
            let caps = match kind {
                Kind::Surface(sid) => self.caps(sid)[0],
                Kind::Device | Kind::Clipper => 0,
            };
            match d3d::attach_faces(state, com, objects, identity, surfaces, m, caps) {
                Ok(true) => (),
                result => {
                    com.release_identity(objects, identity);
                    for pointer in pointers {
                        surfaces.release(pointer, 16);
                    }
                    result?;
                    return Ok(None);
                }
            }
        }
        match kind {
            Kind::Device => {
                self.device_objects.insert(device_id, identity);
            }
            Kind::Surface(sid) => {
                self.surface_objects.insert(sid, identity);
            }
            Kind::Clipper => (),
        }
        self.pending_guest.push(identity);
        Ok(Some(canonical))
    }
    /// COM activation exposes the same device identity before Initialize.
    fn activate<P: Presenter>(
        &mut self,
        objects: &mut Objects,
        surfaces: &mut Surfaces<P>,
        iid: [u8; 16],
        outer: u32,
        output: u32,
        m: &mut dyn SurfaceMemory,
    ) -> Result<u32, CallError> {
        if outer != 0 {
            return Ok(0x80040110);
        }
        if ![
            adv_abi::com::IUNKNOWN,
            IID_DDRAW,
            IID_DDRAW2,
            IID_DDRAW4,
            IID_DDRAW7,
        ]
        .contains(&iid)
        {
            return Ok(0x80004002);
        }
        let Some(pointer) = self.com_version(objects, surfaces, Kind::Device, 7, m)? else {
            return Ok(0x8007000e);
        };
        let com = self.com.unwrap();
        com.get_mut::<ComState>(objects, pointer)
            .unwrap()
            .initialized = false;
        // The requested version takes the creation reference.
        let target = com.query::<ComState>(objects, pointer, iid);
        self.release_com(objects, surfaces, pointer);
        let Ok(target) = target else {
            return Ok(0x8007000e);
        };
        if let Err(error) = m.arg_put_u32(output, target) {
            self.release_com(objects, surfaces, target);
            return Err(error);
        }
        Ok(0)
    }
    /// The clip list of the clipper attached to surface `sid`, if it has one.
    fn clip_list(&self, sid: u32) -> Option<Vec<Rect>> {
        self.surface_clippers
            .get(&sid)
            .and_then(|clipper| self.clip_lists.get(clipper))
            .cloned()
    }
    /// AddRef and return a surface interface pointer for an existing surface id.
    fn surface_interface<P: Presenter>(
        &mut self,
        objects: &mut Objects,
        surfaces: &mut Surfaces<P>,
        sid: u32,
        version: u8,
        m: &mut dyn SurfaceMemory,
    ) -> Result<Option<u32>, CallError> {
        self.com_version(objects, surfaces, Kind::Surface(sid), version, m)
    }

    fn surface_id(&self, objects: &Objects, object: u32) -> Option<u32> {
        match self.com?.get::<ComState>(objects, object)?.kind {
            Kind::Surface(id) => Some(id),
            Kind::Device | Kind::Clipper => None,
        }
    }

    fn release_com<P: Presenter>(
        &mut self,
        objects: &mut Objects,
        surfaces: &mut Surfaces<P>,
        pointer: u32,
    ) -> u32 {
        let Some(com) = self.com else {
            return 0;
        };
        let Some(released) = com.release::<ComState>(objects, pointer) else {
            return 0;
        };
        if let Some(destroyed) = released.destroyed {
            #[cfg(feature = "gpu")]
            if self.d3d.is_some() && matches!(destroyed.value.kind, Kind::Surface(_)) {
                d3d::release_surface(
                    self,
                    objects,
                    surfaces,
                    destroyed.value.d3d_device_faces,
                    &destroyed.interfaces,
                );
            }
            for pointer in destroyed.interfaces {
                surfaces.release(pointer, 16);
                self.clipper_windows.remove(&pointer);
                self.clipper_origins.remove(&pointer);
                self.clip_lists.remove(&pointer);
            }
            if matches!(destroyed.value.kind, Kind::Device) {
                self.device_objects.remove(&destroyed.value.device_id);
                if self.primary_owner == Some(destroyed.value.modern) {
                    self.primary_owner = None;
                }
                if self.exclusive_owner == Some(destroyed.value.device_id) {
                    self.exclusive_owner = None;
                    surfaces.set_exclusive_display_mode(false);
                    if let Some(display) = &mut self.display {
                        display.requested = display.desktop;
                        display.suspended = false;
                        front::modes::set_current(surfaces, display.desktop);
                        self.lose_video_surfaces(Loss::Mode);
                    }
                }
            }
            if let Kind::Surface(sid) = destroyed.value.kind {
                #[cfg(feature = "gpu")]
                if let Some(d3d) = &mut self.d3d {
                    d3d.forget_surface(sid);
                }
                self.lost.remove(&sid);
                self.creation_modes.remove(&sid);
                if let Some(palette) = self.surface_palettes.remove(&sid) {
                    front::palette::release(com, objects, surfaces, palette);
                }
                self.surface_objects.remove(&sid);
                self.surface_owners.remove(&sid);
                self.page_locks.remove(&sid);
                self.surface_dcs.retain(|_, &mut owner| owner != sid);
                // A primary chain hands GDI's memory back to the primary.
                if let Some(at) = self.gdi.remove(&sid)
                    && at != sid
                    && surfaces.get_mut(at).is_some()
                {
                    let front = surfaces[sid].pixels;
                    surfaces.get_mut(sid).unwrap().pixels = surfaces[at].pixels;
                    surfaces.get_mut(at).unwrap().pixels = front;
                }
                // The root ends its chain; explicit attachments are released.
                self.complex.remove(&sid);
                let held: Vec<u32> = self
                    .members
                    .iter()
                    .filter(|&(_, &(root, _))| root == sid)
                    .map(|(_, &(_, pointer))| pointer)
                    .collect();
                self.members.retain(|_, &mut (root, _)| root != sid);
                for pointer in held {
                    self.release_com(objects, surfaces, pointer);
                }
                for (_, pointer) in self.attachments.remove(&sid).unwrap_or_default() {
                    self.release_com(objects, surfaces, pointer);
                }
                if let Some(clipper) = self.surface_clippers.remove(&sid) {
                    self.release_com(objects, surfaces, clipper);
                }
                self.color_keys.retain(|&(owner, _), _| owner != sid);
                self.described.remove(&sid);
                if let Some(creator) = self.creators.remove(&sid) {
                    self.release_com(objects, surfaces, creator);
                }
                if surfaces.primary_id() != Some(sid) && !self.members.contains_key(&sid) {
                    surfaces.delete(sid);
                }
            }
        }
        released.references
    }
}

/// The display mode as the primary's descriptor, for GetDisplayMode.
fn desc<P: Presenter>(surfaces: &Surfaces<P>, id: u32) -> [u8; 124] {
    let s = &surfaces[id];
    let mut d = [0u8; 124];
    let (r, g, b) = s.format.masks();
    for (offset, v) in [
        (0, 124),
        (4, 0x100f),
        (8, s.height),
        (12, s.width),
        (16, s.pitch),
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

/// A destination rectangle that may extend past the surface, shifted by a
/// window clipper's `origin`; the whole surface when `p` is null.
fn unbounded<P: Presenter>(
    surfaces: &Surfaces<P>,
    id: u32,
    p: u32,
    (ox, oy): (i32, i32),
    m: &dyn SurfaceMemory,
) -> Result<Option<BlitRect>, CallError> {
    if p == 0 {
        let r = surfaces[id].rect();
        return Ok(Some(BlitRect {
            x: 0,
            y: 0,
            w: r.w,
            h: r.h,
        }));
    }
    let (left, top, right, bottom) = (
        i64::from(m.arg_u32(p)? as i32) - i64::from(ox),
        i64::from(m.arg_u32(p + 4)? as i32) - i64::from(oy),
        i64::from(m.arg_u32(p + 8)? as i32) - i64::from(ox),
        i64::from(m.arg_u32(p + 12)? as i32) - i64::from(oy),
    );
    Ok((right > left && bottom > top).then(|| BlitRect {
        x: left,
        y: top,
        w: (right - left) as u32,
        h: (bottom - top) as u32,
    }))
}

/// The part of `dr` inside `clip`.
fn written(dr: BlitRect, clip: Rect) -> Rect {
    let left = dr.x.max(i64::from(clip.x));
    let top = dr.y.max(i64::from(clip.y));
    let right = (dr.x + i64::from(dr.w)).min(i64::from(clip.x + clip.w));
    let bottom = (dr.y + i64::from(dr.h)).min(i64::from(clip.y + clip.h));
    if right <= left || bottom <= top {
        return Rect::default();
    }
    Rect {
        x: left as u32,
        y: top as u32,
        w: (right - left) as u32,
        h: (bottom - top) as u32,
    }
}

fn clipped_source(sr: Rect, dr: BlitRect, part: Rect, mirror: [bool; 2]) -> Rect {
    if part.w == dr.w && part.h == dr.h {
        return sr;
    }
    // Windows rounds the clipped edges into source space before stretching
    // the remaining rectangle. A subpixel source span still samples one pixel.
    let axis = |origin, size, dest: i64, span, clipped, extent, mirror| {
        let edge =
            |offset| ((offset * u64::from(size) + u64::from(span / 2)) / u64::from(span)) as u32;
        let offset = (i64::from(clipped) - dest) as u64;
        let start = edge(offset).min(size - 1);
        let end = edge(offset + u64::from(extent)).max(start + 1);
        let at = if mirror { size - end } else { start };
        (origin + at, end - start)
    };
    let (x, w) = axis(sr.x, sr.w, dr.x, dr.w, part.x, part.w, mirror[0]);
    let (y, h) = axis(sr.y, sr.h, dr.y, dr.h, part.y, part.h, mirror[1]);
    Rect { x, y, w, h }
}

fn blt_rect_error(error: CallError) -> u32 {
    match error {
        CallError::Argument(_) => INVALID,
        CallError::Fail(_) => INVALID_RECT,
    }
}

fn rect<P: Presenter>(
    surfaces: &Surfaces<P>,
    id: u32,
    p: u32,
    m: &dyn SurfaceMemory,
) -> Result<Rect, CallError> {
    let s = &surfaces[id];
    if p == 0 {
        return Ok(s.rect());
    }
    let (x, y, r, b) = (
        m.arg_u32(p)? as i32,
        m.arg_u32(p + 4)? as i32,
        m.arg_u32(p + 8)? as i32,
        m.arg_u32(p + 12)? as i32,
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
        objects: &mut adv_machine::Objects,
        m: &mut dyn SurfaceMemory,
    ) -> Result<u32, CallError> {
        #[cfg(feature = "gpu")]
        if let Some(d3d) = &mut self.d3d {
            d3d.flush()?;
        }
        if id == api::UNIMPLEMENTED_COM {
            return Err("unimplemented DirectDraw COM method".into());
        }
        if id == api::DD_CREATE_CLIPPER {
            if a[1] == 0 {
                return Ok(INVALID);
            }
            m.arg_put_u32(a[1], 0)?;
            if a[0] != 0 || a[2] != 0 {
                return Ok(INVALID);
            }
            let Some(clipper) = self.com_version(objects, surfaces, Kind::Clipper, 1, m)? else {
                return Ok(0x8007000e);
            };
            self.clipper_windows.insert(clipper, 0);
            m.arg_put_u32(a[1], clipper)?;
            return Ok(0);
        }
        if id == api::DD_CREATE || id == api::DD_CREATE_OLD {
            m.arg_put_u32(a[1], 0)?;
            if a[0] != 0
                || (id == api::DD_CREATE
                    && (a[3] != 0 || m.arg_read(a[2], 16)?.as_ref() != IID_DDRAW7))
                || (id == api::DD_CREATE_OLD && a[2] != 0)
            {
                return Ok(INVALID);
            }
            let Some(p) = self.com_version(
                objects,
                surfaces,
                Kind::Device,
                if id == api::DD_CREATE_OLD { 1 } else { 7 },
                m,
            )?
            else {
                return Ok(0x8007000e);
            };
            m.arg_put_u32(a[1], p)?;
            return Ok(0);
        }
        let Some(com) = self.com else {
            return Ok(INVALID);
        };
        let Some(object) = com.get::<ComState>(objects, a[0]) else {
            return Ok(INVALID);
        };
        let kind = object.kind;
        let version = object.version(a[0]);
        // Versions before DirectDraw4 exchange DDSURFACEDESC.
        let legacy = version < 4;
        if matches!(kind, Kind::Device)
            && !object.initialized
            && !matches!(
                id,
                api::QUERY | api::ADDREF | api::RELEASE | api::INITIALIZE
            )
        {
            return Ok(0x800401f0);
        }
        Ok(match id {
            api::INITIALIZE => {
                if !matches!(kind, Kind::Device) {
                    INVALID
                } else if object.initialized {
                    0x88760005
                } else if a[1] != 0 {
                    UNSUPPORTED
                } else {
                    com.get_mut::<ComState>(objects, a[0]).unwrap().initialized = true;
                    0
                }
            }
            api::QUERY => {
                m.arg_put_u32(a[2], 0)?;
                let iid: [u8; 16] = m.arg_read(a[1], 16)?.as_ref().try_into().unwrap();
                #[cfg(feature = "gpu")]
                if self.d3d.is_some() {
                    return match d3d::query(self, objects, a[0], iid) {
                        Ok(pointer) => {
                            m.arg_put_u32(a[2], pointer)?;
                            Ok(0)
                        }
                        Err(hr) => Ok(hr),
                    };
                }
                match com.query::<ComState>(objects, a[0], iid) {
                    Ok(pointer) => {
                        m.arg_put_u32(a[2], pointer)?;
                        0
                    }
                    Err(QueryError::NoInterface) => 0x80004002,
                    Err(QueryError::ReferenceLimit) => 0x8007000e,
                    Err(QueryError::InvalidObject) => INVALID,
                }
            }
            api::ADDREF => com.add_ref::<ComState>(objects, a[0]).unwrap_or(0),
            api::RELEASE => self.release_com(objects, surfaces, a[0]),
            api::COOPERATIVE => {
                if !matches!(kind, Kind::Device) {
                    return Ok(INVALID);
                }
                if a[2] == 0x80 {
                    // DDSCL_SETFOCUSWINDOW selects the shared device's HWND;
                    // it does not switch display modes or activate a host UI.
                    if a[1] == 0 {
                        return Ok(INVALID);
                    }
                    com.get_mut::<ComState>(objects, a[0]).unwrap().focus_window = a[1];
                    0
                } else if a[2] & 0x11 == 0x11
                    // DDSCL_NORMAL, alone or with DDSCL_FULLSCREEN, ALLOWREBOOT,
                    // NOWINDOWCHANGES, MULTITHREADED, FPUSETUP or FPUPRESERVE.
                    || (a[2] & 8 != 0
                        && a[2] & !(8 | 1 | 2 | 4 | 0x400 | 0x800 | 0x1000) == 0)
                {
                    let exclusive = a[2] & 0x11 == 0x11;
                    let device_state = com.get_mut::<ComState>(objects, a[0]).unwrap();
                    device_state.focus_window = a[1];
                    device_state.windowed = !exclusive;
                    device_state.cooperative = true;
                    let device_id = device_state.device_id;
                    let device = device_state.modern;
                    let leaving = self.exclusive_owner == Some(device_id) && !exclusive;
                    if exclusive {
                        device_state.was_exclusive = true;
                        self.exclusive_owner = Some(device_id);
                        surfaces.set_exclusive_display_mode(true);
                    } else {
                        device_state.was_exclusive = false;
                        if self.exclusive_owner == Some(device_id) {
                            self.exclusive_owner = None;
                            surfaces.set_exclusive_display_mode(false);
                        }
                    }
                    if leaving && let Some(display) = &mut self.display {
                        display.requested = display.desktop;
                        display.suspended = false;
                        front::modes::set_current(surfaces, display.desktop);
                        self.lose_video_surfaces(Loss::Mode);
                    }
                    // Exclusive mode loses the surfaces of every other
                    // DirectDraw object, so a primary another object made, or
                    // one left behind by a released object, stops being the
                    // display.
                    if exclusive
                        && self.primary_owner != Some(device)
                        && let Some(sid) = surfaces.detach_primary()
                    {
                        if self.surface_objects.contains_key(&sid)
                            || self.members.contains_key(&sid)
                        {
                            self.lost.insert(sid, Loss::Mode);
                        } else {
                            surfaces.delete(sid);
                        }
                    }
                    0
                } else {
                    UNSUPPORTED
                }
            }
            api::CREATE_CLIPPER => {
                m.arg_put_u32(a[2], 0)?;
                if !matches!(kind, Kind::Device) || a[1] != 0 || a[3] != 0 {
                    return Ok(INVALID);
                }
                let Some(clipper) = self.com_version(objects, surfaces, Kind::Clipper, 1, m)?
                else {
                    return Ok(0x8007000e);
                };
                self.clipper_windows.insert(clipper, 0);
                m.arg_put_u32(a[2], clipper)?;
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
                if a[2] == 0 {
                    self.clipper_origins.remove(&a[0]);
                } else {
                    self.clip_lists.remove(&a[0]);
                }
                0
            }
            api::CLIPPER_SET_CLIP_LIST => {
                if !matches!(kind, Kind::Clipper) || a[2] != 0 {
                    return Ok(INVALID);
                }
                if self
                    .clipper_windows
                    .get(&a[0])
                    .is_some_and(|&hwnd| hwnd != 0)
                {
                    return Ok(CLIPPER_IS_USING_HWND);
                }
                if a[1] == 0 {
                    self.clip_lists.remove(&a[0]);
                    return Ok(0);
                }
                // RGNDATAHEADER: dwSize 32, iType RDH_RECTANGLES, nCount,
                // nRgnSize, rcBound; the rectangles follow it.
                if m.arg_u32(a[1])? != 32 || m.arg_u32(a[1] + 4)? != 1 {
                    return Ok(INVALID_CLIP_LIST);
                }
                let count = m.arg_u32(a[1] + 8)?;
                let bytes = m.arg_read(a[1] + 32, count as usize * 16)?;
                let rects = bytes
                    .as_chunks::<16>()
                    .0
                    .iter()
                    .filter_map(|r| {
                        let v =
                            |i: usize| i32::from_le_bytes(r[i..i + 4].try_into().unwrap()).max(0);
                        let (left, top, right, bottom) = (v(0), v(4), v(8), v(12));
                        (right > left && bottom > top).then(|| Rect {
                            x: left as u32,
                            y: top as u32,
                            w: (right - left) as u32,
                            h: (bottom - top) as u32,
                        })
                    })
                    .collect();
                self.clip_lists.insert(a[0], rects);
                0
            }
            api::CLIPPER_GET_HWND => {
                if !matches!(kind, Kind::Clipper) {
                    return Ok(INVALID);
                }
                m.arg_put_u32(a[1], *self.clipper_windows.get(&a[0]).unwrap_or(&0))?;
                0
            }
            api::SET_COLOR_KEY => {
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                #[cfg(feature = "gpu")]
                if self.d3d.is_some() && self.caps(sid)[1] & creation::MIPMAP_SUBLEVEL != 0 {
                    return Ok(0x8876025b); // DDERR_NOTONMIPMAPSUBLEVEL
                }
                let kinds = a[1] & !1;
                if !KEY_KINDS.contains(&kinds) {
                    return Ok(INVALID);
                }
                if a[2] == 0 {
                    if kinds == 4 {
                        return Ok(0x88760104); // DDERR_NOOVERLAYHW
                    }
                    self.color_keys.remove(&(sid, kinds));
                } else {
                    if kinds != 8 || a[1] & 1 != 0 {
                        return Ok(NO_COLOR_KEY_HW);
                    }
                    let low = m.arg_u32(a[2])?;
                    self.color_keys.insert((sid, kinds), (low, low));
                }
                0
            }
            api::GET_COLOR_KEY => {
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                if !KEY_KINDS.contains(&a[1]) || a[1] == 0x10 || a[2] == 0 {
                    return Ok(INVALID);
                }
                let Some(&(low, high)) = self.color_keys.get(&(sid, a[1])) else {
                    return Ok(NO_COLOR_KEY);
                };
                m.arg_put_u32(a[2], low)?;
                m.arg_put_u32(a[2] + 4, high)?;
                0
            }
            // Blits and flips complete synchronously, so DDGBS/DDGFS_CANBLT
            // (CANFLIP) and ISBLTDONE (ISFLIPDONE) always succeed.
            api::GET_BLT_STATUS | api::GET_FLIP_STATUS => {
                if matches!(kind, Kind::Surface(_)) && matches!(a[1], 1 | 2) {
                    0
                } else {
                    INVALID
                }
            }
            api::GET_CLIPPER => {
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                if a[1] == 0 {
                    return Ok(INVALID);
                }
                let Some(&clipper) = self.surface_clippers.get(&sid) else {
                    return Ok(NO_CLIPPER_ATTACHED);
                };
                com.add_ref::<ComState>(objects, clipper);
                m.arg_put_u32(a[1], clipper)?;
                0
            }
            api::SET_CLIPPER => {
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                if a[1] != 0
                    && !com
                        .get::<ComState>(objects, a[1])
                        .is_some_and(|o| matches!(o.kind, Kind::Clipper))
                {
                    return Ok(INVALID);
                }
                if a[1] != 0 && com.add_ref::<ComState>(objects, a[1]).is_none() {
                    return Ok(0x8007000e);
                }
                if let Some(old) = self.surface_clippers.remove(&sid) {
                    self.release_com(objects, surfaces, old);
                }
                if a[1] != 0 {
                    self.surface_clippers.insert(sid, a[1]);
                }
                0
            }
            api::DISPLAY_MODE | api::DISPLAY_MODE_OLD => {
                if !matches!(kind, Kind::Device)
                    || surface::size(a[1], a[2]).is_err()
                    || !matches!(a[3], 16 | 32)
                        && (self.display.is_none() || !matches!(a[3], 8 | 24))
                {
                    return Ok(INVALID);
                }
                if let Some(display) = &mut self.display {
                    if self.exclusive_owner != Some(object.device_id) || display.suspended {
                        return Ok(NO_EXCLUSIVE_MODE);
                    }
                    if !front::modes::sizes((display.desktop.0, display.desktop.1))
                        .contains(&(a[1], a[2]))
                    {
                        return Ok(UNSUPPORTED);
                    }
                    let mode = (
                        a[1],
                        a[2],
                        match a[3] {
                            8 => PixelFormat::Indexed8,
                            16 => PixelFormat::Rgb565,
                            _ => PixelFormat::Bgra32,
                        },
                    );
                    if mode != display.requested {
                        surfaces.flush(m, surface::FlushReason::Immediate)?;
                        display.requested = mode;
                        front::modes::set_current(surfaces, mode);
                        com.get_mut::<ComState>(objects, a[0])
                            .unwrap()
                            .changed_exclusive_mode = true;
                        self.lose_video_surfaces(Loss::Mode);
                    }
                    surfaces.set_exclusive_display_mode(true);
                    return Ok(0);
                }
                if surfaces.primary_id().is_some() {
                    return Ok(BUSY);
                }
                surfaces.flush(m, surface::FlushReason::Immediate)?;
                surfaces.width = a[1];
                surfaces.height = a[2];
                surfaces.display_format = if a[3] == 16 {
                    PixelFormat::Rgb565
                } else {
                    PixelFormat::Bgra32
                };
                surfaces.set_exclusive_display_mode(true);
                0
            }
            api::RESTORE_MODE => {
                if !matches!(kind, Kind::Device) {
                    return Ok(INVALID);
                }
                if self.exclusive_owner.is_none() || self.exclusive_owner == Some(object.device_id)
                {
                    surfaces.set_exclusive_display_mode(false);
                }
                if self.exclusive_owner == Some(object.device_id)
                    && let Some(display) = &mut self.display
                    && display.requested != display.desktop
                {
                    display.requested = display.desktop;
                    front::modes::set_current(surfaces, display.desktop);
                    self.lose_video_surfaces(Loss::Mode);
                }
                0
            }
            api::GET_DISPLAY_MODE => {
                if !matches!(kind, Kind::Device) {
                    return Ok(INVALID);
                }
                let len = m.arg_u32(a[1])?;
                if !matches!(len, 108 | 124) {
                    return Ok(INVALID);
                }
                if self.display.is_some() {
                    let (width, height, format) = front::modes::current(surfaces);
                    let display = creation::Described {
                        flags: creation::DDSD_WIDTH
                            | creation::DDSD_HEIGHT
                            | creation::DDSD_PITCH
                            | creation::DDSD_PIXELFORMAT,
                        width,
                        height,
                        format: creation::pixel_format(format),
                        ..Default::default()
                    };
                    m.arg_write(
                        a[1],
                        &display.bytes(len, (width * format.bytes() + 3) & !3, 0),
                    )?;
                } else {
                    let sid = surfaces.primary(m)?;
                    let mut desc = desc(surfaces, sid);
                    desc[..4].copy_from_slice(&len.to_le_bytes());
                    m.arg_write(a[1], &desc[..len as usize])?;
                }
                0
            }
            api::GET_CAPS => {
                if !matches!(kind, Kind::Device) {
                    return Ok(INVALID);
                }
                for &p in &a[1..3] {
                    if p != 0 {
                        let len = m.arg_u32(p)?;
                        if !(4..=380).contains(&len) {
                            return Ok(INVALID);
                        }
                        let mut caps = vec![0; len as usize];
                        caps[..4].copy_from_slice(&len.to_le_bytes());
                        // dwCaps2 and ddsCaps report the surfaces CreateSurface
                        // makes, local and nonlocal (AGP) video memory alike.
                        for (offset, value) in [
                            (4usize, 0x00420040u32), // DDCAPS_BLT | DDCAPS_READSCANLINE | DDCAPS_GDI
                            (8, 0x200),
                            (12, 0x200),            // dwCKeyCaps: DDCKEYCAPS_SRCBLT
                            (60, 64 * 1024 * 1024), // dwVidMemTotal
                            (64, 32 * 1024 * 1024), // dwVidMemFree
                            (0x16c, SURFACE_CAPS | 0x200 | 0x10 | 8),
                        ] {
                            if offset + 4 <= caps.len() {
                                caps[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
                            }
                        }
                        m.arg_write(p, &caps)?;
                    }
                }
                0
            }
            api::CREATE_SURFACE => {
                use creation::*;
                if !matches!(kind, Kind::Device) {
                    return Ok(INVALID);
                }
                if !object.cooperative {
                    return Ok(NO_COOPERATIVE_LEVEL);
                }
                let len = if legacy { 108 } else { 124 };
                if a[1] == 0 || m.arg_u32(a[1])? != len {
                    return Ok(INVALID);
                }
                m.arg_put_u32(a[2], 0)?;
                let mut d = Described::read(m, a[1], len)?;
                if d.caps[0] & (FRONTBUFFER | BACKBUFFER) != 0 {
                    return Ok(INVALID_CAPS);
                }
                let exclusive = self.exclusive_owner == Some(object.device_id);
                let mode = (surfaces.width, surfaces.height, surfaces.display_format);
                let texture_storage = storage;
                #[cfg(feature = "gpu")]
                let texture_storage = if self.d3d.is_some() {
                    d3d_storage
                } else {
                    texture_storage
                };
                if let Err(hr) = validate(&mut d, a[3], version, exclusive, mode, texture_storage) {
                    return Ok(hr);
                }
                let primary = d.caps[0] & PRIMARY != 0;
                let flip = d.caps[0] & FLIP != 0;
                // Cube maps belong to Direct3D textures, which are not built yet.
                if d.caps[1] & CUBEMAP != 0 {
                    return Ok(UNSUPPORTED);
                }
                let owner_device_id = object.device_id;
                let owner_modern = object.modern;
                let surface_version = surface_version(version);
                let format = (if d.caps[0] & (TEXTURE | THREE_D_DEVICE) != 0 {
                    texture_storage
                } else {
                    storage
                })(&d.format)
                .expect("validated format");
                let sid = if primary {
                    if self.display.is_some()
                        && let Some(previous) = surfaces.primary_id()
                        && !self.surface_objects.contains_key(&previous)
                        && (
                            surfaces[previous].width,
                            surfaces[previous].height,
                            surfaces[previous].format,
                        ) != mode
                    {
                        surfaces.detach_primary();
                        surfaces.delete(previous);
                    }
                    let primary = surfaces.primary(m)?;
                    if flip && self.complex.contains_key(&primary) {
                        return Ok(BUSY);
                    }
                    self.primary_owner = Some(owner_modern);
                    primary
                } else if d.flags & DDSD_LPSURFACE != 0 {
                    let memory = adv_device::surface::Surface {
                        pixels: d.memory,
                        width: d.width,
                        height: d.height,
                        pitch: d.pitch,
                        bottom_up: false,
                        locked: None,
                        version: 0,
                        format,
                        palette: None,
                    };
                    match surfaces.import_guest(memory, m) {
                        Ok(sid) => sid,
                        Err(_) => return Ok(INVALID),
                    }
                } else {
                    surfaces.create(d.width, d.height, false, format, m)?
                };
                // Like Windows, the kept descriptor has no client address.
                d.flags &= !DDSD_LPSURFACE;
                #[cfg(feature = "gpu")]
                if self.d3d.is_some() {
                    surfaces.set_packed_color(sid, d.packed_render_masks());
                }
                self.described.insert(sid, d);
                let result =
                    self.com_version(objects, surfaces, Kind::Surface(sid), surface_version, m);
                let Ok(Some(p)) = result else {
                    self.described.remove(&sid);
                    if surfaces.primary_id() != Some(sid) {
                        surfaces.delete(sid);
                    }
                    result?;
                    return Ok(0x8007000e);
                };
                for (i, &(flag, key)) in KEYS.iter().enumerate() {
                    if d.flags & flag != 0 {
                        self.color_keys.insert((sid, key), d.keys[i]);
                    }
                }
                self.surface_owners.insert(sid, owner_device_id);
                if self.display.is_some() {
                    self.creation_modes.insert(sid, mode);
                }
                // Surfaces from DirectDraw4 and 7 keep their creator alive.
                if version >= 4 {
                    if com.add_ref::<ComState>(objects, a[0]).is_none() {
                        self.release_com(objects, surfaces, p);
                        return Ok(0x8007000e);
                    }
                    self.creators.insert(sid, a[0]);
                }
                let built = self.build_chain(
                    objects,
                    surfaces,
                    m,
                    sid,
                    d,
                    surface_version,
                    owner_device_id,
                );
                if !matches!(built, Ok(true)) {
                    self.release_com(objects, surfaces, p);
                    built?;
                    return Ok(0x8007000e);
                }
                if primary && flip {
                    self.gdi.insert(sid, sid);
                }
                m.arg_put_u32(a[2], p)?;
                0
            }
            api::LOCK => {
                surfaces.flush(m, surface::FlushReason::Immediate)?;
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                if self.display.is_some()
                    && self.lost.contains_key(&sid)
                    && self.caps(sid)[0]
                        & (creation::PRIMARY | creation::BACKBUFFER | creation::ZBUFFER)
                        == 0
                {
                    return Ok(SURFACE_LOST);
                }
                let len = m.arg_u32(a[2])?;
                if !matches!(len, 108 | 124) || a[4] != 0 {
                    return Ok(INVALID);
                }
                if a[3] & !(0x31 | 0x800 | 0x2000 | 0x4000) != 0 {
                    return Ok(UNSUPPORTED);
                }
                if surfaces[sid].locked.is_some() {
                    return Ok(BUSY);
                }
                let Ok(rect) = rect(surfaces, sid, a[1], m) else {
                    return Ok(INVALID);
                };
                let readonly = a[3] & 0x10 != 0;
                let access = if readonly {
                    Access::Read
                } else {
                    Access::Write
                };
                let p = surfaces
                    .cpu_access(sid, rect, access, m)?
                    .row(rect.x, rect.y);
                m.arg_write(a[2], &self.describe(surfaces, sid, len, p))?;
                surfaces.get_mut(sid).unwrap().locked = Some((rect, readonly));
                surfaces.stats.locks += 1;
                0
            }
            api::UNLOCK => {
                let Some(sid) = self.surface_id(objects, a[0]) else {
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
                if !readonly && surfaces.primary_id() == Some(sid) {
                    self.present(surfaces, sid, m, locked)?;
                }
                0
            }
            api::GET_DESC => {
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                let len = m.arg_u32(a[1])?;
                if !matches!(len, 108 | 124) {
                    return Ok(INVALID);
                }
                m.arg_write(a[1], &self.describe(surfaces, sid, len, 0))?;
                0
            }
            api::COMPACT => {
                if !matches!(kind, Kind::Device) {
                    return Ok(INVALID);
                }
                if self.exclusive_owner == Some(object.device_id) {
                    0
                } else {
                    NO_EXCLUSIVE_MODE
                }
            }
            api::TEST_COOPERATIVE_LEVEL => {
                if !matches!(kind, Kind::Device) {
                    return Ok(INVALID);
                }
                if self.display.as_ref().is_some_and(|mode| mode.suspended)
                    && self.exclusive_owner == Some(object.device_id)
                {
                    NO_EXCLUSIVE_MODE
                } else if self.display.is_some() && object.changed_exclusive_mode && object.windowed
                {
                    WRONG_MODE
                } else if object.was_exclusive && self.exclusive_owner != Some(object.device_id) {
                    NO_EXCLUSIVE_MODE
                } else if self
                    .exclusive_owner
                    .is_some_and(|owner| owner != object.device_id)
                {
                    EXCLUSIVE_MODE_ALREADY_SET
                } else {
                    0
                }
            }
            api::RESTORE_ALL_SURFACES => {
                if !matches!(kind, Kind::Device) {
                    return Ok(INVALID);
                }
                if self
                    .exclusive_owner
                    .is_some_and(|owner| owner != object.device_id)
                {
                    return Ok(EXCLUSIVE_MODE_ALREADY_SET);
                }
                let owner_id = object.device_id;
                if let Some(display) = &self.display
                    && self.lost.keys().any(|sid| {
                        self.surface_owners.get(sid) == Some(&owner_id)
                            && (display.suspended
                                || self
                                    .creation_modes
                                    .get(sid)
                                    .is_some_and(|&mode| mode != front::modes::current(surfaces)))
                    })
                {
                    return Ok(WRONG_MODE);
                }
                self.lost
                    .retain(|sid, _| self.surface_owners.get(sid) != Some(&owner_id));
                0
            }
            api::GET_GDI_SURFACE => {
                if a[1] == 0 {
                    return Ok(INVALID);
                }
                m.arg_put_u32(a[1], 0)?;
                let Some(primary) = surfaces
                    .primary_id()
                    .filter(|_| matches!(kind, Kind::Device))
                    .filter(|_| self.primary_owner == Some(object.modern))
                else {
                    return Ok(NOT_FOUND);
                };
                let sid = self.gdi.get(&primary).copied().unwrap_or(primary);
                let Some(pointer) =
                    self.surface_interface(objects, surfaces, sid, surface_version(version), m)?
                else {
                    return Ok(0x8007000e);
                };
                m.arg_put_u32(a[1], pointer)?;
                0
            }
            api::GET_DD_INTERFACE => {
                if a[1] == 0 {
                    return Ok(INVALID);
                }
                m.arg_put_u32(a[1], 0)?;
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                let Some(&device_id) = self.surface_owners.get(&sid) else {
                    return Ok(NOT_FOUND);
                };
                let Some(&device) = self.device_objects.get(&device_id) else {
                    return Ok(NOT_FOUND);
                };
                let Some(canonical) = com.canonical::<ComState>(objects, device) else {
                    return Ok(NOT_FOUND);
                };
                let iid = match version {
                    7 => IID_DDRAW7,
                    4 => IID_DDRAW4,
                    _ => IID_DDRAW,
                };
                match com.query::<ComState>(objects, canonical, iid) {
                    Ok(pointer) => {
                        m.arg_put_u32(a[1], pointer)?;
                        0
                    }
                    Err(QueryError::ReferenceLimit) => 0x8007000e,
                    Err(_) => NOT_FOUND,
                }
            }
            api::PAGE_LOCK => {
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                if a[1] != 0 {
                    return Ok(INVALID);
                }
                let count = self.page_locks.entry(sid).or_insert(0);
                let Some(next) = count.checked_add(1) else {
                    return Ok(0x8007000e);
                };
                *count = next;
                0
            }
            api::PAGE_UNLOCK => {
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                if a[1] != 0 {
                    return Ok(INVALID);
                }
                match self.page_locks.get_mut(&sid) {
                    Some(count) if *count > 0 => {
                        *count -= 1;
                        if *count == 0 {
                            self.page_locks.remove(&sid);
                        }
                        0
                    }
                    _ => NOT_LOCKED,
                }
            }
            api::GET_UNIQUENESS => {
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                if a[1] == 0 {
                    return Ok(INVALID);
                }
                m.arg_put_u32(a[1], surfaces[sid].version as u32)?;
                0
            }
            api::CHANGE_UNIQUENESS => {
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                let s = surfaces.get_mut(sid).unwrap();
                s.version = s.version.wrapping_add(1);
                0
            }
            api::SURFACE_CAPS => {
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                if a[1] == 0 {
                    return Ok(INVALID);
                }
                let desc = self.describe(surfaces, sid, 124, 0);
                let caps = if legacy { 4 } else { 16 };
                m.arg_write(a[1], &desc[104..104 + caps])?;
                0
            }
            api::SURFACE_INITIALIZE => {
                if self.surface_id(objects, a[0]).is_none() {
                    INVALID
                } else {
                    ALREADY_INITIALIZED
                }
            }
            api::IS_LOST | api::RESTORE => match kind {
                Kind::Surface(sid) if id == api::IS_LOST && self.lost.contains_key(&sid) => {
                    SURFACE_LOST
                }
                Kind::Surface(sid) => {
                    if let Some(display) = &self.display
                        && self.lost.contains_key(&sid)
                    {
                        if display.suspended
                            || self
                                .creation_modes
                                .get(&sid)
                                .is_some_and(|&mode| mode != front::modes::current(surfaces))
                        {
                            return Ok(WRONG_MODE);
                        }
                        if self
                            .exclusive_owner
                            .is_some_and(|owner| self.surface_owners.get(&sid) != Some(&owner))
                        {
                            return Ok(NO_EXCLUSIVE_MODE);
                        }
                    }
                    if surfaces.primary_id() == Some(sid)
                        || self
                            .exclusive_owner
                            .is_none_or(|owner| self.surface_owners.get(&sid) == Some(&owner))
                    {
                        self.lost.remove(&sid);
                    }
                    0
                }
                _ => INVALID,
            },
            api::GET_PIXEL_FORMAT => {
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                m.arg_write(a[1], &self.describe(surfaces, sid, 124, 0)[72..104])?;
                0
            }
            api::GET_ATTACHED => {
                m.arg_put_u32(a[2], 0)?;
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                if self.lost.contains_key(&sid) {
                    return Ok(SURFACE_LOST);
                }
                // DirectDraw7 also matches DDSCAPS2; earlier versions pass junk there.
                let want = m.arg_u32(a[1])?;
                let want2 = if version == 7 {
                    m.arg_u32(a[1] + 4)?
                } else {
                    0
                };
                let found = self.attached(sid).into_iter().find(|&other| {
                    let caps = self.caps(other);
                    caps[0] & want == want && caps[1] & want2 == want2
                });
                let Some(found) = found else {
                    return Ok(NOT_FOUND);
                };
                let Some(p) =
                    self.com_version(objects, surfaces, Kind::Surface(found), version, m)?
                else {
                    return Ok(0x8007000e);
                };
                m.arg_put_u32(a[2], p)?;
                0
            }
            api::FLIP => {
                use creation::*;
                let Some(dst) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                let target = match a[1] {
                    0 => None,
                    p => match self.surface_id(objects, p) {
                        Some(target) => Some(target),
                        None => return Ok(INVALID),
                    },
                };
                let caps = self.caps(dst)[0];
                if a[1] == a[0] || caps & (FRONTBUFFER | OVERLAY) == 0 {
                    return Ok(NOT_FLIPPABLE);
                }
                if self.lost.contains_key(&dst) {
                    return Ok(SURFACE_LOST);
                }
                if caps & PRIMARY != 0
                    && self.exclusive_owner != self.surface_owners.get(&dst).copied()
                {
                    return Ok(NO_EXCLUSIVE_MODE);
                }
                let chain = self.flip_chain(dst);
                match target {
                    Some(target) if !chain[1..].contains(&target) => NOT_FLIPPABLE,
                    Some(target) => self.rotate(surfaces, m, &[dst, target])?,
                    None if chain.len() < 2 => NOT_FLIPPABLE,
                    None => self.rotate(surfaces, m, &chain)?,
                }
            }
            api::ADD_ATTACHED => {
                use creation::*;
                let (Some(sid), Some(child)) = (
                    self.surface_id(objects, a[0]),
                    self.surface_id(objects, a[1]),
                ) else {
                    return Ok(INVALID);
                };
                let (parent_caps, child_caps) = (self.caps(sid)[0], self.caps(child)[0]);
                let z = child_caps & ZBUFFER != 0;
                let plain = PRIMARY | OFFSCREEN_PLAIN;
                // DirectDraw7 attaches only Z buffers; earlier versions also
                // attach plain surfaces to each other, all of the same size.
                let allowed = if version == 7 {
                    z
                } else {
                    (parent_caps & plain != 0 && child_caps & plain != 0)
                        || (parent_caps & (PRIMARY | THREE_D_DEVICE) != 0 && z)
                };
                let same_size = (surfaces[sid].width, surfaces[sid].height)
                    == (surfaces[child].width, surfaces[child].height);
                if sid == child || !allowed || !same_size {
                    return Ok(CANNOT_ATTACH);
                }
                if let Some(&(_, through)) = self
                    .attachments
                    .get(&sid)
                    .into_iter()
                    .flatten()
                    .find(|&&(other, _)| other == child)
                {
                    return Ok(if through == a[1] {
                        ALREADY_ATTACHED
                    } else {
                        CANNOT_ATTACH
                    });
                }
                if com.add_ref::<ComState>(objects, a[1]).is_none() {
                    return Ok(0x8007000e);
                }
                self.attachments.entry(sid).or_default().push((child, a[1]));
                0
            }
            api::DELETE_ATTACHED => {
                use creation::*;
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                let child = (a[2] != 0)
                    .then(|| self.surface_id(objects, a[2]))
                    .flatten();
                let list = self.attachments.entry(sid).or_default();
                let Some(i) = list.iter().position(|&(other, _)| Some(other) == child) else {
                    return Ok(CANNOT_DETACH);
                };
                if list[i].1 != a[2] {
                    return Ok(NOT_ATTACHED);
                }
                let (_, through) = list.remove(i);
                if list.is_empty() {
                    self.attachments.remove(&sid);
                }
                self.release_com(objects, surfaces, through);
                0
            }
            api::SET_SURFACE_DESC => {
                use creation::*;
                let Some(sid) = self.surface_id(objects, a[0]) else {
                    return Ok(INVALID);
                };
                if a[1] == 0 || a[2] != 0 {
                    return Ok(INVALID);
                }
                let caps = self.caps(sid);
                if caps[0] & SYSTEM_MEMORY == 0 || caps[0] & PRIMARY != 0 || caps[1] & MANAGED != 0
                {
                    return Ok(INVALID_SURFACE_TYPE);
                }
                let r = Described::read(m, a[1], if legacy { 108 } else { 124 })?;
                let allowed = DDSD_LPSURFACE
                    | DDSD_PIXELFORMAT
                    | DDSD_WIDTH
                    | DDSD_HEIGHT
                    | DDSD_PITCH
                    | DDSD_CAPS;
                if r.flags & !allowed != 0 || r.flags & DDSD_LPSURFACE == 0 || r.memory == 0 {
                    return Ok(INVALID);
                }
                if r.flags & DDSD_CAPS != 0 && r.caps[0] != 0 {
                    return Ok(INVALID_CAPS);
                }
                let s = &surfaces[sid];
                let (mut width, mut height, mut pitch, mut format) =
                    (s.width, s.height, s.pitch, s.format);
                if r.flags & DDSD_WIDTH != 0 {
                    if r.flags & DDSD_PITCH == 0
                        || r.width == 0
                        || r.pitch as i32 <= 0
                        || r.pitch & 3 != 0
                    {
                        return Ok(INVALID);
                    }
                    (width, pitch) = (r.width, r.pitch);
                } else if r.flags & DDSD_PITCH != 0 {
                    return Ok(INVALID);
                }
                if r.flags & DDSD_HEIGHT != 0 {
                    if r.height == 0 {
                        return Ok(INVALID);
                    }
                    height = r.height;
                }
                if r.flags & DDSD_PIXELFORMAT != 0 {
                    match storage(&r.format) {
                        Some(new) => format = new,
                        None => return Ok(INVALID),
                    }
                }
                surfaces.flush(m, surface::FlushReason::Immediate)?;
                if surfaces
                    .rebind(sid, r.memory, (width, height, pitch), format, m)
                    .is_err()
                {
                    return Ok(INVALID);
                }
                if let Some(d) = self.described.get_mut(&sid) {
                    (d.width, d.height) = (width, height);
                    if r.flags & DDSD_PIXELFORMAT != 0 {
                        d.format = r.format;
                    }
                }
                0
            }
            api::FLIP_TO_GDI => {
                // The GDI surface is the front buffer the primary was created
                // with; only the DirectDraw object that created it has one.
                let Some(primary) = surfaces
                    .primary_id()
                    .filter(|_| matches!(kind, Kind::Device))
                    .filter(|_| self.primary_owner == Some(object.modern))
                else {
                    return Ok(NOT_FOUND);
                };
                match self.gdi.get(&primary) {
                    Some(&at) if at != primary => self.rotate(surfaces, m, &[primary, at])?,
                    _ => 0,
                }
            }
            api::BLT_FAST => {
                let (Some(dst), Some(src)) = (
                    self.surface_id(objects, a[0]),
                    self.surface_id(objects, a[3]),
                ) else {
                    return Ok(INVALID);
                };
                if self.display.is_some()
                    && (self.lost.contains_key(&dst) || self.lost.contains_key(&src))
                {
                    return Ok(SURFACE_LOST);
                }
                // DDBLTFAST_SRCCOLORKEY, WAIT and DONOTWAIT.
                if a[5] & !0x31 != 0 {
                    return Ok(UNSUPPORTED);
                }
                let key = if a[5] & 1 != 0 {
                    let Some(&(key, _)) = self.color_keys.get(&(src, 8)) else {
                        return Ok(INVALID);
                    };
                    Some(key)
                } else {
                    None
                };
                let sr = match rect(surfaces, src, a[4], m) {
                    Ok(sr) => sr,
                    Err(error) => return Ok(blt_rect_error(error)),
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
                    return Ok(INVALID_RECT);
                }
                // Windows validates flags, keys and rectangles first.
                if self.surface_clippers.contains_key(&dst) {
                    return Ok(BLT_FAST_CANT_CLIP);
                }
                if d.locked.is_some() || surfaces[src].locked.is_some() {
                    return Ok(BUSY);
                }
                let damage = match Self::execute_blt(
                    surfaces,
                    self.blt_surface(dst, dr),
                    Blt::Copy {
                        source: self.blt_surface(src, sr),
                        mirror: [false; 2],
                        key,
                    },
                    m,
                )? {
                    Ok(damage) => damage,
                    Err(hr) => return Ok(hr),
                };
                if surfaces.primary_id() == Some(dst) {
                    self.present(surfaces, dst, m, damage)?;
                }
                0
            }
            api::BLT => {
                if a[4] & 0x2000 != 0 {
                    // DDBLT_KEYDEST
                    return Ok(INVALID);
                }
                if a[4] & 0x400 != 0 {
                    let Some(dst) = self.surface_id(objects, a[0]) else {
                        return Ok(INVALID);
                    };
                    if self.display.is_some() && self.lost.contains_key(&dst) {
                        return Ok(SURFACE_LOST);
                    }
                    // DDBLT_COLORFILL with ASYNC, WAIT or DONOTWAIT.
                    if a[5] == 0 || a[4] & !(0x400 | 0x200 | 0x0100_0000 | 0x0800_0000) != 0 {
                        return Ok(UNSUPPORTED);
                    }
                    let clips = self.clip_list(dst);
                    let dr = if let Some(clips) = &clips {
                        let Some(dr) = unbounded(surfaces, dst, a[1], (0, 0), m)? else {
                            return Ok(INVALID_RECT);
                        };
                        clips.iter().map(|&clip| written(dr, clip)).collect()
                    } else {
                        match rect(surfaces, dst, a[1], m) {
                            Ok(dr) => vec![dr],
                            Err(error) => return Ok(blt_rect_error(error)),
                        }
                    };
                    let s = &surfaces[dst];
                    if s.locked.is_some() {
                        return Ok(BUSY);
                    }
                    let (color, bounds) = (m.arg_u32(a[5] + 80)?, s.rect());
                    let mut damage = Rect::default();
                    for dr in dr {
                        let dr = dr.clipped(bounds);
                        if !dr.empty() {
                            match Self::execute_blt(
                                surfaces,
                                self.blt_surface(dst, dr),
                                Blt::Fill(color),
                                m,
                            )? {
                                Ok(part) => damage = damage.union(part),
                                Err(hr) => return Ok(hr),
                            }
                        }
                    }
                    if surfaces.primary_id() == Some(dst) {
                        self.present(surfaces, dst, m, damage)?;
                    }
                    return Ok(0);
                }
                let (Some(dst), Some(src)) = (
                    self.surface_id(objects, a[0]),
                    self.surface_id(objects, a[2]),
                ) else {
                    return Ok(INVALID);
                };
                if self.display.is_some()
                    && (self.lost.contains_key(&dst) || self.lost.contains_key(&src))
                {
                    return Ok(SURFACE_LOST);
                }
                // DDBLT_DDFX, KEYSRC, KEYSRCOVERRIDE, ASYNC, WAIT and DONOTWAIT.
                if a[4] & !(0x800 | 0x8000 | 0x10000 | 0x200 | 0x0100_0000 | 0x0800_0000) != 0 {
                    return Ok(UNSUPPORTED);
                }
                // An optional DDBLTFX is legal even when no effect flag selects
                // its fields. Native still validates its x86 size and readable
                // structure, but ignores unrequested effects and color keys.
                if a[5] != 0 {
                    let Ok(fx) = m.arg_read(a[5], 100) else {
                        return Ok(INVALID);
                    };
                    if fx[..4] != 100u32.to_le_bytes() {
                        return Ok(INVALID);
                    }
                }
                let mirror = if a[4] & 0x800 != 0 {
                    if a[5] == 0 {
                        return Ok(INVALID);
                    }
                    let effects = m.arg_u32(a[5] + 4)?;
                    if effects & !6 != 0 {
                        return Ok(UNSUPPORTED);
                    }
                    [effects & 2 != 0, effects & 4 != 0]
                } else {
                    [false; 2]
                };
                let key = if a[4] & 0x10000 != 0 {
                    if a[5] == 0 {
                        return Ok(INVALID);
                    }
                    Some(m.arg_u32(a[5] + 92)?)
                } else if a[4] & 0x8000 != 0 {
                    // Like Wine, a source-keyed blit needs a source key.
                    let Some(&(key, _)) = self.color_keys.get(&(src, 8)) else {
                        return Ok(INVALID);
                    };
                    Some(key)
                } else {
                    None
                };
                if surfaces[dst].locked.is_some() || surfaces[src].locked.is_some() {
                    return Ok(BUSY);
                }
                let sr = match rect(surfaces, src, a[3], m) {
                    Ok(sr) => sr,
                    Err(error) => return Ok(blt_rect_error(error)),
                };
                let clips = self.clip_list(dst);
                let origin = if surfaces.primary_id() == Some(dst) {
                    self.surface_clippers
                        .get(&dst)
                        .and_then(|clipper| self.clipper_origins.get(clipper))
                        .copied()
                        .or(self.primary_origin)
                } else {
                    None
                };
                let bounds = surfaces[dst].rect();
                let Some(dr) = unbounded(surfaces, dst, a[1], origin.unwrap_or_default(), m)?
                else {
                    return Ok(INVALID_RECT);
                };
                let clipped = written(dr, bounds);
                if origin.is_none() && clips.is_none() && (clipped.w != dr.w || clipped.h != dr.h) {
                    return Ok(INVALID_RECT);
                }
                let mut damage = Rect::default();
                for &clip in clips.as_deref().unwrap_or(std::slice::from_ref(&bounds)) {
                    let part = written(dr, clip.clipped(bounds));
                    if part.empty() {
                        continue;
                    }
                    match Self::execute_blt(
                        surfaces,
                        self.blt_surface(dst, part),
                        Blt::Copy {
                            source: self.blt_surface(src, clipped_source(sr, dr, part, mirror)),
                            mirror,
                            key,
                        },
                        m,
                    )? {
                        Ok(part) => damage = damage.union(part),
                        Err(hr) => return Ok(hr),
                    }
                }
                if surfaces.primary_id() == Some(dst) {
                    self.present(surfaces, dst, m, damage)?;
                }
                0
            }
            _ => return Err(format!("unknown DirectDraw API {id}").into()),
        })
    }
}
