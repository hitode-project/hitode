use super::*;
use adv_gpu::{Queue, encode::ComputePassDesc, queue::DeviceOptions};
use std::{collections::BTreeSet, time::Duration};

fn upload(device: &Device, bytes: &[u8], usage: BufferUsage) -> Buffer {
    let buffer = device
        .create_buffer(&BufferDesc {
            size: bytes.len() as u64,
            usage,
            memory: Memory::Upload,
            label: "delta test upload",
        })
        .unwrap();
    // SAFETY: no GPU work references this new buffer.
    unsafe { buffer.mapped().unwrap().as_mut() }.copy_from_slice(bytes);
    buffer
}

fn read(device: &Device, queue: &mut Queue, buffer: &Buffer) -> Vec<u32> {
    let readback = device
        .create_buffer(&BufferDesc {
            size: buffer.desc().size,
            usage: BufferUsage::COPY_DST,
            memory: Memory::Readback,
            label: "delta test readback",
        })
        .unwrap();
    let mut encoder = queue.encoder("delta test readback");
    encoder.copy_buffer(buffer.slice(0..buffer.desc().size), &readback, 0);
    let token = queue.submit(encoder).unwrap();
    assert!(queue.wait(token, Duration::from_secs(60)).unwrap());
    // SAFETY: the copy has completed.
    unsafe { readback.mapped().unwrap().as_ref() }
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .collect()
}

// spec: HOST-VIEW-10
#[test]
fn gpu_filter_lists_cover_the_delta_dependency_closure() {
    let (device, mut queue) = Device::open(&DeviceOptions {
        software: !cfg!(any(target_vendor = "apple", target_os = "android")),
        validation: !cfg!(target_os = "android"),
        disable: adv_gpu::Modern::from_test_env(),
        ..Default::default()
    })
    .unwrap();
    let size = (529, 137);
    let rect = |x, y, w, h| Rect { x, y, w, h };
    let matrix = Features::MATRIX_F16_8X8 | Features::SHADER_F16 | Features::SUBGROUP;
    let tables = [models::REFERENCE, models::FUSED, models::MATRIX];
    for (form, table) in tables.into_iter().enumerate() {
        if form == 2 && !device.caps().features.contains(matrix) {
            continue;
        }
        for (m, model) in table.into_iter().enumerate() {
            let upscaling = Upscaling::with_precision(&device, model, size, form == 2).unwrap();
            for (s, delta) in [
                (vec![rect(0, 0, size.0, size.1)], vec![rect(256, 64, 8, 8)]),
                (
                    vec![rect(0, 0, 32, 40), rect(480, 96, 49, 41)],
                    vec![rect(0, 0, 8, 8), rect(528, 136, 1, 1)],
                ),
                (vec![rect(0, 0, size.0, size.1)], vec![]),
            ] {
                let stride = size.0.div_ceil(8).div_ceil(32);
                let mut bits = vec![0u32; (stride * size.1.div_ceil(8) + 1) as usize];
                for r in &delta {
                    for y in r.y / 8..(r.y + r.h).div_ceil(8) {
                        for x in r.x / 8..(r.x + r.w).div_ceil(8) {
                            bits[(y * stride + x / 32) as usize] |= 1 << (x % 32);
                        }
                    }
                }
                let bitmap = upload(&device, &words(bits), BufferUsage::STORAGE);
                let source = upscaling.cells(&s);
                let expected = upscaling.cells(&delta);
                let mut encoder = queue.encoder("filter cell selection");
                let mut records = vec![0; upscaling.arguments.desc().size as usize];
                for (i, grid) in upscaling.grids.iter().enumerate() {
                    let count = source[i]
                        .as_ref()
                        .map_or(grid.columns * grid.rows, |l| l.len() as u32);
                    let at = grid.arguments as usize;
                    records[at..at + 16].copy_from_slice(&words([0, 1, 1, count]));
                }
                let records = upload(&device, &records, BufferUsage::COPY_SRC);
                encoder.copy_buffer(
                    records.slice(0..records.desc().size),
                    &upscaling.arguments,
                    0,
                );
                let mut pass = encoder.compute_pass(&ComputePassDesc {
                    timestamps: None,
                    label: "filter cell selection",
                });
                pass.set_pipeline(&upscaling.filter);
                pass.bind(1, Bind::StorageBuffer(bitmap.slice(0..bitmap.desc().size)));
                for (i, grid) in upscaling.grids.iter().enumerate() {
                    let list = source[i].clone().unwrap_or_else(|| {
                        (0..grid.rows)
                            .flat_map(|y| (0..grid.columns).map(move |x| x | y << 16))
                            .collect()
                    });
                    let buffer = upload(&device, &words(list), BufferUsage::STORAGE);
                    pass.bind(0, Bind::Uniform(grid.parameters.slice(0..16)));
                    pass.bind(0, Bind::StorageBuffer(buffer.slice(0..buffer.desc().size)));
                    pass.bind(
                        2,
                        Bind::StorageBuffer(grid.compacted.slice(0..grid.compacted.desc().size)),
                    );
                    pass.bind(
                        3,
                        Bind::StorageBuffer(
                            upscaling
                                .arguments
                                .slice(grid.arguments..grid.arguments + 16),
                        ),
                    );
                    pass.dispatch((buffer.desc().size as u32 / 4).div_ceil(64), 1, 1);
                }
                drop(pass);
                let token = queue.submit(encoder).unwrap();
                assert!(queue.wait(token, Duration::from_secs(60)).unwrap());
                let arguments = read(&device, &mut queue, &upscaling.arguments);
                for (i, grid) in upscaling.grids.iter().enumerate() {
                    let count = arguments[grid.arguments as usize / 4] as usize;
                    let selected = read(&device, &mut queue, &grid.compacted);
                    let selected: BTreeSet<_> = selected[..count].iter().copied().collect();
                    assert_eq!(selected.len(), count, "duplicate cells");
                    let whole = || {
                        (0..grid.rows)
                            .flat_map(|y| (0..grid.columns).map(move |x| x | y << 16))
                            .collect::<Vec<_>>()
                    };
                    let expected: BTreeSet<_> = expected[i]
                        .clone()
                        .unwrap_or_else(whole)
                        .into_iter()
                        .collect();
                    let source: BTreeSet<_> = source[i]
                        .clone()
                        .unwrap_or_else(whole)
                        .into_iter()
                        .collect();
                    assert!(
                        expected.is_subset(&selected),
                        "form {form} model {m} pass {i} misses {:?}",
                        expected.difference(&selected).collect::<Vec<_>>()
                    );
                    assert!(selected.is_subset(&source));
                    if delta.is_empty() {
                        assert!(selected.is_empty());
                    }
                }
            }
        }
    }
}

