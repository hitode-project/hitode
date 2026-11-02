use super::*;
use std::ops::Range;

pub(crate) struct Encoder {
    pub(super) device: Device,
    pub(super) command: Object<dyn MTLCommandBuffer>,
    pub(super) spans: Vec<crate::Span>,
    visibility: Option<Object<dyn MTLBuffer>>,
    visibility_offset: usize,
    /// Timestamp resolves, encoded after this submission completes.
    pub(super) resolves: Vec<Resolve>,
}

/// macOS 26 and later lose counter samples resolved before the sampling
/// command buffer completes, so timestamps resolve in a follow-up buffer.
pub(super) struct Resolve {
    pub(super) set: Arc<QueryData>,
    pub(super) range: Range<u32>,
    pub(super) dst: Object<dyn MTLBuffer>,
    pub(super) offset: usize,
    /// Indices whose pass did no work, as of encoding: written as zero.
    pub(super) empty: Vec<bool>,
}

pub(crate) struct RenderPass<'e> {
    pub(super) raw: Object<dyn MTLRenderCommandEncoder>,
    encoder: &'e mut Encoder,
    area: Rect,
    timestamps: Option<(Arc<QueryData>, u32)>,
    /// Draws reached the GPU, so stage-boundary samples are written.
    pub(super) work: bool,
    occlusion: Vec<(Object<dyn MTLBuffer>, usize, usize)>,
    resolves: Vec<(Object<dyn MTLTexture>, Object<dyn MTLTexture>)>,
    pipeline: Option<RenderPipeline>,
    bound: [Bound; 2],
    index: Option<(Object<dyn MTLBuffer>, usize, IndexFormat)>,
    depth_bias: (f32, f32, f32),
}
pub(crate) struct ComputePass<'e> {
    pipeline: Option<ComputePipeline>,
    bound: Bound,
    pub(super) raw: Object<dyn MTLComputeCommandEncoder>,
    /// Holds the encoder exclusively while the pass is open.
    _encoder: std::marker::PhantomData<&'e mut Encoder>,
    timestamps: Option<(Arc<QueryData>, u32)>,
    /// Dispatches reached the GPU, so encoder-boundary samples are written.
    pub(super) work: bool,
}

