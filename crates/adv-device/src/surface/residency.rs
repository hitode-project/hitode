//! CPU stores and GPU images share the same surface identity and cell grid.
use super::{Rect, Surface};

#[cfg(feature = "gpu")]
use super::{Region, rects};
#[cfg(feature = "gpu")]
use crate::gpu::{Depth, Image, Target};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    /// Preserve pixels outside the write, including the rest of each touched cell.
    Write,
}

/// Guest storage described by the entry's layout.
struct Store;

pub(super) struct Residency {
    cpu: Option<Store>,
    #[cfg(feature = "gpu")]
    pub(super) gpu: Option<Image>,
    #[cfg(feature = "gpu")]
    cpu_stale: Region,
    #[cfg(feature = "gpu")]
    gpu_stale: Region,
}

impl Residency {
    pub(super) fn cpu() -> Self {
        Self {
            cpu: Some(Store),
            #[cfg(feature = "gpu")]
            gpu: None,
            #[cfg(feature = "gpu")]
            cpu_stale: Region::default(),
            #[cfg(feature = "gpu")]
            gpu_stale: Region::default(),
        }
    }

    pub(super) fn has_cpu(&self) -> bool {
        self.cpu.is_some()
    }

    #[cfg(feature = "gpu")]
    fn native(gpu: Image) -> Self {
        Self {
            cpu: None,
            gpu: Some(gpu),
            cpu_stale: Region::default(),
            gpu_stale: Region::default(),
        }
    }

    #[cfg(feature = "gpu")]
    fn attach_cpu(&mut self, size: (u32, u32)) {
        self.cpu = Some(Store);
        self.cpu_stale = Region::whole(size);
        self.gpu_stale = Region::empty(size);
    }

    #[cfg(feature = "gpu")]
    fn attach_gpu(&mut self, size: (u32, u32), image: Image) {
        self.gpu = Some(image);
        self.cpu_stale = Region::empty(size);
        self.gpu_stale = Region::whole(size);
    }

    #[cfg(feature = "gpu")]
    fn access<E>(
        &mut self,
        side: Side,
        rect: Rect,
        mode: Access,
        transfer: impl FnOnce(&Image, &[Rect]) -> Result<(), E>,
    ) -> Result<(), E> {
        if self.cpu.is_none() || self.gpu.is_none() {
            return Ok(());
        }
        let (stale, other) = match side {
            Side::Cpu => (&mut self.cpu_stale, &mut self.gpu_stale),
            Side::Gpu => (&mut self.gpu_stale, &mut self.cpu_stale),
        };
        if stale.intersects(rect.into()) {
            let mut needed = Region::empty(stale.size());
            needed.union_clipped(stale, rect.into());
            let parts = rects(&needed);
            transfer(self.gpu.as_ref().unwrap(), &parts)?;
            stale.remove(rect.into());
        }
        if mode == Access::Write {
            other.add(rect.into());
        }
        Ok(())
    }

    #[cfg(feature = "gpu")]
    pub(super) fn gpu_current(&self, rect: Rect) -> bool {
        self.gpu.is_some() && (!self.has_cpu() || !self.gpu_stale.intersects(rect.into()))
    }
}

#[cfg(feature = "gpu")]
#[derive(Clone, Copy)]
enum Side {
    Cpu,
    Gpu,
}

pub(super) struct Entry {
    pub(super) layout: Surface,
    pub(super) residency: Residency,
    #[cfg(feature = "gpu")]
    pub(super) packed_color: [u32; 4],
    #[cfg(feature = "gpu")]
    depth: Option<Depth>,
}

impl Entry {
    pub(super) fn software(layout: Surface) -> Self {
        Self {
            layout,
            residency: Residency::cpu(),
            #[cfg(feature = "gpu")]
            packed_color: [0; 4],
            #[cfg(feature = "gpu")]
            depth: None,
        }
    }

    #[cfg(feature = "gpu")]
    pub(super) fn native(layout: Surface, target: Target) -> Self {
        let depth = target.depth().cloned();
        Self {
            layout,
            residency: Residency::native(target.image),
            packed_color: [0; 4],
            depth,
        }
    }

    #[cfg(feature = "gpu")]
    pub(super) fn target(&self) -> Option<Target> {
        self.residency
            .gpu
            .as_ref()
            .map(|image| Target::from_parts(image.clone(), self.depth.clone()))
    }

    #[cfg(feature = "gpu")]
    pub(super) fn replace_target(&mut self, target: Target) {
        self.depth = target.depth().cloned();
        self.residency = Residency::native(target.image);
    }
}

impl std::ops::Deref for Entry {
    type Target = Surface;

