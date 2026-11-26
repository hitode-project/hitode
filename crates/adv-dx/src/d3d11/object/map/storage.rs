//! Guest shadow layouts and staging coherence for each mapped subresource.
use super::{Access, Error, Map};
use crate::d3d11::{
    abi::D3D11_MAPPED_SUBRESOURCE,
    desc::{Resource, Usage},
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

pub struct Layouts {
    resource: Resource,
    layouts: Box<[Layout]>,
    size: u32,
}

impl Layouts {
    pub fn new(resource: Resource, alignment: NonZeroU32) -> Option<Self> {
        match resource.usage() {
            Usage::Default | Usage::Immutable => return None,
            Usage::Dynamic | Usage::Staging => (),
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
                staging: if self.resource.usage() == Usage::Staging {
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
    staging: Box<[Residency<u32>]>,
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
        let subresource = &mut self.subresources[subresource as usize];
        let layout = subresource.layout;
        if let Map::Staging { access, .. } = request {
            let access = match access {
                Access::Read => SurfaceAccess::Read,
                Access::Write | Access::ReadWrite => SurfaceAccess::Write,
            };
            for slice in &mut subresource.staging {
                slice.cpu_access(layout.full(), access, |&slice, parts| {
                    download(layout, slice, parts)
                })?;
            }
        }
        Ok(D3D11_MAPPED_SUBRESOURCE {
            pData: layout.slice(self.pointer, 0),
            RowPitch: layout.row_pitch,
            DepthPitch: layout.depth_pitch,
        })
    }

    pub fn attach_staging_gpu(&mut self, subresource: u32) {
        let subresource = &mut self.subresources[subresource as usize];
        for (index, slice) in subresource.staging.iter_mut().enumerate() {
            slice.attach_gpu(subresource.layout.blocks, index as u32);
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
