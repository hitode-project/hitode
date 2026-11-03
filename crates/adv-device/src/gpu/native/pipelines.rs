//! Package C1: pipelines for fixed-function draws, programmable draws and
//! blits, keyed as the wgpu renderer keys them, and the disk cache
//! (GFX-CACHE-7, GFX-CACHE-8, GFX-CACHE-10) over `Device::pipeline_identity`.
//! Lookups run on the execution thread; record.rs asks for a pipeline per draw.

use super::super::{DrawState, Error, ProgrammableDraw};
use super::Gpu;
use std::path::Path;

#[derive(Default)]
pub(super) struct Pipelines {}

impl Pipelines {
    /// The pipeline for a fixed-function draw with `state` into attachments of
    /// these formats.
    pub fn fixed(
        &mut self,
        device: &adv_gpu::Device,
        state: &DrawState,
        colors: &[adv_gpu::Format],
        depth: Option<adv_gpu::Format>,
    ) -> Result<adv_gpu::RenderPipeline, Error> {
        let _ = (device, state, colors, depth);
        todo!("C1")
    }
    pub fn programmable(
        &mut self,
        device: &adv_gpu::Device,
        draw: &ProgrammableDraw,
        state: &DrawState,
        colors: &[adv_gpu::Format],
        depth: Option<adv_gpu::Format>,
    ) -> Result<adv_gpu::RenderPipeline, Error> {
        let _ = (device, draw, state, colors, depth);
        todo!("C1")
    }
    /// The filtered copy behind `Gpu::blit`.
    pub fn blit(
        &mut self,
        device: &adv_gpu::Device,
        filter: super::super::Filter,
        color: adv_gpu::Format,
    ) -> Result<adv_gpu::RenderPipeline, Error> {
        let _ = (device, filter, color);
        todo!("C1")
    }
}

impl Gpu {
    pub fn set_pipeline_cache_directory(&mut self, directory: Option<&Path>) {
        let _ = directory;
        todo!("C1")
    }
    pub fn flush_pipeline_cache(&mut self) {
        todo!("C1")
    }
}