impl Encoder {
    pub(super) fn new(device: Device, command: Object<dyn MTLCommandBuffer>) -> Self {
        Self {
            device,
            command,
            spans: Vec::new(),
            visibility: None,
            visibility_offset: 0,
            resolves: Vec::new(),
        }
    }
    fn blit(&self) -> Object<dyn MTLBlitCommandEncoder> {
        self.command
            .blitCommandEncoder()
            .expect("Metal blit encoder")
    }
    pub(crate) fn render_pass<'e>(
        &'e mut self,
        desc: &RenderPassDesc<'_>,
    ) -> crate::RenderPass<'e> {
        let d = MTLRenderPassDescriptor::new();
        let mut size = desc
            .colors
            .first()
            .map(|color| color.view.size())
            .or_else(|| desc.depth_stencil.as_ref().map(|depth| depth.view.size()))
            .unwrap_or(Extent {
                width: desc.area.x + desc.area.w,
                height: desc.area.y + desc.area.h,
                depth_or_layers: 1,
            });
        let mut resolves = Vec::new();
        for (i, color) in desc.colors.iter().enumerate() {
            let attachment = unsafe { d.colorAttachments().objectAtIndexedSubscript(i) };
            size.width = size.width.min(color.view.size().width);
            size.height = size.height.min(color.view.size().height);
            if color.load == Load::None || color.store == Store::None {
                continue;
            }
            attachment.setTexture(Some(&color.view.inner.raw));
            let bounded = partial(desc.area, color.view.size());
            let memoryless = color.view.inner.raw.storageMode() == MTLStorageMode::Memoryless;
            attachment.setLoadAction(load(color.load, bounded && !memoryless));
            if let Load::Clear(c) = color.load {
                attachment.setClearColor(MTLClearColor {
                    red: c[0],
                    green: c[1],
                    blue: c[2],
                    alpha: c[3],
                });
            }
            attachment.setStoreAction(store(color.store, bounded && !memoryless));
            if let Some(resolve) = color.resolve {
                if bounded {
                    // Metal resolves every pixel of an active tile, including
                    // pixels outside the scissor. Copy only the area from an
                    // intermediate resolve so the destination keeps its border.
                    let texture = MTLTextureDescriptor::new();
                    texture.setPixelFormat(color.view.inner.raw.pixelFormat());
                    texture.setTextureType(MTLTextureType::Type2D);
                    unsafe {
                        texture.setWidth(color.view.size().width as usize);
                        texture.setHeight(color.view.size().height as usize);
                    }
                    texture.setStorageMode(MTLStorageMode::Private);
                    texture.setHazardTrackingMode(MTLHazardTrackingMode::Tracked);
                    texture.setUsage(MTLTextureUsage::RenderTarget);
                    let scratch = self
                        .device
                        .0
                        .raw
                        .newTextureWithDescriptor(&texture)
                        .expect("Metal bounded resolve");
                    attachment.setResolveTexture(Some(&scratch));
                    resolves.push((scratch, resolve.inner.raw.clone()));
                } else {
                    attachment.setResolveTexture(Some(&resolve.inner.raw));
                }
                attachment.setStoreAction(
                    if !memoryless && (color.store == Store::Store || bounded) {
                        MTLStoreAction::StoreAndMultisampleResolve
                    } else {
                        MTLStoreAction::MultisampleResolve
                    },
                );
            }
        }
        if let Some(depth) = &desc.depth_stencil {
            let view = &depth.view.inner;
            size.width = size.width.min(depth.view.size().width);
            size.height = size.height.min(depth.view.size().height);
            let bounded = partial(desc.area, depth.view.size())
                && view.raw.storageMode() != MTLStorageMode::Memoryless;
            if depth.depth.0 != Load::None && depth.depth.1 != Store::None {
                let attachment = d.depthAttachment();
                attachment.setTexture(Some(&view.raw));
                attachment.setLoadAction(load(depth.depth.0, bounded));
                attachment.setStoreAction(store(depth.depth.1, bounded));
                if let Load::Clear(value) = depth.depth.0 {
                    attachment.setClearDepth(f64::from(value));
                }
            }
            if view.raw.pixelFormat() == MTLPixelFormat::Depth32Float_Stencil8
                && depth.stencil.0 != Load::None
                && depth.stencil.1 != Store::None
            {
                let attachment = d.stencilAttachment();
                attachment.setTexture(Some(&view.raw));
                attachment.setLoadAction(load(depth.stencil.0, bounded));
                attachment.setStoreAction(store(depth.stencil.1, bounded));
                if let Load::Clear(value) = depth.stencil.0 {
                    attachment.setClearStencil(value);
                }
            }
        }
        let area = intersect(
            desc.area,
            Rect {
                x: 0,
                y: 0,
                w: size.width,
                h: size.height,
            },
        );
        d.setDefaultRasterSampleCount(desc.colors.first().map_or_else(
            || {
                desc.depth_stencil
                    .as_ref()
                    .map_or(1, |depth| depth.view.inner.raw.sampleCount())
            },
            |color| color.view.inner.raw.sampleCount(),
        ));
        d.setRenderTargetWidth(size.width as usize);
        d.setRenderTargetHeight(size.height as usize);
        // Counting results accumulate; reuse the slots only after the previous
        // pass copied its results out, with tracked hazards ordering the reset.
        if self.visibility_offset != 0 {
            let blit = self.blit();
            blit.fillBuffer_range_value(
                self.visibility.as_ref().unwrap(),
                NSRange::new(0, self.visibility_offset),
                0,
            );
            blit.endEncoding();
            self.visibility_offset = 0;
        }
        // Visibility buffers must be specified before creating the render encoder.
        let visibility = self.visibility.get_or_insert_with(|| {
            let buffer = self
                .device
                .0
                .raw
                .newBufferWithLength_options(
                    65536,
                    MTLResourceOptions::StorageModeShared
                        | MTLResourceOptions::HazardTrackingModeTracked,
                )
                .expect("Metal visibility buffer");
            // The command buffer has not been submitted, so the CPU owns it.
            unsafe {
                std::ptr::write_bytes(buffer.contents().as_ptr().cast::<u8>(), 0, 65536);
            }
            buffer
        });
        d.setVisibilityResultBuffer(Some(visibility));
        if let Some((set, first)) = desc.timestamps
            && let QueryData::Timestamp { raw, .. } = &*set.inner.0
        {
            unsafe {
                let a = d.sampleBufferAttachments().objectAtIndexedSubscript(0);
                a.setSampleBuffer(Some(raw));
                a.setStartOfVertexSampleIndex(first as usize);
                a.setEndOfFragmentSampleIndex(first as usize + 1);
                a.setEndOfVertexSampleIndex(MTLCounterDontSample);
                a.setStartOfFragmentSampleIndex(MTLCounterDontSample);
            }
        }
        let raw = self
            .command
            .renderCommandEncoderWithDescriptor(&d)
            .expect("Metal render encoder");
        raw.setLabel(Some(&NSString::from_str(desc.label)));
        raw.setViewport(MTLViewport {
            originX: 0.0,
            originY: 0.0,
            width: f64::from(size.width),
            height: f64::from(size.height),
            znear: 0.0,
            zfar: 1.0,
        });
        raw.setScissorRect(scissor(area));
        let work = self.device.clear(&raw, desc, area);
        crate::RenderPass {
            inner: RenderPass {
                raw,
                encoder: self,
                area,
                timestamps: desc
                    .timestamps
                    .map(|(set, first)| (set.inner.0.clone(), first)),
                work,
                occlusion: Vec::new(),
                resolves,
                pipeline: None,
                bound: std::array::from_fn(|_| Bound::default()),
                index: None,
                depth_bias: (0.0, 0.0, 0.0),
            },
        }
    }
    pub(crate) fn compute_pass<'e>(
        &'e mut self,
        desc: &ComputePassDesc<'_>,
    ) -> crate::ComputePass<'e> {
        let d = MTLComputePassDescriptor::new();
        if let Some((set, first)) = desc.timestamps
            && let QueryData::Timestamp { raw, .. } = &*set.inner.0
        {
            unsafe {
                let a = d.sampleBufferAttachments().objectAtIndexedSubscript(0);
                a.setSampleBuffer(Some(raw));
                a.setStartOfEncoderSampleIndex(first as usize);
                a.setEndOfEncoderSampleIndex(first as usize + 1);
            }
        }
        let raw = self
            .command
            .computeCommandEncoderWithDescriptor(&d)
            .expect("Metal compute encoder");
        raw.setLabel(Some(&NSString::from_str(desc.label)));
        crate::ComputePass {
            inner: ComputePass {
                pipeline: None,
                bound: Bound::default(),
                raw,
                _encoder: std::marker::PhantomData,
                timestamps: desc
                    .timestamps
                    .map(|(set, first)| (set.inner.0.clone(), first)),
                work: false,
            },
        }
    }
    pub(crate) fn copy_buffer_to_image(
        &mut self,
        src: BufferCopy<'_>,
        dst: ImageCopy<'_>,
        size: Extent,
    ) {
        let blit = self.blit();
        let (pitch, stride) = pitches(src, dst.image.desc().format, size);
        for (slice, origin, depth, offset) in regions(dst, size) {
            unsafe {
                blit.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                &src.buffer.inner.0.raw, src.offset as usize + offset * stride, pitch, stride,
                metal_size(size.width, size.height, depth), &dst.image.inner.data.raw, slice, dst.mip as usize, origin);
            }
        }
        blit.endEncoding();
    }
    pub(crate) fn copy_image_to_buffer(
        &mut self,
        src: ImageCopy<'_>,
        dst: BufferCopy<'_>,
        size: Extent,
    ) {
        let blit = self.blit();
        let (pitch, stride) = pitches(dst, src.image.desc().format, size);
        for (slice, origin, depth, offset) in regions(src, size) {
            unsafe {
                blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                &src.image.inner.data.raw, slice, src.mip as usize, origin,
                metal_size(size.width, size.height, depth), &dst.buffer.inner.0.raw,
                dst.offset as usize + offset * stride, pitch, stride);
            }
        }
        blit.endEncoding();
    }
    pub(crate) fn copy_image(&mut self, src: ImageCopy<'_>, dst: ImageCopy<'_>, size: Extent) {
        let blit = self.blit();
        for ((ss, so, depth, _), (ds, dest, _, _)) in
            regions(src, size).into_iter().zip(regions(dst, size))
        {
            unsafe {
                blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                &src.image.inner.data.raw, ss, src.mip as usize, so, metal_size(size.width, size.height, depth),
                &dst.image.inner.data.raw, ds, dst.mip as usize, dest);
            }
        }
        blit.endEncoding();
    }
    pub(crate) fn copy_buffer(
        &mut self,
        src: BufferSlice<'_>,
        dst: &crate::Buffer,
        dst_offset: u64,
    ) {
        let blit = self.blit();
        unsafe {
            blit.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                &src.buffer.inner.0.raw,
                src.offset as usize,
                &dst.inner.0.raw,
                dst_offset as usize,
                src.size as usize,
            );
        }
        blit.endEncoding();
    }
    pub(crate) fn fill_buffer(&mut self, dst: BufferSlice<'_>, value: u32) {
        if dst.size == 0 {
            return;
        }
        let blit = self.blit();
        let bytes = value.to_le_bytes();
        if bytes.iter().all(|&b| b == bytes[0]) {
            blit.fillBuffer_range_value(
                &dst.buffer.inner.0.raw,
                NSRange::new(dst.offset as usize, dst.size as usize),
                bytes[0],
            );
        } else {
            // Seed one word, then double non-overlapping ranges in GPU memory.
            let seed = self
                .device
                .0
                .raw
                .newBufferWithLength_options(4, MTLResourceOptions::StorageModeShared)
                .expect("Metal fill pattern");
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    seed.contents().as_ptr().cast::<u8>(),
                    4,
                );
                let raw = &dst.buffer.inner.0.raw;
                let offset = dst.offset as usize;
                let size = dst.size as usize;
                let mut filled = size.min(4);
                blit.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                    &seed, 0, raw, offset, filled,
                );
                while filled < size {
                    let count = filled.min(size - filled);
                    blit.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                        raw,
                        offset,
                        raw,
                        offset + filled,
                        count,
                    );
                    filled += count;
                }
            }
        }
        blit.endEncoding();
    }
    pub(crate) fn retain(&mut self, span: crate::Span) {
        self.spans.push(span);
    }
    pub(crate) fn resolve_queries(
        &mut self,
        set: &crate::queue::QuerySet,
        range: Range<u32>,
        dst: &crate::Buffer,
        offset: u64,
    ) {
        match &*set.inner.0 {
            QueryData::Timestamp { empty, .. } => {
                let empty =
                    empty.lock().unwrap()[range.start as usize..range.end as usize].to_vec();
                self.resolves.push(Resolve {
                    set: set.inner.0.clone(),
                    range,
                    dst: dst.inner.0.raw.clone(),
                    offset: offset as usize,
                    empty,
                });
            }
            QueryData::Occlusion(raw) => {
                let blit = self.blit();
                unsafe {
                    blit.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                        raw,
                        range.start as usize * 8,
                        &dst.inner.0.raw,
                        offset as usize,
                        range.len() * 8,
                    );
                }
                blit.endEncoding();
            }
        }
    }
}

