use adv_device::surface::{
    BlitOptions, BlitRect, FlushReason, PixelFormat, Rect, Sampling, Snapshot, Sprite, SpriteAlpha,
    SpriteMode, SurfaceMemory, Surfaces, VideoCursorRegion, VideoFrame, VideoLayer, frame_mailbox,
};
use std::borrow::Cow;
use std::sync::Arc;

struct Memory {
    bytes: Vec<u8>,
    owned_reads: bool,
    denied: Option<(u32, usize)>,
}

impl Memory {
    fn new(owned_reads: bool) -> Self {
        Self {
            bytes: vec![0; 65536],
            owned_reads,
            denied: None,
        }
    }
}
impl SurfaceMemory for Memory {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        let data = self
            .bytes
            .get(addr as usize..addr as usize + len)
            .ok_or("read bounds")?;
        Ok(if self.owned_reads {
            Cow::Owned(data.to_vec())
        } else {
            Cow::Borrowed(data)
        })
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        self.writable(addr, data.len())?;
        self.bytes[addr as usize..addr as usize + data.len()].copy_from_slice(data);
        Ok(())
    }
    fn writable(&self, addr: u32, len: usize) -> Result<(), String> {
        self.read(addr, len)?;
        if let Some((start, count)) = self.denied
            && (addr as usize) < start as usize + count
            && (start as usize) < addr as usize + len
        {
            return Err("write protected".into());
        }
        Ok(())
    }
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
        self.read(src, len)?;
        self.writable(dst, len)?;
        self.bytes
            .copy_within(src as usize..src as usize + len, dst as usize);
        Ok(())
    }
}
fn store() -> Surfaces<Snapshot> {
    Surfaces::new(5, 4, 16, 65520, Snapshot::default()).unwrap()
}

fn video_layer() -> VideoLayer {
    VideoLayer {
        frame: VideoFrame {
            width: 2,
            height: 1,
            pitch: 8,
            bgra: Arc::from([22, 0, 0, 255, 33, 0, 0, 255]),
        },
        source: Rect {
            x: 0,
            y: 0,
            w: 2,
            h: 1,
        },
        destination: BlitRect {
            x: -1,
            y: 1,
            w: 4,
            h: 2,
        },
        background: Some((
            BlitRect {
                x: 0,
                y: 1,
                w: 4,
                h: 2,
            },
            [9, 0, 0, 255],
        )),
        clip: Rect {
            x: 0,
            y: 1,
            w: 4,
            h: 2,
        },
        canvas: (5, 4),
        visible: true,
        hide_cursor: true,
    }
}

#[test]
// spec: DEV-FRAME-11
fn deleting_the_pending_source_discards_its_frame() {
    let mut store = store();
    let mut memory = Memory::new(false);
    let (producer, mut consumer) = frame_mailbox();
    store.set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
    let first = store
        .create(5, 4, false, PixelFormat::Bgra32, &mut memory)
        .unwrap();
    store.present(first, &memory, store[first].rect()).unwrap();
    assert!(store.delete(first));
    assert!(!store.flush(&memory, FlushReason::Immediate).unwrap());
    assert!(!consumer.update());
    let second = store
        .create(5, 4, false, PixelFormat::Bgra32, &mut memory)
        .unwrap();
    store
        .present(second, &memory, store[second].rect())
        .unwrap();
    assert!(store.flush(&memory, FlushReason::Immediate).unwrap());
    assert!(consumer.update());
}

#[test]
// spec: MEDIA-VIDEO-4
fn video_updates_clip_and_restore_an_immutable_base_without_game_present() {
    let mut store = store();
    let mut memory = Memory::new(false);
    let base = store
        .create(5, 4, false, PixelFormat::Bgra32, &mut memory)
        .unwrap();
    let pixels = [11, 12, 13, 255].repeat(20);
    memory.write(store[base].pixels, &pixels).unwrap();
    let (producer, mut consumer) = frame_mailbox();
    store.set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
    store.present(base, &memory, store[base].rect()).unwrap();
    store
        .flush(&memory, adv_device::surface::FlushReason::Immediate)
        .unwrap();
    // Later writes are not a presentation and cannot replace the retained base.
    memory.write(store[base].pixels, &[0; 80]).unwrap();
    let layer = video_layer();
    assert_eq!(
        store
            .set_video_layer(7, Some(layer.clone()), &memory)
            .unwrap(),
        2
    );
    assert!(consumer.update());
    assert!(consumer.current().hide_cursor());
    let mut cursor = consumer.current().video_cursor_regions().unwrap();
    assert!(VideoCursorRegion::hidden_at(&cursor, 1, 1));
    assert!(!VideoCursorRegion::hidden_at(&cursor, 4, 1));
    cursor.push(VideoCursorRegion {
        rectangle: Rect {
            x: 1,
            y: 1,
            w: 1,
            h: 1,
        },
        hide_cursor: false,
    });
    assert!(!VideoCursorRegion::hidden_at(&cursor, 1, 1));
    assert!(VideoCursorRegion::hidden_at(&cursor, 2, 1));
    let mut composed = Snapshot::default();
    consumer.current().snapshot(&mut composed).unwrap();
    assert_eq!(&composed.bgra[..20], &pixels[..20]);
    assert_eq!(
        &composed.bgra[20..36],
        &[22, 0, 0, 255, 33, 0, 0, 255, 33, 0, 0, 255, 9, 0, 0, 255]
    );
    assert_eq!(&composed.bgra[36..40], &pixels[36..40]);
    let mut patch = Snapshot::default();
    let bounds = layer.rasterize((5, 4), &mut patch).unwrap().unwrap();
    assert_eq!(
        bounds,
        Rect {
            x: 0,
            y: 1,
            w: 4,
            h: 2
        }
    );
    assert_eq!(patch.bgra[..16], composed.bgra[20..36]);
    let mut hidden = layer.clone();
    hidden.visible = false;
    store.set_video_layer(7, Some(hidden), &memory).unwrap();
    assert!(consumer.update());
    assert!(!consumer.current().hide_cursor());
    consumer.current().snapshot(&mut composed).unwrap();
    assert_eq!(composed.bgra, pixels);
    store.set_video_layer(7, Some(layer), &memory).unwrap();
    assert_eq!(store.set_video_layer(7, None, &memory).unwrap(), 5);
    assert!(consumer.update());
    consumer.current().snapshot(&mut composed).unwrap();
    assert_eq!(composed.bgra, pixels);
    assert_eq!(store.set_video_layer(7, None, &memory).unwrap(), 5);

    // USER's 960x544 client and a 1280x720 backbuffer describe the same whole
    // canvas. Scale the video, border and clip together, rather than covering 75%.
    let mut scaled = video_layer();
    scaled.canvas = (960, 544);
    scaled.destination = BlitRect {
        x: 0,
        y: 0,
        w: 960,
        h: 544,
    };
    scaled.background = Some((scaled.destination, [9, 0, 0, 255]));
    scaled.clip = Rect {
        x: 0,
        y: 0,
        w: 960,
        h: 544,
    };
    assert_eq!(
        scaled.rasterize((1280, 720), &mut patch).unwrap(),
        Some(Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 720
        })
    );
    assert_eq!((patch.width, patch.height), (1280, 720));
    assert_eq!(&patch.bgra[639 * 4..640 * 4], &[22, 0, 0, 255]);
    assert_eq!(&patch.bgra[640 * 4..641 * 4], &[33, 0, 0, 255]);
    assert_eq!(&patch.bgra[patch.bgra.len() - 4..], &[33, 0, 0, 255]);
    store.set_video_layer(7, Some(scaled), &memory).unwrap();
    assert!(consumer.update());
    consumer.current().snapshot(&mut composed).unwrap();
    for row in composed.bgra.as_chunks::<20>().0 {
        assert_eq!(
            row,
            &[
                22, 0, 0, 255, 22, 0, 0, 255, 33, 0, 0, 255, 33, 0, 0, 255, 33, 0, 0, 255
            ]
        );
    }
    store.set_video_layer(7, None, &memory).unwrap();
    assert!(consumer.update());
    consumer.current().snapshot(&mut composed).unwrap();
    assert_eq!(composed.bgra, pixels);
}

