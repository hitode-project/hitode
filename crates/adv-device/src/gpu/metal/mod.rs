//! Native programmable passes on the wgpu Metal queue, including deferred clears.
use super::*;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::{NSRange, NSString};
use objc2_metal::*;
use std::sync::atomic::AtomicBool;
pub(super) mod pipeline;
mod ring;
use ring::{CAPACITY, MAX_DRAW, Ring};
type Object<T> = Retained<ProtocolObject<T>>;

#[derive(Clone, Default)]
pub(super) struct ImageState {
    initialized: Arc<AtomicBool>,
    pub(super) view: Arc<OnceLock<Object<dyn MTLTexture>>>,
}
// Texture views are immutable; Metal retains/releases them across threads.
unsafe impl Send for ImageState {}
unsafe impl Sync for ImageState {}
impl Image {
    fn metal_view(&self) -> Object<dyn MTLTexture> {
        self.metal
            .view
            .get_or_init(|| unsafe {
                let texture = self.texture.as_hal::<wgpu::hal::api::Metal>().unwrap();
                texture
                    .raw_handle()
                    .newTextureViewWithPixelFormat_textureType_levels_slices(
                        MTLPixelFormat::BGRA8Unorm,
                        MTLTextureType::Type2D,
                        NSRange::new(self.mip as usize, self.levels as usize),
                        NSRange::new(0, 1),
                    )
                    .unwrap()
            })
            .clone()
    }
    fn initialize_metal(&self, encoder: &mut wgpu::CommandEncoder, stats: &program::BatchStats) {
        if self.metal.initialized.swap(true, Ordering::Relaxed) {
            return;
        }
        for mip in 0..self.texture.mip_level_count() {
            program::Clear::default().count(stats, false, false);
            let view = self.texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: mip,
                mip_level_count: Some(1),
                ..Default::default()
            });
            let colors = [Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })];
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &colors,
                ..Default::default()
            });
        }
    }
}

