//! Presentable buffers: IOSurfaces that Core Animation composes without a
//! copy. Apple has no compositor fences; the Core Animation compositor
//! attaches a surface once its write token completes.

use super::*;
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_io_surface::{
    IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceColorSpace, kIOSurfaceHeight,
    kIOSurfacePixelFormat, kIOSurfaceWidth,
};

#[link(name = "IOSurface", kind = "framework")]
unsafe extern "C" {
    static kIOSurfaceYCbCrMatrix: &'static CFString;
    static kIOSurfaceYCbCrMatrix_ITU_R_601_4_String: &'static CFString;
    static kIOSurfaceYCbCrMatrix_ITU_R_709_2_String: &'static CFString;
}

/// The RGB presentable format: Core Animation's `BGRA`. Its 10-bit
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
        if desc.format == Format::Nv12 {
            return Self::nv12(device, desc);
        }
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
    fn nv12(device: &Device, desc: &PoolDesc) -> Result<(crate::Image, Self)> {
        use objc2_io_surface::*;
        let plane = |w, h, bytes| {
            let values = [w, h, bytes].map(|n| CFNumber::new_i64(i64::from(n)));
            CFDictionary::from_slices(
                &unsafe {
                    [
                        kIOSurfacePlaneWidth,
                        kIOSurfacePlaneHeight,
                        kIOSurfacePlaneBytesPerElement,
                    ]
                },
                &values.each_ref().map(|v| &**v),
            )
        };
        let y = plane(desc.width, desc.height, 1u32);
        let uv = plane(desc.width.div_ceil(2), desc.height.div_ceil(2), 2);
        let planes = CFArray::from_objects(&[&*y, &*uv]);
        let values = [
            desc.width,
            desc.height,
            u32::from_be_bytes(match desc.color.unwrap() {
                YuvColor::Bt601Full | YuvColor::Bt709Full => *b"420f",
                _ => *b"420v",
            }),
        ]
        .map(|n| CFNumber::new_i64(i64::from(n)));
        let properties: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
            &unsafe {
                [
                    kIOSurfaceWidth,
                    kIOSurfaceHeight,
                    kIOSurfacePixelFormat,
                    kIOSurfacePlaneInfo,
                ]
            },
            &[
                values[0].as_ref(),
                values[1].as_ref(),
                values[2].as_ref(),
                planes.as_ref(),
            ],
        );
        let surface =
            unsafe { IOSurfaceRef::new(properties.as_opaque()) }.ok_or(Error::OutOfMemory)?;
        let color = desc.color.unwrap();
        unsafe {
            let matrix = match color {
                YuvColor::Bt601Limited | YuvColor::Bt601Full => {
                    kIOSurfaceYCbCrMatrix_ITU_R_601_4_String
                }
                YuvColor::Bt709Limited | YuvColor::Bt709Full => {
                    kIOSurfaceYCbCrMatrix_ITU_R_709_2_String
                }
            };
            surface.set_value(kIOSurfaceYCbCrMatrix, matrix.as_ref());
        }
        // Core Animation reads the transfer/primaries and range from the surface.
        let space = objc2_core_graphics::CGColorSpace::with_name(Some(unsafe {
            objc2_core_graphics::kCGColorSpaceITUR_709
        }))
        .and_then(|s| s.property_list())
        .ok_or(Error::OutOfMemory)?;
        unsafe { surface.set_value(kIOSurfaceColorSpace, &space) };
        let textures: [_; 2] = std::array::from_fn(|i| {
            let d = MTLTextureDescriptor::new();
            d.setTextureType(MTLTextureType::Type2D);
            d.setPixelFormat([MTLPixelFormat::R8Unorm, MTLPixelFormat::RG8Unorm][i]);
            unsafe {
                d.setWidth(surface.width_of_plane(i));
                d.setHeight(surface.height_of_plane(i));
            }
            d.setStorageMode(MTLStorageMode::Shared);
            d.setUsage(MTLTextureUsage::ShaderRead);
            device
                .0
                .raw
                .newTextureWithDescriptor_iosurface_plane(&d, &surface, i)
                .expect("NV12 IOSurface plane texture")
        });
        let image_desc = ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: desc.width,
                height: desc.height,
                depth_or_layers: 1,
            },
            format: Format::Nv12,
            mips: 1,
            samples: 1,
            usage: Usage::SAMPLED,
            view_formats: &[],
            label: desc.label,
        };
        let mut image = device.wrap_image(textures[0].clone(), &image_desc);
        Arc::get_mut(&mut image.inner.data).unwrap().planes = Some(textures);
        Ok((image, Self(surface)))
    }
    pub(crate) fn write(&self, f: impl FnOnce(&mut [PlaneMut<'_>])) {
        use objc2_io_surface::IOSurfaceLockOptions;
        let surface = &*self.0;
        unsafe {
            assert_eq!(
                surface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()),
                0
            );
        }
        struct Unlock<'a>(&'a IOSurfaceRef);
        impl Drop for Unlock<'_> {
            fn drop(&mut self) {
                unsafe {
                    assert_eq!(
                        self.0
                            .unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()),
                        0
                    );
                }
            }
        }
        let _unlock = Unlock(surface);
        assert_eq!(surface.plane_count(), 2, "CPU writes require NV12");
        let mut planes = std::array::from_fn::<_, 2, _>(|i| {
            let stride = surface.bytes_per_row_of_plane(i);
            let data = unsafe {
                std::slice::from_raw_parts_mut(
                    surface.base_address_of_plane(i).cast::<u8>().as_ptr(),
                    stride * surface.height_of_plane(i),
                )
            };
            PlaneMut { data, stride }
        });
        f(&mut planes);
    }
    pub(crate) fn export(&self) -> Export<'_> {
        Export::IoSurface(NonNull::from(&*self.0).cast(), std::marker::PhantomData)
    }
}

/// No compositor fences exist on Apple.
pub(crate) enum Fence {}

impl Fence {
    pub(crate) fn wait_cpu(self) {
        match self {}
    }
    pub(crate) fn duplicate(&self) -> Result<Self> {
        match *self {}
    }
}
