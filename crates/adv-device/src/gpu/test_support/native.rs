//! Test adapters for the native executor; absent from ordinary builds.
use super::{Gpu, Image, stream};
use crate::frame::{
    Region,
    test_support::{CapturePresenter, CapturedFrame},
};
use crate::gpu::{Error, GpuBackend};
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
    damage: Region,
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

/// Captures software publications, with their damage, into the front's
/// existing snapshot assertions.
pub struct SurfaceCapture {
    gpu: Gpu,
    published: u64,
}
impl SurfaceCapture {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            gpu: request(!cfg!(any(target_vendor = "apple", target_os = "android")))?,
            published: 0,
        })
    }
    pub fn update(&mut self, surfaces: &mut crate::surface::SurfaceStore) {
        let source = self.gpu.execution.capture.borrow();
        let source = source.as_ref().unwrap();
        while self.published < surfaces.stats.published_frames {
            let frame = source.get(source.reserve());
            let pixels = &frame.pixels;
            // The guest's own screen: its windows over the frame.
            let mut bgra = pixels.bgra.clone();
            for window in frame.windows.iter() {
                for row in 0..window.height as i32 {
                    let y = window.at.1 + row;
                    if !(0..pixels.height as i32).contains(&y) {
                        continue;
                    }
                    for column in 0..window.width as i32 {
                        let x = window.at.0 + column;
                        if (0..pixels.width as i32).contains(&x) {
                            let from = (row * window.width as i32 + column) as usize * 4;
                            let to = (y * pixels.width as i32 + x) as usize * 4;
                            bgra[to..to + 4].copy_from_slice(&window.pixels[from..from + 4]);
                        }
                    }
                }
            }
            crate::surface::Presenter::present(
                &mut surfaces.presenter,
                crate::surface::FrameView {
                    width: pixels.width,
                    height: pixels.height,
                    pitch: pixels.width * 4,
                    bgra: &bgra,
                    serial: pixels.frames,
                },
            )
            .unwrap();
            surfaces.presenter.damage = frame.damage.clone();
            self.published += 1;
        }
    }
}

