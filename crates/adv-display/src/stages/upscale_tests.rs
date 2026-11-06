use super::*;
use adv_gpu::encode::{BufferCopy, ImageCopy};
use adv_gpu::{Queue, queue::DeviceOptions};
use std::{collections::BTreeSet, time::Duration};

fn open() -> (Device, Queue) {
    Device::open(&DeviceOptions {
        software: !cfg!(any(target_vendor = "apple", target_os = "android")),
        validation: !cfg!(target_os = "android"),
        disable: adv_gpu::Modern::from_test_env(),
        ..Default::default()
    })
    .unwrap()
}

fn upload(device: &Device, bytes: &[u8], usage: BufferUsage) -> Buffer {
    let buffer = device
        .create_buffer(&BufferDesc {
            size: bytes.len() as u64,
            usage,
            memory: Memory::Upload,
            label: "upscale test upload",
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
            label: "upscale test readback",
        })
        .unwrap();
    let mut encoder = queue.encoder("upscale test readback");
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

fn whole_image(image: &Image) -> ImageCopy<'_> {
    ImageCopy {
        image,
        mip: 0,
        layer: 0,
        origin: (0, 0, 0),
    }
}

/// Writes `pixels` (RGBA16F) where composition must and encodes the frame.
fn frame(
    queue: &mut Queue,
    ring: &mut Ring,
    upscaling: &Upscaling,
    pixels: &Buffer,
    damage: Option<&[Rect]>,
    input: Input,
) -> u64 {
    let mut encoder = queue.encoder("upscale test frame");
    let target = upscaling.input();
    let size = upscaling.size();
    let whole = vec![Rect {
        x: 0,
        y: 0,
        w: size.0,
        h: size.1,
    }];
    for r in upscaling.composition(damage).unwrap_or(whole) {
        encoder.copy_buffer_to_image(
            BufferCopy {
                buffer: pixels,
                offset: u64::from((r.y * size.0 + r.x) * 8),
                row_pitch: size.0 * 8,
                rows: size.1,
            },
            ImageCopy {
                image: target,
                mip: 0,
                layer: 0,
                origin: (r.x, r.y, 0),
            },
            Extent {
                width: r.w,
                height: r.h,
                depth_or_layers: 1,
            },
        );
    }
    let computed = upscaling
        .encode((&mut encoder, ring), damage, input, None)
        .unwrap();
    let token = queue.submit(encoder).unwrap();
    assert!(queue.wait(token, Duration::from_secs(60)).unwrap());
    computed
}

/// The cells each pass selected in the last encode.
fn selected(device: &Device, queue: &mut Queue, upscaling: &Upscaling) -> Vec<BTreeSet<u32>> {
    let state = read(device, queue, &upscaling.state);
    let lists = read(device, queue, &upscaling.compacted);
    let set = upscaling.layout.arguments[(upscaling.epoch.get() % 2) as usize] as usize;
    upscaling
        .grids
        .iter()
        .enumerate()
        .map(|(i, grid)| {
            let count = state[set + upscaling.layout.records as usize + i] as usize;
            let start = grid.compacted as usize;
            let cells: BTreeSet<u32> = lists[start..start + count].iter().copied().collect();
            assert_eq!(cells.len(), count, "pass {i} lists a cell twice");
            cells
        })
        .collect()
}

