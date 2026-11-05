//! Three guest-format buffers, each with its own accumulated damage debt.
use super::*;
use crate::frame::{Base, Slot};
use crate::gpu::native::stream::{Command, Producer};
use std::sync::atomic::{AtomicU64, Ordering};

struct Entry {
    buffer: adv_gpu::Buffer,
    state: Arc<AtomicU64>,
    debt: Region,
}
pub(crate) struct Slots {
    entries: [Entry; 3],
    next: usize,
    last: Option<usize>,
    layout: (u32, u32, u32, PixelFormat, u32, bool),
    palette: Option<crate::frame::Palette>,
}
impl Slots {
    fn layout(s: &Surface) -> (u32, u32, u32, PixelFormat, u32, bool) {
        (s.width, s.height, s.pitch, s.format, s.pixels, s.bottom_up)
    }
    pub fn matches(&self, s: &Surface) -> bool {
        self.layout == Self::layout(s)
    }
    pub fn new(device: &adv_gpu::Device, s: &Surface) -> Result<Self, String> {
        let mut entries = arrayvec::ArrayVec::<_, 3>::new();
        for _ in 0..3 {
            let slot = Slot::new(device, s.width, s.height, s.pitch, s.format)
                .map_err(|e| e.to_string())?;
            let bytes = s.pitch * s.height;
            s.pixels
                .checked_add(bytes - 1)
                .ok_or("software slot guest address overflow")?;
            entries.push(Entry {
                buffer: slot.buffer.clone(),
                state: slot.state.clone(),
                debt: Region::whole((s.width, s.height)),
            });
        }
        Ok(Self {
            entries: entries.into_inner().ok().unwrap(),
            next: 0,
            last: None,
            layout: Self::layout(s),
            palette: s.palette.clone(),
        })
    }
    pub fn previous(&self) -> Option<&[u8]> {
        self.last.map(|i| {
            // Only this execution thread writes slots. The last publication
            // remains immutable until this method's borrow ends.
            unsafe { self.entries[i].buffer.mapped().unwrap().as_ref() }
        })
    }
    pub fn palette_matches(&self, surface: &Surface) -> bool {
        self.palette == surface.palette
    }
    /// The only admission point for CPU writes to a slot. A held frame has no
    /// completion token yet; a submitted ingest retires with its queue token.
    fn acquire(&mut self, stream: &mut Producer) -> usize {
        loop {
            let completed = stream.completed();
            for offset in 0..3 {
                let i = (self.next + offset) % 3;
                let state = self.entries[i].state.load(Ordering::Acquire);
                if state != u64::MAX && state <= completed {
                    self.entries[i].state.store(u64::MAX, Ordering::Release);
                    self.next = (i + 1) % 3;
                    return i;
                }
            }
            stream.wait();
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &mut self,
        stream: &mut Producer,
        s: &Surface,
        m: &dyn SurfaceMemory,
        damage: Region,
        whole: bool,
        serial: u64,
        video: Arc<[crate::frame::VideoPlane]>,
    ) -> Result<u64, String> {
        for entry in &mut self.entries {
            entry.debt.union(&damage);
        }
        let i = self.acquire(stream);
        // A guest read failure must not retain a partly overwritten baseline.
        self.last = self.last.filter(|&last| last != i);
        let entry = &mut self.entries[i];
        let slot = Slot {
            buffer: entry.buffer.clone(),
            width: s.width,
            height: s.height,
            pitch: s.pitch,
            state: entry.state.clone(),
        };
        // SAFETY: acquire admitted this slot only after its GPU reads completed.
        let output = unsafe { entry.buffer.mapped().unwrap().as_mut() };
        let mut copied = 0;
        let debt = rects(&entry.debt);
        for &r in &debt {
            let start = s.format.byte_offset(r.x);
            let len = s.span(r.x, r.w) as usize;
            for y in r.y..r.y + r.h {
                let row = m.read(s.row(0, y) + start, len)?;
                let at = (y * s.pitch + start) as usize;
                output[at..at + len].copy_from_slice(&row);
                copied += len as u64;
            }
        }
        entry.buffer.flush(debt.iter().map(|r| {
            let start = u64::from(r.y * s.pitch + s.format.byte_offset(r.x));
            start..start + u64::from((r.h - 1) * s.pitch + s.span(r.x, r.w))
        }));
        entry.debt = Region::empty((s.width, s.height));
        self.last = Some(i);
        let palette_changed = match (&self.palette, &s.palette) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        };
        self.palette.clone_from(&s.palette);
        let damage = if whole || palette_changed {
            Region::whole((s.width, s.height))
        } else {
            damage
        };
        stream.push(Command::Present {
            base: Base::Software {
                slot,
                format: s.format,
                palette: self.palette.clone(),
                damage,
            },
            video,
            serial,
        });
        Ok(copied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::native::stream::{self, Consumer};
    use adv_gpu::queue::DeviceOptions;
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;
    const TIMEOUT: Duration = Duration::from_secs(20);
    struct Memory {
        bytes: Vec<u8>,
        reads: Mutex<Vec<(u32, usize)>>,
    }
    impl SurfaceMemory for Memory {
        fn read(&self, at: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
            self.reads.lock().unwrap().push((at, len));
            self.bytes
                .get(at as usize..at as usize + len)
                .map(Cow::Borrowed)
                .ok_or("guest read".into())
        }
        fn write(&mut self, _: u32, _: &[u8]) -> Result<(), String> {
            unreachable!()
        }
        fn writable(&self, _: u32, _: usize) -> Result<(), String> {
            unreachable!()
        }
        fn copy(&mut self, _: u32, _: u32, _: usize) -> Result<(), String> {
            unreachable!()
        }
    }
    fn open() -> (adv_gpu::Device, adv_gpu::Queue) {
        adv_gpu::Device::open(&DeviceOptions {
            software: !cfg!(any(target_vendor = "apple", target_os = "android")),
            validation: !cfg!(target_os = "android"),
            label: "software slots test",
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
    fn take(
        consumer: &Consumer,
    ) -> (
        Slot,
        PixelFormat,
        Option<crate::frame::Palette>,
        crate::frame::Region,
    ) {
        let mut chunk = consumer.try_next().unwrap();
        let Command::Present {
            base:
                Base::Software {
                    slot,
                    format,
                    palette,
                    damage,
                },
            ..
        } = chunk.commands.remove(0)
        else {
            panic!("software frame")
        };
        consumer.recycle(chunk);
        (slot, format, palette, damage)
    }

    // spec: DEV-FRAME-4
    #[test]
    fn publication_copies_only_each_slots_age_debt_and_keeps_padding() {
        let (device, _queue) = open();
        let s = surface(PixelFormat::Bgr24, 64, 2, 7);
        let mut memory = Memory {
            bytes: vec![9; (s.pitch * s.height) as usize],
            reads: Mutex::new(Vec::new()),
        };
        let mut slots = Slots::new(&device, &s).unwrap();
        for entry in &slots.entries {
            unsafe { entry.buffer.mapped().unwrap().as_mut() }.fill(0xa5);
        }
        let (mut producer, consumer) = stream::channel(device.ring(4096).unwrap());
        let mut ids = Vec::new();
        for frame in 0..9 {
            // Cells apart, so each slot's debt stays three separate cells.
            let rect = Rect {
                x: frame * 16 % 64,
                y: 0,
                w: 1,
                h: 1,
            };
            let at = s.row(rect.x, 0) as usize;
            memory.bytes[at..at + 3].fill(frame as u8);
            memory.reads.lock().unwrap().clear();
            let copied = slots
                .publish(
                    &mut producer,
                    &s,
                    &memory,
                    super::region((s.width, s.height), rect),
                    frame == 3,
                    frame as u64,
                    Arc::from([]),
                )
                .unwrap();
            let (slot, _, _, damage) = take(&consumer);
            if frame == 3 {
                assert!(damage.is_whole());
            }
            ids.push(slot.buffer().mapped().unwrap().as_ptr().cast::<u8>() as usize);
            let reads = memory.reads.lock().unwrap();
            if frame < 3 {
                assert_eq!(copied, 64 * 2 * 3);
                assert_eq!(*reads, [(0, 192), (s.pitch, 192)]);
            } else {
                // Three cells of 8 pixels, each on both rows.
                assert_eq!(copied, 3 * 2 * 24);
                let mut cells: Vec<_> = ((frame - 2)..=frame).map(|i| i * 16 % 64).collect();
                cells.sort();
                assert_eq!(
                    *reads,
                    cells
                        .into_iter()
                        .flat_map(|x| [(s.row(x, 0), 24), (s.row(x, 1), 24)])
                        .collect::<Vec<_>>()
                );
                assert_eq!(ids[frame as usize], ids[frame as usize - 3]);
            }
            let bytes = unsafe { slot.buffer().mapped().unwrap().as_ref() };
            for y in 0..2 {
                let at = (y * s.pitch) as usize;
                assert_eq!(&bytes[at..at + 192], &memory.bytes[at..at + 192]);
                assert_eq!(&bytes[at + 192..at + s.pitch as usize], &[0xa5; 7]);
            }
        }
    }

    // spec: DEV-FRAME-5, DEV-FRAME-6
    #[test]
    fn palette_changes_share_the_table_and_invalidate_decode_without_copying_pixels() {
        let (device, _queue) = open();
        let mut s = surface(PixelFormat::Indexed8, 16, 2, 0);
        s.palette = Some(Arc::new(BLACK_PALETTE));
        let memory = Memory {
            bytes: vec![1; 32],
            reads: Mutex::new(Vec::new()),
        };
        let (mut producer, consumer) = stream::channel(device.ring(4096).unwrap());
        let mut slots = Slots::new(&device, &s).unwrap();
        for serial in 0..3 {
            slots
                .publish(
                    &mut producer,
                    &s,
                    &memory,
                    Region::empty((s.width, s.height)),
                    serial == 0,
                    serial,
                    Arc::from([]),
                )
                .unwrap();
            let (slot, _, palette, _) = take(&consumer);
            assert!(Arc::ptr_eq(
                palette.as_ref().unwrap(),
                s.palette.as_ref().unwrap()
            ));
            drop(slot);
        }
        let previous = s.palette.clone().unwrap();
        Arc::make_mut(s.palette.as_mut().unwrap())[1] = [1, 2, 3, 4];
        memory.reads.lock().unwrap().clear();
        assert_eq!(
            slots
                .publish(
                    &mut producer,
                    &s,
                    &memory,
                    Region::empty((s.width, s.height)),
                    false,
                    3,
                    Arc::from([])
                )
                .unwrap(),
            0
        );
        let (_, _, palette, damage) = take(&consumer);
        assert!(Arc::ptr_eq(
            palette.as_ref().unwrap(),
            s.palette.as_ref().unwrap()
        ));
        assert_eq!(previous[1], [0; 4]);
        assert!(memory.reads.lock().unwrap().is_empty());
        assert!(damage.is_whole());
    }

    #[test]
    fn held_ingests_block_rotation_until_the_owner_reports_completion() {
        let (device, _queue) = open();
        let s = surface(PixelFormat::Indexed1, 19, 2, 1);
        let memory = Memory {
            bytes: vec![0; (s.pitch * s.height) as usize],
            reads: Mutex::new(Vec::new()),
        };
        let mut slots = Slots::new(&device, &s).unwrap();
        let (mut producer, consumer) = stream::channel(device.ring(4096).unwrap());
        let mut held = Vec::new();
        for serial in 0..3 {
            slots
                .publish(
                    &mut producer,
                    &s,
                    &memory,
                    Region::whole((s.width, s.height)),
                    false,
                    serial,
                    Arc::from([]),
                )
                .unwrap();
            held.push(take(&consumer).0);
        }
        let (events, recv) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            events.send(0).unwrap();
            slots
                .publish(
                    &mut producer,
                    &s,
                    &memory,
                    Region::whole((s.width, s.height)),
                    false,
                    3,
                    Arc::from([]),
                )
                .unwrap();
            events.send(1).unwrap();
        });
        assert_eq!(recv.recv_timeout(TIMEOUT).unwrap(), 0);
        assert!(recv.recv_timeout(Duration::from_millis(20)).is_err());
        held.remove(1).ingested(adv_gpu::Token(7));
        consumer.retired(adv_gpu::Token(6));
        assert!(recv.recv_timeout(Duration::from_millis(20)).is_err());
        consumer.retired(adv_gpu::Token(7));
        assert_eq!(recv.recv_timeout(TIMEOUT).unwrap(), 1);
        thread.join().unwrap();
    }

    #[test]
    fn slot_creation_rejects_invalid_guest_layouts() {
        let (device, _queue) = open();
        for (width, height, pitch) in [
            (0, 2, 4),
            (1, 0, 4),
            (2, 2, 7),
            (16385, 1, 65540),
            (1, 2, u32::MAX),
        ] {
            let mut s = surface(PixelFormat::Bgra32, width, height, 0);
            s.pitch = pitch;
            assert!(Slots::new(&device, &s).is_err());
        }
    }
}
