use super::*;
use crate::health::{
    DevicePlan, Mask, Optional, ProbeStep, QueriedSupport, load_health, next_fallback,
    run_probe_step, save_verified, select_device_plan,
};

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

struct OpenedInstance {
    entry: Option<ash::Entry>,
    instance: Option<ash::Instance>,
    instance_api: u32,
    debug: Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
}

impl OpenedInstance {
    fn into_parts(
        mut self,
    ) -> (
        ash::Entry,
        ash::Instance,
        Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    ) {
        (
            self.entry.take().unwrap(),
            self.instance.take().unwrap(),
            self.debug.take(),
        )
    }
}

impl Drop for OpenedInstance {
    fn drop(&mut self) {
        unsafe {
            if let Some((loader, messenger)) = self.debug.take() {
                loader.destroy_debug_utils_messenger(messenger, None);
            }
            if let Some(instance) = self.instance.take() {
                instance.destroy_instance(None);
            }
        }
    }
}

struct DeviceGuard<'a> {
    device: Option<ash::Device>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl DeviceGuard<'_> {
    fn new(device: ash::Device) -> Self {
        Self {
            device: Some(device),
            _marker: std::marker::PhantomData,
        }
    }
    fn get(&self) -> &ash::Device {
        self.device.as_ref().unwrap()
    }
    fn into_inner(mut self) -> ash::Device {
        self.device.take().unwrap()
    }
}

impl Drop for DeviceGuard<'_> {
    fn drop(&mut self) {
        if let Some(device) = self.device.take() {
            unsafe { device.destroy_device(None) };
        }
    }
}

struct SelectedAdapter {
    physical: vk::PhysicalDevice,
    props: vk::PhysicalDeviceProperties,
    driver_label: String,
    family: u32,
    families: Vec<vk::QueueFamilyProperties>,
    api: u32,
    cache_prefix: [u8; 28],
    base: vk::PhysicalDeviceFeatures,
    support: QueriedSupport,
}

impl Device {
    pub(crate) fn open(options: &DeviceOptions) -> Result<(crate::Device, crate::Queue)> {
        let cache_dir = options.cache_dir.as_deref();
        // SAFETY: All creation structures and extension names live through their calls.
        let mut current = Some(unsafe { Self::prepare_adapter(options)? });
        let prefix = current.as_ref().unwrap().1.cache_prefix;
        let health = load_health(cache_dir, &prefix, options.disable);
        let mut disabled = health.degraded;
        let crashed = health.crashed;

        loop {
            let (inst, adapter) = match current.take() {
                Some(pair) => pair,
                None => unsafe { Self::prepare_adapter(options)? },
            };
            let plan = select_device_plan(&adapter.support, options.disable, disabled)?;
            let active = plan.active_mask();

            let opened = run_probe_step(
                cache_dir,
                &prefix,
                ProbeStep::OpenDevice,
                active,
                disabled,
                || unsafe {
                    Self::create_configured_device(
                        inst, &adapter, options, &plan, disabled, crashed, None,
                    )
                },
            );
            let (device, mut queue) = match opened {
                Ok(pair) => pair,
                Err(Error::OutOfMemory) => return Err(Error::OutOfMemory),
                Err(error) => match next_fallback(ProbeStep::OpenDevice, active, disabled) {
                    Some(next) => {
                        disabled = next;
                        continue;
                    }
                    None => return Err(error),
                },
            };

            if health.verified && disabled == health.degraded {
                return Ok((device, queue));
            }

            let ok_4444 = run_probe_step(
                cache_dir,
                &prefix,
                ProbeStep::CanaryPack4444,
                active,
                disabled,
                || clears_pack_4444(&device, &mut queue),
            );
            match ok_4444 {
                Ok(true) => {}
                Err(Error::OutOfMemory) => return Err(Error::OutOfMemory),
                Ok(false) | Err(_) => {
                    drop((device, queue));
                    if let Some(next) = next_fallback(ProbeStep::CanaryPack4444, active, disabled) {
                        disabled = next;
                        continue;
                    }
                    return Err(Error::Unavailable("4444 clear canary failed".into()));
                }
            }

            let ok_render = run_probe_step(
                cache_dir,
                &prefix,
                ProbeStep::CanaryRender,
                active,
                disabled,
                || canary_render(&device, &mut queue),
            );
            match ok_render {
                Ok(true) => {}
                Err(Error::OutOfMemory) => return Err(Error::OutOfMemory),
                Ok(false) | Err(_) => {
                    drop((device, queue));
                    if let Some(next) = next_fallback(ProbeStep::CanaryRender, active, disabled) {
                        disabled = next;
                        continue;
                    }
                    return Err(Error::Unavailable("graphics canary failed".into()));
                }
            }

            let ok_compute = run_probe_step(
                cache_dir,
                &prefix,
                ProbeStep::CanaryCompute,
                active,
                disabled,
                || canary_compute(&device, &mut queue),
            );
            match ok_compute {
                Ok(true) => {}
                Err(Error::OutOfMemory) => return Err(Error::OutOfMemory),
                Ok(false) | Err(_) => {
                    drop((device, queue));
                    if let Some(next) = next_fallback(ProbeStep::CanaryCompute, active, disabled) {
                        disabled = next;
                        continue;
                    }
                    return Err(Error::Unavailable("compute canary failed".into()));
                }
            }

            #[cfg(target_os = "android")]
            {
                let ok_present = run_probe_step(
                    cache_dir,
                    &prefix,
                    ProbeStep::CanaryPresent,
                    active,
                    disabled,
                    || canary_present(&device, &mut queue),
                );
                match ok_present {
                    Ok(true) => {}
                    Err(Error::OutOfMemory) => return Err(Error::OutOfMemory),
                    Ok(false) | Err(_) => {
                        drop((device, queue));
                        if let Some(next) =
                            next_fallback(ProbeStep::CanaryPresent, active, disabled)
                        {
                            disabled = next;
                            continue;
                        }
                        return Err(Error::Unavailable(
                            "presentable buffer canary failed".into(),
                        ));
                    }
                }
            }

            // Reap temporary buffers, views, samplers and bindless heap slots
            // dropped at the end of each canary before handing the device back.
            queue.poll()?;
            if let Some(heap) = &device.inner.0.heap {
                heap.reset_free_order();
            }

            save_verified(
                cache_dir,
                &prefix,
                options.disable,
                device.inner.0.degraded,
                crashed,
            );
            return Ok((device, queue));
        }
    }