fn pitches(buffer: BufferCopy<'_>, format: Format, size: Extent) -> (usize, usize) {
    let (block, bytes) = caps::block(format);
    let pitch = if buffer.row_pitch == 0 {
        size.width.div_ceil(block) * bytes
    } else {
        buffer.row_pitch
    };
    let rows = if buffer.rows == 0 {
        size.height.div_ceil(block)
    } else {
        buffer.rows
    };
    (pitch as usize, pitch as usize * rows as usize)
}
fn regions(copy: ImageCopy<'_>, size: Extent) -> Vec<(usize, MTLOrigin, u32, usize)> {
    let origin = MTLOrigin {
        x: copy.origin.0 as usize,
        y: copy.origin.1 as usize,
        z: copy.origin.2 as usize,
    };
    if copy.image.desc().dimension == Dimension::D3 {
        vec![(0, origin, size.depth_or_layers, 0)]
    } else {
        (0..size.depth_or_layers)
            .map(|i| ((copy.layer + i) as usize, origin, 1, i as usize))
            .collect()
    }
}
fn metal_size(width: u32, height: u32, depth: u32) -> MTLSize {
    MTLSize {
        width: width as usize,
        height: height as usize,
        depth: depth as usize,
    }
}
pub(super) fn intersect(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x).min(b.x.saturating_add(b.w));
    let y = a.y.max(b.y).min(b.y.saturating_add(b.h));
    Rect {
        x,
        y,
        w: (a.x.saturating_add(a.w).min(b.x.saturating_add(b.w))).saturating_sub(x),
        h: (a.y.saturating_add(a.h).min(b.y.saturating_add(b.h))).saturating_sub(y),
    }
}
pub(super) fn partial(area: Rect, size: Extent) -> bool {
    area.x != 0 || area.y != 0 || area.w < size.width || area.h < size.height
}
fn scissor(rect: Rect) -> MTLScissorRect {
    MTLScissorRect {
        x: rect.x as usize,
        y: rect.y as usize,
        width: rect.w as usize,
        height: rect.h as usize,
    }
}
fn load<T>(load: Load<T>, partial: bool) -> MTLLoadAction {
    match load {
        Load::Clear(_) if !partial => MTLLoadAction::Clear,
        Load::DontCare if !partial => MTLLoadAction::DontCare,
        _ => MTLLoadAction::Load,
    }
}
fn store(store: Store, partial: bool) -> MTLStoreAction {
    match store {
        Store::DontCare if !partial => MTLStoreAction::DontCare,
        _ => MTLStoreAction::Store,
    }
}

