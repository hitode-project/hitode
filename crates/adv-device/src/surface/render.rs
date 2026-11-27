//! Share surface images with a recording graphics front.
use super::{Access, Presenter, Rect, SurfaceMemory, Surfaces};
use crate::gpu::{self, Gpu, Target};

impl<P: Presenter> Surfaces<P> {
    pub fn fork_gpu(&self) -> Result<Gpu, gpu::Error> {
        self.gpu.fork_front()
    }

    /// Discard a GPU cache of guest-backed pixels after synchronizing its CPU copy.
    pub fn evict_surface_image(
        &mut self,
        id: u32,
        memory: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        let entry = &self.surfaces[&id];
        if !entry.residency.has_cpu() || entry.residency.gpu.is_none() {
            return Ok(());
        }
        self.cpu_access(id, entry.layout.rect(), Access::Read, memory)?;
        self.surfaces.get_mut(&id).unwrap().residency = super::residency::Residency::cpu();
        Ok(())
    }

    pub fn set_packed_color(&mut self, id: u32, masks: [u32; 4]) {
        self.surfaces.get_mut(&id).unwrap().packed_color = masks;
    }

    pub fn packed_color(&self, id: u32) -> [u32; 4] {
        self.surfaces[&id].packed_color
    }

    pub fn packed_clear(&self, id: u32, color: [f32; 4]) -> [f64; 4] {
        let masks = self.packed_color(id);
        let [r, g, b, a] = color.map(|v| (v * 255.).round() as u8);
        let raw = pack_color([b, g, r, if masks[3] == 0 { 0 } else { a }], masks);
        let [b, g, r, a] = unpack_color(raw, masks);
        [r, g, b, a].map(|v| f64::from(v) / 255.)
    }

    /// Obtain the color attachment and preserve the requested cells before a draw.
    pub fn render_access(
        &mut self,
        id: u32,
        rect: Rect,
        access: Access,
        memory: &dyn SurfaceMemory,
    ) -> Result<Target, String> {
        Ok(Target::from_parts(
            self.gpu_access(id, rect, access, memory)?,
            None,
        ))
    }
}

fn storage_masks(mut masks: [u32; 4]) -> [u32; 4] {
    if masks == [0x7c00, 0x3e0, 0x1f, 0] {
        masks[3] = 0x8000;
    }
    masks
}

pub(super) fn pack_color([b, g, r, a]: [u8; 4], masks: [u32; 4]) -> u16 {
    storage_masks(masks)
        .into_iter()
        .zip([r, g, b, a])
        .filter(|&(mask, _)| mask != 0)
        .fold(0, |raw, (mask, channel)| {
            let shift = mask.trailing_zeros();
            raw | ((((u32::from(channel) * (mask >> shift) + 127) / 255) << shift) as u16)
        })
}

pub(super) fn unpack_color(raw: u16, masks: [u32; 4]) -> [u8; 4] {
    let [r, g, b, a] = storage_masks(masks).map(|mask| {
        if mask == 0 {
            return 255;
        }
        let value = ((u32::from(raw) & mask) >> mask.trailing_zeros()) as u8;
        match mask.count_ones() {
            1 => value * 255,
            4 => value * 17,
            5 => value << 3 | value >> 2,
            6 => value << 2 | value >> 4,
            _ => unreachable!("packed color channel width"),
        }
    });
    [b, g, r, a]
}
