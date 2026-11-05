use super::*;
use ash::vk::Handle;
use std::hash::{Hash, Hasher};

#[derive(Clone, PartialEq, Eq, Hash)]
struct EntryKey(u64, u32, Stage);
fn entry_key(entry: &Entry) -> EntryKey {
    EntryKey(entry.shader.identity, entry.index, entry.stage)
}
#[derive(Clone, PartialEq, Eq, Hash)]
struct RenderKey {
    vertex: EntryKey,
    fragment: Option<EntryKey>,
    bindings: Vec<Binding>,
    primitive: Primitive,
    colors: Vec<ColorTarget>,
    depth: Option<DepthStencil>,
    samples: u32,
    alpha: bool,
    video: Option<u64>,
}
impl RenderKey {
    fn new(desc: &RenderPipelineDesc, modern: Modern) -> Self {
        let mut key = Self {
            vertex: entry_key(&desc.vertex),
            fragment: desc.fragment.as_ref().map(entry_key),
            bindings: desc.bindings.clone(),
            primitive: desc.primitive,
            colors: desc.colors.to_vec(),
            depth: desc.depth_stencil,
            samples: desc.samples,
            alpha: desc.alpha_to_coverage,
            video: None,
        };
        key.bindings
            .sort_by_key(|b| (binding::group(b.kind), b.slot));
        if modern.contains(Modern::EXTENDED_DYNAMIC_STATE) {
            key.primitive.cull = Cull::None;
            key.primitive.front_ccw = false;
            // EDS1/2 permits list/strip changes within a topology class. Crossing
            // classes requires EDS3's dynamicPrimitiveTopologyUnrestricted.
            key.primitive.topology = match key.primitive.topology {
                Topology::LineStrip => Topology::Lines,
                Topology::TriangleStrip => Topology::Triangles,
                t => t,
            };
            if let Some(depth) = &mut key.depth {
                depth.depth_compare = Compare::Always;
                depth.depth_write = false;
                depth.depth_bias = false;
                depth.stencil = None;
            }
        }
        if let Some(depth) = &mut key.depth {
            depth.stencil_read_mask = 0;
            depth.stencil_write_mask = 0;
        }
        key
    }
}
#[derive(Clone, PartialEq, Eq, Hash)]
struct ComputeKey(EntryKey, Vec<Binding>);
#[derive(Default)]
pub(super) struct Pipelines {
    render: HashMap<RenderKey, Arc<Compiled>>,
    compute: HashMap<ComputeKey, Arc<Compiled>>,
    passes: HashMap<(Vec<Format>, Option<Format>, u32), vk::RenderPass>,
}
/// A driver pipeline and what binding needs from it, resolved once. Lives
/// in the device's cache until the device drops.
pub(super) struct Compiled {
    pub(super) handle: vk::Pipeline,
    pub(super) layout: vk::PipelineLayout,
    pub(super) set_layout: vk::DescriptorSetLayout,
    /// Sorted by `(group, slot)`.
    pub(super) bindings: Vec<Binding>,
    pub(super) push: binding::PushLayout,
    /// On heap tiers, atomic storage requires classic sets for the whole pipeline.
    pub(super) classic: bool,
    /// Bindless tier: uniform slots, one bit each.
    pub(super) uniforms: u64,
    #[cfg(target_os = "android")]
    pub(super) video: Option<(u32, Arc<VideoPool>)>,
}
impl Pipelines {
    pub(super) fn destroy(&mut self, device: &ash::Device) {
        unsafe {
            for compiled in self.render.values().chain(self.compute.values()) {
                device.destroy_pipeline(compiled.handle, None);
                device.destroy_pipeline_layout(compiled.layout, None);
                if compiled.set_layout != vk::DescriptorSetLayout::null() {
                    device.destroy_descriptor_set_layout(compiled.set_layout, None);
                }
            }
            for (_, pass) in self.passes.drain() {
                device.destroy_render_pass(pass, None);
            }
        }
        self.render.clear();
        self.compute.clear();
    }
    fn compatible_pass(
        &mut self,
        context: &Context,
        desc: &RenderPipelineDesc,
    ) -> Result<vk::RenderPass> {
        let key = (
            desc.colors.iter().map(|c| c.format).collect::<Vec<_>>(),
            desc.depth_stencil.map(|d| d.format),
            desc.samples,
        );
        if let Some(&pass) = self.passes.get(&key) {
            return Ok(pass);
        }
        let attachments: Vec<_> = key
            .0
            .iter()
            .copied()
            .chain(key.1)
            .map(|format| {
                vk::AttachmentDescription::default()
                    .format(formats::vk(format))
                    .samples(vk::SampleCountFlags::from_raw(desc.samples))
                    .load_op(vk::AttachmentLoadOp::LOAD)
                    .store_op(vk::AttachmentStoreOp::STORE)
                    .stencil_load_op(vk::AttachmentLoadOp::LOAD)
                    .stencil_store_op(vk::AttachmentStoreOp::STORE)
                    .initial_layout(vk::ImageLayout::GENERAL)
                    .final_layout(vk::ImageLayout::GENERAL)
            })
            .collect();
        let colors: Vec<_> = (0..desc.colors.len())
            .map(|i| vk::AttachmentReference {
                attachment: i as u32,
                layout: vk::ImageLayout::GENERAL,
            })
            .collect();
        let depth = vk::AttachmentReference {
            attachment: desc.colors.len() as u32,
            layout: vk::ImageLayout::GENERAL,
        };
        let mut subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&colors);
        if key.1.is_some() {
            subpass = subpass.depth_stencil_attachment(&depth);
        }
        let pass = context.result(unsafe {
            context.device.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(&attachments)
                    .subpasses(&[subpass]),
                None,
            )
        })?;
        self.passes.insert(key, pass);
        Ok(pass)
    }
}
#[derive(Clone)]
pub(crate) struct RenderPipeline {
    pub(super) compiled: Arc<Compiled>,
    _context: Arc<Context>,
    pub(super) dynamic: Dynamic,
}
#[derive(Clone)]
pub(crate) struct ComputePipeline {
    _context: Arc<Context>,
    pub(super) compiled: Arc<Compiled>,
}