impl Drop for RenderPass<'_> {
    fn drop(&mut self) {
        if let Some((set, first)) = &self.timestamps {
            set.mark(*first, !self.work);
        }
        self.raw.endEncoding();
        if self.occlusion.is_empty() && self.resolves.is_empty() {
            return;
        }
        let blit = self.encoder.blit();
        for (dst, offset, src) in &self.occlusion {
            unsafe {
                blit.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                    self.encoder.visibility.as_ref().unwrap(),
                    *src,
                    dst,
                    *offset,
                    8,
                );
            }
        }
        for (src, dst) in &self.resolves {
            let origin = MTLOrigin {
                x: self.area.x as usize,
                y: self.area.y as usize,
                z: 0,
            };
            unsafe {
                blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                src, 0, 0, origin, metal_size(self.area.w, self.area.h, 1), dst, 0, 0, origin);
            }
        }
        blit.endEncoding();
    }
}
impl RenderPass<'_> {
    pub(crate) fn set_pipeline(&mut self, pipeline: &crate::RenderPipeline) {
        let p = &pipeline.inner;
        if self.pipeline.as_ref().is_none_or(|old| old.raw != p.raw) {
            self.raw.setRenderPipelineState(&p.raw);
        }
        if self
            .pipeline
            .as_ref()
            .is_none_or(|old| old.depth != p.depth)
        {
            self.raw.setDepthStencilState(Some(&p.depth));
        }
        self.raw.setCullMode(match p.primitive.cull {
            Cull::None => MTLCullMode::None,
            Cull::Front => MTLCullMode::Front,
            Cull::Back => MTLCullMode::Back,
        });
        self.raw.setFrontFacingWinding(if p.primitive.front_ccw {
            MTLWinding::CounterClockwise
        } else {
            MTLWinding::Clockwise
        });
        self.raw.setTriangleFillMode(if p.primitive.wireframe {
            MTLTriangleFillMode::Lines
        } else {
            MTLTriangleFillMode::Fill
        });
        self.raw.setDepthClipMode(if p.primitive.unclipped_depth {
            MTLDepthClipMode::Clamp
        } else {
            MTLDepthClipMode::Clip
        });
        let (constant, slope, clamp) = if p.depth_bias {
            self.depth_bias
        } else {
            (0.0, 0.0, 0.0)
        };
        self.raw
            .setDepthBias_slopeScale_clamp(constant, slope, clamp);
        self.pipeline = Some(p.clone());
        self.update_sizes();
    }
    pub(crate) fn bind(&mut self, slot: u32, resource: Bind<'_>) {
        let group = bind_group(resource);
        let p = self
            .pipeline
            .as_ref()
            .expect("render pipeline before binding");
        for b in p
            .bindings
            .iter()
            .filter(|b| b.resource.slot == slot && b.resource.group == group)
        {
            let stage = usize::from(b.stages == Stages::FRAGMENT);
            let index = b.resource.index as usize;
            if !self.bound[stage].set(index, resource) {
                continue;
            }
            unsafe {
                match resource {
                    Bind::Uniform(buffer) | Bind::StorageBuffer(buffer) => {
                        if stage == 0 {
                            self.raw.setVertexBuffer_offset_atIndex(
                                Some(&buffer.buffer.inner.0.raw),
                                buffer.offset as usize,
                                index,
                            );
                        } else {
                            self.raw.setFragmentBuffer_offset_atIndex(
                                Some(&buffer.buffer.inner.0.raw),
                                buffer.offset as usize,
                                index,
                            );
                        }
                    }
                    Bind::Sampled(image) | Bind::StorageImage(image) => {
                        if stage == 0 {
                            self.raw
                                .setVertexTexture_atIndex(Some(&image.inner.raw), index);
                        } else {
                            self.raw
                                .setFragmentTexture_atIndex(Some(&image.inner.raw), index);
                        }
                    }
                    Bind::Sampler(sampler) => {
                        if stage == 0 {
                            self.raw
                                .setVertexSamplerState_atIndex(Some(&sampler.inner.0), index);
                        } else {
                            self.raw
                                .setFragmentSamplerState_atIndex(Some(&sampler.inner.0), index);
                        }
                    }
                }
            }
        }
        self.update_sizes();
    }
    fn update_sizes(&mut self) {
        let p = self.pipeline.as_ref().unwrap();
        for (stage, sizes) in p.sizes.iter().enumerate() {
            if let Some((index, values)) = self.bound[stage].sizes(sizes.as_ref()) {
                unsafe {
                    if stage == 0 {
                        self.raw.setVertexBytes_length_atIndex(
                            NonNull::new(values.as_ptr().cast_mut()).unwrap().cast(),
                            std::mem::size_of_val(values),
                            index,
                        );
                    } else {
                        self.raw.setFragmentBytes_length_atIndex(
                            NonNull::new(values.as_ptr().cast_mut()).unwrap().cast(),
                            std::mem::size_of_val(values),
                            index,
                        );
                    }
                }
            }
        }
    }
    pub(crate) fn set_index_buffer(&mut self, buffer: BufferSlice<'_>, format: IndexFormat) {
        self.index = Some((
            buffer.buffer.inner.0.raw.clone(),
            buffer.offset as usize,
            format,
        ));
    }
    pub(crate) fn draw(&mut self, vertices: Range<u32>, instances: Range<u32>) {
        self.work = true;
        let topology = topology(
            self.pipeline
                .as_ref()
                .expect("render pipeline before draw")
                .primitive
                .topology,
        );
        unsafe {
            self.raw
                .drawPrimitives_vertexStart_vertexCount_instanceCount_baseInstance(
                    topology,
                    vertices.start as usize,
                    vertices.len(),
                    instances.len(),
                    instances.start as usize,
                );
        }
    }
    pub(crate) fn draw_indexed(
        &mut self,
        indices: Range<u32>,
        base_vertex: i32,
        instances: Range<u32>,
    ) {
        self.work = true;
        let topology = topology(
            self.pipeline
                .as_ref()
                .expect("render pipeline before draw")
                .primitive
                .topology,
        );
        let (buffer, offset, format) = self
            .index
            .as_ref()
            .expect("index buffer before indexed draw");
        let (index_type, stride) = index_type(*format);
        unsafe {
            self.raw.drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset_instanceCount_baseVertex_baseInstance(topology, indices.len(), index_type, buffer, offset + indices.start as usize * stride, instances.len(), base_vertex as isize, instances.start as usize);
        }
    }
    pub(crate) fn draw_indirect(&mut self, args: BufferSlice<'_>, indexed: bool) {
        self.work = true;
        let topology = topology(
            self.pipeline
                .as_ref()
                .expect("render pipeline before draw")
                .primitive
                .topology,
        );
        unsafe {
            if indexed {
                let (buffer, offset, format) = self
                    .index
                    .as_ref()
                    .expect("index buffer before indexed draw");
                self.raw.drawIndexedPrimitives_indexType_indexBuffer_indexBufferOffset_indirectBuffer_indirectBufferOffset(topology, index_type(*format).0, buffer, *offset, &args.buffer.inner.0.raw, args.offset as usize);
            } else {
                self.raw.drawPrimitives_indirectBuffer_indirectBufferOffset(
                    topology,
                    &args.buffer.inner.0.raw,
                    args.offset as usize,
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
        self.raw.setViewport(MTLViewport {
            originX: f64::from(x),
            originY: f64::from(y),
            width: f64::from(w),
            height: f64::from(h),
            znear: f64::from(min_depth),
            zfar: f64::from(max_depth),
        });
    }
    pub(crate) fn set_scissor(&mut self, rect: Rect) {
        self.raw.setScissorRect(scissor(intersect(rect, self.area)));
    }
    pub(crate) fn set_stencil_reference(&mut self, value: u32) {
        self.raw.setStencilReferenceValue(value);
    }
    pub(crate) fn set_blend_constant(&mut self, rgba: [f32; 4]) {
        self.raw
            .setBlendColorRed_green_blue_alpha(rgba[0], rgba[1], rgba[2], rgba[3]);
    }
    pub(crate) fn set_depth_bias(&mut self, constant: f32, slope: f32, clamp: f32) {
        self.depth_bias = (constant, slope, clamp);
        if self.pipeline.as_ref().is_some_and(|p| p.depth_bias) {
            self.raw
                .setDepthBias_slopeScale_clamp(constant, slope, clamp);
        }
    }
    pub(crate) fn begin_occlusion(&mut self, set: &crate::queue::QuerySet, index: u32) {
        if let QueryData::Occlusion(raw) = &*set.inner.0 {
            let offset = self.encoder.visibility_offset;
            self.encoder.visibility_offset += 8;
            self.raw
                .setVisibilityResultMode_offset(MTLVisibilityResultMode::Counting, offset);
            self.occlusion
                .push((raw.clone(), index as usize * 8, offset));
        }
    }
    pub(crate) fn end_occlusion(&mut self) {
        self.raw
            .setVisibilityResultMode_offset(MTLVisibilityResultMode::Disabled, 0);
    }
}
impl Drop for ComputePass<'_> {
    fn drop(&mut self) {
        if let Some((set, first)) = &self.timestamps {
            set.mark(*first, !self.work);
        }
        self.raw.endEncoding();
    }
}
impl ComputePass<'_> {
    pub(crate) fn set_pipeline(&mut self, pipeline: &crate::ComputePipeline) {
        if self
            .pipeline
            .as_ref()
            .is_none_or(|p| p.raw != pipeline.inner.raw)
        {
            self.raw.setComputePipelineState(&pipeline.inner.raw);
        }
        self.pipeline = Some(pipeline.inner.clone());
        self.update_sizes();
    }
    pub(crate) fn bind(&mut self, slot: u32, resource: Bind<'_>) {
        let group = bind_group(resource);
        let p = self
            .pipeline
            .as_ref()
            .expect("compute pipeline before binding");
        let Some(binding) = p
            .bindings
            .iter()
            .find(|b| b.resource.slot == slot && b.resource.group == group)
        else {
            return;
        };
        let index = binding.resource.index as usize;
        if self.bound.set(index, resource) {
            unsafe {
                match resource {
                    Bind::Uniform(buffer) | Bind::StorageBuffer(buffer) => {
                        self.raw.setBuffer_offset_atIndex(
                            Some(&buffer.buffer.inner.0.raw),
                            buffer.offset as usize,
                            index,
                        )
                    }
                    Bind::Sampled(image) | Bind::StorageImage(image) => {
                        self.raw.setTexture_atIndex(Some(&image.inner.raw), index)
                    }
                    Bind::Sampler(sampler) => self
                        .raw
                        .setSamplerState_atIndex(Some(&sampler.inner.0), index),
                }
            }
        }
        self.update_sizes();
    }
    fn update_sizes(&mut self) {
        if let Some((index, values)) = self
            .bound
            .sizes(self.pipeline.as_ref().unwrap().sizes.as_ref())
        {
            unsafe {
                self.raw.setBytes_length_atIndex(
                    NonNull::new(values.as_ptr().cast_mut()).unwrap().cast(),
                    std::mem::size_of_val(values),
                    index,
                );
            }
        }
    }
    pub(crate) fn dispatch(&mut self, x: u32, y: u32, z: u32) {
        self.work = true;
        let [w, h, d] = self
            .pipeline
            .as_ref()
            .expect("compute pipeline before dispatch")
            .workgroup;
        self.raw
            .dispatchThreadgroups_threadsPerThreadgroup(metal_size(x, y, z), metal_size(w, h, d));
    }
    pub(crate) fn dispatch_indirect(&mut self, args: BufferSlice<'_>) {
        self.work = true;
        let [w, h, d] = self
            .pipeline
            .as_ref()
            .expect("compute pipeline before dispatch")
            .workgroup;
        unsafe {
            self.raw
                .dispatchThreadgroupsWithIndirectBuffer_indirectBufferOffset_threadsPerThreadgroup(
                    &args.buffer.inner.0.raw,
                    args.offset as usize,
                    metal_size(w, h, d),
                );
        }
    }
}

