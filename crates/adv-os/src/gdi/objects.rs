//! Windows-visible logical GDI payloads owned by the shared process object table.
use adv_machine::{Namespace, Object, Objects};
use std::{cell::Cell, rc::Rc};
const STOCK_BASE: u32 = 0x0190_0010;
const SYSTEM_FONT: u32 = 13;

#[derive(Clone, Copy, Debug)]
pub struct DcModes {
    pub map: i32,
    pub poly_fill: i32,
    pub rop2: i32,
    pub stretch: u32,
    pub text_align: u32,
    /// Logical window origin. Initial MM_TEXT DCs start at (0, 0).
    pub window_origin: [i32; 2],
    /// Device viewport origin. MM_TEXT uses translation without scaling.
    pub viewport_origin: [i32; 2],
    /// Brush alignment in client device coordinates, independent of mapping.
    pub brush_origin: [i32; 2],
}
impl Default for DcModes {
    fn default() -> Self {
        Self {
            map: 1,
            poly_fill: 1,
            rop2: 13, // R2_COPYPEN
            stretch: 1,
            text_align: 0,
            window_origin: [0, 0],
            viewport_origin: [0, 0],
            brush_origin: [0, 0],
        }
    }
}

impl DcModes {
    pub fn to_logical(&self, x: i64, y: i64) -> [i64; 2] {
        [
            x + i64::from(self.window_origin[0]) - i64::from(self.viewport_origin[0]),
            y + i64::from(self.window_origin[1]) - i64::from(self.viewport_origin[1]),
        ]
    }
    pub fn to_device(&self, x: i64, y: i64) -> [i64; 2] {
        [
            x - i64::from(self.window_origin[0]) + i64::from(self.viewport_origin[0]),
            y - i64::from(self.window_origin[1]) + i64::from(self.viewport_origin[1]),
        ]
    }
    pub fn device_point(&self, x: i32, y: i32) -> Option<[i32; 2]> {
        let [x, y] = self.to_device(i64::from(x), i64::from(y));
        Some([i32::try_from(x).ok()?, i32::try_from(y).ok()?])
    }
}

/// What SaveDC records and RestoreDC brings back.
#[derive(Clone, Debug)]
pub struct DcState {
    pub selected: [u32; 4],
    pub palette: u32,
    pub text_color: u32,
    pub bk_color: u32,
    pub bk_mode: u32,
    pub modes: DcModes,
    pub clip: super::DcClip,
}

#[derive(Clone, Debug)]
pub struct LogFont {
    pub height: i32,
    pub width: i32,
    pub weight: i32,
    pub italic: bool,
    pub charset: u8,
    pub face: String,
    /// The raw LOGFONTA for GetObjectA.
    pub raw: Vec<u8>,
    /// LOGFONTW retains the Unicode creation field, including its fixed tail.
    pub wide: [u8; 92],
}

#[derive(Clone, Debug)]
pub enum GdiObj {
    Font(LogFont),
    Brush {
        color: u32,
    },
    Pen {
        style: u32,
        width: u32,
        color: u32,
    },
    Bitmap {
        width: i32,
        height: i32,
        bpp: u16,
        bits: u32,
        stride: u32,
        owned: bool,
        /// Private shared-memory view retained by a mapped DIB until deletion.
        mapping_view: Option<u32>,
        /// Number of RGB entries in a DIB's table; zero for DDB/truecolor.
        /// The colors themselves belong to the common device surface.
        dib_colors: u16,
    },
    Palette {
        entries: Vec<[u8; 4]>,
    },
    Dc {
        /// The DC selects a surface from the common raster store.
        raster: bool,
        hwnd: u32,
        selected: [u32; 4],
        palette: u32,
        text_color: u32,
        bk_color: u32,
        bk_mode: u32,
        modes: DcModes,
        clip: super::DcClip,
        /// The SaveDC stack, oldest first.
        saved: Vec<DcState>,
    },
    /// Non-overlapping half-open rectangles in logical units.
    Region(Vec<[i32; 4]>),
}

#[derive(Clone, Default)]
pub struct Gdi {
    // Copies share only the namespace token; payloads live exclusively in Objects.
    namespace: Rc<Cell<Option<Namespace>>>,
}