/// Pipeline state that commands set, resolved at creation so `set_pipeline`
/// emits only what differs from the pass's previous pipeline. Every pipeline
/// of a device declares the same dynamic states, so state carries over.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Dynamic {
    cull: vk::CullModeFlags,
    front: vk::FrontFace,
    topology: vk::PrimitiveTopology,
    depth_test: bool,
    depth_write: bool,
    depth_compare: vk::CompareOp,
    stencil_test: bool,
    depth_bias: bool,
    stencil: [(vk::StencilOp, vk::StencilOp, vk::StencilOp, vk::CompareOp); 2],
    read_mask: u32,
    write_mask: u32,
}
impl Dynamic {
    fn new(desc: &RenderPipelineDesc) -> Self {
        let depth = desc.depth_stencil;
        let face = |f: StencilFace| {
            (
                stencil_op(f.fail),
                stencil_op(f.pass),
                stencil_op(f.depth_fail),
                resource::compare(f.compare),
            )
        };
        let keep = StencilFace {
            compare: Compare::Always,
            fail: StencilOp::Keep,
            depth_fail: StencilOp::Keep,
            pass: StencilOp::Keep,
        };
        let (front, back) = depth.and_then(|d| d.stencil).unwrap_or((keep, keep));
        Self {
            cull: cull(desc.primitive.cull),
            front: winding(desc.primitive.front_ccw),
            topology: topology(desc.primitive.topology),
            depth_test: depth.is_some(),
            depth_write: depth.is_some_and(|d| d.depth_write),
            depth_compare: resource::compare(depth.map_or(Compare::Always, |d| d.depth_compare)),
            stencil_test: depth.is_some_and(|d| d.stencil.is_some()),
            depth_bias: depth.is_some_and(|d| d.depth_bias),
            stencil: [face(front), face(back)],
            read_mask: depth.map_or(255, |d| d.stencil_read_mask as u32),
            write_mask: depth.map_or(255, |d| d.stencil_write_mask as u32),
        }
    }
    pub(super) fn emit(&self, context: &Context, command: vk::CommandBuffer, old: Option<&Self>) {
        macro_rules! changed {
            ($($field:ident),+) => { old.is_none_or(|o| $(o.$field != self.$field)||+) };
        }
        let device = &context.device;
        unsafe {
            if let Some(eds) = &context.eds {
                if old.is_none() {
                    (eds.primitive_restart)(command, vk::FALSE);
                    (eds.rasterizer_discard)(command, vk::FALSE);
                }
                if changed!(cull) {
                    (eds.cull)(command, self.cull);
                }
                if changed!(front) {
                    (eds.front)(command, self.front);
                }
                if changed!(topology) {
                    (eds.topology)(command, self.topology);
                }
                if changed!(depth_test) {
                    (eds.depth_test)(command, self.depth_test.into());
                }
                if changed!(depth_write) {
                    (eds.depth_write)(command, self.depth_write.into());
                }
                if changed!(depth_compare) {
                    (eds.depth_compare)(command, self.depth_compare);
                }
                if changed!(stencil_test) {
                    (eds.stencil_test)(command, self.stencil_test.into());
                }
                if changed!(depth_bias) {
                    (eds.depth_bias)(command, self.depth_bias.into());
                }
                if changed!(stencil) {
                    for (face, (fail, pass, depth_fail, compare)) in [
                        (vk::StencilFaceFlags::FRONT, self.stencil[0]),
                        (vk::StencilFaceFlags::BACK, self.stencil[1]),
                    ] {
                        (eds.stencil_op)(command, face, fail, pass, depth_fail, compare);
                    }
                }
            }
            if changed!(read_mask) {
                device.cmd_set_stencil_compare_mask(
                    command,
                    vk::StencilFaceFlags::FRONT_AND_BACK,
                    self.read_mask,
                );
            }
            if changed!(write_mask) {
                device.cmd_set_stencil_write_mask(
                    command,
                    vk::StencilFaceFlags::FRONT_AND_BACK,
                    self.write_mask,
                );
            }
        }
    }
}

