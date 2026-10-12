//! Equal native attachments for a smaller logical color view. Color is copied
//! within one submission; the original depth/stencil image is never replaced.
use super::*;

// One scratch image per command producer, bounded to 64 MiB. Queue ordering
// makes reuse safe without a CPU fence or a per-guest-resource scratch cache.
const MAX_SCRATCH_PIXELS: u64 = 16 * 1024 * 1024;

pub(super) struct AttachmentColor {
    pub(super) image: Image,
    padded: bool,
}
impl AttachmentColor {
    pub(super) fn load(&self, encoder: &mut wgpu::CommandEncoder, logical: &Image) {
        if self.padded {
            copy(encoder, logical, &self.image, logical.width, logical.height);
        }
    }
    pub(super) fn store(&self, encoder: &mut wgpu::CommandEncoder, logical: &Image) {
        if self.padded {
            copy(encoder, &self.image, logical, logical.width, logical.height);
        }
    }
}
fn copy(encoder: &mut wgpu::CommandEncoder, from: &Image, to: &Image, width: u32, height: u32) {
    fn view(image: &Image) -> wgpu::TexelCopyTextureInfo<'_> {
        wgpu::TexelCopyTextureInfo {
            texture: &image.texture,
            mip_level: image.mip,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        }
    }
    encoder.copy_texture_to_texture(view(from), view(to), extent(width, height));
}
impl Gpu {
    pub(super) fn attachment_color(&mut self, target: &Target) -> Result<AttachmentColor, Error> {
        if !target.padded_attachments() {
            return Ok(AttachmentColor {
                image: target.image.clone(),
                padded: false,
            });
        }
        let depth = target.depth.as_ref().unwrap();
        let (width, height) = (depth.width(), depth.height());
        self.dimensions(width, height)?;
        if u64::from(width) * u64::from(height) > MAX_SCRATCH_PIXELS {
            return Err(Error::Unavailable(
                "attachment color scratch exceeds 64 MiB".into(),
            ));
        }
        if self
            .attachment_scratch
            .as_ref()
            .is_none_or(|image| image.width != width || image.height != height)
        {
            // Retire before allocating the replacement to keep the cache budget
            // bounded. Submitted command buffers retain their native resources.
            self.attachment_scratch = None;
            let image = Self::make_image(&self.context, width, height, 1);
            self.check()?;
            self.attachment_scratch = Some(image);
        }
        let mut image = self.attachment_scratch.as_ref().unwrap().clone();
        image.alpha = target.image.alpha;
        Ok(AttachmentColor {
            image,
            padded: true,
        })
    }
}

#[cfg(test)]
mod tests;
