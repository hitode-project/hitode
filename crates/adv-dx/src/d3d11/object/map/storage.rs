//! Guest shadow layouts and coherence for each mapped subresource.
use super::{Access, Error, Map};
use crate::d3d11::{
    abi::D3D11_MAPPED_SUBRESOURCE,
    desc::{Misc, Resource, Usage},
};
use adv_device::surface::{Access as SurfaceAccess, Rect, Residency};
use std::num::NonZeroU32;

#[derive(Clone, Copy)]
pub struct Layout {
    offset: u32,
    row_pitch: u32,
    depth_pitch: u32,
    blocks: (u32, u32),
    depth: u32,
}

impl Layout {
    pub(super) fn rows(self) -> u32 {
        self.blocks.1
    }

    pub fn row_pitch(self) -> u32 {
        self.row_pitch
    }

    pub fn depth_pitch(self) -> u32 {
        self.depth_pitch
    }

    pub fn slice(self, storage: u32, depth_slice: u32) -> u32 {
        storage + self.offset + depth_slice * self.depth_pitch
    }

    fn full(self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.blocks.0,
            h: self.blocks.1,
        }
    }
}

pub(in crate::d3d11) fn coherent(resource: Resource) -> bool {
    resource.usage() == Usage::Staging || resource.misc().contains(Misc::GDI_COMPATIBLE)
}

pub(in crate::d3d11) fn has_shadow(resource: Resource) -> bool {
    resource.usage() == Usage::Dynamic || coherent(resource)
}

pub struct Layouts {
    resource: Resource,
    layouts: Box<[Layout]>,
    size: u32,
}

impl Layouts {
    pub fn new(resource: Resource, alignment: NonZeroU32) -> Option<Self> {
        if !has_shadow(resource) {
            return None;
        }
        let mut layouts = Vec::with_capacity(resource.subresources() as usize);
        let mut offset = 0u32;
        for subresource in 0..resource.subresources() {
            let (width, height, depth, row_bytes) = match resource {
                Resource::Buffer(buffer) => (buffer.size(), 1, 1, buffer.size()),
                Resource::Texture(texture) => {
                    let mip = subresource % texture.levels();
                    let [width, height, depth] = texture.extent(mip);
                    let format = texture.format();
                    let blocks = (
                        width.div_ceil(u32::from(format.alignment[0])),
                        height.div_ceil(u32::from(format.alignment[1])),
                    );
                    (
                        blocks.0,
                        blocks.1,
                        depth,
                        blocks.0 * u32::from(format.aligned_unit_bytes),
                    )
                }
            };
            let row_pitch = match resource {
                Resource::Buffer(_) => row_bytes,
                Resource::Texture(_) => row_bytes
                    .div_ceil(alignment.get())
                    .checked_mul(alignment.get())?,
            };
            let depth_pitch = row_pitch.checked_mul(height)?;
            layouts.push(Layout {
                offset,
                row_pitch,
                depth_pitch,
                blocks: (width, height),
                depth,
            });
            offset = offset.checked_add(depth_pitch.checked_mul(depth)?)?;
        }
        Some(Self {
            resource,
            layouts: layouts.into_boxed_slice(),
            size: offset,
        })
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn attach(self, pointer: u32) -> Storage {
        let subresources = self
            .layouts
            .into_iter()
            .map(|layout| Subresource {
                layout,
                dynamic: DynamicResidency::Current,
                staging: if coherent(self.resource) {
                    (0..layout.depth).map(|_| Residency::cpu()).collect()
                } else {
                    Box::default()
                },
            })
            .collect();
        Storage {
            resource: self.resource,
            pointer,
            subresources,
        }
    }
}

struct Subresource {
    layout: Layout,
    dynamic: DynamicResidency,
    staging: Box<[Residency<u32>]>,
}

#[derive(PartialEq, Eq)]
enum DynamicResidency {
    Current,
    CpuNewer,
    GpuNewer,
}

pub struct Storage {
    resource: Resource,
    pointer: u32,
    subresources: Box<[Subresource]>,
}

impl Storage {
    pub fn pointer(&self) -> u32 {
        self.pointer
    }

