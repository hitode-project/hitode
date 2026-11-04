use super::*;

struct Quirk {
    vendor: u32,
    versions: std::ops::RangeInclusive<u32>,
    flags: Modern,
}
const QUIRKS: &[Quirk] = &[];

unsafe extern "system" fn debug_message(
    _severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _kind: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user: *mut std::ffi::c_void,
) -> vk::Bool32 {
    // SAFETY: Vulkan supplies callback data and a nul-terminated message.
    let message = unsafe { CStr::from_ptr((*data).p_message) };
    eprintln!("Vulkan validation: {}", message.to_string_lossy());
    std::process::abort();
}

impl Device {
    pub(crate) fn open(options: &DeviceOptions) -> Result<(crate::Device, crate::Queue)> {
        // SAFETY: All creation structures and extension names live through their calls.
        let (device, mut queue) = unsafe { Self::open_vk(options, None) }?;
        if clears_pack_4444(&device, &mut queue)? {
            return Ok((device, queue));
        }
        drop((device, queue));
        let options = DeviceOptions {
            disable: options.disable | Modern::DYNAMIC_RENDERING,
            ..options.clone()
        };
        unsafe { Self::open_vk(&options, None) }
    }
    #[cfg(test)]
    pub(super) fn open_with_push_limit(
        options: &DeviceOptions,
        limit: u32,
    ) -> Result<(crate::Device, crate::Queue)> {
        unsafe { Self::open_vk(options, Some(limit)) }
    }
    unsafe fn open_vk(
        options: &DeviceOptions,
        push_limit: Option<u32>,
    ) -> Result<(crate::Device, crate::Queue)> {
        // SAFETY: Handles are used with their originating instance/device. Queried feature
        // chains remain alive through device creation; only supported features are enabled.
        unsafe {
            let entry = ash::Entry::load()
                .map_err(|e| Error::Unavailable(format!("Vulkan loader: {e}")))?;
            let api = entry
                .try_enumerate_instance_version()
                .map_err(vk_error)?
                .unwrap_or(vk::API_VERSION_1_0);
            if api < vk::API_VERSION_1_1 {
                return Err(Error::Unavailable("Vulkan 1.1".into()));
            }
            let mut layers = Vec::new();
            let mut instance_extensions = Vec::new();
            #[cfg(target_os = "android")]
            instance_extensions.push(ash::khr::surface::NAME.as_ptr());
            if options.validation {
                if !entry
                    .enumerate_instance_layer_properties()
                    .map_err(vk_error)?
                    .iter()
                    .any(|p| {
                        CStr::from_ptr(p.layer_name.as_ptr()) == c"VK_LAYER_KHRONOS_validation"
                    })
                {
                    return Err(Error::Unavailable("VK_LAYER_KHRONOS_validation".into()));
                }
                layers.push(c"VK_LAYER_KHRONOS_validation".as_ptr());
                instance_extensions.push(ash::ext::debug_utils::NAME.as_ptr());
            }
            let mut messenger_info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(
                    vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                        | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                )
                .message_type(
                    vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                        | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                )
                .pfn_user_callback(Some(debug_message));
            let app = vk::ApplicationInfo::default()
                .api_version(api)
                .application_name(c"adv-gpu");
            let mut info = vk::InstanceCreateInfo::default()
                .application_info(&app)
                .enabled_layer_names(&layers)
                .enabled_extension_names(&instance_extensions);
            if options.validation {
                info = info.push_next(&mut messenger_info);
            }
            let instance = entry.create_instance(&info, None).map_err(vk_error)?;
            // Keep partially opened instances owned too, so an unavailable adapter leaks nothing.
            let debug = if options.validation {
                let loader = ash::ext::debug_utils::Instance::new(&entry, &instance);
                match loader.create_debug_utils_messenger(&messenger_info, None) {
                    Ok(messenger) => Some((loader, messenger)),
                    Err(error) => {
                        instance.destroy_instance(None);
                        return Err(vk_error(error));
                    }
                }
            } else {
                None
            };
            #[cfg(target_os = "android")]
            let mut video_format = None;
            let result = Self::open_device(
                options,
                &entry,
                &instance,
                #[cfg(target_os = "android")]
                &mut video_format,
            );
            let (
                physical,
                device,
                family,
                caps,
                memory,
                anisotropy,
                max_anisotropy,
                mirror_clamp,
                device_api,
            ) = match result {
                Ok(value) => value,
                Err(error) => {
                    if let Some((loader, messenger)) = &debug {
                        loader.destroy_debug_utils_messenger(*messenger, None);
                    }
                    instance.destroy_instance(None);
                    return Err(error);
                }
            };
            let allocator = match Allocator::new(&AllocatorCreateDesc {
                instance: instance.clone(),
                device: device.clone(),
                physical_device: physical,
                debug_settings: Default::default(),
                buffer_device_address: false,
                allocation_sizes: Default::default(),
            }) {
                Ok(a) => a,
                Err(e) => {
                    device.destroy_device(None);
                    if let Some((loader, messenger)) = &debug {
                        loader.destroy_debug_utils_messenger(*messenger, None);
                    }
                    instance.destroy_instance(None);
                    return Err(Error::Unavailable(format!("Vulkan allocator: {e}")));
                }
            };
            let properties = instance.get_physical_device_properties(physical);
            let mut cache_prefix = [0; 28];
            cache_prefix[..16].copy_from_slice(&properties.pipeline_cache_uuid);
            for (i, value) in [
                properties.vendor_id,
                properties.device_id,
                properties.driver_version,
            ]
            .into_iter()
            .enumerate()
            {
                cache_prefix[16 + i * 4..20 + i * 4].copy_from_slice(&value.to_le_bytes());
            }
            let mut caps = caps;
            if let Some(limit) = push_limit {
                caps.limits.push_constant_bytes = caps.limits.push_constant_bytes.min(limit);
            }
            let heap = if caps.modern.contains(Modern::BINDLESS) {
                match binding::Heap::new(&instance, physical, &device) {
                    Ok(heap) => Some(heap),
                    Err(error) => {
                        drop(allocator);
                        device.destroy_device(None);
                        if let Some((loader, messenger)) = &debug {
                            loader.destroy_debug_utils_messenger(*messenger, None);
                        }
                        instance.destroy_instance(None);
                        return Err(error);
                    }
                }
            } else {
                None
            };
            if let Some(heap) = &heap {
                let indices = caps.limits.push_constant_bytes / 2;
                caps.limits.max_sampled_images = caps
                    .limits
                    .max_sampled_images
                    .min(indices)
                    .min(heap.counts[0]);
                caps.limits.max_samplers =
                    caps.limits.max_samplers.min(indices).min(heap.counts[1]);
                caps.limits.max_storage_bindings = caps
                    .limits
                    .max_storage_bindings
                    .min(caps.limits.push_constant_bytes / 6)
                    .min(heap.counts[2])
                    .min(heap.counts[3]);
            }
            let eds = caps
                .modern
                .contains(Modern::EXTENDED_DYNAMIC_STATE)
                .then(|| Eds::load(&instance, &device, device_api));
            let atom = instance
                .get_physical_device_properties(physical)
                .limits
                .non_coherent_atom_size;
            let context = Arc::new(Context {
                eds,
                pipelines: Mutex::new(pipeline::Pipelines::default()),
                heap,
                cache_prefix,
                push: ash::khr::push_descriptor::Device::new(&instance, &device),
                dynamic: ash::khr::dynamic_rendering::Device::new(&instance, &device),
                sync2: ash::khr::synchronization2::Device::new(&instance, &device),
                timeline: ash::khr::timeline_semaphore::Device::new(&instance, &device),
                #[cfg(target_os = "android")]
                ahb: ash::android::external_memory_android_hardware_buffer::Device::new(
                    &instance, &device,
                ),
                #[cfg(target_os = "android")]
                semaphore_fd: ash::khr::external_semaphore_fd::Device::new(&instance, &device),
                #[cfg(target_os = "android")]
                vendor: properties.vendor_id,
                #[cfg(target_os = "android")]
                video_format,
                _entry: entry,
                instance,
                debug,
                physical,
                device,
                family,
                caps,
                memory,
                atom,
                allocator: Mutex::new(Some(allocator)),
                lost: AtomicBool::new(false),
                next: AtomicU64::new(1),
                open: AtomicU64::new(0),
                retired: Mutex::new(VecDeque::new()),
                passes: Mutex::new(HashMap::new()),
                samplers: Mutex::new(HashMap::new()),
                rings: Mutex::new(Vec::new()),
                api: device_api,
                anisotropy,
                max_anisotropy,
                mirror_clamp,
            });
            let device = crate::Device {
                inner: Device(context),
            };
            let queue = Queue::new(device.clone())?;
            Ok((device, crate::Queue { inner: queue }))
        }
    }

