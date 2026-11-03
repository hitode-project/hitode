//! Native display ownership and the bounded UI/owner exchange.
use adv_device::frame::{self, SwapImage};
use adv_display::{
    compositor::{Compositor, HeadlessCompositor},
    display::{self, Display},
    plan,
};
use adv_gpu::{Device, Queue};
use adv_sync::WakeEvent;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Upscaler {
    #[default]
    Off,
    Anime4k,
    Cunny,
    Artcnn,
    K7,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    pub rectangle: [f32; 4],
    pub nearest: bool,
}
#[derive(Clone, Debug, PartialEq)]
struct Settings {
    view: (u32, u32),
    scaling: crate::Scaling,
    upscaler: Upscaler,
    rate: u32,
}
impl Settings {
    fn plan(&self) -> plan::Settings {
        plan::Settings {
            view: self.view,
            rotation: plan::Rotation::None,
            upscaler: plan::Upscaler::Off,
            background: [0.; 3],
        }
    }
}
struct Shared {
    settings: Mutex<Settings>,
    feedback: Mutex<VecDeque<Result<Feedback, String>>>,
    statistics: Mutex<Option<crate::performance::NativePresentation>>,
    #[cfg(target_os = "android")]
    window: Mutex<Option<Option<AndroidCompositor>>>,
    #[cfg(target_os = "android")]
    caps: plan::CompositorCaps,
    wake: WakeEvent,
    layout: Mutex<([f64; 4], crate::Scaling)>,
    #[cfg(target_os = "android")]
    rate: Arc<std::sync::atomic::AtomicU32>,
}
impl Shared {
    fn send(&self, feedback: Result<Feedback, String>) {
        let mut inbox = self.feedback.lock().unwrap();
        if inbox.len() == 128 {
            inbox.pop_front();
        }
        inbox.push_back(feedback);
        drop(inbox);
        self.wake.signal();
    }
}
pub(crate) struct Feedback {
    pub serial: u64,
    pub published_at: Instant,
    pub canvas: (u32, u32),
    pub placement: Placement,
    pub cursor_regions: Arc<[adv_runtime::VideoCursorRegion]>,
    pub stats: crate::performance::NativePresentation,
    pub elapsed: Duration,
    pub at: Instant,
}
pub struct Ui {
    shared: Arc<Shared>,
    #[cfg(target_vendor = "apple")]
    apple: Option<Arc<Mutex<adv_display::compositor::LayerCompositor>>>,
}
impl Ui {
    #[cfg(target_vendor = "apple")]
    pub(crate) fn set_scale(&self, scale: f64) {
        if let Some(apple) = &self.apple {
            apple
                .lock()
                .unwrap()
                .set_scale(scale, objc2::MainThreadMarker::new().unwrap());
        }
    }
    pub fn set_upscaler(&mut self, value: Upscaler) {
        self.shared.settings.lock().unwrap().upscaler = value;
    }
    pub fn set_scaling(&mut self, value: crate::Scaling) {
        self.shared.settings.lock().unwrap().scaling = value;
    }
    pub fn update(&mut self, view: (u32, u32), rate: u32) {
        let mut s = self.shared.settings.lock().unwrap();
        s.view = view;
        s.rate = rate;
        #[cfg(target_os = "android")]
        self.shared
            .rate
            .store(rate, std::sync::atomic::Ordering::Relaxed);
    }
    pub(crate) fn statistics(&self) -> Option<crate::performance::NativePresentation> {
        self.shared.statistics.lock().unwrap().take()
    }
    pub(crate) fn poll(&mut self) -> Result<Option<Feedback>, String> {
        self.shared.feedback.lock().unwrap().pop_front().transpose()
    }
    #[cfg(target_os = "android")]
    pub unsafe fn replace_window(
        &mut self,
        window: Option<std::ptr::NonNull<ndk_sys::ANativeWindow>>,
    ) -> Result<(), String> {
        let compositor = window
            .map(|w| unsafe {
                AndroidCompositor::new(w, self.shared.caps.clone(), self.shared.rate.clone())
            })
            .transpose()?;
        *self.shared.window.lock().unwrap() = Some(compositor);
        Ok(())
    }
}
pub struct Startup {
    device: Device,
    queue: Queue,
    presenter: Presenter,
}
impl Startup {
    pub fn start(self) -> Result<adv_device::gpu::Gpu, String> {
        pollster::block_on(adv_device::gpu::Gpu::request_with_presenter(
            self.device,
            self.queue,
            Box::new(self.presenter),
        ))
        .map_err(|e| e.to_string())
    }
}
struct Presenter {
    next_present: Instant,
    settings: Option<Settings>,
    display: Display,
    shared: Arc<Shared>,
    stats: crate::performance::NativePresentation,
}
pub(crate) fn platform_caps(device: Option<&Device>) -> plan::CompositorCaps {
    plan::CompositorCaps {
        scales: true,
        nearest: !cfg!(target_os = "android"),
        rotates: cfg!(target_os = "android"),
        yuv_layers: true,
        solid_color_layers: true,
        damage_hints: true,
        presentable_compressed: device.is_some_and(|d| {
            d.caps()
                .format(adv_gpu::Format::Bgra8Unorm)
                .contains(adv_gpu::FormatCaps::PRESENTABLE_COMPRESSED)
        }),
        tile: device.map_or((0, 0), |d| d.caps().limits.tile),
    }
}
pub(crate) fn open(software: bool) -> Result<(Device, Queue), String> {
    Device::open(&adv_gpu::queue::DeviceOptions {
        software,
        label: "session display",
        ..Default::default()
    })
    .map_err(|e| e.to_string())
}
pub(crate) fn channel(
    device: Device,
    queue: Queue,
    compositor: Box<dyn Compositor + Send>,
    view: (u32, u32),
    wake: WakeEvent,
    #[cfg(target_os = "android")] rate: Arc<std::sync::atomic::AtomicU32>,
) -> Result<(Startup, Ui), String> {
    let settings = Settings {
        view,
        scaling: crate::Scaling::Fit,
        upscaler: Upscaler::Off,
        rate: 60,
    };
    let shared = Arc::new(Shared {
        settings: Mutex::new(settings),
        feedback: Mutex::new(VecDeque::new()),
        statistics: Mutex::new(None),
        wake,
        layout: Mutex::new(([0., 0., view.0 as f64, view.1 as f64], crate::Scaling::Fit)),
        #[cfg(target_os = "android")]
        rate,
        #[cfg(target_os = "android")]
        window: Mutex::new(None),
        #[cfg(target_os = "android")]
        caps: compositor.caps(),
    });
    let compositor = Box::new(PlacedCompositor {
        compositor,
        shared: shared.clone(),
    });
    let display = Display::new(
        device.clone(),
        compositor,
        shared.settings.lock().unwrap().plan(),
    )
    .map_err(|e| e.to_string())?;
    Ok((
        Startup {
            device,
            queue,
            presenter: Presenter {
                next_present: Instant::now(),
                settings: None,
                display,
                shared: shared.clone(),
                stats: Default::default(),
            },
        },
        Ui {
            shared,
            #[cfg(target_vendor = "apple")]
            apple: None,
        },
    ))
}
pub(crate) fn headless(
    software: bool,
    view: (u32, u32),
    wake: WakeEvent,
) -> Result<Startup, String> {
    let (device, queue) = open(software)?;
    let compositor = Box::new(HeadlessCompositor::new(platform_caps(Some(&device))));
    Ok(channel(
        device,
        queue,
        compositor,
        view,
        wake,
        #[cfg(target_os = "android")]
        Arc::new(std::sync::atomic::AtomicU32::new(60)),
    )?
    .0)
}
fn damage(d: &frame::Damage) -> plan::Damage {
    match d {
        frame::Damage::None => plan::Damage::None,
        frame::Damage::Whole => plan::Damage::Whole,
        frame::Damage::Rects(r) => plan::Damage::rects(r.iter().copied().map(Into::into)),
    }
}
fn format(f: frame::PixelFormat) -> adv_display::stages::ingest::PixelFormat {
    use adv_display::stages::ingest::PixelFormat as P;
    match f {
        frame::PixelFormat::Bgra32 => P::Bgra32,
        frame::PixelFormat::Bgr24 => P::Bgr24,
        frame::PixelFormat::Rgb565 => P::Rgb565,
        frame::PixelFormat::Rgb555 => P::Rgb555,
        frame::PixelFormat::Indexed8 => P::Indexed8,
        frame::PixelFormat::Indexed4 => P::Indexed4,
        frame::PixelFormat::Indexed1 => P::Indexed1,
    }
}
fn map(
    frame: frame::Frame,
) -> (
    display::Frame,
    Option<frame::Slot>,
    Instant,
    Arc<[adv_runtime::VideoCursorRegion]>,
) {
    let published_at = frame.published_at;
    let (canvas, base, kind, changed, slot) = match frame.base {
        frame::Base::Gpu {
            image,
            damage: changed,
            ready,
        } => {
            let size = match &image {
                SwapImage::Private(i) => i.desc().size,
                SwapImage::Presentable(i) => i.image().desc().size,
            };
            let presentable = matches!(image, SwapImage::Presentable(_));
            let base = match image {
                SwapImage::Private(i) => display::Base::Private(i),
                SwapImage::Presentable(i) => display::Base::Presentable { image: i, ready },
            };
            (
                (size.width, size.height),
                base,
                plan::BaseKind::Gpu { presentable },
                damage(&changed),
                None,
            )
        }
        frame::Base::Software {
            slot,
            format: f,
            palette,
            damage: changed,
        } => {
            let canvas = slot.extent();
            let base = display::Base::Software {
                buffer: slot.buffer().clone(),
                pitch: slot.pitch(),
                extent: canvas,
                format: format(f),
                palette: palette.map(|p| Box::new(*p)),
            };
            (
                canvas,
                base,
                plan::BaseKind::Software {
                    indexed: matches!(
                        f,
                        frame::PixelFormat::Indexed8
                            | frame::PixelFormat::Indexed4
                            | frame::PixelFormat::Indexed1
                    ),
                },
                damage(&changed),
                Some(slot),
            )
        }
    };
    let mut info = plan::FrameInfo {
        serial: frame.serial,
        canvas,
        base: kind,
        damage: changed,
        video: Default::default(),
        cursor_regions: Default::default(),
    };
    let cursor = frame
        .video
        .iter()
        .map(|v| adv_runtime::VideoCursorRegion {
            rectangle: adv_device::surface::Rect {
                x: v.geometry.clip.x,
                y: v.geometry.clip.y,
                w: v.geometry.clip.w,
                h: v.geometry.clip.h,
            },
            hide_cursor: v.geometry.hide_cursor,
        })
        .collect::<Vec<_>>()
        .into();
    let video = frame
        .video
        .iter()
        .map(|v| {
            let update = match v.take_picture() {
                frame::VideoUpdate::Changed(i) => Some(i),
                frame::VideoUpdate::Retained => None,
            };
            info.video.push(plan::VideoGeometry {
                id: v.id,
                source: v.geometry.source,
                rect: v.geometry.destination.into(),
                changed: update.is_some(),
            });
            display::Video {
                id: v.id,
                image: v.picture.image().clone(),
                planes: (v.picture.image().desc().format == adv_gpu::Format::Nv12)
                    .then(|| v.picture.planes().clone()),
                geometry: adv_display::stages::video::Geometry {
                    destination: v.geometry.destination.into(),
                    source_uv: v.geometry.source_uv,
                    background: v.geometry.background.map(|(r, c)| (r.into(), c)),
                    color: v.picture.color(),
                },
                update,
            }
        })
        .collect();
    (
        display::Frame {
            info,
            base,
            video,
            at: Instant::now(),
        },
        slot,
        published_at,
        cursor,
    )
}
impl frame::Presenter for Presenter {
    fn presentable(&self, width: u32, height: u32) -> bool {
        self.display.presentable((width, height))
    }
    fn present(&mut self, queue: &mut Queue, frame: frame::Frame) {
        let settings = self.shared.settings.lock().unwrap().clone();

        #[cfg(target_os = "android")]
        if let Some(window) = self.shared.window.lock().unwrap().take() {
            match window {
                Some(c) => {
                    if let Err(e) = self.display.rebuild(
                        Box::new(PlacedCompositor {
                            compositor: Box::new(c),
                            shared: self.shared.clone(),
                        }),
                        queue,
                    ) {
                        self.shared.send(Err(format!("display rebuild: {e:?}")));
                        return;
                    }
                }
                None => self.display.suspend(),
            }
        }
        if settings.view.0 == 0 || settings.view.1 == 0 || !self.display.attached() {
            return;
        }
        let started = Instant::now();
        let (mut frame, slot, published_at, cursor_regions) = map(frame);
        frame.at = crate::frame_pacer::submission_at(frame.at, self.next_present);
        self.next_present = frame.at + Duration::from_nanos(1_000_000_000 / 60);
        let serial = frame.info.serial;
        let canvas = frame.info.canvas;
        let rectangle = settings.scaling.rectangle(
            (f64::from(settings.view.0), f64::from(settings.view.1)),
            canvas,
        );
        *self.shared.layout.lock().unwrap() = (rectangle, settings.scaling);
        let mut planned = settings.plan();
        if settings.scaling == crate::Scaling::Integer {
            planned.view = (rectangle[2] as u32, rectangle[3] as u32);
        }
        if self.settings.as_ref() != Some(&settings) {
            self.display.invalidate();
        }
        self.settings = Some(settings.clone());
        self.display.settings(planned);
        let software = slot.is_some();
        let result = self.display.present(queue, frame);
        if self.display.last_read().0 != 0
            && let Some(slot) = slot
        {
            slot.ingested(self.display.last_read());
        }
        match result {
            Ok(Some(_)) => {
                self.stats.consumed += 1;
                self.stats.presented += 1;
                self.stats.uploads += u64::from(software);
                if let Some(stats) = self.display.statistics().back() {
                    self.stats.direct_presentations +=
                        u64::from(stats.kind == display::PlanKind::Direct);
                    self.stats.display_passes += u64::from(stats.passes);
                    self.stats.display_submissions += u64::from(stats.submissions);
                    self.stats.display_tiles +=
                        stats.stages.iter().map(|s| s.tiles as u64).sum::<u64>();
                    self.stats.display_pixels += stats.stages.iter().map(|s| s.pixels).sum::<u64>();
                    self.stats.display_plan = match stats.kind {
                        display::PlanKind::Direct => "direct",
                        display::PlanKind::Merged => "merged",
                        display::PlanKind::Upscaled => "upscaled",
                    };
                    self.stats.display_stage_tiles = [0; 3];
                    self.stats.display_stage_pixels = [0; 3];
                    for stage in &stats.stages {
                        let index = match stage.kind {
                            display::StageKind::Ingest => 0,
                            display::StageKind::Video => 1,
                            display::StageKind::Output => 2,
                        };
                        self.stats.display_stage_tiles[index] += stage.tiles as u32;
                        self.stats.display_stage_pixels[index] += stage.pixels as u32;
                    }
                }
                let rectangle = settings
                    .scaling
                    .rectangle(
                        (f64::from(settings.view.0), f64::from(settings.view.1)),
                        canvas,
                    )
                    .map(|v| v as f32);
                let at = Instant::now();
                let elapsed = at - started;
                self.stats.max_job_ns = self.stats.max_job_ns.max(elapsed.as_nanos() as u64);
                self.shared.send(Ok(Feedback {
                    serial,
                    published_at,
                    canvas,
                    placement: Placement {
                        rectangle,
                        nearest: (rectangle[2] as u32).is_multiple_of(canvas.0)
                            && (rectangle[3] as u32).is_multiple_of(canvas.1),
                    },
                    cursor_regions,
                    stats: self.stats,
                    elapsed,
                    at,
                }));
            }
            Ok(None) => {}
            Err(display::Error::Compositor(adv_display::compositor::Error::SurfaceLost)) => {
                self.display.suspend()
            }
            Err(e) => self.shared.send(Err(format!("display present: {e:?}"))),
        }
    }
    fn drain(&mut self) {
        self.display.drain();
        if let Some(stats) = self.display.statistics().back() {
            self.stats.display_latch_ns = stats
                .latched
                .map(|t| t.saturating_duration_since(stats.at).as_nanos() as u64);
            self.stats.display_shown_ns = stats
                .shown
                .map(|t| t.saturating_duration_since(stats.at).as_nanos() as u64);
            *self.shared.statistics.lock().unwrap() = Some(self.stats);
            self.shared.wake.signal();
        }
    }
}

