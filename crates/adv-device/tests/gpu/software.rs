use super::*;
use adv_device::surface::{
    BlitRect, FlushReason, PixelFormat, Snapshot, SurfaceMemory, Surfaces, VideoFrame, VideoLayer,
    frame_mailbox,
};

fn read_texture(context: &NativeContext, texture: &wgpu::Texture) -> Vec<u8> {
    let pitch = (texture.width() * 4).next_multiple_of(256);
    let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(pitch) * u64::from(texture.height()),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = context.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pitch),
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    let index = context.submission().unwrap().submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |result| {
        tx.send(result).unwrap();
    });
    context
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(index),
            timeout: Some(Duration::from_secs(10)),
        })
        .unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = buffer.get_mapped_range(..).unwrap();
    bytes
        .chunks_exact(pitch as usize)
        .flat_map(|row| row[..texture.width() as usize * 4].iter().copied())
        .collect()
}

// spec: HOST-DAMAGE-1, DEV-FRAME-4, GFX-METAL-1
#[test]
fn no_copy_software_formats_match_snapshots_and_preserve_publications() {
    let gpu = pollster::block_on(Gpu::request_backend(false, GpuBackend::Metal)).unwrap();
    let context = gpu.context();
    let (width, height) = (257, 256);
    for format in [
        PixelFormat::Bgra32,
        PixelFormat::Bgr24,
        PixelFormat::Rgb565,
        PixelFormat::Rgb555,
        PixelFormat::Indexed8,
        PixelFormat::Indexed4,
        PixelFormat::Indexed1,
    ] {
        for top_down in [false, true] {
            let mut memory = Memory(vec![0; 1024 * 1024]);
            let mut surfaces =
                Surfaces::new(width, height, 16, 1024 * 1024 - 16, Snapshot::default()).unwrap();
            let id = surfaces
                .create(width, height, top_down, format, &mut memory)
                .unwrap();
            if format.indexed() {
                let palette: [_; 256] = std::array::from_fn(|i| {
                    [i as u8, (i * 19) as u8, (255 - i) as u8, (i * 7) as u8]
                });
                surfaces.set_palette(id, 0, &palette, &memory).unwrap();
            }
            for y in 0..height {
                let row: Vec<_> = (0..surfaces[id].pitch)
                    .map(|x| {
                        let pixel = y * width + x / 2;
                        (pixel >> ((x & 1) * 8)) as u8
                    })
                    .collect();
                memory.write(surfaces[id].row(0, y), &row).unwrap();
            }
            let (tx, mut rx) = frame_mailbox();
            surfaces.set_frame_mailbox(tx, adv_sync::WakeEvent::new().unwrap());
            surfaces.present(id, &memory, surfaces[id].rect()).unwrap();
            surfaces.flush(&memory, FlushReason::Immediate).unwrap();
            assert!(rx.update());
            let texture = context.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let held = rx.current().clone();
            let mut expected = Snapshot::default();
            held.snapshot(&mut expected).unwrap();
            context
                .submission()
                .unwrap()
                .upload_software(&texture, &held, 0)
                .unwrap();
            // The guest and mailbox can reuse slots before the GPU read completes.
            for y in [3, 200, 255] {
                let row = vec![0x91; surfaces[id].pitch as usize];
                memory.write(surfaces[id].row(0, y), &row).unwrap();
                surfaces
                    .present(
                        id,
                        &memory,
                        Rect {
                            x: 0,
                            y,
                            w: width,
                            h: 1,
                        },
                    )
                    .unwrap();
                surfaces.flush(&memory, FlushReason::Immediate).unwrap();
            }
            let patch = Rect {
                x: 16,
                y: 17,
                w: 16,
                h: 1,
            };
            let bytes = match format {
                PixelFormat::Indexed1 => 2,
                PixelFormat::Indexed4 => 8,
                _ => 16 * format.bytes() as usize,
            };
            memory
                .write(surfaces[id].row(patch.x, patch.y), &vec![0xf3; bytes])
                .unwrap();
            surfaces.present(id, &memory, patch).unwrap();
            surfaces.flush(&memory, FlushReason::Immediate).unwrap();
            assert_eq!(
                read_texture(context, &texture),
                expected.bgra,
                "{format:?}, top_down={top_down}"
            );
            assert!(rx.update());
            let damage = context
                .submission()
                .unwrap()
                .upload_software(&texture, rx.current(), held.serial())
                .unwrap();
            assert_eq!(
                damage.iter().map(|r| r.w * r.h).sum::<u32>(),
                width * 3 + 16
            );
            rx.current().snapshot(&mut expected).unwrap();
            assert_eq!(
                read_texture(context, &texture),
                expected.bgra,
                "incremental {format:?}"
            );
            assert!(
                context
                    .submission()
                    .unwrap()
                    .upload_software(&texture, rx.current(), expected.frames)
                    .unwrap()
                    .is_empty()
            );
            // Keep the prior immutable publication usable after a newer upload.
            context
                .submission()
                .unwrap()
                .upload_software(&texture, &held, 0)
                .unwrap();
            held.snapshot(&mut expected).unwrap();
            assert_eq!(read_texture(context, &texture), expected.bgra);
        }
    }
}