pub(super) struct State {
    device: Object<dyn MTLDevice>,
    queue: Object<dyn MTLCommandQueue>,
    gate: Arc<submission::Gate>,
    failure: Arc<OnceLock<String>>,
    ring: Ring,
    command: Option<Object<dyn MTLCommandBuffer>>,
    encoder: Option<Object<dyn MTLRenderCommandEncoder>>,
    attachments: Option<(wgpu::TextureView, Option<wgpu::TextureView>)>,
    constants: Option<u64>,
    draws: u64,
    batch_start: u64,
    owner: Arc<program::BatchStats>,
    last_metal: bool,
    pipelines: pipeline::Cache,
    pub(super) compiler: pipeline::Compiler,
}
// All encoding and CPU ring writes are serialized by NativeContext::pending.
unsafe impl Send for State {}
impl State {
    pub(super) fn new(context: &NativeContext) -> Result<Self, Error> {
        let (device, queue) = unsafe {
            (
                context
                    .device
                    .as_hal::<wgpu::hal::api::Metal>()
                    .unwrap()
                    .raw_device()
                    .clone(),
                context
                    .queue
                    .as_hal::<wgpu::hal::api::Metal>()
                    .unwrap()
                    .as_raw()
                    .into(),
            )
        };
        let ring = Ring::new(&device)?;
        Ok(Self {
            compiler: pipeline::Compiler::new(device.clone()),
            device,
            queue,
            ring,
            gate: context.gate.clone(),
            failure: context.failure.clone(),
            command: None,
            encoder: None,
            attachments: None,
            constants: None,
            draws: 0,
            batch_start: 0,
            owner: Default::default(),
            last_metal: false,
            pipelines: Default::default(),
        })
    }
    pub(super) fn has_depth(&self, target: &Target) -> bool {
        self.attachments.as_ref().is_some_and(|(color, depth)| {
            color == &target.image.render_view
                && depth.is_some()
                && depth.as_ref() == target.depth.as_ref().map(Depth::view)
        })
    }
    pub(super) fn detach_depth(&mut self, depth: &Depth) {
        if self
            .attachments
            .as_ref()
            .is_some_and(|(_, view)| view.as_ref() == Some(depth.view()))
        {
            self.attachments = None;
        }
    }
    pub(super) fn flush(&mut self) {
        if let Some(encoder) = self.encoder.take() {
            encoder.endEncoding();
        }
        if let Some(command) = self.command.take() {
            self.ring
                .submitted(&command, self.gate.clone(), self.failure.clone());
            command.commit();
            self.owner.submissions.fetch_add(1, Ordering::Relaxed);
            self.owner
                .last_flush_buffer_writes
                .store(0, Ordering::Relaxed);
        }
        self.attachments = None;
        self.constants = None;
        self.draws = 0;
    }
    pub(super) fn select_wgpu(&mut self) {
        if self.last_metal {
            self.owner.backend_switches.fetch_add(1, Ordering::Relaxed);
            self.last_metal = false;
        }
    }
    pub(super) fn draw(
        &mut self,
        gpu: &mut Gpu,
        pending: &mut program::Pending,
        target: &Target,
        draw: &ProgrammableDraw,
        state: &DrawState,
        key: &program::Key,
        initialization_target: &Target,
    ) -> Result<(), Error> {
        if state.scissor.is_some_and(Rect::empty) {
            self.pipelines
                .get(gpu, &self.compiler, key, &draw.program)?;
            return Ok(());
        }
        let blocks = draw.constant_ranges();
        let same_owner = Arc::ptr_eq(&self.owner, &gpu.programmable.batch_stats);
        // Bootstrap depth before the first native batch so later depth upgrades
        // can reopen the encoder without submitting that batch.
        let depth_uninitialized = initialization_target
            .depth
            .as_ref()
            .is_some_and(|d| !d.metal_initialized().load(Ordering::Relaxed));
        let initialize = depth_uninitialized
            || !target.image.metal.initialized.load(Ordering::Relaxed)
            || draw.textures.iter().any(|t| {
                !t.image
                    .as_ref()
                    .unwrap_or(&gpu.white)
                    .metal
                    .initialized
                    .load(Ordering::Relaxed)
            });
        let reuse = !initialize
            && !target.padded_attachments()
            && same_owner
            && self.constants.is_some_and(|offset| {
                blocks
                    .clone()
                    .all(|(at, bytes)| self.ring.equal(offset + at as u64, bytes))
            });
        let constants_size = if reuse { 0 } else { program::UNIFORM_STRIDE };
        let size = (constants_size
            + 256
            + draw
                .streams
                .iter()
                .map(|s| (s.bytes.len() as u64).next_multiple_of(4))
                .sum::<u64>()
            + draw.indices.as_ref().map_or(0, |v| v.len() as u64 * 4))
        .next_multiple_of(256);
        let start = match self.ring.plan(size) {
            Ok(start) => start,
            Err(error) => {
                gpu.programmable
                    .batch_stats
                    .ring_busy
                    .fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
        };
        let pipeline = self
            .pipelines
            .get(gpu, &self.compiler, key, &draw.program)?;
        // Admission is complete: Busy has not uploaded, encoded, or flushed work.
        if !same_owner {
            self.flush();
        }
        pending.select_target(target);
        let clear = pending.clear.take().map(|(_, clear)| clear);
        if clear.is_some() && pending.pass.is_none() {
            pending.encoder = None;
        }
        pending.flush(&gpu.context.queue);
        self.owner = gpu.programmable.batch_stats.clone();
        let attachment = gpu.attachment_color(target)?;
        let white = gpu.white.clone();
        let images: Vec<_> = draw
            .textures
            .iter()
            .map(|t| t.image.as_ref().unwrap_or(&white))
            .collect();
        if initialize || target.padded_attachments() {
            self.flush();
            self.select_wgpu();
            let mut encoder = gpu
                .context
                .device
                .create_command_encoder(&Default::default());
            attachment.image.initialize_metal(&mut encoder, &self.owner);
            for image in &images {
                image.initialize_metal(&mut encoder, &self.owner);
            }
            gpu.context.queue.submit([encoder.finish()]);
            self.owner.submissions.fetch_add(1, Ordering::Relaxed);
            if depth_uninitialized || target.padded_attachments() {
                pending.owner = Some(self.owner.clone());
                gpu.begin_batch_pass(pending, initialization_target)?;
                pending.flush(&gpu.context.queue);
            }
        }
        if !self.last_metal {
            // Queue writes are deferred by wgpu until submit, even without a Pending pass.
            gpu.context.queue.submit([]);
            self.owner.submissions.fetch_add(1, Ordering::Relaxed);
            self.owner.backend_switches.fetch_add(1, Ordering::Relaxed);
            self.last_metal = true;
        }
        if self.command.is_none() {
            self.batch_start = self.ring.head;
        }
        let mut offset = start % CAPACITY;
        let base = if reuse {
            self.constants.unwrap()
        } else {
            let base = offset;
            for (at, block) in blocks.clone() {
                self.ring.write(base + at as u64, block);
            }
            offset = base + program::UNIFORM_STRIDE;
            self.constants = Some(base);
            self.owner
                .ring_bytes
                .fetch_add(blocks.map(|(_, b)| b.len() as u64).sum(), Ordering::Relaxed);
            base
        };
        let sizes_offset = offset;
        let sizes: Vec<u32> = draw.streams.iter().map(|s| s.bytes.len() as u32).collect();
        self.ring.write(offset, bytemuck::cast_slice(&sizes));
        offset += 256;
        let mut offsets = Vec::new();
        for stream in &draw.streams {
            offsets.push(offset);
            self.ring.write(offset, &stream.bytes);
            offset += (stream.bytes.len() as u64).next_multiple_of(4);
        }
        let index_offset = offset;
        if let Some(indices) = &draw.indices {
            self.ring.write(offset, bytemuck::cast_slice(indices));
        }
        self.ring.head = start + size;
        let attachments = (
            attachment.image.render_view.clone(),
            target.depth.as_ref().map(|d| d.view().clone()),
        );
        unsafe {
            if clear.is_some() || self.attachments.as_ref() != Some(&attachments) {
                if let Some(encoder) = self.encoder.take() {
                    encoder.endEncoding();
                }
                let mut clear = clear.unwrap_or_default();
                if let Some((depth, stencil)) =
                    target.depth.as_ref().and_then(Depth::take_whole_clear)
                {
                    (clear.depth, clear.stencil) = (depth, stencil);
                }
                clear.count(&self.owner, target.depth.is_some(), true);
                let descriptor = MTLRenderPassDescriptor::new();
                let color = descriptor.colorAttachments().objectAtIndexedSubscript(0);
                let texture = attachment
                    .image
                    .texture
                    .as_hal::<wgpu::hal::api::Metal>()
                    .unwrap();
                color.setTexture(Some(texture.raw_handle()));
                color.setLevel(attachment.image.mip as usize);
                color.setLoadAction(if clear.color.is_some() {
                    MTLLoadAction::Clear
                } else {
                    MTLLoadAction::Load
                });
                if let Some(c) = clear.color {
                    color.setClearColor(MTLClearColor {
                        red: c.r,
                        green: c.g,
                        blue: c.b,
                        alpha: c.a,
                    });
                }
                color.setStoreAction(MTLStoreAction::Store);
                drop(texture);
                if let Some(depth) = &target.depth {
                    let texture = depth.texture().as_hal::<wgpu::hal::api::Metal>().unwrap();
                    let depth_attachment = descriptor.depthAttachment();
                    depth_attachment.setTexture(Some(texture.raw_handle()));
                    depth_attachment.setLoadAction(if clear.depth.is_some() {
                        MTLLoadAction::Clear
                    } else {
                        MTLLoadAction::Load
                    });
                    if let Some(depth) = clear.depth {
                        depth_attachment.setClearDepth(f64::from(depth));
                    }
                    depth_attachment.setStoreAction(MTLStoreAction::Store);
                    if depth.format().has_stencil() {
                        let stencil = descriptor.stencilAttachment();
                        stencil.setTexture(Some(texture.raw_handle()));
                        stencil.setLoadAction(if clear.stencil.is_some() {
                            MTLLoadAction::Clear
                        } else {
                            MTLLoadAction::Load
                        });
                        if let Some(value) = clear.stencil {
                            stencil.setClearStencil(value);
                        }
                        stencil.setStoreAction(MTLStoreAction::Store);
                    }
                }
                let command = self
                    .command
                    .get_or_insert_with(|| self.queue.commandBuffer().unwrap());
                self.encoder = Some(
                    command
                        .renderCommandEncoderWithDescriptor(&descriptor)
                        .unwrap(),
                );
                self.attachments = Some(attachments);
            }
            let encoder = self.encoder.as_ref().unwrap();
            encoder.setRenderPipelineState(&pipeline.0);
            encoder.setDepthStencilState(Some(&pipeline.1));
            encoder.setFrontFacingWinding(MTLWinding::CounterClockwise);
            encoder.setCullMode(match key.raster.cull {
                Cull::None => MTLCullMode::None,
                Cull::Clockwise => MTLCullMode::Back,
                Cull::CounterClockwise => MTLCullMode::Front,
            });
            encoder.setStencilReferenceValue(state.stencil_reference & 255);
            let v = state.viewport;
            encoder.setViewport(MTLViewport {
                originX: v.x as f64,
                originY: v.y as f64,
                width: v.width as f64,
                height: v.height as f64,
                znear: v.minimum_depth as f64,
                zfar: v.maximum_depth as f64,
            });
            let r = state.scissor.unwrap_or(Rect {
                x: 0,
                y: 0,
                w: target.image.width,
                h: target.image.height,
            });
            encoder.setScissorRect(MTLScissorRect {
                x: r.x as usize,
                y: r.y as usize,
                width: r.w as usize,
                height: r.h as usize,
            });
            for (slot, offset) in [
                base,
                base + std::mem::size_of::<ConstantBlock>() as u64,
                base + 2 * std::mem::size_of::<ConstantBlock>() as u64,
                sizes_offset,
            ]
            .into_iter()
            .enumerate()
            {
                encoder.setVertexBuffer_offset_atIndex(
                    Some(&self.ring.buffer),
                    offset as usize,
                    slot,
                );
                encoder.setFragmentBuffer_offset_atIndex(
                    Some(&self.ring.buffer),
                    offset as usize,
                    slot,
                );
            }
            for (index, offset) in offsets.iter().enumerate() {
                encoder.setVertexBuffer_offset_atIndex(
                    Some(&self.ring.buffer),
                    *offset as usize,
                    30 - index,
                );
            }
            for (index, (binding, image)) in draw.textures.iter().zip(&images).enumerate() {
                let slot = draw.program.textures[..index]
                    .iter()
                    .filter(|&&vertex| vertex == draw.program.textures[index])
                    .count();
                let sampler = self.pipelines.sampler(&self.device, binding.sampler);
                let texture = image.metal_view();
                if draw.program.textures[index] {
                    encoder.setVertexTexture_atIndex(Some(&texture), slot);
                    encoder.setVertexSamplerState_atIndex(Some(&sampler), slot);
                } else {
                    encoder.setFragmentTexture_atIndex(Some(&texture), slot);
                    encoder.setFragmentSamplerState_atIndex(Some(&sampler), slot);
                }
            }
            let primitive = match state.topology {
                Topology::Points => MTLPrimitiveType::Point,
                Topology::Lines => MTLPrimitiveType::Line,
                Topology::LineStrip => MTLPrimitiveType::LineStrip,
                Topology::Triangles => MTLPrimitiveType::Triangle,
                Topology::TriangleStrip => MTLPrimitiveType::TriangleStrip,
            };
            if let Some(indices) = &draw.indices {
                encoder.drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset(
                    primitive,
                    indices.len(),
                    MTLIndexType::UInt32,
                    &self.ring.buffer,
                    index_offset as usize,
                );
            } else {
                encoder.drawPrimitives_vertexStart_vertexCount(
                    primitive,
                    0,
                    draw.vertex_count as usize,
                );
            }
        }
        self.draws += 1;
        self.owner.metal_draws.fetch_add(1, Ordering::Relaxed);
        self.owner.ring_bytes.fetch_add(
            (sizes.len() * 4
                + draw.streams.iter().map(|s| s.bytes.len()).sum::<usize>()
                + draw.indices.as_ref().map_or(0, |v| v.len() * 4)) as u64,
            Ordering::Relaxed,
        );
        gpu.stats.draws += 1;
        gpu.stats.vertices += draw
            .indices
            .as_ref()
            .map_or(draw.vertex_count as u64, |v| v.len() as u64);
        // A future maximum draw must fit after earlier submissions complete,
        // including wrap padding, without stranding this unsubmitted batch.
        let next = if self.ring.head % CAPACITY + MAX_DRAW > CAPACITY {
            self.ring.head.next_multiple_of(CAPACITY)
        } else {
            self.ring.head
        };
        if self.draws == 32
            || next + MAX_DRAW - self.batch_start > CAPACITY
            || target.padded_attachments()
        {
            self.flush();
        }
        if target.padded_attachments() {
            self.select_wgpu();
            let mut encoder = gpu
                .context
                .device
                .create_command_encoder(&Default::default());
            attachment.store(&mut encoder, &target.image);
            gpu.context.queue.submit([encoder.finish()]);
            self.owner.submissions.fetch_add(1, Ordering::Relaxed);
        }
        gpu.check()
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests;