pub fn request(fallback: bool) -> Result<Gpu, Error> {
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
    let gpu = Gpu::request_with_presenter(device, queue, Box::new(presenter))?;
    *gpu.execution.capture.borrow_mut() = Some(Rc::new(Frames {
        receiver,
        frames: RefCell::new(Vec::new()),
        next: Cell::new(0),
    }));
    Ok(gpu)
}
pub fn request_backend(fallback: bool, _: GpuBackend) -> Result<Gpu, Error> {
    request(fallback)
}
pub fn present(
    gpu: &mut Gpu,
    image: &Image,
    damage: Option<Region>,
    serial: u64,
) -> Result<Frame, Error> {
    let damage = damage.unwrap_or_else(|| Region::whole((image.width(), image.height())));
    gpu.present(
        image,
        damage.clone(),
        serial,
        std::sync::Arc::from([]),
        std::sync::Arc::from([]),
    )?;
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
    damage: Option<Region>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // spec: GFX-REC-1
    fn pause_and_device_loss_apply_to_every_facade_on_the_execution() {
        let mut gpu = request(!cfg!(any(target_vendor = "apple", target_os = "android"))).unwrap();
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

    // spec: WINE-GFX-10
    #[test]
    fn window_planes_show_over_a_native_base_and_leave_it_again() {
        struct Flat(Vec<u8>);
        impl crate::surface::SurfaceMemory for Flat {
            fn read(&self, at: u32, len: usize) -> Result<std::borrow::Cow<'_, [u8]>, String> {
                let at = at as usize;
                Ok(std::borrow::Cow::Borrowed(&self.0[at..at + len]))
            }
            fn write(&mut self, _: u32, _: &[u8]) -> Result<(), String> {
                unreachable!()
            }
            fn writable(&self, _: u32, _: usize) -> Result<(), String> {
                Ok(())
            }
            fn copy(&mut self, _: u32, _: u32, _: usize) -> Result<(), String> {
                unreachable!()
            }
        }
        const BLUE: [u8; 4] = [255, 0, 0, 255];
        const RED: [u8; 4] = [0, 0, 255, 255];
        let mut gpu = request(!cfg!(any(target_vendor = "apple", target_os = "android"))).unwrap();
        let mut store =
            crate::surface::SurfaceStore::new(8, 4, 16, 65520, Default::default()).unwrap();
        let target = gpu.target(8, 4, false).unwrap();
        let whole = crate::surface::Rect {
            x: 0,
            y: 0,
            w: 8,
            h: 4,
        };
        gpu.upload(&target.image, whole, 32, &BLUE.repeat(32))
            .unwrap();
        let base = store.insert_native(target).unwrap();
        // A 2 × 2 window at (7, 1), its right column past the base's right
        // edge, shows as a layer of its own over the unchanged base.
        let memory = Flat(RED.repeat(4));
        let plane = crate::surface::Plane {
            id: 9,
            pixels: 0,
            pitch: 8,
            x: 7,
            y: 1,
            width: 2,
            height: 2,
            layered: false,
        };
        store.set_planes(vec![plane], &[], (8, 4), &memory).unwrap();
        store.present_native(base, None, &mut gpu).unwrap();
        let frames = gpu.execution.capture.borrow().clone().unwrap();
        let shown = frames.get(0);
        assert_eq!(shown.pixels.bgra, BLUE.repeat(32));
        let [window] = &shown.windows[..] else {
            panic!("one window layer")
        };
        assert_eq!(
            (window.id, window.at, window.width, window.height),
            (9, (7, 1), 2, 2)
        );
        assert_eq!(&window.pixels[..], &RED.repeat(4)[..]);
        drop(shown);
        // Closing it while the guest presents nothing shows the base alone,
        // without redrawing it.
        store.set_planes(Vec::new(), &[], (8, 4), &memory).unwrap();
        store.present_planes(0, &memory).unwrap();
        let shown = frames.get(1);
        assert!(shown.windows.is_empty() && shown.damage.is_empty());
        assert_eq!(shown.pixels.bgra, BLUE.repeat(32));
    }

    #[cfg(feature = "media")]
    #[test]
    fn video_layers_reach_the_native_display_without_a_game_present() {
        struct Flat(Vec<u8>);
        impl crate::surface::SurfaceMemory for Flat {
            fn read(&self, at: u32, len: usize) -> Result<std::borrow::Cow<'_, [u8]>, String> {
                let at = at as usize;
                Ok(std::borrow::Cow::Borrowed(&self.0[at..at + len]))
            }
            fn write(&mut self, at: u32, data: &[u8]) -> Result<(), String> {
                self.0[at as usize..at as usize + data.len()].copy_from_slice(data);
                Ok(())
            }
            fn writable(&self, _: u32, _: usize) -> Result<(), String> {
                Ok(())
            }
            fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
                self.0
                    .copy_within(src as usize..src as usize + len, dst as usize);
                Ok(())
            }
        }
        let _gpu = request(!cfg!(any(target_vendor = "apple", target_os = "android"))).unwrap();
        let mut decoder = crate::media::Decoder::open_memory(
            include_bytes!("../../../../adv-dx/src/dshow/graph/render/fixtures/guest_movie.mpg")
                .to_vec(),
        )
        .unwrap();
        let mut samples = (0..1_000_000)
            .step_by(40_000)
            .filter_map(|time| decoder.frame_at(time).unwrap().video);
        let video = samples.next().unwrap();
        let next = samples.next().unwrap();
        let source = crate::surface::Rect {
            x: 0,
            y: 0,
            w: video.width,
            h: video.height,
        };
        let mut memory = Flat(vec![0; 1 << 16]);
        let mut store =
            crate::surface::SurfaceStore::new(8, 8, 16, 65520, Default::default()).unwrap();
        let base = store
            .create(
                8,
                8,
                false,
                crate::surface::PixelFormat::Bgra32,
                &mut memory,
            )
            .unwrap();
        store.present(base, &memory, store[base].rect()).unwrap();
        let canvas = crate::surface::Rect {
            x: 0,
            y: 0,
            w: 8,
            h: 8,
        };
        let layer = crate::surface::VideoLayer {
            frame: video.into_surface(),
            source,
            destination: crate::surface::BlitRect {
                x: 0,
                y: 0,
                w: 8,
                h: 8,
            },
            background: None,
            clip: canvas,
            canvas: (8, 8),
            visible: true,
            hide_cursor: false,
        };
        store
            .set_video_layer(7, Some(layer.clone()), &memory)
            .unwrap();
        // The next movie sample publishes over the same software base.
        store
            .set_video_layer(
                7,
                Some(crate::surface::VideoLayer {
                    frame: next.into_surface(),
                    ..layer
                }),
                &memory,
            )
            .unwrap();
        let execution = super::super::EXECUTION
            .with(|cached| cached.borrow().upgrade())
            .unwrap();
        let frames = execution.capture.borrow().clone().unwrap();
        assert!(frames.get(0).video.is_empty());
        // The movie frame is shown over the unchanged base.
        assert_eq!(frames.get(1).video, [7]);
        assert_eq!(frames.get(1).pixels.frames, 2);
        assert_eq!(frames.get(2).video, [7]);
    }
}
