//! Device contexts that draw into shared surfaces. GDI creates and selects them, and a
//! DirectDraw surface lends one out between `GetDC` and `ReleaseDC`, so both fronts use this
//! one table. Ids come from the surface store, which numbers surfaces and DCs together.
use std::collections::BTreeMap;

/// The window DC: always present, it draws into the primary surface.
pub const WINDOW_DC: u32 = 1;
/// The 1x1 bitmap a new memory DC selects.
pub const STOCK_BITMAP: u32 = 2;

/// DC -> the surface (bitmap) it currently selects.
#[derive(Default)]
pub struct DeviceContexts {
    selected: BTreeMap<u32, u32>,
}

impl DeviceContexts {
    /// Bind `dc` to `bitmap`, returning the previous selection.
    pub fn insert(&mut self, dc: u32, bitmap: u32) -> Option<u32> {
        self.selected.insert(dc, bitmap)
    }

    pub fn get(&self, dc: u32) -> Option<u32> {
        self.selected.get(&dc).copied()
    }

    pub fn remove(&mut self, dc: u32) -> Option<u32> {
        self.selected.remove(&dc)
    }

    pub fn contains(&self, dc: u32) -> bool {
        self.selected.contains_key(&dc)
    }

    /// Whether any DC other than `except` selects `bitmap`.
    pub fn selects(&self, bitmap: u32, except: Option<u32>) -> bool {
        self.selected
            .iter()
            .any(|(&dc, &selected)| Some(dc) != except && selected == bitmap)
    }
}
