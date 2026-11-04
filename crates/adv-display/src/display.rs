//! One queue owns planning, target passes, presentation and its flight recorder.
use crate::{
    Rect,
    compositor::*,
    plan::*,
    recorder::{self, PlanKind, PresentRecord, StageStats, Statistics, Totals},
    stages::{ingest, output, upscale, video},
};
use adv_gpu::{
    Device, Format, Image, ImageView, PresentImage, PresentPool, Queue, RenderPipeline, Ring,
    Sampler, Span, Token,
};
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

pub enum Base {
    Presentable {
        image: PresentImage,
        ready: Token,
    },
    Private(Image),
    Software {
        buffer: adv_gpu::Buffer,
        pitch: u32,
        extent: (u32, u32),
        format: ingest::PixelFormat,
        palette: Option<std::sync::Arc<[[u8; 4]; 256]>>,
    },
    Retained {
        image: Image,
        canvas: (u32, u32),
        rotation: Rotation,
    },
}
impl Base {
    fn source(&self) -> output::Source<'_> {
        match self {
            Self::Presentable { image, .. } => output::Source::Image(image.image().view()),
            Self::Private(i) => output::Source::Image(i.view()),
            Self::Retained {
                image,
                canvas,
                rotation,
            } => output::Source::Retained {
                view: image.view(),
                canvas: *canvas,
                rotation: *rotation,
            },
            Self::Software {
                buffer,
                pitch,
                extent,
                format,
                palette,
            } => output::Source::Software(ingest::Source {
                buffer,
                pitch: *pitch,
                extent: *extent,
                format: *format,
                palette: palette.as_deref(),
            }),
        }
    }
}
pub struct Video {
    pub id: u64,
    pub image: Image,
    pub planes: Option<[ImageView; 2]>,
    pub geometry: video::Geometry,
    pub hide_cursor: bool,
    pub update: Option<PresentImage>,
}
impl Video {
    fn retained(&self) -> Self {
        Self {
            id: self.id,
            image: self.image.clone(),
            planes: self.planes.clone(),
            geometry: self.geometry,
            hide_cursor: self.hide_cursor,
            update: None,
        }
    }
}
pub struct Frame {
    pub info: FrameInfo,
    pub base: Base,
    pub video: Vec<Video>,
    pub published_at: Instant,
}
#[derive(Debug)]
pub enum Error {
    Gpu(adv_gpu::Error),
    Compositor(crate::compositor::Error),
}
impl From<adv_gpu::Error> for Error {
    fn from(e: adv_gpu::Error) -> Self {
        Self::Gpu(e)
    }
}
#[derive(Debug)]
pub struct Failure {
    pub error: Error,
    pub token: Token,
}
pub struct Publication {
    pub presentation: u64,
    pub serial: u64,
    pub published_at: Instant,
    pub canvas: (u32, u32),
    pub placement: Rect,
    pub nearest: bool,
    pub cursor_regions: Vec<(Rect, bool)>,
}
struct Pending {
    record: PresentRecord,
    images: Vec<AcquiredImage>,
    video: Vec<Video>,
    /// The last submission that reads the frame's base, or `Token(0)`.
    read: Token,
}
struct Converted {
    pool: adv_gpu::present::PoolWeak,
    pipeline: RenderPipeline,
}
struct Timing {
    token: Token,
    buffer: adv_gpu::Buffer,
    stages: Vec<usize>,
}
struct Pipelines {
    ingest: RenderPipeline,
    output: RenderPipeline,
    software: RenderPipeline,
    video: RenderPipeline,
    software_video: RenderPipeline,
    sampler: Sampler,
}
impl Upscaled {
    fn new(device: &Device, choice: Upscaler, canvas: (u32, u32)) -> adv_gpu::Result<Self> {
        let upscaling = upscale::Upscaling::new(device, upscale::model(device, choice), canvas)?;
        let output = device.create_image(&adv_gpu::resource::ImageDesc {
            dimension: adv_gpu::resource::Dimension::D2,
            size: adv_gpu::Extent {
                width: canvas.0 * 2,
                height: canvas.1 * 2,
                depth_or_layers: 1,
            },
            format: upscale::OUTPUT,
            mips: 1,
            samples: 1,
            usage: adv_gpu::resource::Usage::STORAGE | adv_gpu::resource::Usage::SAMPLED,
            view_formats: &[],
            label: "upscaled",
        })?;
        Ok(Self {
            choice,
            upscaling,
            output,
            pipelines: Pipelines::new(device, upscale::INPUT)?,
        })
    }
}
impl Pipelines {
    fn new(device: &Device, format: Format) -> adv_gpu::Result<Self> {
        let (video, sampler) = video::resources(device, format)?;
        Ok(Self {
            software_video: video::software_resources(device, format)?.0,
            ingest: ingest::pipeline(device, format)?,
            output: output::pipeline(device, format, false)?,
            software: output::pipeline(device, format, true)?,
            video,
            sampler,
        })
    }
}
/// A model build: its choice and canvas, and the result once sent. A failed
/// build stays here, so the view stays unscaled instead of retrying.
struct Building {
    key: (Upscaler, (u32, u32)),
    result: std::sync::mpsc::Receiver<adv_gpu::Result<Upscaled>>,
    failed: bool,
}
/// The selected model at one canvas size and the doubled image it keeps
/// complete, which the output pass reads.
struct Upscaled {
    choice: Upscaler,
    upscaling: upscale::Upscaling,
    output: Image,
    /// Composition into the model's half-precision input.
    pipelines: Pipelines,
}
pub struct Display {
    device: Device,
    compositor: Option<Box<dyn Compositor>>,
    /// Every attached image with the serial that attached it, until the
    /// compositor releases it. Declared after the compositor, which detaches
    /// before these drop.
    screen: Vec<(u64, AcquiredImage)>,
    caps: CompositorCaps,
    settings: Settings,
    pools: PoolState,
    allocations: HashMap<PoolId, PresentPool>,
    pool_ids: HashMap<usize, PoolId>,
    pipelines: Pipelines,
    /// Keyed by decoder pool, software base and target format.
    converted: HashMap<(usize, bool, Format), Converted>,
    upscaled: Option<Upscaled>,
    /// The model being built off the owner thread, for this choice and canvas.
    building: Option<Building>,
    /// Each video's current decoder image while it is not attached.
    leases: HashMap<u64, PresentImage>,
    ring: Ring,
    spans: Vec<Span>,
    recorder: recorder::Recorder,
    totals: Totals,
    origin: Instant,
    wake: Option<Wake>,
    unlatched: Option<u64>,
    held: Option<Pending>,
    video: Vec<Video>,
    publication: Option<Publication>,
    queries: Option<adv_gpu::queue::QuerySet>,
    timings: VecDeque<Timing>,
    readbacks: Vec<adv_gpu::Buffer>,
    invalid: bool,
}
impl Display {
    pub fn new(
        device: Device,
        compositor: Box<dyn Compositor>,
        settings: Settings,
    ) -> adv_gpu::Result<Self> {
        let queries = if device
            .caps()
            .features
            .contains(adv_gpu::Features::TIMESTAMPS)
        {
            Some(device.create_query_set(adv_gpu::queue::QueryKind::Timestamp, 8)?)
        } else {
            None
        };
        let ring = device.ring(4 << 20)?;
        let pipelines = Pipelines::new(&device, adv_gpu::present::COLOR_FORMAT)?;
        let mut this = Self {
            caps: compositor.caps(),
            compositor: Some(compositor),
            screen: vec![],
            device,
            settings,
            pools: Default::default(),
            allocations: HashMap::new(),
            pool_ids: HashMap::new(),
            pipelines,
            converted: HashMap::new(),
            upscaled: None,
            building: None,
            leases: HashMap::new(),
            ring,
            spans: vec![],
            recorder: recorder::Recorder::present_history(),
            totals: Totals::default(),
            origin: Instant::now(),
            wake: None,
            unlatched: None,
            held: None,
            video: vec![],
            publication: None,
            queries,
            timings: VecDeque::new(),
            readbacks: vec![],
            invalid: true,
        };
        this.configure();
        Ok(this)
    }
    fn configure(&mut self) {
        self.totals.configurations += 1;
        if let Some(c) = &mut self.compositor {
            c.set_scale(self.settings.scale);
            c.set_frame_rate(self.settings.rate);
        }
    }
    pub fn set_waker(&mut self, wake: Wake) {
        self.compositor.as_mut().unwrap().set_waker(wake.clone());
        self.wake = Some(wake);
    }
    pub fn origin(&self) -> Instant {
        self.origin
    }
    pub fn recorder(&self) -> &recorder::Recorder {
        &self.recorder
    }
    pub fn totals(&self) -> &Totals {
        &self.totals
    }
    pub fn ledger(&self) -> &PoolState {
        &self.pools
    }
    /// Whether the selected model is installed, taking a finished build.
    #[cfg(test)]
    pub(crate) fn upscaler_ready(&mut self) -> bool {
        self.install() || self.upscaled.is_some()
    }
    /// The base image the compositor shows last.
    #[cfg(test)]
    pub(crate) fn shown_base(&self) -> Image {
        shown(&self.screen, LayerId::Base).unwrap().image().clone()
    }
    pub fn take_publication(&mut self) -> Option<Publication> {
        self.publication.take()
    }
    pub fn settings(&mut self, queue: &mut Queue, settings: Settings) -> Result<(), Error> {
        if self.settings == settings {
            return Ok(());
        }
        let redraw = self.settings.view != settings.view
            || self.settings.rotation != settings.rotation
            || self.settings.placement != settings.placement
            || self.settings.upscaler != settings.upscaler
            || self.settings.background != settings.background;
        if let Some(c) = &mut self.compositor {
            if self.settings.scale != settings.scale {
                c.set_scale(settings.scale);
            }
            if self.settings.rate != settings.rate {
                c.set_frame_rate(settings.rate);
            }
        }
        self.settings = settings;
        if redraw {
            self.pools.invalidate();
            self.invalid = true;
        }
        if redraw && let Some(frame) = self.redraw_frame() {
            self.present(queue, frame).map_err(|e| e.error)?;
        }
        Ok(())
    }
    fn redraw_frame(&self) -> Option<Frame> {
        let (record, image, video) = if let Some(held) = &self.held {
            let base = held.images.iter().find(|i| i.layer == LayerId::Base);
            (&held.record, base.map(|i| &i.image), &held.video)
        } else {
            (self.recorder.latest()?, None, &self.video)
        };
        let image = image
            .or_else(|| shown(&self.screen, LayerId::Base))
            .unwrap();
        let mut info = record.frame.clone();
        info.base = BaseKind::Gpu { presentable: false };
        info.damage = Damage::Whole;
        Some(Frame {
            info,
            base: Base::Retained {
                image: image.image().clone(),
                canvas: record.frame.canvas,
                rotation: buffer_rotation(record),
            },
            video: video.iter().map(Video::retained).collect(),
            published_at: self.origin + record.published_at,
        })
    }
    pub fn presentable(&self, canvas: (u32, u32)) -> bool {
        let info = FrameInfo {
            serial: 0,
            canvas,
            base: BaseKind::Gpu { presentable: true },
            damage: Damage::None,
            video: Default::default(),
            cursor_regions: Default::default(),
        };
        matches!(
            plan(&info, &self.settings, &self.caps, &self.pools).swap_role,
            SwapRole::Presentable { .. }
        )
    }
    pub fn suspend(&mut self) {
        let Some(mut compositor) = self.compositor.take() else {
            return;
        };
        let fence = compositor.detach();
        drop(compositor);
        // The latest present's images stay leased for the next compositor;
        // everything else is free once the detach fence signals.
        let latest = self
            .recorder
            .latest()
            .map(|r| (r.statistics.presents, &r.plan.layers));
        let current: Vec<_> = self
            .screen
            .iter()
            .filter(|(_, a)| match (a.layer, latest) {
                (LayerId::Base, Some(_)) => true,
                (LayerId::Video(id), Some((_, layers))) => {
                    layers.video.iter().any(|(v, _)| *v == id)
                }
                _ => false,
            })
            .map(|(serial, a)| (a.layer, *serial))
            .fold(
                Vec::<(LayerId, u64)>::new(),
                |mut newest, (layer, serial)| {
                    match newest.iter_mut().find(|(l, _)| *l == layer) {
                        Some(n) => n.1 = n.1.max(serial),
                        None => newest.push((layer, serial)),
                    }
                    newest
                },
            );
        for (serial, mut acquired) in std::mem::take(&mut self.screen) {
            let fence = fence
                .as_ref()
                .map(adv_gpu::queue::Fence::duplicate)
                .transpose()
                .expect("detach fence duplication");
            if current.contains(&(acquired.layer, serial)) {
                acquired
                    .image
                    .detached(fence)
                    .expect("detach fence duplication");
                self.screen.push((serial, acquired));
            } else {
                self.retire(acquired, fence)
                    .expect("detach fence duplication");
            }
        }
        self.unlatched = None;
        self.recorder
            .record(recorder::Event::Lifecycle("surface detached".into()));
    }
    pub fn rebuild(
        &mut self,
        compositor: Box<dyn Compositor>,
        queue: &mut Queue,
    ) -> Result<(), Error> {
        self.suspend();
        self.caps = compositor.caps();
        self.compositor = Some(compositor);
        self.configure();
        let compositor = self.compositor.as_mut().unwrap();
        if let Some(w) = &self.wake {
            compositor.set_waker(w.clone());
        }
        if let Some(record) = self.recorder.latest() {
            let serial = record.statistics.presents;
            let mut images: Vec<_> = self.screen.drain(..).map(|(_, a)| a).collect();
            let result = compositor.present(
                &record.plan.layers,
                &Tiles::default(),
                &mut images,
                serial,
                Instant::now(),
            );
            // On failure they wait for the next compositor.
            self.screen.extend(images.into_iter().map(|a| (serial, a)));
            result.map_err(Error::Compositor)?;
            self.unlatched = Some(serial);
        }
        self.recorder
            .record(recorder::Event::Lifecycle("surface attached".into()));
        self.service(queue)?;
        Ok(())
    }
    fn release(&mut self, image: PresentImage, fence: Option<adv_gpu::queue::Fence>) {
        if let Some(&id) = self.pool_ids.get(&image.pool().id()) {
            self.recorder.record(recorder::Event::Released {
                pool: id.0,
                buffer: image.index(),
            });
        }
        image.released(fence);
    }
    /// Return a released image to its pool, or keep it while it is still its
    /// video's current image.
    fn retire(
        &mut self,
        acquired: AcquiredImage,
        fence: Option<adv_gpu::queue::Fence>,
    ) -> adv_gpu::Result<()> {
        if let LayerId::Video(id) = acquired.layer
            && !self.leases.contains_key(&id)
            && self
                .video
                .iter()
                .any(|v| v.id == id && v.image.id() == acquired.image.image().id())
        {
            let mut image = acquired.image;
            image.detached(fence)?;
            self.leases.insert(id, image);
        } else {
            self.release(acquired.image, fence);
        }
        Ok(())
    }
    fn discard(&mut self, pending: Pending, source: Option<u64>) -> Option<PresentImage> {
        let mut reading = None;
        for acquired in pending.images {
            if let LayerId::Video(id) = acquired.layer {
                self.leases.insert(id, acquired.image);
            } else if source == Some(acquired.image.image().id()) {
                reading = Some(acquired.image);
            } else {
                self.release(acquired.image, None);
            }
        }
        reading
    }
    fn attachable(&self) -> bool {
        self.compositor.is_some()
            && self.unlatched.is_none()
            && self.settings.view.0 != 0
            && self.settings.view.1 != 0
    }
    pub fn present(&mut self, queue: &mut Queue, mut frame: Frame) -> Result<Token, Failure> {
        self.drain(queue).map_err(|error| Failure {
            error,
            token: Token(0),
        })?;
        let source = if let Base::Retained { image, .. } = &frame.base {
            Some(image.id())
        } else {
            None
        };
        // A redraw resamples the shown image; the next frame replaces it whole.
        let redraw = source.is_some();
        let reading = self.held.take().and_then(|held| {
            frame.info.damage = frame.info.damage.union(&held.record.plan.damage);
            self.discard(held, source)
        });
        if self.invalid {
            frame.info.damage = Damage::Whole;
        }
        let result = self.encode(queue, frame, reading.as_ref());
        if let Some(image) = reading {
            self.release(image, None);
        }
        let pending = result.map_err(|error| Failure {
            error,
            token: Token(0),
        })?;
        let token = pending.read;
        if self.attachable() {
            self.attach(pending)
                .map_err(|error| Failure { error, token })?;
        } else {
            self.held = Some(pending);
        }
        self.invalid |= redraw;
        Ok(token)
    }
    fn attach(&mut self, mut pending: Pending) -> Result<(), Error> {
        let serial = self.totals.presents + 1;
        let presented = self.compositor.as_mut().unwrap().present(
            &pending.record.plan.layers,
            &Tiles::default(),
            &mut pending.images,
            serial,
            Instant::now(),
        );
        if let Err(error) = presented {
            self.discard(pending, None);
            return Err(Error::Compositor(error));
        }
        let record = &mut pending.record;
        record.statistics.presents = serial;
        let software =
            matches!(record.frame.base, BaseKind::Software { .. }) && pending.read != Token(0);
        let totals = &mut self.totals;
        totals.presents = serial;
        if software {
            totals.software_frames += 1;
            totals.software_bytes += record.statistics.stages[0].pixels * 4;
            totals.software_started.get_or_insert(record.at);
        }
        totals.direct_presents += u64::from(record.plan.stages.is_empty());
        totals.max_cpu = totals.max_cpu.max(record.cpu_time);
        totals.max_queue = totals
            .max_queue
            .max(record.at.saturating_sub(record.published_at));
        self.screen
            .extend(pending.images.into_iter().map(|a| (serial, a)));
        self.pools.after_present(&pending.record.plan);
        self.video = pending.video;
        self.unlatched = Some(serial);
        self.invalid = false;
        self.recorder
            .record(recorder::Event::Present(Box::new(pending.record)));
        Ok(())
    }
    fn drain(&mut self, queue: &mut Queue) -> Result<(), Error> {
        let mut shown = vec![];
        let mut released = vec![];
        if let Some(c) = &mut self.compositor {
            c.drain(&mut shown, &mut released);
        }
        for p in shown {
            let current = self.unlatched == Some(p.serial);
            if current {
                self.unlatched = None;
            }
            if let Some(r) = self.recorder.get_mut(p.serial) {
                r.statistics.latched = p
                    .latched
                    .map(|i| i.saturating_duration_since(self.origin))
                    .or(r.statistics.latched);
                r.statistics.shown = p
                    .shown
                    .map(|i| i.saturating_duration_since(self.origin))
                    .or(r.statistics.shown);
                if current && p.latched.is_some() {
                    self.publication = Some(Publication {
                        presentation: p.serial,
                        serial: r.frame.serial,
                        published_at: self.origin + r.published_at,
                        canvas: r.frame.canvas,
                        placement: r.plan.layers.base.dst,
                        nearest: r.plan.layers.base.nearest,
                        cursor_regions: self
                            .video
                            .iter()
                            .filter_map(|v| {
                                v.geometry
                                    .background
                                    .map(|(r, _)| r)
                                    .or((v.geometry.destination.w > 0
                                        && v.geometry.destination.h > 0)
                                        .then_some(v.geometry.destination))
                                    .map(|r| (r, v.hide_cursor))
                            })
                            .collect(),
                    });
                }
            }
        }
        for r in released {
            let i = self
                .screen
                .iter()
                .position(|(serial, a)| *serial == r.serial && a.layer == r.layer)
                .expect("released image is attached");
            let (_, acquired) = self.screen.swap_remove(i);
            self.retire(acquired, r.fence)?;
        }
        if !self.timings.is_empty() {
            let completed = queue.poll()?;
            while self.timings.front().is_some_and(|t| t.token <= completed) {
                let t = self.timings.pop_front().unwrap();
                let bytes = unsafe { t.buffer.mapped().unwrap().as_ref() };
                if let Some(record) = self.recorder.submission_mut(t.token.0).or_else(|| {
                    self.held
                        .as_mut()
                        .filter(|h| h.record.token == t.token.0)
                        .map(|h| &mut h.record)
                }) {
                    for (pair, &stage) in t.stages.iter().enumerate() {
                        let start =
                            u64::from_ne_bytes(bytes[pair * 16..pair * 16 + 8].try_into().unwrap());
                        let end = u64::from_ne_bytes(
                            bytes[pair * 16 + 8..pair * 16 + 16].try_into().unwrap(),
                        );
                        record.stage_times[stage] = Duration::from_nanos(
                            ((end - start) as f64 * self.queries.as_ref().unwrap().period()) as u64,
                        );
                    }
                }
                self.readbacks.push(t.buffer);
            }
        }
        self.converted.retain(|_, v| v.pool.alive());
        Ok(())
    }
    pub fn service(&mut self, queue: &mut Queue) -> Result<(), Error> {
        if self.install()
            && let Some(frame) = self.redraw_frame()
        {
            self.present(queue, frame).map_err(|e| e.error)?;
        }
        self.drain(queue)?;
        if self.attachable()
            && let Some(p) = self.held.take()
        {
            self.attach(p)?;
        }
        Ok(())
    }
    fn video_pipeline(&mut self, v: &Video, software: bool, format: Format) -> adv_gpu::Result<()> {
        if v.planes.is_some() {
            return Ok(());
        }
        let pool = video_image(&self.leases, &self.screen, v.id).pool();
        let key = (pool.id(), software, format);
        if !self.converted.contains_key(&key) {
            let pipeline = if software {
                video::converted_software_resources(&self.device, format, pool)?
            } else {
                video::converted_resources(&self.device, format, pool)?
            };
            let pool = pool.weak();
            self.converted.insert(key, Converted { pool, pipeline });
        }
        Ok(())
    }
    /// Draw the base and `videos` into `target` within `tiles`, decoding a
    /// software base inline with the first video.
    fn compose(
        &mut self,
        pass: &mut adv_gpu::RenderPass,
        target: &Image,
        base: &Base,
        videos: &[Video],
        tiles: &Tiles,
    ) -> adv_gpu::Result<()> {
        let size = target.desc().size;
        let format = target.desc().format;
        let software = matches!(base, Base::Software { .. });
        if !software || videos.is_empty() {
            let pipelines = match &self.upscaled {
                Some(u) if format != adv_gpu::present::COLOR_FORMAT => &u.pipelines,
                _ => &self.pipelines,
            };
            let pipeline = if software {
                &pipelines.software
            } else {
                &pipelines.output
            };
            self.spans.extend(output::encode(
                pass,
                base.source(),
                size,
                Transform {
                    rotation: Rotation::None,
                    dst: Rect {
                        x: 0,
                        y: 0,
                        w: size.width,
                        h: size.height,
                    },
                },
                Scale::Integer { x: 1, y: 1 },
                tiles,
                (pipeline, &mut self.ring),
            )?);
        }
        for (i, v) in videos.iter().enumerate() {
            let base = if software && i == 0 {
                let output::Source::Software(base) = base.source() else {
                    unreachable!()
                };
                Some(base)
            } else {
                None
            };
            self.draw_video(pass, target, base.as_ref(), v, tiles)?;
        }
        Ok(())
    }
    /// The upscaler this frame can run: `choice` once its model is built for
    /// `canvas`, otherwise none while a build runs off the owner thread
    /// (pipeline creation blocks for long on mobile drivers).
    fn upscaler(&mut self, choice: Upscaler, canvas: (u32, u32)) -> Upscaler {
        self.install();
        let key = (choice, canvas);
        if choice == Upscaler::Off {
            self.upscaled = None;
            self.building = None;
            return choice;
        }
        if self
            .upscaled
            .as_ref()
            .is_some_and(|u| (u.choice, u.upscaling.size()) == key)
        {
            return choice;
        }
        if self.building.as_ref().is_none_or(|b| b.key != key) {
            let (send, result) = std::sync::mpsc::sync_channel(1);
            let (device, wake) = (self.device.clone(), self.wake.clone());
            let spawned = std::thread::Builder::new()
                .name("adv-upscale-build".into())
                .spawn(move || {
                    let _ = send.send(Upscaled::new(&device, choice, canvas));
                    if let Some(wake) = wake {
                        wake();
                    }
                });
            self.building = Some(Building {
                key,
                result,
                failed: spawned.is_err(),
            });
        }
        Upscaler::Off
    }
    /// Take a finished build. Content shown so far is unscaled, so the next
    /// frame is whole.
    fn install(&mut self) -> bool {
        let Some(building) = self.building.as_mut().filter(|b| !b.failed) else {
            return false;
        };
        match building.result.try_recv() {
            Ok(Ok(upscaled)) => {
                self.building = None;
                self.upscaled = Some(upscaled);
                self.pools.invalidate();
                self.invalid = true;
                true
            }
            Ok(Err(error)) => {
                eprintln!(
                    "[display] {:?} upscaler unavailable: {error:?}",
                    building.key.0
                );
                building.failed = true;
                false
            }
            Err(_) => false,
        }
    }
    fn draw_video(
        &mut self,
        pass: &mut adv_gpu::RenderPass,
        target: &Image,
        base: Option<&ingest::Source<'_>>,
        v: &Video,
        tiles: &Tiles,
    ) -> adv_gpu::Result<()> {
        let format = target.desc().format;
        let pipelines = match &self.upscaled {
            Some(u) if format != adv_gpu::present::COLOR_FORMAT => &u.pipelines,
            _ => &self.pipelines,
        };
        let data = if let Some(planes) = &v.planes {
            if let Some(base) = base {
                video::encode_software(
                    pass,
                    target,
                    base,
                    planes,
                    v.geometry,
                    tiles,
                    (
                        &pipelines.software_video,
                        &pipelines.sampler,
                        &mut self.ring,
                    ),
                )?
            } else {
                video::encode(
                    pass,
                    target,
                    planes,
                    v.geometry,
                    tiles,
                    (&pipelines.video, &pipelines.sampler, &mut self.ring),
                )?
            }
        } else {
            let image = video_image(&self.leases, &self.screen, v.id);
            let pipeline = &self.converted[&(image.pool().id(), base.is_some(), format)].pipeline;
            if let Some(base) = base {
                video::encode_converted_software(
                    pass,
                    target,
                    base,
                    image,
                    v.geometry,
                    tiles,
                    (pipeline, &mut self.ring),
                )?
            } else {
                video::encode_converted(
                    pass,
                    target,
                    image,
                    v.geometry,
                    tiles,
                    (pipeline, &mut self.ring),
                )?
            }
        };
        self.spans.extend(data);
        Ok(())
    }
    /// Plan around every image a compositor or this frame may still read.
    /// A window without a size plans at the last presented view.
    fn plan(&mut self, info: &FrameInfo, reading: Option<&PresentImage>) -> (Settings, Plan) {
        let leased = self.screen.iter().map(|(_, a)| &a.image).chain(reading);
        self.pools.occupy(leased.filter_map(|i| {
            let size = i.image().desc().size;
            Some(Target {
                pool: *self.pool_ids.get(&i.pool().id())?,
                buffer: i.index(),
                size: (size.width, size.height),
            })
        }));
        let mut settings = self.settings.clone();
        settings.upscaler = self.upscaler(settings.upscaler, info.canvas);
        if settings.view.0 == 0 || settings.view.1 == 0 {
            settings.view = self
                .recorder
                .latest()
                .map_or(info.canvas, |r| r.settings.view);
        }
        let planned = plan(info, &settings, &self.caps, &self.pools);
        (settings, planned)
    }
    /// Acquire the planned target buffers, reallocating pools whose size changed.
    fn targets(
        &mut self,
        queue: &mut Queue,
        planned: &Plan,
    ) -> Result<[Option<PresentImage>; 3], Error> {
        let mut targets: [Option<PresentImage>; 3] = std::array::from_fn(|_| None);
        // The upscaler's pool is its own private images.
        for target in planned.targets.iter().filter(|t| t.pool != UPSCALE) {
            let replace = self.allocations.get(&target.pool).is_none_or(|p| {
                let d = p.description();
                (d.width, d.height) != target.size
            });
            if replace {
                let pool = self
                    .device
                    .create_present_pool(&adv_gpu::present::PoolDesc {
                        color: None,
                        width: target.size.0,
                        height: target.size.1,
                        format: adv_gpu::present::COLOR_FORMAT,
                        count: (target.buffer + 1) as u32,
                        storage: false,
                        label: "display target",
                    })?;
                self.pool_ids.insert(pool.id(), target.pool);
                if let Some(old) = self.allocations.insert(target.pool, pool) {
                    self.pool_ids.remove(&old.id());
                }
            }
            let image = self
                .allocations
                .get_mut(&target.pool)
                .unwrap()
                .acquire_index(
                    queue,
                    target.buffer,
                    planned.stages.iter().any(|s| {
                        let (into, tiles) = stage_target(s);
                        into == target.pool && !tiles.rects.is_empty()
                    }),
                )?
                .expect("planner selected a free pool slot");
            targets[target.pool.0 as usize] = Some(image);
        }
        Ok(targets)
    }
    /// Take each video's new decoder image; the replaced ones and those of
    /// videos that left are returned for retirement after this frame.
    fn update_leases(&mut self, video: &mut [Video]) -> Vec<PresentImage> {
        let mut retired = vec![];
        let gone = self
            .leases
            .extract_if(|id, _| !video.iter().any(|v| v.id == *id));
        retired.extend(gone.map(|(_, image)| image));
        for v in video {
            if let Some(image) = v.update.take()
                && let Some(old) = self.leases.insert(v.id, image)
            {
                retired.push(old);
            }
        }
        retired
    }
    fn encode(
        &mut self,
        queue: &mut Queue,
        mut frame: Frame,
        reading: Option<&PresentImage>,
    ) -> Result<Pending, Error> {
        let started = Instant::now();
        let (settings, planned) = self.plan(&frame.info, reading);
        let mut targets = self.targets(queue, &planned)?;
        let retired = self.update_leases(&mut frame.video);
        for stage in &planned.stages {
            let format = match stage {
                Stage::Video { .. } => adv_gpu::present::COLOR_FORMAT,
                Stage::Upscale { video: true, .. } => upscale::INPUT,
                _ => continue,
            };
            let software = matches!(frame.base, Base::Software { .. });
            for (i, v) in frame.video.iter().enumerate() {
                self.video_pipeline(v, i == 0 && software, format)?;
            }
        }
        let mut statistics = Statistics {
            kind: if planned.stages.is_empty() {
                PlanKind::Direct
            } else {
                PlanKind::Merged
            },
            ..Default::default()
        };
        let mut encoder = planned
            .stages
            .iter()
            .any(|s| !stage_target(s).1.rects.is_empty())
            .then(|| queue.encoder("display frame"));
        let mut timed = vec![];
        let mut reads_base = false;
        for (index, stage) in planned.stages.iter().enumerate() {
            let (into, tiles) = stage_target(stage);
            statistics.stages[index] = StageStats {
                tiles: tiles.rects.len() as u32,
                pixels: tiles
                    .rects
                    .iter()
                    .map(|r| u64::from(r.w) * u64::from(r.h))
                    .sum(),
            };
            if tiles.rects.is_empty() {
                continue;
            }
            reads_base |= matches!(
                stage,
                Stage::Ingest { .. }
                    | Stage::Video { .. }
                    | Stage::Upscale { .. }
                    | Stage::Output {
                        from: Source::Base,
                        ..
                    }
            );
            let destination = match stage {
                Stage::Upscale { .. } => self.upscaled.as_ref().unwrap().upscaling.input().clone(),
                _ => targets[into.0 as usize].as_ref().unwrap().image().clone(),
            };
            let size = destination.desc().size;
            let (area, load) = ingest::coverage(tiles);
            let mut pass = encoder
                .as_mut()
                .unwrap()
                .render_pass(&adv_gpu::RenderPassDesc {
                    colors: [adv_gpu::encode::ColorAttachment {
                        view: destination.view(),
                        load,
                        store: adv_gpu::encode::Store::Store,
                        resolve: None,
                    }]
                    .into_iter()
                    .collect(),
                    depth_stencil: None,
                    area,
                    timestamps: self.queries.as_ref().map(|q| (q, statistics.passes * 2)),
                    label: "display target",
                });
            match stage {
                Stage::Ingest { .. } => {
                    let output::Source::Software(source) = frame.base.source() else {
                        unreachable!()
                    };
                    if let Some(data) = ingest::encode(
                        &mut pass,
                        &self.pipelines.ingest,
                        &mut self.ring,
                        &source,
                        tiles,
                    )? {
                        self.spans.push(data);
                    }
                }
                Stage::Output {
                    from,
                    transform,
                    scale,
                    ..
                } => {
                    let source = match from {
                        Source::Base => frame.base.source(),
                        Source::Pool(UPSCALE) => {
                            output::Source::Image(self.upscaled.as_ref().unwrap().output.view())
                        }
                        Source::Pool(p) => output::Source::Image(
                            targets[p.0 as usize].as_ref().unwrap().image().view(),
                        ),
                        _ => unreachable!(),
                    };
                    let pipeline = if matches!(source, output::Source::Software(_)) {
                        &self.pipelines.software
                    } else {
                        &self.pipelines.output
                    };
                    self.spans.extend(output::encode(
                        &mut pass,
                        source,
                        size,
                        *transform,
                        *scale,
                        tiles,
                        (pipeline, &mut self.ring),
                    )?);
                }
                Stage::Video { .. } => {
                    self.compose(&mut pass, &destination, &frame.base, &frame.video, tiles)?
                }
                Stage::Upscale { video, .. } => {
                    let video = if *video { &frame.video[..] } else { &[] };
                    self.compose(&mut pass, &destination, &frame.base, video, tiles)?
                }
            }
            drop(pass);
            if let Stage::Upscale { .. } = stage {
                let u = self.upscaled.as_ref().unwrap();
                u.upscaling.encode(
                    encoder.as_mut().unwrap(),
                    &mut self.ring,
                    u.output.view(),
                    Some(&tiles.rects),
                )?;
            }
            statistics.passes += 1;
            timed.push(index);
        }
        for data in self.spans.drain(..) {
            encoder.as_mut().unwrap().retain(data);
        }
        let timing_buffer = if !timed.is_empty() {
            if let Some(q) = &self.queries {
                let buffer = self.readbacks.pop().map(Ok).unwrap_or_else(|| {
                    self.device.create_buffer(&adv_gpu::resource::BufferDesc {
                        size: 64,
                        usage: adv_gpu::resource::BufferUsage::COPY_DST,
                        memory: adv_gpu::resource::Memory::Readback,
                        label: "stage times",
                    })
                })?;
                encoder
                    .as_mut()
                    .unwrap()
                    .resolve_queries(q, 0..statistics.passes * 2, &buffer, 0);
                Some(buffer)
            } else {
                None
            }
        } else {
            None
        };
        let mut images = vec![];
        let source = planned.layers.base.source;
        if let Source::Pool(p) = source {
            let image = targets[p.0 as usize].take().unwrap();
            images.push(AcquiredImage {
                layer: LayerId::Base,
                image,
                acquire: Token(0),
            });
        } else {
            let Base::Presentable { image, ready } = frame.base else {
                unreachable!("direct frame")
            };
            images.push(AcquiredImage {
                layer: LayerId::Base,
                image,
                acquire: ready,
            });
        }
        let token = if let Some(encoder) = encoder {
            statistics.submissions = 1;
            let mut writes = images.iter_mut().map(|i| &mut i.image).collect::<Vec<_>>();
            let token = queue.submit_present(encoder, &mut writes)?;
            for i in &mut images {
                i.acquire = token;
            }
            token
        } else {
            images[0].acquire
        };
        if let Some(buffer) = timing_buffer {
            self.timings.push_back(Timing {
                token,
                buffer,
                stages: timed,
            });
            if let Some(w) = &self.wake {
                let w = w.clone();
                queue.on_complete(token, Box::new(move || w()));
            }
        }
        if !retired.is_empty() {
            queue.on_complete(token, Box::new(move || drop(retired)));
        }
        for (id, _) in &planned.layers.video {
            if let Some(image) = self.leases.remove(id) {
                images.push(AcquiredImage {
                    layer: LayerId::Video(*id),
                    image,
                    acquire: Token(0),
                });
            }
        }
        Ok(Pending {
            record: PresentRecord {
                token: token.0,
                published_at: frame.published_at.saturating_duration_since(self.origin),
                frame: frame.info,
                settings,
                caps: self.caps.clone(),
                pools: self.pools.clone(),
                stage_times: vec![Duration::ZERO; planned.stages.len()],
                plan: planned,
                statistics,
                at: started.saturating_duration_since(self.origin),
                cpu_time: started.elapsed(),
            },
            images,
            video: frame.video,
            read: if reads_base { token } else { Token(0) },
        })
    }
}