/// The CPU dependency closure of a change within `damage`: walking back from
/// the last pass, a pass covers what later readers need of the tensors it
/// writes, and needs its inputs its radius beyond those cells.
fn closure(upscaling: &Upscaling, damage: &[Rect]) -> Vec<BTreeSet<u32>> {
    let size = upscaling.size;
    let model = upscaling.model;
    let grow = |r: Rect, by: u32| {
        let x = r.x.saturating_sub(by);
        let y = r.y.saturating_sub(by);
        Rect {
            x,
            y,
            w: (r.x + r.w + by).min(size.0).saturating_sub(x),
            h: (r.y + r.h + by).min(size.1).saturating_sub(y),
        }
    };
    let output = model.tensors.len();
    let mut needed = vec![Vec::new(); output + 1];
    needed[output] = damage.iter().map(|&r| grow(r, model.reach())).collect();
    let mut lists = vec![BTreeSet::new(); model.passes.len()];
    let last = model.passes.len() - 1;
    for (i, pass) in model.passes.iter().enumerate().rev() {
        let unit = pass.unit();
        let wanted: Vec<Rect> = pass
            .writes()
            .flat_map(|t| std::mem::take(&mut needed[t.unwrap_or(output)]))
            .filter(|r| r.w > 0 && r.h > 0)
            .collect();
        let mut reads = Vec::new();
        for r in &wanted {
            for y in r.y / unit..=(r.y + r.h - 1) / unit {
                for x in r.x / unit..=(r.x + r.w - 1) / unit {
                    // The output's need is its whole cells; inner passes
                    // need only the pixels, grown by each radius.
                    if lists[i].insert(x | y << 16) && i == last {
                        reads.push(Rect {
                            x: x * unit,
                            y: y * unit,
                            w: unit,
                            h: unit,
                        });
                    }
                }
            }
        }
        if i != last {
            reads = wanted;
        }
        for t in pass.reads() {
            needed[t].extend(reads.iter().map(|&r| grow(r, pass.radius)));
        }
    }
    lists
}

// spec: HOST-VIEW-10
#[test]
fn bookkeeping_lists_cover_the_changed_dependency_closure() {
    let (device, mut queue) = open();
    let mut ring = device.ring(1 << 20).unwrap();
    let size = (529, 137);
    let rect = |x, y, w, h| Rect { x, y, w, h };
    let matrix = Features::MATRIX_F16_8X8 | Features::SHADER_F16 | Features::SUBGROUP;
    let pixels = upload(
        &device,
        &vec![0x3c; (size.0 * size.1 * 8) as usize],
        BufferUsage::COPY_SRC,
    );
    for (form, table) in [models::REFERENCE, models::FUSED, models::MATRIX]
        .into_iter()
        .enumerate()
    {
        if form == 2 && !device.caps().features.contains(matrix) {
            continue;
        }
        for (m, model) in table.into_iter().enumerate() {
            let upscaling = Upscaling::with_precision(&device, model, size, form == 2).unwrap();
            let first = frame(
                &mut queue,
                &mut ring,
                &upscaling,
                &pixels,
                None,
                Input::Exact,
            );
            assert!(first > 0);
            for (i, cells) in selected(&device, &mut queue, &upscaling).iter().enumerate() {
                let grid = &upscaling.grids[i];
                assert_eq!(
                    cells.len() as u32,
                    grid.columns * grid.rows,
                    "first frames are whole"
                );
            }
            for delta in [
                vec![rect(256, 64, 8, 8)],
                vec![rect(0, 0, 8, 8), rect(528, 136, 1, 1)],
                vec![rect(120, 0, 16, 137)],
            ] {
                frame(
                    &mut queue,
                    &mut ring,
                    &upscaling,
                    &pixels,
                    Some(&delta),
                    Input::Exact,
                );
                covers_closure(
                    &device,
                    &mut queue,
                    &upscaling,
                    &delta,
                    &format!("form {form} model {m}"),
                );
            }
        }
    }
}

