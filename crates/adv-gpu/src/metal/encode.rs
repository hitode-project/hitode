use super::*;
use std::ops::Range;

pub(crate) struct Encoder {
    pub(super) device: Device,
    pub(super) command: Object<dyn MTLCommandBuffer>,
    pub(super) spans: Vec<crate::Span>,
    visibility: Option<Object<dyn MTLBuffer>>,
    visibility_offset: usize,
    // Counter sample buffers do not participate in tracked-resource hazards.
    // Order stage-boundary writes before a following counter resolve blit.
    counter_fence: Option<Object<dyn MTLFence>>,
}

pub(crate) struct RenderPass<'e> {
    pub(super) raw: Object<dyn MTLRenderCommandEncoder>,
    encoder: &'e mut Encoder,
    area: Rect,
    timestamps: bool,
    occlusion: Vec<(Object<dyn MTLBuffer>, usize, usize)>,
    resolves: Vec<(Object<dyn MTLTexture>, Object<dyn MTLTexture>)>,
}
pub(crate) struct ComputePass<'e> {
    #[allow(dead_code)] // B2 encodes compute commands here.
    pub(super) raw: Object<dyn MTLComputeCommandEncoder>,
    encoder: &'e mut Encoder,
    timestamps: bool,
}

impl Encoder {
    pub(super) fn new(device: Device, command: Object<dyn MTLCommandBuffer>) -> Self {
        Self {
            device,
            command,
            spans: Vec::new(),
            visibility: None,
            visibility_offset: 0,
            counter_fence: None,
        }
    }
    fn blit(&self) -> Object<dyn MTLBlitCommandEncoder> {
        let blit = self
            .command
            .blitCommandEncoder()
            .expect("Metal blit encoder");
        if let Some(fence) = &self.counter_fence {
            blit.waitForFence(fence);
        }
        blit
    }
    pub(crate) fn render_pass<'e>(
        &'e mut self,
        desc: &RenderPassDesc<'_>,
    ) -> crate::RenderPass<'e> {
        let d = MTLRenderPassDescriptor::new();
        if desc.timestamps.is_some() && self.counter_fence.is_none() {
            self.counter_fence = Some(self.device.0.raw.newFence().expect("Metal counter fence"));
        }
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
        self.device.clear(&raw, desc, area);
        crate::RenderPass {
            inner: RenderPass {
                raw,
                encoder: self,
                area,
                timestamps: desc.timestamps.is_some(),
                occlusion: Vec::new(),
                resolves,
            },
        }
    }
    pub(crate) fn compute_pass<'e>(
        &'e mut self,
        desc: &ComputePassDesc<'_>,
    ) -> crate::ComputePass<'e> {
        let d = MTLComputePassDescriptor::new();
        if desc.timestamps.is_some() && self.counter_fence.is_none() {
            self.counter_fence = Some(self.device.0.raw.newFence().expect("Metal counter fence"));
        }
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
        if desc.timestamps.is_some() {
            // Empty compute encoders can omit their counter writes. A tiny
            // tracked write keeps the stage boundaries present for sampling.
            let (tick, scratch) = self.device.0.timestamp_tick.get_or_init(|| {
                let library = self.device.0.raw.newLibraryWithSource_options_error(
                    &NSString::from_str("#include <metal_stdlib>\nusing namespace metal; kernel void timestamp_tick(device uint* value [[buffer(0)]]) { *value = 0; }"), None)
                    .expect("Metal timestamp shader");
                let function = library.newFunctionWithName(&NSString::from_str("timestamp_tick")).unwrap();
                let pipeline = self.device.0.raw.newComputePipelineStateWithFunction_error(&function)
                    .expect("Metal timestamp pipeline");
                let scratch = self.device.create_buffer(&BufferDesc { size: 4,
                    usage: BufferUsage::STORAGE, memory: Memory::Device, label: "timestamp tick" })
                    .expect("Metal timestamp scratch");
                (pipeline, scratch)
            });
            raw.setComputePipelineState(tick);
            unsafe {
                raw.setBuffer_offset_atIndex(Some(&scratch.inner.0.raw), 0, 0);
            }
            raw.dispatchThreadgroups_threadsPerThreadgroup(
                metal_size(1, 1, 1),
                metal_size(1, 1, 1),
            );
        }
        crate::ComputePass {
            inner: ComputePass {
                raw,
                encoder: self,
                timestamps: desc.timestamps.is_some(),
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
        let blit = self.blit();
        unsafe {
            match &*set.inner.0 {
                QueryData::Timestamp { raw, .. } => blit
                    .resolveCounters_inRange_destinationBuffer_destinationOffset(
                        raw,
                        NSRange::new(range.start as usize, range.len()),
                        &dst.inner.0.raw,
                        offset as usize,
                    ),
                QueryData::Occlusion(raw) => blit
                    .copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                        raw,
                        range.start as usize * 8,
                        &dst.inner.0.raw,
                        offset as usize,
                        range.len() * 8,
                    ),
            }
        }
        blit.endEncoding();
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
        if self.timestamps {
            self.raw.updateFence_afterStages(
                self.encoder.counter_fence.as_ref().unwrap(),
                MTLRenderStages::Vertex | MTLRenderStages::Fragment,
            );
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
    pub(crate) fn set_pipeline(&mut self, _pipeline: &crate::RenderPipeline) {
        todo!("B2")
    }
    pub(crate) fn bind(&mut self, _slot: u32, _resource: Bind<'_>) {
        todo!("B2")
    }
    pub(crate) fn set_index_buffer(&mut self, _buffer: BufferSlice<'_>, _format: IndexFormat) {
        todo!("B2")
    }
    pub(crate) fn draw(&mut self, _vertices: Range<u32>, _instances: Range<u32>) {
        todo!("B2")
    }
    pub(crate) fn draw_indexed(
        &mut self,
        _indices: Range<u32>,
        _base_vertex: i32,
        _instances: Range<u32>,
    ) {
        todo!("B2")
    }
    pub(crate) fn draw_indirect(&mut self, _args: BufferSlice<'_>, _indexed: bool) {
        todo!("B2")
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
        self.raw
            .setDepthBias_slopeScale_clamp(constant, slope, clamp);
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
        if self.timestamps {
            self.raw
                .updateFence(self.encoder.counter_fence.as_ref().unwrap());
        }
        self.raw.endEncoding();
    }
}
impl ComputePass<'_> {
    pub(crate) fn set_pipeline(&mut self, _pipeline: &crate::ComputePipeline) {
        todo!("B2")
    }
    pub(crate) fn bind(&mut self, _slot: u32, _resource: Bind<'_>) {
        todo!("B2")
    }
    pub(crate) fn dispatch(&mut self, _x: u32, _y: u32, _z: u32) {
        todo!("B2")
    }
    pub(crate) fn dispatch_indirect(&mut self, _args: BufferSlice<'_>) {
        todo!("B2")
    }
}