    #[allow(clippy::type_complexity)]
    unsafe fn open_device(
        options: &DeviceOptions,
        entry: &ash::Entry,
        instance: &ash::Instance,
        #[cfg(target_os = "android")] opened_video_format: &mut Option<video::VideoFormat>,
    ) -> Result<(
        vk::PhysicalDevice,
        ash::Device,
        u32,
        Caps,
        vk::PhysicalDeviceMemoryProperties,
        bool,
        f32,
        bool,
        u32,
    )> {
        unsafe {
            let mut adapters = Vec::new();
            for physical in instance.enumerate_physical_devices().map_err(vk_error)? {
                let mut driver = vk::PhysicalDeviceDriverProperties::default();
                let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut driver);
                instance.get_physical_device_properties2(physical, &mut props);
                let properties = props.properties;
                if options.software && driver.driver_id != vk::DriverId::MESA_LLVMPIPE {
                    continue;
                }
                let rank = match properties.device_type {
                    vk::PhysicalDeviceType::DISCRETE_GPU => 0,
                    vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
                    _ => 2,
                };
                adapters.push((rank, physical, properties, driver));
            }
            adapters.sort_by_key(|a| a.0);
            let (_, physical, props, driver) = adapters.into_iter().next().ok_or_else(|| {
                Error::Unavailable(
                    if options.software {
                        "DRIVER_ID_MESA_LLVMPIPE"
                    } else {
                        "Vulkan adapter"
                    }
                    .into(),
                )
            })?;
            if props.api_version < vk::API_VERSION_1_1 {
                return Err(Error::Unavailable("Vulkan 1.1".into()));
            }
            let extensions = instance
                .enumerate_device_extension_properties(physical)
                .map_err(vk_error)?;
            let has = |name: &CStr| {
                extensions
                    .iter()
                    .any(|e| CStr::from_ptr(e.extension_name.as_ptr()) == name)
            };
            let api = props.api_version.min(
                entry
                    .try_enumerate_instance_version()
                    .map_err(vk_error)?
                    .unwrap_or(vk::API_VERSION_1_0),
            );
            let mut names = Vec::new();
            #[cfg(target_os = "android")]
            for name in [
                ash::android::external_memory_android_hardware_buffer::NAME,
                ash::khr::external_fence_fd::NAME,
                ash::khr::external_semaphore_fd::NAME,
                ash::khr::swapchain::NAME,
                ash::ext::queue_family_foreign::NAME,
            ] {
                if !has(name) {
                    return Err(Error::Unavailable(name.to_string_lossy().into()));
                }
                names.push(name.as_ptr());
            }
            let families = instance.get_physical_device_queue_family_properties(physical);
            let family = families
                .iter()
                .position(|q| {
                    q.queue_count > 0
                        && q.queue_flags
                            .contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE)
                })
                .ok_or_else(|| Error::Unavailable("graphics and compute queue".into()))?
                as u32;
            let mut ycbcr = vk::PhysicalDeviceSamplerYcbcrConversionFeatures::default();
            let mut dynamic = vk::PhysicalDeviceDynamicRenderingFeatures::default();
            let mut sync2 = vk::PhysicalDeviceSynchronization2Features::default();
            let mut timeline = vk::PhysicalDeviceTimelineSemaphoreFeatures::default();
            let mut indexing = vk::PhysicalDeviceDescriptorIndexingFeatures::default();
            let mut eds1 = vk::PhysicalDeviceExtendedDynamicStateFeaturesEXT::default();
            let mut eds2 = vk::PhysicalDeviceExtendedDynamicState2FeaturesEXT::default();
            let mut float16 = vk::PhysicalDeviceShaderFloat16Int8Features::default();
            let mut dot = vk::PhysicalDeviceShaderIntegerDotProductFeatures::default();
            let mut matrix = vk::PhysicalDeviceCooperativeMatrixFeaturesKHR::default();
            let mut memory_model = vk::PhysicalDeviceVulkanMemoryModelFeatures::default();
            let mut matrix_nv = vk::PhysicalDeviceCooperativeMatrixFeaturesNV::default();
            let mut weighted = vk::PhysicalDeviceImageProcessingFeaturesQCOM::default();
            let mut tile_features = vk::PhysicalDeviceTilePropertiesFeaturesQCOM::default();
            let mut imageless = vk::PhysicalDeviceImagelessFramebufferFeatures::default();
            let mut format4444 = vk::PhysicalDevice4444FormatsFeaturesEXT::default();
            let mut features = vk::PhysicalDeviceFeatures2::default()
                .push_next(&mut ycbcr)
                .push_next(&mut dynamic)
                .push_next(&mut sync2)
                .push_next(&mut timeline)
                .push_next(&mut indexing)
                .push_next(&mut eds1)
                .push_next(&mut eds2)
                .push_next(&mut float16)
                .push_next(&mut dot)
                .push_next(&mut matrix)
                .push_next(&mut matrix_nv)
                .push_next(&mut memory_model)
                .push_next(&mut weighted)
                .push_next(&mut format4444)
                .push_next(&mut tile_features);
            if api >= vk::API_VERSION_1_2 {
                features = features.push_next(&mut imageless);
            }
            instance.get_physical_device_features2(physical, &mut features);
            let base = features.features;
            // Feature query chains must be detached before building the enabled chain.
            ycbcr.p_next = std::ptr::null_mut();
            dynamic.p_next = std::ptr::null_mut();
            sync2.p_next = std::ptr::null_mut();
            timeline.p_next = std::ptr::null_mut();
            indexing.p_next = std::ptr::null_mut();
            eds1.p_next = std::ptr::null_mut();
            eds2.p_next = std::ptr::null_mut();
            float16.p_next = std::ptr::null_mut();
            dot.p_next = std::ptr::null_mut();
            matrix.p_next = std::ptr::null_mut();
            matrix_nv.p_next = std::ptr::null_mut();
            memory_model.p_next = std::ptr::null_mut();
            weighted.p_next = std::ptr::null_mut();
            format4444.p_next = std::ptr::null_mut();
            tile_features.p_next = std::ptr::null_mut();
            imageless.p_next = std::ptr::null_mut();

