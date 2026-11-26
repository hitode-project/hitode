use super::resource::{ImageCore, ImageState};
use super::*;
use arrayvec::ArrayVec;
use ash::vk::Handle;

const WRITES: vk::AccessFlags2 = vk::AccessFlags2::from_raw(
    vk::AccessFlags2::COLOR_ATTACHMENT_WRITE.as_raw()
        | vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE.as_raw()
        | vk::AccessFlags2::SHADER_WRITE.as_raw()
        | vk::AccessFlags2::SHADER_STORAGE_WRITE.as_raw()
        | vk::AccessFlags2::TRANSFER_WRITE.as_raw()
        | vk::AccessFlags2::MEMORY_WRITE.as_raw(),
);

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
    pub(super) bindings: Option<binding::Bindings>,
    pub(super) error: Option<Error>,
    pub(super) submitted: bool,
    /// No work recorded since the last memory barrier.
    pub(super) synced: bool,
    /// Whether any query operation (timestamps, occlusion, resolve) has been
    /// recorded in `command`.
    has_query_ops: bool,
    /// Each image this encoder touched: its states before, and whether the
    /// encoder writes it.
    originals: Vec<(Arc<ImageCore>, Vec<ImageState>, bool)>,
    pub(super) commands: Vec<vk::CommandBuffer>,
    /// The pool `commands` come from, owned until the submission retires.
    pub(super) pool: Option<Commands>,
}
/// One encoder's command buffers, reset as a whole pool once the GPU is done
/// with them. Resetting single buffers made Adreno free and remap their
/// memory through the kernel for every render pass.
pub(super) struct Commands {
    pool: vk::CommandPool,
    pub(super) buffers: Vec<vk::CommandBuffer>,
    used: usize,
}
impl Commands {
    fn new(context: &Context) -> Result<Self> {
        let pool = context.result(unsafe {
            context.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default().queue_family_index(context.family),
                None,
            )
        })?;
        Ok(Self {
            pool,
            buffers: Vec::new(),
            used: 0,
        })
    }
    fn next(&mut self, context: &Context) -> Result<vk::CommandBuffer> {
        if self.used == self.buffers.len() {
            let buffer = context.result(unsafe {
                context.device.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
            })?[0];
            self.buffers.push(buffer);
        }
        self.used += 1;
        Ok(self.buffers[self.used - 1])
    }
    /// Returns every buffer to the initial state, keeping its memory.
    pub(super) fn reset(&mut self, context: &Context) -> Result<()> {
        self.used = 0;
        context.result(unsafe {
            context
                .device
                .reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty())
        })
    }
    pub(super) fn destroy(self, context: &Context) {
        unsafe { context.device.destroy_command_pool(self.pool, None) };
    }
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
    pub(super) fn new(shared: Arc<queue::Shared>, bindings: binding::Bindings) -> Self {
        shared.context.open.fetch_add(1, Ordering::AcqRel);
        let mut encoder = Self {
            shared,
            command: vk::CommandBuffer::null(),
            resources: Resources::default(),
            bindings: Some(bindings),
            error: None,
            submitted: false,
            synced: false,
            has_query_ops: false,
            originals: Vec::new(),
            commands: Vec::new(),
            pool: None,
        };
        if let Err(e) = encoder.allocate_command() {
            encoder.error = Some(e);
        }
        encoder
    }
    fn allocate_command(&mut self) -> Result<()> {
        let buffer = self.allocate_buffer()?;
        self.command = buffer;
        self.has_query_ops = false;
        Ok(())
    }
    fn allocate_buffer(&mut self) -> Result<vk::CommandBuffer> {
        let context = &self.shared.context;
        if self.pool.is_none() {
            let reused = self.shared.state.lock().unwrap().pools.pop();
            self.pool = Some(match reused {
                Some(pool) => pool,
                None => Commands::new(context)?,
            });
        }
        let buffer = self.pool.as_mut().unwrap().next(context)?;
        context.result(unsafe {
            context.device.begin_command_buffer(
                buffer,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        })?;
        Ok(buffer)
    }
    /// False after a driver failure; later commands are dropped and submit
    /// reports it.
    pub(super) fn ready(&self) -> bool {
        self.error.is_none()
    }
    fn emit_memory_barrier(
        &mut self,
        src_stage: vk::PipelineStageFlags2,
        src_access: vk::AccessFlags2,
        dst_stage: vk::PipelineStageFlags2,
        dst_access: vk::AccessFlags2,
    ) {
        let context = &self.shared.context;
        unsafe {
            if context.caps.modern.contains(Modern::SYNCHRONIZATION2) {
                let barriers = [vk::MemoryBarrier2::default()
                    .src_stage_mask(src_stage)
                    .src_access_mask(src_access)
                    .dst_stage_mask(dst_stage)
                    .dst_access_mask(dst_access)];
                let info = vk::DependencyInfo::default().memory_barriers(&barriers);
                if context.api >= vk::API_VERSION_1_3 {
                    context.device.cmd_pipeline_barrier2(self.command, &info);
                } else {
                    context.sync2.cmd_pipeline_barrier2(self.command, &info);
                }
            } else {
                let barriers = [vk::MemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::from_raw(src_access.as_raw() as u32))
                    .dst_access_mask(vk::AccessFlags::from_raw(dst_access.as_raw() as u32))];
                context.device.cmd_pipeline_barrier(
                    self.command,
                    vk::PipelineStageFlags::from_raw(src_stage.as_raw() as u32),
                    vk::PipelineStageFlags::from_raw(dst_stage.as_raw() as u32),
                    vk::DependencyFlags::empty(),
                    &barriers,
                    &[],
                    &[],
                );
            }
        }
    }
    /// Orders earlier GPU work before what follows. Skipped when nothing was
    /// recorded since the last barrier.
    pub(super) fn memory_barrier(&mut self) {
        if !self.ready() || self.synced {
            return;
        }
        self.synced = true;
        self.emit_memory_barrier(
            vk::PipelineStageFlags2::ALL_COMMANDS,
            vk::AccessFlags2::MEMORY_WRITE,
            vk::PipelineStageFlags2::ALL_COMMANDS,
            vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE,
        );
    }
    /// Orders compute shader writes before subsequent compute or indirect dispatches.
    fn compute_barrier(&mut self) {
        if !self.ready() || self.synced {
            return;
        }
        self.synced = true;
        self.emit_memory_barrier(
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_WRITE,
            vk::PipelineStageFlags2::COMPUTE_SHADER | vk::PipelineStageFlags2::DRAW_INDIRECT,
            vk::AccessFlags2::SHADER_READ
                | vk::AccessFlags2::SHADER_WRITE
                | vk::AccessFlags2::INDIRECT_COMMAND_READ,
        );
    }
    /// Orders all GPU writes in this submission before host reads and later submissions.
    pub(super) fn submit_barrier(&mut self) {
        if !self.ready() {
            return;
        }
        self.synced = true;
        self.emit_memory_barrier(
            vk::PipelineStageFlags2::ALL_COMMANDS,
            vk::AccessFlags2::MEMORY_WRITE,
            vk::PipelineStageFlags2::ALL_COMMANDS | vk::PipelineStageFlags2::HOST,
            vk::AccessFlags2::MEMORY_READ
                | vk::AccessFlags2::MEMORY_WRITE
                | vk::AccessFlags2::HOST_READ,
        );
    }
    #[allow(clippy::too_many_arguments)]
    fn push_transition(
        &mut self,
        barriers: &mut Vec<vk::ImageMemoryBarrier2<'static>>,
        core: &Arc<ImageCore>,
        mips: std::ops::Range<u32>,
        layers: std::ops::Range<u32>,
        layout: vk::ImageLayout,
        access: vk::AccessFlags2,
        stage: vk::PipelineStageFlags2,
    ) {
        if !self.ready() {
            return;
        }
        let mut states = core.states.lock().unwrap();
        let written = access.intersects(WRITES);
        match self
            .originals
            .iter_mut()
            .find(|(c, ..)| Arc::ptr_eq(c, core))
        {
            Some(original) => original.2 |= written,
            None => self.originals.push((core.clone(), states.clone(), written)),
        }
        #[cfg(target_os = "android")]
        let family = self.shared.context.family;
        let aspect = formats::aspect(core.desc.format);
        for layer in layers {
            for mip in mips.clone() {
                let index = (layer * core.desc.mips + mip) as usize;
                let old = states[index];
                #[cfg(target_os = "android")]
                let (src_family, dst_family) = old.queue_families(layout, family);
                #[cfg(not(target_os = "android"))]
                let (src_family, dst_family) = (vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED);
                if old.layout == layout
                    && src_family == dst_family
                    && !old.access.intersects(WRITES)
                    && !written
                {
                    states[index].stage |= stage;
                    states[index].access |= access;
                    continue;
                }
                let range = vk::ImageSubresourceRange::default()
                    .aspect_mask(aspect)
                    .base_mip_level(mip)
                    .level_count(1)
                    .base_array_layer(layer)
                    .layer_count(1);
                barriers.push(
                    vk::ImageMemoryBarrier2::default()
                        .src_stage_mask(old.stage)
                        .src_access_mask(old.access)
                        .dst_stage_mask(stage)
                        .dst_access_mask(access)
                        .old_layout(old.layout)
                        .new_layout(layout)
                        .src_queue_family_index(src_family)
                        .dst_queue_family_index(dst_family)
                        .image(core.handle)
                        .subresource_range(range),
                );
                states[index] = ImageState {
                    layout,
                    access,
                    stage,
                    #[cfg(target_os = "android")]
                    family: if layout == vk::ImageLayout::PRESENT_SRC_KHR {
                        vk::QUEUE_FAMILY_FOREIGN_EXT
                    } else {
                        family
                    },
                };
            }
        }
    }
    fn flush_image_barriers(&mut self, barriers: &[vk::ImageMemoryBarrier2<'static>]) {
        if barriers.is_empty() || !self.ready() {
            return;
        }
        let context = &self.shared.context;
        unsafe {
            if context.caps.modern.contains(Modern::SYNCHRONIZATION2) {
                let info = vk::DependencyInfo::default().image_memory_barriers(barriers);
                if context.api >= vk::API_VERSION_1_3 {
                    context.device.cmd_pipeline_barrier2(self.command, &info);
                } else {
                    context.sync2.cmd_pipeline_barrier2(self.command, &info);
                }
            } else {
                let mut src_stage = vk::PipelineStageFlags::empty();
                let mut dst_stage = vk::PipelineStageFlags::empty();
                let legacy: Vec<_> = barriers
                    .iter()
                    .map(|b| {
                        src_stage |=
                            vk::PipelineStageFlags::from_raw(b.src_stage_mask.as_raw() as u32);
                        dst_stage |=
                            vk::PipelineStageFlags::from_raw(b.dst_stage_mask.as_raw() as u32);
                        vk::ImageMemoryBarrier::default()
                            .src_access_mask(vk::AccessFlags::from_raw(
                                b.src_access_mask.as_raw() as u32
                            ))
                            .dst_access_mask(vk::AccessFlags::from_raw(
                                b.dst_access_mask.as_raw() as u32
                            ))
                            .old_layout(b.old_layout)
                            .new_layout(b.new_layout)
                            .src_queue_family_index(b.src_queue_family_index)
                            .dst_queue_family_index(b.dst_queue_family_index)
                            .image(b.image)
                            .subresource_range(b.subresource_range)
                    })
                    .collect();
                context.device.cmd_pipeline_barrier(
                    self.command,
                    src_stage,
                    dst_stage,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &legacy,
                );
            }
        }
    }
    fn transition(
        &mut self,
        core: &Arc<ImageCore>,
        mips: std::ops::Range<u32>,
        layers: std::ops::Range<u32>,
        layout: vk::ImageLayout,
        access: vk::AccessFlags2,
        stage: vk::PipelineStageFlags2,
    ) {
        let mut barriers = Vec::new();
        self.push_transition(&mut barriers, core, mips, layers, layout, access, stage);
        self.flush_image_barriers(&barriers);
    }
    /// True if any image touched earlier in this encoder (excluding single-subresource
    /// `attachments`) could need a mid-pass transition (either currently in a non-read-only
    /// layout, or created with `Usage::STORAGE` so a later pass can transition it to `GENERAL`).
    fn has_pending_image_transitions(&self, attachments: &[vk::Image]) -> bool {
        self.originals.iter().any(|(core, _, _)| {
            if core.desc.mips == 1
                && (core.desc.dimension == Dimension::D3 || core.desc.size.depth_or_layers == 1)
                && attachments.contains(&core.handle)
            {
                return false;
            }
            if core.desc.usage.contains(Usage::STORAGE) {
                return true;
            }
            core.states.lock().unwrap().iter().any(|s| {
                s.layout != vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
                    && s.layout != vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL
            })
        })
    }
    /// Hand a presentable image to the compositor after this submission.
    #[cfg(target_os = "android")]
    pub(super) fn release(&mut self, core: &Arc<ImageCore>) {
        self.transition(
            core,
            0..1,
            0..1,
            vk::ImageLayout::PRESENT_SRC_KHR,
            vk::AccessFlags2::empty(),
            vk::PipelineStageFlags2::BOTTOM_OF_PIPE,
        );
    }
    /// Return every image this encoder took from the external domain and only
    /// read: the compositor may still show it. Written images stay owned here
    /// unless submitted for presentation.
    #[cfg(target_os = "android")]
    pub(super) fn release_borrowed(&mut self) {
        for i in 0..self.originals.len() {
            let (core, states, written) = &self.originals[i];
            if !written && states[0].family == vk::QUEUE_FAMILY_FOREIGN_EXT {
                let core = core.clone();
                self.release(&core);
            }
        }
    }
    pub(super) fn view_transition(
        &mut self,
        view: &ImageView,
        layout: vk::ImageLayout,
        access: vk::AccessFlags2,
        stage: vk::PipelineStageFlags2,
    ) {
        self.transition(
            &view.0.image.inner.core,
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
        self.has_query_ops = true;
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
        let dynamic = modern.contains(Modern::DYNAMIC_RENDERING);
        let mut barriers = Vec::with_capacity(desc.colors.len() * 2 + 1);
        let mut attachment_handles = ArrayVec::<vk::Image, 17>::new();
        let mut dimensions = Extent {
            width: desc.area.x + desc.area.w,
            height: desc.area.y + desc.area.h,
            depth_or_layers: 1,
        };
        let mut needs_empty_pass = false;
        let begin_info = if dynamic {
            let mut dynamic_colors = ArrayVec::<vk::RenderingAttachmentInfo<'static>, 8>::new();
            let mut depth_info = None;
            let mut stencil_info = None;
            for color in &desc.colors {
                let view = &color.view.inner;
                dimensions = view.size();
                if matches!(color.load, Load::Clear(_)) || color.resolve.is_some() {
                    needs_empty_pass = true;
                }
                attachment_handles.push(view.0.image.inner.core.handle);
                self.push_transition(
                    &mut barriers,
                    &view.0.image.inner.core,
                    view.0.desc.mips.clone(),
                    view.0.desc.layers.clone(),
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::AccessFlags2::COLOR_ATTACHMENT_READ
                        | vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                );
                let clear = match color.load {
                    Load::Clear(value) => vk::ClearValue {
                        color: if view.0.format.is_uint() {
                            vk::ClearColorValue {
                                uint32: value.map(|v| v as u32),
                            }
                        } else if view.0.format.is_sint() {
                            vk::ClearColorValue {
                                int32: value.map(|v| v as i32),
                            }
                        } else {
                            vk::ClearColorValue {
                                float32: value.map(|v| v as f32),
                            }
                        },
                    },
                    _ => vk::ClearValue::default(),
                };
                let mut info = vk::RenderingAttachmentInfo::default()
                    .image_view(view.0.handle)
                    .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                    .load_op(load_op(&color.load, none))
                    .store_op(store_op(color.store, none))
                    .clear_value(clear);
                if let Some(resolve) = color.resolve {
                    let view = &resolve.inner;
                    attachment_handles.push(view.0.image.inner.core.handle);
                    self.push_transition(
                        &mut barriers,
                        &view.0.image.inner.core,
                        view.0.desc.mips.clone(),
                        view.0.desc.layers.clone(),
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                        vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                        vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                    );
                    info = info
                        .resolve_mode(if view.0.format.is_uint() || view.0.format.is_sint() {
                            vk::ResolveModeFlags::SAMPLE_ZERO
                        } else {
                            vk::ResolveModeFlags::AVERAGE
                        })
                        .resolve_image_view(view.0.handle)
                        .resolve_image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
                }
                dynamic_colors.push(info);
            }
            if let Some(depth) = &desc.depth_stencil {
                let view = &depth.view.inner;
                let size = view.size();
                dimensions = if desc.colors.is_empty() {
                    size
                } else {
                    Extent {
                        width: dimensions.width.min(size.width),
                        height: dimensions.height.min(size.height),
                        ..dimensions
                    }
                };
                if matches!(depth.depth.0, Load::Clear(_))
                    || matches!(depth.stencil.0, Load::Clear(_))
                {
                    needs_empty_pass = true;
                }
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
                attachment_handles.push(view.0.image.inner.core.handle);
                self.push_transition(
                    &mut barriers,
                    &view.0.image.inner.core,
                    view.0.desc.mips.clone(),
                    view.0.desc.layers.clone(),
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
                if view.0.format.has_depth() {
                    depth_info = Some(
                        vk::RenderingAttachmentInfo::default()
                            .image_view(view.0.handle)
                            .image_layout(layout)
                            .load_op(load_op(&depth.depth.0, none))
                            .store_op(store_op(depth.depth.1, none))
                            .clear_value(clear),
                    );
                }
                if view.0.format.has_stencil() {
                    stencil_info = Some(
                        vk::RenderingAttachmentInfo::default()
                            .image_view(view.0.handle)
                            .image_layout(layout)
                            .load_op(load_op(&depth.stencil.0, none))
                            .store_op(store_op(depth.stencil.1, none))
                            .clear_value(clear),
                    );
                }
            }
            self.flush_image_barriers(&barriers);
            PassBegin::Dynamic {
                colors: dynamic_colors,
                depth: depth_info,
                stencil: stencil_info,
                layers: dimensions.depth_or_layers,
            }
        } else {
            // Colors, their resolves and depth: no allocation per pass.
            let mut attachments = ArrayVec::<_, 17>::new();
            let mut views = ArrayVec::<_, 17>::new();
            let mut clears = ArrayVec::<_, 17>::new();
            let mut colors = ArrayVec::<_, 8>::new();
            let mut resolves = ArrayVec::<_, 8>::new();
            let mut depth_ref = None;
            for color in &desc.colors {
                let view = &color.view.inner;
                dimensions = view.size();
                if matches!(color.load, Load::Clear(_)) || color.resolve.is_some() {
                    needs_empty_pass = true;
                }
                attachment_handles.push(view.0.image.inner.core.handle);
                self.push_transition(
                    &mut barriers,
                    &view.0.image.inner.core,
                    view.0.desc.mips.clone(),
                    view.0.desc.layers.clone(),
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::AccessFlags2::COLOR_ATTACHMENT_READ
                        | vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                );
                let clear = match color.load {
                    Load::Clear(value) => vk::ClearValue {
                        color: if view.0.format.is_uint() {
                            vk::ClearColorValue {
                                uint32: value.map(|v| v as u32),
                            }
                        } else if view.0.format.is_sint() {
                            vk::ClearColorValue {
                                int32: value.map(|v| v as i32),
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
                if let Some(resolve) = color.resolve {
                    let view = &resolve.inner;
                    attachment_handles.push(view.0.image.inner.core.handle);
                    self.push_transition(
                        &mut barriers,
                        &view.0.image.inner.core,
                        view.0.desc.mips.clone(),
                        view.0.desc.layers.clone(),
                        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                        vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                        vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                    );
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
            }
            if let Some(depth) = &desc.depth_stencil {
                let view = &depth.view.inner;
                let size = view.size();
                dimensions = if desc.colors.is_empty() {
                    size
                } else {
                    Extent {
                        width: dimensions.width.min(size.width),
                        height: dimensions.height.min(size.height),
                        ..dimensions
                    }
                };
                if matches!(depth.depth.0, Load::Clear(_))
                    || matches!(depth.stencil.0, Load::Clear(_))
                {
                    needs_empty_pass = true;
                }
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
                attachment_handles.push(view.0.image.inner.core.handle);
                self.push_transition(
                    &mut barriers,
                    &view.0.image.inner.core,
                    view.0.desc.mips.clone(),
                    view.0.desc.layers.clone(),
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
            self.flush_image_barriers(&barriers);
            let legacy = if self.ready() {
                match self.legacy_pass(
                    &attachments,
                    &views,
                    &colors,
                    &resolves,
                    depth_ref.as_ref(),
                    dimensions,
                ) {
                    Ok(pair) => Some(pair),
                    Err(e) => {
                        self.error = Some(e);
                        None
                    }
                }
            } else {
                None
            };
            PassBegin::Legacy { legacy, clears }
        };
        let preamble_index = self.commands.len();
        crate::RenderPass {
            inner: RenderPass {
                encoder: self,
                area: desc.area,
                begin_info,
                attachment_handles,
                needs_empty_pass,
                begun: false,
                timestamp,
                occlusion: None,
                statistics: None,
                preamble: None,
                preamble_index,
                resets: Vec::new(),
                pipeline: None,
                state: None,
                index: None,
                viewport: vk::Viewport {
                    x: desc.area.x as f32,
                    y: desc.area.y as f32,
                    width: desc.area.w as f32,
                    height: desc.area.h as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                },
                scissor: rect(desc.area),
                stencil_reference: 0,
                blend_constant: [0.0; 4],
                depth_bias: (0.0, 0.0, 0.0),
            },
        }
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
            &copy.image.inner.core,
            copy.mip..copy.mip + 1,
            copy.layer..copy.layer + count,
            layout,
            access,
            vk::PipelineStageFlags2::TRANSFER,
        );
    }
    pub(crate) fn generate_mips(&mut self, view: &crate::ImageView) {
        let image = view.image();
        let desc = &view.inner.0.desc;
        if desc.mips.len() == 1 {
            return;
        }
        if formats::vk(view.format()) != formats::vk(image.desc().format) {
            self.generate_view_mips(view);
            return;
        }
        let core = &image.inner.core;
        let is_3d = core.desc.dimension == crate::resource::Dimension::D3;
        let offsets = |level: u32| {
            let axis = |n: u32| (n >> level).max(1) as i32;
            [
                vk::Offset3D::default(),
                vk::Offset3D {
                    x: axis(core.desc.size.width),
                    y: axis(core.desc.size.height),
                    z: if is_3d {
                        axis(core.desc.size.depth_or_layers)
                    } else {
                        1
                    },
                },
            ]
        };
        let layers = |level| {
            vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .mip_level(level)
                .base_array_layer(desc.layers.start)
                .layer_count(desc.layers.len() as u32)
        };
        for dst in desc.mips.start + 1..desc.mips.end {
            let src = dst - 1;
            let mut barriers = Vec::new();
            self.push_transition(
                &mut barriers,
                core,
                src..src + 1,
                desc.layers.clone(),
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::AccessFlags2::TRANSFER_READ,
                vk::PipelineStageFlags2::TRANSFER,
            );
            self.push_transition(
                &mut barriers,
                core,
                dst..dst + 1,
                desc.layers.clone(),
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags2::TRANSFER_WRITE,
                vk::PipelineStageFlags2::TRANSFER,
            );
            self.flush_image_barriers(&barriers);
            if self.ready() {
                let region = vk::ImageBlit::default()
                    .src_subresource(layers(src))
                    .src_offsets(offsets(src))
                    .dst_subresource(layers(dst))
                    .dst_offsets(offsets(dst));
                unsafe {
                    self.shared.context.device.cmd_blit_image(
                        self.command,
                        core.handle,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        core.handle,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &[region],
                        vk::Filter::LINEAR,
                    );
                }
            }
        }
    }

    fn generate_view_mips(&mut self, view: &crate::ImageView) {
        use crate::resource::{Dimension, ImageDesc, Usage};
        let src = view.image();
        let desc = &view.inner.0.desc;
        let dimension = match src.desc().dimension {
            Dimension::Cube => Dimension::D2,
            dimension => dimension,
        };
        let is_3d = dimension == Dimension::D3;
        // Vulkan blits use the allocation's format. Copy the view's bits to
        // a matching image first so e.g. sRGB mip generation filters in linear space.
        let size = view.size();
        let scratch = match Device(self.shared.context.clone()).create_image(&ImageDesc {
            dimension,
            size,
            format: view.format(),
            mips: desc.mips.len() as u32,
            samples: 1,
            usage: Usage::COPY_SRC | Usage::COPY_DST,
            view_formats: &[],
            label: "mip view",
        }) {
            Ok(image) => image,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let copy = |image, mip, layer| ImageCopy {
            image,
            mip,
            layer,
            origin: (0, 0, 0),
        };
        self.copy_image(
            copy(src, desc.mips.start, desc.layers.start),
            copy(&scratch, 0, 0),
            size,
        );
        self.generate_mips(scratch.view());
        for mip in 1..desc.mips.len() as u32 {
            self.copy_image(
                copy(&scratch, mip, 0),
                copy(src, desc.mips.start + mip, desc.layers.start),
                Extent {
                    width: (size.width >> mip).max(1),
                    height: (size.height >> mip).max(1),
                    depth_or_layers: if is_3d {
                        (size.depth_or_layers >> mip).max(1)
                    } else {
                        size.depth_or_layers
                    },
                },
            );
        }
    }

    pub(crate) fn copy_image(&mut self, src: ImageCopy<'_>, dst: ImageCopy<'_>, size: Extent) {
        self.copy_image_regions(
            src.image,
            dst.image,
            &[ImageCopyRegion {
                src_mip: src.mip,
                src_layer: src.layer,
                src_origin: src.origin,
                dst_mip: dst.mip,
                dst_layer: dst.layer,
                dst_origin: dst.origin,
                size,
            }],
        );
    }
    pub(crate) fn copy_image_regions(
        &mut self,
        src: &crate::Image,
        dst: &crate::Image,
        copies: &[ImageCopyRegion],
    ) {
        if copies.is_empty() {
            return;
        }
        self.memory_barrier();
        let mut barriers = Vec::with_capacity(2);
        let src_d3 = src.desc().dimension == Dimension::D3;
        let dst_d3 = dst.desc().dimension == Dimension::D3;
        let mut seen_src = Vec::<(u32, u32)>::new();
        let mut seen_dst = Vec::<(u32, u32)>::new();
        for copy in copies {
            let src_layers = if src_d3 { 1 } else { copy.size.depth_or_layers };
            for layer in copy.src_layer..copy.src_layer + src_layers {
                let key = (copy.src_mip, layer);
                if !seen_src.contains(&key) {
                    seen_src.push(key);
                    self.push_transition(
                        &mut barriers,
                        &src.inner.core,
                        copy.src_mip..copy.src_mip + 1,
                        layer..layer + 1,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::AccessFlags2::TRANSFER_READ,
                        vk::PipelineStageFlags2::TRANSFER,
                    );
                }
            }
            let dst_layers = if dst_d3 { 1 } else { copy.size.depth_or_layers };
            for layer in copy.dst_layer..copy.dst_layer + dst_layers {
                let key = (copy.dst_mip, layer);
                if !seen_dst.contains(&key) {
                    seen_dst.push(key);
                    self.push_transition(
                        &mut barriers,
                        &dst.inner.core,
                        copy.dst_mip..copy.dst_mip + 1,
                        layer..layer + 1,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::AccessFlags2::TRANSFER_WRITE,
                        vk::PipelineStageFlags2::TRANSFER,
                    );
                }
            }
        }
        self.flush_image_barriers(&barriers);
        if !self.ready() {
            return;
        }
        let planes = src.desc().format.planes();
        let aspect = formats::aspect(src.desc().format);
        let mut regions = Vec::with_capacity(copies.len() * planes as usize);
        for copy in copies {
            let src_layer_count = if src_d3 { 1 } else { copy.size.depth_or_layers };
            let dst_layer_count = if dst_d3 { 1 } else { copy.size.depth_or_layers };
            let base_extent = vk::Extent3D {
                width: copy.size.width,
                height: copy.size.height,
                depth: if src_d3 { copy.size.depth_or_layers } else { 1 },
            };
            for plane in 0..planes {
                let scale = if plane == 0 { 1 } else { 2 };
                let plane_aspect = if planes > 1 {
                    if plane == 0 {
                        vk::ImageAspectFlags::PLANE_0
                    } else {
                        vk::ImageAspectFlags::PLANE_1
                    }
                } else {
                    aspect
                };
                let source = vk::ImageSubresourceLayers::default()
                    .aspect_mask(plane_aspect)
                    .mip_level(copy.src_mip)
                    .base_array_layer(copy.src_layer)
                    .layer_count(src_layer_count);
                let destination = vk::ImageSubresourceLayers::default()
                    .aspect_mask(plane_aspect)
                    .mip_level(copy.dst_mip)
                    .base_array_layer(copy.dst_layer)
                    .layer_count(dst_layer_count);
                let mut extent = base_extent;
                extent.width /= scale;
                extent.height /= scale;
                regions.push(
                    vk::ImageCopy::default()
                        .src_subresource(source)
                        .src_offset(offset((
                            copy.src_origin.0 / scale,
                            copy.src_origin.1 / scale,
                            copy.src_origin.2,
                        )))
                        .dst_subresource(destination)
                        .dst_offset(offset((
                            copy.dst_origin.0 / scale,
                            copy.dst_origin.1 / scale,
                            copy.dst_origin.2,
                        )))
                        .extent(extent),
                );
            }
        }
        self.synced = false;
        unsafe {
            self.shared.context.device.cmd_copy_image(
                self.command,
                src.inner.core.handle,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst.inner.core.handle,
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
        self.has_query_ops = true;
        unsafe {
            self.shared.context.device.cmd_copy_query_pool_results(
                self.command,
                set.inner.0.handle,
                range.start,
                range.end - range.start,
                dst.inner.0.handle,
                offset,
                if set.inner.0.kind == crate::queue::QueryKind::PipelineStatistics {
                    16
                } else {
                    8
                },
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
            for (image, states, _) in self.originals.drain(..) {
                *image.states.lock().unwrap() = states;
            }
            // Unsubmitted buffers never reached the GPU. The open one is
            // ended first, so the reset never meets a recording buffer.
            if let Some(mut pool) = self.pool.take() {
                let context = &self.shared.context;
                if self.command != vk::CommandBuffer::null() {
                    let _ = unsafe { context.device.end_command_buffer(self.command) };
                }
                match pool.reset(context) {
                    Ok(()) => self.shared.state.lock().unwrap().pools.push(pool),
                    Err(_) => pool.destroy(context),
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

#[allow(clippy::large_enum_variant)]
enum PassBegin {
    Dynamic {
        colors: ArrayVec<vk::RenderingAttachmentInfo<'static>, 8>,
        depth: Option<vk::RenderingAttachmentInfo<'static>>,
        stencil: Option<vk::RenderingAttachmentInfo<'static>>,
        layers: u32,
    },
    Legacy {
        legacy: Option<(vk::RenderPass, vk::Framebuffer)>,
        clears: ArrayVec<vk::ClearValue, 17>,
    },
}

pub(crate) struct RenderPass<'e> {
    pub(super) encoder: &'e mut Encoder,
    area: Rect,
    begin_info: PassBegin,
    attachment_handles: ArrayVec<vk::Image, 17>,
    needs_empty_pass: bool,
    begun: bool,
    timestamp: Option<(vk::QueryPool, u32)>,
    occlusion: Option<(vk::QueryPool, u32)>,
    statistics: Option<(vk::QueryPool, u32)>,
    preamble: Option<vk::CommandBuffer>,
    preamble_index: usize,
    resets: Vec<(vk::QueryPool, u32)>,
    pipeline: Option<Arc<pipeline::Compiled>>,
    state: Option<pipeline::Dynamic>,
    index: Option<(vk::Buffer, u64, IndexFormat)>,
    viewport: vk::Viewport,
    scissor: vk::Rect2D,
    stencil_reference: u32,
    blend_constant: [f32; 4],
    depth_bias: (f32, f32, f32),
}
impl RenderPass<'_> {
    fn ensure_preamble(
        encoder: &mut Encoder,
        preamble: &mut Option<vk::CommandBuffer>,
        preamble_index: usize,
    ) -> Option<vk::CommandBuffer> {
        if let Some(cb) = *preamble {
            return Some(cb);
        }
        if !encoder.ready() {
            return None;
        }
        match encoder.allocate_buffer() {
            Ok(cb) => {
                encoder.commands.insert(preamble_index, cb);
                *preamble = Some(cb);
                Some(cb)
            }
            Err(error) => {
                encoder.error = Some(error);
                None
            }
        }
    }
    fn ensure_begun(&mut self) -> bool {
        if self.begun {
            return self.encoder.ready();
        }
        if !self.encoder.ready() {
            return false;
        }
        // Only split out a preamble CB eagerly if an earlier command in this
        // encoder touched an image that is not in a read-only layout or ran
        // query ops, so any lazy mid-pass preamble stays strictly ordered.
        if self
            .encoder
            .has_pending_image_transitions(&self.attachment_handles)
            || self.encoder.has_query_ops
        {
            let preamble = self.encoder.command;
            self.encoder.commands.push(preamble);
            self.preamble = Some(preamble);
            self.encoder.command = vk::CommandBuffer::null();
            if let Err(error) = self.encoder.allocate_command() {
                self.encoder.error = Some(error);
                return false;
            }
        }
        let context = &self.encoder.shared.context;
        let command = self.encoder.command;
        match &self.begin_info {
            PassBegin::Dynamic {
                colors,
                depth,
                stencil,
                layers,
            } => {
                let mut info = vk::RenderingInfo::default()
                    .render_area(rect(self.area))
                    .layer_count(*layers)
                    .color_attachments(colors);
                if let Some(depth) = depth {
                    info = info.depth_attachment(depth);
                }
                if let Some(stencil) = stencil {
                    info = info.stencil_attachment(stencil);
                }
                unsafe {
                    if context.api >= vk::API_VERSION_1_3 {
                        context.device.cmd_begin_rendering(command, &info);
                    } else {
                        context.dynamic.cmd_begin_rendering(command, &info);
                    }
                }
                self.begun = true;
            }
            PassBegin::Legacy { legacy, clears } => {
                let Some((pass, framebuffer)) = *legacy else {
                    return false;
                };
                let info = vk::RenderPassBeginInfo::default()
                    .render_pass(pass)
                    .framebuffer(framebuffer)
                    .render_area(rect(self.area))
                    .clear_values(clears);
                unsafe {
                    context.device.cmd_begin_render_pass(
                        command,
                        &info,
                        vk::SubpassContents::INLINE,
                    );
                }
                self.begun = true;
            }
        }
        unsafe {
            context
                .device
                .cmd_set_viewport(command, 0, &[self.viewport]);
            context.device.cmd_set_scissor(command, 0, &[self.scissor]);
            context.device.cmd_set_stencil_reference(
                command,
                vk::StencilFaceFlags::FRONT_AND_BACK,
                self.stencil_reference,
            );
            context
                .device
                .cmd_set_blend_constants(command, &self.blend_constant);
            context.device.cmd_set_depth_bias(
                command,
                self.depth_bias.0,
                self.depth_bias.2,
                self.depth_bias.1,
            );
            if let Some(pipeline) = &self.pipeline {
                context.device.cmd_bind_pipeline(
                    command,
                    vk::PipelineBindPoint::GRAPHICS,
                    pipeline.handle,
                );
            }
            if let Some(state) = &self.state {
                state.emit(context, command, None);
            }
            if let Some((buffer, offset, format)) = self.index {
                context.device.cmd_bind_index_buffer(
                    command,
                    buffer,
                    offset,
                    match format {
                        IndexFormat::U16 => vk::IndexType::UINT16,
                        IndexFormat::U32 => vk::IndexType::UINT32,
                    },
                );
            }
        }
        self.encoder.synced = false;
        true
    }
}
impl Drop for RenderPass<'_> {
    fn drop(&mut self) {
        if !self.begun && self.needs_empty_pass {
            self.ensure_begun();
        }
        if self.begun && self.encoder.ready() {
            unsafe {
                let context = &self.encoder.shared.context;
                if matches!(self.begin_info, PassBegin::Dynamic { .. }) {
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
        if self.encoder.ready()
            && let Some(preamble) = self.preamble
        {
            for &(pool, index) in &self.resets {
                unsafe {
                    self.encoder
                        .shared
                        .context
                        .device
                        .cmd_reset_query_pool(preamble, pool, index, 1);
                }
            }
            if let Err(error) = self.encoder.shared.context.result(unsafe {
                self.encoder
                    .shared
                    .context
                    .device
                    .end_command_buffer(preamble)
            }) {
                self.encoder.error = Some(error);
            }
        }
        if let Some(bindings) = &mut self.encoder.bindings {
            bindings.clear();
        }
    }
}
impl RenderPass<'_> {
    pub(crate) fn set_pipeline(&mut self, pipeline: &crate::RenderPipeline) {
        if !self.encoder.ready() {
            return;
        }
        let pipeline = &pipeline.inner;
        let changed = self
            .pipeline
            .as_ref()
            .is_none_or(|p| p.handle != pipeline.compiled.handle);
        if changed {
            if self.begun {
                unsafe {
                    self.encoder.shared.context.device.cmd_bind_pipeline(
                        self.encoder.command,
                        vk::PipelineBindPoint::GRAPHICS,
                        pipeline.compiled.handle,
                    );
                }
            }
            self.encoder
                .bindings
                .as_mut()
                .unwrap()
                .pipeline(&pipeline.compiled);
            self.pipeline = Some(pipeline.compiled.clone());
        }
        if self.state != Some(pipeline.dynamic) {
            if self.begun {
                pipeline.dynamic.emit(
                    &self.encoder.shared.context,
                    self.encoder.command,
                    self.state.as_ref(),
                );
            }
            self.state = Some(pipeline.dynamic);
        }
    }
    pub(crate) fn bind(&mut self, slot: u32, resource: Bind<'_>) {
        let mut bindings = self.encoder.bindings.take().unwrap();
        let begun = self.begun;
        let preamble = &mut self.preamble;
        let preamble_index = self.preamble_index;
        bindings.bind(
            self.encoder,
            self.pipeline.as_deref(),
            slot,
            resource,
            |encoder| {
                if begun {
                    Self::ensure_preamble(encoder, preamble, preamble_index)
                } else {
                    None
                }
            },
        );
        self.encoder.bindings = Some(bindings);
    }
    pub(crate) fn bind_vertex(&mut self, slot: u32, resource: Bind<'_>) {
        self.bind(slot, resource);
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
        let (_, expected) = pipeline.video.as_ref().expect("converted video pipeline");
        debug_assert!(
            Arc::ptr_eq(pool, expected),
            "video buffer belongs to another pool"
        );
        if self.begun {
            let core = &image.image().inner.core;
            let family = self.encoder.shared.context.family;
            let stale = {
                let state = core.states.lock().unwrap()[0];
                state.layout != vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL || state.family != family
            };
            if stale {
                let command = self.encoder.command;
                if let Some(cb) =
                    Self::ensure_preamble(self.encoder, &mut self.preamble, self.preamble_index)
                {
                    self.encoder.command = cb;
                    self.encoder.view_transition(
                        &image.image().view().inner,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        vk::AccessFlags2::SHADER_READ,
                        vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    );
                    self.encoder.command = command;
                }
            }
        } else {
            self.encoder.view_transition(
                &image.image().view().inner,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::AccessFlags2::SHADER_READ,
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
            );
            if !self.ensure_begun() {
                return;
            }
        }
        let pipeline = self.pipeline.as_ref().unwrap();
        let (index, _) = pipeline.video.as_ref().unwrap();
        unsafe {
            self.encoder.shared.context.device.cmd_bind_descriptor_sets(
                self.encoder.command,
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
        if self.begun {
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
        }
        self.index = Some(value);
    }
    fn prepare(&mut self) -> bool {
        if !self.ensure_begun() {
            return false;
        }
        let pipeline = self.pipeline.as_deref();
        debug_assert!(
            pipeline.is_some() || !self.encoder.ready(),
            "draw without a pipeline"
        );
        let mut bindings = self.encoder.bindings.take().unwrap();
        let ready = pipeline
            .is_some_and(|p| bindings.flush(self.encoder, p, vk::PipelineBindPoint::GRAPHICS));
        self.encoder.bindings = Some(bindings);
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
        if !self.encoder.ready() {
            return;
        }
        self.viewport = vk::Viewport {
            x,
            y,
            width: w,
            height: h,
            min_depth,
            max_depth,
        };
        if self.begun {
            unsafe {
                self.encoder.shared.context.device.cmd_set_viewport(
                    self.encoder.command,
                    0,
                    &[self.viewport],
                );
            }
        }
    }
    pub(crate) fn set_scissor(&mut self, value: Rect) {
        if !self.encoder.ready() {
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
        self.scissor = rect(Rect {
            x,
            y,
            w: right - x,
            h: bottom - y,
        });
        if self.begun {
            unsafe {
                self.encoder.shared.context.device.cmd_set_scissor(
                    self.encoder.command,
                    0,
                    &[self.scissor],
                );
            }
        }
    }
    pub(crate) fn set_stencil_reference(&mut self, value: u32) {
        if !self.encoder.ready() {
            return;
        }
        self.stencil_reference = value;
        if self.begun {
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
        if !self.encoder.ready() {
            return;
        }
        self.blend_constant = rgba;
        if self.begun {
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
        if !self.encoder.ready() {
            return;
        }
        self.depth_bias = (constant, slope, clamp);
        if self.begun {
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
        self.occlusion = self.begin_query(set, index, vk::QueryControlFlags::PRECISE);
    }
    pub(crate) fn begin_statistics(&mut self, set: &crate::queue::QuerySet, index: u32) {
        self.statistics = self.begin_query(set, index, vk::QueryControlFlags::empty());
    }
    fn begin_query(
        &mut self,
        set: &crate::queue::QuerySet,
        index: u32,
        flags: vk::QueryControlFlags,
    ) -> Option<(vk::QueryPool, u32)> {
        if !self.encoder.ready() {
            return None;
        }
        if !self.begun {
            unsafe {
                self.encoder.shared.context.device.cmd_reset_query_pool(
                    self.encoder.command,
                    set.inner.0.handle,
                    index,
                    1,
                );
            }
            if !self.ensure_begun() {
                return None;
            }
        } else {
            Self::ensure_preamble(self.encoder, &mut self.preamble, self.preamble_index)?;
            self.resets.push((set.inner.0.handle, index));
        }
        self.encoder.has_query_ops = true;
        unsafe {
            self.encoder.shared.context.device.cmd_begin_query(
                self.encoder.command,
                set.inner.0.handle,
                index,
                flags,
            );
        }
        Some((set.inner.0.handle, index))
    }
    pub(crate) fn end_occlusion(&mut self) {
        let query = self.occlusion.take();
        self.end_query(query);
    }
    pub(crate) fn end_statistics(&mut self) {
        let query = self.statistics.take();
        self.end_query(query);
    }
    fn end_query(&mut self, query: Option<(vk::QueryPool, u32)>) {
        if self.encoder.ready()
            && let Some((pool, index)) = query
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
}
impl Drop for ComputePass<'_> {
    fn drop(&mut self) {
        self.encoder.timestamp_end(self.timestamp);
        if let Some(bindings) = &mut self.encoder.bindings {
            bindings.clear();
        }
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
        self.encoder.bindings.as_mut().unwrap().pipeline(compiled);
        self.pipeline = Some(compiled.clone());
    }
    pub(crate) fn bind(&mut self, slot: u32, resource: Bind<'_>) {
        let mut bindings = self.encoder.bindings.take().unwrap();
        bindings.bind(
            self.encoder,
            self.pipeline.as_deref(),
            slot,
            resource,
            |_| None,
        );
        self.encoder.bindings = Some(bindings);
    }
    fn prepare(&mut self) -> bool {
        let pipeline = self.pipeline.as_deref();
        debug_assert!(
            pipeline.is_some() || !self.encoder.ready(),
            "dispatch without a pipeline"
        );
        let mut bindings = self.encoder.bindings.take().unwrap();
        let ready = pipeline
            .is_some_and(|p| bindings.flush(self.encoder, p, vk::PipelineBindPoint::COMPUTE));
        self.encoder.bindings = Some(bindings);
        self.encoder.synced &= !ready;
        ready
    }
    pub(crate) fn dispatch(&mut self, x: u32, y: u32, z: u32) {
        self.encoder.compute_barrier();
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
        self.encoder.compute_barrier();
        self.dispatch_indirect_unordered(args);
    }
    pub(crate) fn dispatch_indirect_unordered(&mut self, args: BufferSlice<'_>) {
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
