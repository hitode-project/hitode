//! Command encoding, on the GPU owner thread only.
//!
//! One encoder is open per queue at a time and submits in creation order, so
//! encoding order is execution order. The Vulkan backend derives every image
//! layout transition and barrier from that order and each image's last use;
//! callers never write barriers. The Metal backend needs none for a single
//! queue with tracked resources.

use crate::pipeline::{ComputePipeline, MAX_COLOR_TARGETS, RenderPipeline};
use crate::queue::QuerySet;
use crate::resource::BufferSlice;
use crate::{Buffer, Extent, Image, ImageView, Rect, Sampler, Span};
use arrayvec::ArrayVec;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Load<T> {
    Load,
    Clear(T),
    /// Contents may be discarded. Use when the pass covers its area opaquely
    /// before reading it.
    DontCare,
    /// The pass never reads or writes this attachment; it is bound only for
    /// pipeline compatibility. Contents are preserved and no tile memory
    /// traffic is paid (`LOAD_OP_NONE`; Metal leaves it unattached; drivers
    /// without load/store op none load it).
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Store {
    Store,
    /// Not written by this pass; contents preserved without a store
    /// (`STORE_OP_NONE`).
    None,
    DontCare,
}

pub struct ColorAttachment<'a> {
    pub view: &'a ImageView,
    pub load: Load<[f64; 4]>,
    pub store: Store,
    /// Multisample resolve target, written at the end of the pass.
    pub resolve: Option<&'a ImageView>,
}

pub struct DepthAttachment<'a> {
    pub view: &'a ImageView,
    pub depth: (Load<f32>, Store),
    pub stencil: (Load<u32>, Store),
    /// Bound for testing only; allows sampling the same image in the pass.
    pub read_only: bool,
}

pub struct RenderPassDesc<'a> {
    pub colors: ArrayVec<ColorAttachment<'a>, MAX_COLOR_TARGETS>,
    pub depth_stencil: Option<DepthAttachment<'a>>,
    /// The only region the pass may change, normally the damage being redrawn.
    /// Load operations (including `Clear` and `DontCare`), draws and stores
    /// apply inside it only; every attachment keeps its contents outside it.
    /// Vulkan passes it as `renderArea`. Metal clamps every scissor to it, and
    /// where its load actions would act on the whole attachment (a `Clear` or
    /// `DontCare` with an area smaller than the target) it loads and clears
    /// the area with a draw instead. Loads and stores outside it are not paid
    /// on either backend.
    pub area: Rect,
    /// Write GPU timestamps at the pass's start and end into `(set, first)` and
    /// `(set, first + 1)`. Needs [`crate::Features::TIMESTAMPS`].
    pub timestamps: Option<(&'a QuerySet, u32)>,
    pub label: &'static str,
}

pub struct ComputePassDesc<'a> {
    pub timestamps: Option<(&'a QuerySet, u32)>,
    pub label: &'static str,
}