            if ycbcr.sampler_ycbcr_conversion == 0 {
                return Err(Error::Unavailable("samplerYcbcrConversion".into()));
            }
            let mut masked = options.disable;
            for quirk in QUIRKS {
                if quirk.vendor == props.vendor_id && quirk.versions.contains(&props.driver_version)
                {
                    masked |= quirk.flags;
                }
            }
            let mut modern = Modern::empty();
            let mut enable = |flag, supported: bool, extension: &CStr, promoted: u32| {
                if supported && (api >= promoted || has(extension)) && !masked.contains(flag) {
                    modern |= flag;
                    if api < promoted {
                        names.push(extension.as_ptr());
                    }
                    true
                } else {
                    false
                }
            };
            let use_dynamic = enable(
                Modern::DYNAMIC_RENDERING,
                dynamic.dynamic_rendering != 0
                    && (api >= vk::API_VERSION_1_2
                        || (has(ash::khr::depth_stencil_resolve::NAME)
                            && has(ash::khr::create_renderpass2::NAME))),
                ash::khr::dynamic_rendering::NAME,
                vk::API_VERSION_1_3,
            );
            let use_sync2 = enable(
                Modern::SYNCHRONIZATION2,
                sync2.synchronization2 != 0,
                ash::khr::synchronization2::NAME,
                vk::API_VERSION_1_3,
            );
            let use_timeline = enable(
                Modern::TIMELINE_SEMAPHORES,
                timeline.timeline_semaphore != 0,
                ash::khr::timeline_semaphore::NAME,
                vk::API_VERSION_1_2,
            );
            let bindless = indexing.runtime_descriptor_array != 0
                && indexing.descriptor_binding_partially_bound != 0
                && indexing.descriptor_binding_update_unused_while_pending != 0
                && indexing.descriptor_binding_uniform_buffer_update_after_bind != 0
                && indexing.descriptor_binding_sampled_image_update_after_bind != 0
                && indexing.descriptor_binding_storage_buffer_update_after_bind != 0
                && indexing.descriptor_binding_storage_image_update_after_bind != 0
                && indexing.shader_uniform_buffer_array_non_uniform_indexing != 0
                && indexing.shader_sampled_image_array_non_uniform_indexing != 0
                && indexing.shader_storage_buffer_array_non_uniform_indexing != 0
                && indexing.shader_storage_image_array_non_uniform_indexing != 0;
            let use_indexing = enable(
                Modern::BINDLESS,
                bindless,
                ash::ext::descriptor_indexing::NAME,
                vk::API_VERSION_1_2,
            );
            let _use_push = enable(
                Modern::PUSH_DESCRIPTORS,
                true,
                ash::khr::push_descriptor::NAME,
                u32::MAX,
            );
            let use_eds = enable(
                Modern::EXTENDED_DYNAMIC_STATE,
                api >= vk::API_VERSION_1_3
                    || (eds1.extended_dynamic_state != 0
                        && eds2.extended_dynamic_state2 != 0
                        && has(ash::ext::extended_dynamic_state2::NAME)),
                ash::ext::extended_dynamic_state::NAME,
                vk::API_VERSION_1_3,
            );
            enable(
                Modern::LOAD_STORE_OP_NONE,
                true,
                if has(ash::khr::load_store_op_none::NAME) {
                    ash::khr::load_store_op_none::NAME
                } else {
                    ash::ext::load_store_op_none::NAME
                },
                vk::make_api_version(0, 1, 4, 0),
            );
            if use_dynamic && api < vk::API_VERSION_1_2 {
                names.push(ash::khr::depth_stencil_resolve::NAME.as_ptr());
                names.push(ash::khr::create_renderpass2::NAME.as_ptr());
            }
            if use_eds && api < vk::API_VERSION_1_3 {
                names.push(ash::ext::extended_dynamic_state2::NAME.as_ptr());
            }
            let mut device_features = vk::PhysicalDeviceFeatures2::default()
                .features(base)
                .push_next(&mut ycbcr);
            if use_dynamic {
                device_features = device_features.push_next(&mut dynamic);
            }
            if use_sync2 {
                device_features = device_features.push_next(&mut sync2);
            }
            if use_timeline {
                device_features = device_features.push_next(&mut timeline);
            }
            if use_indexing {
                device_features = device_features.push_next(&mut indexing);
            }
            if use_eds && api < vk::API_VERSION_1_3 {
                device_features = device_features.push_next(&mut eds1).push_next(&mut eds2);
            }
            let mut feature_bits =
                Features::PIPELINE_CACHE | Features::INDIRECT | Features::YCBCR_SAMPLING;
            if float16.shader_float16 != 0
                && (api >= vk::API_VERSION_1_2 || has(ash::khr::shader_float16_int8::NAME))
            {
                if api < vk::API_VERSION_1_2 {
                    names.push(ash::khr::shader_float16_int8::NAME.as_ptr());
                }
                device_features = device_features.push_next(&mut float16);
                feature_bits |= Features::SHADER_F16;
            }
            if dot.shader_integer_dot_product != 0
                && (api >= vk::API_VERSION_1_3 || has(ash::khr::shader_integer_dot_product::NAME))
            {
                if api < vk::API_VERSION_1_3 {
                    names.push(ash::khr::shader_integer_dot_product::NAME.as_ptr());
                }
                device_features = device_features.push_next(&mut dot);
                feature_bits |= Features::INT8_DOT_PRODUCT;
            }
            let has_memory_model = memory_model.vulkan_memory_model != 0
                && (api >= vk::API_VERSION_1_2 || has(ash::khr::vulkan_memory_model::NAME));
            if has_memory_model
                && matrix.cooperative_matrix != 0
                && has(ash::khr::cooperative_matrix::NAME)
            {
                let loader = ash::khr::cooperative_matrix::Instance::new(entry, instance);
                let shapes = loader
                    .get_physical_device_cooperative_matrix_properties(physical)
                    .map_err(vk_error)?;
                if shapes.iter().any(|s| {
                    s.m_size == 8
                        && s.n_size == 8
                        && s.a_type == vk::ComponentTypeKHR::FLOAT16
                        && s.b_type == vk::ComponentTypeKHR::FLOAT16
                        && s.c_type == vk::ComponentTypeKHR::FLOAT16
                        && s.result_type == vk::ComponentTypeKHR::FLOAT16
                }) {
                    names.push(ash::khr::cooperative_matrix::NAME.as_ptr());
                    device_features = device_features.push_next(&mut matrix);
                    feature_bits |= Features::MATRIX_F16_8X8;
                }
            }
            if has_memory_model
                && !feature_bits.contains(Features::MATRIX_F16_8X8)
                && matrix_nv.cooperative_matrix != 0
                && has(ash::nv::cooperative_matrix::NAME)
            {
                let loader = ash::nv::cooperative_matrix::Instance::new(entry, instance);
                let mut count = 0;
                (loader
                    .fp()
                    .get_physical_device_cooperative_matrix_properties_nv)(
                    physical,
                    &mut count,
                    std::ptr::null_mut(),
                )
                .result()
                .map_err(vk_error)?;
                let mut shapes = vec![vk::CooperativeMatrixPropertiesNV::default(); count as usize];
                (loader
                    .fp()
                    .get_physical_device_cooperative_matrix_properties_nv)(
                    physical,
                    &mut count,
                    shapes.as_mut_ptr(),
                )
                .result()
                .map_err(vk_error)?;
                if shapes.iter().any(|s| {
                    s.m_size == 8
                        && s.n_size == 8
                        && s.a_type == vk::ComponentTypeNV::FLOAT16
                        && s.b_type == vk::ComponentTypeNV::FLOAT16
                        && s.c_type == vk::ComponentTypeNV::FLOAT16
                        && s.d_type == vk::ComponentTypeNV::FLOAT16
                }) {
                    names.push(ash::nv::cooperative_matrix::NAME.as_ptr());
                    device_features = device_features.push_next(&mut matrix_nv);
                    feature_bits |= Features::MATRIX_F16_8X8;
                }
            }
            if feature_bits.contains(Features::MATRIX_F16_8X8) {
                if api < vk::API_VERSION_1_2 {
                    names.push(ash::khr::vulkan_memory_model::NAME.as_ptr());
                }
                device_features = device_features.push_next(&mut memory_model);
            }
            if weighted.texture_sample_weighted != 0 && has(ash::qcom::image_processing::NAME) {
                names.push(ash::qcom::image_processing::NAME.as_ptr());
                device_features = device_features.push_next(&mut weighted);
                feature_bits |= Features::WEIGHTED_SAMPLING;
            }
            let has4444 = has(ash::ext::_4444_formats::NAME) && format4444.format_a4r4g4b4 != 0;
            if has4444 {
                names.push(ash::ext::_4444_formats::NAME.as_ptr());
                device_features = device_features.push_next(&mut format4444);
            }
            let mirror_clamp =
                api >= vk::API_VERSION_1_2 || has(ash::khr::sampler_mirror_clamp_to_edge::NAME);
            if mirror_clamp && api < vk::API_VERSION_1_2 {
                names.push(ash::khr::sampler_mirror_clamp_to_edge::NAME.as_ptr());
            }
            // The tile query describes its attachment through an imageless
            // framebuffer: drivers size bins by attachment format.
            let tile_extension = has(ash::qcom::tile_properties::NAME)
                && tile_features.tile_properties != 0
                && imageless.imageless_framebuffer != 0;
            if tile_extension {
                names.push(ash::qcom::tile_properties::NAME.as_ptr());
                device_features = device_features
                    .push_next(&mut tile_features)
                    .push_next(&mut imageless);
            }
            let priorities = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(family)
                .queue_priorities(&priorities)];
            let device = instance
                .create_device(
                    physical,
                    &vk::DeviceCreateInfo::default()
                        .queue_create_infos(&queues)
                        .enabled_extension_names(&names)
                        .push_next(&mut device_features),
                    None,
                )
                .map_err(vk_error)?;
            let mut subgroup = vk::PhysicalDeviceSubgroupProperties::default();
            instance.get_physical_device_properties2(
                physical,
                &mut vk::PhysicalDeviceProperties2::default().push_next(&mut subgroup),
            );
            if subgroup
                .supported_stages
                .contains(vk::ShaderStageFlags::COMPUTE)
                && subgroup
                    .supported_operations
                    .contains(vk::SubgroupFeatureFlags::BASIC)
            {
                feature_bits |= Features::SUBGROUP;
            }
            let memory = instance.get_physical_device_memory_properties(physical);
            #[cfg(target_os = "android")]
            let present_caps = present::capabilities(props.vendor_id);
            #[cfg(target_os = "android")]
            let video_format = match present::yuv_capabilities(
                &device,
                &ash::android::external_memory_android_hardware_buffer::Device::new(
                    instance, &device,
                ),
            ) {
                Ok(caps) => caps,
                Err(error) => {
                    device.destroy_device(None);
                    return Err(error);
                }
            };
            #[cfg(target_os = "android")]
            if present_caps.contains(FormatCaps::PRESENTABLE) || video_format.is_some() {
                feature_bits |= Features::PRESENTABLE_BUFFERS;
            }
            if memory.memory_types_as_slice().iter().any(|t| {
                t.property_flags
                    .contains(vk::MemoryPropertyFlags::LAZILY_ALLOCATED)
            }) {
                feature_bits |= Features::TRANSIENT_ATTACHMENTS;
            }
            if families[family as usize].timestamp_valid_bits != 0
                && props.limits.timestamp_compute_and_graphics != 0
            {
                feature_bits |= Features::TIMESTAMPS;
            }
            if base.occlusion_query_precise != 0 {
                feature_bits |= Features::OCCLUSION_QUERIES;
            }
            if base.texture_compression_bc != 0 {
                feature_bits |= Features::BC_TEXTURES;
            }
            let l = props.limits;
            let sample_counts =
                (l.framebuffer_color_sample_counts & l.framebuffer_depth_sample_counts).as_raw();
            if sample_counts & !1 != 0 {
                feature_bits |= Features::MULTISAMPLE;
            }
            let formats = formats::ALL
                .iter()
                .map(|&f| {
                    if f == Format::Nv21 {
                        #[cfg(target_os = "android")]
                        return (
                            f,
                            video_format.map_or(FormatCaps::empty(), |format| {
                                let mut caps = FormatCaps::PRESENTABLE | FormatCaps::SAMPLED;
                                if format
                                    .features
                                    .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR)
                                {
                                    caps |= FormatCaps::FILTERED;
                                }
                                caps
                            }),
                        );
                        #[cfg(not(target_os = "android"))]
                        return (f, FormatCaps::empty());
                    }
                    let flags = instance
                        .get_physical_device_format_properties(physical, formats::vk(f))
                        .optimal_tiling_features;
                    let mut caps = FormatCaps::empty();
                    for (native, public) in [
                        (vk::FormatFeatureFlags::SAMPLED_IMAGE, FormatCaps::SAMPLED),
                        (
                            vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR,
                            FormatCaps::FILTERED,
                        ),
                        (vk::FormatFeatureFlags::COLOR_ATTACHMENT, FormatCaps::COLOR),
                        (
                            vk::FormatFeatureFlags::COLOR_ATTACHMENT_BLEND,
                            FormatCaps::BLEND,
                        ),
                        (
                            vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT,
                            FormatCaps::DEPTH_STENCIL,
                        ),
                        (vk::FormatFeatureFlags::STORAGE_IMAGE, FormatCaps::STORAGE),
                    ] {
                        if flags.contains(native) {
                            caps |= public;
                        }
                    }
                    if f == Format::Bgra4Unorm && !has4444 {
                        caps = FormatCaps::empty();
                    }
                    #[cfg(target_os = "android")]
                    if f == present::FORMAT {
                        caps |= present_caps;
                    }
                    (f, caps)
                })
                .collect();
            let tile = if tile_extension {
                match query_tile(instance, &device) {
                    Ok(tile) => tile,
                    Err(error) => {
                        device.destroy_device(None);
                        return Err(error);
                    }
                }
            } else {
                (0, 0)
            };
            let caps = Caps {
                features: feature_bits,
                modern,
                adapter: CStr::from_ptr(props.device_name.as_ptr())
                    .to_string_lossy()
                    .into_owned(),
                driver: format!(
                    "{} {} ({})",
                    CStr::from_ptr(driver.driver_name.as_ptr()).to_string_lossy(),
                    CStr::from_ptr(driver.driver_info.as_ptr()).to_string_lossy(),
                    props.driver_version
                ),
                formats,
                limits: Limits {
                    max_image_2d: l.max_image_dimension2_d,
                    max_image_3d: l.max_image_dimension3_d,
                    max_image_layers: l.max_image_array_layers,
                    max_color_attachments: l.max_color_attachments,
                    max_uniform_buffers: if use_indexing {
                        (l.max_bound_descriptor_sets - 1)
                            .min(l.max_descriptor_set_uniform_buffers_dynamic)
                            .min(l.max_per_stage_descriptor_uniform_buffers)
                    } else {
                        l.max_per_stage_descriptor_uniform_buffers
                    },
                    max_sampled_images: l.max_per_stage_descriptor_sampled_images,
                    max_samplers: l.max_per_stage_descriptor_samplers,
                    max_storage_bindings: l
                        .max_per_stage_descriptor_storage_buffers
                        .min(l.max_per_stage_descriptor_storage_images),
                    max_workgroup_storage: l.max_compute_shared_memory_size,
                    max_workgroup_invocations: l.max_compute_work_group_invocations,
                    uniform_offset_alignment: l.min_uniform_buffer_offset_alignment as u32,
                    storage_offset_alignment: l.min_storage_buffer_offset_alignment as u32,
                    sample_counts,
                    tile,
                    push_constant_bytes: l.max_push_constants_size,
                },
            };
            #[cfg(target_os = "android")]
            {
                *opened_video_format = video_format;
            }
            Ok((
                physical,
                device,
                family,
                caps,
                memory,
                base.sampler_anisotropy != 0,
                l.max_sampler_anisotropy,
                mirror_clamp,
                api,
            ))
        }
    }
}

