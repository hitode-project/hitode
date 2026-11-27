//! Compressed residency for textures the backend cannot sample in BC form.

use super::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct Blocks {
    desc: Texture,
    data: Arc<Mutex<Data>>,
}

struct Data {
    bytes: Vec<Option<Vec<u8>>>,
    views: BTreeMap<u32, Image>,
}

impl Blocks {
    pub fn new(desc: Texture) -> Self {
        Self {
            desc,
            data: Arc::new(Mutex::new(Data {
                bytes: vec![None; (desc.layers() * desc.levels()) as usize],
                views: BTreeMap::new(),
            })),
        }
    }

    pub fn read(&self, subresource: u32, at: [u32; 3], extent: [u32; 3]) -> Vec<u8> {
        let layout = self.layout(subresource, at, extent);
        let data = self.data.lock().unwrap();
        let mut packed = vec![0; layout.row * layout.rows * extent[2] as usize];
        if let Some(bytes) = &data.bytes[subresource as usize] {
            for (range, row) in layout
                .ranges(extent[2])
                .zip(packed.chunks_exact_mut(layout.row))
            {
                row.copy_from_slice(&bytes[range]);
            }
        }
        packed
    }

    pub fn write(&self, subresource: u32, at: [u32; 3], extent: [u32; 3], packed: &[u8]) {
        let layout = self.layout(subresource, at, extent);
        let mut data = self.data.lock().unwrap();
        let bytes = data.bytes[subresource as usize].get_or_insert_with(|| vec![0; layout.size]);
        for (range, row) in layout
            .ranges(extent[2])
            .zip(packed.chunks_exact(layout.row))
        {
            bytes[range].copy_from_slice(row);
        }
        // Earlier recorded draws retain the images containing their versions.
        data.views.clear();
    }

    pub fn view(&self, gpu: &Gpu, view: &View) -> Result<ImageView, Error> {
        let mut data = self.data.lock().unwrap();
        if let Some(image) = data.views.get(&view.format()) {
            return image_view(image, view, image.desc().format);
        }
        let bc = native_format(view.format()).unwrap();
        let decoded = adv_device::gpu::decoded_bc_format(bc).unwrap();
        let dimension = texture_dimension(self.desc);
        let extent = self.desc.extent(0);
        let image = gpu.device().create_image(&ImageDesc {
            dimension,
            size: Extent {
                width: extent[0],
                height: extent[1],
                depth_or_layers: if dimension == Dimension::D3 {
                    extent[2]
                } else {
                    self.desc.layers()
                },
            },
            format: decoded,
            mips: self.desc.levels(),
            samples: 1,
            usage: Usage::SAMPLED | Usage::COPY_DST,
            view_formats: &[],
            label: "D3D11 decoded BC texture",
        })?;
        for (subresource, bytes) in data.bytes.iter().enumerate() {
            let Some(bytes) = bytes else { continue };
            let destination = region(&image, self.desc, subresource as u32);
            let extent = destination.extent;
            let pixels = adv_device::gpu::decode_bc(bc, extent, bytes).unwrap();
            let row = pixels.len() as u32 / (extent.height * extent.depth_or_layers);
            let source = transfer_buffer(gpu, row, extent.height, extent.depth_or_layers, &pixels)?;
            gpu.transfer(adv_device::gpu::ModernTransfer::UploadStaged {
                source,
                rows: extent.height,
                destination,
            })?;
        }
        let result = image_view(&image, view, decoded)?;
        data.views.insert(view.format(), image);
        Ok(result)
    }

    fn layout(&self, subresource: u32, at: [u32; 3], extent: [u32; 3]) -> BlockRows {
        let [width, height, depth] = self.desc.extent(subresource % self.desc.levels());
        let unit = self.desc.format().aligned_unit_bytes as usize;
        let pitch = width.div_ceil(4) as usize * unit;
        let slice = height.div_ceil(4) as usize * pitch;
        BlockRows {
            row: extent[0].div_ceil(4) as usize * unit,
            rows: extent[1].div_ceil(4) as usize,
            pitch,
            slice,
            offset: at[2] as usize * slice
                + (at[1] / 4) as usize * pitch
                + (at[0] / 4) as usize * unit,
            size: depth as usize * slice,
        }
    }
}

struct BlockRows {
    row: usize,
    rows: usize,
    pitch: usize,
    slice: usize,
    offset: usize,
    size: usize,
}

impl BlockRows {
    fn ranges(&self, slices: u32) -> impl Iterator<Item = std::ops::Range<usize>> {
        (0..slices as usize).flat_map(move |z| {
            (0..self.rows).map(move |y| {
                let at = self.offset + z * self.slice + y * self.pitch;
                at..at + self.row
            })
        })
    }
}