    fn deref(&self) -> &Surface {
        &self.layout
    }
}

impl std::ops::DerefMut for Entry {
    fn deref_mut(&mut self) -> &mut Surface {
        &mut self.layout
    }
}

use super::{Presenter, SurfaceMemory, Surfaces};

impl<P: Presenter> Surfaces<P> {
    #[cfg(feature = "gpu")]
    pub(super) fn is_native(&self, id: u32) -> bool {
        self.surfaces
            .get(&id)
            .is_some_and(|entry| entry.residency.gpu.is_some())
    }

    /// Synchronize the touched cells before returning the CPU layout.
    /// Readback orders queued producers before the copy. A write preserves
    /// the rest of each touched cell.
    pub fn cpu_access(
        &mut self,
        id: u32,
        rect: Rect,
        access: Access,
        memory: &mut dyn SurfaceMemory,
    ) -> Result<&Surface, String> {
        let entry = self.surfaces.get_mut(&id).ok_or("unknown surface")?;
        #[cfg(feature = "gpu")]
        {
            let layout = &mut entry.layout;
            let size = (layout.width, layout.height);
            if entry.residency.cpu.is_none() {
                layout.pixels = self.arena.alloc(layout.pitch * layout.height, memory)?;
                entry.residency.attach_cpu(size);
            }
            entry
                .residency
                .access(Side::Cpu, rect, access, |image, parts| {
                    let mut ticket = self
                        .gpu
                        .readback_regions(image, parts, None)
                        .map_err(gpu_error)?;
                    ticket.take_completion_job().unwrap()();
                    let pixels = ticket.poll().unwrap().map_err(gpu_error)?;
                    let bytes = layout.format.bytes() as usize;
                    let mut rows = pixels.as_chunks::<4>().0.iter();
                    let mut row = Vec::new();
                    for part in parts {
                        for y in part.y..part.y + part.h {
                            row.clear();
                            for pixel in rows.by_ref().take(part.w as usize) {
                                if entry.packed_color[0] != 0 {
                                    let raw = super::render::pack_color(*pixel, entry.packed_color);
                                    row.extend_from_slice(&raw.to_le_bytes());
                                } else {
                                    row.extend_from_slice(&pixel[..bytes]);
                                }
                            }
                            layout.write_pixels(memory, part.x, y, &row)?;
                        }
                    }
                    Ok::<(), String>(())
                })?;
        }
        #[cfg(not(feature = "gpu"))]
        let _ = (rect, access, memory);
        Ok(&entry.layout)
    }

    /// Obtain the current GPU copy, uploading only the requested stale cells.
    /// Packed render targets use BGRA color. Other packed layouts retain their raw bits.
    #[cfg(feature = "gpu")]
    pub fn gpu_access(
        &mut self,
        id: u32,
        rect: Rect,
        access: Access,
        memory: &dyn SurfaceMemory,
    ) -> Result<Image, String> {
        let entry = self.surfaces.get_mut(&id).ok_or("unknown surface")?;
        let layout = &entry.layout;
        if entry.residency.gpu.is_none() {
            let image = self
                .gpu
                .target(layout.width, layout.height, false)
                .map_err(gpu_error)?
                .image;
            entry
                .residency
                .attach_gpu((layout.width, layout.height), image);
        }
        entry
            .residency
            .access(Side::Gpu, rect, access, |image, parts| {
                let bytes = layout.format.bytes() as usize;
                let mut pixels = Vec::new();
                for part in parts {
                    pixels.clear();
                    for y in part.y..part.y + part.h {
                        let row = layout.read_pixels(memory, part.x, y, part.w)?;
                        for pixel in row.chunks_exact(bytes) {
                            if entry.packed_color[0] != 0 {
                                let raw = u16::from_le_bytes(pixel.try_into().unwrap());
                                pixels.extend_from_slice(&super::render::unpack_color(
                                    raw,
                                    entry.packed_color,
                                ));
                            } else {
                                pixels.extend_from_slice(pixel);
                                pixels.resize(pixels.len() + 4 - bytes, 0);
                            }
                        }
                    }
                    self.gpu
                        .upload(image, *part, part.w * 4, &pixels)
                        .map_err(gpu_error)?;
                }
                Ok::<(), String>(())
            })?;
        Ok(entry.residency.gpu.as_ref().unwrap().clone())
    }
}

#[cfg(feature = "gpu")]
fn gpu_error(error: crate::gpu::Error) -> String {
    error.to_string()
}

