//! Presentable buffers: IOSurfaces that Core Animation composes without a
//! copy. Apple has no compositor fences; the Core Animation compositor
//! attaches a surface once its write token completes.

use super::*;
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained};
use objc2_io_surface::{
    IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceColorSpace, kIOSurfaceHeight,
    kIOSurfacePixelFormat, kIOSurfaceWidth,
};

/// The only presentable format: Core Animation's `BGRA`. Its 10-bit
/// surface (`l10r`) orders channels as `Bgr10a2`, which `Format` lacks.
pub(super) const FORMAT: Format = Format::Bgra8Unorm;

pub(crate) struct PresentBuffer(CFRetained<IOSurfaceRef>);
// SAFETY: IOSurface objects are thread-safe; the buffer only holds a
// reference and hands out the pointer.
unsafe impl Send for PresentBuffer {}
unsafe impl Sync for PresentBuffer {}

impl PresentBuffer {
    pub(crate) fn new(device: &Device, desc: &PoolDesc) -> Result<(crate::Image, Self)> {
        device.check()?;
        debug_assert_eq!(desc.format, FORMAT);
        let values = [desc.width, desc.height, 4, u32::from_be_bytes(*b"BGRA")]
            .map(|n| CFNumber::new_i64(i64::from(n)));
        let properties = CFDictionary::from_slices(
            &unsafe {
                [
                    kIOSurfaceWidth,
                    kIOSurfaceHeight,
                    kIOSurfaceBytesPerElement,
                    kIOSurfacePixelFormat,
                ]
            },
            &values.each_ref().map(|v| &**v),
        );
        let surface =
            unsafe { IOSurfaceRef::new(properties.as_opaque()) }.ok_or(Error::OutOfMemory)?;
        // Tagged sRGB, so Core Animation color-manages it as the old stack did.
        let srgb = objc2_core_graphics::CGColorSpace::with_name(Some(unsafe {
            objc2_core_graphics::kCGColorSpaceSRGB
        }))
        .and_then(|space| space.property_list())
        .ok_or(Error::OutOfMemory)?;
        unsafe { surface.set_value(kIOSurfaceColorSpace, &srgb) };
        let image = ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: desc.width,
                height: desc.height,
                depth_or_layers: 1,
            },
            format: desc.format,
            mips: 1,
            samples: 1,
            usage: Usage::COLOR | Usage::SAMPLED | Usage::COPY_SRC | Usage::COPY_DST,
            view_formats: &[],
            label: desc.label,
        };
        let d = device.texture_descriptor(&image)?;
        d.setStorageMode(MTLStorageMode::Shared);
        let raw = device
            .0
            .raw
            .newTextureWithDescriptor_iosurface_plane(&d, &surface, 0)
            .ok_or(Error::OutOfMemory)?;
        Ok((device.wrap_image(raw, &image), Self(surface)))
    }
    pub(crate) fn export(&self) -> Export<'_> {
        Export::IoSurface(NonNull::from(&*self.0).cast(), std::marker::PhantomData)
    }
}

/// No compositor fences exist on Apple.
pub(crate) enum Fence {}

impl Fence {
    pub(crate) fn duplicate(&self) -> Result<Self> {
        match *self {}
    }
}
