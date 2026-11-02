use super::*;
use ash::vk::Handle;

pub(crate) struct Buffer(
    pub(super) Arc<BufferCore>,
    std::sync::OnceLock<Box<crate::Buffer>>,
);
impl Clone for Buffer {
    fn clone(&self) -> Self {
        Self(self.0.clone(), std::sync::OnceLock::new())
    }
}
pub(super) struct BufferCore {
    pub(super) context: Arc<Context>,
    pub(super) handle: vk::Buffer,
    desc: BufferDesc,
    allocation: Option<Allocation>,
    pub(super) uniforms: Mutex<HashMap<u64, vk::DescriptorSet>>,
    // One descriptor covers the whole buffer for the allocation's lifetime.
    pub(super) storage: Option<u32>,
}
impl Drop for BufferCore {
    fn drop(&mut self) {
        if let Some(heap) = &self.context.heap {
            for (_, set) in self.uniforms.get_mut().unwrap().drain() {
                heap.free_uniform(&self.context, set);
            }
            if let Some(index) = self.storage {
                heap.free(3, index);
            }
        }
        unsafe {
            self.context.device.destroy_buffer(self.handle, None);
        }
        self.context.free(self.allocation.take().unwrap());
    }
}
impl Buffer {
    pub(crate) fn desc(&self) -> &BufferDesc {
        &self.0.desc
    }
    pub(crate) fn mapped(&self) -> Option<std::ptr::NonNull<[u8]>> {
        if self.0.desc.memory == Memory::Device {
            return None;
        }
        self.0
            .allocation
            .as_ref()?
            .mapped_ptr()
            .map(|p| std::ptr::NonNull::slice_from_raw_parts(p.cast(), self.0.desc.size as usize))
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

pub(crate) struct Image {
    pub(super) core: Arc<ImageCore>,
    default: std::sync::OnceLock<crate::ImageView>,
}
impl Clone for Image {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            default: self
                .default
                .get()
                .cloned()
                .map(std::sync::OnceLock::from)
                .unwrap_or_default(),
        }
    }
}
pub(super) struct ImageCore {
    pub(super) context: Arc<Context>,
    pub(super) handle: vk::Image,
    pub(super) desc: ImageDesc,
    usage: vk::ImageUsageFlags,
    pub(super) conversion: Option<vk::SamplerYcbcrConversion>,
    allocation: Option<Allocation>,
    pub(super) states: Mutex<Vec<ImageState>>,
}
#[derive(Clone, Copy)]
pub(super) struct ImageState {
    pub(super) layout: vk::ImageLayout,
    pub(super) access: vk::AccessFlags2,
    pub(super) stage: vk::PipelineStageFlags2,
}
impl Default for ImageState {
    fn default() -> Self {
        Self {
            layout: vk::ImageLayout::UNDEFINED,
            access: vk::AccessFlags2::empty(),
            stage: vk::PipelineStageFlags2::TOP_OF_PIPE,
        }
    }
}
impl Drop for ImageCore {
    fn drop(&mut self) {
        unsafe {
            self.context.device.destroy_image(self.handle, None);
            if let Some(conversion) = self.conversion {
                self.context
                    .device
                    .destroy_sampler_ycbcr_conversion(conversion, None);
            }
        }
        self.context.free(self.allocation.take().unwrap());
    }
}
#[derive(Clone)]
pub(crate) struct ImageView(pub(super) Arc<ViewCore>);
pub(super) struct ViewCore {
    pub(super) image: crate::Image,
    pub(super) handle: vk::ImageView,
    pub(super) desc: ViewDesc,
    pub(super) format: Format,
    pub(super) sampled: Option<u32>,
    pub(super) storage: Option<u32>,
}
impl Drop for ViewCore {
    fn drop(&mut self) {
        if let Some(heap) = &self.image.inner.core.context.heap {
            if let Some(index) = self.sampled {
                heap.free(0, index);
            }
            if let Some(index) = self.storage {
                heap.free(2, index);
            }
        }
        unsafe {
            self.image
                .inner
                .core
                .context
                .device
                .destroy_image_view(self.handle, None);
        }
    }
}
impl Image {
    pub(crate) fn desc(&self) -> &ImageDesc {
        &self.core.desc
    }
    pub(crate) fn view(&self) -> &crate::ImageView {
        self.default.get_or_init(|| {
            self.default_view(&ViewDesc {
                format: None,
                dimension: None,
                mips: 0..self.desc().mips,
                layers: 0..layers(self.desc()),
                plane: 0,
            })
            .expect("default image view")
        })
    }
    pub(crate) fn id(&self) -> u64 {
        self.core.handle.as_raw()
    }
    pub(crate) fn create_view(&self, desc: &ViewDesc) -> Result<crate::ImageView> {
        self.core.context.check()?;
        if self.desc().format.planes() > 1 {
            return Err(Error::Unavailable(
                "plane views require mutable multi-planar images".into(),
            ));
        }
        self.default_view(desc)
    }
    fn default_view(&self, desc: &ViewDesc) -> Result<crate::ImageView> {
        self.core.context.check()?;
        let format = desc.format.unwrap_or(self.core.desc.format);
        if format != self.core.desc.format && !self.core.desc.view_formats.contains(&format) {
            return Err(Error::Invalid(
                "image view format not declared in view_formats".into(),
            ));
        }
        let layers = layers(&self.core.desc);
        if desc.mips.is_empty()
            || desc.mips.end > self.core.desc.mips
            || desc.layers.is_empty()
            || desc.layers.end > layers
        {
            return Err(Error::Invalid("image view subresources".into()));
        }
        let dimension = desc.dimension.unwrap_or(self.core.desc.dimension);
        let view_type = match dimension {
            Dimension::D1 => {
                if desc.layers.len() > 1 {
                    vk::ImageViewType::TYPE_1D_ARRAY
                } else {
                    vk::ImageViewType::TYPE_1D
                }
            }
            Dimension::D2 => {
                if desc.layers.len() > 1 {
                    vk::ImageViewType::TYPE_2D_ARRAY
                } else {
                    vk::ImageViewType::TYPE_2D
                }
            }
            Dimension::D3 => vk::ImageViewType::TYPE_3D,
            Dimension::Cube => {
                if desc.layers.len() > 6 {
                    vk::ImageViewType::CUBE_ARRAY
                } else {
                    vk::ImageViewType::CUBE
                }
            }
        };
        let aspect = formats::aspect(format);
        let caps = self.core.context.caps.format(format);
        if caps.is_empty() {
            return Err(Error::Unavailable(format!("image view format {format:?}")));
        }
        let mut allowed = vk::ImageUsageFlags::TRANSFER_SRC
            | vk::ImageUsageFlags::TRANSFER_DST
            | vk::ImageUsageFlags::TRANSIENT_ATTACHMENT;
        for (cap, usage) in [
            (FormatCaps::SAMPLED, vk::ImageUsageFlags::SAMPLED),
            (FormatCaps::COLOR, vk::ImageUsageFlags::COLOR_ATTACHMENT),
            (
                FormatCaps::DEPTH_STENCIL,
                vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
            ),
            (FormatCaps::STORAGE, vk::ImageUsageFlags::STORAGE),
        ] {
            if caps.contains(cap) {
                allowed |= usage;
            }
        }
        let view_usage = self.core.usage & allowed;
        if !view_usage.intersects(
            vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::STORAGE
                | vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
        ) {
            return Err(Error::Unavailable(format!(
                "image view usage for {format:?}"
            )));
        }
        let mut usage = vk::ImageViewUsageCreateInfo::default().usage(view_usage);
        let mut conversion = vk::SamplerYcbcrConversionInfo::default().conversion(
            self.core
                .conversion
                .unwrap_or(vk::SamplerYcbcrConversion::null()),
        );
        let mut info = vk::ImageViewCreateInfo::default()
            .push_next(&mut usage)
            .image(self.core.handle)
            .view_type(view_type)
            .format(formats::vk(format))
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(aspect)
                    .base_mip_level(desc.mips.start)
                    .level_count(desc.mips.end - desc.mips.start)
                    .base_array_layer(desc.layers.start)
                    .layer_count(desc.layers.end - desc.layers.start),
            );
        if self.core.conversion.is_some() {
            info = info.push_next(&mut conversion);
        }
        let handle = self
            .core
            .context
            .result(unsafe { self.core.context.device.create_image_view(&info, None) })?;
        let mut core = ViewCore {
            sampled: None,
            storage: None,
            image: crate::Image {
                inner: Image {
                    core: self.core.clone(),
                    default: std::sync::OnceLock::new(),
                },
            },
            handle,
            desc: desc.clone(),
            format,
        };
        if let Some(heap) = &self.core.context.heap {
            if view_usage.contains(vk::ImageUsageFlags::SAMPLED) && self.core.conversion.is_none() {
                core.sampled = Some(
                    heap.insert(
                        &self.core.context,
                        0,
                        None,
                        Some(
                            vk::DescriptorImageInfo::default()
                                .image_view(handle)
                                .image_layout(if format.has_depth() {
                                    vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL
                                } else {
                                    vk::ImageLayout::GENERAL
                                }),
                        ),
                    )?,
                );
            }
            if view_usage.contains(vk::ImageUsageFlags::STORAGE) {
                core.storage = Some(
                    heap.insert(
                        &self.core.context,
                        2,
                        None,
                        Some(
                            vk::DescriptorImageInfo::default()
                                .image_view(handle)
                                .image_layout(vk::ImageLayout::GENERAL),
                        ),
                    )?,
                );
            }
        }
        Ok(crate::ImageView {
            inner: ImageView(Arc::new(core)),
        })
    }
}
pub(super) fn layers(desc: &ImageDesc) -> u32 {
    if desc.dimension == Dimension::D3 {
        1
    } else {
        desc.size.depth_or_layers
    }
}
impl ImageView {
    pub(crate) fn image(&self) -> &crate::Image {
        &self.0.image
    }
    pub(crate) fn size(&self) -> Extent {
        let desc = self.0.image.desc();
        Extent {
            width: (desc.size.width >> self.0.desc.mips.start).max(1),
            height: (desc.size.height >> self.0.desc.mips.start).max(1),
            depth_or_layers: if desc.dimension == Dimension::D3 {
                (desc.size.depth_or_layers >> self.0.desc.mips.start).max(1)
            } else {
                self.0.desc.layers.end - self.0.desc.layers.start
            },
        }
    }
}
#[derive(Clone)]
pub(crate) struct Sampler(pub(super) Arc<SamplerCore>);
pub(crate) struct SamplerCore {
    context: Arc<Context>,
    pub(super) handle: vk::Sampler,
    pub(super) index: Option<u32>,
}
impl Drop for SamplerCore {
    fn drop(&mut self) {
        if let Some(index) = self.index {
            self.context.heap.as_ref().unwrap().free(1, index);
        }
        unsafe {
            self.context.device.destroy_sampler(self.handle, None);
        }
    }
}
#[derive(Clone)]
pub(crate) struct QuerySet(pub(super) Arc<QueryCore>);
pub(super) struct QueryCore {
    pub(super) context: Arc<Context>,
    pub(super) handle: vk::QueryPool,
    period: f64,
}
impl QuerySet {
    pub(crate) fn period(&self) -> f64 {
        self.0.period
    }
}
impl Drop for QueryCore {
    fn drop(&mut self) {
        unsafe {
            self.context.device.destroy_query_pool(self.handle, None);
        }
    }
}

