use super::resource::{ImageCore, ImageState};
use super::*;
use arrayvec::ArrayVec;
use ash::vk::Handle;

/// What a submission owns until it completes. Objects it uses need no
/// reference: their destruction waits for it (see [`Retired`]).
#[derive(Default)]
pub(super) struct Resources {
    pub(super) spans: Vec<Span>,
    pub(super) descriptors: Option<binding::DescriptorArena>,
}
pub(crate) struct Encoder {
    pub(super) shared: Arc<queue::Shared>,
    pub(super) command: vk::CommandBuffer,
    pub(super) resources: Resources,
    pub(super) error: Option<Error>,
    pub(super) submitted: bool,
    /// No work recorded since the last memory barrier.
    pub(super) synced: bool,
    originals: Vec<(Arc<ImageCore>, Vec<ImageState>)>,
    pub(super) commands: Vec<vk::CommandBuffer>,
}
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct PassKey {
    attachments: Vec<AttachmentKey>,
    width: u32,
    height: u32,
    layers: u32,
}
#[derive(Clone, PartialEq, Eq, Hash)]
struct AttachmentKey {
    view: u64,
    format: i32,
    samples: u32,
    load: i32,
    store: i32,
    stencil_load: i32,
    stencil_store: i32,
    layout: i32,
}
impl PassKey {
    pub(super) fn uses(&self, view: vk::ImageView) -> bool {
        self.attachments.iter().any(|a| a.view == view.as_raw())
    }
}
/// Destroyed with the first of its views to retire.
pub(super) struct LegacyPass {
    pub(super) pass: vk::RenderPass,
    pub(super) framebuffer: vk::Framebuffer,
}