fn bind_group(resource: Bind<'_>) -> u32 {
    match resource {
        Bind::Uniform(_) => 0,
        Bind::Sampled(_) => 1,
        Bind::Sampler(_) => 2,
        Bind::StorageImage(_) | Bind::StorageBuffer(_) => 3,
    }
}
fn topology(t: Topology) -> MTLPrimitiveType {
    match t {
        Topology::Points => MTLPrimitiveType::Point,
        Topology::Lines => MTLPrimitiveType::Line,
        Topology::LineStrip => MTLPrimitiveType::LineStrip,
        Topology::Triangles => MTLPrimitiveType::Triangle,
        Topology::TriangleStrip => MTLPrimitiveType::TriangleStrip,
    }
}
fn index_type(f: IndexFormat) -> (MTLIndexType, usize) {
    match f {
        IndexFormat::U16 => (MTLIndexType::UInt16, 2),
        IndexFormat::U32 => (MTLIndexType::UInt32, 4),
    }
}

struct Bound {
    buffers: [Option<(usize, u64, u64)>; 31],
    textures: [usize; 128],
    samplers: [usize; 16],
    sizes: Option<(u8, Vec<u32>)>,
}
impl Default for Bound {
    fn default() -> Self {
        Self {
            buffers: [None; 31],
            textures: [0; 128],
            samplers: [0; 16],
            sizes: None,
        }
    }
}
impl Bound {
    fn set(&mut self, index: usize, resource: Bind<'_>) -> bool {
        match resource {
            Bind::Uniform(b) | Bind::StorageBuffer(b) => {
                let value = (
                    Retained::as_ptr(&b.buffer.inner.0.raw) as usize,
                    b.offset,
                    b.size,
                );
                let changed =
                    self.buffers[index].is_none_or(|old| (old.0, old.1) != (value.0, value.1));
                self.buffers[index] = Some(value);
                if changed && self.sizes.as_ref().is_some_and(|s| s.0 as usize == index) {
                    self.sizes = None;
                }
                return changed;
            }
            Bind::Sampled(image) | Bind::StorageImage(image) => {
                let value = Retained::as_ptr(&image.inner.raw) as usize;
                if self.textures[index] == value {
                    return false;
                }
                self.textures[index] = value;
            }
            Bind::Sampler(sampler) => {
                let value = Retained::as_ptr(&sampler.inner.0) as usize;
                if self.samplers[index] == value {
                    return false;
                }
                self.samplers[index] = value;
            }
        }
        true
    }
    fn sizes(&mut self, layout: Option<&shader::BufferSizes>) -> Option<(usize, &[u32])> {
        let shader::BufferSizes { index, slots } = layout?;
        let values: Vec<_> = slots
            .iter()
            .map(|slot| {
                slot.and_then(|slot| self.buffers[slot as usize])
                    .map_or(0, |b| b.2 as u32)
            })
            .collect();
        if self
            .sizes
            .as_ref()
            .is_some_and(|s| s.0 == *index && s.1 == values)
        {
            return None;
        }
        self.buffers[*index as usize] = None;
        self.sizes = Some((*index, values));
        Some((*index as usize, &self.sizes.as_ref().unwrap().1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_cache_tracks_slice_sizes_and_shared_buffer_indices() {
        let mut bound = Bound::default();
        let sizes = shader::BufferSizes {
            index: 29,
            slots: vec![Some(0), None],
        };
        bound.buffers[0] = Some((1, 16, 64));
        assert_eq!(bound.sizes(Some(&sizes)), Some((29, &[64, 0][..])));
        assert!(bound.sizes(Some(&sizes)).is_none());
        bound.buffers[0] = Some((1, 16, 32));
        assert_eq!(bound.sizes(Some(&sizes)), Some((29, &[32, 0][..])));
        // Another pipeline uses this index as an ordinary buffer, then the
        // runtime-array pipeline returns and must restore its size table.
        bound.sizes = None;
        bound.buffers[29] = Some((2, 0, 16));
        assert_eq!(bound.sizes(Some(&sizes)), Some((29, &[32, 0][..])));
        assert_eq!(bound.buffers[29], None);
    }
}