unsafe fn query_tile(instance: &ash::Instance, device: &ash::Device) -> Result<(u32, u32)> {
    // The bin size of one full-HD RGBA8 color target, the common display case.
    let (width, height) = (1920, 1080);
    let format = [vk::Format::R8G8B8A8_UNORM];
    unsafe {
        let attachments = [vk::AttachmentDescription::default()
            .format(format[0])
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::DONT_CARE)
            .store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let color = [vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let subpasses = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color)];
        let pass = device
            .create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(&attachments)
                    .subpasses(&subpasses),
                None,
            )
            .map_err(vk_error)?;
        let images = [vk::FramebufferAttachmentImageInfo::default()
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .width(width)
            .height(height)
            .layer_count(1)
            .view_formats(&format)];
        let mut imageless =
            vk::FramebufferAttachmentsCreateInfo::default().attachment_image_infos(&images);
        let framebuffer = match device.create_framebuffer(
            &vk::FramebufferCreateInfo::default()
                .push_next(&mut imageless)
                .flags(vk::FramebufferCreateFlags::IMAGELESS)
                .render_pass(pass)
                .attachment_count(1)
                .width(width)
                .height(height)
                .layers(1),
            None,
        ) {
            Ok(value) => value,
            Err(error) => {
                device.destroy_render_pass(pass, None);
                return Err(vk_error(error));
            }
        };
        let loader = ash::qcom::tile_properties::Device::new(instance, device);
        let mut count = 1;
        let mut properties = vk::TilePropertiesQCOM::default();
        let result = (loader.fp().get_framebuffer_tile_properties_qcom)(
            device.handle(),
            framebuffer,
            &mut count,
            &mut properties,
        );
        device.destroy_framebuffer(framebuffer, None);
        device.destroy_render_pass(pass, None);
        result.result().map_err(vk_error)?;
        Ok((properties.tile_size.width, properties.tile_size.height))
    }
}