// spec: HOST-VIEW-10
#[test]
fn unchanged_composition_launches_no_filter_or_model_workgroups() {
    use adv_gpu::encode::{BufferCopy, ImageCopy};
    let (device, mut queue) = Device::open(&DeviceOptions {
        software: !cfg!(any(target_vendor = "apple", target_os = "android")),
        validation: !cfg!(target_os = "android"),
        disable: adv_gpu::Modern::from_test_env(),
        ..Default::default()
    })
    .unwrap();
    let size = (33, 25);
    let upscaling = Upscaling::with_precision(&device, models::FUSED[0], size, false).unwrap();
    let output = device
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: size.0 * 2,
                height: size.1 * 2,
                depth_or_layers: 1,
            },
            format: OUTPUT,
            mips: 1,
            samples: 1,
            usage: Usage::STORAGE | Usage::COPY_SRC,
            view_formats: &[],
            label: "unchanged output",
        })
        .unwrap();
    let pixels = upload(
        &device,
        &vec![0; (size.0 * size.1 * 8) as usize],
        BufferUsage::COPY_SRC,
    );
    let bytes = u64::from(size.0 * size.1 * 16);
    let readback = device
        .create_buffer(&BufferDesc {
            size: bytes * 2,
            usage: BufferUsage::COPY_DST,
            memory: Memory::Readback,
            label: "unchanged output readback",
        })
        .unwrap();
    let mut ring = device.ring(1 << 20).unwrap();
    for frame in 0..2 {
        let exact = false;
        let input = upscaling.input(exact);
        let mut encoder = queue.encoder("unchanged composition");
        encoder.copy_buffer_to_image(
            BufferCopy {
                buffer: &pixels,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            ImageCopy {
                image: input,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            input.desc().size,
        );
        assert!(
            upscaling
                .encode(&mut encoder, &mut ring, output.view(), None, exact, None)
                .unwrap()
                > 0
        );
        encoder.copy_image_to_buffer(
            ImageCopy {
                image: &output,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            BufferCopy {
                buffer: &readback,
                offset: frame * bytes,
                row_pitch: 0,
                rows: 0,
            },
            output.desc().size,
        );
        let token = queue.submit(encoder).unwrap();
        assert!(queue.wait(token, Duration::from_secs(60)).unwrap());
    }
    for buffer in [&upscaling.gates, &upscaling.arguments] {
        let records = read(&device, &mut queue, buffer);
        for record in records.chunks(upscaling.argument_stride as usize / 4) {
            assert_eq!(record[0], 0);
        }
    }
    // SAFETY: both output copies completed.
    let pixels = unsafe { readback.mapped().unwrap().as_ref() };
    assert_eq!(&pixels[..bytes as usize], &pixels[bytes as usize..]);
    let mut encoder = queue.encoder("empty producer damage");
    assert_eq!(
        upscaling
            .encode(
                &mut encoder,
                &mut ring,
                output.view(),
                Some(&[]),
                false,
                None
            )
            .unwrap(),
        0
    );
}