/// A CPU snapshot of GPU-current cells. The worker owns this version while
/// the frontend waits to publish it into a temporary mapping.
#[cfg(feature = "gpu")]
pub struct CpuReadback {
    pub image: Image,
    pub regions: Vec<Rect>,
}

#[cfg(feature = "gpu")]
impl CpuReadback {
    pub fn new(image: Image, rect: Rect) -> Self {
        let regions = rects(&super::region((image.width(), image.height()), rect));
        Self { image, regions }
    }
}

#[cfg(feature = "gpu")]
impl<P: Presenter> Surfaces<P> {
    /// Plan the read phase of a temporary CPU mapping. Its rows are published
    /// by the frontend after its already-admitted producers and the copy finish.
    pub fn cpu_readback(&self, id: u32, rect: Rect) -> Option<CpuReadback> {
        let entry = self.surfaces.get(&id)?;
        if !entry.residency.gpu_current(rect) {
            return None;
        }
        let image = entry.residency.gpu.as_ref()?.clone();
        Some(CpuReadback::new(image, rect))
    }
}

#[cfg(all(test, feature = "gpu"))]
mod tests {
    use super::*;
    use crate::gpu::Gpu;

    // spec: DEV-FRAME-5 DEV-FRAME-13
    #[test]
    fn access_sequences_match_a_single_copy_and_transfer_only_stale_cells() {
        let gpu = Gpu::execution(false).unwrap();
        let size = (539, 26);
        let image = gpu.target(size.0, size.1, false).unwrap().image;
        let mut resident = Residency::cpu();
        resident.attach_gpu(size, image);
        let (width, height) = size;
        let columns = width.div_ceil(8);
        let cells = (columns * height.div_ceil(8)) as usize;
        let pixel_count = (width * height) as usize;
        let mut pixels = [vec![0; pixel_count], vec![u32::MAX; pixel_count]];
        let mut original = vec![0; pixel_count];
        let mut stale = [vec![false; cells], vec![true; cells]];
        let mut seed = 0x8431b5c2u32;
        let mut random = |n| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) % n
        };
        let mut transferred = [0; 2];
        for value in 1..=1800 {
            let side = random(2) as usize;
            let other = 1 - side;
            let mode = if random(2) == 0 {
                Access::Read
            } else {
                Access::Write
            };
            let x = random(width);
            let y = random(height);
            let rect = Rect {
                x,
                y,
                w: 1 + random((width - x).min(11)),
                h: 1 + random((height - y).min(11)),
            };
            let mut expected = stale[side].clone();
            for (cell, needed) in expected.iter_mut().enumerate() {
                let x = (cell as u32 % columns) * 8;
                let y = (cell as u32 / columns) * 8;
                *needed &= !(Rect { x, y, w: 8, h: 8 }).clipped(rect).empty();
            }
            let mut copied = vec![false; cells];
            let mut count = 0;
            resident
                .access([Side::Cpu, Side::Gpu][side], rect, mode, |_, parts| {
                    for part in parts {
                        for y in part.y..part.y + part.h {
                            for x in part.x..part.x + part.w {
                                let pixel = (y * width + x) as usize;
                                pixels[side][pixel] = pixels[other][pixel];
                            }
                        }
                        for y in part.y / 8..(part.y + part.h).div_ceil(8) {
                            for x in part.x / 8..(part.x + part.w).div_ceil(8) {
                                let cell = (y * columns + x) as usize;
                                assert!(!copied[cell]);
                                copied[cell] = true;
                                count += 1;
                            }
                        }
                    }
                    Ok::<(), ()>(())
                })
                .unwrap();
            assert_eq!(copied, expected);
            transferred[side] += count;
            for y in rect.y..rect.y + rect.h {
                for x in rect.x..rect.x + rect.w {
                    let pixel = (y * width + x) as usize;
                    let cell = ((y / 8) * columns + x / 8) as usize;
                    assert_eq!(pixels[side][pixel], original[pixel]);
                    stale[side][cell] = false;
                    if mode == Access::Write {
                        pixels[side][pixel] = value;
                        original[pixel] = value;
                        stale[other][cell] = true;
                    }
                }
            }
            for (cell, (&cpu, &gpu)) in stale[0].iter().zip(&stale[1]).enumerate() {
                let rect = Rect {
                    x: cell as u32 % columns * 8,
                    y: cell as u32 / columns * 8,
                    w: 1,
                    h: 1,
                };
                assert!(!(cpu && gpu));
                assert_eq!(resident.cpu_stale.intersects(rect.into()), cpu);
                assert_eq!(resident.gpu_stale.intersects(rect.into()), gpu);
            }
        }
        assert!(transferred.into_iter().all(|count| count > 100));
    }
}