/// Whether dynamic rendering clears a 4444 target to the documented bits.
/// Mali-G715 drivers (r44) pack the clear color as 565 there; render passes
/// pack it right, so such a device opens without dynamic rendering. One 1x1
/// clear and readback at open.
fn clears_pack_4444(device: &crate::Device, queue: &mut crate::Queue) -> Result<bool> {
    use crate::encode::{BufferCopy, ColorAttachment, ImageCopy, Load, RenderPassDesc, Store};
    use crate::resource::{BufferDesc, BufferUsage, Dimension, ImageDesc, Memory, Usage};
    let caps = device.caps();
    if !caps.modern.contains(Modern::DYNAMIC_RENDERING)
        || !caps.format(Format::Bgra4Unorm).contains(FormatCaps::COLOR)
    {
        return Ok(true);
    }
    let size = crate::Extent {
        width: 1,
        height: 1,
        depth_or_layers: 1,
    };
    let image = device.create_image(&ImageDesc {
        dimension: Dimension::D2,
        size,
        format: Format::Bgra4Unorm,
        mips: 1,
        samples: 1,
        usage: Usage::COLOR | Usage::COPY_SRC,
        view_formats: &[],
        label: "4444 clear check",
    })?;
    let readback = device.create_buffer(&BufferDesc {
        size: 4,
        usage: BufferUsage::COPY_DST,
        memory: Memory::Readback,
        label: "4444 clear check",
    })?;
    let mut encoder = queue.encoder("4444 clear check");
    drop(
        encoder.render_pass(&RenderPassDesc {
            colors: [ColorAttachment {
                view: image.view(),
                load: Load::Clear([1.0, 0.0, 0.0, 1.0]),
                store: Store::Store,
                resolve: None,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            area: crate::Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            timestamps: None,
            label: "4444 clear check",
        }),
    );
    encoder.copy_image_to_buffer(
        ImageCopy {
            image: &image,
            mip: 0,
            layer: 0,
            origin: (0, 0, 0),
        },
        BufferCopy {
            buffer: &readback,
            offset: 0,
            row_pitch: 0,
            rows: 0,
        },
        size,
    );
    let token = queue.submit(encoder)?;
    if !queue.wait(token, std::time::Duration::from_secs(5))? {
        return Err(Error::DeviceLost("4444 clear check timed out".into()));
    }
    // SAFETY: the submission that wrote the buffer has completed.
    let texel = unsafe { &readback.mapped().unwrap().as_ref()[..2] };
    Ok(texel == [0x00, 0xff])
}