pub struct Captured {
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}
impl Display {
    pub fn capture(
        &mut self,
        queue: &mut Queue,
        reply: Box<dyn FnOnce(Result<Captured, Error>) + Send>,
    ) {
        let result = self.capture_commands(queue);
        match result {
            Err(error) => reply(Err(error)),
            Ok((token, buffer, serial, canvas)) => queue.on_complete(
                token,
                Box::new(move || {
                    let mut bgra = unsafe { buffer.mapped().unwrap().as_ref() }.to_vec();
                    for pixel in bgra.as_chunks_mut::<4>().0 {
                        if adv_gpu::present::COLOR_FORMAT == Format::Rgba8Unorm {
                            pixel.swap(0, 2);
                        }
                        pixel[3] = 255;
                    }
                    reply(Ok(Captured {
                        serial,
                        width: canvas.0,
                        height: canvas.1,
                        bgra,
                    }));
                }),
            ),
        }
    }
    fn capture_commands(
        &mut self,
        queue: &mut Queue,
    ) -> Result<(Token, adv_gpu::Buffer, u64, (u32, u32)), Error> {
        let record = self
            .recorder
            .latest()
            .ok_or_else(|| adv_gpu::Error::Invalid("no presented frame".into()))?;
        let serial = record.frame.serial;
        let canvas = record.frame.canvas;
        let separate = record
            .plan
            .layers
            .video
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        let base = shown(&self.screen, LayerId::Base).unwrap().image().clone();
        let rotation = buffer_rotation(record);
        let videos = self
            .video
            .iter()
            .filter(|v| separate.contains(&v.id))
            .map(Video::retained)
            .collect::<Vec<_>>();
        for v in &videos {
            self.video_pipeline(v, false, adv_gpu::present::COLOR_FORMAT)?;
        }
        let size = adv_gpu::Extent {
            width: canvas.0,
            height: canvas.1,
            depth_or_layers: 1,
        };
        let target = self.device.create_image(&adv_gpu::resource::ImageDesc {
            dimension: adv_gpu::resource::Dimension::D2,
            size,
            format: adv_gpu::present::COLOR_FORMAT,
            mips: 1,
            samples: 1,
            usage: adv_gpu::resource::Usage::COLOR | adv_gpu::resource::Usage::COPY_SRC,
            view_formats: &[],
            label: "capture canvas",
        })?;
        let buffer = self.device.create_buffer(&adv_gpu::resource::BufferDesc {
            size: u64::from(canvas.0) * u64::from(canvas.1) * 4,
            usage: adv_gpu::resource::BufferUsage::COPY_DST,
            memory: adv_gpu::resource::Memory::Readback,
            label: "capture readback",
        })?;
        let tiles = Tiles {
            size: canvas,
            rects: vec![Rect {
                x: 0,
                y: 0,
                w: canvas.0,
                h: canvas.1,
            }],
        };
        let mut encoder = queue.encoder("capture");
        {
            let mut pass = encoder.render_pass(&adv_gpu::RenderPassDesc {
                colors: [adv_gpu::encode::ColorAttachment {
                    view: target.view(),
                    load: adv_gpu::encode::Load::DontCare,
                    store: adv_gpu::encode::Store::Store,
                    resolve: None,
                }]
                .into_iter()
                .collect(),
                depth_stencil: None,
                area: tiles.rects[0].into(),
                timestamps: None,
                label: "capture canvas",
            });
            self.spans.extend(output::encode(
                &mut pass,
                output::Source::Retained {
                    view: base.view(),
                    canvas,
                    rotation,
                },
                size,
                Transform {
                    rotation: Rotation::None,
                    dst: tiles.rects[0],
                },
                Scale::Integer { x: 1, y: 1 },
                &tiles,
                (&self.pipelines.output, &mut self.ring),
            )?);
            for v in &videos {
                self.draw_video(&mut pass, &target, None, v, &tiles)?;
            }
        }
        for span in self.spans.drain(..) {
            encoder.retain(span);
        }
        encoder.copy_image_to_buffer(
            adv_gpu::encode::ImageCopy {
                image: &target,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            adv_gpu::encode::BufferCopy {
                buffer: &buffer,
                offset: 0,
                row_pitch: canvas.0 * 4,
                rows: canvas.1,
            },
            size,
        );
        Ok((queue.submit(encoder)?, buffer, serial, canvas))
    }
}

/// The image `layer` shows: its newest attached one.
fn shown(screen: &[(u64, AcquiredImage)], layer: LayerId) -> Option<&PresentImage> {
    screen
        .iter()
        .filter(|(_, a)| a.layer == layer)
        .max_by_key(|(serial, _)| *serial)
        .map(|(_, a)| &a.image)
}

fn video_image<'a>(
    leases: &'a HashMap<u64, PresentImage>,
    screen: &'a [(u64, AcquiredImage)],
    id: u64,
) -> &'a PresentImage {
    leases
        .get(&id)
        .or_else(|| shown(screen, LayerId::Video(id)))
        .expect("retained decoder lease")
}

fn buffer_rotation(record: &PresentRecord) -> Rotation {
    if record.plan.layers.base.rotation == Rotation::None {
        record.settings.rotation
    } else {
        Rotation::None
    }
}

fn stage_target(stage: &Stage) -> (PoolId, &Tiles) {
    match stage {
        Stage::Ingest { into, tiles }
        | Stage::Video { into, tiles, .. }
        | Stage::Upscale { into, tiles, .. }
        | Stage::Output { into, tiles, .. } => (*into, tiles),
    }
}
