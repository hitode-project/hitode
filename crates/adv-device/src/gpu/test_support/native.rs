//! Test adapters for the native executor; absent from ordinary builds.
use super::{Gpu, Image, stream};
use crate::frame::{
    Damage,
    test_support::{CapturePresenter, CapturedFrame},
};
use crate::gpu::{Error, GpuBackend};
use crate::surface::Rect;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc::Receiver,
    time::Duration,
};

pub(super) struct Frames {
    receiver: Receiver<CapturedFrame>,
    frames: RefCell<Vec<CapturedFrame>>,
    next: Cell<usize>,
}
#[derive(Clone)]
pub struct Frame {
    source: Rc<Frames>,
    index: usize,
    serial: u64,
    damage: Damage,
}
impl Frames {
    fn get(&self, index: usize) -> std::cell::Ref<'_, CapturedFrame> {
        let mut frames = self.frames.borrow_mut();
        while frames.len() <= index {
            frames.push(self.receiver.recv_timeout(Duration::from_secs(20)).unwrap());
        }
        drop(frames);
        std::cell::Ref::map(self.frames.borrow(), |frames| &frames[index])
    }
    fn reserve(&self) -> usize {
        let index = self.next.get();
        self.next.set(index + 1);
        index
    }
}
impl Frame {
    pub fn pixels(&self, _: &mut Gpu) -> Vec<u8> {
        let frame = self.source.get(self.index);
        assert_eq!(frame.pixels.frames, self.serial);
        assert_eq!(frame.damage, self.damage);
        frame.pixels.bgra.clone()
    }
}

/// Captures software publications into the front's existing snapshot assertions.
pub struct SurfaceCapture {
    gpu: Gpu,
    published: u64,
}
impl SurfaceCapture {
    pub async fn new() -> Result<Self, Error> {
        Ok(Self {
            gpu: request(!cfg!(any(target_vendor = "apple", target_os = "android"))).await?,
            published: 0,
        })
    }
    pub fn update(&mut self, surfaces: &mut crate::surface::SurfaceStore) {
        let source = self.gpu.execution.capture.borrow();
        let source = source.as_ref().unwrap();
        while self.published < surfaces.stats.published_frames {
            let frame = source.get(source.reserve());
            let pixels = &frame.pixels;
            crate::surface::Presenter::present(
                &mut surfaces.presenter,
                crate::surface::FrameView {
                    width: pixels.width,
                    height: pixels.height,
                    pitch: pixels.width * 4,
                    bgra: &pixels.bgra,
                    serial: pixels.frames,
                },
            )
            .unwrap();
            self.published += 1;
        }
    }
}

pub async fn request(fallback: bool) -> Result<Gpu, Error> {
    if let Some(execution) = super::EXECUTION.with(|cached| cached.borrow().upgrade()) {
        assert!(
            execution.capture.borrow().is_some(),
            "capture must precede other facades"
        );
        return Gpu::facade(execution);
    }
    let (presenter, receiver) = CapturePresenter::new();
    let (device, queue) = adv_gpu::Device::open(&adv_gpu::queue::DeviceOptions {
        software: fallback,
        validation: !cfg!(target_os = "android"),
        ..Default::default()
    })
    .map_err(super::record::gpu_error)?;
    let gpu = Gpu::request_with_presenter(device, queue, Box::new(presenter)).await?;
    *gpu.execution.capture.borrow_mut() = Some(Rc::new(Frames {
        receiver,
        frames: RefCell::new(Vec::new()),
        next: Cell::new(0),
    }));
    Ok(gpu)
}
pub async fn request_backend(fallback: bool, _: GpuBackend) -> Result<Gpu, Error> {
    request(fallback).await
}
pub fn present(
    gpu: &mut Gpu,
    image: &Image,
    damage: Option<Rect>,
    serial: u64,
) -> Result<Frame, Error> {
    let damage = match damage {
        None => Damage::Whole,
        Some(r) if r.empty() => Damage::None,
        Some(r) => Damage::Rects(
            [adv_gpu::Rect {
                x: r.x,
                y: r.y,
                w: r.w,
                h: r.h,
            }]
            .into_iter()
            .collect(),
        ),
    };
    gpu.present(image, damage.clone(), serial)?;
    let source = gpu.execution.capture.borrow().as_ref().unwrap().clone();
    let index = source.reserve();
    Ok(Frame {
        source,
        index,
        serial,
        damage,
    })
}
pub fn completed_frame(
    gpu: &mut Gpu,
    image: Image,
    damage: Option<Rect>,
    serial: u64,
) -> Result<Frame, Error> {
    present(gpu, &image, damage, serial)
}
pub use stream::SubmissionPause;
pub fn pause_submissions(gpu: &Gpu) -> Result<SubmissionPause, Error> {
    gpu.stream.0.borrow().pause_submissions()
}
pub fn destroy(gpu: &Gpu) {
    gpu.execution
        .owner
        .failure
        .set("authored device loss".into())
        .unwrap();
}

// Feature selection lives in the dependency that owns native-display.
#[macro_export]
macro_rules! renderer_test {
    ($($tokens:tt)*) => {};
}
pub use crate::renderer_test;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // spec: GFX-REC-1
    fn pause_and_device_loss_apply_to_every_facade_on_the_execution() {
        let mut gpu = pollster::block_on(request(!cfg!(target_vendor = "apple"))).unwrap();
        let child = gpu.fork().unwrap();
        let target = gpu.target(2, 2, false).unwrap();
        gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
        let pause = pause_submissions(&child).unwrap();
        assert!(matches!(pause_submissions(&gpu), Err(Error::Busy)));
        assert!(matches!(
            gpu.readback(&target.image, None),
            Err(Error::Busy)
        ));
        assert!(matches!(
            present(&mut gpu, &target.image, None, 1),
            Err(Error::Busy)
        ));
        drop(pause);
        let frame = present(&mut gpu, &target.image, None, 1).unwrap();
        assert_eq!(frame.pixels(&mut gpu), [0, 0, 255, 255].repeat(4));
        destroy(&child);
        assert!(matches!(gpu.check(), Err(Error::Device(_))));
        assert!(matches!(child.check(), Err(Error::Device(_))));
    }
}
