//! BC texture upload, using the D3DX block decoder on devices without BC.

use super::{Error, Gpu, Staged, record::WorkerItem, resources::gpu_error, stream::Command};
use adv_gpu::resource::{Dimension, ImageDesc, Usage};
use adv_gpu::{Extent, Format, Image};

pub struct BlockTexture {
    image: Image,
    encoding: Encoding,
    native: bool,
}

#[derive(Clone, Copy)]
struct Encoding {
    kind: u32,
    block_bytes: u32,
    pixel_bytes: u32,
    uncompressed: Format,
}

fn encoding(format: Format) -> Option<Encoding> {
    use Format::*;
    let (kind, block_bytes, pixel_bytes, uncompressed) = match format {
        Bc1Unorm => (1, 8, 4, Rgba8Unorm),
        Bc1Srgb => (1, 8, 4, Rgba8Srgb),
        Bc2Unorm => (2, 16, 4, Rgba8Unorm),
        Bc2Srgb => (2, 16, 4, Rgba8Srgb),
        Bc3Unorm => (3, 16, 4, Rgba8Unorm),
        Bc3Srgb => (3, 16, 4, Rgba8Srgb),
        Bc4Unorm => (4, 8, 1, R8Unorm),
        Bc4Snorm => (8, 8, 1, R8Snorm),
        Bc5Unorm => (5, 16, 2, Rg8Unorm),
        Bc5Snorm => (9, 16, 2, Rg8Snorm),
        Bc6hUfloat => (6, 16, 8, Rgba16Float),
        Bc6hSfloat => (10, 16, 8, Rgba16Float),
        Bc7Unorm => (7, 16, 4, Rgba8Unorm),
        Bc7Srgb => (7, 16, 4, Rgba8Srgb),
        _ => return None,
    };
    Some(Encoding {
        kind,
        block_bytes,
        pixel_bytes,
        uncompressed,
    })
}

#[link(name = "adv_bc_upload", kind = "static")]
unsafe extern "C" {
    fn adv_decode_bc(
        kind: u32,
        source: *const u8,
        source_pitch: u32,
        destination: *mut u8,
        width: u32,
        height: u32,
        destination_pitch: u32,
    );
}

/// The texel format produced by the BC decoder for a typed compressed view.
pub fn decoded_bc_format(format: Format) -> Option<Format> {
    encoding(format).map(|entry| entry.uncompressed)
}

/// Decode tightly packed BC blocks one volume slice at a time.
pub fn decode_bc(format: Format, extent: Extent, data: &[u8]) -> Option<Vec<u8>> {
    let encoding = encoding(format)?;
    let source_pitch = extent.width.div_ceil(4) * encoding.block_bytes;
    let source_slice = source_pitch * extent.height.div_ceil(4);
    let pitch = extent.width * encoding.pixel_bytes;
    let slice = pitch * extent.height;
    let mut decoded = vec![0; slice as usize * extent.depth_or_layers as usize];
    for (input, output) in data
        .chunks_exact(source_slice as usize)
        .zip(decoded.chunks_exact_mut(slice as usize))
    {
        // SAFETY: Each input contains complete BC blocks and each output has
        // room for all valid texels of the slice at the given row pitch.
        unsafe {
            adv_decode_bc(
                encoding.kind,
                input.as_ptr(),
                source_pitch,
                output.as_mut_ptr(),
                extent.width,
                extent.height,
                pitch,
            );
        }
    }
    Some(decoded)
}

impl BlockTexture {
    pub fn image(&self) -> &Image {
        &self.image
    }
}

impl Gpu {
    /// Create a sampled BC texture or its decoded backing allocation.
    pub fn block_texture(&self, mut desc: ImageDesc) -> Result<BlockTexture, Error> {
        let encoding =
            encoding(desc.format).ok_or_else(|| Error::Invalid("BC texture format".into()))?;
        let native = self
            .device
            .caps()
            .features
            .contains(adv_gpu::Features::BC_TEXTURES);
        // Metal rounds the BC1 three-color midpoint up. DXGI returns the
        // truncated code, including for textures that mix both BC1 modes.
        let native = native && !(cfg!(target_vendor = "apple") && encoding.kind == 1);
        if !native {
            desc.format = encoding.uncompressed;
            if !desc.view_formats.is_empty() {
                desc.view_formats = match desc.format {
                    Format::Rgba8Unorm => &[Format::Rgba8Srgb],
                    Format::Rgba8Srgb => &[Format::Rgba8Unorm],
                    _ => &[],
                };
            }
        }
        desc.usage |= Usage::COPY_DST | Usage::SAMPLED;
        let image = self.device.create_image(&desc).map_err(gpu_error)?;
        Ok(BlockTexture {
            image,
            encoding,
            native,
        })
    }