/// A resource bound to a slot of the current pipeline's binding list.
///
/// On the modern Vulkan tier every image, buffer and sampler holds a fixed
/// index in one bindless heap (descriptor indexing, update after bind), and
/// `bind` writes that index and any offset into the draw's push constants: no
/// descriptor work per draw. Older drivers use push descriptors (Adreno 6xx)
/// or descriptor sets cached by content (Mali G76). On Metal `bind` is a
/// direct `set*` call, the cheapest binding on Apple5 (argument buffers are
/// only tier 1 there). Vertex streams bind as storage buffers.
#[derive(Clone, Copy)]
pub enum Bind<'a> {
    Uniform(BufferSlice<'a>),
    Sampled(&'a ImageView),
    Sampler(&'a Sampler),
    StorageImage(&'a ImageView),
    StorageBuffer(BufferSlice<'a>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexFormat {
    U16,
    U32,
}

#[derive(Clone, Copy)]
pub struct ImageCopy<'a> {
    pub image: &'a Image,
    pub mip: u32,
    pub layer: u32,
    pub origin: (u32, u32, u32),
}

#[derive(Clone, Copy)]
pub struct BufferCopy<'a> {
    pub buffer: &'a Buffer,
    pub offset: u64,
    /// Bytes per row and rows per layer; rows are packed when zero.
    pub row_pitch: u32,
    pub rows: u32,
}

pub struct Encoder {
    pub(crate) inner: crate::imp::Encoder,
}

impl Encoder {
    pub fn render_pass<'e>(&'e mut self, desc: &RenderPassDesc<'_>) -> RenderPass<'e> {
        self.inner.render_pass(desc)
    }
    pub fn compute_pass<'e>(&'e mut self, desc: &ComputePassDesc<'_>) -> ComputePass<'e> {
        self.inner.compute_pass(desc)
    }
    pub fn copy_buffer_to_image(&mut self, src: BufferCopy<'_>, dst: ImageCopy<'_>, size: Extent) {
        self.inner.copy_buffer_to_image(src, dst, size)
    }
    pub fn copy_image_to_buffer(&mut self, src: ImageCopy<'_>, dst: BufferCopy<'_>, size: Extent) {
        self.inner.copy_image_to_buffer(src, dst, size)
    }
    pub fn copy_image(&mut self, src: ImageCopy<'_>, dst: ImageCopy<'_>, size: Extent) {
        self.inner.copy_image(src, dst, size)
    }
    pub fn copy_buffer(&mut self, src: BufferSlice<'_>, dst: &Buffer, dst_offset: u64) {
        self.inner.copy_buffer(src, dst, dst_offset)
    }
    /// Fill a buffer range with a 32-bit pattern.
    pub fn fill_buffer(&mut self, dst: BufferSlice<'_>, value: u32) {
        self.inner.fill_buffer(dst, value)
    }
    /// Keep ring spans alive until this submission completes.
    pub fn retain(&mut self, span: Span) {
        self.inner.retain(span)
    }
    pub fn resolve_queries(
        &mut self,
        set: &QuerySet,
        range: std::ops::Range<u32>,
        dst: &Buffer,
        offset: u64,
    ) {
        self.inner.resolve_queries(set, range, dst, offset)
    }
}

/// Dropping a pass ends it.
pub struct RenderPass<'e> {
    pub(crate) inner: crate::imp::RenderPass<'e>,
}

impl RenderPass<'_> {
    pub fn set_pipeline(&mut self, pipeline: &RenderPipeline) {
        self.inner.set_pipeline(pipeline)
    }
    pub fn bind(&mut self, slot: u32, resource: Bind<'_>) {
        self.inner.bind(slot, resource)
    }
    pub fn set_index_buffer(&mut self, buffer: BufferSlice<'_>, format: IndexFormat) {
        self.inner.set_index_buffer(buffer, format)
    }
    pub fn set_viewport(&mut self, x: f32, y: f32, w: f32, h: f32, min_depth: f32, max_depth: f32) {
        self.inner.set_viewport(x, y, w, h, min_depth, max_depth)
    }
    /// Clamped to the pass area.
    pub fn set_scissor(&mut self, rect: Rect) {
        self.inner.set_scissor(rect)
    }
    pub fn set_stencil_reference(&mut self, value: u32) {
        self.inner.set_stencil_reference(value)
    }
    pub fn set_blend_constant(&mut self, rgba: [f32; 4]) {
        self.inner.set_blend_constant(rgba)
    }
    pub fn set_depth_bias(&mut self, constant: f32, slope: f32, clamp: f32) {
        self.inner.set_depth_bias(constant, slope, clamp)
    }
    pub fn draw(&mut self, vertices: std::ops::Range<u32>, instances: std::ops::Range<u32>) {
        self.inner.draw(vertices, instances)
    }
    pub fn draw_indexed(
        &mut self,
        indices: std::ops::Range<u32>,
        base_vertex: i32,
        instances: std::ops::Range<u32>,
    ) {
        self.inner.draw_indexed(indices, base_vertex, instances)
    }
    /// Needs [`crate::Features::INDIRECT`].
    pub fn draw_indirect(&mut self, args: BufferSlice<'_>, indexed: bool) {
        self.inner.draw_indirect(args, indexed)
    }
    pub fn begin_occlusion(&mut self, set: &QuerySet, index: u32) {
        self.inner.begin_occlusion(set, index)
    }
    pub fn end_occlusion(&mut self) {
        self.inner.end_occlusion()
    }
}

pub struct ComputePass<'e> {
    pub(crate) inner: crate::imp::ComputePass<'e>,
}

impl ComputePass<'_> {
    pub fn set_pipeline(&mut self, pipeline: &ComputePipeline) {
        self.inner.set_pipeline(pipeline)
    }
    pub fn bind(&mut self, slot: u32, resource: Bind<'_>) {
        self.inner.bind(slot, resource)
    }
    pub fn dispatch(&mut self, x: u32, y: u32, z: u32) {
        self.inner.dispatch(x, y, z)
    }
    /// Needs [`crate::Features::INDIRECT`]. Tile lists use this.
    pub fn dispatch_indirect(&mut self, args: BufferSlice<'_>) {
        self.inner.dispatch_indirect(args)
    }
}