// spec: HOST-DAMAGE-3, DEV-UPDATE-2
#[test]
fn bgra_texture_reads_snapshot_storage_after_the_guest_and_mailbox_reuse_their_frames() {
    let gpu = pollster::block_on(Gpu::request_backend(false, GpuBackend::Metal)).unwrap();
    let context = gpu.context();
    for bottom_up in [false, true] {
        let mut memory = Memory(vec![0; 65536]);
        let mut surfaces = Surfaces::new(67, 9, 16, 65520, Snapshot::default()).unwrap();
        let id = surfaces
            .create(67, 9, bottom_up, PixelFormat::Bgra32, &mut memory)
            .unwrap();
        for y in 0..9 {
            let bytes: Vec<_> = (0..67)
                .flat_map(|x| [x as u8, y as u8, 23, (x + y) as u8])
                .collect();
            memory.write(surfaces[id].row(0, y), &bytes).unwrap();
        }
        let (tx, mut rx) = frame_mailbox();
        surfaces.set_frame_mailbox(tx, adv_sync::WakeEvent::new().unwrap());
        surfaces.present(id, &memory, surfaces[id].rect()).unwrap();
        surfaces.flush(&memory, FlushReason::Immediate).unwrap();
        assert!(rx.update());
        let mut expected = Snapshot::default();
        rx.current().snapshot(&mut expected).unwrap();
        let texture = context
            .submission()
            .unwrap()
            .software_texture(rx.current())
            .unwrap()
            .unwrap();
        assert!(!texture.usage().contains(wgpu::TextureUsages::COPY_DST));
        for value in 1..8 {
            surfaces
                .fill_raw(id, surfaces[id].rect(), value, &mut memory)
                .unwrap();
            surfaces.present(id, &memory, surfaces[id].rect()).unwrap();
            surfaces.flush(&memory, FlushReason::Immediate).unwrap();
            assert!(rx.update());
        }
        drop(rx);
        drop(surfaces);
        assert_eq!(read_texture(context, &texture), expected.bgra);
    }
    // A video-only publication retains a blank base after the layer is removed.
    let mut surfaces = Surfaces::new(67, 9, 16, 65520, Snapshot::default()).unwrap();
    let memory = Memory(vec![0; 65536]);
    let (tx, mut rx) = frame_mailbox();
    surfaces.set_frame_mailbox(tx, adv_sync::WakeEvent::new().unwrap());
    let one = Rect {
        x: 0,
        y: 0,
        w: 1,
        h: 1,
    };
    let layer = VideoLayer {
        frame: VideoFrame {
            width: 1,
            height: 1,
            pitch: 4,
            bgra: Arc::from([1, 2, 3, 255]),
        },
        source: one,
        destination: BlitRect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        },
        background: None,
        clip: one,
        canvas: (67, 9),
        visible: true,
        hide_cursor: false,
    };
    surfaces.set_video_layer(1, Some(layer), &memory).unwrap();
    surfaces.set_video_layer(1, None, &memory).unwrap();
    assert!(rx.update());
    let texture = context
        .submission()
        .unwrap()
        .software_texture(rx.current())
        .unwrap()
        .unwrap();
    assert_eq!(read_texture(context, &texture), vec![0; 67 * 9 * 4]);
    gpu.check().unwrap();
}