fn layouts(
    context: &Context,
    bindings: &[Binding],
    video: Option<&Arc<VideoPool>>,
    classic: bool,
) -> Result<(vk::PipelineLayout, vk::DescriptorSetLayout)> {
    let (mut sets, own) = if let Some(heap) = context.heap.as_ref().filter(|_| !classic) {
        let count = bindings
            .iter()
            .filter(|b| b.kind == BindingKind::UniformBuffer)
            .map(|b| b.slot + 1)
            .max()
            .unwrap_or(0);
        let mut sets = vec![heap.layout];
        sets.extend(std::iter::repeat_n(heap.uniform_layout, count as usize));
        (sets, vk::DescriptorSetLayout::null())
    } else {
        let native: Vec<_> = bindings
            .iter()
            .map(|b| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(b.slot * 4 + binding::group(b.kind))
                    .descriptor_count(1)
                    .descriptor_type(binding::descriptor(b.kind))
                    .stage_flags(binding::stages(b.stages))
            })
            .collect();
        let flags = if !classic && context.caps.modern.contains(Modern::PUSH_DESCRIPTORS) {
            vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR
        } else {
            vk::DescriptorSetLayoutCreateFlags::empty()
        };
        let set = context.result(unsafe {
            context.device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default()
                    .flags(flags)
                    .bindings(&native),
                None,
            )
        })?;
        (vec![set], set)
    };
    if let Some(video) = video {
        let limit = unsafe {
            context
                .instance
                .get_physical_device_properties(context.physical)
        }
        .limits
        .max_bound_descriptor_sets;
        if sets.len() as u32 == limit {
            if own != vk::DescriptorSetLayout::null() {
                unsafe {
                    context.device.destroy_descriptor_set_layout(own, None);
                }
            }
            return Err(Error::Invalid(
                "video set exceeds descriptor-set limit".into(),
            ));
        }
        sets.push(video.resources.layout);
    }
    let push = [vk::PushConstantRange {
        stage_flags: vk::ShaderStageFlags::ALL,
        offset: 0,
        size: context.caps.limits.push_constant_bytes,
    }];
    let mut info = vk::PipelineLayoutCreateInfo::default().set_layouts(&sets);
    if !classic && context.heap.is_some() {
        info = info.push_constant_ranges(&push);
    }
    match context.result(unsafe { context.device.create_pipeline_layout(&info, None) }) {
        Ok(layout) => Ok((layout, own)),
        Err(e) => {
            if own != vk::DescriptorSetLayout::null() {
                unsafe {
                    context.device.destroy_descriptor_set_layout(own, None);
                }
            }
            Err(e)
        }
    }
}
impl Device {
    pub(crate) fn pipeline_identity(&self, desc: &RenderPipelineDesc) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        RenderKey::new(desc, self.caps().modern).hash(&mut hasher);
        hasher.finish()
    }
    pub(crate) fn create_render_pipeline(
        &self,
        desc: &RenderPipelineDesc,
        cache: Option<&crate::pipeline::PipelineCache>,
    ) -> Result<crate::RenderPipeline> {
        self.render_pipeline(desc, cache, None)
    }
    #[cfg(target_os = "android")]
    pub(crate) fn create_video_pipeline(
        &self,
        desc: &VideoPipelineDesc<'_>,
        cache: Option<&crate::pipeline::PipelineCache>,
    ) -> Result<crate::RenderPipeline> {
        if !Arc::ptr_eq(&self.0, &desc.pool.device.inner.0) {
            return Err(Error::Invalid(
                "video pool belongs to another device".into(),
            ));
        }
        let video = desc
            .pool
            .video
            .as_ref()
            .ok_or_else(|| Error::Unsupported("pool has no converted video sampler".into()))?;
        self.render_pipeline(desc.pipeline, cache, Some(video))
    }
    fn render_pipeline(
        &self,
        desc: &RenderPipelineDesc,
        cache: Option<&crate::pipeline::PipelineCache>,
        video: Option<&Arc<VideoPool>>,
    ) -> Result<crate::RenderPipeline> {
        let mut key = RenderKey::new(desc, self.caps().modern);
        key.video = video.map(|v| v.resources.layout.as_raw());
        let mut pipelines = self.0.pipelines.lock().unwrap();
        let compiled = match pipelines.render.get(&key) {
            Some(compiled) => compiled.clone(),
            None => {
                let mut external = cache.map(|c| c.inner.0.lock().unwrap());
                let cache = match &mut external {
                    Some(cache) => cache.handle(&self.0)?,
                    None => vk::PipelineCache::null(),
                };
                let entries: Vec<&Entry> = std::iter::once(&desc.vertex)
                    .chain(&desc.fragment)
                    .collect();
                let compiled =
                    Arc::new(
                        self.compile(&key.bindings, &entries, video, |layout, stages| {
                            self.compile_render(desc, layout, stages, cache, &mut pipelines)
                        })?,
                    );
                pipelines.render.insert(key, compiled.clone());
                compiled
            }
        };
        Ok(crate::RenderPipeline {
            inner: RenderPipeline {
                _context: self.0.clone(),
                compiled,
                dynamic: Dynamic::new(desc),
            },
        })
    }
    pub(crate) fn create_compute_pipeline(
        &self,
        desc: &ComputePipelineDesc,
        cache: Option<&crate::pipeline::PipelineCache>,
    ) -> Result<crate::ComputePipeline> {
        let mut bindings = desc.bindings.clone();
        bindings.sort_by_key(|b| (binding::group(b.kind), b.slot));
        let key = ComputeKey(entry_key(&desc.entry), bindings);
        let mut pipelines = self.0.pipelines.lock().unwrap();
        let compiled = match pipelines.compute.get(&key) {
            Some(compiled) => compiled.clone(),
            None => {
                let mut external = cache.map(|c| c.inner.0.lock().unwrap());
                let cache = match &mut external {
                    Some(cache) => cache.handle(&self.0)?,
                    None => vk::PipelineCache::null(),
                };
                let compiled =
                    Arc::new(
                        self.compile(&key.1, &[&desc.entry], None, |layout, stages| {
                            let info = vk::ComputePipelineCreateInfo::default()
                                .stage(stages[0])
                                .layout(layout);
                            let created = unsafe {
                                self.0.device.create_compute_pipelines(cache, &[info], None)
                            };
                            self.0
                                .result(created.map(|h| h[0]).map_err(|(_, error)| error))
                        })?,
                    );
                pipelines.compute.insert(key, compiled.clone());
                compiled
            }
        };
        Ok(crate::ComputePipeline {
            inner: ComputePipeline {
                _context: self.0.clone(),
                compiled,
            },
        })
    }
    /// The push layout, shader stages and pipeline layout for `bindings`
    /// (sorted), then the driver pipeline from `build`.
    fn compile(
        &self,
        bindings: &[Binding],
        entries: &[&Entry],
        video: Option<&Arc<VideoPool>>,
        build: impl FnOnce(
            vk::PipelineLayout,
            &[vk::PipelineShaderStageCreateInfo<'_>],
        ) -> Result<vk::Pipeline>,
    ) -> Result<Compiled> {
        let classic =
            self.0.heap.is_some() && entries.iter().any(|e| e.shader.inner.storage_atomics());
        let heap = self.0.heap.is_some() && !classic;
        let push = match self.0.heap.as_ref().filter(|_| heap) {
            Some(heap) => {
                let mut strides = HashMap::new();
                for entry in entries {
                    strides.extend(entry.shader.inner.storage_strides(entry.index));
                }
                let bytes = self.0.caps.limits.push_constant_bytes;
                binding::PushLayout::new(bindings, heap, bytes, &strides)?
            }
            None => binding::PushLayout::default(),
        };
        let video = video.map(|pool| {
            let set = if heap {
                1 + bindings
                    .iter()
                    .filter(|b| b.kind == BindingKind::UniformBuffer)
                    .map(|b| b.slot + 1)
                    .max()
                    .unwrap_or(0)
            } else {
                1
            };
            (set, pool.clone())
        });
        let modules = entries
            .iter()
            .map(|e| {
                e.shader
                    .inner
                    .stage(e, &push, video.as_ref().map(|v| v.0), heap)
            })
            .collect::<Result<Vec<_>>>()?;
        let stages: Vec<_> = modules.iter().map(|m| m.info()).collect();
        let (layout, set_layout) =
            layouts(&self.0, bindings, video.as_ref().map(|v| &v.1), classic)?;
        let handle = match build(layout, &stages) {
            Ok(handle) => handle,
            Err(error) => {
                unsafe {
                    self.0.device.destroy_pipeline_layout(layout, None);
                    if set_layout != vk::DescriptorSetLayout::null() {
                        self.0
                            .device
                            .destroy_descriptor_set_layout(set_layout, None);
                    }
                }
                return Err(error);
            }
        };
        let uniforms = bindings
            .iter()
            .filter(|b| b.kind == BindingKind::UniformBuffer)
            .fold(0, |mask, b| mask | 1 << b.slot);
        Ok(Compiled {
            handle,
            layout,
            set_layout,
            bindings: bindings.to_vec(),
            push,
            classic,
            uniforms,
            #[cfg(target_os = "android")]
            video,
        })
    }
    fn compile_render(
        &self,
        desc: &RenderPipelineDesc,
        layout: vk::PipelineLayout,
        stages: &[vk::PipelineShaderStageCreateInfo<'_>],
        cache: vk::PipelineCache,
        pipelines: &mut Pipelines,
    ) -> Result<vk::Pipeline> {
        let context = &self.0;
        let modern = self.caps().modern;
        let vertex = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(topology(desc.primitive.topology));
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .depth_clamp_enable(desc.primitive.unclipped_depth)
            .polygon_mode(if desc.primitive.wireframe {
                vk::PolygonMode::LINE
            } else {
                vk::PolygonMode::FILL
            })
            .cull_mode(cull(desc.primitive.cull))
            .front_face(winding(desc.primitive.front_ccw))
            .depth_bias_enable(desc.depth_stencil.is_some_and(|d| d.depth_bias))
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::from_raw(desc.samples))
            .alpha_to_coverage_enable(desc.alpha_to_coverage);
        let mut depth = vk::PipelineDepthStencilStateCreateInfo::default();
        if let Some(d) = desc.depth_stencil {
            depth = depth
                .depth_test_enable(true)
                .depth_write_enable(d.depth_write)
                .depth_compare_op(resource::compare(d.depth_compare));
            if let Some((front, back)) = d.stencil {
                depth = depth
                    .stencil_test_enable(true)
                    .front(stencil(front, d))
                    .back(stencil(back, d));
            }
        }
        let attachments: Vec<_> = desc
            .colors
            .iter()
            .map(|c| {
                let mut attachment = vk::PipelineColorBlendAttachmentState::default()
                    .color_write_mask(vk::ColorComponentFlags::from_raw(c.write_mask as u32));
                if let Some((rgb, alpha)) = c.blend {
                    attachment = attachment
                        .blend_enable(true)
                        .src_color_blend_factor(factor(rgb.src))
                        .dst_color_blend_factor(factor(rgb.dst))
                        .color_blend_op(blend_op(rgb.op))
                        .src_alpha_blend_factor(factor(alpha.src))
                        .dst_alpha_blend_factor(factor(alpha.dst))
                        .alpha_blend_op(blend_op(alpha.op));
                }
                attachment
            })
            .collect();
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
        let mut dynamic = vec![
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::BLEND_CONSTANTS,
            vk::DynamicState::STENCIL_REFERENCE,
            vk::DynamicState::STENCIL_COMPARE_MASK,
            vk::DynamicState::STENCIL_WRITE_MASK,
            vk::DynamicState::DEPTH_BIAS,
        ];
        if modern.contains(Modern::EXTENDED_DYNAMIC_STATE) {
            dynamic.extend([
                vk::DynamicState::CULL_MODE,
                vk::DynamicState::FRONT_FACE,
                vk::DynamicState::PRIMITIVE_TOPOLOGY,
                vk::DynamicState::DEPTH_TEST_ENABLE,
                vk::DynamicState::DEPTH_WRITE_ENABLE,
                vk::DynamicState::DEPTH_COMPARE_OP,
                vk::DynamicState::STENCIL_TEST_ENABLE,
                vk::DynamicState::STENCIL_OP,
                vk::DynamicState::DEPTH_BIAS_ENABLE,
                vk::DynamicState::PRIMITIVE_RESTART_ENABLE,
                vk::DynamicState::RASTERIZER_DISCARD_ENABLE,
            ]);
        }
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
        let formats: Vec<_> = desc.colors.iter().map(|c| formats::vk(c.format)).collect();
        let mut rendering =
            vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
        if let Some(d) = desc.depth_stencil {
            if d.format.has_depth() {
                rendering = rendering.depth_attachment_format(formats::vk(d.format));
            }
            if d.format.has_stencil() {
                rendering = rendering.stencil_attachment_format(formats::vk(d.format));
            }
        }
        let mut info = vk::GraphicsPipelineCreateInfo::default()
            .stages(stages)
            .vertex_input_state(&vertex)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(layout);
        if modern.contains(Modern::DYNAMIC_RENDERING) {
            info = info.push_next(&mut rendering);
        } else {
            info = info.render_pass(pipelines.compatible_pass(context, desc)?);
        }
        let created = unsafe {
            context
                .device
                .create_graphics_pipelines(cache, &[info], None)
        };
        context.result(created.map(|h| h[0]).map_err(|(_, error)| error))
    }
}
pub(super) fn topology(t: Topology) -> vk::PrimitiveTopology {
    match t {
        Topology::Points => vk::PrimitiveTopology::POINT_LIST,
        Topology::Lines => vk::PrimitiveTopology::LINE_LIST,
        Topology::LineStrip => vk::PrimitiveTopology::LINE_STRIP,
        Topology::Triangles => vk::PrimitiveTopology::TRIANGLE_LIST,
        Topology::TriangleStrip => vk::PrimitiveTopology::TRIANGLE_STRIP,
    }
}
pub(super) fn cull(c: Cull) -> vk::CullModeFlags {
    match c {
        Cull::None => vk::CullModeFlags::NONE,
        Cull::Front => vk::CullModeFlags::FRONT,
        Cull::Back => vk::CullModeFlags::BACK,
    }
}
pub(super) fn winding(ccw: bool) -> vk::FrontFace {
    if ccw {
        vk::FrontFace::COUNTER_CLOCKWISE
    } else {
        vk::FrontFace::CLOCKWISE
    }
}
fn factor(f: Factor) -> vk::BlendFactor {
    match f {
        Factor::Zero => vk::BlendFactor::ZERO,
        Factor::One => vk::BlendFactor::ONE,
        Factor::Src => vk::BlendFactor::SRC_COLOR,
        Factor::OneMinusSrc => vk::BlendFactor::ONE_MINUS_SRC_COLOR,
        Factor::SrcAlpha => vk::BlendFactor::SRC_ALPHA,
        Factor::OneMinusSrcAlpha => vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        Factor::Dst => vk::BlendFactor::DST_COLOR,
        Factor::OneMinusDst => vk::BlendFactor::ONE_MINUS_DST_COLOR,
        Factor::DstAlpha => vk::BlendFactor::DST_ALPHA,
        Factor::OneMinusDstAlpha => vk::BlendFactor::ONE_MINUS_DST_ALPHA,
        Factor::SrcAlphaSaturated => vk::BlendFactor::SRC_ALPHA_SATURATE,
        Factor::Constant => vk::BlendFactor::CONSTANT_COLOR,
        Factor::OneMinusConstant => vk::BlendFactor::ONE_MINUS_CONSTANT_COLOR,
    }
}
fn blend_op(op: BlendOp) -> vk::BlendOp {
    match op {
        BlendOp::Add => vk::BlendOp::ADD,
        BlendOp::Subtract => vk::BlendOp::SUBTRACT,
        BlendOp::ReverseSubtract => vk::BlendOp::REVERSE_SUBTRACT,
        BlendOp::Min => vk::BlendOp::MIN,
        BlendOp::Max => vk::BlendOp::MAX,
    }
}
pub(super) fn stencil_op(op: StencilOp) -> vk::StencilOp {
    match op {
        StencilOp::Keep => vk::StencilOp::KEEP,
        StencilOp::Zero => vk::StencilOp::ZERO,
        StencilOp::Replace => vk::StencilOp::REPLACE,
        StencilOp::IncrementClamp => vk::StencilOp::INCREMENT_AND_CLAMP,
        StencilOp::DecrementClamp => vk::StencilOp::DECREMENT_AND_CLAMP,
        StencilOp::Invert => vk::StencilOp::INVERT,
        StencilOp::IncrementWrap => vk::StencilOp::INCREMENT_AND_WRAP,
        StencilOp::DecrementWrap => vk::StencilOp::DECREMENT_AND_WRAP,
    }
}
fn stencil(f: StencilFace, d: DepthStencil) -> vk::StencilOpState {
    vk::StencilOpState::default()
        .fail_op(stencil_op(f.fail))
        .pass_op(stencil_op(f.pass))
        .depth_fail_op(stencil_op(f.depth_fail))
        .compare_op(resource::compare(f.compare))
        .compare_mask(d.stencil_read_mask as u32)
        .write_mask(d.stencil_write_mask as u32)
}