#[test]
// spec: MEDIA-VIDEO-4
fn video_only_mailbox_coalesces_to_latest_and_capture_uses_same_composition() {
    let memory = Memory::new(false);
    let mut store = store();
    let (producer, mut consumer) = frame_mailbox();
    store.set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
    for color in 1..=100u8 {
        let mut layer = video_layer();
        layer.frame.bgra = Arc::from([color, 0, 0, 255, color, 0, 0, 255]);
        store.set_video_layer(3, Some(layer), &memory).unwrap();
    }
    assert!(consumer.update());
    assert!(!consumer.update());
    let mut shown = Snapshot::default();
    consumer.current().snapshot(&mut shown).unwrap();
    assert_eq!(shown.frames, 100);
    assert_eq!(&shown.bgra[..4], &[0; 4]);
    assert_eq!(&shown.bgra[20..24], &[100, 0, 0, 255]);
    assert!(store.capture_latest(&memory).unwrap());
    assert_eq!(store.presenter.bgra, shown.bgra);
    assert_eq!(store.stats.coalesced_frames, 99);
    store.set_video_layer(3, None, &memory).unwrap();
    store.capture_latest(&memory).unwrap();
    assert!(store.presenter.bgra.iter().all(|byte| *byte == 0));
}

// spec: PROC-DEV-1
#[test]
fn foreground_mailbox_moves_between_isolated_stores_with_monotonic_frames() {
    let mut parent = store();
    let mut child = store();
    let mut parent_memory = Memory::new(false);
    let mut child_memory = Memory::new(false);
    let parent_surface = parent
        .create(1, 1, false, PixelFormat::Bgra32, &mut parent_memory)
        .unwrap();
    let child_surface = child
        .create(1, 1, false, PixelFormat::Bgra32, &mut child_memory)
        .unwrap();
    let (producer, mut consumer) = frame_mailbox();
    parent.set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
    child_memory
        .write(child[child_surface].pixels, &[5, 6, 7, 8])
        .unwrap();
    for value in [3, 2, 1] {
        parent_memory
            .write(parent[parent_surface].pixels, &[value, 2, 3, 4])
            .unwrap();
        parent
            .present(
                parent_surface,
                &parent_memory,
                parent[parent_surface].rect(),
            )
            .unwrap();
        parent
            .flush(&parent_memory, adv_device::surface::FlushReason::Immediate)
            .unwrap();
    }
    assert!(
        parent
            .transfer_frame_mailbox_to(&mut child, &parent_memory)
            .unwrap()
    );
    assert!(
        !parent
            .transfer_frame_mailbox_to(&mut store(), &parent_memory)
            .unwrap()
    );
    child
        .present(child_surface, &child_memory, child[child_surface].rect())
        .unwrap();
    child
        .flush(&child_memory, adv_device::surface::FlushReason::Immediate)
        .unwrap();
    assert!(consumer.update());
    let mut snapshot = Snapshot::default();
    consumer.current().snapshot(&mut snapshot).unwrap();
    assert_eq!(
        (snapshot.frames, snapshot.bgra.as_slice()),
        (4, &[5, 6, 7, 8][..])
    );
    // Background output stays local and cannot overwrite the foreground child.
    parent
        .present(
            parent_surface,
            &parent_memory,
            parent[parent_surface].rect(),
        )
        .unwrap();
    parent
        .flush(&parent_memory, adv_device::surface::FlushReason::Immediate)
        .unwrap();
    assert!(!consumer.update());
    assert!(
        child
            .transfer_frame_mailbox_to(&mut parent, &child_memory)
            .unwrap()
    );
    parent
        .present(
            parent_surface,
            &parent_memory,
            parent[parent_surface].rect(),
        )
        .unwrap();
    parent
        .flush(&parent_memory, adv_device::surface::FlushReason::Immediate)
        .unwrap();
    assert!(consumer.update());
    consumer.current().snapshot(&mut snapshot).unwrap();
    assert_eq!(
        (snapshot.frames, snapshot.bgra.as_slice()),
        (5, &[1, 2, 3, 4][..])
    );
}

#[test]
fn mailbox_handoff_rejects_an_owned_destination_without_losing_the_source() {
    let parent_memory = Memory::new(false);
    let mut parent = store();
    let mut child = store();
    let (parent_producer, _) = frame_mailbox();
    let (child_producer, _) = frame_mailbox();
    parent.set_frame_mailbox(parent_producer, adv_sync::WakeEvent::new().unwrap());
    child.set_frame_mailbox(child_producer, adv_sync::WakeEvent::new().unwrap());
    assert!(
        parent
            .transfer_frame_mailbox_to(&mut child, &parent_memory)
            .is_err()
    );
    assert!(
        parent
            .transfer_frame_mailbox_to(&mut store(), &parent_memory)
            .unwrap()
    );
}

#[test]
fn headless_foreground_handoff_keeps_deferred_capture_and_frame_sequence() {
    let mut parent = store();
    let mut child = store();
    let mut parent_memory = Memory::new(false);
    let mut child_memory = Memory::new(false);
    parent.set_deferred_presentation(true).unwrap();
    let parent_surface = parent
        .create(1, 1, false, PixelFormat::Bgra32, &mut parent_memory)
        .unwrap();
    let child_surface = child
        .create(1, 1, false, PixelFormat::Bgra32, &mut child_memory)
        .unwrap();
    for value in 1..=3 {
        parent
            .fill_raw(
                parent_surface,
                parent[parent_surface].rect(),
                value,
                &mut parent_memory,
            )
            .unwrap();
        parent
            .present(
                parent_surface,
                &parent_memory,
                parent[parent_surface].rect(),
            )
            .unwrap();
        parent
            .flush(&parent_memory, adv_device::surface::FlushReason::Immediate)
            .unwrap();
    }
    assert!(
        !parent
            .transfer_frame_mailbox_to(&mut child, &parent_memory)
            .unwrap()
    );
    child
        .present(child_surface, &child_memory, child[child_surface].rect())
        .unwrap();
    child
        .flush(&child_memory, adv_device::surface::FlushReason::Immediate)
        .unwrap();
    assert_eq!(child.presenter.frames, 0);
    assert!(child.capture_latest(&child_memory).unwrap());
    assert_eq!(child.presenter.frames, 4);
}

#[test]
fn segmented_storage_reuses_deleted_blocks_and_rolls_back_initialization_failure() {
    struct FailedWrite<'a>(&'a mut Memory);
    impl SurfaceMemory for FailedWrite<'_> {
        fn read(&self, address: u32, length: usize) -> Result<Cow<'_, [u8]>, String> {
            self.0.read(address, length)
        }
        fn writable(&self, address: u32, length: usize) -> Result<(), String> {
            self.0.writable(address, length)
        }
        fn write(&mut self, _: u32, _: &[u8]) -> Result<(), String> {
            Err("authored write failure".into())
        }
        fn copy(&mut self, source: u32, destination: u32, length: usize) -> Result<(), String> {
            self.0.copy(source, destination, length)
        }
    }
    let mut memory = Memory::new(false);
    let mut s = Surfaces::new(4, 4, 16, 128, Snapshot::default()).unwrap();
    assert!(
        s.create(
            4,
            4,
            false,
            PixelFormat::Bgra32,
            &mut FailedWrite(&mut memory)
        )
        .is_err()
    );
    assert_eq!((s.arena_usage().free, s.live_surfaces()), (128, 0));
    let first = s
        .create(4, 4, false, PixelFormat::Bgra32, &mut memory)
        .unwrap();
    let second = s
        .create(4, 4, false, PixelFormat::Bgra32, &mut memory)
        .unwrap();
    assert_eq!(
        (s.arena_usage().pixel_bytes, s.arena_usage().largest_free),
        (128, 0)
    );
    assert_ne!(s.arena_growth(80), Some(0));
    assert!(
        s.extend_arena(32, 128).is_err(),
        "overlapping ranges are never adopted"
    );
    assert_eq!(s.arena_usage().capacity, 128);
    s.extend_arena(4096, 128).unwrap();
    let third = s
        .create(5, 4, false, PixelFormat::Bgra32, &mut memory)
        .unwrap();
    assert_eq!(s.get(third).unwrap().pixels, 4096);
    assert!(s.delete(first));
    assert!(s.delete(second));
    assert!(s.delete(third));
    for _ in 0..32 {
        let id = s
            .create(8, 4, false, PixelFormat::Bgra32, &mut memory)
            .unwrap();
        assert_eq!(s.get(id).unwrap().pixels, 16);
        assert!(s.delete(id));
    }
    assert_eq!(
        (
            s.arena_usage().capacity,
            s.arena_usage().free,
            s.live_surfaces()
        ),
        (256, 256, 0)
    );
}
fn logical_pixels(s: &Surfaces<Snapshot>, id: u32, m: &Memory) -> Vec<Vec<u8>> {
    let surface = &s[id];
    (0..surface.height)
        .flat_map(|y| {
            (0..surface.width).map(move |x| {
                m.read(surface.row(x, y), surface.format.bytes() as usize)
                    .unwrap()
                    .into_owned()
            })
        })
        .collect()
}
fn pattern(s: &Surfaces<Snapshot>, id: u32, m: &mut Memory) {
    let surface = &s[id];
    for y in 0..surface.height {
        for x in 0..surface.width {
            let p = [1 + x as u8 + y as u8 * 5, 0x36, 0x98, 0xaa];
            m.write(surface.row(x, y), &p[..surface.format.bytes() as usize])
                .unwrap();
        }
        let padding = surface.pitch - surface.width * surface.format.bytes();
        if padding != 0 {
            m.write(
                surface.row(0, y) + surface.width * surface.format.bytes(),
                &[0xcc; 3][..padding as usize],
            )
            .unwrap();
        }
    }
}