#[cfg(target_os = "android")]
pub(crate) struct AndroidCompositor {
    compositor: adv_display::compositor::android::SurfaceCompositor,
    _window: WindowLease,
    rate: Arc<std::sync::atomic::AtomicU32>,
}
#[cfg(target_os = "android")]
struct WindowLease(std::ptr::NonNull<ndk_sys::ANativeWindow>);
#[cfg(target_os = "android")]
impl Drop for WindowLease {
    fn drop(&mut self) {
        unsafe {
            ndk_sys::ANativeWindow_release(self.0.as_ptr());
        }
    }
}
// SAFETY: SurfaceControl handles have no thread affinity. The wrapper retains
// the window until its compositor has detached every buffer.
#[cfg(target_os = "android")]
unsafe impl Send for AndroidCompositor {}
#[cfg(target_os = "android")]
impl AndroidCompositor {
    pub unsafe fn new(
        window: std::ptr::NonNull<ndk_sys::ANativeWindow>,
        caps: plan::CompositorCaps,
        rate: Arc<std::sync::atomic::AtomicU32>,
    ) -> Result<Self, String> {
        let compositor =
            unsafe { adv_display::compositor::android::SurfaceCompositor::new(window, caps) }
                .map_err(|e| format!("SurfaceControl: {e:?}"))?;
        unsafe {
            ndk_sys::ANativeWindow_acquire(window.as_ptr());
        }
        Ok(Self {
            compositor,
            _window: WindowLease(window),
            rate,
        })
    }
}
#[cfg(target_os = "android")]
impl Compositor for AndroidCompositor {
    fn caps(&self) -> plan::CompositorCaps {
        self.compositor.caps()
    }
    fn present(
        &mut self,
        l: &plan::Layers,
        d: &plan::Tiles,
        i: Vec<adv_display::compositor::AcquiredImage>,
        s: u64,
        a: Instant,
    ) -> Result<(), adv_display::compositor::PresentError> {
        self.compositor
            .set_frame_rate(self.rate.load(std::sync::atomic::Ordering::Relaxed));
        self.compositor.present(l, d, i, s, a)
    }
    fn drain(
        &mut self,
        p: &mut Vec<adv_display::compositor::Presented>,
        r: &mut Vec<adv_display::compositor::ReleasedImage>,
    ) {
        self.compositor.drain(p, r);
    }
}