impl Encoder {
    pub(super) fn descriptor_set(
        &mut self,
        layout: vk::DescriptorSetLayout,
        key: &[binding::Bound],
    ) -> Result<(vk::DescriptorSet, bool)> {
        if self.resources.descriptors.is_none() {
            let mut arena = self
                .shared
                .descriptors
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_default();
            arena.reset(&self.shared.context)?;
            self.resources.descriptors = Some(arena);
        }
        self.resources
            .descriptors
            .as_mut()
            .unwrap()
            .get(&self.shared.context, layout, key)
    }
    pub(super) fn new(shared: Arc<queue::Shared>) -> Self {
        shared.context.open.fetch_add(1, Ordering::AcqRel);
        let mut encoder = Self {
            shared,
            command: vk::CommandBuffer::null(),
            resources: Resources::default(),
            error: None,
            submitted: false,
            synced: false,
            originals: Vec::new(),
            commands: Vec::new(),
        };
        if let Err(e) = encoder.allocate_command() {
            encoder.error = Some(e);
        }
        encoder
    }
    fn allocate_command(&mut self) -> Result<()> {
        let context = &self.shared.context;
        let mut state = self.shared.state.lock().unwrap();
        self.command = match state.commands.pop() {
            Some(command) => {
                context.result(unsafe {
                    context
                        .device
                        .reset_command_buffer(command, vk::CommandBufferResetFlags::empty())
                })?;
                command
            }
            None => context.result(unsafe {
                context.device.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.shared.pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
            })?[0],
        };
        context.result(unsafe {
            context.device.begin_command_buffer(
                self.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        })
    }
    /// False after a driver failure; later commands are dropped and submit
    /// reports it.
    pub(super) fn ready(&self) -> bool {
        self.error.is_none()
    }
    /// Orders all earlier work, including earlier submissions, before what
    /// follows. Skipped when nothing was recorded since the last one.
    pub(super) fn memory_barrier(&mut self) {
        if !self.ready() || self.synced {
            return;
        }
        self.synced = true;
        let context = &self.shared.context;
        unsafe {
            if context.caps.modern.contains(Modern::SYNCHRONIZATION2) {
                let barriers = [vk::MemoryBarrier2::default()
                    .src_stage_mask(
                        vk::PipelineStageFlags2::ALL_COMMANDS | vk::PipelineStageFlags2::HOST,
                    )
                    .src_access_mask(vk::AccessFlags2::MEMORY_WRITE | vk::AccessFlags2::HOST_WRITE)
                    .dst_stage_mask(
                        vk::PipelineStageFlags2::ALL_COMMANDS | vk::PipelineStageFlags2::HOST,
                    )
                    .dst_access_mask(
                        vk::AccessFlags2::MEMORY_READ
                            | vk::AccessFlags2::MEMORY_WRITE
                            | vk::AccessFlags2::HOST_READ,
                    )];
                let info = vk::DependencyInfo::default().memory_barriers(&barriers);
                if context.api >= vk::API_VERSION_1_3 {
                    context.device.cmd_pipeline_barrier2(self.command, &info);
                } else {
                    context.sync2.cmd_pipeline_barrier2(self.command, &info);
                }
            } else {
                let barriers = [vk::MemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::MEMORY_WRITE | vk::AccessFlags::HOST_WRITE)
                    .dst_access_mask(
                        vk::AccessFlags::MEMORY_READ
                            | vk::AccessFlags::MEMORY_WRITE
                            | vk::AccessFlags::HOST_READ,
                    )];
                context.device.cmd_pipeline_barrier(
                    self.command,
                    vk::PipelineStageFlags::ALL_COMMANDS | vk::PipelineStageFlags::HOST,
                    vk::PipelineStageFlags::ALL_COMMANDS | vk::PipelineStageFlags::HOST,
                    vk::DependencyFlags::empty(),
                    &barriers,
                    &[],
                    &[],
                );
            }
        }
    }
    fn transition(
        &mut self,
        image: &Image,
        mips: std::ops::Range<u32>,
        layers: std::ops::Range<u32>,
        layout: vk::ImageLayout,
        access: vk::AccessFlags2,
        stage: vk::PipelineStageFlags2,
    ) {
        if !self.ready() {
            return;
        }
        let mut states = image.core.states.lock().unwrap();
        if !self
            .originals
            .iter()
            .any(|(core, _)| Arc::ptr_eq(core, &image.core))
        {
            self.originals.push((image.core.clone(), states.clone()));
        }
        let context = &self.shared.context;
        for layer in layers {
            for mip in mips.clone() {
                let index = (layer * image.desc().mips + mip) as usize;
                let old = states[index];
                #[cfg(target_os = "android")]
                let (src_family, dst_family) = old.queue_families(layout, context.family);
                #[cfg(not(target_os = "android"))]
                let (src_family, dst_family) = (vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED);
                let range = vk::ImageSubresourceRange::default()
                    .aspect_mask(formats::aspect(image.desc().format))
                    .base_mip_level(mip)
                    .level_count(1)
                    .base_array_layer(layer)
                    .layer_count(1);
                unsafe {
                    if context.caps.modern.contains(Modern::SYNCHRONIZATION2) {
                        let barriers = [vk::ImageMemoryBarrier2::default()
                            .src_stage_mask(old.stage)
                            .src_access_mask(old.access)
                            .dst_stage_mask(stage)
                            .dst_access_mask(access)
                            .old_layout(old.layout)
                            .new_layout(layout)
                            .src_queue_family_index(src_family)
                            .dst_queue_family_index(dst_family)
                            .image(image.core.handle)
                            .subresource_range(range)];
                        let info = vk::DependencyInfo::default().image_memory_barriers(&barriers);
                        if context.api >= vk::API_VERSION_1_3 {
                            context.device.cmd_pipeline_barrier2(self.command, &info);
                        } else {
                            context.sync2.cmd_pipeline_barrier2(self.command, &info);
                        }
                    } else {
                        let barriers = [vk::ImageMemoryBarrier::default()
                            .src_access_mask(vk::AccessFlags::from_raw(old.access.as_raw() as u32))
                            .dst_access_mask(vk::AccessFlags::from_raw(access.as_raw() as u32))
                            .old_layout(old.layout)
                            .new_layout(layout)
                            .src_queue_family_index(src_family)
                            .dst_queue_family_index(dst_family)
                            .image(image.core.handle)
                            .subresource_range(range)];
                        context.device.cmd_pipeline_barrier(
                            self.command,
                            vk::PipelineStageFlags::from_raw(old.stage.as_raw() as u32),
                            vk::PipelineStageFlags::from_raw(stage.as_raw() as u32),
                            vk::DependencyFlags::empty(),
                            &[],
                            &[],
                            &barriers,
                        );
                    }
                }
                states[index] = ImageState {
                    layout,
                    access,
                    stage,
                    #[cfg(target_os = "android")]
                    family: if layout == vk::ImageLayout::PRESENT_SRC_KHR {
                        vk::QUEUE_FAMILY_FOREIGN_EXT
                    } else {
                        context.family
                    },
                };
            }
        }
    }
    #[cfg(target_os = "android")]
    pub(super) fn release(&mut self, image: &Image) {
        self.transition(
            image,
            0..1,
            0..1,
            vk::ImageLayout::PRESENT_SRC_KHR,
            vk::AccessFlags2::empty(),
            vk::PipelineStageFlags2::BOTTOM_OF_PIPE,
        );
    }
    pub(super) fn view_transition(
        &mut self,
        view: &ImageView,
        layout: vk::ImageLayout,
        access: vk::AccessFlags2,
        stage: vk::PipelineStageFlags2,
    ) {
        self.transition(
            &view.0.image.inner,
            view.0.desc.mips.clone(),
            view.0.desc.layers.clone(),
            layout,
            access,
            stage,
        );
    }
    fn timestamp_start(
        &mut self,
        timestamps: Option<(&crate::queue::QuerySet, u32)>,
    ) -> Option<(vk::QueryPool, u32)> {
        let (set, index) = timestamps?;
        if !self.ready() {
            return None;
        }
        let pool = set.inner.0.handle;
        unsafe {
            let device = &self.shared.context.device;
            device.cmd_reset_query_pool(self.command, pool, index, 2);
            device.cmd_write_timestamp(
                self.command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                pool,
                index,
            );
        }
        Some((pool, index + 1))
    }
    fn timestamp_end(&mut self, timestamp: Option<(vk::QueryPool, u32)>) {
        if let (true, Some((pool, index))) = (self.ready(), timestamp) {
            unsafe {
                self.shared.context.device.cmd_write_timestamp(
                    self.command,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    pool,
                    index,
                );
            }
        }
    }
    pub(crate) fn compute_pass<'e>(
        &'e mut self,
        desc: &ComputePassDesc<'_>,
    ) -> crate::ComputePass<'e> {
        self.memory_barrier();
        let timestamp = self.timestamp_start(desc.timestamps);
        crate::ComputePass {
            inner: ComputePass {
                encoder: self,
                timestamp,
                pipeline: None,
                bindings: binding::Bindings::default(),
            },
        }
    }
    pub(crate) fn render_pass<'e>(
        &'e mut self,
        desc: &RenderPassDesc<'_>,
    ) -> crate::RenderPass<'e> {
        self.memory_barrier();
        let timestamp = self.timestamp_start(desc.timestamps);
        let modern = self.shared.context.caps.modern;
        let none = modern.contains(Modern::LOAD_STORE_OP_NONE);
        // Colors, their resolves and depth: no allocation per pass.
        let mut attachments = ArrayVec::<_, 17>::new();
        let mut views = ArrayVec::<_, 17>::new();
        let mut clears = ArrayVec::<_, 17>::new();
        let mut colors = ArrayVec::<_, 8>::new();
        let mut resolves = ArrayVec::<_, 8>::new();
        let mut dynamic_colors = ArrayVec::<_, 8>::new();
        let mut depth_info = vk::RenderingAttachmentInfo::default();
        let mut stencil_info = vk::RenderingAttachmentInfo::default();
        let mut depth_ref = None;
        let mut dimensions = Extent {
            width: desc.area.x + desc.area.w,
            height: desc.area.y + desc.area.h,
            depth_or_layers: 1,
        };
        for color in &desc.colors {
            let view = &color.view.inner;
            dimensions = view.size();
            self.view_transition(
                view,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::AccessFlags2::COLOR_ATTACHMENT_READ | vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            );
            let clear = match color.load {
                Load::Clear(value) => vk::ClearValue {
                    color: if view.0.format == Format::R32Uint {
                        vk::ClearColorValue {
                            uint32: value.map(|v| v as u32),
                        }
                    } else {
                        vk::ClearColorValue {
                            float32: value.map(|v| v as f32),
                        }
                    },
                },
                _ => vk::ClearValue::default(),
            };
            let index = attachments.len() as u32;
            attachments.push(attachment(
                view,
                load_op(&color.load, none),
                store_op(color.store, none),
                vk::AttachmentLoadOp::DONT_CARE,
                vk::AttachmentStoreOp::DONT_CARE,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            ));
            views.push(view.0.handle);
            clears.push(clear);
            colors.push(vk::AttachmentReference {
                attachment: index,
                layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            });
            let mut dynamic = vk::RenderingAttachmentInfo::default()
                .image_view(view.0.handle)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(load_op(&color.load, none))
                .store_op(store_op(color.store, none))
                .clear_value(clear);
            if let Some(resolve) = color.resolve {
                let view = &resolve.inner;
                self.view_transition(
                    view,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                );
                dynamic = dynamic
                    .resolve_mode(if matches!(view.0.format, Format::R32Uint) {
                        vk::ResolveModeFlags::SAMPLE_ZERO
                    } else {
                        vk::ResolveModeFlags::AVERAGE
                    })
                    .resolve_image_view(view.0.handle)
                    .resolve_image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
                resolves.push(vk::AttachmentReference {
                    attachment: attachments.len() as u32,
                    layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                });
                attachments.push(attachment(
                    view,
                    vk::AttachmentLoadOp::DONT_CARE,
                    vk::AttachmentStoreOp::STORE,
                    vk::AttachmentLoadOp::DONT_CARE,
                    vk::AttachmentStoreOp::DONT_CARE,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                ));
                views.push(view.0.handle);
                clears.push(vk::ClearValue::default());
            } else {
                resolves.push(vk::AttachmentReference {
                    attachment: vk::ATTACHMENT_UNUSED,
                    layout: vk::ImageLayout::UNDEFINED,
                });
            }
            dynamic_colors.push(dynamic);
        }
        if let Some(depth) = &desc.depth_stencil {
            let view = &depth.view.inner;
            dimensions = view.size();
            let layout = if depth.read_only {
                vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL
            } else {
                vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL
            };
            let access = vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_READ
                | if depth.read_only {
                    vk::AccessFlags2::empty()
                } else {
                    vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE
                };
            self.view_transition(
                view,
                layout,
                access,
                vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS
                    | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS,
            );
            let clear = vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: match depth.depth.0 {
                        Load::Clear(v) => v,
                        _ => 0.0,
                    },
                    stencil: match depth.stencil.0 {
                        Load::Clear(v) => v,
                        _ => 0,
                    },
                },
            };
            depth_info = vk::RenderingAttachmentInfo::default()
                .image_view(view.0.handle)
                .image_layout(layout)
                .load_op(load_op(&depth.depth.0, none))
                .store_op(store_op(depth.depth.1, none))
                .clear_value(clear);
            stencil_info = vk::RenderingAttachmentInfo::default()
                .image_view(view.0.handle)
                .image_layout(layout)
                .load_op(load_op(&depth.stencil.0, none))
                .store_op(store_op(depth.stencil.1, none))
                .clear_value(clear);
            depth_ref = Some(vk::AttachmentReference {
                attachment: attachments.len() as u32,
                layout,
            });
            attachments.push(attachment(
                view,
                load_op(&depth.depth.0, none),
                store_op(depth.depth.1, none),
                load_op(&depth.stencil.0, none),
                store_op(depth.stencil.1, none),
                layout,
            ));
            views.push(view.0.handle);
            clears.push(clear);
        }
        // Keep the preamble open until the pass ends: its occlusion queries
        // are discovered while recording and must be reset before the pass.
        let preamble = self.command;
        if self.ready() {
            self.commands.push(preamble);
            self.command = vk::CommandBuffer::null();
            if let Err(error) = self.allocate_command() {
                self.error = Some(error);
            }
        }
        let dynamic = modern.contains(Modern::DYNAMIC_RENDERING);
        let mut begun = false;
        if self.ready() {
            if dynamic {
                let mut info = vk::RenderingInfo::default()
                    .render_area(rect(desc.area))
                    .layer_count(dimensions.depth_or_layers)
                    .color_attachments(&dynamic_colors);
                if let Some(depth) = &desc.depth_stencil {
                    if depth.view.inner.0.format.has_depth() {
                        info = info.depth_attachment(&depth_info);
                    }
                    if depth.view.inner.0.format.has_stencil() {
                        info = info.stencil_attachment(&stencil_info);
                    }
                }
                unsafe {
                    if self.shared.context.api >= vk::API_VERSION_1_3 {
                        self.shared
                            .context
                            .device
                            .cmd_begin_rendering(self.command, &info);
                    } else {
                        self.shared
                            .context
                            .dynamic
                            .cmd_begin_rendering(self.command, &info);
                    }
                }
                begun = true;
            } else {
                match self.legacy_pass(
                    &attachments,
                    &views,
                    &colors,
                    &resolves,
                    depth_ref.as_ref(),
                    dimensions,
                ) {
                    Ok((pass, framebuffer)) => {
                        let info = vk::RenderPassBeginInfo::default()
                            .render_pass(pass)
                            .framebuffer(framebuffer)
                            .render_area(rect(desc.area))
                            .clear_values(&clears);
                        unsafe {
                            self.shared.context.device.cmd_begin_render_pass(
                                self.command,
                                &info,
                                vk::SubpassContents::INLINE,
                            );
                        }
                        begun = true;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
        let mut pass = RenderPass {
            encoder: self,
            area: desc.area,
            dynamic,
            begun,
            timestamp,
            occlusion: None,
            preamble,
            resets: Vec::new(),
            pipeline: None,
            state: None,
            bindings: binding::Bindings::default(),
            index: None,
        };
        pass.set_viewport(
            desc.area.x as f32,
            desc.area.y as f32,
            desc.area.w as f32,
            desc.area.h as f32,
            0.0,
            1.0,
        );
        pass.set_scissor(desc.area);
        pass.set_stencil_reference(0);
        pass.set_blend_constant([0.0; 4]);
        pass.set_depth_bias(0.0, 0.0, 0.0);
        crate::RenderPass { inner: pass }
    }
    fn legacy_pass(
        &mut self,
        attachments: &[vk::AttachmentDescription],
        views: &[vk::ImageView],
        colors: &[vk::AttachmentReference],
        resolves: &[vk::AttachmentReference],
        depth: Option<&vk::AttachmentReference>,
        size: Extent,
    ) -> Result<(vk::RenderPass, vk::Framebuffer)> {
        let key = PassKey {
            attachments: attachments
                .iter()
                .zip(views)
                .map(|(a, v)| AttachmentKey {
                    view: v.as_raw(),
                    format: a.format.as_raw(),
                    samples: a.samples.as_raw(),
                    load: a.load_op.as_raw(),
                    store: a.store_op.as_raw(),
                    stencil_load: a.stencil_load_op.as_raw(),
                    stencil_store: a.stencil_store_op.as_raw(),
                    layout: a.initial_layout.as_raw(),
                })
                .collect(),
            width: size.width,
            height: size.height,
            layers: size.depth_or_layers,
        };
        let mut cache = self.shared.context.passes.lock().unwrap();
        if let Some(pass) = cache.get(&key) {
            return Ok((pass.pass, pass.framebuffer));
        }
        let mut subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(colors)
            .resolve_attachments(resolves);
        if let Some(depth) = depth {
            subpass = subpass.depth_stencil_attachment(depth);
        }
        let subpasses = [subpass];
        let context = &self.shared.context;
        let pass = context.result(unsafe {
            context.device.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(attachments)
                    .subpasses(&subpasses),
                None,
            )
        })?;
        let framebuffer = match context.result(unsafe {
            context.device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(pass)
                    .attachments(views)
                    .width(size.width)
                    .height(size.height)
                    .layers(size.depth_or_layers),
                None,
            )
        }) {
            Ok(f) => f,
            Err(e) => {
                unsafe {
                    context.device.destroy_render_pass(pass, None);
                }
                return Err(e);
            }
        };
        cache.insert(key, LegacyPass { pass, framebuffer });
        Ok((pass, framebuffer))
    }
    pub(crate) fn copy_buffer_to_image(
        &mut self,
        src: BufferCopy<'_>,
        dst: ImageCopy<'_>,
        size: Extent,
    ) {
        self.memory_barrier();
        self.copy_transition(
            dst,
            size,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::AccessFlags2::TRANSFER_WRITE,
        );
        if !self.ready() {
            return;
        }
        let regions = buffer_image_copies(src, dst, size);
        self.synced = false;
        unsafe {
            self.shared.context.device.cmd_copy_buffer_to_image(
                self.command,
                src.buffer.inner.0.handle,
                dst.image.inner.core.handle,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &regions,
            );
        }
    }
    pub(crate) fn copy_image_to_buffer(
        &mut self,
        src: ImageCopy<'_>,
        dst: BufferCopy<'_>,
        size: Extent,
    ) {
        self.memory_barrier();
        self.copy_transition(
            src,
            size,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::AccessFlags2::TRANSFER_READ,
        );
        if !self.ready() {
            return;
        }
        let regions = buffer_image_copies(dst, src, size);
        self.synced = false;
        unsafe {
            self.shared.context.device.cmd_copy_image_to_buffer(
                self.command,
                src.image.inner.core.handle,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst.buffer.inner.0.handle,
                &regions,
            );
        }
    }
    fn copy_transition(
        &mut self,
        copy: ImageCopy<'_>,
        size: Extent,
        layout: vk::ImageLayout,
        access: vk::AccessFlags2,
    ) {
        let count = if copy.image.desc().dimension == Dimension::D3 {
            1
        } else {
            size.depth_or_layers
        };
        self.transition(
            &copy.image.inner,
            copy.mip..copy.mip + 1,
            copy.layer..copy.layer + count,
            layout,
            access,
            vk::PipelineStageFlags2::TRANSFER,
        );
    }
    pub(crate) fn copy_image(&mut self, src: ImageCopy<'_>, dst: ImageCopy<'_>, size: Extent) {
        self.memory_barrier();
        self.copy_transition(
            src,
            size,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::AccessFlags2::TRANSFER_READ,
        );
        self.copy_transition(
            dst,
            size,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::AccessFlags2::TRANSFER_WRITE,
        );
        if !self.ready() {
            return;
        }
        let regions: Vec<_> = (0..src.image.desc().format.planes())
            .map(|plane| {
                let scale = if plane == 0 { 1 } else { 2 };
                let mut source = subresource(src, size);
                let mut destination = subresource(dst, size);
                if src.image.desc().format.planes() > 1 {
                    let aspect = if plane == 0 {
                        vk::ImageAspectFlags::PLANE_0
                    } else {
                        vk::ImageAspectFlags::PLANE_1
                    };
                    source.aspect_mask = aspect;
                    destination.aspect_mask = aspect;
                }
                let mut extent = copy_extent(src, size);
                extent.width /= scale;
                extent.height /= scale;
                vk::ImageCopy::default()
                    .src_subresource(source)
                    .src_offset(offset((
                        src.origin.0 / scale,
                        src.origin.1 / scale,
                        src.origin.2,
                    )))
                    .dst_subresource(destination)
                    .dst_offset(offset((
                        dst.origin.0 / scale,
                        dst.origin.1 / scale,
                        dst.origin.2,
                    )))
                    .extent(extent)
            })
            .collect();
        self.synced = false;
        unsafe {
            self.shared.context.device.cmd_copy_image(
                self.command,
                src.image.inner.core.handle,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst.image.inner.core.handle,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &regions,
            );
        }
    }
    pub(crate) fn copy_buffer(
        &mut self,
        src: BufferSlice<'_>,
        dst: &crate::Buffer,
        dst_offset: u64,
    ) {
        self.memory_barrier();
        if !self.ready() {
            return;
        }
        self.synced = false;
        unsafe {
            self.shared.context.device.cmd_copy_buffer(
                self.command,
                src.buffer.inner.0.handle,
                dst.inner.0.handle,
                &[vk::BufferCopy {
                    src_offset: src.offset,
                    dst_offset,
                    size: src.size,
                }],
            );
        }
    }
    pub(crate) fn fill_buffer(&mut self, dst: BufferSlice<'_>, value: u32) {
        self.memory_barrier();
        if !self.ready() {
            return;
        }
        self.synced = false;
        unsafe {
            self.shared.context.device.cmd_fill_buffer(
                self.command,
                dst.buffer.inner.0.handle,
                dst.offset,
                dst.size,
                value,
            );
        }
    }
    pub(crate) fn retain(&mut self, span: crate::Span) {
        self.resources.spans.push(span.inner);
    }
    pub(crate) fn resolve_queries(
        &mut self,
        set: &crate::queue::QuerySet,
        range: std::ops::Range<u32>,
        dst: &crate::Buffer,
        offset: u64,
    ) {
        self.memory_barrier();
        if !self.ready() {
            return;
        }
        self.synced = false;
        unsafe {
            self.shared.context.device.cmd_copy_query_pool_results(
                self.command,
                set.inner.0.handle,
                range.start,
                range.end - range.start,
                dst.inner.0.handle,
                offset,
                8,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
            );
        }
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        if !self.submitted {
            self.shared.context.open.fetch_sub(1, Ordering::Release);
            if let Some(arena) = self.resources.descriptors.take() {
                self.shared.descriptors.lock().unwrap().push(arena);
            }
            for (image, states) in self.originals.drain(..) {
                *image.states.lock().unwrap() = states;
            }
            if self.command != vk::CommandBuffer::null() {
                self.commands.push(self.command);
            }
            if !self.commands.is_empty() {
                let _state = self.shared.state.lock().unwrap();
                unsafe {
                    self.shared
                        .context
                        .device
                        .free_command_buffers(self.shared.pool, &self.commands);
                }
            }
        }
    }
}
fn load_op<T>(load: &Load<T>, none: bool) -> vk::AttachmentLoadOp {
    match load {
        Load::Load => vk::AttachmentLoadOp::LOAD,
        Load::Clear(_) => vk::AttachmentLoadOp::CLEAR,
        Load::DontCare => vk::AttachmentLoadOp::DONT_CARE,
        Load::None => {
            if none {
                vk::AttachmentLoadOp::NONE_EXT
            } else {
                vk::AttachmentLoadOp::LOAD
            }
        }
    }
}
fn store_op(store: Store, none: bool) -> vk::AttachmentStoreOp {
    match store {
        Store::Store => vk::AttachmentStoreOp::STORE,
        Store::DontCare => vk::AttachmentStoreOp::DONT_CARE,
        Store::None => {
            if none {
                vk::AttachmentStoreOp::NONE
            } else {
                vk::AttachmentStoreOp::STORE
            }
        }
    }
}
fn attachment(
    view: &ImageView,
    load: vk::AttachmentLoadOp,
    store: vk::AttachmentStoreOp,
    stencil_load: vk::AttachmentLoadOp,
    stencil_store: vk::AttachmentStoreOp,
    layout: vk::ImageLayout,
) -> vk::AttachmentDescription {
    vk::AttachmentDescription::default()
        .format(formats::vk(view.0.format))
        .samples(vk::SampleCountFlags::from_raw(view.image().desc().samples))
        .load_op(load)
        .store_op(store)
        .stencil_load_op(stencil_load)
        .stencil_store_op(stencil_store)
        .initial_layout(layout)
        .final_layout(layout)
}
fn rect(rect: Rect) -> vk::Rect2D {
    vk::Rect2D {
        offset: vk::Offset2D {
            x: rect.x as i32,
            y: rect.y as i32,
        },
        extent: vk::Extent2D {
            width: rect.w,
            height: rect.h,
        },
    }
}
fn offset(origin: (u32, u32, u32)) -> vk::Offset3D {
    vk::Offset3D {
        x: origin.0 as i32,
        y: origin.1 as i32,
        z: origin.2 as i32,
    }
}
fn copy_extent(copy: ImageCopy<'_>, size: Extent) -> vk::Extent3D {
    vk::Extent3D {
        width: size.width,
        height: size.height,
        depth: if copy.image.desc().dimension == Dimension::D3 {
            size.depth_or_layers
        } else {
            1
        },
    }
}
fn subresource(copy: ImageCopy<'_>, size: Extent) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers::default()
        .aspect_mask(formats::aspect(copy.image.desc().format))
        .mip_level(copy.mip)
        .base_array_layer(copy.layer)
        .layer_count(if copy.image.desc().dimension == Dimension::D3 {
            1
        } else {
            size.depth_or_layers
        })
}
fn buffer_image_copies(
    buffer: BufferCopy<'_>,
    image: ImageCopy<'_>,
    size: Extent,
) -> Vec<vk::BufferImageCopy> {
    let (block, bytes) = formats::block(image.image.desc().format);
    if image.image.desc().format.planes() == 1 {
        return vec![
            vk::BufferImageCopy::default()
                .buffer_offset(buffer.offset)
                .buffer_row_length(buffer.row_pitch / bytes * block)
                .buffer_image_height(buffer.rows)
                .image_subresource(subresource(image, size))
                .image_offset(offset(image.origin))
                .image_extent(copy_extent(image, size)),
        ];
    }
    let pitch = u64::from(if buffer.row_pitch == 0 {
        size.width * bytes
    } else {
        buffer.row_pitch
    });
    let rows = if buffer.rows == 0 {
        size.height
    } else {
        buffer.rows
    };
    let luma = pitch * u64::from(rows);
    let stride = luma + pitch * u64::from(rows / 2);
    let mut regions = Vec::new();
    for layer in 0..size.depth_or_layers {
        for plane in 0..2 {
            let scale = if plane == 0 { 1 } else { 2 };
            regions.push(
                vk::BufferImageCopy::default()
                    .buffer_offset(
                        buffer.offset
                            + u64::from(layer) * stride
                            + if plane == 0 { 0 } else { luma },
                    )
                    .buffer_row_length(buffer.row_pitch / (bytes * scale))
                    .buffer_image_height(buffer.rows / scale)
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(if plane == 0 {
                                vk::ImageAspectFlags::PLANE_0
                            } else {
                                vk::ImageAspectFlags::PLANE_1
                            })
                            .mip_level(image.mip)
                            .base_array_layer(image.layer + layer)
                            .layer_count(1),
                    )
                    .image_offset(offset((
                        image.origin.0 / scale,
                        image.origin.1 / scale,
                        image.origin.2,
                    )))
                    .image_extent(vk::Extent3D {
                        width: size.width / scale,
                        height: size.height / scale,
                        depth: 1,
                    }),
            );
        }
    }
    regions
}