impl Gdi {
    fn namespace(&mut self, objects: &mut Objects) -> Option<Namespace> {
        if self.namespace.get().is_none() {
            self.namespace
                .set(objects.new_namespace_in(0x0400_0004, 0x04ff_fffc));
        }
        self.namespace.get()
    }
    pub fn add(&mut self, objects: &mut Objects, o: GdiObj) -> u32 {
        self.namespace(objects)
            .and_then(|ns| objects.insert_host_in(ns, o))
            .unwrap_or(0)
    }
    pub fn add_at(&mut self, objects: &mut Objects, handle: u32, object: GdiObj) -> bool {
        self.namespace(objects)
            .is_some_and(|ns| objects.insert_host_at(ns, handle, object))
    }
    pub fn get<'a>(&self, objects: &'a Objects, h: u32) -> Option<&'a GdiObj> {
        objects.owned(objects.lookup_alias(self.namespace.get()?, h)?)
    }
    pub fn get_mut<'a>(&self, objects: &'a mut Objects, h: u32) -> Option<&'a mut GdiObj> {
        objects.owned_mut(objects.lookup_alias(self.namespace.get()?, h)?)
    }

    /// Display DC identity is independent of the shared primary surface handle.
    pub fn is_window_dc(&self, objects: &Objects, dc: u32) -> bool {
        matches!(self.get(objects, dc), Some(GdiObj::Dc { raster: false, hwnd, selected, .. })
            if *hwnd != 0 || dc == super::WINDOW_DC || selected[3] != 0)
    }
    pub fn remove(&self, objects: &mut Objects, h: u32) -> Option<GdiObj> {
        objects.lookup_alias::<GdiObj>(self.namespace.get()?, h)?;
        match objects.close_alias(self.namespace.get()?, h)?? {
            Object::Host(value) => value.downcast::<GdiObj>().ok().map(|value| *value),
            _ => unreachable!("GDI namespace contains typed host objects"),
        }
    }
    pub fn font_selected(&self, objects: &Objects, font: u32) -> bool {
        self.namespace.get().is_some_and(|ns| {
            objects.hosts_in::<GdiObj>(ns).any(
                |(_, object)| matches!(object, GdiObj::Dc { selected, .. } if selected[2] == font),
            )
        })
    }

    /// Attach GDI metadata to a DC owned by another drawing provider. Repeated
    /// GetDC calls preserve its logical selections instead of resetting them.
    pub fn register_external_dc(&mut self, objects: &mut Objects, handle: u32, hwnd: u32) -> bool {
        if handle == 0 {
            return false;
        }
        if let Some(object) = self.get(objects, handle) {
            return matches!(object, GdiObj::Dc { .. });
        }
        let selected = [
            stock_object(0),
            stock_object(7),
            stock_object(SYSTEM_FONT),
            0,
        ];
        let Some(ns) = self.namespace(objects) else {
            return false;
        };
        objects.insert_host_at(
            ns,
            handle,
            GdiObj::Dc {
                raster: false,
                hwnd,
                selected,
                palette: stock_object(15),
                text_color: 0,
                bk_color: 0xff_ffff,
                bk_mode: 2,
                modes: DcModes::default(),
                clip: super::DcClip::default(),
                saved: Vec::new(),
            },
        )
    }

    pub fn values<'a>(&self, objects: &'a Objects) -> impl Iterator<Item = &'a GdiObj> {
        self.namespace
            .get()
            .into_iter()
            .flat_map(move |ns| objects.hosts_in::<GdiObj>(ns).map(|(_, value)| value))
    }

    /// Retire the logical selections when the drawing provider destroys a DC.
    pub fn remove_external_dc(&mut self, objects: &mut Objects, handle: u32) -> bool {
        if !matches!(self.get(objects, handle), Some(GdiObj::Dc { .. })) {
            return false;
        }
        self.remove(objects, handle);
        true
    }

    /// Keep a drawing provider's externally stored selection visible to GDI queries.
    pub fn set_external_bitmap(&mut self, objects: &mut Objects, handle: u32, bitmap: u32) {
        if let Some(GdiObj::Dc { selected, .. }) = self.get_mut(objects, handle) {
            selected[3] = bitmap;
        }
    }
}

pub fn stock_object(i: u32) -> u32 {
    STOCK_BASE + i
}
