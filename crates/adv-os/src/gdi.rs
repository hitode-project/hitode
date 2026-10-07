//! Device contexts that draw into shared surfaces. GDI creates and selects them, and a
//! DirectDraw surface lends one out between `GetDC` and `ReleaseDC`, so both fronts use this
//! one process object. Ids come from the surface store, which numbers surfaces and DCs together.
use adv_machine::Objects;
mod clip;
mod objects;
pub use clip::DcClip;
pub use objects::{DcModes, DcState, Gdi, GdiObj, LogFont, stock_object};

/// The window DC: always present, it draws into the primary surface.
pub const WINDOW_DC: u32 = 1;
/// The 1x1 bitmap a new memory DC selects.
pub const STOCK_BITMAP: u32 = 2;

/// A raster view of the same logical DCs used by the font and palette frontends.
#[derive(Default)]
pub struct DeviceContexts {
    gdi: Gdi,
}

impl DeviceContexts {
    pub fn new(gdi: Gdi) -> Self {
        Self { gdi }
    }

    pub fn create(&mut self, objects: &mut Objects, dc: u32, bitmap: u32) -> bool {
        if !self.gdi.register_external_dc(objects, dc, 0) {
            return false;
        }
        let Some(GdiObj::Dc {
            raster, selected, ..
        }) = self.gdi.get_mut(objects, dc)
        else {
            return false;
        };
        *raster = true;
        selected[3] = bitmap;
        true
    }

    pub fn select(&mut self, objects: &mut Objects, dc: u32, bitmap: u32) -> Option<u32> {
        match self.gdi.get_mut(objects, dc)? {
            GdiObj::Dc {
                raster: true,
                selected,
                ..
            } => Some(std::mem::replace(&mut selected[3], bitmap)),
            _ => None,
        }
    }

    pub fn get(&self, objects: &Objects, dc: u32) -> Option<u32> {
        match self.gdi.get(objects, dc)? {
            GdiObj::Dc {
                raster: true,
                selected,
                ..
            } => Some(selected[3]),
            _ => None,
        }
    }

    pub fn remove(&mut self, objects: &mut Objects, dc: u32) -> Option<u32> {
        let bitmap = self.get(objects, dc)?;
        self.gdi.remove(objects, dc)?;
        Some(bitmap)
    }

    pub fn contains(&self, objects: &Objects, dc: u32) -> bool {
        self.get(objects, dc).is_some()
    }

    pub fn is_window_dc(&self, objects: &Objects, dc: u32) -> bool {
        dc == WINDOW_DC || self.gdi.is_window_dc(objects, dc)
    }

    /// Publish the GDI handle for pixels owned by the common device surface store.
    pub fn register_bitmap(
        &mut self,
        objects: &mut Objects,
        bitmap: u32,
        metadata: GdiObj,
    ) -> bool {
        matches!(metadata, GdiObj::Bitmap { owned: false, .. })
            && self.gdi.add_at(objects, bitmap, metadata)
    }

    pub fn remove_bitmap(&mut self, objects: &mut Objects, bitmap: u32) {
        if matches!(
            self.gdi.get(objects, bitmap),
            Some(GdiObj::Bitmap { owned: false, .. })
        ) {
            self.gdi.remove(objects, bitmap);
        }
    }

    pub fn stretch_mode(&self, objects: &Objects, dc: u32) -> u32 {
        match self.gdi.get(objects, dc) {
            Some(GdiObj::Dc { modes, .. }) => modes.stretch,
            _ => 1,
        }
    }

    pub fn clip(&self, objects: &Objects, dc: u32) -> DcClip {
        match self.gdi.get(objects, dc) {
            Some(GdiObj::Dc { clip, .. }) => clip.clone(),
            _ => DcClip::default(),
        }
    }

    pub fn modes(&self, objects: &Objects, dc: u32) -> Option<DcModes> {
        match self.gdi.get(objects, dc) {
            Some(GdiObj::Dc { modes, .. }) => Some(*modes),
            _ => None,
        }
    }

    pub fn set_stretch_mode(&mut self, objects: &mut Objects, dc: u32, mode: u32) -> Option<u32> {
        if dc == WINDOW_DC && !self.gdi.register_external_dc(objects, dc, 0) {
            return None;
        }
        match self.gdi.get_mut(objects, dc)? {
            GdiObj::Dc { modes, .. } => Some(std::mem::replace(&mut modes.stretch, mode)),
            _ => None,
        }
    }

    /// Whether any DC other than `except` selects `bitmap`.
    pub fn selects(&self, objects: &Objects, bitmap: u32, except: Option<u32>) -> bool {
        // Each raster DC has one alias. Exclude its borrowed payload by address.
        let except = except.and_then(|dc| self.gdi.get(objects, dc));
        self.gdi.values(objects).any(|object| !except.is_some_and(|other| std::ptr::eq(other, object))
            && matches!(object, GdiObj::Dc { raster: true, selected, .. } if selected[3] == bitmap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_and_raster_views_share_one_payload_and_one_lifetime() {
        let mut objects = Objects::default();
        let logical = Gdi::default();
        let mut raster = DeviceContexts::new(logical.clone());
        assert!(raster.create(&mut objects, 17, STOCK_BITMAP));
        assert_eq!(objects.object_count(), 1);
        let Some(GdiObj::Dc { selected, .. }) = logical.get_mut(&mut objects, 17) else {
            panic!("DC")
        };
        selected[2] = 42;
        assert_eq!(raster.select(&mut objects, 17, 91), Some(STOCK_BITMAP));
        assert!(
            matches!(logical.get(&objects, 17), Some(GdiObj::Dc { selected, .. }) if selected[2] == 42 && selected[3] == 91)
        );
        assert!(raster.selects(&objects, 91, None));
        assert!(!raster.selects(&objects, 91, Some(17)));
        assert_eq!(raster.set_stretch_mode(&mut objects, 17, 3), Some(1));
        let Some(GdiObj::Dc { modes, .. }) = logical.get_mut(&mut objects, 17) else {
            panic!("DC")
        };
        assert_eq!(modes.stretch, 3);
        modes.poly_fill = 2;
        assert_eq!(raster.remove(&mut objects, 17), Some(91));
        assert!(logical.get(&objects, 17).is_none());
        assert_eq!(objects.object_count(), 0);
        assert!(raster.create(&mut objects, 17, STOCK_BITMAP));
        assert_eq!(raster.stretch_mode(&objects, 17), 1);
        assert!(
            matches!(logical.get(&objects, 17), Some(GdiObj::Dc { modes, .. }) if modes.poly_fill == 1)
        );
        raster.remove(&mut objects, 17).unwrap();
        assert!(raster.create(&mut objects, 21, 93));
        logical.remove(&mut objects, 21).unwrap();
        assert!(!raster.contains(&objects, 21));
        assert!(!raster.selects(&objects, 93, None));
    }
}