    #[cfg(test)]
    pub(super) fn open_with_push_limit(
        options: &DeviceOptions,
        limit: u32,
    ) -> Result<(crate::Device, crate::Queue)> {
        unsafe {
            let (inst, adapter) = Self::prepare_adapter(options)?;
            let plan = select_device_plan(&adapter.support, options.disable, Mask::empty())?;
            Self::create_configured_device(
                inst,
                &adapter,
                options,
                &plan,
                Mask::empty(),
                false,
                Some(limit),
            )
        }
    }

    unsafe fn prepare_adapter(
        options: &DeviceOptions,
    ) -> Result<(OpenedInstance, SelectedAdapter)> {
        unsafe {
            let inst = Self::open_instance(options)?;
            let adapter = Self::select_adapter(&inst, options)?;
            Ok((inst, adapter))
        }
    }

    unsafe fn open_instance(options: &DeviceOptions) -> Result<OpenedInstance> {
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
            let available_exts = entry
                .enumerate_instance_extension_properties(None)
                .unwrap_or_default();
            let has_inst_ext = |name: &CStr| {
                available_exts
                    .iter()
                    .any(|e| CStr::from_ptr(e.extension_name.as_ptr()) == name)
            };
            let mut layers = Vec::new();
            let mut instance_extensions = Vec::new();
            #[cfg(target_os = "android")]
            if has_inst_ext(ash::khr::surface::NAME) {
                instance_extensions.push(ash::khr::surface::NAME.as_ptr());
            }
            let _ = has_inst_ext;
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
            let mut opened = OpenedInstance {
                entry: Some(entry),
                instance: Some(instance),
                instance_api: api,
                debug: None,
            };
            if options.validation {
                let loader = ash::ext::debug_utils::Instance::new(
                    opened.entry.as_ref().unwrap(),
                    opened.instance.as_ref().unwrap(),
                );
                let messenger = loader
                    .create_debug_utils_messenger(&messenger_info, None)
                    .map_err(vk_error)?;
                opened.debug = Some((loader, messenger));
            }
            Ok(opened)
        }
    }

    unsafe fn select_adapter(
        inst: &OpenedInstance,
        options: &DeviceOptions,
    ) -> Result<SelectedAdapter> {
        unsafe {
            let entry = inst.entry.as_ref().unwrap();
            let instance = inst.instance.as_ref().unwrap();
            let mut candidates = Vec::new();
            let mut saw_1_0_only = false;

            for (order, physical) in instance
                .enumerate_physical_devices()
                .map_err(vk_error)?
                .into_iter()
                .enumerate()
            {
                let props = instance.get_physical_device_properties(physical);
                if props.api_version < vk::API_VERSION_1_1 {
                    saw_1_0_only = true;
                    continue;
                }
                let families = instance.get_physical_device_queue_family_properties(physical);
                let Some(family) = families.iter().position(|q| {
                    q.queue_count > 0
                        && q.queue_flags
                            .contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE)
                }) else {
                    continue;
                };
                let Ok(extensions) = instance.enumerate_device_extension_properties(physical)
                else {
                    continue;
                };
                let has_driver_props = props.api_version.min(inst.instance_api)
                    >= vk::API_VERSION_1_2
                    || extensions.iter().any(|e| {
                        CStr::from_ptr(e.extension_name.as_ptr())
                            == ash::khr::driver_properties::NAME
                    });
                let mut driver = vk::PhysicalDeviceDriverProperties::default();
                if has_driver_props {
                    let mut props2 =
                        vk::PhysicalDeviceProperties2::default().push_next(&mut driver);
                    instance.get_physical_device_properties2(physical, &mut props2);
                }
                if options.software {
                    let name_is_llvmpipe = CStr::from_ptr(props.device_name.as_ptr())
                        .to_bytes()
                        .starts_with(b"llvmpipe");
                    if driver.driver_id != vk::DriverId::MESA_LLVMPIPE && !name_is_llvmpipe {
                        continue;
                    }
                }
                let rank = match props.device_type {
                    vk::PhysicalDeviceType::DISCRETE_GPU => 0,
                    vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
                    _ => 2,
                };
                candidates.push((
                    rank,
                    order,
                    physical,
                    props,
                    driver,
                    family as u32,
                    families,
                    extensions,
                ));
            }

            candidates.sort_by_key(|a| (a.0, a.1));
            let (_, _, physical, props, driver, family, families, extensions) =
                candidates.into_iter().next().ok_or_else(|| {
                    Error::Unavailable(
                        if options.software {
                            "DRIVER_ID_MESA_LLVMPIPE"
                        } else if saw_1_0_only {
                            "Vulkan 1.1"
                        } else {
                            "Vulkan adapter"
                        }
                        .into(),
                    )
                })?;

            let has = |name: &CStr| {
                extensions
                    .iter()
                    .any(|e| CStr::from_ptr(e.extension_name.as_ptr()) == name)
            };
            let api = props.api_version.min(inst.instance_api);

            let mut cache_prefix = [0; 28];
            cache_prefix[..16].copy_from_slice(&props.pipeline_cache_uuid);
            for (i, value) in [props.vendor_id, props.device_id, props.driver_version]
                .into_iter()
                .enumerate()
            {
                cache_prefix[16 + i * 4..20 + i * 4].copy_from_slice(&value.to_le_bytes());
            }

            let mut float_controls = vk::PhysicalDeviceFloatControlsProperties::default();
            if api >= vk::API_VERSION_1_2 || has(ash::khr::shader_float_controls::NAME) {
                let mut props2 =
                    vk::PhysicalDeviceProperties2::default().push_next(&mut float_controls);
                instance.get_physical_device_properties2(physical, &mut props2);
            }

            // Gate every pNext feature struct on API promotion or extension presence so
            // strict Vulkan 1.1 drivers never see unknown structure types.
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
            let mut matrix_nv = vk::PhysicalDeviceCooperativeMatrixFeaturesNV::default();
            let mut memory_model = vk::PhysicalDeviceVulkanMemoryModelFeatures::default();
            let mut weighted = vk::PhysicalDeviceImageProcessingFeaturesQCOM::default();
            let mut format4444 = vk::PhysicalDevice4444FormatsFeaturesEXT::default();
            let mut tile_features = vk::PhysicalDeviceTilePropertiesFeaturesQCOM::default();
            let mut imageless = vk::PhysicalDeviceImagelessFramebufferFeatures::default();

            let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut ycbcr);
            if api >= vk::API_VERSION_1_3 || has(ash::khr::dynamic_rendering::NAME) {
                features = features.push_next(&mut dynamic);
            }
            if api >= vk::API_VERSION_1_3 || has(ash::khr::synchronization2::NAME) {
                features = features.push_next(&mut sync2);
            }
            if api >= vk::API_VERSION_1_2 || has(ash::khr::timeline_semaphore::NAME) {
                features = features.push_next(&mut timeline);
            }
            if api >= vk::API_VERSION_1_2 || has(ash::ext::descriptor_indexing::NAME) {
                features = features.push_next(&mut indexing);
            }
            if api < vk::API_VERSION_1_3 && has(ash::ext::extended_dynamic_state::NAME) {
                features = features.push_next(&mut eds1);
            }
            if api < vk::API_VERSION_1_3 && has(ash::ext::extended_dynamic_state2::NAME) {
                features = features.push_next(&mut eds2);
            }
            if api >= vk::API_VERSION_1_2 || has(ash::khr::shader_float16_int8::NAME) {
                features = features.push_next(&mut float16);
            }
            if api >= vk::API_VERSION_1_3 || has(ash::khr::shader_integer_dot_product::NAME) {
                features = features.push_next(&mut dot);
            }
            if has(ash::khr::cooperative_matrix::NAME) {
                features = features.push_next(&mut matrix);
            }
            if has(ash::nv::cooperative_matrix::NAME) {
                features = features.push_next(&mut matrix_nv);
            }
            if api >= vk::API_VERSION_1_2 || has(ash::khr::vulkan_memory_model::NAME) {
                features = features.push_next(&mut memory_model);
            }
            if has(ash::qcom::image_processing::NAME)
                && (api >= vk::API_VERSION_1_3 || has(ash::khr::format_feature_flags2::NAME))
            {
                features = features.push_next(&mut weighted);
            }
            if has(ash::ext::_4444_formats::NAME) {
                features = features.push_next(&mut format4444);
            }
            if api >= vk::API_VERSION_1_2 {
                features = features.push_next(&mut imageless);
                if has(ash::qcom::tile_properties::NAME) {
                    features = features.push_next(&mut tile_features);
                }
            }
            instance.get_physical_device_features2(physical, &mut features);
            let base = features.features;

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

            let has_memory_model = memory_model.vulkan_memory_model != 0
                && (api >= vk::API_VERSION_1_2 || has(ash::khr::vulkan_memory_model::NAME));
            let feat_coop_matrix_khr_8x8_f16 = has_memory_model
                && matrix.cooperative_matrix != 0
                && has(ash::khr::cooperative_matrix::NAME)
                && query_khr_8x8_f16(entry, instance, physical);
            let feat_coop_matrix_nv_8x8_f16 = has_memory_model
                && !feat_coop_matrix_khr_8x8_f16
                && matrix_nv.cooperative_matrix != 0
                && has(ash::nv::cooperative_matrix::NAME)
                && query_nv_8x8_f16(entry, instance, physical);

            let support = QueriedSupport {
                api,
                vendor_id: props.vendor_id,
                driver_version: props.driver_version,
                android: cfg!(target_os = "android"),
                ext_ahb: has(ash::android::external_memory_android_hardware_buffer::NAME),
                ext_semaphore_fd: has(ash::khr::external_semaphore_fd::NAME),
                ext_foreign_queue: has(ash::ext::queue_family_foreign::NAME),
                ext_dynamic_rendering: has(ash::khr::dynamic_rendering::NAME),
                ext_depth_stencil_resolve: has(ash::khr::depth_stencil_resolve::NAME),
                ext_create_renderpass2: has(ash::khr::create_renderpass2::NAME),
                ext_sync2: has(ash::khr::synchronization2::NAME),
                ext_timeline: has(ash::khr::timeline_semaphore::NAME),
                ext_descriptor_indexing: has(ash::ext::descriptor_indexing::NAME),
                ext_push_descriptor: has(ash::khr::push_descriptor::NAME),
                ext_eds1: has(ash::ext::extended_dynamic_state::NAME),
                ext_eds2: has(ash::ext::extended_dynamic_state2::NAME),
                ext_load_store_op_none_khr: has(ash::khr::load_store_op_none::NAME),
                ext_load_store_op_none_ext: has(ash::ext::load_store_op_none::NAME),
                ext_float_controls: has(ash::khr::shader_float_controls::NAME),
                ext_float16_int8: has(ash::khr::shader_float16_int8::NAME),
                ext_int8_dot: has(ash::khr::shader_integer_dot_product::NAME),
                ext_vulkan_memory_model: has(ash::khr::vulkan_memory_model::NAME),
                ext_coop_matrix_khr: has(ash::khr::cooperative_matrix::NAME),
                ext_coop_matrix_nv: has(ash::nv::cooperative_matrix::NAME),
                ext_qcom_image_processing: has(ash::qcom::image_processing::NAME),
                ext_format_feature_flags2: has(ash::khr::format_feature_flags2::NAME),
                ext_4444_formats: has(ash::ext::_4444_formats::NAME),
                ext_mirror_clamp: has(ash::khr::sampler_mirror_clamp_to_edge::NAME),
                ext_qcom_tile_properties: has(ash::qcom::tile_properties::NAME),
                feat_ycbcr: ycbcr.sampler_ycbcr_conversion != 0,
                feat_dynamic_rendering: dynamic.dynamic_rendering != 0,
                feat_sync2: sync2.synchronization2 != 0,
                feat_timeline: timeline.timeline_semaphore != 0,
                feat_bindless: bindless,
                feat_eds1: eds1.extended_dynamic_state != 0,
                feat_eds2: eds2.extended_dynamic_state2 != 0,
                feat_ieee_f32: float_controls.shader_signed_zero_inf_nan_preserve_float32 != 0,
                feat_shader_f16: float16.shader_float16 != 0,
                feat_int8_dot: dot.shader_integer_dot_product != 0,
                feat_memory_model: memory_model.vulkan_memory_model != 0,
                feat_coop_matrix_khr_8x8_f16,
                feat_coop_matrix_nv_8x8_f16,
                feat_weighted_sampling: weighted.texture_sample_weighted != 0,
                feat_4444: format4444.format_a4r4g4b4 != 0,
                feat_tile_properties: tile_features.tile_properties != 0,
                feat_imageless_framebuffer: imageless.imageless_framebuffer != 0,
            };

            let driver_name = CStr::from_ptr(driver.driver_name.as_ptr()).to_string_lossy();
            let driver_info = CStr::from_ptr(driver.driver_info.as_ptr()).to_string_lossy();
            let driver_label = if driver_name.is_empty() && driver_info.is_empty() {
                format!("{}", props.driver_version)
            } else {
                format!("{driver_name} {driver_info} ({})", props.driver_version)
            };

            Ok(SelectedAdapter {
                physical,
                props,
                driver_label,
                family,
                families,
                api,
                cache_prefix,
                base,
                support,
            })
        }
    }

    unsafe fn create_configured_device(
        inst: OpenedInstance,
        adapter: &SelectedAdapter,
        options: &DeviceOptions,
        plan: &DevicePlan,
        mut degraded: Mask,
        crashed: bool,
        push_limit: Option<u32>,
    ) -> Result<(crate::Device, crate::Queue)> {
        unsafe {
            let instance = inst.instance.as_ref().unwrap();
            let physical = adapter.physical;
            let base = adapter.base;

            // Enable only the Vulkan 1.0 feature bits our front ends and host
            // passes actually use, avoiding driver bugs in unused stages,
            // sparse residency, or robust buffer access.
            let enabled_base = vk::PhysicalDeviceFeatures {
                full_draw_index_uint32: base.full_draw_index_uint32,
                image_cube_array: base.image_cube_array,
                independent_blend: base.independent_blend,
                dual_src_blend: base.dual_src_blend,
                depth_clamp: base.depth_clamp,
                depth_bias_clamp: base.depth_bias_clamp,
                fill_mode_non_solid: base.fill_mode_non_solid,
                sampler_anisotropy: base.sampler_anisotropy,
                texture_compression_bc: base.texture_compression_bc,
                occlusion_query_precise: base.occlusion_query_precise,
                vertex_pipeline_stores_and_atomics: base.vertex_pipeline_stores_and_atomics,
                fragment_stores_and_atomics: base.fragment_stores_and_atomics,
                shader_storage_image_extended_formats: base.shader_storage_image_extended_formats,
                shader_image_gather_extended: base.shader_image_gather_extended,
                shader_clip_distance: base.shader_clip_distance,
                draw_indirect_first_instance: base.draw_indirect_first_instance,
                ..Default::default()
            };

            let mut enabled_ycbcr = vk::PhysicalDeviceSamplerYcbcrConversionFeatures::default()
                .sampler_ycbcr_conversion(true);
            let mut enabled_dynamic =
                vk::PhysicalDeviceDynamicRenderingFeatures::default().dynamic_rendering(true);
            let mut enabled_sync2 =
                vk::PhysicalDeviceSynchronization2Features::default().synchronization2(true);
            let mut enabled_timeline =
                vk::PhysicalDeviceTimelineSemaphoreFeatures::default().timeline_semaphore(true);
            let mut enabled_indexing = vk::PhysicalDeviceDescriptorIndexingFeatures::default()
                .runtime_descriptor_array(true)
                .descriptor_binding_partially_bound(true)
                .descriptor_binding_update_unused_while_pending(true)
                .descriptor_binding_uniform_buffer_update_after_bind(true)
                .descriptor_binding_sampled_image_update_after_bind(true)
                .descriptor_binding_storage_buffer_update_after_bind(true)
                .descriptor_binding_storage_image_update_after_bind(true)
                .shader_uniform_buffer_array_non_uniform_indexing(true)
                .shader_sampled_image_array_non_uniform_indexing(true)
                .shader_storage_buffer_array_non_uniform_indexing(true)
                .shader_storage_image_array_non_uniform_indexing(true);
            let mut enabled_eds1 = vk::PhysicalDeviceExtendedDynamicStateFeaturesEXT::default()
                .extended_dynamic_state(true);
            let mut enabled_eds2 = vk::PhysicalDeviceExtendedDynamicState2FeaturesEXT::default()
                .extended_dynamic_state2(true);
            let mut enabled_float16 =
                vk::PhysicalDeviceShaderFloat16Int8Features::default().shader_float16(true);
            let mut enabled_dot = vk::PhysicalDeviceShaderIntegerDotProductFeatures::default()
                .shader_integer_dot_product(true);
            let mut enabled_matrix =
                vk::PhysicalDeviceCooperativeMatrixFeaturesKHR::default().cooperative_matrix(true);
            let mut enabled_matrix_nv =
                vk::PhysicalDeviceCooperativeMatrixFeaturesNV::default().cooperative_matrix(true);
            let mut enabled_memory_model =
                vk::PhysicalDeviceVulkanMemoryModelFeatures::default().vulkan_memory_model(true);
            let mut enabled_weighted = vk::PhysicalDeviceImageProcessingFeaturesQCOM::default()
                .texture_sample_weighted(true);
            let mut enabled_4444 =
                vk::PhysicalDevice4444FormatsFeaturesEXT::default().format_a4r4g4b4(true);
            let mut enabled_tile =
                vk::PhysicalDeviceTilePropertiesFeaturesQCOM::default().tile_properties(true);
            let mut enabled_imageless = vk::PhysicalDeviceImagelessFramebufferFeatures::default()
                .imageless_framebuffer(true);

            let mut device_features = vk::PhysicalDeviceFeatures2::default().features(enabled_base);
            if plan.use_ycbcr {
                device_features = device_features.push_next(&mut enabled_ycbcr);
            }
            if plan.modern.contains(Modern::DYNAMIC_RENDERING) {
                device_features = device_features.push_next(&mut enabled_dynamic);
            }
            if plan.modern.contains(Modern::SYNCHRONIZATION2) {
                device_features = device_features.push_next(&mut enabled_sync2);
            }
            if plan.modern.contains(Modern::TIMELINE_SEMAPHORES) {
                device_features = device_features.push_next(&mut enabled_timeline);
            }
            if plan.modern.contains(Modern::BINDLESS) {
                device_features = device_features.push_next(&mut enabled_indexing);
            }
            if plan.chain_eds_features {
                device_features = device_features
                    .push_next(&mut enabled_eds1)
                    .push_next(&mut enabled_eds2);
            }
            if plan.use_shader_f16 {
                device_features = device_features.push_next(&mut enabled_float16);
            }
            if plan.use_int8_dot {
                device_features = device_features.push_next(&mut enabled_dot);
            }
            if plan.use_coop_matrix_khr {
                device_features = device_features.push_next(&mut enabled_matrix);
            }
            if plan.use_coop_matrix_nv {
                device_features = device_features.push_next(&mut enabled_matrix_nv);
            }
            if plan.use_memory_model {
                device_features = device_features.push_next(&mut enabled_memory_model);
            }
            if plan.use_weighted_sampling {
                device_features = device_features.push_next(&mut enabled_weighted);
            }
            if plan.use_4444 {
                device_features = device_features.push_next(&mut enabled_4444);
            }
            if plan.use_tile_properties {
                device_features = device_features
                    .push_next(&mut enabled_tile)
                    .push_next(&mut enabled_imageless);
            }

            let names: Vec<*const std::ffi::c_char> =
                plan.extensions.iter().map(|c| c.as_ptr()).collect();
            let priorities = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(adapter.family)
                .queue_priorities(&priorities)];
            let raw_device = instance
                .create_device(
                    physical,
                    &vk::DeviceCreateInfo::default()
                        .queue_create_infos(&queues)
                        .enabled_extension_names(&names)
                        .push_next(&mut device_features),
                    None,
                )
                .map_err(vk_error)?;
            let device_guard = DeviceGuard::new(raw_device);
            let device = device_guard.get();

            let mut feature_bits = Features::PIPELINE_CACHE | Features::INDIRECT;
            if plan.use_ycbcr {
                feature_bits |= Features::YCBCR_SAMPLING;
            }
            if plan.use_shader_f16 {
                feature_bits |= Features::SHADER_F16;
            }
            if plan.use_int8_dot {
                feature_bits |= Features::INT8_DOT_PRODUCT;
            }
            if plan.use_coop_matrix_khr || plan.use_coop_matrix_nv {
                feature_bits |= Features::MATRIX_F16_8X8;
            }
            if plan.use_weighted_sampling {
                feature_bits |= Features::WEIGHTED_SAMPLING;
            }

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
            let ahb = ash::android::external_memory_android_hardware_buffer::Device::new(
                instance, device,
            );
            #[cfg(target_os = "android")]
            let (present_caps, ahb_ubwc) = present::rgb_capabilities(
                device,
                &ahb,
                adapter.props.vendor_id,
                plan.use_ahb_ubwc,
            )?;
            #[cfg(target_os = "android")]
            if plan.use_ahb_ubwc && adapter.props.vendor_id == 0x5143 && !ahb_ubwc {
                degraded.optional |= Optional::AHB_UBWC;
            }
            #[cfg(target_os = "android")]
            let video_format = if plan.use_ycbcr {
                present::yuv_capabilities(device, &ahb)?
            } else {
                None
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
            if adapter.families[adapter.family as usize].timestamp_valid_bits != 0
                && adapter.props.limits.timestamp_compute_and_graphics != 0
            {
                feature_bits |= Features::TIMESTAMPS;
            }
            if base.occlusion_query_precise != 0 {
                feature_bits |= Features::OCCLUSION_QUERIES;
            }
            if base.texture_compression_bc != 0 {
                feature_bits |= Features::BC_TEXTURES;
            }
            let l = adapter.props.limits;
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
                    if f == Format::Bgra4Unorm && !plan.use_4444 {
                        caps = FormatCaps::empty();
                    }
                    #[cfg(target_os = "android")]
                    if f == present::FORMAT {
                        caps |= present_caps;
                    }
                    (f, caps)
                })
                .collect();

            let tile = if plan.use_tile_properties {
                match query_tile(instance, device, true) {
                    Ok(tile) => tile,
                    Err(_) => {
                        degraded.optional |= Optional::TILE_PROPERTIES;
                        query_tile(instance, device, false).unwrap_or((0, 0))
                    }
                }
            } else {
                query_tile(instance, device, false).unwrap_or((0, 0))
            };

            let use_indexing = plan.modern.contains(Modern::BINDLESS);
            let mut caps = Caps {
                features: feature_bits,
                ieee_f32: plan.use_ieee_f32,
                modern: plan.modern,
                adapter: CStr::from_ptr(adapter.props.device_name.as_ptr())
                    .to_string_lossy()
                    .into_owned(),
                driver: adapter.driver_label.clone(),
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

            if let Some(limit) = push_limit {
                caps.limits.push_constant_bytes = caps.limits.push_constant_bytes.min(limit);
            }

            let allocator = Allocator::new(&AllocatorCreateDesc {
                instance: instance.clone(),
                device: device.clone(),
                physical_device: physical,
                debug_settings: Default::default(),
                buffer_device_address: false,
                allocation_sizes: Default::default(),
            })
            .map_err(|e| Error::Unavailable(format!("Vulkan allocator: {e}")))?;

            let heap = if use_indexing {
                match binding::Heap::new(instance, physical, device) {
                    Ok(heap) => Some(heap),
                    Err(error) => {
                        drop(allocator);
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
                .then(|| Eds::load(instance, device, adapter.api));
            let push = ash::khr::push_descriptor::Device::new(instance, device);
            let dynamic = ash::khr::dynamic_rendering::Device::new(instance, device);
            let sync2 = ash::khr::synchronization2::Device::new(instance, device);
            let timeline = ash::khr::timeline_semaphore::Device::new(instance, device);
            #[cfg(target_os = "android")]
            let semaphore_fd = ash::khr::external_semaphore_fd::Device::new(instance, device);

            let device = device_guard.into_inner();
            let (entry, instance, debug) = inst.into_parts();
            let context = Arc::new(Context {
                eds,
                pipelines: Mutex::new(pipeline::Pipelines::default()),
                heap,
                cache_prefix: adapter.cache_prefix,
                health_dir: options.cache_dir.clone(),
                degraded,
                crashed,
                push,
                dynamic,
                sync2,
                timeline,
                #[cfg(target_os = "android")]
                ahb,
                #[cfg(target_os = "android")]
                ahb_ubwc,
                #[cfg(target_os = "android")]
                semaphore_fd,
                #[cfg(target_os = "android")]
                video_format,
                _entry: entry,
                instance,
                debug,
                physical,
                device,
                family: adapter.family,
                caps,
                memory,
                atom: l.non_coherent_atom_size,
                allocator: Mutex::new(Some(allocator)),
                lost: AtomicBool::new(false),
                next: AtomicU64::new(1),
                open: AtomicU64::new(0),
                retired: Mutex::new(VecDeque::new()),
                passes: Mutex::new(HashMap::new()),
                samplers: Mutex::new(HashMap::new()),
                rings: Mutex::new(Vec::new()),
                api: adapter.api,
                anisotropy: base.sampler_anisotropy != 0,
                max_anisotropy: l.max_sampler_anisotropy,
                mirror_clamp: plan.use_mirror_clamp,
            });
            let device = crate::Device {
                inner: Device(context),
            };
            let queue = Queue::new(device.clone())?;
            Ok((device, crate::Queue { inner: queue }))
        }
    }
}

unsafe fn query_khr_8x8_f16(
    entry: &ash::Entry,
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
) -> bool {
    let loader = ash::khr::cooperative_matrix::Instance::new(entry, instance);
    let Ok(shapes) =
        (unsafe { loader.get_physical_device_cooperative_matrix_properties(physical) })
    else {
        return false;
    };
    shapes.iter().any(|s| {
        s.m_size == 8
            && s.n_size == 8
            && s.a_type == vk::ComponentTypeKHR::FLOAT16
            && s.b_type == vk::ComponentTypeKHR::FLOAT16
            && s.c_type == vk::ComponentTypeKHR::FLOAT16
            && s.result_type == vk::ComponentTypeKHR::FLOAT16
    })
}

unsafe fn query_nv_8x8_f16(
    entry: &ash::Entry,
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
) -> bool {
    let loader = ash::nv::cooperative_matrix::Instance::new(entry, instance);
    let mut count = 0;
    let first = unsafe {
        (loader
            .fp()
            .get_physical_device_cooperative_matrix_properties_nv)(
            physical,
            &mut count,
            std::ptr::null_mut(),
        )
    };
    if first.result().is_err() || count == 0 {
        return false;
    }
    let mut shapes = vec![vk::CooperativeMatrixPropertiesNV::default(); count as usize];
    let second = unsafe {
        (loader
            .fp()
            .get_physical_device_cooperative_matrix_properties_nv)(
            physical,
            &mut count,
            shapes.as_mut_ptr(),
        )
    };
    if second.result().is_err() {
        return false;
    }
    shapes.iter().any(|s| {
        s.m_size == 8
            && s.n_size == 8
            && s.a_type == vk::ComponentTypeNV::FLOAT16
            && s.b_type == vk::ComponentTypeNV::FLOAT16
            && s.c_type == vk::ComponentTypeNV::FLOAT16
            && s.d_type == vk::ComponentTypeNV::FLOAT16
    })
}

/// The bin size of one full-HD RGBA8 color target, the common display case;
/// without `VK_QCOM_tile_properties`, the render area granularity. Damage
/// tiles align to it, so partial passes keep the optimal render areas the
/// specification describes (Mali drivers lose pixels beside unaligned ones).
unsafe fn query_tile(
    instance: &ash::Instance,
    device: &ash::Device,
    qcom: bool,
) -> Result<(u32, u32)> {
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
        if !qcom {
            let granularity = device.get_render_area_granularity(pass);
            device.destroy_render_pass(pass, None);
            return Ok((granularity.width, granularity.height));
        }
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

/// Startup graphics canary: exercises shader compilation, graphics pipeline
/// creation, active render-pass / dynamic-rendering path, active binding tier
/// (bindless / push descriptors / descriptor sets), extended dynamic state,
/// barrier transitions, completion signaling, and RGBA8 readback.
pub(super) fn canary_render(device: &crate::Device, queue: &mut crate::Queue) -> Result<bool> {
    const SHADER: &str = "
struct Tint { color: vec4<f32> }
@group(0) @binding(0) var<uniform> tint: Tint;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(2) @binding(0) var samp: sampler;
@group(3) @binding(0) var<storage, read> verts: array<vec4<f32>>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    return verts[i];
}
@fragment fn fs() -> @location(0) vec4<f32> {
    return textureSampleLevel(tex, samp, vec2<f32>(0.5, 0.5), 0.0) * tint.color;
}
";
    let size = Extent {
        width: 2,
        height: 2,
        depth_or_layers: 1,
    };
    let area = Rect {
        x: 0,
        y: 0,
        w: 2,
        h: 2,
    };
    let sampled = device.create_image(&ImageDesc {
        dimension: Dimension::D2,
        size,
        format: Format::Rgba8Unorm,
        mips: 1,
        samples: 1,
        usage: Usage::COLOR | Usage::SAMPLED,
        view_formats: &[],
        label: "canary sampled",
    })?;
    let target = device.create_image(&ImageDesc {
        dimension: Dimension::D2,
        size,
        format: Format::Rgba8Unorm,
        mips: 1,
        samples: 1,
        usage: Usage::COLOR | Usage::COPY_SRC,
        view_formats: &[],
        label: "canary target",
    })?;
    let sampler = device.sampler(&SamplerDesc {
        min: Filter::Nearest,
        mag: Filter::Nearest,
        mip: Filter::Nearest,
        address: [Address::Clamp; 3],
        border: BorderColor::TransparentBlack,
        lod_bias: 0,
        lod_min: 0,
        lod_max: 0,
        anisotropy: 1,
        compare: None,
    })?;
    let uniform_size = u64::from(device.caps().limits.uniform_offset_alignment).max(16);
    let uniforms = device.create_buffer(&BufferDesc {
        size: uniform_size,
        usage: BufferUsage::UNIFORM,
        memory: Memory::Upload,
        label: "canary uniform",
    })?;
    let storage_size = u64::from(device.caps().limits.storage_offset_alignment).max(48);
    let vertices = device.create_buffer(&BufferDesc {
        size: storage_size,
        usage: BufferUsage::STORAGE,
        memory: Memory::Upload,
        label: "canary vertices",
    })?;
    // SAFETY: newly allocated Upload buffers have no GPU readers yet.
    unsafe {
        let tint_bytes: Vec<u8> = [1.0f32, 1.0, 0.0, 1.0]
            .into_iter()
            .flat_map(f32::to_ne_bytes)
            .collect();
        (&mut *uniforms.mapped().unwrap().as_ptr())[..16].copy_from_slice(&tint_bytes);
        let vert_bytes: Vec<u8> = [
            -1.0f32, -1.0, 0.0, 1.0, 3.0, -1.0, 0.0, 1.0, -1.0, 3.0, 0.0, 1.0,
        ]
        .into_iter()
        .flat_map(f32::to_ne_bytes)
        .collect();
        (&mut *vertices.mapped().unwrap().as_ptr())[..48].copy_from_slice(&vert_bytes);
    }
    let readback = device.create_buffer(&BufferDesc {
        size: 16,
        usage: BufferUsage::COPY_DST,
        memory: Memory::Readback,
        label: "canary readback",
    })?;
    let shader = device.create_shader(ShaderDesc {
        source: Source::Wgsl(SHADER),
        identity: 0xca4a_7901,
        label: "canary render",
    })?;
    let pipeline = device.create_render_pipeline(
        &RenderPipelineDesc {
            vertex: shader.entry("vs", Stage::Vertex)?,
            fragment: Some(shader.entry("fs", Stage::Fragment)?),
            bindings: vec![
                Binding {
                    slot: 0,
                    kind: BindingKind::UniformBuffer,
                    stages: Stages::FRAGMENT,
                },
                Binding {
                    slot: 0,
                    kind: BindingKind::SampledImage {
                        dimension: Dimension::D2,
                        depth: false,
                        multisampled: false,
                    },
                    stages: Stages::FRAGMENT,
                },
                Binding {
                    slot: 0,
                    kind: BindingKind::Sampler { comparison: false },
                    stages: Stages::FRAGMENT,
                },
                Binding {
                    slot: 0,
                    kind: BindingKind::StorageBuffer { writable: false },
                    stages: Stages::VERTEX,
                },
            ],
            primitive: Primitive {
                topology: Topology::Triangles,
                cull: Cull::None,
                front_ccw: false,
                wireframe: false,
                unclipped_depth: false,
            },
            colors: [ColorTarget {
                format: Format::Rgba8Unorm,
                blend: None,
                write_mask: 0xf,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            samples: 1,
            alpha_to_coverage: false,
            label: "canary render",
        },
        None,
    )?;

    let mut encoder = queue.encoder("canary render");
    drop(
        encoder.render_pass(&RenderPassDesc {
            colors: [ColorAttachment {
                view: sampled.view(),
                load: Load::Clear([0.0, 1.0, 1.0, 1.0]),
                store: Store::Store,
                resolve: None,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            area,
            timestamps: None,
            label: "canary init sampled",
        }),
    );
    {
        let mut pass = encoder.render_pass(&RenderPassDesc {
            colors: [ColorAttachment {
                view: target.view(),
                load: Load::Clear([1.0, 0.0, 0.0, 1.0]),
                store: Store::Store,
                resolve: None,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            area,
            timestamps: None,
            label: "canary draw",
        });
        pass.set_pipeline(&pipeline);
        pass.bind(0, Bind::Uniform(uniforms.slice(0..16)));
        pass.bind(0, Bind::Sampled(sampled.view()));
        pass.bind(0, Bind::Sampler(&sampler));
        pass.bind(0, Bind::StorageBuffer(vertices.slice(0..48)));
        pass.draw(0..3, 0..1);
    }
    encoder.copy_image_to_buffer(
        ImageCopy {
            image: &target,
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
        return Err(Error::DeviceLost("graphics canary timed out".into()));
    }
    // SAFETY: the submission that wrote `readback` has completed.
    let out = unsafe { &readback.mapped().unwrap().as_ref()[..16] };
    Ok(out
        .as_chunks::<4>()
        .0
        .iter()
        .all(|px| *px == [0, 255, 0, 255]))
}

/// Startup compute canary: exercises compute pipeline compilation (including
/// `SHADER_F16` arithmetic when advertised), storage buffer binding, dispatch,
/// and readback.
pub(super) fn canary_compute(device: &crate::Device, queue: &mut crate::Queue) -> Result<bool> {
    const SHADER_F32: &str = "
@group(3) @binding(0) var<storage, read> inp: array<u32>;
@group(3) @binding(1) var<storage, read_write> out: array<u32>;
@compute @workgroup_size(1)
fn main() {
    out[0] = inp[0] + 21u;
}
";
    const SHADER_F16: &str = "
enable f16;
@group(3) @binding(0) var<storage, read> inp: array<u32>;
@group(3) @binding(1) var<storage, read_write> out: array<u32>;
@compute @workgroup_size(1)
fn main() {
    let scale = f16(3.0) * f16(7.0);
    out[0] = inp[0] + u32(f32(scale));
}
";
    let use_f16 = device.caps().features.contains(Features::SHADER_F16);
    let input = device.create_buffer(&BufferDesc {
        size: 16,
        usage: BufferUsage::STORAGE,
        memory: Memory::Upload,
        label: "canary compute in",
    })?;
    // SAFETY: newly allocated Upload buffer has no GPU readers yet.
    unsafe {
        (&mut *input.mapped().unwrap().as_ptr())[..4].copy_from_slice(&21u32.to_le_bytes());
    }
    let output = device.create_buffer(&BufferDesc {
        size: 16,
        usage: BufferUsage::STORAGE | BufferUsage::COPY_SRC,
        memory: Memory::Device,
        label: "canary compute out",
    })?;
    let readback = device.create_buffer(&BufferDesc {
        size: 16,
        usage: BufferUsage::COPY_DST,
        memory: Memory::Readback,
        label: "canary compute readback",
    })?;
    let shader = device.create_shader(ShaderDesc {
        source: Source::Wgsl(if use_f16 { SHADER_F16 } else { SHADER_F32 }),
        identity: if use_f16 { 0xca4a_7903 } else { 0xca4a_7902 },
        label: "canary compute",
    })?;
    let pipeline = device.create_compute_pipeline(
        &ComputePipelineDesc {
            entry: shader.entry("main", Stage::Compute)?,
            bindings: vec![
                Binding {
                    slot: 0,
                    kind: BindingKind::StorageBuffer { writable: false },
                    stages: Stages::COMPUTE,
                },
                Binding {
                    slot: 1,
                    kind: BindingKind::StorageBuffer { writable: true },
                    stages: Stages::COMPUTE,
                },
            ],
            label: "canary compute",
        },
        None,
    )?;
    let mut encoder = queue.encoder("canary compute");
    {
        let mut pass = encoder.compute_pass(&ComputePassDesc {
            timestamps: None,
            label: "canary compute",
        });
        pass.set_pipeline(&pipeline);
        pass.bind(0, Bind::StorageBuffer(input.slice(0..16)));
        pass.bind(1, Bind::StorageBuffer(output.slice(0..16)));
        pass.dispatch(1, 1, 1);
    }
    encoder.copy_buffer(output.slice(0..16), &readback, 0);
    let token = queue.submit(encoder)?;
    if !queue.wait(token, std::time::Duration::from_secs(5))? {
        return Err(Error::DeviceLost("compute canary timed out".into()));
    }
    // SAFETY: the submission that wrote `readback` has completed.
    let bytes = unsafe { &readback.mapped().unwrap().as_ref()[..4] };
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()) == 42)
}

#[cfg(target_os = "android")]
fn canary_present(device: &crate::Device, queue: &mut crate::Queue) -> Result<bool> {
    let caps = device.caps();
    if !caps.features.contains(Features::PRESENTABLE_BUFFERS)
        || !caps
            .format(present::FORMAT)
            .contains(FormatCaps::PRESENTABLE)
    {
        return Ok(true);
    }
    let mut pool = device.create_present_pool(&PoolDesc {
        color: None,
        width: 8,
        height: 8,
        format: present::FORMAT,
        count: 1,
        storage: false,
        label: "canary present",
    })?;
    let Some(mut image) = pool.acquire(queue)? else {
        return Ok(false);
    };
    let size = Extent {
        width: 8,
        height: 8,
        depth_or_layers: 1,
    };
    let readback = device.create_buffer(&BufferDesc {
        size: 8 * 8 * 4,
        usage: BufferUsage::COPY_DST,
        memory: Memory::Readback,
        label: "canary present readback",
    })?;
    let mut encoder = queue.encoder("canary present");
    drop(
        encoder.render_pass(&RenderPassDesc {
            colors: [ColorAttachment {
                view: image.image().view(),
                load: Load::Clear([0.0, 1.0, 0.0, 1.0]),
                store: Store::Store,
                resolve: None,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            area: Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 8,
            },
            timestamps: None,
            label: "canary present clear",
        }),
    );
    encoder.copy_image_to_buffer(
        ImageCopy {
            image: image.image(),
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
    let token = queue.submit_present(encoder, &mut [&mut image])?;
    let fence = image.acquire_fence();
    image.released(fence);
    if !queue.wait(token, std::time::Duration::from_secs(5))? {
        return Err(Error::DeviceLost("present canary timed out".into()));
    }
    // SAFETY: the submission that wrote `readback` has completed.
    let bytes = unsafe { &readback.mapped().unwrap().as_ref()[..4] };
    Ok(bytes == [0, 255, 0, 255])
}
