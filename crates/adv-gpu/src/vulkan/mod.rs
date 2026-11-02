//! Stub backend: every type the public layer wraps, with `todo!` bodies.
//! The backend package replaces this file's contents; the public layer in
//! the crate root stays as it is.
#![allow(dead_code, unused_imports, clippy::unused_self)]

use crate::caps::*;
use crate::encode::*;
use crate::pipeline::*;
use crate::present::*;
use crate::queue::*;
use crate::resource::*;
use crate::shader::*;
use crate::{Extent, Rect, Result};
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct Device;

impl Device {
    pub(crate) fn open(_options: &DeviceOptions) -> Result<(crate::Device, crate::Queue)> {
        Err(crate::Error::Unavailable(
            "no backend on this target yet".into(),
        ))
    }

    pub(crate) fn caps(&self) -> &Caps {
        todo!()
    }

    pub(crate) fn create_image(&self, _desc: &ImageDesc) -> Result<crate::Image> {
        todo!()
    }

    pub(crate) fn create_buffer(&self, _desc: &BufferDesc) -> Result<crate::Buffer> {
        todo!()
    }

    pub(crate) fn sampler(&self, _desc: &SamplerDesc) -> Result<crate::Sampler> {
        todo!()
    }

    pub(crate) fn create_shader(&self, _desc: ShaderDesc<'_>) -> Result<crate::Shader> {
        todo!()
    }

    pub(crate) fn create_render_pipeline(
        &self,
        _desc: &RenderPipelineDesc,
        _cache: Option<&crate::pipeline::PipelineCache>,
    ) -> Result<crate::RenderPipeline> {
        todo!()
    }

    pub(crate) fn pipeline_identity(&self, _desc: &RenderPipelineDesc) -> u64 {
        todo!()
    }

    pub(crate) fn create_compute_pipeline(
        &self,
        _desc: &ComputePipelineDesc,
        _cache: Option<&crate::pipeline::PipelineCache>,
    ) -> Result<crate::ComputePipeline> {
        todo!()
    }

    pub(crate) fn ring(&self, _size: u64) -> Result<crate::Ring> {
        todo!()
    }

    pub(crate) fn create_present_pool(&self, _desc: &PoolDesc) -> Result<crate::PresentPool> {
        todo!()
    }

    pub(crate) fn create_query_set(
        &self,
        _kind: QueryKind,
        _count: u32,
    ) -> Result<crate::queue::QuerySet> {
        todo!()
    }

    pub(crate) fn lost(&self) -> Option<String> {
        todo!()
    }
}

#[derive(Clone)]
pub(crate) struct Queue;

impl Queue {
    pub(crate) fn device(&self) -> &crate::Device {
        todo!()
    }

    pub(crate) fn encoder(&mut self, _label: &'static str) -> crate::Encoder {
        todo!()
    }

    pub(crate) fn submit(&mut self, _encoder: crate::Encoder) -> Result<Token> {
        todo!()
    }

    pub(crate) fn poll(&mut self) -> Result<Token> {
        todo!()
    }

    pub(crate) fn wait(&mut self, _token: Token, _timeout: Duration) -> Result<bool> {
        todo!()
    }

    pub(crate) fn on_complete(&mut self, _token: Token, _callback: Box<dyn FnOnce() + Send>) {
        todo!()
    }

    pub(crate) fn export_fence(&mut self, _token: Token) -> Result<crate::queue::Fence> {
        todo!()
    }

    pub(crate) fn wait_fence(&mut self, _fence: crate::queue::Fence) -> Result<()> {
        todo!()
    }
}

#[derive(Clone)]
pub(crate) struct Fence;

impl Fence {
    #[cfg(unix)]
    pub(crate) fn into_fd(self) -> Option<std::os::fd::OwnedFd> {
        todo!()
    }

    #[cfg(unix)]
    pub(crate) fn from_fd(_fd: std::os::fd::OwnedFd) -> crate::queue::Fence {
        todo!()
    }
}