#[test]
fn packed_formats_encode_known_pixels_without_an_owned_result() {
    for (format, expected) in [
        (PixelFormat::Rgb565, [0x21, 0xec, 0, 0]),
        (PixelFormat::Rgb555, [0x01, 0x76, 0, 0]),
    ] {
        assert_eq!(format.encode([13, 135, 242, 99]), expected);
    }
    assert_eq!(
        PixelFormat::Bgra32.encode([13, 135, 242, 99]),
        [13, 135, 242, 99]
    );
    assert_eq!(PixelFormat::Rgb565.decode(&[0x1f, 0xf8]), [255, 0, 255, 0]);
    assert_eq!(PixelFormat::Rgb555.decode(&[0xe0, 0x03]), [0, 255, 0, 0]);
}

#[test]
fn cross_format_rows_preserve_pitch_orientation_and_permission_checks() {
    for owned in [false, true] {
        for bottom_up in [false, true] {
            let mut m = Memory::new(owned);
            let mut s = store();
            let src = s
                .create(3, 2, bottom_up, PixelFormat::Bgra32, &mut m)
                .unwrap();
            let dst = s
                .create(3, 2, !bottom_up, PixelFormat::Rgb565, &mut m)
                .unwrap();
            for (y, row) in [
                [0, 0, 255, 77, 0, 255, 0, 77, 255, 0, 0, 77],
                [255, 255, 255, 77, 0, 0, 0, 77, 255, 0, 255, 77],
            ]
            .iter()
            .enumerate()
            {
                m.write(s[src].row(0, y as u32), row).unwrap();
                m.write(s[dst].row(0, y as u32) + 6, &[0xaa, 0xbb]).unwrap();
            }
            let rect = s[src].rect();
            s.copy(src, rect, dst, rect, &mut m).unwrap();
            assert_eq!(
                m.read(s[dst].row(0, 0), 8).unwrap().as_ref(),
                &[0, 0xf8, 0xe0, 7, 0x1f, 0, 0xaa, 0xbb]
            );
            assert_eq!(
                m.read(s[dst].row(0, 1), 8).unwrap().as_ref(),
                &[255, 255, 0, 0, 0x1f, 0xf8, 0xaa, 0xbb]
            );
            let back = s
                .create(3, 2, bottom_up, PixelFormat::Rgb555, &mut m)
                .unwrap();
            s.copy(dst, rect, back, rect, &mut m).unwrap();
            assert_eq!(
                m.read(s[back].row(0, 0), 6).unwrap().as_ref(),
                &[0, 0x7c, 0xe0, 3, 0x1f, 0]
            );
            m.denied = Some((s[dst].row(0, 0), 6));
            let before = logical_pixels(&s, dst, &m);
            assert_eq!(
                s.copy(src, rect, dst, rect, &mut m).unwrap_err(),
                "write protected"
            );
            assert_eq!(logical_pixels(&s, dst, &m), before);
            assert!(
                s.copy(src, Rect { x: 2, w: 2, ..rect }, dst, rect, &mut m)
                    .is_err()
            );
        }
    }
}

