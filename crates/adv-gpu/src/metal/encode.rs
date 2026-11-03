use super::pipeline::{ComputeData, Raster, RenderData};
use super::shader::{self, Used};
use super::*;
use arrayvec::ArrayVec;
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
    pipeline: Option<Arc<RenderData>>,
    raster: Raster,
    bindings: Box<Bindings>,
    index: Option<(Object<dyn MTLBuffer>, usize, IndexFormat)>,
    depth_bias: (f32, f32, f32),
}
pub(crate) struct ComputePass<'e> {
    pipeline: Option<Arc<ComputeData>>,
    bindings: Box<Bindings>,
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
            if !attached(color.load, color.store) {
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
            if attached(depth.depth.0, depth.depth.1) {
                let attachment = d.depthAttachment();
                attachment.setTexture(Some(&view.raw));
                attachment.setLoadAction(load(depth.depth.0, bounded));
                attachment.setStoreAction(store(depth.depth.1, bounded));
                if let Load::Clear(value) = depth.depth.0 {
                    attachment.setClearDepth(f64::from(value));
                }
            }
            if view.raw.pixelFormat() == MTLPixelFormat::Depth32Float_Stencil8
                && attached(depth.stencil.0, depth.stencil.1)
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
                raster: Raster::DEFAULT,
                bindings: Bindings::new(),
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
                bindings: Bindings::new(),
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
/// Whether the pass binds the attachment: Metal leaves out one the pass
/// neither reads nor keeps. A read-only one (`Load` with `Store::None`) stays.
pub(super) fn attached<T>(load: Load<T>, store: Store) -> bool {
    matches!(load, Load::Load) || !matches!(load, Load::None) && store != Store::None
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
        let p = &pipeline.inner.0;
        let old = self.pipeline.as_deref();
        if old.is_some_and(|old| std::ptr::eq(old, &**p)) {
            return;
        }
        if old.is_none_or(|old| !same(&old.raw, &p.raw)) {
            self.raw.setRenderPipelineState(&p.raw);
        }
        if old.is_none_or(|old| !same(&old.depth, &p.depth)) {
            self.raw.setDepthStencilState(Some(&p.depth));
        }
        let (old, new) = (self.raster, p.raster);
        if old.cull != new.cull {
            self.raw.setCullMode(new.cull);
        }
        if old.winding != new.winding {
            self.raw.setFrontFacingWinding(new.winding);
        }
        if old.fill != new.fill {
            self.raw.setTriangleFillMode(new.fill);
        }
        if old.clip != new.clip {
            self.raw.setDepthClipMode(new.clip);
        }
        if old.depth_bias != new.depth_bias && self.depth_bias != (0.0, 0.0, 0.0) {
            let (constant, slope, clamp) = if new.depth_bias {
                self.depth_bias
            } else {
                (0.0, 0.0, 0.0)
            };
            self.raw
                .setDepthBias_slopeScale_clamp(constant, slope, clamp);
        }
        self.raster = new;
        self.bindings.sizes_dirty |= p.sizes.iter().any(Option::is_some);
        self.pipeline = Some(p.clone());
    }
    pub(crate) fn bind(&mut self, slot: u32, resource: Bind<'_>) {
        self.bindings.bind(slot, resource);
    }
    /// Gives each stage the bindings it reads that changed since its last draw.
    fn flush(&mut self) -> MTLPrimitiveType {
        let p = self
            .pipeline
            .as_deref()
            .expect("render pipeline before draw");
        let sizes = std::mem::take(&mut self.bindings.sizes_dirty);
        let raw = &*self.raw;
        let table = |stage: usize| p.sizes[stage].as_deref().filter(|_| sizes);
        self.bindings.flush(0, p.used[0], table(0), &Vertex(raw));
        self.bindings.flush(1, p.used[1], table(1), &Fragment(raw));
        p.topology
    }
    pub(crate) fn set_index_buffer(&mut self, buffer: BufferSlice<'_>, format: IndexFormat) {
        let raw = &buffer.buffer.inner.0.raw;
        let offset = buffer.offset as usize;
        match &mut self.index {
            Some((old, o, f)) if same(old, raw) => (*o, *f) = (offset, format),
            index => *index = Some((raw.clone(), offset, format)),
        }
    }
    pub(crate) fn draw(&mut self, vertices: Range<u32>, instances: Range<u32>) {
        self.work = true;
        let topology = self.flush();
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
        let topology = self.flush();
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
        let topology = self.flush();
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
        if self.raster.depth_bias {
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
        let p = &pipeline.inner.0;
        if self
            .pipeline
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, p))
        {
            return;
        }
        if self
            .pipeline
            .as_ref()
            .is_none_or(|old| !same(&old.raw, &p.raw))
        {
            self.raw.setComputePipelineState(&p.raw);
        }
        self.bindings.sizes_dirty |= p.sizes.is_some();
        self.pipeline = Some(p.clone());
    }
    pub(crate) fn bind(&mut self, slot: u32, resource: Bind<'_>) {
        self.bindings.bind(slot, resource);
    }
    fn flush(&mut self) -> MTLSize {
        let p = self
            .pipeline
            .as_deref()
            .expect("compute pipeline before dispatch");
        let sizes = std::mem::take(&mut self.bindings.sizes_dirty);
        let table = p.sizes.as_deref().filter(|_| sizes);
        self.bindings.flush(0, p.used, table, &Compute(&self.raw));
        p.workgroup
    }
    pub(crate) fn dispatch(&mut self, x: u32, y: u32, z: u32) {
        self.work = true;
        let workgroup = self.flush();
        self.raw
            .dispatchThreadgroups_threadsPerThreadgroup(metal_size(x, y, z), workgroup);
    }
    pub(crate) fn dispatch_indirect(&mut self, args: BufferSlice<'_>) {
        self.work = true;
        let workgroup = self.flush();
        unsafe {
            self.raw
                .dispatchThreadgroupsWithIndirectBuffer_indirectBufferOffset_threadsPerThreadgroup(
                    &args.buffer.inner.0.raw,
                    args.offset as usize,
                    workgroup,
                );
        }
    }
}