/// Every pass selected the cells of the change's closure it can reach, and
/// nothing outside the closure: the cells meeting the output cells' need,
/// grown in pixels. `delta` covers whole 8x8 cells (clipped to
/// the canvas), the granularity bookkeeping sees.
fn covers_closure(
    device: &Device,
    queue: &mut Queue,
    upscaling: &Upscaling,
    delta: &[Rect],
    label: &str,
) {
    let selected = selected(device, queue, upscaling);
    let closure = closure(upscaling, delta);
    for (i, pass) in upscaling.model.passes.iter().enumerate() {
        // Valid cells the change cannot reach keep their values; every
        // reached cell of the closure recomputes.
        let reach = upscaling.grids[i].affected;
        let unit = pass.unit();
        let reached = |cell: &u32| {
            let (x, y) = ((cell & 65535) * unit, (cell >> 16) * unit);
            delta.iter().any(|r| {
                x < r.x + r.w + reach
                    && r.x < x + unit + reach
                    && y < r.y + r.h + reach
                    && r.y < y + unit + reach
            })
        };
        let required: BTreeSet<u32> = closure[i].iter().copied().filter(reached).collect();
        assert!(
            required.is_subset(&selected[i]),
            "{label} pass {i} misses {:?}",
            required.difference(&selected[i]).collect::<Vec<_>>()
        );
        assert!(
            selected[i].is_subset(&closure[i]),
            "{label} pass {i} adds {:?}",
            selected[i].difference(&closure[i]).collect::<Vec<_>>()
        );
    }
}

// spec: HOST-VIEW-10
#[test]
fn large_canvases_select_only_the_changed_closure() {
    let (device, mut queue) = open();
    let mut ring = device.ring(1 << 20).unwrap();
    // 4K is the largest 16:9 canvas within the cell limit.
    let size = (3840, 2160);
    let pixels = upload(
        &device,
        &vec![0x3c; (size.0 * size.1 * 8) as usize],
        BufferUsage::COPY_SRC,
    );
    let upscaling = Upscaling::with_precision(&device, models::FUSED[0], size, false).unwrap();
    frame(
        &mut queue,
        &mut ring,
        &upscaling,
        &pixels,
        None,
        Input::Exact,
    );
    for delta in [
        vec![Rect {
            x: 3832,
            y: 2152,
            w: 8,
            h: 8,
        }],
        vec![Rect {
            x: 0,
            y: 1000,
            w: 3840,
            h: 8,
        }],
    ] {
        frame(
            &mut queue,
            &mut ring,
            &upscaling,
            &pixels,
            Some(&delta),
            Input::Exact,
        );
        covers_closure(&device, &mut queue, &upscaling, &delta, "4K");
    }
}