pub(crate) struct RenderPass<'e> {
    pub(super) encoder: &'e mut Encoder,
    area: Rect,
    dynamic: bool,
    begun: bool,
    timestamp: Option<(vk::QueryPool, u32)>,
    occlusion: Option<(vk::QueryPool, u32)>,
    preamble: vk::CommandBuffer,
    resets: Vec<(vk::QueryPool, u32)>,
    pipeline: Option<Arc<pipeline::Compiled>>,
    state: Option<pipeline::Dynamic>,
    bindings: binding::Bindings,
    index: Option<(vk::Buffer, u64, IndexFormat)>,
}
impl Drop for RenderPass<'_> {
    fn drop(&mut self) {
        if self.begun && self.encoder.ready() {
            unsafe {
                let context = &self.encoder.shared.context;
                if self.dynamic {
                    if context.api >= vk::API_VERSION_1_3 {
                        context.device.cmd_end_rendering(self.encoder.command);
                    } else {
                        context.dynamic.cmd_end_rendering(self.encoder.command);
                    }
                } else {
                    context.device.cmd_end_render_pass(self.encoder.command);
                }
            }
        }
        self.encoder.timestamp_end(self.timestamp);
        if self.encoder.ready() {
            let command = self.encoder.command;
            self.encoder.command = self.preamble;
            for &(pool, index) in &self.resets {
                unsafe {
                    self.encoder.shared.context.device.cmd_reset_query_pool(
                        self.preamble,
                        pool,
                        index,
                        1,
                    );
                }
            }
            if let Err(error) = self.encoder.shared.context.result(unsafe {
                self.encoder
                    .shared
                    .context
                    .device
                    .end_command_buffer(self.preamble)
            }) {
                self.encoder.error = Some(error);
            }
            self.encoder.command = command;
        }
    }
}
impl RenderPass<'_> {
    pub(crate) fn set_pipeline(&mut self, pipeline: &crate::RenderPipeline) {
        if !self.encoder.ready() {
            return;
        }
        let pipeline = &pipeline.inner;
        let context = &self.encoder.shared.context;
        let command = self.encoder.command;
        if self
            .pipeline
            .as_ref()
            .is_none_or(|p| p.handle != pipeline.compiled.handle)
        {
            unsafe {
                context.device.cmd_bind_pipeline(
                    command,
                    vk::PipelineBindPoint::GRAPHICS,
                    pipeline.compiled.handle,
                );
            }
            self.bindings.pipeline(&pipeline.compiled);
            self.pipeline = Some(pipeline.compiled.clone());
        }
        if self.state != Some(pipeline.dynamic) {
            pipeline.dynamic.emit(context, command, self.state.as_ref());
            self.state = Some(pipeline.dynamic);
        }
    }
    pub(crate) fn bind(&mut self, slot: u32, resource: Bind<'_>) {
        self.bindings.bind(
            self.encoder,
            self.pipeline.as_deref(),
            slot,
            resource,
            Some(self.preamble),
        );
    }
    #[cfg(target_os = "android")]
    pub(crate) fn bind_video(&mut self, image: &crate::PresentImage) {
        if !self.encoder.ready() {
            return;
        }
        let (pool, set) = image.video_set();
        let pipeline = self
            .pipeline
            .as_ref()
            .expect("video pipeline before bind_video");
        let (index, expected) = pipeline.video.as_ref().expect("converted video pipeline");
        debug_assert!(
            Arc::ptr_eq(pool, expected),
            "video buffer belongs to another pool"
        );
        let command = self.encoder.command;
        self.encoder.command = self.preamble;
        self.encoder.view_transition(
            &image.image().view().inner,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::AccessFlags2::SHADER_READ,
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
        );
        self.encoder.command = command;
        unsafe {
            self.encoder.shared.context.device.cmd_bind_descriptor_sets(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline.layout,
                *index,
                &[set],
                &[],
            );
        }
    }
    pub(crate) fn set_index_buffer(&mut self, buffer: BufferSlice<'_>, format: IndexFormat) {
        if !self.encoder.ready() {
            return;
        }
        let value = (buffer.buffer.inner.0.handle, buffer.offset, format);
        if self.index == Some(value) {
            return;
        }
        unsafe {
            self.encoder.shared.context.device.cmd_bind_index_buffer(
                self.encoder.command,
                value.0,
                value.1,
                match format {
                    IndexFormat::U16 => vk::IndexType::UINT16,
                    IndexFormat::U32 => vk::IndexType::UINT32,
                },
            );
        }
        self.index = Some(value);
    }
    fn prepare(&mut self) -> bool {
        let pipeline = self.pipeline.as_deref();
        debug_assert!(
            pipeline.is_some() || !self.encoder.ready(),
            "draw without a pipeline"
        );
        let ready = pipeline.is_some_and(|p| {
            self.bindings
                .flush(self.encoder, p, vk::PipelineBindPoint::GRAPHICS)
        });
        self.encoder.synced &= !ready;
        ready
    }
    pub(crate) fn draw(&mut self, vertices: std::ops::Range<u32>, instances: std::ops::Range<u32>) {
        if self.prepare() {
            unsafe {
                self.encoder.shared.context.device.cmd_draw(
                    self.encoder.command,
                    vertices.end - vertices.start,
                    instances.end - instances.start,
                    vertices.start,
                    instances.start,
                );
            }
        }
    }
    pub(crate) fn draw_indexed(
        &mut self,
        indices: std::ops::Range<u32>,
        base_vertex: i32,
        instances: std::ops::Range<u32>,
    ) {
        if self.prepare() {
            unsafe {
                self.encoder.shared.context.device.cmd_draw_indexed(
                    self.encoder.command,
                    indices.end - indices.start,
                    instances.end - instances.start,
                    indices.start,
                    base_vertex,
                    instances.start,
                );
            }
        }
    }
    pub(crate) fn draw_indirect(&mut self, args: BufferSlice<'_>, indexed: bool) {
        if !self.prepare() {
            return;
        }
        unsafe {
            let device = &self.encoder.shared.context.device;
            if indexed {
                device.cmd_draw_indexed_indirect(
                    self.encoder.command,
                    args.buffer.inner.0.handle,
                    args.offset,
                    1,
                    20,
                );
            } else {
                device.cmd_draw_indirect(
                    self.encoder.command,
                    args.buffer.inner.0.handle,
                    args.offset,
                    1,
                    16,
                );
            }
        }
    }
    pub(crate) fn set_viewport(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        min_depth: f32,
        max_depth: f32,
    ) {
        if !self.begun || !self.encoder.ready() {
            return;
        }
        unsafe {
            self.encoder.shared.context.device.cmd_set_viewport(
                self.encoder.command,
                0,
                &[vk::Viewport {
                    x,
                    y,
                    width: w,
                    height: h,
                    min_depth,
                    max_depth,
                }],
            );
        }
    }
    pub(crate) fn set_scissor(&mut self, value: Rect) {
        if !self.begun || !self.encoder.ready() {
            return;
        }
        let x = value.x.max(self.area.x).min(self.area.x + self.area.w);
        let y = value.y.max(self.area.y).min(self.area.y + self.area.h);
        let right = value
            .x
            .saturating_add(value.w)
            .min(self.area.x + self.area.w)
            .max(x);
        let bottom = value
            .y
            .saturating_add(value.h)
            .min(self.area.y + self.area.h)
            .max(y);
        unsafe {
            self.encoder.shared.context.device.cmd_set_scissor(
                self.encoder.command,
                0,
                &[rect(Rect {
                    x,
                    y,
                    w: right - x,
                    h: bottom - y,
                })],
            );
        }
    }
    pub(crate) fn set_stencil_reference(&mut self, value: u32) {
        if self.begun && self.encoder.ready() {
            unsafe {
                self.encoder
                    .shared
                    .context
                    .device
                    .cmd_set_stencil_reference(
                        self.encoder.command,
                        vk::StencilFaceFlags::FRONT_AND_BACK,
                        value,
                    );
            }
        }
    }
    pub(crate) fn set_blend_constant(&mut self, rgba: [f32; 4]) {
        if self.begun && self.encoder.ready() {
            unsafe {
                self.encoder
                    .shared
                    .context
                    .device
                    .cmd_set_blend_constants(self.encoder.command, &rgba);
            }
        }
    }
    pub(crate) fn set_depth_bias(&mut self, constant: f32, slope: f32, clamp: f32) {
        if self.begun && self.encoder.ready() {
            unsafe {
                self.encoder.shared.context.device.cmd_set_depth_bias(
                    self.encoder.command,
                    constant,
                    clamp,
                    slope,
                );
            }
        }
    }
    pub(crate) fn begin_occlusion(&mut self, set: &crate::queue::QuerySet, index: u32) {
        if !self.begun || !self.encoder.ready() {
            return;
        }
        self.resets.push((set.inner.0.handle, index));
        unsafe {
            self.encoder.shared.context.device.cmd_begin_query(
                self.encoder.command,
                set.inner.0.handle,
                index,
                vk::QueryControlFlags::PRECISE,
            );
        }
        self.occlusion = Some((set.inner.0.handle, index));
    }
    pub(crate) fn end_occlusion(&mut self) {
        if self.encoder.ready()
            && let Some((pool, index)) = self.occlusion.take()
        {
            unsafe {
                self.encoder
                    .shared
                    .context
                    .device
                    .cmd_end_query(self.encoder.command, pool, index);
            }
        }
    }
}
pub(crate) struct ComputePass<'e> {
    pub(super) encoder: &'e mut Encoder,
    timestamp: Option<(vk::QueryPool, u32)>,
    pipeline: Option<Arc<pipeline::Compiled>>,
    bindings: binding::Bindings,
}
impl Drop for ComputePass<'_> {
    fn drop(&mut self) {
        self.encoder.timestamp_end(self.timestamp);
    }
}
impl ComputePass<'_> {
    pub(crate) fn set_pipeline(&mut self, pipeline: &crate::ComputePipeline) {
        let compiled = &pipeline.inner.compiled;
        if !self.encoder.ready()
            || self
                .pipeline
                .as_ref()
                .is_some_and(|p| p.handle == compiled.handle)
        {
            return;
        }
        unsafe {
            self.encoder.shared.context.device.cmd_bind_pipeline(
                self.encoder.command,
                vk::PipelineBindPoint::COMPUTE,
                compiled.handle,
            );
        }
        self.bindings.pipeline(compiled);
        self.pipeline = Some(compiled.clone());
    }
    pub(crate) fn bind(&mut self, slot: u32, resource: Bind<'_>) {
        self.bindings
            .bind(self.encoder, self.pipeline.as_deref(), slot, resource, None);
    }
    fn prepare(&mut self) -> bool {
        let pipeline = self.pipeline.as_deref();
        debug_assert!(
            pipeline.is_some() || !self.encoder.ready(),
            "dispatch without a pipeline"
        );
        let ready = pipeline.is_some_and(|p| {
            self.bindings
                .flush(self.encoder, p, vk::PipelineBindPoint::COMPUTE)
        });
        self.encoder.synced &= !ready;
        ready
    }
    pub(crate) fn dispatch(&mut self, x: u32, y: u32, z: u32) {
        self.encoder.memory_barrier();
        if self.prepare() {
            unsafe {
                self.encoder
                    .shared
                    .context
                    .device
                    .cmd_dispatch(self.encoder.command, x, y, z);
            }
        }
    }
    pub(crate) fn dispatch_indirect(&mut self, args: BufferSlice<'_>) {
        self.encoder.memory_barrier();
        if self.prepare() {
            unsafe {
                self.encoder.shared.context.device.cmd_dispatch_indirect(
                    self.encoder.command,
                    args.buffer.inner.0.handle,
                    args.offset,
                );
            }
        }
    }
}