fn index_type(f: IndexFormat) -> (MTLIndexType, usize) {
    match f {
        IndexFormat::U16 => (MTLIndexType::UInt16, 2),
        IndexFormat::U32 => (MTLIndexType::UInt32, 4),
    }
}
fn same<T: objc2::Message + ?Sized>(a: &Retained<T>, b: &Retained<T>) -> bool {
    std::ptr::eq(Retained::as_ptr(a), Retained::as_ptr(b))
}

/// What a pass has bound at each native index (fixed per slot, see
/// [`shader::index`]), and which of it each stage has yet to receive. `bind`
/// only records; draws send each stage the changed indices it reads, so
/// slots persist across pipelines and may be bound before one is set.
struct Bindings {
    buffers: [Option<BufferBinding>; 31],
    textures: [Option<Object<dyn MTLTexture>>; 128],
    samplers: [Option<Object<dyn MTLSamplerState>>; 32],
    sampler_slots: [u32; 2],
    /// Per stage: indices whose resource changed since the stage last drew.
    dirty: [Used; 2],
    /// Per stage: buffer indices whose object (not just offset) changed.
    moved: [u32; 2],
    /// A buffer size changed or a pipeline with a size table was set.
    sizes_dirty: bool,
    /// Per stage: the size table last written to `buffer(SIZES)`.
    sent: [Option<ArrayVec<u32, { shader::SIZES }>>; 2],
}

struct BufferBinding {
    raw: Object<dyn MTLBuffer>,
    offset: u64,
    size: u64,
}

/// One stage of an encoder, as `Bindings::flush` writes to it.
trait Target {
    fn buffer(&self, buffer: &ProtocolObject<dyn MTLBuffer>, offset: usize, index: usize);
    fn offset(&self, offset: usize, index: usize);
    fn texture(&self, texture: &ProtocolObject<dyn MTLTexture>, index: usize);
    fn sampler(&self, sampler: &ProtocolObject<dyn MTLSamplerState>, index: usize);
    fn bytes(&self, bytes: &[u32], index: usize);
}
struct Vertex<'a>(&'a ProtocolObject<dyn MTLRenderCommandEncoder>);
struct Fragment<'a>(&'a ProtocolObject<dyn MTLRenderCommandEncoder>);
struct Compute<'a>(&'a ProtocolObject<dyn MTLComputeCommandEncoder>);
fn bytes(bytes: &[u32]) -> (NonNull<std::ffi::c_void>, usize) {
    (NonNull::from(bytes).cast(), std::mem::size_of_val(bytes))
}
macro_rules! target {
    ($ty:ident, $buffer:ident, $offset:ident, $texture:ident, $sampler:ident, $bytes:ident) => {
        impl Target for $ty<'_> {
            fn buffer(&self, buffer: &ProtocolObject<dyn MTLBuffer>, offset: usize, index: usize) {
                unsafe { self.0.$buffer(Some(buffer), offset, index) }
            }
            fn offset(&self, offset: usize, index: usize) {
                unsafe { self.0.$offset(offset, index) }
            }
            fn texture(&self, texture: &ProtocolObject<dyn MTLTexture>, index: usize) {
                unsafe { self.0.$texture(Some(texture), index) }
            }
            fn sampler(&self, sampler: &ProtocolObject<dyn MTLSamplerState>, index: usize) {
                unsafe { self.0.$sampler(Some(sampler), index) }
            }
            fn bytes(&self, data: &[u32], index: usize) {
                let (pointer, length) = bytes(data);
                unsafe { self.0.$bytes(pointer, length, index) }
            }
        }
    };
}
target!(
    Vertex,
    setVertexBuffer_offset_atIndex,
    setVertexBufferOffset_atIndex,
    setVertexTexture_atIndex,
    setVertexSamplerState_atIndex,
    setVertexBytes_length_atIndex
);
target!(
    Fragment,
    setFragmentBuffer_offset_atIndex,
    setFragmentBufferOffset_atIndex,
    setFragmentTexture_atIndex,
    setFragmentSamplerState_atIndex,
    setFragmentBytes_length_atIndex
);
target!(
    Compute,
    setBuffer_offset_atIndex,
    setBufferOffset_atIndex,
    setTexture_atIndex,
    setSamplerState_atIndex,
    setBytes_length_atIndex
);

