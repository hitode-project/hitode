//! Frame-source adapters for the same surface assertions in both renderer modes.
use adv_device::surface::{self, FrameView, Rect, VideoCursorRegion};
#[cfg(feature = "native-display")]
use adv_device::surface::{FlushReason, SurfaceMemory, VideoLayer};
#[cfg(feature = "native-display")]
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct Snapshot {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub frames: u64,
    pub damage: Vec<Rect>,
}
impl Snapshot {
    fn take_cpu(&mut self) -> surface::Snapshot {
        surface::Snapshot {
            width: self.width,
            height: self.height,
            frames: self.frames,
            bgra: std::mem::take(&mut self.bgra),
            ..Default::default()
        }
    }
    fn set_cpu(&mut self, cpu: surface::Snapshot) {
        self.width = cpu.width;
        self.height = cpu.height;
        self.frames = cpu.frames;
        self.damage = cpu.damage.to_vec();
        self.bgra = cpu.bgra;
    }
}
impl surface::Presenter for Snapshot {
    fn present(&mut self, frame: FrameView<'_>) -> Result<(), String> {
        let mut cpu = self.take_cpu();
        surface::Presenter::present(&mut cpu, frame)?;
        self.set_cpu(cpu);
        Ok(())
    }
}

#[cfg(not(feature = "native-display"))]
pub type Store = surface::Surfaces<Snapshot>;
#[cfg(not(feature = "native-display"))]
pub use surface::FrameProducer as Producer;
#[cfg(not(feature = "native-display"))]
pub struct Consumer(surface::FrameConsumer);
#[cfg(not(feature = "native-display"))]
pub struct Frame<'a>(&'a surface::PendingFrame);
#[cfg(not(feature = "native-display"))]
pub fn frame_mailbox() -> (Producer, Consumer) {
    let (tx, rx) = surface::frame_mailbox();
    (tx, Consumer(rx))
}
#[cfg(not(feature = "native-display"))]
impl Consumer {
    pub fn update(&mut self) -> bool {
        self.0.update()
    }
    pub fn current(&self) -> Frame<'_> {
        Frame(self.0.current())
    }
}
#[cfg(not(feature = "native-display"))]
impl Frame<'_> {
    pub fn snapshot(&self, out: &mut Snapshot) -> Result<(), String> {
        let mut cpu = out.take_cpu();
        self.0.snapshot(&mut cpu)?;
        out.set_cpu(cpu);
        Ok(())
    }
    pub fn damage(&self) -> &[Rect] {
        self.0.damage()
    }
    pub fn hide_cursor(&self) -> bool {
        self.0.hide_cursor()
    }
    pub fn video_cursor_regions(&self) -> Result<Vec<VideoCursorRegion>, String> {
        self.0.video_cursor_regions()
    }
}
#[cfg(not(feature = "native-display"))]
pub fn assert_age_copies(_: &Store, full_copies: usize) {
    assert!(full_copies >= 1);
}

#[cfg(feature = "native-display")]
mod native {
    use super::*;
    use adv_device::{
        frame,
        frame::test_support::{CapturePresenter, CapturedFrame},
        gpu::Gpu,
    };
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::{Rc, Weak};
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    struct Publication {
        frame: Frame,
        buffer: u64,
    }
    struct Execution {
        _gpu: Gpu,
        frames: Receiver<CapturedFrame>,
    }
    thread_local! {
        static EXECUTION: RefCell<Weak<Execution>> = const { RefCell::new(Weak::new()) };
    }
    fn execution() -> Rc<Execution> {
        EXECUTION.with(|cached| {
            if let Some(execution) = cached.borrow().upgrade() {
                return execution;
            }
            let (presenter, frames) = CapturePresenter::new();
            let (device, queue) = adv_gpu::Device::open(&adv_gpu::queue::DeviceOptions {
                software: !cfg!(any(target_vendor = "apple", target_os = "android")),
                validation: !cfg!(target_os = "android"),
                disable: adv_gpu::Modern::from_test_env(),
                ..Default::default()
            })
            .unwrap();
            let gpu = pollster::block_on(Gpu::request_with_presenter(
                device,
                queue,
                Box::new(presenter),
            ))
            .unwrap();
            let execution = Rc::new(Execution { _gpu: gpu, frames });
            *cached.borrow_mut() = Rc::downgrade(&execution);
            execution
        })
    }

