use super::*;

pub(crate) struct Image {
    pub(super) data: Arc<ImageData>,
    // The default view owns a fresh Image wrapper over the same allocation,
    // so its image back-reference never forms an Arc cycle.
    view: OnceLock<Box<crate::ImageView>>,
}

pub(super) struct ImageData {
    pub(super) raw: Object<dyn MTLTexture>,
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
        let supported = self.caps().format(desc.format);
        if supported.is_empty() {
            return Err(Error::Invalid(format!(
                "unsupported format {:?}",
                desc.format
            )));
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
        let mut usage = MTLTextureUsage::PixelFormatView;
        if desc.usage.contains(Usage::SAMPLED) {
            usage |= MTLTextureUsage::ShaderRead;
        }
        if desc.usage.contains(Usage::STORAGE) {
            usage |= MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite;
        }
        if desc.usage.intersects(Usage::COLOR | Usage::DEPTH_STENCIL) {
            usage |= MTLTextureUsage::RenderTarget;
        }
        d.setUsage(usage);
        let raw = self
            .0
            .raw
            .newTextureWithDescriptor(&d)
            .ok_or(Error::OutOfMemory)?;
        raw.setLabel(Some(&NSString::from_str(desc.label)));
        Ok(crate::Image {
            inner: Image {
                data: Arc::new(ImageData {
                    raw,
                    desc: *desc,
                    lost: self.0.lost.clone(),
                }),
                view: OnceLock::new(),
            },
        })
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
            Box::new(crate::ImageView {
                inner: ImageView {
                    raw: self.data.raw.clone(),
                    image: crate::Image {
                        inner: self.clone(),
                    },
                    size: self.desc().size,
                },
            })
        })
    }
    pub(crate) fn create_view(&self, desc: &ViewDesc) -> Result<crate::ImageView> {
        if let Some(reason) = self.data.lost.get() {
            return Err(Error::DeviceLost(reason.clone()));
        }
        let image = self.desc();
        if desc.plane != 0
            || desc.mips.is_empty()
            || desc.layers.is_empty()
            || desc.mips.end > image.mips
            || desc.layers.end > image.size.depth_or_layers
        {
            return Err(Error::Invalid("image view range".into()));
        }
        let format = caps::pixel_format(desc.format.unwrap_or(image.format))
            .ok_or_else(|| Error::Invalid("image view format".into()))?;
        let dimension = desc.dimension.unwrap_or(image.dimension);
        let raw = unsafe {
            self.data
                .raw
                .newTextureViewWithPixelFormat_textureType_levels_slices(
                    format,
                    texture_type(dimension, desc.layers.len() as u32, image.samples),
                    NSRange::new(desc.mips.start as usize, desc.mips.len()),
                    NSRange::new(
                        desc.layers.start as usize,
                        if dimension == Dimension::D3 {
                            1
                        } else {
                            desc.layers.len()
                        },
                    ),
                )
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
            },
        })
    }
    pub(crate) fn id(&self) -> u64 {
        Arc::as_ptr(&self.data) as u64
    }
}
impl ImageView {
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