// Adapt the planner's aspect-preserving geometry to the shell's explicit
// integer/stretch placement, keeping video and pointer coordinates together.
struct PlacedCompositor {
    compositor: Box<dyn Compositor + Send>,
    shared: Arc<Shared>,
}
impl Compositor for PlacedCompositor {
    fn caps(&self) -> plan::CompositorCaps {
        self.compositor.caps()
    }
    fn present(
        &mut self,
        l: &plan::Layers,
        d: &plan::Tiles,
        i: Vec<adv_display::compositor::AcquiredImage>,
        s: u64,
        a: Instant,
    ) -> Result<(), adv_display::compositor::PresentError> {
        let (rectangle, scaling) = *self.shared.layout.lock().unwrap();
        if scaling == crate::Scaling::Fit {
            return self.compositor.present(l, d, i, s, a);
        }
        let mut layers = l.clone();
        let from = l.base.dst;
        let map = |r: adv_display::Rect| adv_display::Rect {
            x: (rectangle[0] + f64::from(r.x - from.x) * rectangle[2] / f64::from(from.w)).round()
                as u32,
            y: (rectangle[1] + f64::from(r.y - from.y) * rectangle[3] / f64::from(from.h)).round()
                as u32,
            w: (f64::from(r.w) * rectangle[2] / f64::from(from.w)).round() as u32,
            h: (f64::from(r.h) * rectangle[3] / f64::from(from.h)).round() as u32,
        };
        layers.base.dst = map(layers.base.dst);
        for (_, layer) in &mut layers.video {
            layer.dst = map(layer.dst);
        }
        self.compositor.present(&layers, d, i, s, a)
    }
    fn drain(
        &mut self,
        p: &mut Vec<adv_display::compositor::Presented>,
        r: &mut Vec<adv_display::compositor::ReleasedImage>,
    ) {
        self.compositor.drain(p, r);
    }
}

