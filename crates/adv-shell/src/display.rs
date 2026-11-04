//! UI mailboxes and the frame boundary adapter; display owns rendering policy.
use adv_device::frame::{self, SwapImage};
use adv_display::{
    compositor::Compositor,
    display::{self, Display},
    plan,
};
use adv_gpu::{Device, Queue};
use adv_sync::WakeEvent;
pub use plan::Upscaler;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    pub rectangle: [f32; 4],
    pub nearest: bool,
}
struct Mailbox {
    settings: plan::Settings,
    dirty: bool,
    window: Option<Option<Box<dyn Compositor>>>,
    wake: Option<frame::Waker>,
}
impl Mailbox {
    fn poke(&self) {
        if let Some(w) = &self.wake {
            w.poke();
        }
    }
}
struct Shared {
    settings: Mutex<Mailbox>,
    feedback: Mutex<VecDeque<Result<Feedback, String>>>,
    wake: WakeEvent,
}
impl Shared {
    fn send(&self, value: Result<Feedback, String>) {
        let mut q = self.feedback.lock().unwrap();
        if q.len() == 128 {
            q.pop_front();
        }
        q.push_back(value);
        drop(q);
        self.wake.signal();
    }
}
pub(crate) struct Presented {
    pub serial: u64,
    pub canvas: (u32, u32),
    pub placement: Placement,
    pub cursor_regions: Arc<[adv_runtime::VideoCursorRegion]>,
    pub elapsed: Duration,
    pub at: Instant,
    pub stats: crate::performance::NativePresentation,
}
pub(crate) enum Feedback {
    Presented(Presented),
    Statistics(crate::performance::NativePresentation),
}
pub struct Ui {
    shared: Arc<Shared>,
    #[cfg(target_os = "android")]
    caps: plan::CompositorCaps,
}
impl Ui {
    pub fn set_upscaler(&mut self, value: Upscaler) {
        let mut m = self.shared.settings.lock().unwrap();
        if m.settings.upscaler != value {
            m.settings.upscaler = value;
            m.dirty = true;
            m.poke();
        }
    }
    pub fn set_scaling(&mut self, value: crate::Scaling) {
        let mut m = self.shared.settings.lock().unwrap();
        if m.settings.placement != value {
            m.settings.placement = value;
            m.dirty = true;
            m.poke();
        }
    }
    pub fn update(&mut self, view: (u32, u32), scale: f64) {
        let mut m = self.shared.settings.lock().unwrap();
        if m.settings.view != view || m.settings.scale != scale {
            m.settings.view = view;
            m.settings.scale = scale;
            m.dirty = true;
            m.poke();
        }
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
            .map(|w| {
                unsafe {
                    adv_display::compositor::android::SurfaceCompositor::new(w, self.caps.clone())
                }
                .map(|c| Box::new(c) as Box<dyn Compositor>)
                .map_err(|e| format!("SurfaceControl: {e:?}"))
            })
            .transpose()?;
        let mut m = self.shared.settings.lock().unwrap();
        m.window = Some(compositor);
        m.poke();
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
        adv_device::gpu::Gpu::request_with_presenter(
            self.device,
            self.queue,
            Box::new(self.presenter),
        )
        .map_err(|e| e.to_string())
    }
}
struct Presenter {
    display: Display,
    shared: Arc<Shared>,
    reported: Option<(u64, u64, bool, bool, bool)>,
}
pub(crate) fn platform_caps(device: Option<&Device>) -> plan::CompositorCaps {
    plan::CompositorCaps {
        fractional_crops: !cfg!(target_os = "android"),
        scales: true,
        nearest: !cfg!(target_os = "android"),
        rotates: cfg!(target_os = "android"),
        yuv_layers: true,
        solid_color_layers: true,
        damage_hints: true,
        presentable_compressed: device.is_some_and(|d| {
            d.caps()
                .format(adv_gpu::present::COLOR_FORMAT)
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
    compositor: Box<dyn Compositor>,
    view: (u32, u32),
    wake: WakeEvent,
) -> Result<(Startup, Ui), String> {
    let settings = plan::Settings {
        view,
        rotation: plan::Rotation::None,
        placement: plan::Placement::Fit,
        scale: 1.,
        upscaler: Upscaler::Off,
        background: [0.; 3],
    };
    #[cfg(target_os = "android")]
    let caps = compositor.caps();
    let display =
        Display::new(device.clone(), compositor, settings.clone()).map_err(|e| e.to_string())?;
    let shared = Arc::new(Shared {
        settings: Mutex::new(Mailbox {
            settings,
            dirty: true,
            window: None,
            wake: None,
        }),
        feedback: Mutex::new(VecDeque::new()),
        wake,
    });
    Ok((
        Startup {
            device,
            queue,
            presenter: Presenter {
                display,
                shared: shared.clone(),
                reported: None,
            },
        },
        Ui {
            shared,
            #[cfg(target_os = "android")]
            caps,
        },
    ))
}
#[cfg(any(target_os = "macos", test))]
pub(crate) fn headless(
    software: bool,
    view: (u32, u32),
    wake: WakeEvent,
) -> Result<Startup, String> {
    let (device, queue) = open(software)?;
    let c = Box::new(adv_display::compositor::HeadlessCompositor::new(
        platform_caps(Some(&device)),
    ));
    Ok(channel(device, queue, c, view, wake)?.0)
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
    let c = adv_display::compositor::LayerCompositor::new(
        root,
        scale,
        queue.main_notifier(),
        platform_caps(Some(&device)),
        objc2::MainThreadMarker::new().unwrap(),
    );
    let (startup, mut ui) = channel(device, queue, Box::new(c), view, wake)?;
    ui.update(view, scale);
    Ok((startup, ui))
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
fn map(frame: frame::Frame) -> (display::Frame, Option<frame::Slot>) {
    let (canvas, base, kind, changed, slot) = match frame.base {
        frame::Base::Gpu {
            image,
            damage: d,
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
                damage(&d),
                None,
            )
        }
        frame::Base::Software {
            slot,
            format: f,
            palette,
            damage: d,
        } => {
            let canvas = slot.extent();
            let base = display::Base::Software {
                buffer: slot.buffer().clone(),
                pitch: slot.pitch(),
                extent: canvas,
                format: format(f),
                palette,
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
                damage(&d),
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
    let video = frame
        .video
        .iter()
        .map(|v| {
            let update = match v.take_picture() {
                frame::VideoUpdate::Changed(i) => Some(i),
                frame::VideoUpdate::Retained => None,
            };
            let g = v.geometry;
            info.video.push(plan::VideoGeometry {
                id: v.id,
                source: g.source,
                rect: g.destination.into(),
                changed: update.is_some(),
                source_uv: Some(g.source_uv),
                background: g.background.map(|(r, c)| (r.into(), c)),
            });
            display::Video {
                id: v.id,
                image: v.picture.image().clone(),
                planes: (v.picture.image().desc().format == adv_gpu::Format::Nv12)
                    .then(|| v.picture.planes().clone()),
                geometry: adv_display::stages::video::Geometry {
                    destination: g.destination.into(),
                    source_uv: g.source_uv,
                    background: g.background.map(|(r, c)| (r.into(), c)),
                    color: v.picture.color(),
                },
                hide_cursor: g.hide_cursor,
                update,
            }
        })
        .collect();
    (
        display::Frame {
            info,
            base,
            video,
            published_at: frame.published_at,
        },
        slot,
    )
}
impl Presenter {
    fn update(&mut self, queue: &mut Queue) -> Result<(), String> {
        let (settings, window) = {
            let mut m = self.shared.settings.lock().unwrap();
            let settings = std::mem::take(&mut m.dirty).then(|| m.settings.clone());
            (settings, m.window.take())
        };
        if let Some(window) = window {
            match window {
                Some(c) => self
                    .display
                    .rebuild(c, queue)
                    .map_err(|e| format!("display rebuild: {e:?}"))?,
                None => self.display.suspend(),
            }
        }
        if let Some(settings) = settings {
            self.display
                .settings(queue, settings)
                .map_err(|e| format!("display settings: {e:?}"))?;
        }
        Ok(())
    }
    fn feedback(&mut self) {
        let publication = self.display.take_publication();
        if let Some(p) = publication {
            let record = self.display.recorder().get(p.presentation).unwrap();
            let totals = self.display.totals();
            let stats = statistics(record, totals, self.display.origin());
            self.reported = Some(stamp(record, totals));
            self.shared.send(Ok(Feedback::Presented(Presented {
                serial: p.serial,
                canvas: p.canvas,
                placement: Placement {
                    rectangle: [
                        p.placement.x as f32,
                        p.placement.y as f32,
                        p.placement.w as f32,
                        p.placement.h as f32,
                    ],
                    nearest: p.nearest,
                },
                cursor_regions: p
                    .cursor_regions
                    .into_iter()
                    .map(|(r, hide_cursor)| adv_runtime::VideoCursorRegion {
                        rectangle: adv_device::surface::Rect {
                            x: r.x,
                            y: r.y,
                            w: r.w,
                            h: r.h,
                        },
                        hide_cursor,
                    })
                    .collect::<Vec<_>>()
                    .into(),
                elapsed: record.cpu_time,
                at: Instant::now(),
                stats,
            })));
        }
        let Some(record) = self.display.recorder().latest() else {
            return;
        };
        let totals = self.display.totals();
        let stamp = stamp(record, totals);
        if self.reported != Some(stamp) {
            self.reported = Some(stamp);
            self.shared.send(Ok(Feedback::Statistics(statistics(
                record,
                totals,
                self.display.origin(),
            ))));
        }
    }
}
fn stamp(
    record: &adv_display::recorder::PresentRecord,
    totals: &adv_display::recorder::Totals,
) -> (u64, u64, bool, bool, bool) {
    (
        totals.presents,
        totals.configurations,
        record.statistics.latched.is_some(),
        record.statistics.shown.is_some(),
        record.stage_times.iter().any(|d| !d.is_zero()),
    )
}
fn statistics(
    r: &adv_display::recorder::PresentRecord,
    t: &adv_display::recorder::Totals,
    origin: Instant,
) -> crate::performance::NativePresentation {
    let s = r.statistics;
    let mut stats = crate::performance::NativePresentation {
        consumed: r.frame.serial,
        uploads: t.software_frames,
        presented: t.presents,
        direct_presentations: t.direct_presents,
        gpu_transfer_bytes: t.software_bytes,
        software_started: t.software_started.map(|at| origin + at),
        configurations: t.configurations,
        max_queue_ns: t.max_queue.as_nanos() as u64,
        display_plan: match s.kind {
            adv_display::recorder::PlanKind::Direct => "direct",
            adv_display::recorder::PlanKind::Merged => "merged",
            adv_display::recorder::PlanKind::Upscaled => "upscaled",
        },
        display_passes: s.passes as u64,
        display_submissions: s.submissions as u64,
        display_latch_ns: s.latched.map(|d| d.saturating_sub(r.at).as_nanos() as u64),
        display_shown_ns: s.shown.map(|d| d.saturating_sub(r.at).as_nanos() as u64),
        max_job_ns: t.max_cpu.as_nanos() as u64,
        ..Default::default()
    };
    for (i, stage) in r.plan.stages.iter().enumerate() {
        let index = match stage {
            plan::Stage::Ingest { .. } => 0,
            plan::Stage::Video { .. } => 1,
            plan::Stage::Output { .. } => 2,
            _ => unreachable!(),
        };
        stats.display_stage_tiles[index] += s.stages[i].tiles;
        stats.display_stage_pixels[index] += s.stages[i].pixels as u32;
        stats.display_stage_ns[index] += r.stage_times[i].as_nanos() as u64;
    }
    stats
}
impl frame::Presenter for Presenter {
    fn presentable(&self, width: u32, height: u32) -> bool {
        self.display.presentable((width, height))
    }
    fn present(&mut self, queue: &mut Queue, frame: frame::Frame) {
        if let Err(e) = self.update(queue) {
            self.shared.send(Err(e));
            return;
        }
        let (frame, slot) = map(frame);
        let result = self.display.present(queue, frame);
        let token = match &result {
            Ok(t) => *t,
            Err(e) => e.token,
        };
        if token.0 != 0
            && let Some(slot) = slot
        {
            slot.ingested(token);
        }
        if let Err(e) = result {
            self.shared
                .send(Err(format!("display present: {:?}", e.error)));
        }
        self.feedback();
    }
    fn service(&mut self, queue: &mut Queue, wake: Option<&frame::Waker>) {
        if let Some(wake) = wake {
            let mut m = self.shared.settings.lock().unwrap();
            if m.wake.is_none() {
                m.wake = Some(wake.clone());
                let w = wake.clone();
                self.display.set_waker(Arc::new(move || w.poke()));
            }
        }
        let result = if wake.is_some() {
            self.update(queue)
        } else {
            Ok(())
        }
        .and_then(|()| {
            self.display
                .service(queue)
                .map_err(|e| format!("display service: {e:?}"))
        });
        if let Err(e) = result {
            self.shared.send(Err(e));
        }
        for reply in wake.map_or_else(Vec::new, |w| w.captures()) {
            self.display.capture(
                queue,
                Box::new(move |result| {
                    let _ = reply.send(
                        result
                            .map(|c| frame::Captured {
                                serial: c.serial,
                                width: c.width,
                                height: c.height,
                                bgra: c.bgra,
                            })
                            .map_err(|e| format!("capture: {e:?}")),
                    );
                }),
            );
        }
        self.feedback();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            settings: Mutex::new(Mailbox {
                settings: plan::Settings {
                    view: (640, 480),
                    rotation: plan::Rotation::None,
                    placement: crate::Scaling::Fit,
                    scale: 1.,
                    upscaler: Upscaler::Off,
                    background: [0.; 3],
                },
                dirty: true,
                window: None,
                wake: None,
            }),
            feedback: Mutex::new(VecDeque::new()),
            wake: WakeEvent::new().unwrap(),
        })
    }
    fn ui(shared: Arc<Shared>) -> Ui {
        Ui {
            shared,
            #[cfg(target_os = "android")]
            caps: platform_caps(None),
        }
    }
    // spec: HOST-VIEW-1
    #[test]
    fn settings_mailbox_reads_the_latest_complete_settings_and_retains_the_filter() {
        let shared = shared();
        let mut ui = ui(shared.clone());
        ui.set_upscaler(Upscaler::Artcnn);
        ui.set_scaling(crate::Scaling::Integer);
        for n in 1..=100 {
            ui.update((n * 8, n * 6), 1.);
        }
        let s = shared.settings.lock().unwrap().settings.clone();
        assert_eq!(s.view, (800, 600));
        assert_eq!(s.placement, crate::Scaling::Integer);
        assert_eq!(s.upscaler, Upscaler::Artcnn);
        let (device, mut queue) = open(false).unwrap();
        let (mut startup, _) = channel(
            device,
            queue,
            Box::new(adv_display::compositor::HeadlessCompositor::new(
                platform_caps(None),
            )),
            (640, 480),
            WakeEvent::new().unwrap(),
        )
        .unwrap();
        startup.presenter.shared = shared;
        queue = startup.queue;
        startup.presenter.update(&mut queue).unwrap();
        assert!(startup.presenter.display.presentable((640, 480)));
    }
    // spec: HOST-SES-3
    #[test]
    fn feedback_is_bounded_and_preserves_publication_and_geometry() {
        let shared = shared();
        let mut ui = ui(shared.clone());
        let published_at = Instant::now();
        for serial in 1..=140 {
            shared.send(Ok(Feedback::Presented(Presented {
                serial,
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
            })));
        }
        assert!(shared.wake.wait(Some(Duration::ZERO)).unwrap());
        let Feedback::Presented(first) = ui.poll().unwrap().unwrap() else {
            panic!("presented");
        };
        assert_eq!(first.serial, 13);
        assert_eq!(first.canvas, (320, 240));
        assert_eq!(first.placement.rectangle, [10., 20., 640., 480.]);
        assert!(first.cursor_regions[0].hide_cursor);
        let mut last = 0;
        while let Some(Feedback::Presented(f)) = ui.poll().unwrap() {
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
            let (mapped, slot) = map(frame::Frame {
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
                format: adv_gpu::present::COLOR_FORMAT,
                count: 1,
                storage: false,
                label: "mapping",
            })
            .unwrap();
        let image = pool.acquire(&mut queue).unwrap().unwrap();
        let private = image.image().clone();
        for image in [SwapImage::Private(private), SwapImage::Presentable(image)] {
            let presentable = matches!(image, SwapImage::Presentable(_));
            let (mapped, slot) = map(frame::Frame {
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
            let (mapped, _) = map(frame::Frame {
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
            assert_eq!(mapped.info.video[0].rect, geometry.destination.into());
            assert_eq!(mapped.info.video[0].source_uv, Some(geometry.source_uv));
            assert_eq!(mapped.info.video[0].background, None);
            assert!(mapped.video[0].hide_cursor);
        }
    }
    // spec: AND-APP-10
    #[test]
    fn owner_services_settings_and_window_pokes_without_a_guest_frame() {
        use adv_display::compositor::*;
        struct Probe {
            inner: HeadlessCompositor,
            send: std::sync::mpsc::Sender<&'static str>,
        }
        impl Compositor for Probe {
            fn caps(&self) -> plan::CompositorCaps {
                self.inner.caps()
            }
            fn set_scale(&mut self, scale: f64) {
                if scale == 2. {
                    self.send.send("scale").unwrap();
                }
            }
            fn detach(&mut self) -> Option<adv_gpu::queue::Fence> {
                self.send.send("detached").unwrap();
                self.inner.detach()
            }
            fn present(
                &mut self,
                l: &plan::Layers,
                d: &plan::Tiles,
                i: &mut [AcquiredImage],
                s: u64,
                a: Instant,
            ) -> Result<(), Error> {
                self.inner.present(l, d, i, s, a)
            }
            fn drain(&mut self, p: &mut Vec<Presented>, r: &mut Vec<Released>) {
                self.inner.drain(p, r);
            }
        }
        let (send, received) = std::sync::mpsc::channel();
        let make = || {
            Box::new(Probe {
                inner: HeadlessCompositor::new(platform_caps(None)),
                send: send.clone(),
            }) as Box<dyn Compositor>
        };
        let (device, queue) = open(false).unwrap();
        let (startup, mut ui) =
            channel(device, queue, make(), (64, 48), WakeEvent::new().unwrap()).unwrap();
        let gpu = startup.start().unwrap();
        ui.update((128, 96), 2.);
        assert_eq!(
            received.recv_timeout(Duration::from_secs(10)).unwrap(),
            "scale"
        );
        {
            let mut m = ui.shared.settings.lock().unwrap();
            m.window = Some(None);
            m.poke();
        }
        assert_eq!(
            received.recv_timeout(Duration::from_secs(10)).unwrap(),
            "detached"
        );
        {
            let mut m = ui.shared.settings.lock().unwrap();
            m.window = Some(Some(make()));
            m.poke();
        }
        assert_eq!(
            received.recv_timeout(Duration::from_secs(10)).unwrap(),
            "scale"
        );
        drop(gpu);
    }
}
