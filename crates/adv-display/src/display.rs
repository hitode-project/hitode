//! One queue owns planning, target passes, presentation and its flight recorder.
use crate::{
    Rect,
    compositor::*,
    plan::*,
    recorder::{self, PlanKind, PresentRecord, StageStats, Statistics},
    stages::{ingest, output, video},
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
pub struct Display {
    device: Device,
    compositor: Option<Box<dyn Compositor>>,
    caps: CompositorCaps,
    settings: Settings,
    cost: CostModel,
    pools: PoolState,
    allocations: HashMap<PoolId, PresentPool>,
    pool_ids: HashMap<usize, PoolId>,
    pipelines: Pipelines,
    converted: HashMap<(usize, bool), Converted>,
    leases: HashMap<u64, PresentImage>,
    ring: Ring,
    spans: Vec<Span>,
    recorder: recorder::Recorder,
    origin: Instant,
    wake: Option<Wake>,
    unlatched: Option<u64>,
    held: Option<Pending>,
    kept: Vec<AcquiredImage>,
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
        let format = adv_gpu::present::COLOR_FORMAT;
        let (video, sampler) = video::resources(&device, format)?;
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
        let (software_video, _) = video::software_resources(&device, format)?;
        let pipelines = Pipelines {
            software_video,
            ingest: ingest::pipeline(&device, format)?,
            output: output::pipeline(&device, format, false)?,
            software: output::pipeline(&device, format, true)?,
            video,
            sampler,
        };
        let mut this = Self {
            caps: compositor.caps(),
            compositor: Some(compositor),
            device,
            settings,
            cost: CostModel {
                ns_per_megapixel: Default::default(),
                thermal: Thermal::Nominal,
                frame_budget_ns: 16_666_667,
            },
            pools: Default::default(),
            allocations: HashMap::new(),
            pool_ids: HashMap::new(),
            pipelines,
            converted: HashMap::new(),
            leases: HashMap::new(),
            ring,
            spans: vec![],
            recorder: recorder::Recorder::present_history(),
            origin: Instant::now(),
            wake: None,
            unlatched: None,
            held: None,
            kept: vec![],
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
        if let Some(r) = self.recorder.latest() {
            let serial = r.statistics.presents;
            self.recorder
                .get_mut(serial)
                .unwrap()
                .statistics
                .configurations += 1;
        }
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
    pub fn ledger(&self) -> &PoolState {
        &self.pools
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
        let (record, images, video) = if let Some(held) = &self.held {
            (&held.record, held.images.as_slice(), &held.video)
        } else {
            (self.recorder.latest()?, self.kept.as_slice(), &self.video)
        };
        let image = images
            .iter()
            .find(|i| i.layer == LayerId::Base)
            .or_else(|| {
                self.compositor
                    .as_ref()
                    .and_then(|c| c.image(LayerId::Base))
            })
            .unwrap();
        let mut info = record.frame.clone();
        info.base = BaseKind::Gpu { presentable: false };
        info.damage = Damage::Whole;
        Some(Frame {
            info,
            base: Base::Retained {
                image: image.image.image().clone(),
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
            plan(&info, &self.settings, &self.caps, &self.cost, &self.pools).swap_role,
            SwapRole::Presentable { .. }
        )
    }
    pub fn suspend(&mut self) {
        if let Some(c) = self.compositor.take() {
            self.kept = c.detach();
            // Detach retired every older lease; only these images remain owned.
            for pool in &mut self.pools.pools {
                for buffer in &mut pool.buffers {
                    buffer.in_use = false;
                }
            }
            for acquired in &self.kept {
                if let Some(id) = self.pool_ids.get(&acquired.image.pool().id()) {
                    self.pools
                        .pools
                        .iter_mut()
                        .find(|p| p.id == *id)
                        .unwrap()
                        .buffers[acquired.image.index()]
                    .in_use = true;
                }
            }
            if let Some(held) = &self.held {
                self.pools.reserve(&held.record.plan);
            }
            self.unlatched = None;
            self.recorder
                .record(recorder::Event::Lifecycle("surface detached".into()));
        }
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
        if let Some(w) = &self.wake {
            self.compositor.as_mut().unwrap().set_waker(w.clone());
        }
        if let Some(record) = self.recorder.latest() {
            let layers = &record.plan.layers;
            let serial = self.recorder.latest().unwrap().statistics.presents;
            self.compositor
                .as_mut()
                .unwrap()
                .present(
                    layers,
                    &Tiles::default(),
                    std::mem::take(&mut self.kept),
                    serial,
                    Instant::now(),
                )
                .map_err(|e| Error::Compositor(e.error))?;
            self.unlatched = Some(serial);
        }
        self.recorder
            .record(recorder::Event::Lifecycle("surface attached".into()));
        self.service(queue)?;
        Ok(())
    }
    fn release(&mut self, image: PresentImage, fence: Option<adv_gpu::queue::Fence>) {
        if let Some(&id) = self.pool_ids.get(&image.pool().id()) {
            self.pools.release(id, image.index());
            self.recorder.record(recorder::Event::Released {
                pool: id.0,
                buffer: image.index(),
            });
        }
        image.released(fence);
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
        let reading = self.held.take().and_then(|held| {
            frame.info.damage = frame.info.damage.union(&held.record.plan.damage);
            self.discard(held, source)
        });
        if self.invalid {
            frame.info.damage = Damage::Whole;
        }
        let result = self.encode(queue, frame);
        if let Some(image) = reading {
            self.release(image, None);
        }
        let pending = result.map_err(|error| Failure {
            error,
            token: Token(0),
        })?;
        let token = if matches!(pending.record.frame.base, BaseKind::Software { .. })
            && pending.record.statistics.stages[0].pixels == 0
        {
            Token(0)
        } else {
            Token(pending.record.token)
        };
        if self.compositor.is_none()
            || self.unlatched.is_some()
            || self.settings.view.0 == 0
            || self.settings.view.1 == 0
        {
            self.pools.reserve(&pending.record.plan);
            self.held = Some(pending);
        } else {
            self.attach(pending)
                .map_err(|error| Failure { error, token })?;
        }
        Ok(token)
    }
    fn attach(&mut self, mut pending: Pending) -> Result<(), Error> {
        let layers = &pending.record.plan.layers;
        let previous = self
            .recorder
            .latest()
            .map(|r| r.statistics)
            .unwrap_or_default();
        let serial = previous.presents + 1;
        pending.record.statistics.presents = serial;
        let software = matches!(pending.record.frame.base, BaseKind::Software { .. })
            && pending.record.statistics.stages[0].pixels != 0;
        pending.record.statistics.software_frames = previous.software_frames + u64::from(software);
        pending.record.statistics.software_bytes = previous.software_bytes
            + if software {
                pending.record.statistics.stages[0].pixels * 4
            } else {
                0
            };
        pending.record.statistics.software_started = previous
            .software_started
            .or(software.then_some(pending.record.at));
        pending.record.statistics.direct_presents =
            previous.direct_presents + u64::from(pending.record.plan.stages.is_empty());
        pending.record.statistics.configurations = previous.configurations.max(1);
        pending.record.statistics.max_cpu = previous.max_cpu.max(pending.record.cpu_time);
        pending.record.statistics.max_queue = previous.max_queue.max(
            pending
                .record
                .at
                .saturating_sub(pending.record.published_at),
        );
        self.compositor
            .as_mut()
            .unwrap()
            .present(
                layers,
                &Tiles::default(),
                pending.images,
                serial,
                Instant::now(),
            )
            .map_err(|e| Error::Compositor(e.error))?;
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
            let retain = matches!(r.layer,LayerId::Video(id) if self.video.iter().any(|v|v.id==id&&v.image.id()==r.image.image().id()));
            if retain {
                let LayerId::Video(id) = r.layer else {
                    unreachable!()
                };
                let mut image = r.image;
                image.detached(r.fence)?;
                self.leases.insert(id, image);
            } else {
                self.release(r.image, r.fence);
            }
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
        self.drain(queue)?;
        if self.unlatched.is_none()
            && self.compositor.is_some()
            && self.settings.view.0 != 0
            && self.settings.view.1 != 0
            && let Some(p) = self.held.take()
        {
            self.attach(p)?;
        }
        Ok(())
    }
    fn video_image(&self, id: u64) -> &PresentImage {
        self.leases
            .get(&id)
            .or_else(|| {
                self.compositor
                    .as_ref()
                    .and_then(|c| c.image(LayerId::Video(id)))
                    .map(|a| &a.image)
            })
            .or_else(|| {
                self.kept
                    .iter()
                    .find(|a| a.layer == LayerId::Video(id))
                    .map(|a| &a.image)
            })
            .expect("retained decoder lease")
    }
    fn video_pipeline(&mut self, v: &Video, software: bool) -> adv_gpu::Result<()> {
        if v.planes.is_some() {
            return Ok(());
        }
        let pool = self.video_image(v.id).pool();
        let key = (pool.id(), software);
        if !self.converted.contains_key(&key) {
            let pipeline = if software {
                video::converted_software_resources(
                    &self.device,
                    adv_gpu::present::COLOR_FORMAT,
                    pool,
                )?
            } else {
                video::converted_resources(&self.device, adv_gpu::present::COLOR_FORMAT, pool)?
            };
            let pool = pool.weak();
            self.converted.insert(key, Converted { pool, pipeline });
        }
        Ok(())
    }
    fn draw_video(
        &mut self,
        pass: &mut adv_gpu::RenderPass,
        target: &Image,
        base: Option<&ingest::Source<'_>>,
        v: &Video,
        tiles: &Tiles,
    ) -> adv_gpu::Result<()> {
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
                        &self.pipelines.software_video,
                        &self.pipelines.sampler,
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
                    (
                        &self.pipelines.video,
                        &self.pipelines.sampler,
                        &mut self.ring,
                    ),
                )?
            }
        } else {
            // Split the borrows: these stores own all decoder leases; only the ring mutates.
            let image = self
                .leases
                .get(&v.id)
                .or_else(|| {
                    self.compositor
                        .as_ref()
                        .and_then(|c| c.image(LayerId::Video(v.id)))
                        .map(|a| &a.image)
                })
                .or_else(|| {
                    self.kept
                        .iter()
                        .find(|a| a.layer == LayerId::Video(v.id))
                        .map(|a| &a.image)
                })
                .unwrap();
            let pipeline = &self.converted[&(image.pool().id(), base.is_some())].pipeline;
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
    fn encode(&mut self, queue: &mut Queue, mut frame: Frame) -> Result<Pending, Error> {
        let started = Instant::now();
        let mut settings = self.settings.clone();
        if settings.view.0 == 0 || settings.view.1 == 0 {
            settings.view = self
                .recorder
                .latest()
                .map_or(frame.info.canvas, |r| r.settings.view);
        }
        let planned = plan(&frame.info, &settings, &self.caps, &self.cost, &self.pools);
        let mut targets: [Option<PresentImage>; 3] = std::array::from_fn(|_| None);
        for target in &planned.targets {
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
        let mut retired = vec![];
        let gone = self
            .leases
            .extract_if(|id, _| !frame.video.iter().any(|v| v.id == *id));
        retired.extend(gone.map(|(_, image)| image));
        for v in &mut frame.video {
            if let Some(image) = v.update.take()
                && let Some(old) = self.leases.insert(v.id, image)
            {
                retired.push(old);
            }
        }
        if planned
            .stages
            .iter()
            .any(|s| matches!(s, Stage::Video { .. }))
        {
            for (i, v) in frame.video.iter().enumerate() {
                self.video_pipeline(v, i == 0 && matches!(frame.base, Base::Software { .. }))?;
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
            let destination = targets[into.0 as usize].as_ref().unwrap().image();
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
                    let software = matches!(frame.base, Base::Software { .. });
                    if !software || frame.video.is_empty() {
                        let pipeline = if software {
                            &self.pipelines.software
                        } else {
                            &self.pipelines.output
                        };
                        self.spans.extend(output::encode(
                            &mut pass,
                            frame.base.source(),
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
                    for (i, v) in frame.video.iter().enumerate() {
                        let base = if software && i == 0 {
                            let output::Source::Software(base) = frame.base.source() else {
                                unreachable!()
                            };
                            Some(base)
                        } else {
                            None
                        };
                        self.draw_video(&mut pass, destination, base.as_ref(), v, tiles)?;
                    }
                }
                _ => unreachable!(),
            }
            drop(pass);
            statistics.passes += 1;
            timed.push(index);
        }
        if let Some(encoder) = &mut encoder {
            match &frame.base {
                Base::Presentable { image, .. } => encoder.release_presentable(image.image()),
                Base::Retained { image, .. } => encoder.release_presentable(image),
                _ => {}
            }
            if planned
                .stages
                .iter()
                .any(|s| matches!(s, Stage::Video { .. }))
            {
                for v in &frame.video {
                    encoder.release_presentable(&v.image);
                }
            }
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
                cost: self.cost.clone(),
                pools: self.pools.clone(),
                stage_times: vec![Duration::ZERO; planned.stages.len()],
                plan: planned,
                statistics,
                at: started.saturating_duration_since(self.origin),
                cpu_time: started.elapsed(),
            },
            images,
            video: frame.video,
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
        let image = self
            .compositor
            .as_ref()
            .and_then(|c| c.image(LayerId::Base))
            .or_else(|| self.kept.iter().find(|a| a.layer == LayerId::Base))
            .unwrap();
        let base = image.image.image().clone();
        let rotation = buffer_rotation(record);
        let videos = self
            .video
            .iter()
            .filter(|v| separate.contains(&v.id))
            .map(Video::retained)
            .collect::<Vec<_>>();
        for v in &videos {
            self.video_pipeline(v, false)?;
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
        encoder.release_presentable(&base);
        for v in &videos {
            encoder.release_presentable(&v.image);
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
        | Stage::Output { into, tiles, .. } => (*into, tiles),
        Stage::Upscale { .. } | Stage::Refine { .. } => {
            unreachable!("upscaling is not wired into the executor")
        }
    }
}
