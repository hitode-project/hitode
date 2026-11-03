//! Frame and hook adapters for the reference renderer.
use crate::gpu::{Error, Gpu, GpuBackend, Image, NativeFrame};
use crate::surface::Rect;

#[derive(Clone)]
pub struct Frame(NativeFrame);
impl Frame {
    pub fn pixels(&self, gpu: &mut Gpu) -> Vec<u8> {
        let mut read = gpu.readback(&self.0.image, None).unwrap();
        read.take_completion_job().unwrap()();
        read.poll().unwrap().unwrap()
    }
}
pub async fn request(fallback: bool) -> Result<Gpu, Error> {
    Gpu::request(fallback).await
}
pub async fn request_backend(fallback: bool, backend: GpuBackend) -> Result<Gpu, Error> {
    Gpu::request_backend(fallback, backend).await
}
pub fn present(gpu: &mut Gpu, image: &Image, _: Option<Rect>, serial: u64) -> Result<Frame, Error> {
    gpu.present(image, serial).map(Frame)
}
pub fn completed_frame(
    gpu: &mut Gpu,
    image: Image,
    _: Option<Rect>,
    serial: u64,
) -> Result<Frame, Error> {
    gpu.completed_frame(image, serial).map(Frame)
}
pub use crate::gpu::SubmissionPause;
pub fn pause_submissions(gpu: &Gpu) -> Result<SubmissionPause, Error> {
    gpu.context().pause_submissions()
}
pub fn destroy(gpu: &Gpu) {
    gpu.context().device.destroy();
}

#[macro_export]
macro_rules! renderer_test { ($($tokens:tt)*) => { $($tokens)* }; }
pub use crate::renderer_test;

/// The renderer already updates a surface store's snapshot synchronously.
pub struct SurfaceCapture;
impl SurfaceCapture {
    pub async fn new() -> Result<Self, Error> {
        Ok(Self)
    }
    pub fn update(&mut self, _: &mut crate::surface::SurfaceStore) {}
}