    #[derive(Clone, Default)]
    pub struct Frame {
        base: Snapshot,
        damage: Vec<Rect>,
        history: Vec<(u64, Vec<Rect>)>,
        video: Arc<[(u64, VideoLayer)]>,
    }
    fn add(rects: &mut Vec<Rect>, mut rect: Rect) {
        let mut i = 0;
        while i < rects.len() {
            let old = rects[i];
            if rect.x <= old.x + old.w
                && old.x <= rect.x + rect.w
                && rect.y <= old.y + old.h
                && old.y <= rect.y + rect.h
            {
                rect = rect.union(rects.remove(i));
                i = 0;
            } else {
                i += 1;
            }
        }
        if rects.len() == 4 {
            rect = rects.drain(..).fold(rect, Rect::union);
        }
        rects.push(rect);
    }
    impl Frame {
        fn full(&self) -> Rect {
            Rect {
                x: 0,
                y: 0,
                w: self.base.width,
                h: self.base.height,
            }
        }
        pub fn damage(&self) -> &[Rect] {
            &self.damage
        }
        pub fn hide_cursor(&self) -> bool {
            self.video_cursor_regions()
                .unwrap()
                .iter()
                .any(|r| r.hide_cursor)
        }
        pub fn video_cursor_regions(&self) -> Result<Vec<VideoCursorRegion>, String> {
            self.video
                .iter()
                .filter_map(|(_, v)| {
                    v.layout((self.base.width, self.base.height))
                        .map(|l| l.cursor_region(v.hide_cursor))
                        .transpose()
                })
                .collect()
        }
        pub fn snapshot(&self, out: &mut Snapshot) -> Result<(), String> {
            let mut damage = Vec::new();
            if out.width != self.base.width
                || out.height != self.base.height
                || out.frames == 0
                || !self.video.is_empty()
                || self.history.first().is_none_or(|(n, _)| out.frames < n - 1)
            {
                damage.push(self.full());
            } else {
                for (_, rects) in self.history.iter().filter(|(n, _)| *n > out.frames) {
                    for &r in rects {
                        add(&mut damage, r);
                    }
                }
            }
            *out = self.base.clone();
            out.damage = damage;
            for (_, video) in self.video.iter() {
                let mut patch = surface::Snapshot::default();
                if let Some(rect) = video.rasterize((out.width, out.height), &mut patch)? {
                    for y in 0..rect.h as usize {
                        let at = ((rect.y as usize + y) * out.width as usize + rect.x as usize) * 4;
                        let n = rect.w as usize * 4;
                        out.bgra[at..at + n].copy_from_slice(&patch.bgra[y * n..(y + 1) * n]);
                    }
                }
            }
            Ok(())
        }
    }
    pub struct Producer {
        legacy: surface::FrameProducer,
        native: adv_sync::mailbox::Producer<Frame>,
    }
    pub struct Consumer {
        legacy: surface::FrameConsumer,
        native: adv_sync::mailbox::Consumer<Frame>,
    }
    pub fn frame_mailbox() -> (Producer, Consumer) {
        let (tx, rx) = surface::frame_mailbox();
        let (native_tx, native_rx) =
            adv_sync::mailbox::channel(std::array::from_fn(|_| Frame::default()));
        (
            Producer {
                legacy: tx,
                native: native_tx,
            },
            Consumer {
                legacy: rx,
                native: native_rx,
            },
        )
    }
    impl Consumer {
        pub fn update(&mut self) -> bool {
            self.legacy.update();
            self.native.update()
        }
        pub fn current(&self) -> &Frame {
            self.native.current()
        }
    }
    struct Batch {
        publications: Vec<Publication>,
        copied: u64,
    }
    pub struct Store {
        inner: surface::Surfaces<Snapshot>,
        execution: Rc<Execution>,
        latest: Option<Frame>,
        mailbox: Option<(adv_sync::mailbox::Producer<Frame>, adv_sync::WakeEvent)>,
        deferred: bool,
        captured: u64,
        published: u64,
        copied: u64,
        batches: Vec<Batch>,
    }
    impl std::ops::Deref for Store {
        type Target = surface::Surfaces<Snapshot>;
        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl std::ops::DerefMut for Store {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }
    impl Store {
        pub fn new(
            width: u32,
            height: u32,
            arena: u32,
            len: u32,
            presenter: Snapshot,
        ) -> Result<Self, String> {
            let execution = execution();
            Ok(Self {
                inner: surface::Surfaces::new(width, height, arena, len, presenter)?,
                execution,
                latest: None,
                mailbox: None,
                deferred: false,
                captured: 0,
                published: 0,
                copied: 0,
                batches: Vec::new(),
            })
        }
        fn publish(&mut self, frame: Frame) {
            if let Some((mailbox, wake)) = &mut self.mailbox {
                let overwritten = mailbox.publish(|slot| *slot = frame.clone());
                if !overwritten {
                    wake.signal();
                }
            }
            self.latest = Some(frame);
            if !self.deferred {
                self.capture().unwrap();
            }
        }
        fn collect(&mut self) {
            let mut publications = Vec::new();
            for _ in self.published..self.inner.stats.published_frames {
                let captured = self
                    .execution
                    .frames
                    .recv_timeout(Duration::from_secs(20))
                    .unwrap();
                let pixels = captured.pixels;
                let full = Rect {
                    x: 0,
                    y: 0,
                    w: pixels.width,
                    h: pixels.height,
                };
                let damage = match captured.damage {
                    frame::Damage::None => vec![],
                    frame::Damage::Whole => vec![full],
                    frame::Damage::Rects(rects) => rects
                        .iter()
                        .map(|r| Rect {
                            x: r.x,
                            y: r.y,
                            w: r.w,
                            h: r.h,
                        })
                        .collect(),
                };
                let mut publication = Publication {
                    buffer: captured.buffer.unwrap(),
                    frame: Frame {
                        base: Snapshot {
                            width: pixels.width,
                            height: pixels.height,
                            bgra: pixels.bgra,
                            frames: pixels.frames,
                            ..Default::default()
                        },
                        damage,
                        ..Default::default()
                    },
                };
                if let Some(previous) = &self.latest {
                    if (publication.frame.base.width, publication.frame.base.height)
                        == (previous.base.width, previous.base.height)
                    {
                        publication.frame.history = previous.history.clone();
                    }
                    publication.frame.video = previous.video.clone();
                }
                publication.frame.history.push((
                    publication.frame.base.frames,
                    publication.frame.damage.clone(),
                ));
                if publication.frame.history.len() > 16 {
                    publication.frame.history.remove(0);
                }
                self.publish(publication.frame.clone());
                publications.push(publication);
            }
            if !publications.is_empty() {
                self.batches.push(Batch {
                    publications,
                    copied: self.inner.stats.copied_bytes - self.copied,
                });
            }
            self.published = self.inner.stats.published_frames;
            self.copied = self.inner.stats.copied_bytes;
        }
        fn capture(&mut self) -> Result<bool, String> {
            let Some(frame) = &self.latest else {
                return Ok(false);
            };
            if frame.base.frames == self.captured {
                return Ok(false);
            }
            frame.snapshot(&mut self.inner.presenter)?;
            self.captured = frame.base.frames;
            Ok(true)
        }
        pub fn present(
            &mut self,
            id: u32,
            m: &dyn SurfaceMemory,
            damage: Rect,
        ) -> Result<(), String> {
            self.present_from(0, id, m, damage)
        }
        pub fn present_from(
            &mut self,
            writer: u32,
            id: u32,
            m: &dyn SurfaceMemory,
            damage: Rect,
        ) -> Result<(), String> {
            self.inner.present_from(writer, id, m, damage)?;
            self.collect();
            Ok(())
        }
        pub fn flush(
            &mut self,
            m: &dyn SurfaceMemory,
            reason: FlushReason,
        ) -> Result<bool, String> {
            let result = self.inner.flush(m, reason)?;
            self.collect();
            Ok(result)
        }
        pub fn flush_context(
            &mut self,
            context: u32,
            m: &dyn SurfaceMemory,
            reason: FlushReason,
        ) -> Result<bool, String> {
            let result = self.inner.flush_context(context, m, reason)?;
            self.collect();
            Ok(result)
        }
        pub fn capture_latest(&mut self, m: &dyn SurfaceMemory) -> Result<bool, String> {
            self.flush(m, FlushReason::Immediate)?;
            self.capture()
        }
        pub fn set_deferred_presentation(&mut self, enabled: bool) -> Result<(), String> {
            self.inner.set_deferred_presentation(enabled)?;
            self.deferred = enabled;
            if !enabled {
                self.capture()?;
            }
            Ok(())
        }
        pub fn set_frame_mailbox(&mut self, tx: Producer, wake: adv_sync::WakeEvent) {
            self.inner.set_frame_mailbox(tx.legacy, wake.clone());
            self.mailbox = Some((tx.native, wake));
            self.deferred = true;
        }
        pub fn transfer_frame_mailbox_to(
            &mut self,
            destination: &mut Self,
            m: &dyn SurfaceMemory,
        ) -> Result<bool, String> {
            let result = self
                .inner
                .transfer_frame_mailbox_to(&mut destination.inner, m)?;
            self.collect();
            destination.mailbox = self.mailbox.take();
            destination.deferred = self.deferred;
            destination.captured = destination.inner.stats.presentations;
            self.deferred = true;
            Ok(result)
        }
        pub fn set_palette(
            &mut self,
            id: u32,
            start: u32,
            colors: &[[u8; 4]],
            m: &dyn SurfaceMemory,
        ) -> Result<usize, String> {
            let count = self.inner.set_palette(id, start, colors, m)?;
            self.collect();
            Ok(count)
        }
        pub fn fill_raw(
            &mut self,
            id: u32,
            rect: Rect,
            value: u32,
            m: &mut dyn SurfaceMemory,
        ) -> Result<(), String> {
            self.inner.fill_raw(id, rect, value, m)
        }
        pub fn copy(
            &mut self,
            src: u32,
            sr: Rect,
            dst: u32,
            dr: Rect,
            m: &mut dyn SurfaceMemory,
        ) -> Result<Rect, String> {
            self.inner.copy(src, sr, dst, dr, m)
        }
        pub fn blit(
            &mut self,
            src: u32,
            sr: Rect,
            dst: u32,
            dr: surface::BlitRect,
            options: surface::BlitOptions,
            m: &mut dyn SurfaceMemory,
        ) -> Result<Rect, String> {
            self.inner.blit(src, sr, dst, dr, options, m)
        }
        pub fn configure_window(
            &mut self,
            width: u32,
            height: u32,
            m: &mut dyn SurfaceMemory,
        ) -> Result<(), String> {
            self.inner.configure_window(width, height, m)?;
            self.collect();
            Ok(())
        }
        pub fn set_video_layer(
            &mut self,
            id: u64,
            video: Option<VideoLayer>,
            m: &dyn SurfaceMemory,
        ) -> Result<u64, String> {
            let before = self.inner.stats.presentations;
            let serial = self.inner.set_video_layer(id, video, m)?;
            self.collect();
            if serial != before {
                let metadata = self.inner.latest_frame().unwrap();
                let mut frame = self.latest.clone().unwrap_or_else(|| {
                    let (width, height) = metadata.extent();
                    Frame {
                        base: Snapshot {
                            width,
                            height,
                            bgra: vec![0; width as usize * height as usize * 4],
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                });
                frame.base.frames = serial;
                frame.video = metadata.shared_video_layers();
                frame.damage = vec![frame.full()];
                frame.history.clear();
                self.publish(frame);
            }
            Ok(serial)
        }
    }
    pub fn assert_age_copies(store: &Store, _: usize) {
        let mut slots: HashMap<u64, Vec<Rect>> = HashMap::new();
        for batch in &store.batches {
            let mut expected = 0;
            for publication in &batch.publications {
                let width = publication.frame.base.width;
                let pixels = (width * publication.frame.base.height) as usize;
                for debt in slots.values_mut() {
                    for &rect in &publication.frame.damage {
                        add(debt, rect);
                    }
                }
                let debt = slots
                    .entry(publication.buffer)
                    .or_insert_with(|| vec![publication.frame.full()]);
                // Count the union's coverage independently of the slot copier.
                // Debt uses the same bounded-rectangle contract as frame damage.
                let mut covered = vec![false; pixels];
                for r in debt.iter() {
                    for y in r.y..r.y + r.h {
                        for x in r.x..r.x + r.w {
                            covered[(y * width + x) as usize] = true;
                        }
                    }
                }
                expected += covered.iter().filter(|&&pixel| pixel).count() as u64 * 4;
                debt.clear();
            }
            assert_eq!(batch.copied, expected, "slot age debt plus new damage");
        }
    }
}
#[cfg(feature = "native-display")]
pub use native::{Store, assert_age_copies, frame_mailbox};