#[derive(Clone)]
pub(crate) struct QuerySet;

impl QuerySet {
    pub(crate) fn period(&self) -> f64 {
        todo!()
    }
}

#[derive(Clone)]
pub(crate) struct Image;

impl Image {
    pub(crate) fn desc(&self) -> &ImageDesc {
        todo!()
    }

    pub(crate) fn view(&self) -> &crate::ImageView {
        todo!()
    }

    pub(crate) fn create_view(&self, _desc: &ViewDesc) -> Result<crate::ImageView> {
        todo!()
    }

    pub(crate) fn id(&self) -> u64 {
        todo!()
    }
}

#[derive(Clone)]
pub(crate) struct ImageView;

impl ImageView {
    pub(crate) fn image(&self) -> &crate::Image {
        todo!()
    }

    pub(crate) fn size(&self) -> Extent {
        todo!()
    }
}

#[derive(Clone)]
pub(crate) struct Buffer;

impl Buffer {
    pub(crate) fn desc(&self) -> &BufferDesc {
        todo!()
    }

    pub(crate) fn mapped(&self) -> Option<std::ptr::NonNull<[u8]>> {
        todo!()
    }

    pub(crate) fn slice(&self, _range: std::ops::Range<u64>) -> BufferSlice<'_> {
        todo!()
    }
}

#[derive(Clone)]
pub(crate) struct Sampler;

#[derive(Clone)]
pub(crate) struct Shader;

impl Shader {
    pub(crate) fn entry(&self, _name: &str, _stage: Stage) -> Result<Entry> {
        todo!()
    }
}

#[derive(Clone)]
pub(crate) struct RenderPipeline;

#[derive(Clone)]
pub(crate) struct ComputePipeline;

#[derive(Clone)]
pub(crate) struct PipelineCache;

impl PipelineCache {
    pub(crate) fn load(_data: &[u8]) -> crate::pipeline::PipelineCache {
        todo!()
    }

    pub(crate) fn serialize(&self) -> Result<Vec<u8>> {
        todo!()
    }
}

#[derive(Clone)]
pub(crate) struct Ring;

impl Ring {
    pub(crate) fn reserve(&mut self, _size: u64, _align: u64) -> Result<crate::Span> {
        todo!()
    }
}

#[derive(Clone)]
pub(crate) struct Span;

impl Span {
    pub(crate) fn bytes(&mut self) -> &mut [u8] {
        todo!()
    }

    pub(crate) fn buffer(&self) -> &crate::Buffer {
        todo!()
    }

    pub(crate) fn offset(&self) -> u64 {
        todo!()
    }

    pub(crate) fn len(&self) -> u64 {
        todo!()
    }
}

#[derive(Clone)]
pub(crate) struct Encoder;

impl Encoder {
    pub(crate) fn render_pass<'e>(
        &'e mut self,
        _desc: &RenderPassDesc<'_>,
    ) -> crate::RenderPass<'e> {
        todo!()
    }

    pub(crate) fn compute_pass<'e>(
        &'e mut self,
        _desc: &ComputePassDesc<'_>,
    ) -> crate::ComputePass<'e> {
        todo!()
    }

    pub(crate) fn copy_buffer_to_image(
        &mut self,
        _src: BufferCopy<'_>,
        _dst: ImageCopy<'_>,
        _size: Extent,
    ) {
        todo!()
    }

    pub(crate) fn copy_image_to_buffer(
        &mut self,
        _src: ImageCopy<'_>,
        _dst: BufferCopy<'_>,
        _size: Extent,
    ) {
        todo!()
    }

    pub(crate) fn copy_image(&mut self, _src: ImageCopy<'_>, _dst: ImageCopy<'_>, _size: Extent) {
        todo!()
    }

    pub(crate) fn copy_buffer(
        &mut self,
        _src: BufferSlice<'_>,
        _dst: &crate::Buffer,
        _dst_offset: u64,
    ) {
        todo!()
    }

    pub(crate) fn fill_buffer(&mut self, _dst: BufferSlice<'_>, _value: u32) {
        todo!()
    }

    pub(crate) fn retain(&mut self, _span: crate::Span) {
        todo!()
    }

    pub(crate) fn resolve_queries(
        &mut self,
        _set: &crate::queue::QuerySet,
        _range: std::ops::Range<u32>,
        _dst: &crate::Buffer,
        _offset: u64,
    ) {
        todo!()
    }
}

