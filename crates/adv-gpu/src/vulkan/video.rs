//! A pool's immutable converted-video sampler and prebuilt descriptor sets.

use super::*;

#[cfg(target_os = "android")]
#[derive(Clone, Copy)]
pub(super) struct VideoFormat {
    pub(super) external: u64,
    pub(super) features: vk::FormatFeatureFlags,
    pub(super) x: vk::ChromaLocation,
    pub(super) y: vk::ChromaLocation,
}

pub(crate) struct VideoPool {
    context: Weak<Context>,
    device: ash::Device,
    pub(super) resources: Resources,
}

#[derive(Clone, Copy)]
pub(crate) struct Resources {
    pub(super) conversion: vk::SamplerYcbcrConversion,
    sampler: vk::Sampler,
    pub(super) layout: vk::DescriptorSetLayout,
}
impl Resources {
    pub(super) fn destroy(&self, device: &ash::Device) {
        unsafe {
            device.destroy_descriptor_set_layout(self.layout, None);
            device.destroy_sampler(self.sampler, None);
            device.destroy_sampler_ycbcr_conversion(self.conversion, None);
        }
    }
}
impl Drop for VideoPool {
    fn drop(&mut self) {
        let resources = self.resources;
        if let Some(context) = self.context.upgrade() {
            context.retire(Retired::Video(resources));
        } else {
            // The pipeline cache was the last owner; Context is destroying it.
            resources.destroy(&self.device);
        }
    }
}

#[cfg(target_os = "android")]
impl VideoFormat {
    pub(super) fn conversion(
        self,
        device: &ash::Device,
        color: YuvColor,
    ) -> Result<vk::SamplerYcbcrConversion> {
        let model = match color {
            YuvColor::Bt601Limited | YuvColor::Bt601Full => {
                vk::SamplerYcbcrModelConversion::YCBCR_601
            }
            YuvColor::Bt709Limited | YuvColor::Bt709Full => {
                vk::SamplerYcbcrModelConversion::YCBCR_709
            }
        };
        let range = match color {
            YuvColor::Bt601Limited | YuvColor::Bt709Limited => vk::SamplerYcbcrRange::ITU_NARROW,
            YuvColor::Bt601Full | YuvColor::Bt709Full => vk::SamplerYcbcrRange::ITU_FULL,
        };
        let mut external = vk::ExternalFormatANDROID::default().external_format(self.external);
        unsafe {
            device.create_sampler_ycbcr_conversion(
                &vk::SamplerYcbcrConversionCreateInfo::default()
                    .push_next(&mut external)
                    .format(vk::Format::UNDEFINED)
                    .ycbcr_model(model)
                    .ycbcr_range(range)
                    .components(vk::ComponentMapping::default())
                    .x_chroma_offset(self.x)
                    .y_chroma_offset(self.y)
                    .chroma_filter(self.filter()),
                None,
            )
        }
        .map_err(vk_error)
    }
    fn filter(self) -> vk::Filter {
        if self
            .features
            .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE_YCBCR_CONVERSION_LINEAR_FILTER)
        {
            vk::Filter::LINEAR
        } else {
            vk::Filter::NEAREST
        }
    }
}

#[cfg(target_os = "android")]
impl VideoPool {
    pub(crate) fn for_pool(device: &Device, desc: &PoolDesc) -> Result<Option<Arc<Self>>> {
        if desc.format != Format::Nv21 {
            return Ok(None);
        }
        let context = &device.0;
        let format = context.video_format.expect("presentable NV21 format");
        let mut resources = Resources {
            conversion: format.conversion(&context.device, desc.color.unwrap())?,
            sampler: vk::Sampler::null(),
            layout: vk::DescriptorSetLayout::null(),
        };
        let result = (|| unsafe {
            let mut conversion =
                vk::SamplerYcbcrConversionInfo::default().conversion(resources.conversion);
            // Without separate reconstruction filters the sampler must match chroma.
            let filter = if format.features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE_YCBCR_CONVERSION_SEPARATE_RECONSTRUCTION_FILTER) {
                if format.features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR) {
                    vk::Filter::LINEAR
                } else {
                    vk::Filter::NEAREST
                }
            } else { format.filter() };
            resources.sampler = context
                .device
                .create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .push_next(&mut conversion)
                        .min_filter(filter)
                        .mag_filter(filter)
                        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                    None,
                )
                .map_err(vk_error)?;
            let samplers = [resources.sampler];
            let bindings = [vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_count(1)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                .immutable_samplers(&samplers)];
            resources.layout = context
                .device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                    None,
                )
                .map_err(vk_error)?;
            Ok(())
        })();
        if let Err(error) = result {
            resources.destroy(&context.device);
            return Err(error);
        }
        Ok(Some(Arc::new(Self {
            context: Arc::downgrade(context),
            device: context.device.clone(),
            resources,
        })))
    }
    pub(super) fn allocate(self: &Arc<Self>, view: vk::ImageView) -> Result<VideoSet> {
        // One descriptor pool per buffer lets trimming retire its set without
        // consuming capacity needed by a later buffer allocation.
        let sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 3, // Budget all three Android YUV planes.
        }];
        let descriptors = unsafe {
            self.device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(1)
                    .pool_sizes(&sizes),
                None,
            )
        }
        .map_err(vk_error)?;
        let layouts = [self.resources.layout];
        let handle = match unsafe {
            self.device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(descriptors)
                    .set_layouts(&layouts),
            )
        } {
            Ok(sets) => sets[0],
            Err(error) => {
                unsafe {
                    self.device.destroy_descriptor_pool(descriptors, None);
                }
                return Err(vk_error(error));
            }
        };
        let images = [vk::DescriptorImageInfo::default()
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        unsafe {
            self.device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(handle)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&images)],
                &[],
            );
        }
        Ok(VideoSet {
            pool: self.clone(),
            context: self.context.upgrade().unwrap(),
            descriptors,
            handle,
        })
    }
}

#[cfg(target_os = "android")]
pub(super) struct VideoSet {
    pub(super) pool: Arc<VideoPool>,
    context: Arc<Context>,
    descriptors: vk::DescriptorPool,
    pub(super) handle: vk::DescriptorSet,
}
#[cfg(target_os = "android")]
impl Drop for VideoSet {
    fn drop(&mut self) {
        self.context
            .retire(Retired::VideoDescriptors(self.descriptors));
    }
}
