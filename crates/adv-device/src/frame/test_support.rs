//! Owner-thread capture shared by graphics fixtures.
use super::{Base, Frame, Presenter, Region, SwapImage};
use crate::surface::{Rect, Snapshot, convert};
use adv_gpu::{
    Extent, Queue,
    encode::{BufferCopy, ImageCopy},
    resource::{BufferDesc, BufferUsage, Memory},
};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

pub struct CapturedFrame {
    pub pixels: Snapshot,
    pub damage: Region,
    /// Software slot identity, for buffer-age assertions.
    pub buffer: Option<u64>,
    /// The video layers the frame carried, by id.
    pub video: Vec<u64>,
    pub windows: std::sync::Arc<[super::WindowLayer]>,
}

pub struct CapturePresenter {
    sender: Sender<CapturedFrame>,
    latest: Option<super::Captured>,
}
impl CapturePresenter {
    pub fn new() -> (Self, Receiver<CapturedFrame>) {
        let (tx, rx) = mpsc::channel();
        (
            Self {
                sender: tx,
                latest: None,
            },
            rx,
        )
    }
}
impl Presenter for CapturePresenter {
    fn presentable(&self, _: u32, _: u32) -> bool {
        false
    }
    fn present(&mut self, queue: &mut Queue, frame: Frame) {
        let video = frame.video.iter().map(|v| v.id).collect();
        let windows = frame.windows.clone();
        let (pixels, damage, buffer) = match frame.base {
            Base::Gpu { image, damage, .. } => {
                let image = match &image {
                    SwapImage::Private(image) => image,
                    SwapImage::Presentable(image) => image.image(),
                };
                let size = image.desc().size;
                let readback = queue
                    .device()
                    .create_buffer(&BufferDesc {
                        size: u64::from(size.width) * u64::from(size.height) * 4,
                        usage: BufferUsage::COPY_DST,
                        memory: Memory::Readback,
                        label: "captured frame",
                    })
                    .unwrap();
                let mut encoder = queue.encoder("capture presented frame");
                encoder.copy_image_to_buffer(
                    ImageCopy {
                        image,
                        mip: 0,
                        layer: 0,
                        origin: (0, 0, 0),
                    },
                    BufferCopy {
                        buffer: &readback,
                        offset: 0,
                        row_pitch: size.width * 4,
                        rows: size.height,
                    },
                    Extent {
                        width: size.width,
                        height: size.height,
                        depth_or_layers: 1,
                    },
                );
                let ready = queue.submit(encoder).unwrap();
                assert!(queue.wait(ready, Duration::from_secs(20)).unwrap());
                let mut bgra = unsafe { readback.mapped().unwrap().as_ref() }.to_vec();
                if image.desc().format == adv_gpu::Format::Rgba8Unorm {
                    for pixel in bgra.as_chunks_mut::<4>().0 {
                        pixel.swap(0, 2);
                    }
                }
                (
                    Snapshot {
                        width: size.width,
                        height: size.height,
                        bgra,
                        frames: frame.serial,
                        ..Default::default()
                    },
                    damage,
                    None,
                )
            }
            Base::Software {
                slot,
                format,
                palette,
                damage,
            } => {
                let (width, height) = slot.extent();
                let full = Rect {
                    x: 0,
                    y: 0,
                    w: width,
                    h: height,
                };
                let mut pixels = Snapshot {
                    width,
                    height,
                    frames: frame.serial,
                    bgra: vec![0; width as usize * height as usize * 4],
                    ..Default::default()
                };
                // CPU ingest finishes before the publication releases its slot.
                let mapped = slot.buffer().mapped().unwrap();
                convert::convert(
                    format,
                    palette.as_deref(),
                    width,
                    slot.pitch(),
                    unsafe { mapped.as_ref() },
                    &mut pixels,
                    &[full],
                );
                (
                    pixels,
                    damage,
                    Some(mapped.as_ptr() as *mut u8 as usize as u64),
                )
            }
        };
        self.latest = Some(super::Captured {
            serial: pixels.frames,
            width: pixels.width,
            height: pixels.height,
            bgra: pixels.bgra.clone(),
        });
        self.sender
            .send(CapturedFrame {
                pixels,
                damage,
                buffer,
                video,
                windows,
            })
            .unwrap();
    }
    fn service(&mut self, _: &mut adv_gpu::Queue, wake: Option<&crate::frame::Waker>) {
        if let Some(wake) = wake {
            for reply in wake.captures() {
                let frame = self
                    .latest
                    .as_ref()
                    .map(|frame| super::Captured {
                        serial: frame.serial,
                        width: frame.width,
                        height: frame.height,
                        bgra: frame.bgra.clone(),
                    })
                    .ok_or_else(|| "no presented frame".to_owned());
                let _ = reply.send(frame);
            }
        }
    }
}