#[cfg(target_vendor = "apple")]
pub(crate) fn apple_channel(
    device: Device,
    queue: Queue,
    root: objc2::rc::Retained<objc2_quartz_core::CALayer>,
    scale: f64,
    view: (u32, u32),
    wake: WakeEvent,
) -> Result<(Startup, Ui), String> {
    let compositor = adv_display::compositor::LayerCompositor::new(
        root,
        scale,
        queue.main_notifier(),
        platform_caps(Some(&device)),
        objc2::MainThreadMarker::new().unwrap(),
    );
    let apple = Arc::new(Mutex::new(compositor));
    let (startup, mut ui) = channel(
        device,
        queue,
        Box::new(AppleCompositor(apple.clone())),
        view,
        wake,
    )?;
    ui.apple = Some(apple);
    Ok((startup, ui))
}
#[cfg(target_vendor = "apple")]
struct AppleCompositor(Arc<Mutex<adv_display::compositor::LayerCompositor>>);
#[cfg(target_vendor = "apple")]
impl Compositor for AppleCompositor {
    fn caps(&self) -> plan::CompositorCaps {
        self.0.lock().unwrap().caps()
    }
    fn present(
        &mut self,
        l: &plan::Layers,
        d: &plan::Tiles,
        i: Vec<adv_display::compositor::AcquiredImage>,
        s: u64,
        a: Instant,
    ) -> Result<(), adv_display::compositor::PresentError> {
        self.0.lock().unwrap().present(l, d, i, s, a)
    }
    fn drain(
        &mut self,
        p: &mut Vec<adv_display::compositor::Presented>,
        r: &mut Vec<adv_display::compositor::ReleasedImage>,
    ) {
        self.0.lock().unwrap().drain(p, r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            settings: Mutex::new(Settings {
                view: (640, 480),
                scaling: crate::Scaling::Fit,
                upscaler: Upscaler::Off,
                rate: 60,
            }),
            feedback: Mutex::new(VecDeque::new()),
            statistics: Mutex::new(None),
            wake: WakeEvent::new().unwrap(),
            layout: Mutex::new(([0., 0., 640., 480.], crate::Scaling::Fit)),
            #[cfg(target_os = "android")]
            window: Mutex::new(None),
            #[cfg(target_os = "android")]
            caps: platform_caps(None),
            #[cfg(target_os = "android")]
            rate: Arc::new(std::sync::atomic::AtomicU32::new(60)),
        })
    }
    // spec: HOST-VIEW-1, HOST-SES-11
    #[test]
    fn settings_mailbox_reads_the_latest_complete_settings_and_retains_the_filter() {
        let shared = shared();
        let mut ui = Ui {
            shared: shared.clone(),
            #[cfg(target_vendor = "apple")]
            apple: None,
        };
        ui.set_upscaler(Upscaler::Artcnn);
        ui.set_scaling(crate::Scaling::Integer);
        for n in 1..=100 {
            ui.update((n * 8, n * 6), 30);
        }
        let s = shared.settings.lock().unwrap().clone();
        assert_eq!(s.view, (800, 600));
        assert_eq!(s.rate, 30);
        assert_eq!(s.scaling, crate::Scaling::Integer);
        assert_eq!(s.upscaler, Upscaler::Artcnn);
        assert_eq!(s.plan().upscaler, plan::Upscaler::Off);
    }
    // spec: HOST-SES-3, HOST-SES-11
    #[test]
    fn feedback_is_bounded_and_preserves_publication_and_geometry() {
        let shared = shared();
        let mut ui = Ui {
            shared: shared.clone(),
            #[cfg(target_vendor = "apple")]
            apple: None,
        };
        let published_at = Instant::now();
        for serial in 1..=140 {
            shared.send(Ok(Feedback {
                serial,
                published_at,
                canvas: (320, 240),
                placement: Placement {
                    rectangle: [10., 20., 640., 480.],
                    nearest: true,
                },
                cursor_regions: Arc::from([adv_runtime::VideoCursorRegion {
                    rectangle: adv_device::surface::Rect {
                        x: 2,
                        y: 3,
                        w: 4,
                        h: 5,
                    },
                    hide_cursor: true,
                }]),
                stats: Default::default(),
                elapsed: Duration::from_micros(42),
                at: published_at,
            }));
        }
        assert!(shared.wake.wait(Some(Duration::ZERO)).unwrap());
        let first = ui.poll().unwrap().unwrap();
        assert_eq!(first.serial, 13);
        assert_eq!(first.published_at, published_at);
        assert_eq!(first.canvas, (320, 240));
        assert_eq!(first.placement.rectangle, [10., 20., 640., 480.]);
        assert!(first.cursor_regions[0].hide_cursor);
        let mut last = 0;
        while let Some(f) = ui.poll().unwrap() {
            last = f.serial;
        }
        assert_eq!(last, 140);
    }
    // spec: HOST-VIEW-3
    #[test]
    fn presenter_maps_every_software_encoding_and_both_swap_kinds() {
        let (device, mut queue) = open(false).unwrap();
        for f in [
            frame::PixelFormat::Bgra32,
            frame::PixelFormat::Bgr24,
            frame::PixelFormat::Rgb565,
            frame::PixelFormat::Rgb555,
            frame::PixelFormat::Indexed8,
            frame::PixelFormat::Indexed4,
            frame::PixelFormat::Indexed1,
        ] {
            let slot = frame::Slot::new(&device, 2, 2, 8, f).unwrap();
            let id = slot.buffer().desc().size;
            let (mapped, slot, _, _) = map(frame::Frame {
                serial: 5,
                base: frame::Base::Software {
                    slot,
                    format: f,
                    palette: Some(Arc::new([[1, 2, 3, 4]; 256])),
                    damage: frame::Damage::Whole,
                },
                video: Arc::from([]),
                published_at: Instant::now(),
            });
            assert_eq!(mapped.info.canvas, (2, 2));
            assert_eq!(mapped.info.serial, 5);
            assert_eq!(mapped.info.damage, plan::Damage::Whole);
            assert!(slot.is_some());
            let display::Base::Software {
                buffer,
                pitch,
                format: actual,
                palette,
                ..
            } = mapped.base
            else {
                panic!("software")
            };
            assert_eq!(pitch, 8);
            assert_eq!(actual, format(f));
            assert_eq!(buffer.desc().size, id);
            assert_eq!(palette.unwrap()[0], [1, 2, 3, 4]);
        }
        let mut pool = device
            .create_present_pool(&adv_gpu::present::PoolDesc {
                color: None,
                width: 2,
                height: 2,
                format: adv_gpu::Format::Bgra8Unorm,
                count: 1,
                label: "mapping",
            })
            .unwrap();
        let image = pool.acquire(&mut queue).unwrap().unwrap();
        let private = image.image().clone();
        for image in [SwapImage::Private(private), SwapImage::Presentable(image)] {
            let presentable = matches!(image, SwapImage::Presentable(_));
            let (mapped, slot, _, _) = map(frame::Frame {
                serial: 6,
                base: frame::Base::Gpu {
                    image,
                    damage: frame::Damage::None,
                    ready: adv_gpu::Token(7),
                },
                video: Arc::from([]),
                published_at: Instant::now(),
            });
            assert_eq!(mapped.info.base, plan::BaseKind::Gpu { presentable });
            assert_eq!(mapped.info.canvas, (2, 2));
            assert!(slot.is_none());
            if let display::Base::Presentable { ready, .. } = mapped.base {
                assert_eq!(ready, adv_gpu::Token(7));
            }
        }
    }
    // spec: HOST-VIEW-3, MEDIA-VIDEO-7
    #[cfg(feature = "media")]
    #[test]
    fn presenter_maps_changed_and_retained_video_planes_and_cursor_regions() {
        let gpu = headless(false, (64, 48), WakeEvent::new().unwrap())
            .unwrap()
            .start()
            .unwrap();
        let mut decoder = adv_device::media::Decoder::open_memory(
            include_bytes!("../../adv-dx/src/dshow/graph/render/fixtures/guest_movie.mpg").to_vec(),
        )
        .unwrap();
        let picture = decoder.frame_at(100_000).unwrap().video.unwrap();
        let geometry = frame::VideoGeometry {
            source: (picture.width, picture.height),
            destination: adv_gpu::Rect {
                x: 1,
                y: 2,
                w: 20,
                h: 10,
            },
            source_uv: [0.1, 0.2, 0.8, 0.9],
            background: None,
            clip: adv_gpu::Rect {
                x: 2,
                y: 3,
                w: 4,
                h: 5,
            },
            hide_cursor: true,
        };
        let video: Arc<[frame::VideoPlane]> = Arc::from([frame::VideoPlane {
            id: 12,
            geometry,
            picture: picture.picture,
        }]);
        for changed in [true, false] {
            let slot =
                frame::Slot::new(gpu.device(), 64, 48, 256, frame::PixelFormat::Bgra32).unwrap();
            let (mapped, _, _, cursor) = map(frame::Frame {
                serial: 8,
                base: frame::Base::Software {
                    slot,
                    format: frame::PixelFormat::Bgra32,
                    palette: None,
                    damage: frame::Damage::None,
                },
                video: Arc::clone(&video),
                published_at: Instant::now(),
            });
            assert_eq!(mapped.info.video[0].changed, changed);
            assert_eq!(mapped.video[0].update.is_some(), changed);
            assert_eq!(mapped.video[0].geometry.source_uv, geometry.source_uv);
            assert_eq!(
                mapped.video[0].geometry.destination,
                geometry.destination.into()
            );
            assert_eq!(cursor[0].rectangle.w, 4);
            assert!(cursor[0].hide_cursor);
        }
    }
}