pub(crate) struct RenderPass<'e>(std::marker::PhantomData<&'e mut ()>);

impl Drop for RenderPass<'_> {
    fn drop(&mut self) {}
}

impl RenderPass<'_> {
    pub(crate) fn set_pipeline(&mut self, _pipeline: &crate::RenderPipeline) {
        todo!()
    }

    pub(crate) fn bind(&mut self, _slot: u32, _resource: Bind<'_>) {
        todo!()
    }

    pub(crate) fn set_index_buffer(&mut self, _buffer: BufferSlice<'_>, _format: IndexFormat) {
        todo!()
    }

    pub(crate) fn set_viewport(
        &mut self,
        _x: f32,
        _y: f32,
        _w: f32,
        _h: f32,
        _min_depth: f32,
        _max_depth: f32,
    ) {
        todo!()
    }

    pub(crate) fn set_scissor(&mut self, _rect: Rect) {
        todo!()
    }

    pub(crate) fn set_stencil_reference(&mut self, _value: u32) {
        todo!()
    }

    pub(crate) fn set_blend_constant(&mut self, _rgba: [f32; 4]) {
        todo!()
    }

    pub(crate) fn set_depth_bias(&mut self, _constant: f32, _slope: f32, _clamp: f32) {
        todo!()
    }

    pub(crate) fn draw(
        &mut self,
        _vertices: std::ops::Range<u32>,
        _instances: std::ops::Range<u32>,
    ) {
        todo!()
    }

    pub(crate) fn draw_indexed(
        &mut self,
        _indices: std::ops::Range<u32>,
        _base_vertex: i32,
        _instances: std::ops::Range<u32>,
    ) {
        todo!()
    }

    pub(crate) fn draw_indirect(&mut self, _args: BufferSlice<'_>, _indexed: bool) {
        todo!()
    }

    pub(crate) fn begin_occlusion(&mut self, _set: &crate::queue::QuerySet, _index: u32) {
        todo!()
    }

    pub(crate) fn end_occlusion(&mut self) {
        todo!()
    }
}

pub(crate) struct ComputePass<'e>(std::marker::PhantomData<&'e mut ()>);

impl Drop for ComputePass<'_> {
    fn drop(&mut self) {}
}

impl ComputePass<'_> {
    pub(crate) fn set_pipeline(&mut self, _pipeline: &crate::ComputePipeline) {
        todo!()
    }

    pub(crate) fn bind(&mut self, _slot: u32, _resource: Bind<'_>) {
        todo!()
    }

    pub(crate) fn dispatch(&mut self, _x: u32, _y: u32, _z: u32) {
        todo!()
    }

    pub(crate) fn dispatch_indirect(&mut self, _args: BufferSlice<'_>) {
        todo!()
    }
}

#[derive(Clone)]
pub(crate) struct PresentPool;

impl PresentPool {
    pub(crate) fn acquire(&mut self) -> Option<crate::PresentImage> {
        todo!()
    }

    pub(crate) fn trim(&mut self, _keep: u32) {
        todo!()
    }
}

pub(crate) struct PresentImage;

impl Drop for PresentImage {
    fn drop(&mut self) {}
}

impl PresentImage {
    pub(crate) fn image(&self) -> &crate::Image {
        todo!()
    }

    pub(crate) fn age(&self) -> Option<u32> {
        todo!()
    }

    pub(crate) fn export(&self) -> Export<'_> {
        todo!()
    }

    pub(crate) fn presented(&mut self, _written: Token) {
        todo!()
    }

    pub(crate) fn released(self, _fence: Option<crate::queue::Fence>) {
        todo!()
    }
}