    pub(in crate::d3d11) fn initialize<E>(
        &self,
        subresource: u32,
        bytes: &[u8],
        mut write: impl FnMut(u32, &[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        let layout = self.layout(subresource);
        let row_bytes = bytes.len() / (layout.rows() * layout.depth) as usize;
        for (i, row) in bytes.chunks_exact(row_bytes).enumerate() {
            let y = i as u32 % layout.rows();
            let z = i as u32 / layout.rows();
            write(layout.slice(self.pointer, z) + y * layout.row_pitch, row)?;
        }
        Ok(())
    }

    pub(in crate::d3d11) fn layout(&self, subresource: u32) -> Layout {
        self.subresources[subresource as usize].layout
    }

    pub fn map(
        &mut self,
        subresource: u32,
        mode: u32,
        flags: u32,
        busy: bool,
        download: impl FnMut(Layout, u32, &[Rect]) -> Result<(), Error>,
    ) -> Result<D3D11_MAPPED_SUBRESOURCE, Error> {
        let request = Map::admit(self.resource, subresource, mode, flags)?;
        request.check_busy(busy)?;
        let mut download = download;
        if let Map::Staging { access, .. } = request {
            let access = match access {
                Access::Read => SurfaceAccess::Read,
                Access::Write | Access::ReadWrite => SurfaceAccess::Write,
            };
            return self.cpu_access(subresource, access, &mut download);
        }
        let entry = &mut self.subresources[subresource as usize];
        if entry.dynamic == DynamicResidency::GpuNewer {
            let layout = entry.layout;
            for slice in 0..layout.depth {
                download(layout, slice, &[layout.full()])?;
            }
        }
        entry.dynamic = DynamicResidency::CpuNewer;
        Ok(self.mapped(subresource))
    }

    #[cfg(feature = "gpu")]
    pub(in crate::d3d11) fn dynamic_shadow_stale(&self, subresource: u32) -> bool {
        self.subresources[subresource as usize].dynamic == DynamicResidency::GpuNewer
    }

    #[cfg(feature = "gpu")]
    pub(in crate::d3d11) fn dynamic_cpu_newer(&self, subresource: u32) -> bool {
        self.subresources[subresource as usize].dynamic == DynamicResidency::CpuNewer
    }

    #[cfg(feature = "gpu")]
    pub(in crate::d3d11) fn dynamic_gpu_write(&mut self, subresource: u32) {
        self.subresources[subresource as usize].dynamic = DynamicResidency::GpuNewer;
    }

    #[cfg(feature = "gpu")]
    pub(in crate::d3d11) fn dynamic_uploaded(&mut self, subresource: u32) {
        self.subresources[subresource as usize].dynamic = DynamicResidency::Current;
    }

    #[cfg(feature = "gpu")]
    pub(in crate::d3d11) fn sync_dynamic<E>(
        &mut self,
        subresource: u32,
        mut upload: impl FnMut(Layout, u32, &[Rect]) -> Result<(), E>,
    ) -> Result<(), E> {
        let entry = &mut self.subresources[subresource as usize];
        if entry.dynamic == DynamicResidency::CpuNewer {
            let layout = entry.layout;
            for slice in 0..layout.depth {
                upload(layout, slice, &[layout.full()])?;
            }
            entry.dynamic = DynamicResidency::Current;
        }
        Ok(())
    }

    fn mapped(&self, subresource: u32) -> D3D11_MAPPED_SUBRESOURCE {
        let layout = self.layout(subresource);
        D3D11_MAPPED_SUBRESOURCE {
            pData: layout.slice(self.pointer, 0),
            RowPitch: layout.row_pitch,
            DepthPitch: layout.depth_pitch,
        }
    }

    pub(in crate::d3d11) fn cpu_access<E>(
        &mut self,
        index: u32,
        access: SurfaceAccess,
        mut download: impl FnMut(Layout, u32, &[Rect]) -> Result<(), E>,
    ) -> Result<D3D11_MAPPED_SUBRESOURCE, E> {
        let subresource = &mut self.subresources[index as usize];
        let layout = subresource.layout;
        for slice in &mut subresource.staging {
            slice.cpu_access(layout.full(), access, |&slice, parts| {
                download(layout, slice, parts)
            })?;
        }
        Ok(self.mapped(index))
    }

    pub fn attach_staging_gpu(&mut self, subresource: u32) {
        let subresource = &mut self.subresources[subresource as usize];
        for (index, slice) in subresource.staging.iter_mut().enumerate() {
            slice.attach_gpu(subresource.layout.blocks, index as u32);
        }
    }

    #[cfg(feature = "gpu")]
    pub(in crate::d3d11) fn staging_gpu_overwrite(&mut self, subresource: u32) {
        for slice in &mut self.subresources[subresource as usize].staging {
            slice.gpu_overwrite();
        }
    }

    pub fn staging_gpu_access(
        &mut self,
        subresource: u32,
        access: SurfaceAccess,
        mut upload: impl FnMut(Layout, u32, &[Rect]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let subresource = &mut self.subresources[subresource as usize];
        let layout = subresource.layout;
        for slice in &mut subresource.staging {
            slice.gpu_access(layout.full(), access, |&slice, parts| {
                upload(layout, slice, parts)
            })?;
        }
        Ok(())
    }
}