impl Device {
    pub(crate) fn create_buffer(&self, desc: &BufferDesc) -> Result<crate::Buffer> {
        self.0.check()?;
        if desc.size == 0 {
            return Err(Error::Invalid("zero buffer size".into()));
        }
        let mut usage = vk::BufferUsageFlags::empty();
        for (public, native) in [
            (BufferUsage::VERTEX, vk::BufferUsageFlags::VERTEX_BUFFER),
            (BufferUsage::INDEX, vk::BufferUsageFlags::INDEX_BUFFER),
            (BufferUsage::UNIFORM, vk::BufferUsageFlags::UNIFORM_BUFFER),
            (BufferUsage::STORAGE, vk::BufferUsageFlags::STORAGE_BUFFER),
            (BufferUsage::INDIRECT, vk::BufferUsageFlags::INDIRECT_BUFFER),
            (BufferUsage::COPY_SRC, vk::BufferUsageFlags::TRANSFER_SRC),
            (BufferUsage::COPY_DST, vk::BufferUsageFlags::TRANSFER_DST),
        ] {
            if desc.usage.contains(public) {
                usage |= native;
            }
        }
        let handle = self.0.result(unsafe {
            self.0.device.create_buffer(
                &vk::BufferCreateInfo::default().size(desc.size).usage(usage),
                None,
            )
        })?;
        let requirements = unsafe { self.0.device.get_buffer_memory_requirements(handle) };
        let allocation = match self
            .0
            .allocate(requirements, desc.memory, true, false, desc.label)
        {
            Ok(a) => a,
            Err(e) => {
                unsafe {
                    self.0.device.destroy_buffer(handle, None);
                }
                return Err(e);
            }
        };
        if let Err(e) = self.0.result(unsafe {
            self.0
                .device
                .bind_buffer_memory(handle, allocation.memory(), allocation.offset())
        }) {
            unsafe {
                self.0.device.destroy_buffer(handle, None);
            }
            self.0.free(allocation);
            return Err(e);
        }
        let mut core = BufferCore {
            context: self.0.clone(),
            handle,
            desc: *desc,
            allocation: Some(allocation),
            uniforms: Mutex::new(HashMap::new()),
            storage: None,
        };
        if desc.usage.contains(BufferUsage::STORAGE)
            && let Some(heap) = &self.0.heap
        {
            core.storage = Some(
                heap.insert(
                    &self.0,
                    3,
                    Some(
                        vk::DescriptorBufferInfo::default()
                            .buffer(handle)
                            .range(desc.size),
                    ),
                    None,
                )?,
            );
        }
        Ok(crate::Buffer {
            inner: Buffer(Arc::new(core), std::sync::OnceLock::new()),
        })
    }
    pub(crate) fn create_image(&self, desc: &ImageDesc) -> Result<crate::Image> {
        self.0.check()?;
        if self.caps().format(desc.format).is_empty() {
            return Err(Error::Unavailable(format!(
                "image format {:?}",
                desc.format
            )));
        }
        let mut usage = vk::ImageUsageFlags::empty();
        for (public, native) in [
            (Usage::SAMPLED, vk::ImageUsageFlags::SAMPLED),
            (Usage::COLOR, vk::ImageUsageFlags::COLOR_ATTACHMENT),
            (
                Usage::DEPTH_STENCIL,
                vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
            ),
            (Usage::STORAGE, vk::ImageUsageFlags::STORAGE),
            (Usage::COPY_SRC, vk::ImageUsageFlags::TRANSFER_SRC),
            (Usage::COPY_DST, vk::ImageUsageFlags::TRANSFER_DST),
        ] {
            if desc.usage.contains(public) {
                usage |= native;
            }
        }
        // Vulkan views require a viewable usage even on a copy-only image.
        if !usage.intersects(
            vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::STORAGE
                | vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
        ) {
            let caps = self.caps().format(desc.format);
            usage |= if caps.contains(FormatCaps::SAMPLED) {
                vk::ImageUsageFlags::SAMPLED
            } else if caps.contains(FormatCaps::COLOR) {
                vk::ImageUsageFlags::COLOR_ATTACHMENT
            } else if caps.contains(FormatCaps::DEPTH_STENCIL) {
                vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
            } else {
                vk::ImageUsageFlags::STORAGE
            };
        }
        let lazy = desc.usage.contains(Usage::TRANSIENT)
            && self
                .caps()
                .features
                .contains(Features::TRANSIENT_ATTACHMENTS);
        if lazy {
            usage |= vk::ImageUsageFlags::TRANSIENT_ATTACHMENT;
        }
        // Only images that declare other view formats are mutable, and they
        // name the list so drivers can keep framebuffer compression.
        let mut flags = vk::ImageCreateFlags::empty();
        let view_formats: Vec<vk::Format> = std::iter::once(desc.format)
            .chain(desc.view_formats.iter().copied())
            .map(formats::vk)
            .collect();
        let mut format_list = vk::ImageFormatListCreateInfo::default().view_formats(&view_formats);
        if !desc.view_formats.is_empty() {
            flags |= vk::ImageCreateFlags::MUTABLE_FORMAT;
        }
        if desc.dimension == Dimension::Cube {
            flags |= vk::ImageCreateFlags::CUBE_COMPATIBLE;
        }
        let image_type = match desc.dimension {
            Dimension::D1 => vk::ImageType::TYPE_1D,
            Dimension::D3 => vk::ImageType::TYPE_3D,
            _ => vk::ImageType::TYPE_2D,
        };
        let extent = vk::Extent3D {
            width: desc.size.width,
            height: desc.size.height,
            depth: if desc.dimension == Dimension::D3 {
                desc.size.depth_or_layers
            } else {
                1
            },
        };
        let mut info = vk::ImageCreateInfo::default()
            .flags(flags)
            .image_type(image_type)
            .format(formats::vk(desc.format))
            .extent(extent)
            .mip_levels(desc.mips)
            .array_layers(layers(desc))
            .samples(vk::SampleCountFlags::from_raw(desc.samples))
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage);
        // The list is core from 1.2; older drivers get the flag alone.
        if !desc.view_formats.is_empty() && self.0.api >= vk::API_VERSION_1_2 {
            info = info.push_next(&mut format_list);
        }
        let handle = self
            .0
            .result(unsafe { self.0.device.create_image(&info, None) })?;
        let requirements = unsafe { self.0.device.get_image_memory_requirements(handle) };
        let allocation =
            match self
                .0
                .allocate(requirements, Memory::Device, false, lazy, desc.label)
            {
                Ok(a) => a,
                Err(e) => {
                    unsafe {
                        self.0.device.destroy_image(handle, None);
                    }
                    return Err(e);
                }
            };
        if let Err(e) = self.0.result(unsafe {
            self.0
                .device
                .bind_image_memory(handle, allocation.memory(), allocation.offset())
        }) {
            unsafe {
                self.0.device.destroy_image(handle, None);
            }
            self.0.free(allocation);
            return Err(e);
        }
        let mut core = ImageCore {
            context: self.0.clone(),
            handle,
            desc: *desc,
            usage,
            conversion: None,
            allocation: Some(allocation),
            states: Mutex::new(vec![
                ImageState::default();
                (desc.mips * layers(desc)) as usize
            ]),
        };
        if desc.format.planes() > 1 {
            let features = unsafe {
                self.0.instance.get_physical_device_format_properties(
                    self.0.physical,
                    formats::vk(desc.format),
                )
            }
            .optimal_tiling_features;
            let location = if features.contains(vk::FormatFeatureFlags::MIDPOINT_CHROMA_SAMPLES) {
                vk::ChromaLocation::MIDPOINT
            } else {
                vk::ChromaLocation::COSITED_EVEN
            };
            // The default view exposes the original channels; color conversion
            // remains shader policy, since the API carries no video color space.
            let info = vk::SamplerYcbcrConversionCreateInfo::default()
                .format(formats::vk(desc.format))
                .ycbcr_model(vk::SamplerYcbcrModelConversion::RGB_IDENTITY)
                .ycbcr_range(vk::SamplerYcbcrRange::ITU_FULL)
                .x_chroma_offset(location)
                .y_chroma_offset(location)
                .chroma_filter(vk::Filter::NEAREST);
            core.conversion =
                Some(self.0.result(unsafe {
                    self.0.device.create_sampler_ycbcr_conversion(&info, None)
                })?);
        }
        let image = Image {
            core: Arc::new(core),
            default: std::sync::OnceLock::new(),
        };
        let default = image.default_view(&ViewDesc {
            format: None,
            dimension: None,
            mips: 0..desc.mips,
            layers: 0..layers(desc),
            plane: 0,
        })?;
        let _ = image.default.set(default);
        Ok(crate::Image { inner: image })
    }
    pub(crate) fn sampler(&self, desc: &SamplerDesc) -> Result<crate::Sampler> {
        self.0.check()?;
        let mut cache = self.0.samplers.lock().unwrap();
        if let Some(sampler) = cache.get(desc).and_then(Weak::upgrade) {
            return Ok(crate::Sampler {
                inner: Sampler(sampler),
            });
        }
        if desc.anisotropy > 1 && !self.0.anisotropy {
            return Err(Error::Unavailable("samplerAnisotropy".into()));
        }
        if desc.address.contains(&Address::MirrorOnce) && !self.0.mirror_clamp {
            return Err(Error::Unavailable("samplerMirrorClampToEdge".into()));
        }
        let filter = |f| match f {
            Filter::Nearest => vk::Filter::NEAREST,
            Filter::Linear => vk::Filter::LINEAR,
        };
        let address = |a| match a {
            Address::Repeat => vk::SamplerAddressMode::REPEAT,
            Address::Mirror => vk::SamplerAddressMode::MIRRORED_REPEAT,
            Address::Clamp => vk::SamplerAddressMode::CLAMP_TO_EDGE,
            Address::Border => vk::SamplerAddressMode::CLAMP_TO_BORDER,
            Address::MirrorOnce => vk::SamplerAddressMode::MIRROR_CLAMP_TO_EDGE,
        };
        let info = vk::SamplerCreateInfo::default()
            .min_filter(filter(desc.min))
            .mag_filter(filter(desc.mag))
            .mipmap_mode(if desc.mip == Filter::Nearest {
                vk::SamplerMipmapMode::NEAREST
            } else {
                vk::SamplerMipmapMode::LINEAR
            })
            .address_mode_u(address(desc.address[0]))
            .address_mode_v(address(desc.address[1]))
            .address_mode_w(address(desc.address[2]))
            .mip_lod_bias(f32::from(desc.lod_bias) / 256.0)
            .min_lod(f32::from(desc.lod_min) / 256.0)
            .max_lod(f32::from(desc.lod_max) / 256.0)
            .anisotropy_enable(desc.anisotropy > 1)
            .max_anisotropy(
                f32::from(desc.anisotropy)
                    .max(1.0)
                    .min(self.0.max_anisotropy),
            )
            .compare_enable(desc.compare.is_some())
            .compare_op(compare(desc.compare.unwrap_or(Compare::Always)))
            .border_color(match desc.border {
                BorderColor::TransparentBlack => vk::BorderColor::FLOAT_TRANSPARENT_BLACK,
                BorderColor::OpaqueBlack => vk::BorderColor::FLOAT_OPAQUE_BLACK,
                BorderColor::OpaqueWhite => vk::BorderColor::FLOAT_OPAQUE_WHITE,
            });
        let handle = self
            .0
            .result(unsafe { self.0.device.create_sampler(&info, None) })?;
        let mut core = SamplerCore {
            index: None,
            context: self.0.clone(),
            handle,
        };
        if let Some(heap) = &self.0.heap {
            core.index = Some(heap.insert(
                &self.0,
                1,
                None,
                Some(vk::DescriptorImageInfo::default().sampler(handle)),
            )?);
        }
        let sampler = Arc::new(core);
        cache.insert(*desc, Arc::downgrade(&sampler));
        Ok(crate::Sampler {
            inner: Sampler(sampler),
        })
    }
    pub(crate) fn create_query_set(
        &self,
        kind: QueryKind,
        count: u32,
    ) -> Result<crate::queue::QuerySet> {
        self.0.check()?;
        let feature = match kind {
            QueryKind::Timestamp => Features::TIMESTAMPS,
            QueryKind::Occlusion => Features::OCCLUSION_QUERIES,
        };
        if !self.caps().features.contains(feature) {
            return Err(Error::Unavailable(format!("{kind:?} queries")));
        }
        let info = vk::QueryPoolCreateInfo::default()
            .query_type(match kind {
                QueryKind::Timestamp => vk::QueryType::TIMESTAMP,
                QueryKind::Occlusion => vk::QueryType::OCCLUSION,
            })
            .query_count(count);
        let handle = self
            .0
            .result(unsafe { self.0.device.create_query_pool(&info, None) })?;
        let period = unsafe {
            self.0
                .instance
                .get_physical_device_properties(self.0.physical)
        }
        .limits
        .timestamp_period as f64;
        Ok(crate::queue::QuerySet {
            inner: QuerySet(Arc::new(QueryCore {
                context: self.0.clone(),
                handle,
                period,
            })),
        })
    }
}
pub(super) fn compare(c: Compare) -> vk::CompareOp {
    match c {
        Compare::Never => vk::CompareOp::NEVER,
        Compare::Less => vk::CompareOp::LESS,
        Compare::Equal => vk::CompareOp::EQUAL,
        Compare::LessEqual => vk::CompareOp::LESS_OR_EQUAL,
        Compare::Greater => vk::CompareOp::GREATER,
        Compare::NotEqual => vk::CompareOp::NOT_EQUAL,
        Compare::GreaterEqual => vk::CompareOp::GREATER_OR_EQUAL,
        Compare::Always => vk::CompareOp::ALWAYS,
    }
}