// spec: HOST-VIEW-10
#[test]
fn unchanged_composition_launches_no_model_workgroups() {
    let (device, mut queue) = open();
    let size = (33, 25);
    let upscaling = Upscaling::with_precision(&device, models::FUSED[0], size, false).unwrap();
    let pixels = upload(
        &device,
        &vec![0; (size.0 * size.1 * 8) as usize],
        BufferUsage::COPY_SRC,
    );
    let mut ring = device.ring(1 << 20).unwrap();
    let mut outputs = vec![];
    for frame_index in 0..3 {
        assert!(
            frame(
                &mut queue,
                &mut ring,
                &upscaling,
                &pixels,
                None,
                Input::Composed,
            ) > 0
        );
        let lists = selected(&device, &mut queue, &upscaling);
        let grid = upscaling.grids.last().unwrap();
        let expected = if frame_index == 0 {
            grid.columns * grid.rows
        } else {
            0
        };
        assert_eq!(lists.last().unwrap().len() as u32, expected);
        let readback = device
            .create_buffer(&BufferDesc {
                size: u64::from(size.0 * size.1 * 16),
                usage: BufferUsage::COPY_DST,
                memory: Memory::Readback,
                label: "unchanged output readback",
            })
            .unwrap();
        let mut encoder = queue.encoder("unchanged output");
        let output = upscaling.output();
        encoder.copy_image_to_buffer(
            whole_image(output),
            BufferCopy {
                buffer: &readback,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            output.desc().size,
        );
        let token = queue.submit(encoder).unwrap();
        assert!(queue.wait(token, Duration::from_secs(60)).unwrap());
        // SAFETY: the copy completed.
        outputs.push(unsafe { readback.mapped().unwrap().as_ref() }.to_vec());
    }
    assert_eq!(outputs[0], outputs[1]);
    assert_eq!(outputs[0], outputs[2]);
    let mut encoder = queue.encoder("empty producer damage");
    assert_eq!(
        upscaling
            .encode((&mut encoder, &mut ring), Some(&[]), Input::Composed, None)
            .unwrap(),
        0
    );
}

fn upscaling_last(upscaling: &Upscaling) -> usize {
    upscaling.model.passes.len() - 1
}

/// Output bytes where the first run differs from the second, its whole-frame
/// reference.
fn mismatches(device: &Device, queue: &mut Queue, runs: &[Upscaling; 2]) -> usize {
    let size = runs[0].output().desc().size;
    let bytes = u64::from(size.width * size.height * 4);
    let readback = device
        .create_buffer(&BufferDesc {
            size: bytes * 2,
            usage: BufferUsage::COPY_DST,
            memory: Memory::Readback,
            label: "upscale output comparison",
        })
        .unwrap();
    let mut encoder = queue.encoder("upscale output comparison");
    for (i, run) in runs.iter().enumerate() {
        encoder.copy_image_to_buffer(
            whole_image(run.output()),
            BufferCopy {
                buffer: &readback,
                offset: i as u64 * bytes,
                row_pitch: 0,
                rows: 0,
            },
            size,
        );
    }
    let token = queue.submit(encoder).unwrap();
    assert!(queue.wait(token, Duration::from_secs(60)).unwrap());
    // SAFETY: both image copies completed.
    let both = unsafe { readback.mapped().unwrap().as_ref() };
    let (actual, expected) = both[..bytes as usize * 2].split_at(bytes as usize);
    actual.iter().zip(expected).filter(|(a, b)| a != b).count()
}

// spec: HOST-VIEW-10, HOST-VIEW-13
#[test]
fn invalid_tensor_neighbours_recompute_after_rebuilt_outputs() {
    let (device, mut queue) = open();
    let size = (97u32, 81u32);
    let mut ring = device.ring(1 << 20).unwrap();
    let mut seed = 0x6d31_a207u32;
    let mut pixels = vec![0u8; (size.0 * size.1 * 8) as usize];
    let matrix = Features::MATRIX_F16_8X8 | Features::SHADER_F16 | Features::SUBGROUP;
    for half in [false, true] {
        if half && !device.caps().features.contains(Features::SHADER_F16) {
            continue;
        }
        for (form, table) in [models::REFERENCE, models::FUSED, models::MATRIX]
            .into_iter()
            .enumerate()
        {
            if form == 2 && (!half || !device.caps().features.contains(matrix)) {
                continue;
            }
            for (m, model) in table.into_iter().enumerate() {
                let runs = [
                    Upscaling::with_precision(&device, model, size, half).unwrap(),
                    Upscaling::with_precision(&device, model, size, half).unwrap(),
                ];
                for index in 0..12 {
                    // Pans alternate with small software changes inside the
                    // rebuilt area, whose neighbours' tensors went unused.
                    let pan = index >= 2 && index % 3 != 1;
                    let small = Rect {
                        x: 40 + index,
                        y: 40,
                        w: 3,
                        h: 2,
                    };
                    let old = pixels.clone();
                    for y in 0..size.1 {
                        for x in 0..size.0 {
                            let at = ((y * size.0 + x) * 8) as usize;
                            let fresh = index < 2
                                || (pan && x == 0)
                                || (!pan
                                    && (small.x..small.x + small.w).contains(&x)
                                    && (small.y..small.y + small.h).contains(&y));
                            if fresh {
                                for channel in pixels[at..at + 8].as_chunks_mut::<2>().0 {
                                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                                    channel.copy_from_slice(&half_bits((seed >> 16) % 257));
                                }
                            } else if pan {
                                let from = ((y * size.0 + x - 1) * 8) as usize;
                                pixels[at..at + 8].copy_from_slice(&old[from..from + 8]);
                            }
                        }
                    }
                    let buffer = upload(&device, &pixels, BufferUsage::COPY_SRC);
                    let damage = [small];
                    let (damage, input) = if pan || index < 2 {
                        (None, Input::Composed)
                    } else {
                        (Some(&damage[..]), Input::Exact)
                    };
                    frame(&mut queue, &mut ring, &runs[0], &buffer, damage, input);
                    frame(&mut queue, &mut ring, &runs[1], &buffer, None, Input::Exact);
                    let mismatches = mismatches(&device, &mut queue, &runs);
                    // A pan copies the old output one pixel over: only cells
                    // whose field meets the fresh column or an x border compute.
                    if pan {
                        let last = upscaling_last(&runs[0]);
                        let unit = model.passes.last().unwrap().unit();
                        let reach = model.reach();
                        let selected = selected(&device, &mut queue, &runs[0]);
                        let border = |cell: &u32| {
                            let x = (cell & 65535) * unit;
                            x < 8 + reach || (x + unit).min(size.0) + reach > size.0
                        };
                        assert!(
                            selected[last].iter().all(border),
                            "form={form} model={m} half={half} frame={index} computes {:?}",
                            selected[last]
                                .iter()
                                .filter(|c| !border(c))
                                .collect::<Vec<_>>()
                        );
                    }
                    assert_eq!(
                        mismatches, 0,
                        "form={form} model={m} half={half} frame={index}"
                    );
                }
            }
        }
    }
}

/// A repeatable texture level in 1/256 steps over the whole plane.
fn texture(seed: u32, x: i32, y: i32) -> u32 {
    let mut h = seed ^ (x as u32).wrapping_mul(0x9e37_79b1) ^ (y as u32).wrapping_mul(0x85eb_ca77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h % 257
}

// spec: HOST-VIEW-13
#[test]
fn offsets_are_verified_only_near_their_votes() {
    let (device, mut queue) = open();
    let size = (257u32, 193u32);
    let mut ring = device.ring(1 << 21).unwrap();
    let model = models::FUSED[0];
    let runs = [
        Upscaling::with_precision(&device, model, size, false).unwrap(),
        Upscaling::with_precision(&device, model, size, false).unwrap(),
    ];
    // New content everywhere but a sprite that moved: only the sprite votes.
    let sprites = [(40, 30), (45, 26)];
    for (index, sprite) in sprites.into_iter().enumerate() {
        let mut pixels = Vec::with_capacity((size.0 * size.1 * 8) as usize);
        for y in 0..size.1 as i32 {
            for x in 0..size.0 as i32 {
                let inside = (sprite.0..sprite.0 + 48).contains(&x)
                    && (sprite.1..sprite.1 + 40).contains(&y);
                let (seed, u, v) = if inside {
                    (7, x - sprite.0, y - sprite.1)
                } else {
                    (11 + 4 * index as u32, x, y)
                };
                for channel in 0..4 {
                    pixels.extend(half_bits(texture(seed + channel, u, v)));
                }
            }
        }
        let buffer = upload(&device, &pixels, BufferUsage::COPY_SRC);
        frame(
            &mut queue,
            &mut ring,
            &runs[0],
            &buffer,
            None,
            Input::Composed,
        );
        frame(&mut queue, &mut ring, &runs[1], &buffer, None, Input::Exact);
        assert_eq!(mismatches(&device, &mut queue, &runs), 0, "frame {index}");
    }
    let facts = read(&device, &mut queue, &runs[0].facts);
    let epoch = runs[0].epoch.get();
    let r = model.neighbourhood() as i32;
    let columns = size.0.div_ceil(8) as i32;
    let (x0, y0) = (45 / 8 - r, 26 / 8 - r);
    let (x1, y1) = ((45 + 47) / 8 + r, (26 + 39) / 8 + r);
    let mut verified = 0;
    for (at, fact) in facts.chunks(FACT_WORDS as usize).enumerate() {
        if fact[1] != epoch {
            continue;
        }
        let (x, y) = (at as i32 % columns, at as i32 / columns);
        assert!(
            (x0..=x1).contains(&x) && (y0..=y1).contains(&y),
            "cell ({x}, {y}) verified far from the votes"
        );
        verified += 1;
    }
    assert!(verified > 0, "the sprite's offset was never verified");
    let last = upscaling_last(&runs[0]);
    let grid = &runs[0].grids[last];
    let computed = selected(&device, &mut queue, &runs[0])[last].len();
    assert!(
        computed < (grid.columns * grid.rows) as usize,
        "the moved sprite copied nothing"
    );
}

// spec: HOST-VIEW-13
#[test]
fn offset_hypotheses_copy_moving_layers_exactly() {
    let (device, mut queue) = open();
    let size = (257u32, 193u32);
    let mut ring = device.ring(1 << 21).unwrap();
    let matrix = Features::MATRIX_F16_8X8 | Features::SHADER_F16 | Features::SUBGROUP;
    // Background offset and sprite position per frame: a pan with a sprite
    // moving across it, a new direction, a repeat of the sprite elsewhere,
    // and a jump back to an older position.
    type Frame = ((i32, i32), (i32, i32), bool);
    let frames: [Frame; 7] = [
        ((0, 0), (40, 30), false),
        ((2, 0), (45, 26), false),
        ((4, 0), (50, 22), false),
        ((6, 0), (55, 18), false),
        ((6, 3), (55, 18), false),
        ((6, 6), (55, 18), true),
        ((0, 0), (40, 30), false),
    ];
    for half in [false, true] {
        if half && !device.caps().features.contains(Features::SHADER_F16) {
            continue;
        }
        for (form, table) in [models::REFERENCE, models::FUSED, models::MATRIX]
            .into_iter()
            .enumerate()
        {
            if form == 2 && (!half || !device.caps().features.contains(matrix)) {
                continue;
            }
            for (m, model) in table.into_iter().enumerate() {
                let runs = [
                    Upscaling::with_precision(&device, model, size, half).unwrap(),
                    Upscaling::with_precision(&device, model, size, half).unwrap(),
                ];
                for (index, &(background, sprite, twin)) in frames.iter().enumerate() {
                    let inside = |x: i32, y: i32, at: (i32, i32)| {
                        (at.0..at.0 + 48).contains(&x) && (at.1..at.1 + 40).contains(&y)
                    };
                    let mut pixels = Vec::with_capacity((size.0 * size.1 * 8) as usize);
                    for y in 0..size.1 as i32 {
                        for x in 0..size.0 as i32 {
                            let twin_at = (sprite.0 + 120, sprite.1 + 90);
                            let (seed, u, v) = if inside(x, y, sprite) {
                                (7, x - sprite.0, y - sprite.1)
                            } else if twin && inside(x, y, twin_at) {
                                (7, x - twin_at.0, y - twin_at.1)
                            } else {
                                (11, x - background.0, y - background.1)
                            };
                            for channel in 0..4 {
                                pixels.extend(half_bits(texture(seed + channel, u, v)));
                            }
                        }
                    }
                    let buffer = upload(&device, &pixels, BufferUsage::COPY_SRC);
                    frame(
                        &mut queue,
                        &mut ring,
                        &runs[0],
                        &buffer,
                        None,
                        Input::Composed,
                    );
                    frame(&mut queue, &mut ring, &runs[1], &buffer, None, Input::Exact);
                    let label = format!("form={form} model={m} half={half} frame={index}");
                    assert_eq!(mismatches(&device, &mut queue, &runs), 0, "{label}");
                    // A steady pan reuses most of the output.
                    if index == 3 {
                        let grid = &runs[0].grids[upscaling_last(&runs[0])];
                        let computed =
                            selected(&device, &mut queue, &runs[0])[upscaling_last(&runs[0])].len();
                        let cells = (grid.columns * grid.rows) as usize;
                        assert!(
                            computed * 2 < cells,
                            "{label}: {computed} of {cells} computed"
                        );
                    }
                }
            }
        }
    }
}