#[derive(Clone)]
pub(crate) struct PipelineCache(Arc<Mutex<CacheState>>);
struct CacheState {
    data: Vec<u8>,
    device: Option<(Arc<Context>, vk::PipelineCache)>,
}
impl PipelineCache {
    pub(crate) fn load(data: &[u8]) -> crate::pipeline::PipelineCache {
        crate::pipeline::PipelineCache {
            inner: Self(Arc::new(Mutex::new(CacheState {
                data: data.to_vec(),
                device: None,
            }))),
        }
    }
    pub(crate) fn serialize(&self) -> Result<Vec<u8>> {
        let state = self.0.lock().unwrap();
        let Some((context, handle)) = &state.device else {
            return Ok(state.data.clone());
        };
        let mut data = context.cache_prefix.to_vec();
        data.extend(context.result(unsafe { context.device.get_pipeline_cache_data(*handle) })?);
        Ok(data)
    }
}
fn cache_data<'a>(data: &'a [u8], prefix: &[u8; 28]) -> Option<&'a [u8]> {
    if data.get(..28)? != prefix {
        return None;
    }
    let native = data.get(28..)?;
    let read = |offset| {
        native
            .get(offset..offset + 4)
            .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
    };
    let size = read(0)? as usize;
    if size != 32
        || native.len() < size
        || read(4)? != 1
        || read(8)? != u32::from_le_bytes(prefix[16..20].try_into().unwrap())
        || read(12)? != u32::from_le_bytes(prefix[20..24].try_into().unwrap())
        || native.get(16..32)? != &prefix[..16]
    {
        return None;
    }
    Some(native)
}
impl CacheState {
    fn handle(&mut self, context: &Arc<Context>) -> Result<vk::PipelineCache> {
        if let Some((owner, handle)) = &self.device {
            if Arc::ptr_eq(owner, context) {
                return Ok(*handle);
            }
            return Err(Error::Invalid(
                "pipeline cache belongs to another device".into(),
            ));
        }
        let data = cache_data(&self.data, &context.cache_prefix).unwrap_or(&[]);
        let handle = context.result(unsafe {
            context.device.create_pipeline_cache(
                &vk::PipelineCacheCreateInfo::default().initial_data(data),
                None,
            )
        })?;
        self.data.clear();
        self.device = Some((context.clone(), handle));
        Ok(handle)
    }
}
impl Drop for CacheState {
    fn drop(&mut self) {
        if let Some((context, handle)) = &self.device {
            unsafe {
                context.device.destroy_pipeline_cache(*handle, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: GFX-CACHE-8
    #[test]
    fn cache_prefix_and_native_header_are_checked_before_driver_use() {
        let mut prefix = [7; 28];
        prefix[16..20].copy_from_slice(&17u32.to_le_bytes());
        prefix[20..24].copy_from_slice(&19u32.to_le_bytes());
        let mut data = prefix.to_vec();
        for value in [32u32, 1, 17, 19] {
            data.extend(value.to_le_bytes());
        }
        data.extend(&prefix[..16]);
        assert_eq!(cache_data(&data, &prefix), Some(&data[28..]));
        for end in 0..data.len() {
            assert!(cache_data(&data[..end], &prefix).is_none());
        }
        for index in 0..data.len() {
            let mut corrupt = data.clone();
            corrupt[index] ^= 1;
            assert!(cache_data(&corrupt, &prefix).is_none());
        }
    }
}
