//! Guest windows as compositor layers of their own (WINE-GFX-10). A window's
//! pixels go once into a presentable image; the compositor places and scales
//! it like a native window, apart from the game's scaling and upscaling.
use crate::compositor::{AcquiredImage, LayerId};
use crate::plan::FrameInfo;
use adv_gpu::{
    Device, Extent, PresentPool, Queue,
    encode::{BufferCopy, ImageCopy},
    resource::{BufferDesc, BufferUsage, Memory},
};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct Window {
    pub id: u32,
    pub size: (u32, u32),
    /// Top-down `Bgra32` rows of `size.0 * 4` bytes.
    pub pixels: Arc<[u8]>,
    /// Changes whenever `pixels` do.
    pub version: u64,
}

#[derive(Default)]
pub(crate) struct Windows {
    /// Each window's images and the version the newest one holds.
    images: HashMap<u32, (PresentPool, u64)>,
    /// The latest encoded frame's windows.
    pub(crate) current: Vec<Window>,
}

impl Windows {
    /// Mark the windows of `info` whose pixels no image holds yet.
    pub(crate) fn changes(&self, info: &mut FrameInfo, windows: &[Window]) {
        for (geometry, window) in info.windows.iter_mut().zip(windows) {
            geometry.changed = self.images.get(&window.id).is_none_or(|(pool, version)| {
                *version != window.version || {
                    let d = pool.description();
                    (d.width, d.height) != window.size
                }
            });
        }
    }

    /// Write the changed windows into images for the compositor. A window
    /// whose images are all on screen waits for the next frame.
    pub(crate) fn upload(
        &mut self,
        (device, queue): (&Device, &mut Queue),
        info: &FrameInfo,
        windows: Vec<Window>,
        images: &mut Vec<AcquiredImage>,
    ) -> adv_gpu::Result<()> {
        self.images
            .retain(|id, _| windows.iter().any(|w| w.id == *id));
        for (geometry, window) in info.windows.iter().zip(&windows) {
            if !geometry.changed {
                continue;
            }
            let (width, height) = window.size;
            let (pool, version) = match self.images.get_mut(&window.id) {
                Some(entry)
                    if {
                        let d = entry.0.description();
                        (d.width, d.height) == window.size
                    } =>
                {
                    entry
                }
                _ => {
                    let pool = device.create_present_pool(&adv_gpu::present::PoolDesc {
                        color: None,
                        width,
                        height,
                        format: adv_gpu::present::COLOR_FORMAT,
                        count: 3,
                        storage: false,
                        label: "guest window",
                    })?;
                    self.images.insert(window.id, (pool, 0));
                    self.images.get_mut(&window.id).unwrap()
                }
            };
            let Some(mut image) = pool.acquire(queue)? else {
                continue;
            };
            let upload = device.create_buffer(&BufferDesc {
                size: window.pixels.len() as u64,
                usage: BufferUsage::COPY_SRC,
                memory: Memory::Upload,
                label: "guest window pixels",
            })?;
            // SAFETY: the buffer is new and not submitted yet.
            let bytes = unsafe { upload.mapped().unwrap().as_mut() };
            // GDI leaves alpha undefined: windows are opaque.
            for (out, pixel) in bytes
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(window.pixels.as_chunks::<4>().0)
            {
                *out = if adv_gpu::present::COLOR_FORMAT == adv_gpu::Format::Rgba8Unorm {
                    [pixel[2], pixel[1], pixel[0], 255]
                } else {
                    [pixel[0], pixel[1], pixel[2], 255]
                };
            }
            let mut encoder = queue.encoder("guest window");
            encoder.copy_buffer_to_image(
                BufferCopy {
                    buffer: &upload,
                    offset: 0,
                    row_pitch: width * 4,
                    rows: height,
                },
                ImageCopy {
                    image: image.image(),
                    mip: 0,
                    layer: 0,
                    origin: (0, 0, 0),
                },
                Extent {
                    width,
                    height,
                    depth_or_layers: 1,
                },
            );
            let token = queue.submit_present(encoder, &mut [&mut image])?;
            queue.on_complete(token, Box::new(move || drop(upload)));
            *version = window.version;
            images.push(AcquiredImage {
                layer: LayerId::Window(window.id),
                image,
                acquire: token,
            });
        }
        self.current = windows;
        Ok(())
    }

    /// An image of window `id` never reached the screen: write it again.
    pub(crate) fn discarded(&mut self, id: u32) {
        if let Some((_, version)) = self.images.get_mut(&id) {
            *version = 0;
        }
    }

    /// The latest windows with their canvas positions, bottom first.
    pub(crate) fn placed<'a>(
        &'a self,
        info: &'a FrameInfo,
    ) -> impl Iterator<Item = ((i32, i32), &'a Window)> {
        info.windows.iter().filter_map(|geometry| {
            let window = self.current.iter().find(|w| w.id == geometry.id)?;
            Some((geometry.at, window))
        })
    }
}

/// Draw `windows` into `bgra`, a top-down capture of a `canvas`, at their
/// canvas positions: what the guest would see on its own screen.
pub(crate) fn overlay(bgra: &mut [u8], canvas: (u32, u32), windows: &[((i32, i32), Window)]) {
    for ((x, y), window) in windows {
        let (width, height) = window.size;
        let left = (*x).clamp(0, canvas.0 as i32);
        let right = (i64::from(*x) + i64::from(width)).clamp(0, i64::from(canvas.0)) as i32;
        if left >= right {
            continue;
        }
        for row in 0..height as i32 {
            let target = y + row;
            if target < 0 || target >= canvas.1 as i32 {
                continue;
            }
            let from = (row as usize * width as usize + (left - x) as usize) * 4;
            let to = (target as usize * canvas.0 as usize + left as usize) * 4;
            let len = (right - left) as usize * 4;
            bgra[to..to + len].copy_from_slice(&window.pixels[from..from + len]);
            for pixel in bgra[to..to + len].as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
        }
    }
}