    pub fn upload_blocks(
        &mut self,
        texture: &BlockTexture,
        mip: u32,
        layer: u32,
        pitch: u32,
        data: &[u8],
    ) -> Result<(), Error> {
        let size = subresource_size(&texture.image, mip, layer)?;
        let rows = size.height.div_ceil(4);
        let row_bytes = size.width.div_ceil(4) * texture.encoding.block_bytes;
        if pitch < row_bytes
            || !pitch.is_multiple_of(texture.encoding.block_bytes)
            || pitch as usize * rows as usize > data.len()
        {
            return Err(Error::Invalid("BC upload row pitch or size".into()));
        }
        let staged = if texture.native {
            self.stage_bytes(pitch, rows, &data[..pitch as usize * rows as usize])?
        } else {
            let row_pitch = size.width * texture.encoding.pixel_bytes;
            self.stage_image_bytes(row_pitch, size.height, |out| {
                // SAFETY: data includes all compressed blocks in each row, out has
                // row_pitch * height bytes, and the decoder copies only valid texels.
                unsafe {
                    adv_decode_bc(
                        texture.encoding.kind,
                        data.as_ptr(),
                        pitch,
                        out.as_mut_ptr(),
                        size.width,
                        size.height,
                        row_pitch,
                    );
                }
            })?
        };
        self.upload_image(&texture.image, mip, layer, staged)
    }

    pub fn upload_image(
        &self,
        image: &Image,
        mip: u32,
        layer: u32,
        staged: Staged,
    ) -> Result<(), Error> {
        let extent = subresource_size(image, mip, layer)?;
        self.enqueue_modern(WorkerItem::TransferImage {
            facade: self.facade_id,
            command: Box::new(Command::UploadImage {
                image: image.clone(),
                mip,
                layer,
                staged,
                extent,
            }),
        })
    }

    pub fn copy_image_to_buffer(
        &self,
        image: &Image,
        mip: u32,
        layer: u32,
        buffer: adv_gpu::Buffer,
        row_pitch: u32,
    ) -> Result<(), Error> {
        let extent = subresource_size(image, mip, layer)?;
        self.enqueue_modern(WorkerItem::TransferImage {
            facade: self.facade_id,
            command: Box::new(Command::ReadImage {
                image: image.clone(),
                mip,
                layer,
                buffer,
                row_pitch,
                extent,
            }),
        })
    }
}

fn subresource_size(image: &Image, mip: u32, layer: u32) -> Result<Extent, Error> {
    let desc = image.desc();
    if mip >= desc.mips || layer >= desc.size.depth_or_layers || desc.dimension == Dimension::D3 {
        return Err(Error::Invalid("image mip or layer".into()));
    }
    Ok(Extent {
        width: (desc.size.width >> mip).max(1),
        height: (desc.size.height >> mip).max(1),
        depth_or_layers: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_bc_fallback_crops_partial_blocks() {
        let bc1 = [0x1f, 0, 0, 0xf8, 0xe4, 0xe4, 0xe4, 0xe4];
        let mut rgba = [0; 12];
        // SAFETY: the input contains one block and the destination one row of three pixels.
        unsafe { adv_decode_bc(1, bc1.as_ptr(), 8, rgba.as_mut_ptr(), 3, 1, 12) };
        assert_eq!(rgba, [0, 0, 255, 255, 255, 0, 0, 255, 127, 0, 127, 255]);

        let bc4 = [0x7f, 0x81, 0x88, 0x06, 0, 0, 0, 0];
        let mut signed = [0; 4];
        // SAFETY: one full compressed block and enough output for the selected texels.
        unsafe { adv_decode_bc(8, bc4.as_ptr(), 8, signed.as_mut_ptr(), 4, 1, 4) };
        assert_eq!(&signed[..2], &[127, 129]);

        let bc6 = [0; 16];
        let mut half = [0; 8];
        // SAFETY: one full compressed BC6H block and one RGBA16F output texel.
        unsafe { adv_decode_bc(6, bc6.as_ptr(), 16, half.as_mut_ptr(), 1, 1, 8) };
        assert_eq!(half, [0, 0, 0, 0, 0, 0, 0, 0x3c]);
    }
}
