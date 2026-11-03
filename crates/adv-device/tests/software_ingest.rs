//! Ingest compared bit for bit with the software snapshot converter.
#![cfg(feature = "native-display")]
use adv_device::frame::Slot;
use adv_device::surface::{PixelFormat, Rect, Snapshot, Surface};
use adv_display::stages::ingest::{PixelFormat as IngestFormat, Source};
use adv_gpu::encode::{BufferCopy, ImageCopy};
use adv_gpu::queue::DeviceOptions;
use adv_gpu::resource::{BufferDesc, BufferUsage, Memory as GpuMemory};
use std::sync::Arc;
use std::time::Duration;
#[path = "../src/surface/convert.rs"]
mod reference;
const TIMEOUT: Duration = Duration::from_secs(20);
struct Memory {
    bytes: Vec<u8>,
}
fn open() -> (adv_gpu::Device, adv_gpu::Queue) {
    adv_gpu::Device::open(&DeviceOptions {
        software: !cfg!(any(target_vendor = "apple", target_os = "android")),
        validation: !cfg!(target_os = "android"),
        disable: adv_gpu::Modern::from_test_env(),
        label: "software ingest test",
        ..Default::default()
    })
    .unwrap()
}
fn surface(format: PixelFormat, width: u32, height: u32, pad: u32) -> Surface {
    Surface {
        pixels: 0,
        width,
        height,
        pitch: format.row_bytes(width) + pad,
        bottom_up: false,
        locked: None,
        version: 0,
        format,
        palette: None,
    }
}
fn encode_ingest(
    encoder: &mut adv_gpu::Encoder,
    pipeline: &adv_gpu::RenderPipeline,
    ring: &mut adv_gpu::Ring,
    source: &Source<'_>,
    target: &adv_gpu::Image,
    tiles: &adv_display::plan::Tiles,
) {
    use adv_gpu::encode::{ColorAttachment, Load, RenderPassDesc, Store};
    let span = {
        let mut pass = encoder.render_pass(&RenderPassDesc {
            colors: [ColorAttachment {
                view: target.view(),
                load: Load::Load,
                store: Store::Store,
                resolve: None,
            }]
            .into_iter()
            .collect(),
            depth_stencil: None,
            area: adv_gpu::Rect {
                x: 0,
                y: 0,
                w: source.extent.0,
                h: source.extent.1,
            },
            timestamps: None,
            label: "ingest",
        });
        adv_display::stages::ingest::encode(&mut pass, pipeline, ring, source, tiles).unwrap()
    };
    if let Some(span) = span {
        encoder.retain(span);
    }
}
// spec: DEV-FRAME-5
#[test]
fn ingest_decodes_every_format_and_palette_bit_for_bit() {
    let (device, mut queue) = open();
    let pipeline =
        adv_display::stages::ingest::pipeline(&device, adv_gpu::Format::Bgra8Unorm).unwrap();
    let mut ring = device.ring(1 << 20).unwrap();
    for format in [
        PixelFormat::Bgra32,
        PixelFormat::Bgr24,
        PixelFormat::Rgb565,
        PixelFormat::Rgb555,
        PixelFormat::Indexed8,
        PixelFormat::Indexed4,
        PixelFormat::Indexed1,
    ] {
        for has_palette in [false, true] {
            for bottom_up in [false, true] {
                let mut s = surface(format, 257, 257, 3);
                s.bottom_up = bottom_up;
                s.palette = has_palette.then(|| {
                    Arc::new(std::array::from_fn(|i| {
                        [
                            i as u8,
                            (i as u8).wrapping_mul(29),
                            255 - i as u8,
                            (i as u8).wrapping_mul(7),
                        ]
                    }))
                });
                let mut memory = Memory {
                    bytes: vec![0x91; (s.pitch * s.height) as usize],
                };
                // The 16-bit formats cover every possible raw value.
                for y in 0..s.height {
                    for x in 0..s.width {
                        let at = s.row(x, y) as usize;
                        let v = x + y * s.width;
                        match format {
                            PixelFormat::Rgb565 | PixelFormat::Rgb555 => {
                                memory.bytes[at..at + 2].copy_from_slice(&(v as u16).to_le_bytes())
                            }
                            _ => memory.bytes[at] = (v as u8).wrapping_mul(37),
                        }
                    }
                }
                let mut normalized = vec![0; memory.bytes.len()];
                for y in 0..s.height {
                    let start = (y * s.pitch) as usize;
                    normalized[start..start + s.pitch as usize].copy_from_slice(
                        &memory.bytes
                            [s.row(0, y) as usize..s.row(0, y) as usize + s.pitch as usize],
                    );
                }
                let mut cpu = Snapshot {
                    width: s.width,
                    height: s.height,
                    bgra: vec![0; (s.width * s.height * 4) as usize],
                    ..Default::default()
                };
                reference::convert(
                    format,
                    s.palette.as_deref(),
                    s.width,
                    s.pitch,
                    &normalized,
                    &mut cpu,
                    &[s.rect()],
                );
                let slot = Slot::new(&device, s.width, s.height, s.pitch, format).unwrap();
                let out = unsafe { slot.buffer().mapped().unwrap().as_mut() };
                out[..memory.bytes.len()].copy_from_slice(&normalized);
                let software = Source {
                    buffer: slot.buffer(),
                    pitch: slot.pitch(),
                    extent: slot.extent(),
                    format: match format {
                        PixelFormat::Bgra32 => IngestFormat::Bgra32,
                        PixelFormat::Bgr24 => IngestFormat::Bgr24,
                        PixelFormat::Rgb565 => IngestFormat::Rgb565,
                        PixelFormat::Rgb555 => IngestFormat::Rgb555,
                        PixelFormat::Indexed8 => IngestFormat::Indexed8,
                        PixelFormat::Indexed4 => IngestFormat::Indexed4,
                        PixelFormat::Indexed1 => IngestFormat::Indexed1,
                    },
                    palette: s.palette.as_deref(),
                };
                #[cfg(target_vendor = "apple")]
                let mut pool = device
                    .create_present_pool(&adv_gpu::present::PoolDesc {
                        width: s.width,
                        height: s.height,
                        format: adv_gpu::Format::Bgra8Unorm,
                        color: None,
                        count: 1,
                        storage: false,
                        label: "ingest target",
                    })
                    .unwrap();
                #[cfg(target_vendor = "apple")]
                let presentable = pool.acquire(&mut queue).unwrap().unwrap();
                #[cfg(target_vendor = "apple")]
                let target = presentable.image();
                // The frozen non-Android Vulkan backend has no presentable buffers.
                // Exercise the same attachment path on a headless color image.
                #[cfg(not(target_vendor = "apple"))]
                let headless = device
                    .create_image(&adv_gpu::resource::ImageDesc {
                        dimension: adv_gpu::resource::Dimension::D2,
                        size: adv_gpu::Extent {
                            width: s.width,
                            height: s.height,
                            depth_or_layers: 1,
                        },
                        format: adv_gpu::Format::Bgra8Unorm,
                        mips: 1,
                        samples: 1,
                        usage: adv_gpu::resource::Usage::COLOR | adv_gpu::resource::Usage::COPY_SRC,
                        view_formats: &[],
                        label: "headless ingest target",
                    })
                    .unwrap();
                #[cfg(not(target_vendor = "apple"))]
                let target = &headless;
                let readback = device
                    .create_buffer(&BufferDesc {
                        size: cpu.bgra.len() as u64,
                        usage: BufferUsage::COPY_DST,
                        memory: GpuMemory::Readback,
                        label: "ingest reference",
                    })
                    .unwrap();
                let whole = adv_display::plan::Tiles {
                    size: (1, 1),
                    rects: vec![adv_display::Rect {
                        x: 0,
                        y: 0,
                        w: s.width,
                        h: s.height,
                    }],
                };
                let mut encoder = queue.encoder("whole ingest");
                encode_ingest(
                    &mut encoder,
                    &pipeline,
                    &mut ring,
                    &software,
                    target,
                    &whole,
                );
                let token = queue.submit(encoder).unwrap();
                assert!(queue.wait(token, TIMEOUT).unwrap());
                // Change every source byte, then decode only two disjoint tiles.
                // The old output must survive both outside the area and in its holes.
                for byte in &mut normalized {
                    *byte ^= 255;
                }
                (unsafe { slot.buffer().mapped().unwrap().as_mut() })[..normalized.len()]
                    .copy_from_slice(&normalized);
                let rects = [
                    Rect {
                        x: 1,
                        y: 2,
                        w: 7,
                        h: 3,
                    },
                    Rect {
                        x: 21,
                        y: 19,
                        w: 5,
                        h: 2,
                    },
                ];
                reference::convert(
                    format,
                    s.palette.as_deref(),
                    s.width,
                    s.pitch,
                    &normalized,
                    &mut cpu,
                    &rects,
                );
                let tiles = adv_display::plan::Tiles {
                    size: (1, 1),
                    rects: rects
                        .iter()
                        .map(|r| adv_display::Rect {
                            x: r.x,
                            y: r.y,
                            w: r.w,
                            h: r.h,
                        })
                        .collect(),
                };
                let mut encoder = queue.encoder("partial ingest");
                encode_ingest(
                    &mut encoder,
                    &pipeline,
                    &mut ring,
                    &software,
                    target,
                    &tiles,
                );
                encoder.copy_image_to_buffer(
                    ImageCopy {
                        image: target,
                        mip: 0,
                        layer: 0,
                        origin: (0, 0, 0),
                    },
                    BufferCopy {
                        buffer: &readback,
                        offset: 0,
                        row_pitch: s.width * 4,
                        rows: s.height,
                    },
                    target.desc().size,
                );
                let token = queue.submit(encoder).unwrap();
                slot.ingested(token);
                assert!(queue.wait(token, TIMEOUT).unwrap());
                // Presented layers are opaque: ingest drops the source alpha byte.
                let mut expected = cpu.bgra.clone();
                for pixel in expected.as_chunks_mut::<4>().0 {
                    pixel[3] = 255;
                }
                assert_eq!(
                    unsafe { readback.mapped().unwrap().as_ref() },
                    expected,
                    "{format:?}, palette={has_palette}, bottom_up={bottom_up}"
                );
            }
        }
    }
}
