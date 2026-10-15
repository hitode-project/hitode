//! Damage-limited uploads from the consumer's persistent BGRA snapshot.
use adv_device::{gpu::Submission, surface::Snapshot};

pub(crate) fn upload(
    submission: &Submission<'_>,
    texture: &wgpu::Texture,
    frame: &Snapshot,
) -> u64 {
    let mut bytes = 0;
    for r in frame.damage.iter() {
        let mut destination = texture.as_image_copy();
        destination.origin = wgpu::Origin3d {
            x: r.x,
            y: r.y,
            z: 0,
        };
        let offset = (u64::from(r.y) * u64::from(frame.width) + u64::from(r.x)) * 4;
        submission.write_texture(
            destination,
            &frame.bgra,
            wgpu::TexelCopyBufferLayout {
                offset,
                bytes_per_row: Some(frame.width * 4),
                rows_per_image: Some(r.h),
            },
            wgpu::Extent3d {
                width: r.w,
                height: r.h,
                depth_or_array_layers: 1,
            },
        );
        bytes += u64::from(r.w) * u64::from(r.h) * 4;
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_device::{
        gpu::Gpu,
        surface::{PixelFormat, Rect, SurfaceMemory, Surfaces, frame_mailbox},
    };
    use std::borrow::Cow;

    struct Memory(Vec<u8>);
    impl SurfaceMemory for Memory {
        fn read(&self, addr: u32, n: usize) -> Result<Cow<'_, [u8]>, String> {
            Ok(Cow::Borrowed(&self.0[addr as usize..addr as usize + n]))
        }
        fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), String> {
            self.0[addr as usize..addr as usize + bytes.len()].copy_from_slice(bytes);
            Ok(())
        }
        fn writable(&self, _: u32, _: usize) -> Result<(), String> {
            Ok(())
        }
        fn copy(&mut self, src: u32, dst: u32, n: usize) -> Result<(), String> {
            self.0
                .copy_within(src as usize..src as usize + n, dst as usize);
            Ok(())
        }
    }
    fn pixels(context: &adv_device::gpu::NativeContext, texture: &wgpu::Texture) -> Vec<u8> {
        let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * 4,
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
                    bytes_per_row: Some(256),
                    rows_per_image: Some(4),
                },
            },
            wgpu::Extent3d {
                width: 5,
                height: 4,
                depth_or_array_layers: 1,
            },
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
                timeout: Some(std::time::Duration::from_secs(10)),
            })
            .unwrap();
        rx.recv().unwrap().unwrap();
        let mapped = buffer.get_mapped_range(..).unwrap();
        (0..4)
            .flat_map(|y| mapped[y * 256..y * 256 + 20].to_vec())
            .collect()
    }

    // spec: HOST-DAMAGE-1
    #[test]
    fn incremental_gpu_uploads_preserve_other_rows_and_skipped_patches() {
        let gpu = pollster::block_on(Gpu::request(!cfg!(any(
            target_os = "macos",
            target_os = "android"
        ))))
        .unwrap();
        let context = gpu.context();
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 5,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mut memory = Memory(vec![0; 1024]);
        let mut surfaces = Surfaces::new(5, 4, 16, 1008, Snapshot::default()).unwrap();
        let id = surfaces
            .create(5, 4, true, PixelFormat::Bgra32, &mut memory)
            .unwrap();
        let (tx, mut rx) = frame_mailbox();
        surfaces.set_frame_mailbox(tx, adv_sync::WakeEvent::new().unwrap());
        let mut frame = Snapshot::default();
        surfaces.present(id, &memory, surfaces[id].rect()).unwrap();
        surfaces
            .flush(&memory, adv_device::surface::FlushReason::Immediate)
            .unwrap();
        assert!(rx.update());
        rx.current().snapshot(&mut frame).unwrap();
        assert_eq!(upload(&context.submission().unwrap(), &texture, &frame), 80);
        let r = Rect {
            x: 0,
            y: 2,
            w: 5,
            h: 1,
        };
        memory.write(surfaces[id].row(0, 2), &[7; 20]).unwrap();
        surfaces.present(id, &memory, r).unwrap();
        surfaces
            .flush(&memory, adv_device::surface::FlushReason::Immediate)
            .unwrap();
        assert!(rx.update());
        rx.current().snapshot(&mut frame).unwrap();
        assert_eq!(upload(&context.submission().unwrap(), &texture, &frame), 20);
        assert_eq!(pixels(context, &texture), frame.bgra);
        for x in [1, 4] {
            let r = Rect {
                x,
                y: 0,
                w: 1,
                h: 1,
            };
            memory
                .write(surfaces[id].row(x, 0), &[30 + x as u8; 4])
                .unwrap();
            surfaces.present(id, &memory, r).unwrap();
            surfaces
                .flush(&memory, adv_device::surface::FlushReason::Immediate)
                .unwrap();
        }
        assert!(rx.update());
        rx.current().snapshot(&mut frame).unwrap();
        assert_eq!(frame.damage.len(), 2);
        assert_eq!(upload(&context.submission().unwrap(), &texture, &frame), 8);
        assert_eq!(pixels(context, &texture), frame.bgra);
        rx.current().snapshot(&mut frame).unwrap();
        assert_eq!(upload(&context.submission().unwrap(), &texture, &frame), 0);
        assert_eq!(pixels(context, &texture), frame.bgra);
    }
}
