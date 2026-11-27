use super::*;

pub(crate) struct Image {
    pub(super) data: Arc<ImageData>,
    // The default view owns a fresh Image wrapper over the same allocation,
    // so its image back-reference never forms an Arc cycle.
    view: OnceLock<Box<crate::ImageView>>,
}

pub(super) struct ImageData {
    pub(super) raw: Object<dyn MTLTexture>,
    pub(super) planes: Option<[Object<dyn MTLTexture>; 2]>,
    desc: ImageDesc,
    lost: Arc<OnceLock<String>>,
}

impl Clone for Image {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            view: OnceLock::new(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ImageView {
    pub(super) raw: Object<dyn MTLTexture>,
    image: crate::Image,
    size: Extent,
    pub(super) depth_plane: usize,
    format: Format,
}

pub(crate) struct Buffer(pub(super) Arc<BufferData>, OnceLock<Box<crate::Buffer>>);
impl Clone for Buffer {
    fn clone(&self) -> Self {
        Self(self.0.clone(), OnceLock::new())
    }
}
pub(super) struct BufferData {
    pub(super) raw: Object<dyn MTLBuffer>,
    desc: BufferDesc,
    pub(super) lost: Arc<OnceLock<String>>,
}

#[derive(Clone)]
pub(crate) struct Sampler(#[allow(dead_code)] pub(super) Arc<Object<dyn MTLSamplerState>>);

impl Device {
    pub(crate) fn create_image(&self, desc: &ImageDesc) -> Result<crate::Image> {
        self.check()?;
        if desc.format == Format::Nv12 {
            return Err(Error::Invalid(
                "NV12 images come from presentable pools".into(),
            ));
        }
        let d = self.texture_descriptor(desc)?;
        let raw = self
            .0
            .raw
            .newTextureWithDescriptor(&d)
            .ok_or(Error::OutOfMemory)?;
        Ok(self.wrap_image(raw, desc))
    }
    /// The texture descriptor for `desc`, after checking it against the caps.
    pub(super) fn texture_descriptor(
        &self,
        desc: &ImageDesc,
    ) -> Result<Retained<MTLTextureDescriptor>> {
        let supported = self.caps().format(desc.format);
        if supported.is_empty() {
            return Err(Error::Invalid(format!(
                "unsupported format {:?}",
                desc.format
            )));
        }
        if desc.samples > 1
            && (!desc.samples.is_power_of_two()
                || self.caps().format_sample_counts(desc.format) & desc.samples == 0)
        {
            return Err(Error::Invalid(
                "unsupported multisample image format or count".into(),
            ));
        }
        for (usage, caps) in [
            (Usage::SAMPLED, FormatCaps::SAMPLED),
            (Usage::COLOR, FormatCaps::COLOR),
            (Usage::DEPTH_STENCIL, FormatCaps::DEPTH_STENCIL),
            (Usage::STORAGE, FormatCaps::STORAGE),
        ] {
            if desc.usage.contains(usage) && !supported.contains(caps) {
                return Err(Error::Invalid(format!("unsupported image usage {usage:?}")));
            }
        }
        let d = MTLTextureDescriptor::new();
        d.setTextureType(texture_type(
            desc.dimension,
            desc.size.depth_or_layers,
            desc.samples,
        ));
        d.setPixelFormat(caps::pixel_format(desc.format).unwrap());
        unsafe {
            d.setWidth(desc.size.width as usize);
            d.setHeight(if desc.dimension == Dimension::D1 {
                1
            } else {
                desc.size.height as usize
            });
            d.setDepth(if desc.dimension == Dimension::D3 {
                desc.size.depth_or_layers as usize
            } else {
                1
            });
            d.setArrayLength(match desc.dimension {
                Dimension::D3 => 1,
                Dimension::Cube => desc.size.depth_or_layers as usize / 6,
                _ => desc.size.depth_or_layers as usize,
            });
            d.setMipmapLevelCount(desc.mips as usize);
            d.setSampleCount(desc.samples as usize);
        }
        d.setStorageMode(
            if desc.usage.contains(Usage::TRANSIENT)
                && self
                    .caps()
                    .features
                    .contains(Features::TRANSIENT_ATTACHMENTS)
            {
                MTLStorageMode::Memoryless
            } else {
                MTLStorageMode::Private
            },
        );
        d.setHazardTrackingMode(MTLHazardTrackingMode::Tracked);
        // Reinterpreting views cost lossless compression; only images that
        // declare other view formats allow them.
        let sampled_view = (desc.format.has_stencil() || desc.format == Format::A8Unorm)
            && desc.usage.contains(Usage::SAMPLED);
        let mut usage = if desc.view_formats.is_empty() && !sampled_view {
            MTLTextureUsage::empty()
        } else {
            MTLTextureUsage::PixelFormatView
        };
        if desc.usage.contains(Usage::SAMPLED) {
            usage |= MTLTextureUsage::ShaderRead;
        }
        if desc.usage.contains(Usage::STORAGE) {
            usage |= MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite;
            if objc2::available!(macos = 15.0, ios = 18.0)
                && supported.contains(FormatCaps::STORAGE_ATOMIC)
            {
                usage |= MTLTextureUsage::ShaderAtomic;
            }
        }
        if desc.usage.intersects(Usage::COLOR | Usage::DEPTH_STENCIL) {
            usage |= MTLTextureUsage::RenderTarget;
        }
        d.setUsage(usage);
        Ok(d)
    }
    pub(super) fn wrap_image(&self, raw: Object<dyn MTLTexture>, desc: &ImageDesc) -> crate::Image {
        raw.setLabel(Some(&NSString::from_str(desc.label)));
        crate::Image {
            inner: Image {
                data: Arc::new(ImageData {
                    raw,
                    planes: None,
                    desc: *desc,
                    lost: self.0.lost.clone(),
                }),
                view: OnceLock::new(),
            },
        }
    }

    pub(crate) fn create_buffer(&self, desc: &BufferDesc) -> Result<crate::Buffer> {
        self.check()?;
        let mode = if desc.memory == Memory::Device {
            MTLResourceOptions::StorageModePrivate
        } else {
            MTLResourceOptions::StorageModeShared
        };
        let raw = self
            .0
            .raw
            .newBufferWithLength_options(
                desc.size as usize,
                mode | MTLResourceOptions::HazardTrackingModeTracked,
            )
            .ok_or(Error::OutOfMemory)?;
        raw.setLabel(Some(&NSString::from_str(desc.label)));
        Ok(crate::Buffer {
            inner: Buffer(
                Arc::new(BufferData {
                    raw,
                    desc: *desc,
                    lost: self.0.lost.clone(),
                }),
                OnceLock::new(),
            ),
        })
    }

    pub(crate) fn sampler(&self, desc: &SamplerDesc) -> Result<crate::Sampler> {
        self.check()?;
        let mut samplers = self.0.samplers.lock().unwrap();
        if let Some(sampler) = samplers.get(desc) {
            return Ok(crate::Sampler {
                inner: sampler.clone(),
            });
        }
        let extended_address = self.0.raw.supportsFamily(MTLGPUFamily::Apple7)
            || self.0.raw.supportsFamily(MTLGPUFamily::Mac2);
        if !extended_address
            && desc.address.iter().any(|&a| {
                a == Address::MirrorOnce
                    || (a == Address::Border && desc.border != BorderColor::TransparentBlack)
            })
        {
            return Err(Error::Invalid(
                "sampler address mode requires Apple7 or Mac2".into(),
            ));
        }
        let d = MTLSamplerDescriptor::new();
        let filter = |f| match f {
            Filter::Nearest => MTLSamplerMinMagFilter::Nearest,
            Filter::Linear => MTLSamplerMinMagFilter::Linear,
        };
        d.setMinFilter(filter(desc.min));
        d.setMagFilter(filter(desc.mag));
        d.setMipFilter(match desc.mip {
            Filter::Nearest => MTLSamplerMipFilter::Nearest,
            Filter::Linear => MTLSamplerMipFilter::Linear,
        });
        let address = |a| match a {
            Address::Repeat => MTLSamplerAddressMode::Repeat,
            Address::Mirror => MTLSamplerAddressMode::MirrorRepeat,
            Address::Clamp => MTLSamplerAddressMode::ClampToEdge,
            Address::Border => {
                if extended_address {
                    MTLSamplerAddressMode::ClampToBorderColor
                } else {
                    MTLSamplerAddressMode::ClampToZero
                }
            }
            Address::MirrorOnce => MTLSamplerAddressMode::MirrorClampToEdge,
        };
        d.setSAddressMode(address(desc.address[0]));
        d.setTAddressMode(address(desc.address[1]));
        d.setRAddressMode(address(desc.address[2]));
        d.setBorderColor(match desc.border {
            BorderColor::TransparentBlack => MTLSamplerBorderColor::TransparentBlack,
            BorderColor::OpaqueBlack => MTLSamplerBorderColor::OpaqueBlack,
            BorderColor::OpaqueWhite => MTLSamplerBorderColor::OpaqueWhite,
        });
        d.setLodMinClamp(f32::from(desc.lod_min) / 256.0);
        d.setLodMaxClamp(f32::from(desc.lod_max) / 256.0);
        if desc.lod_bias != 0 {
            if !self.0.raw.supportsFamily(MTLGPUFamily::Apple10) {
                return Err(Error::Invalid("sampler LOD bias requires Apple10".into()));
            }
            d.setLodBias(f32::from(desc.lod_bias) / 256.0);
        }
        d.setMaxAnisotropy(usize::from(desc.anisotropy));
        d.setCompareFunction(desc.compare.map_or(MTLCompareFunction::Never, compare));
        let raw = self
            .0
            .raw
            .newSamplerStateWithDescriptor(&d)
            .ok_or(Error::OutOfMemory)?;
        let sampler = Sampler(Arc::new(raw));
        samplers.insert(*desc, sampler.clone());
        Ok(crate::Sampler { inner: sampler })
    }
}

pub(super) fn compare(c: Compare) -> MTLCompareFunction {
    match c {
        Compare::Never => MTLCompareFunction::Never,
        Compare::Less => MTLCompareFunction::Less,
        Compare::Equal => MTLCompareFunction::Equal,
        Compare::LessEqual => MTLCompareFunction::LessEqual,
        Compare::Greater => MTLCompareFunction::Greater,
        Compare::NotEqual => MTLCompareFunction::NotEqual,
        Compare::GreaterEqual => MTLCompareFunction::GreaterEqual,
        Compare::Always => MTLCompareFunction::Always,
    }
}

pub(super) fn texture_type(d: Dimension, layers: u32, samples: u32) -> MTLTextureType {
    match d {
        Dimension::D1 => {
            if layers > 1 {
                MTLTextureType::Type1DArray
            } else {
                MTLTextureType::Type1D
            }
        }
        Dimension::D2 => match (layers > 1, samples > 1) {
            (false, false) => MTLTextureType::Type2D,
            (true, false) => MTLTextureType::Type2DArray,
            (false, true) => MTLTextureType::Type2DMultisample,
            (true, true) => MTLTextureType::Type2DMultisampleArray,
        },
        Dimension::D3 => MTLTextureType::Type3D,
        Dimension::Cube => {
            if layers > 6 {
                MTLTextureType::TypeCubeArray
            } else {
                MTLTextureType::TypeCube
            }
        }
    }
}

impl Image {
    pub(crate) fn desc(&self) -> &ImageDesc {
        &self.data.desc
    }
    pub(crate) fn view(&self) -> &crate::ImageView {
        self.view.get_or_init(|| {
            let desc = self.desc();
            if desc.format == Format::A8Unorm && desc.usage.contains(Usage::SAMPLED) {
                return Box::new(
                    self.create_view(
                        &ViewDesc {
                            format: None,
                            dimension: None,
                            mips: 0..desc.mips,
                            layers: 0..if desc.dimension == Dimension::D3 {
                                1
                            } else {
                                desc.size.depth_or_layers
                            },
                            plane: 0,
                        },
                        ViewLayout::default(),
                    )
                    .expect("alpha sample view"),
                );
            }
            Box::new(crate::ImageView {
                inner: ImageView {
                    raw: self.data.raw.clone(),
                    image: crate::Image {
                        inner: self.clone(),
                    },
                    size: self.desc().size,
                    depth_plane: 0,
                    format: self.desc().format,
                },
            })
        })
    }
    pub(crate) fn create_view(
        &self,
        desc: &ViewDesc,
        layout: ViewLayout,
    ) -> Result<crate::ImageView> {
        if let Some(reason) = self.data.lost.get() {
            return Err(Error::DeviceLost(reason.clone()));
        }
        let image = self.desc();
        if let Some(planes) = &self.data.planes {
            let plane = desc.plane as usize;
            if plane > 1
                || desc.mips != (0..1)
                || desc.layers != (0..1)
                || desc.dimension.is_some_and(|d| d != Dimension::D2)
                || desc
                    .format
                    .is_some_and(|f| f != [Format::R8Unorm, Format::Rg8Unorm][plane])
            {
                return Err(Error::Invalid("NV12 plane view".into()));
            }
            return Ok(crate::ImageView {
                inner: ImageView {
                    raw: planes[plane].clone(),
                    format: [Format::R8Unorm, Format::Rg8Unorm][plane],
                    depth_plane: 0,
                    image: crate::Image {
                        inner: self.clone(),
                    },
                    size: Extent {
                        width: planes[plane].width() as u32,
                        height: planes[plane].height() as u32,
                        depth_or_layers: 1,
                    },
                },
            });
        }
        if (desc.plane != 0 && (!image.format.has_stencil() || desc.plane > 2))
            || desc.mips.is_empty()
            || desc.layers.is_empty()
            || desc.mips.end > image.mips
            || desc.layers.end > image.size.depth_or_layers
        {
            return Err(Error::Invalid("image view range".into()));
        }
        let view_format = desc.format.unwrap_or(image.format);
        if view_format != image.format
            && !image.view_formats.contains(&view_format)
            && (image.format, view_format) != (Format::A8Unorm, Format::R8Unorm)
        {
            return Err(Error::Invalid(
                "image view format not declared in view_formats".into(),
            ));
        }
        let format = if desc.plane == 1 {
            MTLPixelFormat::X32_Stencil8
        } else {
            caps::pixel_format(view_format)
                .ok_or_else(|| Error::Invalid("image view format".into()))?
        };
        let dimension = desc.dimension.unwrap_or(image.dimension);
        let volume_slice = image.dimension == Dimension::D3 && dimension == Dimension::D2;
        if volume_slice
            && (desc.mips.len() != 1
                || desc.layers.len() != 1
                || desc.layers.end > (image.size.depth_or_layers >> desc.mips.start).max(1))
        {
            return Err(Error::Invalid("3D color attachment slice".into()));
        }
        let raw_dimension = if volume_slice {
            Dimension::D3
        } else {
            dimension
        };
        let slices = desc.layers.len() as u32;
        let type_slices = if layout.array && raw_dimension == Dimension::Cube {
            slices.max(12)
        } else if layout.array {
            slices.max(2)
        } else {
            slices
        };
        let ty = texture_type(raw_dimension, type_slices, image.samples);
        let mips = NSRange::new(desc.mips.start as usize, desc.mips.len());
        let layers = NSRange::new(
            if volume_slice {
                0
            } else {
                desc.layers.start as usize
            },
            if raw_dimension == Dimension::D3 {
                1
            } else {
                desc.layers.len()
            },
        );
        let raw = unsafe {
            let swizzle = if view_format == Format::A8Unorm {
                Some(alpha_swizzle())
            } else if layout.components == ViewComponents::Rgb {
                Some(MTLTextureSwizzleChannels {
                    red: MTLTextureSwizzle::Red,
                    green: MTLTextureSwizzle::Green,
                    blue: MTLTextureSwizzle::Blue,
                    alpha: MTLTextureSwizzle::One,
                })
            } else if layout.components == ViewComponents::Alpha {
                Some(MTLTextureSwizzleChannels {
                    red: MTLTextureSwizzle::Zero,
                    green: MTLTextureSwizzle::Zero,
                    blue: MTLTextureSwizzle::Zero,
                    alpha: MTLTextureSwizzle::Alpha,
                })
            } else if desc.plane == 1 {
                Some(MTLTextureSwizzleChannels {
                    red: MTLTextureSwizzle::Zero,
                    green: MTLTextureSwizzle::Red,
                    blue: MTLTextureSwizzle::Zero,
                    alpha: MTLTextureSwizzle::One,
                })
            } else {
                None
            };
            if let Some(swizzle) = swizzle {
                self.data
                    .raw
                    .newTextureViewWithPixelFormat_textureType_levels_slices_swizzle(
                        format, ty, mips, layers, swizzle,
                    )
            } else {
                self.data
                    .raw
                    .newTextureViewWithPixelFormat_textureType_levels_slices(
                        format, ty, mips, layers,
                    )
            }
        }
        .ok_or_else(|| Error::Invalid("incompatible Metal texture view".into()))?;
        let size = Extent {
            width: (image.size.width >> desc.mips.start).max(1),
            height: (image.size.height >> desc.mips.start).max(1),
            depth_or_layers: if dimension == Dimension::D3 {
                (image.size.depth_or_layers >> desc.mips.start).max(1)
            } else {
                desc.layers.len() as u32
            },
        };
        Ok(crate::ImageView {
            inner: ImageView {
                raw,
                image: crate::Image {
                    inner: self.clone(),
                },
                size,
                format: view_format,
                depth_plane: if volume_slice {
                    desc.layers.start as usize
                } else {
                    0
                },
            },
        })
    }
    pub(crate) fn id(&self) -> u64 {
        Arc::as_ptr(&self.data) as u64
    }
}
fn alpha_swizzle() -> MTLTextureSwizzleChannels {
    MTLTextureSwizzleChannels {
        red: MTLTextureSwizzle::Zero,
        green: MTLTextureSwizzle::Zero,
        blue: MTLTextureSwizzle::Zero,
        alpha: MTLTextureSwizzle::Red,
    }
}
impl ImageView {
    pub(crate) fn format(&self) -> Format {
        self.format
    }
    pub(crate) fn image(&self) -> &crate::Image {
        &self.image
    }
    pub(crate) fn size(&self) -> Extent {
        self.size
    }
}
impl Buffer {
    pub(crate) fn desc(&self) -> &BufferDesc {
        &self.0.desc
    }
    pub(crate) fn mapped(&self) -> Option<NonNull<[u8]>> {
        if self.desc().memory == Memory::Device {
            return None;
        }
        Some(NonNull::slice_from_raw_parts(
            self.0.raw.contents().cast(),
            self.desc().size as usize,
        ))
    }
    /// Shared storage is coherent.
    pub(crate) fn flush(&self, _: impl IntoIterator<Item = std::ops::Range<u64>>) {}
    pub(crate) fn slice(&self, range: std::ops::Range<u64>) -> BufferSlice<'_> {
        BufferSlice {
            buffer: self.1.get_or_init(|| {
                Box::new(crate::Buffer {
                    inner: self.clone(),
                })
            }),
            offset: range.start,
            size: range.end - range.start,
        }
    }
}

// Metal allocations are thread-safe. Descriptors and views are immutable here;
// mapped accesses are ordered by the caller's token contract.
unsafe impl Send for ImageData {}
unsafe impl Sync for ImageData {}
unsafe impl Send for BufferData {}
unsafe impl Sync for BufferData {}
unsafe impl Send for ImageView {}
unsafe impl Sync for ImageView {}