#[test]
fn overlapping_copies_and_scaled_transfers_read_original_pixels() {
    for format in [
        PixelFormat::Bgra32,
        PixelFormat::Rgb565,
        PixelFormat::Rgb555,
    ] {
        for bottom_up in [false, true] {
            for (sr, dr) in [
                (
                    Rect {
                        x: 0,
                        y: 0,
                        w: 4,
                        h: 3,
                    },
                    Rect {
                        x: 1,
                        y: 1,
                        w: 4,
                        h: 3,
                    },
                ),
                (
                    Rect {
                        x: 1,
                        y: 1,
                        w: 4,
                        h: 3,
                    },
                    Rect {
                        x: 0,
                        y: 0,
                        w: 4,
                        h: 3,
                    },
                ),
                (
                    Rect {
                        x: 0,
                        y: 0,
                        w: 3,
                        h: 2,
                    },
                    Rect {
                        x: 1,
                        y: 1,
                        w: 4,
                        h: 3,
                    },
                ),
                (
                    Rect {
                        x: 1,
                        y: 0,
                        w: 4,
                        h: 4,
                    },
                    Rect {
                        x: 0,
                        y: 1,
                        w: 2,
                        h: 2,
                    },
                ),
            ] {
                let mut m = Memory::new(false);
                let mut s = store();
                let id = s.create(5, 4, bottom_up, format, &mut m).unwrap();
                pattern(&s, id, &mut m);
                let original = logical_pixels(&s, id, &m);
                let mut expected = original.clone();
                for y in 0..dr.h {
                    for x in 0..dr.w {
                        let sx = sr.x + x * sr.w / dr.w;
                        let sy = sr.y + y * sr.h / dr.h;
                        expected[((dr.y + y) * 5 + dr.x + x) as usize] =
                            original[(sy * 5 + sx) as usize].clone();
                    }
                }
                s.copy(id, sr, id, dr, &mut m).unwrap();
                assert_eq!(logical_pixels(&s, id, &m), expected);
                if format != PixelFormat::Bgra32 {
                    for y in 0..4 {
                        assert_eq!(
                            m.read(s[id].row(0, y) + 10, 2).unwrap().as_ref(),
                            &[0xcc, 0xcc]
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn clipped_scaling_keeps_the_original_sampling_phase_and_mirroring() {
    let mut m = Memory::new(false);
    let mut s = store();
    let src = s.create(5, 4, true, PixelFormat::Bgra32, &mut m).unwrap();
    let dst = s.create(3, 4, false, PixelFormat::Bgra32, &mut m).unwrap();
    pattern(&s, src, &mut m);
    let rect = Rect {
        x: 0,
        y: 0,
        w: 4,
        h: 3,
    };
    let dr = BlitRect {
        x: -3,
        y: -1,
        w: 6,
        h: 5,
    };
    for mirror in [false, true] {
        s.blit(
            src,
            rect,
            dst,
            dr,
            BlitOptions {
                mirror_x: mirror,
                mirror_y: mirror,
                ..Default::default()
            },
            &mut m,
        )
        .unwrap();
        let values: Vec<u8> = logical_pixels(&s, dst, &m).iter().map(|p| p[0]).collect();
        assert_eq!(
            values,
            if mirror {
                vec![12, 12, 11, 7, 7, 6, 7, 7, 6, 2, 2, 1]
            } else {
                vec![3, 3, 4, 8, 8, 9, 8, 8, 9, 13, 13, 14]
            }
        );
    }
}

#[test]
fn shrink_modes_combine_removed_pixels_before_format_conversion() {
    let mut m = Memory::new(false);
    let mut s = store();
    let src = s.create(2, 2, false, PixelFormat::Bgra32, &mut m).unwrap();
    let dst = s.create(1, 1, false, PixelFormat::Rgb565, &mut m).unwrap();
    m.write(
        s[src].pixels,
        &[
            0x0f, 0xff, 0xf0, 0, 0x33, 0xff, 0xcc, 0, 0x55, 0xff, 0xaa, 0, 0xff, 0xff, 1, 0,
        ],
    )
    .unwrap();
    for (mode, expected) in [(Sampling::And, [0xe0, 7]), (Sampling::Or, [255, 255])] {
        s.blit(
            src,
            s[src].rect(),
            dst,
            BlitRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            BlitOptions {
                sampling: mode,
                ..Default::default()
            },
            &mut m,
        )
        .unwrap();
        assert_eq!(m.read(s[dst].pixels, 2).unwrap().as_ref(), &expected);
    }
}

#[test]
// spec: DEV-FRAME-2, DEV-FRAME-12
fn immediate_deferred_and_mailbox_frames_preserve_bottom_up_16_bit_static_pixels() {
    for format in [
        PixelFormat::Bgra32,
        PixelFormat::Rgb565,
        PixelFormat::Rgb555,
    ] {
        for path in 0..3 {
            let mut m = Memory::new(false);
            let mut s = store();
            let id = s.create(3, 2, true, format, &mut m).unwrap();
            s.fill_raw(
                id,
                Rect {
                    x: 0,
                    y: 0,
                    w: 3,
                    h: 1,
                },
                if format == PixelFormat::Bgra32 {
                    0x00ff0000
                } else if format == PixelFormat::Rgb565 {
                    0xf800
                } else {
                    0x7c00
                },
                &mut m,
            )
            .unwrap();
            s.fill_raw(
                id,
                Rect {
                    x: 0,
                    y: 1,
                    w: 3,
                    h: 1,
                },
                0x1f,
                &mut m,
            )
            .unwrap();
            let (producer, mut consumer) = frame_mailbox();
            if path == 1 {
                s.set_deferred_presentation(true).unwrap();
            }
            if path == 2 {
                s.set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
            }
            s.present(id, &m, s[id].rect()).unwrap();
            s.flush(&m, adv_device::surface::FlushReason::Immediate)
                .unwrap();
            s.present(id, &m, s[id].rect()).unwrap();
            s.flush(&m, adv_device::surface::FlushReason::Immediate)
                .unwrap();
            let mut expected = [0, 0, 255, 0].repeat(3);
            expected.extend_from_slice(&if format == PixelFormat::Bgra32 {
                [31, 0, 0, 0].repeat(3)
            } else {
                [255, 0, 0, 0].repeat(3)
            });
            if path != 0 {
                assert_eq!(s.stats.converted_pixels, 0);
                assert_eq!(s.stats.coalesced_frames, 0);
                s.fill_raw(id, s[id].rect(), 0, &mut m).unwrap();
                s.delete(id);
                assert!(s.capture_latest(&m).unwrap());
                assert!(!s.capture_latest(&m).unwrap());
            }
            assert_eq!(s.presenter.bgra, expected);
            assert_eq!(s.presenter.frames, 1);
            if path == 2 {
                assert!(consumer.update());
                let mut snapshot = Snapshot::default();
                consumer.current().snapshot(&mut snapshot).unwrap();
                assert_eq!(snapshot.bgra, expected);
                assert_eq!(snapshot.frames, 1);
                assert!(!consumer.update());
            }
        }
    }
}

const INDEXED_COLORS: [[u8; 4]; 6] = [
    [5, 17, 230, 0],
    [180, 40, 5, 0],
    [12, 210, 70, 0],
    [223, 101, 180, 0],
    [51, 88, 13, 0],
    [3, 9, 244, 0],
];

fn indexed_pattern(s: &mut Surfaces<Snapshot>, m: &mut Memory, bottom_up: bool) -> u32 {
    let id = s.create(3, 2, bottom_up, PixelFormat::Indexed8, m).unwrap();
    s.set_palette(id, 1, &INDEXED_COLORS, m).unwrap();
    m.write(s[id].row(0, 0), &[1, 2, 3, 0xcc]).unwrap();
    m.write(s[id].row(0, 1), &[4, 5, 6, 0xcc]).unwrap();
    id
}

#[test]
fn indexed_non_gray_rows_convert_to_bgra_with_stride_and_orientation() {
    assert_eq!(PixelFormat::Indexed8.bytes(), 1);
    assert_eq!(PixelFormat::Indexed8.masks(), (0, 0, 0));
    for owned in [false, true] {
        for bottom_up in [false, true] {
            let mut m = Memory::new(owned);
            let mut s = store();
            let src = indexed_pattern(&mut s, &mut m, bottom_up);
            let dst = s
                .create(3, 2, !bottom_up, PixelFormat::Bgra32, &mut m)
                .unwrap();
            s.copy(src, s[src].rect(), dst, s[dst].rect(), &mut m)
                .unwrap();
            assert_eq!(
                logical_pixels(&s, dst, &m),
                INDEXED_COLORS.map(|c| c.to_vec())
            );
            assert_eq!(s[src].pitch, 4);
            for y in 0..2 {
                assert_eq!(m.read(s[src].row(3, y), 1).unwrap().as_ref(), &[0xcc]);
            }
            s.present(src, &m, s[src].rect()).unwrap();
            s.flush(&m, adv_device::surface::FlushReason::Immediate)
                .unwrap();
            assert_eq!(s.presenter.bgra, INDEXED_COLORS.concat());
        }
    }
}

#[test]
fn indexed_palette_bounds_default_and_nearest_rgb_are_explicit() {
    let mut m = Memory::new(false);
    let mut s = store();
    let id = s
        .create(3, 2, false, PixelFormat::Indexed8, &mut m)
        .unwrap();
    assert_eq!(s[id].decode_pixel(&[255]), [0; 4]);
    assert_eq!(s.palette_entries(id, 0, u32::MAX).unwrap(), &[[0; 4]; 256]);
    assert_eq!(s.set_palette(id, 254, &INDEXED_COLORS, &m).unwrap(), 2);
    assert_eq!(
        s.palette_entries(id, 254, 10).unwrap(),
        &INDEXED_COLORS[..2]
    );
    assert_eq!(
        s.palette_entries(id, 255, u32::MAX).unwrap(),
        &INDEXED_COLORS[1..2]
    );
    let version = s[id].version;
    assert_eq!(s.set_palette(id, u32::MAX, &INDEXED_COLORS, &m).unwrap(), 0);
    assert_eq!(s[id].version, version);
    assert!(s.palette_entries(id, 256, 1).unwrap().is_empty());
    s.set_palette(id, 12, &[[30, 60, 90, 0], [30, 60, 90, 255]], &m)
        .unwrap();
    // A nearest color may differ from every palette entry; alpha and ties
    // cannot change its RGB index. No implicit grayscale interpretation exists.
    assert_eq!(s[id].encode_pixel([31, 59, 90, 128]), [12, 0, 0, 0]);
    let direct = s.create(3, 2, false, PixelFormat::Bgra32, &mut m).unwrap();
    assert!(s.set_palette(direct, 0, &INDEXED_COLORS, &m).is_err());
    assert!(s.palette_entries(direct, 0, 1).is_none());
    assert!(s.set_palette(u32::MAX, 0, &INDEXED_COLORS, &m).is_err());
    assert!(s.palette_entries(u32::MAX, 0, 1).is_none());
}

#[test]
fn indexed_blits_remap_different_palettes_and_preserve_same_palette_indices() {
    let mut m = Memory::new(false);
    let mut s = store();
    let src = indexed_pattern(&mut s, &mut m, true);
    s.set_palette(src, 200, &INDEXED_COLORS[..1], &m).unwrap();
    m.write(s[src].row(0, 0), &[200, 2, 3]).unwrap();
    let different = s
        .create(3, 2, false, PixelFormat::Indexed8, &mut m)
        .unwrap();
    let mut reversed = INDEXED_COLORS;
    reversed.reverse();
    s.set_palette(different, 20, &reversed, &m).unwrap();
    for y in 0..2 {
        m.write(s[different].row(3, y), &[0xa7]).unwrap();
    }
    s.copy(src, s[src].rect(), different, s[different].rect(), &mut m)
        .unwrap();
    assert_eq!(
        m.read(s[different].row(0, 0), 4).unwrap().as_ref(),
        &[25, 24, 23, 0xa7]
    );
    assert_eq!(
        m.read(s[different].row(0, 1), 4).unwrap().as_ref(),
        &[22, 21, 20, 0xa7]
    );
    s.present(different, &m, s[different].rect()).unwrap();
    s.flush(&m, adv_device::surface::FlushReason::Immediate)
        .unwrap();
    assert_eq!(s.presenter.bgra, INDEXED_COLORS.concat());
    let same = s
        .create(3, 2, false, PixelFormat::Indexed8, &mut m)
        .unwrap();
    let table = s[src].palette.clone().unwrap();
    s.set_palette(same, 0, table.as_ref(), &m).unwrap();
    s.copy(src, s[src].rect(), same, s[same].rect(), &mut m)
        .unwrap();
    // Duplicate colors still retain their actual raw index for identical tables.
    assert_eq!(m.read(s[same].row(0, 0), 3).unwrap().as_ref(), &[200, 2, 3]);
    let direct = s.create(3, 2, false, PixelFormat::Bgra32, &mut m).unwrap();
    s.copy(src, s[src].rect(), direct, s[direct].rect(), &mut m)
        .unwrap();
    s.fill_raw(same, s[same].rect(), 0, &mut m).unwrap();
    s.copy(direct, s[direct].rect(), same, s[same].rect(), &mut m)
        .unwrap();
    assert_eq!(m.read(s[same].row(0, 0), 3).unwrap().as_ref(), &[1, 2, 3]);
}

#[test]
fn completed_indexed_frames_keep_their_palette_after_live_update_and_delete() {
    for path in 0..3 {
        let mut m = Memory::new(false);
        let mut s = store();
        let id = indexed_pattern(&mut s, &mut m, true);
        let old_table = s[id].palette.clone().unwrap();
        let (producer, mut consumer) = frame_mailbox();
        if path == 1 {
            s.set_deferred_presentation(true).unwrap();
        }
        if path == 2 {
            s.set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
        }
        s.present(id, &m, s[id].rect()).unwrap();
        s.flush(&m, adv_device::surface::FlushReason::Immediate)
            .unwrap();
        s.set_palette(id, 1, &[[99, 88, 77, 0]; 6], &m).unwrap();
        assert!(!std::sync::Arc::ptr_eq(
            &old_table,
            s[id].palette.as_ref().unwrap()
        ));
        assert_eq!(old_table[1], INDEXED_COLORS[0]);
        s.fill_raw(id, s[id].rect(), 0, &mut m).unwrap();
        s.delete(id);
        if path != 0 {
            assert!(s.capture_latest(&m).unwrap());
            assert!(!s.capture_latest(&m).unwrap());
        }
        assert_eq!(s.presenter.bgra, INDEXED_COLORS.concat());
        if path == 2 {
            assert!(consumer.update());
            let mut snapshot = Snapshot::default();
            consumer.current().snapshot(&mut snapshot).unwrap();
            assert_eq!(snapshot.bgra, INDEXED_COLORS.concat());
            assert!(!consumer.update());
        }
    }
}

#[test]
fn indexed_primary_resize_keeps_the_palette_and_overlapping_indices() {
    let mut m = Memory::new(false);
    let mut s = store();
    s.display_format = PixelFormat::Indexed8;
    let primary = s.primary(&mut m).unwrap();
    s.set_palette(primary, 1, &INDEXED_COLORS, &m).unwrap();
    s.fill_raw(primary, s[primary].rect(), 1, &mut m).unwrap();
    s.configure_window(3, 2, &mut m).unwrap();
    assert_eq!(s.palette_entries(primary, 1, 6).unwrap(), &INDEXED_COLORS);
    assert!(logical_pixels(&s, primary, &m).iter().all(|p| p == &[1]));
    s.present(primary, &m, s[primary].rect()).unwrap();
    s.flush(&m, adv_device::surface::FlushReason::Immediate)
        .unwrap();
    assert_eq!(s.presenter.bgra, INDEXED_COLORS[0].repeat(6));
}

#[test]
fn packed_fill_preserves_edge_bits_padding_and_rejects_late_row_faults_atomically() {
    for (format, expected) in [
        (PixelFormat::Indexed1, vec![0xbf, 0xa5, 0xa5, 0xa5]),
        (
            PixelFormat::Indexed4,
            vec![0xa5, 0xff, 0xff, 0xf5, 0xa5, 0xa5, 0xa5, 0xa5],
        ),
    ] {
        let mut memory = Memory::new(false);
        let mut surfaces = store();
        let id = surfaces.create(9, 2, true, format, &mut memory).unwrap();
        let surface = surfaces[id].clone();
        assert_eq!(surface.pitch, expected.len() as u32);
        memory
            .write(surface.pixels, &vec![0xa5; surface.pitch as usize * 2])
            .unwrap();
        surfaces
            .fill_raw(
                id,
                Rect {
                    x: 2,
                    y: 0,
                    w: 5,
                    h: 1,
                },
                255,
                &mut memory,
            )
            .unwrap();
        assert_eq!(
            memory
                .read(surface.row(0, 0), surface.pitch as usize)
                .unwrap()
                .as_ref(),
            expected
        );
        assert_eq!(
            memory
                .read(surface.row(0, 1), surface.pitch as usize)
                .unwrap()
                .as_ref(),
            vec![0xa5; surface.pitch as usize]
        );
        let before = memory.bytes.clone();
        memory.denied = Some((surface.row(2, 1), 1));
        assert!(
            surfaces
                .fill_raw(
                    id,
                    Rect {
                        x: 2,
                        y: 0,
                        w: 5,
                        h: 2
                    },
                    0,
                    &mut memory
                )
                .is_err()
        );
        assert_eq!(memory.bytes, before);
    }
}

#[test]
fn packed_blit_snapshots_overlaps_and_converts_mirrored_palette_indices() {
    for owned in [false, true] {
        let mut memory = Memory::new(owned);
        let mut surfaces = store();
        let mono = surfaces
            .create(9, 1, false, PixelFormat::Indexed1, &mut memory)
            .unwrap();
        memory
            .write(surfaces[mono].pixels, &[0xaa, 0x80, 0xcc, 0xdd])
            .unwrap();
        surfaces
            .copy(
                mono,
                Rect {
                    x: 0,
                    y: 0,
                    w: 8,
                    h: 1,
                },
                mono,
                Rect {
                    x: 1,
                    y: 0,
                    w: 8,
                    h: 1,
                },
                &mut memory,
            )
            .unwrap();
        assert_eq!(
            memory.read(surfaces[mono].pixels, 4).unwrap().as_ref(),
            [0xd5, 0, 0xcc, 0xdd]
        );
        let indexed = surfaces
            .create(9, 1, false, PixelFormat::Indexed4, &mut memory)
            .unwrap();
        let colors: Vec<_> = (0..16).map(|i| [i, 0, 0, 0]).collect();
        surfaces.set_palette(indexed, 0, &colors, &memory).unwrap();
        memory
            .write(
                surfaces[indexed].pixels,
                &[0x01, 0x23, 0x45, 0x67, 0x80, 0xcc, 0xdd, 0xee],
            )
            .unwrap();
        let rgb = surfaces
            .create(9, 1, false, PixelFormat::Bgra32, &mut memory)
            .unwrap();
        surfaces
            .blit(
                indexed,
                Rect {
                    x: 0,
                    y: 0,
                    w: 9,
                    h: 1,
                },
                rgb,
                BlitRect {
                    x: 0,
                    y: 0,
                    w: 9,
                    h: 1,
                },
                BlitOptions {
                    mirror_x: true,
                    ..BlitOptions::default()
                },
                &mut memory,
            )
            .unwrap();
        let row = memory.read(surfaces[rgb].pixels, 36).unwrap();
        assert_eq!(
            row.as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[0])
                .collect::<Vec<_>>(),
            [8, 7, 6, 5, 4, 3, 2, 1, 0]
        );
        surfaces
            .copy(
                rgb,
                Rect {
                    x: 0,
                    y: 0,
                    w: 9,
                    h: 1,
                },
                indexed,
                Rect {
                    x: 0,
                    y: 0,
                    w: 9,
                    h: 1,
                },
                &mut memory,
            )
            .unwrap();
        assert_eq!(
            memory.read(surfaces[indexed].pixels, 8).unwrap().as_ref(),
            [0x87, 0x65, 0x43, 0x21, 0, 0xcc, 0xdd, 0xee]
        );
        surfaces
            .present(indexed, &memory, surfaces[indexed].rect())
            .unwrap();
        surfaces
            .flush(&memory, adv_device::surface::FlushReason::Immediate)
            .unwrap();
        assert_eq!(
            surfaces
                .presenter
                .bgra
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[0])
                .collect::<Vec<_>>(),
            [8, 7, 6, 5, 4, 3, 2, 1, 0]
        );
    }
}

// spec: DEV-FRAME-3
#[test]
fn repeated_strips_do_not_publish_or_wake_and_one_row_is_precise() {
    for owned in [false, true] {
        let mut s = store();
        let mut m = Memory::new(owned);
        let src = s.create(5, 4, false, PixelFormat::Bgra32, &mut m).unwrap();
        let dst = s.primary(&mut m).unwrap();
        let (tx, mut rx) = frame_mailbox();
        let wake = adv_sync::WakeEvent::new().unwrap();
        s.set_frame_mailbox(tx, wake.clone());
        s.present(dst, &m, s[dst].rect()).unwrap();
        s.flush(&m, adv_device::surface::FlushReason::Immediate)
            .unwrap();
        assert!(rx.update());
        assert!(wake.wait(Some(std::time::Duration::ZERO)).unwrap());
        let mut out = Snapshot::default();
        rx.current().snapshot(&mut out).unwrap();
        let serial = s.stats.presentations;
        let bytes = s.stats.copied_bytes;
        let blitted = s.stats.bytes_blitted;
        let damage = s
            .copy(src, s[src].rect(), dst, s[dst].rect(), &mut m)
            .unwrap();
        assert!(damage.empty());
        s.present(dst, &m, damage).unwrap();
        s.flush(&m, adv_device::surface::FlushReason::Immediate)
            .unwrap();
        assert!(!rx.update());
        assert!(!wake.wait(Some(std::time::Duration::ZERO)).unwrap());
        assert_eq!(s.stats.presentations, serial);
        assert_eq!(s.stats.copied_bytes, bytes);
        assert_eq!(s.stats.bytes_blitted, blitted);
        assert_eq!(s.stats.empty_damage, 1);
        m.write(s[src].row(0, 2), &[7; 20]).unwrap();
        let damage = s
            .copy(src, s[src].rect(), dst, s[dst].rect(), &mut m)
            .unwrap();
        assert_eq!(
            damage,
            Rect {
                x: 0,
                y: 2,
                w: 5,
                h: 1
            }
        );
        assert_eq!(s.stats.bytes_blitted - blitted, 20);
        s.present(dst, &m, damage).unwrap();
        s.flush(&m, adv_device::surface::FlushReason::Immediate)
            .unwrap();
        assert!(rx.update());
        assert_eq!(rx.current().damage(), &[damage]);
        rx.current().snapshot(&mut out).unwrap();
        assert_eq!(&*out.damage, &[damage]);
        assert_eq!(&out.bgra[40..60], &[7; 20]);
        assert_eq!(&out.bgra[..40], &[0; 40]);
        assert_eq!(&out.bgra[60..], &[0; 20]);
    }
}

// spec: DEV-FRAME-4, DEV-FRAME-5, DEV-FRAME-7
#[test]
fn three_slots_catch_up_and_skipped_frames_merge_with_a_bounded_history() {
    let mut s = store();
    let mut m = Memory::new(true);
    let id = s.primary(&mut m).unwrap();
    let (tx, mut rx) = frame_mailbox();
    s.set_frame_mailbox(tx, adv_sync::WakeEvent::new().unwrap());
    let mut out = Snapshot::default();
    let mut expected = vec![0; 80];
    for n in 0..12 {
        let y = n % 4;
        let x = n % 5;
        let r = Rect { x, y, w: 1, h: 1 };
        let value = [n as u8 + 1; 4];
        m.write(s[id].row(x, y), &value).unwrap();
        expected[((y * 5 + x) * 4) as usize..((y * 5 + x) * 4 + 4) as usize]
            .copy_from_slice(&value);
        let before = s.stats.copied_bytes;
        s.present(id, &m, r).unwrap();
        s.flush(&m, adv_device::surface::FlushReason::Immediate)
            .unwrap();
        assert!(rx.update());
        rx.current().snapshot(&mut out).unwrap();
        assert_eq!(out.bgra, expected);
        if n >= 3 {
            assert!(s.stats.copied_bytes - before < 80);
        }
        // A fresh capture verifies the full slot, not only the consumer's patch.
        let mut full = Snapshot::default();
        rx.current().snapshot(&mut full).unwrap();
        assert_eq!(full.bgra, expected);
    }
    let mut lagging = Snapshot {
        width: out.width,
        height: out.height,
        frames: out.frames,
        bgra: out.bgra.clone(),
        ..Default::default()
    };
    for n in 0..3 {
        let r = Rect {
            x: n * 2,
            y: 0,
            w: 1,
            h: 1,
        };
        let value = [40 + n as u8; 4];
        m.write(s[id].row(r.x, r.y), &value).unwrap();
        expected[(r.x * 4) as usize..(r.x * 4 + 4) as usize].copy_from_slice(&value);
        s.present(id, &m, r).unwrap();
        s.flush(&m, adv_device::surface::FlushReason::Immediate)
            .unwrap();
    }
    assert!(rx.update());
    rx.current().snapshot(&mut out).unwrap();
    assert_eq!(out.damage.len(), 3);
    assert_eq!(out.bgra, expected);
    // Keep one slot and two consumer baselines idle beyond the history window.
    for n in 0..20 {
        let r = Rect {
            x: 0,
            y: 3,
            w: 1,
            h: 1,
        };
        let value = [80 + n; 4];
        m.write(s[id].row(0, 3), &value).unwrap();
        expected[60..64].copy_from_slice(&value);
        s.present(id, &m, r).unwrap();
        s.flush(&m, adv_device::surface::FlushReason::Immediate)
            .unwrap();
    }
    assert!(rx.update());
    rx.current().snapshot(&mut out).unwrap();
    assert_eq!(&*out.damage, &[s[id].rect()]);
    rx.current().snapshot(&mut lagging).unwrap();
    assert_eq!(lagging.bgra, expected);
    assert_eq!(out.bgra, expected);
    // The previously parked reader slot now re-enters writer rotation.
    let mut full_copies = 0;
    for n in 0..2 {
        let r = Rect {
            x: 4,
            y: 3,
            w: 1,
            h: 1,
        };
        m.write(s[id].row(4, 3), &[120 + n; 4]).unwrap();
        expected[76..80].fill(120 + n);
        let before = s.stats.copied_bytes;
        s.present(id, &m, r).unwrap();
        s.flush(&m, adv_device::surface::FlushReason::Immediate)
            .unwrap();
        full_copies += usize::from(s.stats.copied_bytes - before == 80);
        assert!(rx.update());
        let mut full = Snapshot::default();
        rx.current().snapshot(&mut full).unwrap();
        assert_eq!(full.bgra, expected);
    }
    assert!(full_copies >= 1);
}

// spec: DEV-FRAME-4, DEV-FRAME-5, DEV-FRAME-6
#[test]
fn bottom_up_palette_changes_and_packed_edges_match_full_conversion() {
    for format in [
        PixelFormat::Indexed8,
        PixelFormat::Indexed4,
        PixelFormat::Indexed1,
    ] {
        let mut s = store();
        let mut m = Memory::new(false);
        let id = s.create(5, 4, true, format, &mut m).unwrap();
        s.set_palette(id, 1, &[[7, 8, 9, 0]], &m).unwrap();
        let (tx, mut rx) = frame_mailbox();
        s.set_frame_mailbox(tx, adv_sync::WakeEvent::new().unwrap());
        s.present(id, &m, s[id].rect()).unwrap();
        s.flush(&m, adv_device::surface::FlushReason::Immediate)
            .unwrap();
        assert!(rx.update());
        let mut out = Snapshot::default();
        rx.current().snapshot(&mut out).unwrap();
        let r = Rect {
            x: 3,
            y: 1,
            w: 1,
            h: 1,
        };
        s.fill_raw(id, r, 1, &mut m).unwrap();
        s.present(id, &m, r).unwrap();
        s.flush(&m, adv_device::surface::FlushReason::Immediate)
            .unwrap();
        assert!(rx.update());
        rx.current().snapshot(&mut out).unwrap();
        assert_eq!(&*out.damage, &[r]);
        assert_eq!(&out.bgra[32..36], &[7, 8, 9, 0]);
        let mut full = Snapshot::default();
        rx.current().snapshot(&mut full).unwrap();
        assert_eq!(out.bgra, full.bgra);
        s.set_palette(id, 1, &[[30, 31, 32, 0]], &m).unwrap();
        s.present(id, &m, s[id].rect()).unwrap();
        s.flush(&m, adv_device::surface::FlushReason::Immediate)
            .unwrap();
        assert!(rx.update());
        rx.current().snapshot(&mut out).unwrap();
        assert_eq!(&*out.damage, &[s[id].rect()]);
        assert_eq!(&out.bgra[32..36], &[30, 31, 32, 0]);
    }
}

// spec: DEV-FRAME-4, DEV-FRAME-6
#[test]
fn surface_switches_and_resize_round_trips_invalidate_damage_history() {
    let mut s = store();
    let mut m = Memory::new(false);
    let a = s.create(5, 4, false, PixelFormat::Bgra32, &mut m).unwrap();
    let b = s.create(6, 4, true, PixelFormat::Bgr24, &mut m).unwrap();
    let (tx, mut rx) = frame_mailbox();
    s.set_frame_mailbox(tx, adv_sync::WakeEvent::new().unwrap());
    s.present(a, &m, s[a].rect()).unwrap();
    s.flush(&m, adv_device::surface::FlushReason::Immediate)
        .unwrap();
    assert!(rx.update());
    let mut out = Snapshot::default();
    rx.current().snapshot(&mut out).unwrap();
    s.fill_raw(b, s[b].rect(), 0xabcdef, &mut m).unwrap();
    s.present(b, &m, s[b].rect()).unwrap();
    s.flush(&m, adv_device::surface::FlushReason::Immediate)
        .unwrap();
    s.fill_raw(a, s[a].rect(), 0x12345678, &mut m).unwrap();
    s.present(
        a,
        &m,
        Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        },
    )
    .unwrap();
    s.flush(&m, adv_device::surface::FlushReason::Immediate)
        .unwrap();
    assert!(rx.update());
    rx.current().snapshot(&mut out).unwrap();
    assert_eq!(&*out.damage, &[s[a].rect()]);
    assert_eq!(out.bgra, [0x78, 0x56, 0x34, 0x12].repeat(20));
}

// spec: DEV-FRAME-5
#[test]
fn more_than_four_disjoint_changes_merge_into_a_bounding_box() {
    let mut s = store();
    let mut m = Memory::new(false);
    let id = s.primary(&mut m).unwrap();
    let (tx, mut rx) = frame_mailbox();
    s.set_frame_mailbox(tx, adv_sync::WakeEvent::new().unwrap());
    s.present(id, &m, s[id].rect()).unwrap();
    s.flush(&m, adv_device::surface::FlushReason::Immediate)
        .unwrap();
    assert!(rx.update());
    let mut out = Snapshot::default();
    rx.current().snapshot(&mut out).unwrap();
    for (x, y) in [(0, 0), (2, 0), (4, 0), (0, 2), (2, 2)] {
        let r = Rect { x, y, w: 1, h: 1 };
        m.write(s[id].row(x, y), &[7; 4]).unwrap();
        s.present(id, &m, r).unwrap();
        s.flush(&m, adv_device::surface::FlushReason::Immediate)
            .unwrap();
    }
    assert!(rx.update());
    rx.current().snapshot(&mut out).unwrap();
    assert_eq!(
        &*out.damage,
        &[Rect {
            x: 0,
            y: 0,
            w: 5,
            h: 3
        }]
    );
    let mut full = Snapshot::default();
    rx.current().snapshot(&mut full).unwrap();
    assert_eq!(out.bgra, full.bgra);
}

// spec: DEV-FRAME-8
#[test]
fn strip_frame_is_published_once_with_all_damage_at_the_writer_boundary() {
    let mut s = store();
    let mut m = Memory::new(false);
    let id = s.primary(&mut m).unwrap();
    let (tx, mut rx) = frame_mailbox();
    let wake = adv_sync::WakeEvent::new().unwrap();
    s.set_frame_mailbox(tx, wake.clone());
    s.present_from(7, id, &m, s[id].rect()).unwrap();
    s.flush_context(7, &m, FlushReason::Wait).unwrap();
    assert!(rx.update());
    assert!(wake.wait(Some(std::time::Duration::ZERO)).unwrap());
    let mut out = Snapshot::default();
    rx.current().snapshot(&mut out).unwrap();
    let copied = s.stats.copied_bytes;
    for y in 0..3 {
        let strip = Rect {
            x: 0,
            y,
            w: 5,
            h: 1,
        };
        s.fill_raw(id, strip, 0x01020304 + y, &mut m).unwrap();
        s.present_from(7, id, &m, strip).unwrap();
        assert!(!rx.update());
        assert!(!wake.wait(Some(std::time::Duration::ZERO)).unwrap());
    }
    assert_eq!(s.stats.presentations, 1);
    assert_eq!(s.stats.copied_bytes, copied);
    assert!(!s.flush_context(8, &m, FlushReason::Wait).unwrap());
    assert!(!s.flush_context(8, &m, FlushReason::Message).unwrap());
    assert!(s.flush_context(7, &m, FlushReason::Message).unwrap());
    assert!(rx.update());
    assert_eq!(s.stats.presentations, 2);
    let damage = Rect {
        x: 0,
        y: 0,
        w: 5,
        h: 3,
    };
    assert_eq!(rx.current().damage(), &[damage]);
    rx.current().snapshot(&mut out).unwrap();
    assert_eq!(&*out.damage, &[damage]);
    for y in 0..3usize {
        assert_eq!(
            &out.bgra[y * 20..y * 20 + 20],
            &(0x01020304u32 + y as u32).to_le_bytes().repeat(5)
        );
    }
    assert_eq!(&out.bgra[60..], &[0; 20]);
    assert_eq!(s.stats.software_flushes, [1, 1, 0, 0]);
}

// spec: DEV-FRAME-8
#[test]
fn overdue_frame_flushes_on_present_or_another_context_wait() {
    let mut s = store();
    let mut m = Memory::new(false);
    let id = s.primary(&mut m).unwrap();
    let (tx, mut rx) = frame_mailbox();
    s.set_frame_mailbox(tx, adv_sync::WakeEvent::new().unwrap());
    for via_present in [false, true] {
        s.fill_raw(id, s[id].rect(), u32::from(via_present), &mut m)
            .unwrap();
        s.present_from(7, id, &m, s[id].rect()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(55));
        if via_present {
            // Unchanged strips must still enforce the pending frame's age.
            s.present_from(8, id, &m, Rect::default()).unwrap();
        } else {
            assert!(s.flush_context(8, &m, FlushReason::Wait).unwrap());
        }
        assert!(rx.update());
    }
    assert_eq!(s.stats.software_flushes, [0, 0, 2, 0]);
    assert_eq!(s.stats.presentations, 2);
}

// spec: DEV-FRAME-8
#[test]
fn capture_resize_palette_and_video_flush_pending_pixels_before_the_change() {
    let mut s = store();
    let mut m = Memory::new(false);
    s.display_format = PixelFormat::Indexed8;
    let id = s.primary(&mut m).unwrap();
    s.set_palette(id, 1, &[[7, 8, 9, 0]], &m).unwrap();
    s.fill_raw(id, s[id].rect(), 1, &mut m).unwrap();
    let (tx, mut rx) = frame_mailbox();
    s.set_frame_mailbox(tx, adv_sync::WakeEvent::new().unwrap());
    s.present_from(7, id, &m, s[id].rect()).unwrap();
    s.set_palette(id, 1, &[[30, 31, 32, 0]], &m).unwrap();
    assert!(rx.update());
    let mut out = Snapshot::default();
    rx.current().snapshot(&mut out).unwrap();
    assert_eq!(out.bgra, [7, 8, 9, 0].repeat(20));
    s.present_from(7, id, &m, s[id].rect()).unwrap();
    s.configure_window(6, 3, &mut m).unwrap();
    assert!(rx.update());
    rx.current().snapshot(&mut out).unwrap();
    assert_eq!((out.width, out.height), (5, 4));
    assert_eq!(out.bgra, [30, 31, 32, 0].repeat(20));
    s.present_from(7, id, &m, s[id].rect()).unwrap();
    assert!(s.capture_latest(&m).unwrap());
    assert_eq!((s.presenter.width, s.presenter.height), (6, 3));
    assert!(rx.update());
    s.fill_raw(id, s[id].rect(), 0, &mut m).unwrap();
    s.present_from(7, id, &m, s[id].rect()).unwrap();
    s.set_video_layer(1, Some(video_layer()), &m).unwrap();
    assert!(rx.update());
    rx.current().snapshot(&mut out).unwrap();
    assert_eq!(&out.bgra[48..72], &[0; 24]);
    assert_eq!(s.stats.software_flushes, [0, 0, 0, 4]);
}

#[test]
// spec: DEV-FRAME-10, DEV-FRAME-12
fn declared_damage_shrinks_to_changed_tiles_against_the_last_publication() {
    for (format, bottom_up) in [
        (PixelFormat::Bgra32, false),
        (PixelFormat::Bgra32, true),
        (PixelFormat::Rgb565, false),
        (PixelFormat::Bgr24, true),
    ] {
        let mut m = Memory::new(false);
        let mut s = Surfaces::new(128, 64, 16, 65520, Snapshot::default()).unwrap();
        let id = s.create(128, 64, bottom_up, format, &mut m).unwrap();
        let (producer, mut consumer) = frame_mailbox();
        s.set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
        let mut publish = |s: &mut Surfaces<Snapshot>, m: &Memory| {
            s.present(id, m, s[id].rect()).unwrap();
            s.flush(m, FlushReason::Immediate).unwrap();
            if !consumer.update() {
                return None;
            }
            let frame = consumer.current();
            let mut shown = Snapshot::default();
            frame.snapshot(&mut shown).unwrap();
            if format == PixelFormat::Bgra32 {
                assert_eq!(shown.bgra, logical_pixels(s, id, m).concat());
            }
            Some(frame.damage().to_vec())
        };
        let full = Rect {
            x: 0,
            y: 0,
            w: 128,
            h: 64,
        };
        assert_eq!(publish(&mut s, &m), Some(vec![full]));
        let one = Rect {
            x: 70,
            y: 20,
            w: 1,
            h: 1,
        };
        s.fill_raw(id, one, 0x0012_3456, &mut m).unwrap();
        assert_eq!(
            publish(&mut s, &m),
            Some(vec![Rect {
                x: 64,
                y: 16,
                w: 64,
                h: 16
            }])
        );
        assert_eq!(publish(&mut s, &m), None);
        s.fill_raw(id, full, 0x0000_ff00, &mut m).unwrap();
        assert_eq!(publish(&mut s, &m), Some(vec![full]));
    }
}

#[test]
// spec: GFX-DDRAW-4
fn source_color_keys_skip_keyed_pixels_when_copying_and_scaling() {
    let mut m = Memory::new(false);
    let mut s = store();
    let src = s.create(2, 2, false, PixelFormat::Rgb565, &mut m).unwrap();
    // Key, red / green, key; 0x0001 lies inside the inclusive key range.
    m.write(s[src].pixels, &[0, 0, 0, 0xf8]).unwrap();
    m.write(s[src].row(0, 1), &[0xe0, 0x07, 1, 0]).unwrap();
    let key = Some((0, 1));
    let same = s.create(2, 2, false, PixelFormat::Rgb565, &mut m).unwrap();
    s.fill_raw(same, s[same].rect(), 0xffff, &mut m).unwrap();
    let whole = BlitRect {
        x: 0,
        y: 0,
        w: 2,
        h: 2,
    };
    let options = BlitOptions {
        key,
        ..Default::default()
    };
    s.blit(src, s[src].rect(), same, whole, options, &mut m)
        .unwrap();
    assert_eq!(
        m.read(s[same].pixels, 4).unwrap().as_ref(),
        &[0xff, 0xff, 0, 0xf8]
    );
    assert_eq!(
        m.read(s[same].row(0, 1), 4).unwrap().as_ref(),
        &[0xe0, 0x07, 0xff, 0xff]
    );
    let wide = s.create(4, 2, false, PixelFormat::Bgra32, &mut m).unwrap();
    let scaled = BlitRect {
        x: 0,
        y: 0,
        w: 4,
        h: 2,
    };
    s.blit(src, s[src].rect(), wide, scaled, options, &mut m)
        .unwrap();
    let first: Vec<u8> = logical_pixels(&s, wide, &m).iter().map(|p| p[2]).collect();
    assert_eq!(first, vec![0, 0, 0xff, 0xff, 0, 0, 0, 0]);
    // Overlapping keyed copies read the source as it was before any write,
    // and a later call reuses the scratch rows without stale pixels.
    let strip = s.create(2, 4, false, PixelFormat::Rgb565, &mut m).unwrap();
    for _ in 0..2 {
        for (y, value) in [0x11u8, 0x22, 0x33, 0x44].into_iter().enumerate() {
            m.write(s[strip].row(0, y as u32), &[0, 0, value, value])
                .unwrap();
        }
        let source = Rect {
            x: 0,
            y: 0,
            w: 2,
            h: 3,
        };
        let shifted = BlitRect {
            x: 0,
            y: 1,
            w: 2,
            h: 3,
        };
        s.blit(strip, source, strip, shifted, options, &mut m)
            .unwrap();
        let rows: Vec<_> = (0..4)
            .map(|y| m.read(s[strip].row(0, y), 4).unwrap().into_owned())
            .collect();
        assert_eq!(rows, [0x11, 0x11, 0x22, 0x33].map(|v: u8| vec![0, 0, v, v]));
    }
}

#[test]
// spec: DEV-IMAGE-15
fn sprites_apply_documented_mask_xor_and_alpha_to_authored_pixels() {
    for owned in [false, true] {
        let mut memory = Memory::new(owned);
        let mut store = store();
        let id = store
            .create(4, 1, false, PixelFormat::Bgra32, &mut memory)
            .unwrap();
        let at = store[id].pixels;
        let original = [10, 20, 30, 40].repeat(4);
        memory.write(at, &original).unwrap();
        let rgba = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let mask = [0, 1, 0, 1];
        let source = Sprite {
            width: 4,
            height: 1,
            rgba: &rgba,
            mask: Some(&mask),
            mode: SpriteMode::Color {
                alpha: SpriteAlpha::Ignore,
                masked: true,
            },
            background: None,
            preserve_alpha: false,
        };
        let rect = Rect {
            x: 0,
            y: 0,
            w: 4,
            h: 1,
        };
        let destination = BlitRect {
            x: 0,
            y: 0,
            w: 4,
            h: 1,
        };
        assert_eq!(
            store
                .sprite(id, source, rect, destination, &mut memory)
                .unwrap(),
            rect
        );
        assert_eq!(
            memory.read(at, 16).unwrap().as_ref(),
            [
                0, 0, 255, 0, 10, 235, 30, 40, 255, 0, 0, 0, 245, 235, 225, 40
            ]
        );
        for (alpha, red) in [
            (SpriteAlpha::Straight, 255),
            (SpriteAlpha::Premultiplied, 128),
        ] {
            memory.write(at, &original).unwrap();
            let rgba = [red, 0, 0, 128, 0, 0, 0, 0, 0, 0, 255, 255, 0, 255, 0, 255];
            let source = Sprite {
                rgba: &rgba,
                mode: SpriteMode::Color {
                    alpha,
                    masked: true,
                },
                ..source
            };
            store
                .sprite(id, source, rect, destination, &mut memory)
                .unwrap();
            assert_eq!(
                memory.read(at, 16).unwrap().as_ref(),
                [
                    5, 10, 143, 148, 10, 20, 30, 40, 255, 0, 0, 255, 0, 255, 0, 255
                ]
            );
        }
        memory.write(at, &original).unwrap();
        store
            .sprite(
                id,
                Sprite {
                    preserve_alpha: true,
                    ..source
                },
                rect,
                destination,
                &mut memory,
            )
            .unwrap();
        assert_eq!(
            memory.read(at, 16).unwrap().as_ref(),
            [
                0, 0, 255, 40, 10, 235, 30, 40, 255, 0, 0, 40, 245, 235, 225, 40
            ]
        );
    }
}

#[test]
// spec: DEV-IMAGE-15
fn sprites_preserve_packed_neighbors_scaling_phase_and_failed_write_transactions() {
    let mut memory = Memory::new(false);
    let mut store = store();
    let id = store
        .create(4, 2, true, PixelFormat::Indexed4, &mut memory)
        .unwrap();
    let mut colors = [[0; 4]; 256];
    for (i, color) in colors[..16].iter_mut().enumerate() {
        *color = [(i * 17) as u8, (i * 17) as u8, (i * 17) as u8, 0];
    }
    store.get_mut(id).unwrap().palette = Some(Arc::new(colors));
    let at = store[id].pixels;
    memory
        .write(at, &[0x12, 0x34, 0xcc, 0xdd, 0x56, 0x78, 0xaa, 0xbb])
        .unwrap();
    let source = Sprite {
        width: 2,
        height: 1,
        rgba: &[255; 8],
        mask: Some(&[1, 1]),
        mode: SpriteMode::Color {
            alpha: SpriteAlpha::Ignore,
            masked: true,
        },
        background: None,
        preserve_alpha: false,
    };
    let rect = Rect {
        x: 0,
        y: 0,
        w: 2,
        h: 1,
    };
    let destination = BlitRect {
        x: 1,
        y: 0,
        w: 2,
        h: 1,
    };
    store
        .sprite(id, source, rect, destination, &mut memory)
        .unwrap();
    assert_eq!(
        memory.read(at, 8).unwrap().as_ref(),
        [0x12, 0x34, 0xcc, 0xdd, 0x59, 0x88, 0xaa, 0xbb]
    );
    let before = memory.bytes.clone();
    let version = store[id].version;
    memory.denied = Some((at, 1));
    assert!(
        store
            .sprite(
                id,
                source,
                rect,
                BlitRect {
                    h: 2,
                    ..destination
                },
                &mut memory
            )
            .is_err()
    );
    assert_eq!(memory.bytes, before);
    assert_eq!(store[id].version, version);
    memory.denied = None;
    let rgba = [0, 0, 0, 255, 255, 255, 255, 255];
    store
        .sprite(
            id,
            Sprite {
                rgba: &rgba,
                mode: SpriteMode::Image {
                    alpha: SpriteAlpha::Ignore,
                },
                ..source
            },
            rect,
            BlitRect {
                x: -1,
                y: 1,
                w: 4,
                h: 1,
            },
            &mut memory,
        )
        .unwrap();
    assert_eq!(
        memory.read(at, 4).unwrap().as_ref(),
        [0x0f, 0xf4, 0xcc, 0xdd]
    );
}

// spec: DEV-FRAME-12, MEDIA-VIDEO-4
#[test]
fn unchanged_base_with_a_retained_video_layer_does_not_republish() {
    let mut store = store();
    let mut memory = Memory::new(false);
    let id = store.primary(&mut memory).unwrap();
    let (producer, mut consumer) = frame_mailbox();
    store.set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
    store
        .set_video_layer(1, Some(video_layer()), &memory)
        .unwrap();
    store.present(id, &memory, store[id].rect()).unwrap();
    assert!(store.flush(&memory, FlushReason::Immediate).unwrap());
    assert!(consumer.update());
    let before = (store.stats.published_frames, store.stats.copied_bytes);
    for damage in [
        store[id].rect(),
        Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        },
    ] {
        store.present(id, &memory, damage).unwrap();
        assert!(!store.flush(&memory, FlushReason::Immediate).unwrap());
        assert!(!consumer.update());
        assert_eq!(
            (store.stats.published_frames, store.stats.copied_bytes),
            before
        );
    }
}