impl Bindings {
    fn new() -> Box<Self> {
        Box::new(Self {
            buffers: std::array::from_fn(|_| None),
            textures: std::array::from_fn(|_| None),
            samplers: std::array::from_fn(|_| None),
            sampler_slots: [0; 2],
            dirty: [Used::default(); 2],
            moved: [0; 2],
            sizes_dirty: false,
            sent: [None, None],
        })
    }
    fn bind(&mut self, slot: u32, resource: Bind<'_>) {
        let index;
        match resource {
            Bind::Uniform(b) | Bind::StorageBuffer(b) => {
                let group = if matches!(resource, Bind::Uniform(_)) {
                    0
                } else {
                    3
                };
                index = shader::index(group, slot, true).1;
                let raw = &b.buffer.inner.0.raw;
                match &mut self.buffers[index as usize] {
                    Some(old) if same(&old.raw, raw) => {
                        if (old.offset, old.size) == (b.offset, b.size) {
                            return;
                        }
                        self.sizes_dirty |= old.size != b.size;
                        (old.offset, old.size) = (b.offset, b.size);
                    }
                    entry => {
                        *entry = Some(BufferBinding {
                            raw: raw.clone(),
                            offset: b.offset,
                            size: b.size,
                        });
                        self.sizes_dirty = true;
                        self.moved[0] |= 1 << index;
                        self.moved[1] |= 1 << index;
                    }
                }
                self.dirty[0].buffers |= 1 << index;
                self.dirty[1].buffers |= 1 << index;
            }
            Bind::Sampled(image) | Bind::StorageImage(image) => {
                let group = if matches!(resource, Bind::Sampled(_)) {
                    1
                } else {
                    3
                };
                index = shader::index(group, slot, false).1;
                let entry = &mut self.textures[index as usize];
                if entry
                    .as_ref()
                    .is_some_and(|old| same(old, &image.inner.raw))
                {
                    return;
                }
                *entry = Some(image.inner.raw.clone());
                self.dirty[0].textures |= 1 << index;
                self.dirty[1].textures |= 1 << index;
            }
            Bind::Sampler(sampler) => {
                index = shader::index(2, slot, false).1;
                let entry = &mut self.samplers[index as usize];
                if entry
                    .as_ref()
                    .is_some_and(|old| same(old, &*sampler.inner.0))
                {
                    return;
                }
                *entry = Some((*sampler.inner.0).clone());
                self.dirty[0].samplers |= 1 << index;
                self.dirty[1].samplers |= 1 << index;
            }
        }
    }
    /// Sends `stage` the changed indices it reads, and its size table when
    /// `sizes` is given and differs from the last one sent.
    fn flush(&mut self, stage: usize, used: Used, sizes: Option<&[Option<u8>]>, to: &impl Target) {
        let dirty = &mut self.dirty[stage];
        // A pipeline can change which logical slot owns a physical sampler index.
        dirty.samplers |= used.samplers & !self.sampler_slots[stage];
        self.sampler_slots[stage] = used.samplers;
        let mut buffers = dirty.buffers & used.buffers;
        let mut textures = dirty.textures & used.textures;
        let mut samplers = dirty.samplers & used.samplers;
        dirty.buffers &= !buffers;
        dirty.textures &= !textures;
        dirty.samplers &= !samplers;
        let moved = self.moved[stage] & buffers;
        self.moved[stage] &= !buffers;
        while buffers != 0 {
            let i = buffers.trailing_zeros() as usize;
            buffers &= buffers - 1;
            let buffer = self.buffers[i].as_ref().unwrap();
            if moved & 1 << i != 0 {
                to.buffer(&buffer.raw, buffer.offset as usize, i);
            } else {
                to.offset(buffer.offset as usize, i);
            }
        }
        while textures != 0 {
            let i = textures.trailing_zeros() as usize;
            textures &= textures - 1;
            to.texture(self.textures[i].as_ref().unwrap(), i);
        }
        while samplers != 0 {
            let i = samplers.trailing_zeros() as usize;
            samplers &= samplers - 1;
            to.sampler(
                self.samplers[i].as_ref().unwrap(),
                crate::shader::compile::sampler_index(i as u32) as usize,
            );
        }
        if let Some(slots) = sizes {
            let values: ArrayVec<u32, { shader::SIZES }> = slots
                .iter()
                .map(|slot| {
                    slot.and_then(|i| self.buffers[i as usize].as_ref())
                        .map_or(0, |b| b.size as u32)
                })
                .collect();
            if self.sent[stage].as_ref() != Some(&values) {
                to.bytes(&values, shader::SIZES);
                self.sent[stage] = Some(values);
            }
        }
    }
}
