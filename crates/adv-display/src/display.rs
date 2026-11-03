//! Runs a planner result on the presenting queue and commits its ledger atomically.
use crate::{
    Rect,
    compositor::*,
    plan::*,
    recorder,
    stages::{ingest, output, video},
};
use adv_gpu::{
    Device, Image, ImageView, PresentImage, PresentPool, Queue, RenderPipeline, Ring, Sampler,
    Token,
};
use std::{collections::VecDeque, time::Instant};

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
        palette: Option<Box<[[u8; 4]; 256]>>,
    },
}
impl Base {
    fn software(&self) -> ingest::Source<'_> {
        let Self::Software {
            buffer,
            pitch,
            extent,
            format,
            palette,
        } = self
        else {
            unreachable!("software stage source")
        };
        ingest::Source {
            buffer,
            pitch: *pitch,
            extent: *extent,
            format: *format,
            palette: palette.as_deref(),
        }
    }
    fn image(&self) -> &Image {
        match self {
            Self::Presentable { image, .. } => image.image(),
            Self::Private(image) => image,
            Self::Software { .. } => unreachable!("image source"),
        }
    }
}
pub struct Video {
    pub id: u64,
    pub image: Image,
    pub planes: Option<[ImageView; 2]>,
    pub geometry: video::Geometry,
    pub update: Option<PresentImage>,
}
pub struct Frame {
    pub info: FrameInfo,
    pub base: Base,
    pub video: Vec<Video>,
    pub at: Instant,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanKind {
    Direct,
    Merged,
    Upscaled,
}
#[derive(Clone, Copy, Debug)]
pub enum StageKind {
    Ingest,
    Video,
    Output,
}
#[derive(Clone, Debug)]
pub struct StageStats {
    pub kind: StageKind,
    pub tiles: usize,
    pub pixels: u64,
}
#[derive(Clone, Debug)]
pub struct Statistics {
    pub at: Instant,
    pub serial: u64,
    pub kind: PlanKind,
    pub stages: Vec<StageStats>,
    pub passes: u32,
    pub submissions: u32,
    pub latched: Option<Instant>,
    pub shown: Option<Instant>,
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

// One lease per ledger slot makes the planner's buffer identity exact, including
// intermediate targets. PresentPool orders the release fence on reacquisition.
struct Buffer {
    pool: PresentPool,
    id: Option<u64>,
    size: (u32, u32),
}
struct Allocation {
    id: PoolId,
    buffers: Vec<Buffer>,
}
struct Pipelines {
    ingest: RenderPipeline,
    output: RenderPipeline,
    software_output: RenderPipeline,
    video: RenderPipeline,
    software_video: RenderPipeline,
    sampler: Sampler,
}
struct Last {
    layers: Layers,
    damage: Tiles,
    serial: u64,
    targets: Vec<Target>,
}
pub struct Display {
    device: Device,
    compositor: Option<Box<dyn Compositor + Send>>,
    caps: CompositorCaps,
    settings: Settings,
    cost: CostModel,
    pools: PoolState,
    allocations: Vec<Allocation>,
    video_leases: Vec<(u64, PresentImage)>,
    pipelines: Pipelines,
    ring: Ring,
    recorder: recorder::Recorder,
    statistics: VecDeque<Statistics>,
    last: Option<Last>,
    invalid: bool,
    last_read: Token,
}
impl Display {
    pub fn new(
        device: Device,
        compositor: Box<dyn Compositor + Send>,
        settings: Settings,
    ) -> adv_gpu::Result<Self> {
        let caps = compositor.caps();
        let format = adv_gpu::Format::Bgra8Unorm;
        let (video, sampler) = video::resources(&device, format)?;
        let (software_video, _) = video::software_resources(&device, format)?;
        let pipelines = Pipelines {
            ingest: ingest::pipeline(&device, format)?,
            output: output::pipeline(&device, format, false)?,
            software_output: output::pipeline(&device, format, true)?,
            video,
            software_video,
            sampler,
        };
        let ring = device.ring(4 << 20)?;
        Ok(Self {
            device,
            compositor: Some(compositor),
            caps,
            settings,
            cost: CostModel {
                ns_per_megapixel: Default::default(),
                thermal: Thermal::Nominal,
                frame_budget_ns: 16_666_667,
            },
            pools: PoolState::default(),
            allocations: vec![],
            video_leases: vec![],
            pipelines,
            ring,
            recorder: recorder::Recorder::present_history(),
            statistics: VecDeque::new(),
            last: None,
            invalid: true,
            last_read: Token(0),
        })
    }
    pub fn settings(&mut self, settings: Settings) {
        if self.settings != settings {
            self.settings = settings;
            self.invalidate();
        }
    }
    pub fn invalidate(&mut self) {
        self.pools.invalidate();
        self.invalid = true;
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
    pub fn recorder(&self) -> &recorder::Recorder {
        &self.recorder
    }
    pub fn statistics(&self) -> &VecDeque<Statistics> {
        &self.statistics
    }
    pub fn ledger(&self) -> &PoolState {
        &self.pools
    }
    /// Last submitted stage read of the current frame, including a failed attachment.
    pub fn last_read(&self) -> Token {
        self.last_read
    }
    pub fn attached(&self) -> bool {
        self.compositor.is_some()
    }
    pub fn suspend(&mut self) {
        if self.compositor.take().is_some() {
            // Compositor teardown returns its leases; each pool retains the
            // release fence and orders any subsequent writer on acquisition.
            for pool in &mut self.pools.pools {
                for buffer in &mut pool.buffers {
                    buffer.in_use = false;
                }
            }
            self.recorder
                .record(recorder::Event::Lifecycle("surface detached".into()));
        }
    }
    /// Pools survive the compositor. Reattach the last executor-owned buffers,
    /// without encoding or submitting a stage.
    pub fn rebuild(
        &mut self,
        compositor: Box<dyn Compositor + Send>,
        queue: &mut Queue,
    ) -> Result<(), Error> {
        self.suspend();
        self.caps = compositor.caps();
        self.compositor = Some(compositor);
        self.recorder
            .record(recorder::Event::Lifecycle("surface attached".into()));
        if let Some(last) = self.last.take() {
            let mut images = vec![];
            for target in &last.targets {
                let layer = if last.layers.base.source == Source::Pool(target.pool) {
                    Some(LayerId::Base)
                } else {
                    None
                };
                if let Some(layer) = layer {
                    images.push(AcquiredImage {
                        layer,
                        image: self.acquire(queue, *target)?,
                        acquire: Token(0),
                    });
                }
            }
            if !images.is_empty() {
                self.compositor
                    .as_mut()
                    .unwrap()
                    .present(
                        &last.layers,
                        &last.damage,
                        images,
                        last.serial,
                        Instant::now(),
                    )
                    .map_err(|e| {
                        for i in e.images {
                            i.image.released(None);
                        }
                        Error::Compositor(e.error)
                    })?;
                for target in &last.targets {
                    if last.layers.base.source == Source::Pool(target.pool) {
                        self.pools
                            .pools
                            .iter_mut()
                            .find(|p| p.id == target.pool)
                            .unwrap()
                            .buffers[target.buffer]
                            .in_use = true;
                    }
                }
            }
            self.last = Some(last);
        }
        Ok(())
    }
    fn acquire(&mut self, queue: &mut Queue, target: Target) -> adv_gpu::Result<PresentImage> {
        let index = match self.allocations.iter().position(|a| a.id == target.pool) {
            Some(i) => i,
            None => {
                self.allocations.push(Allocation {
                    id: target.pool,
                    buffers: vec![],
                });
                self.allocations.len() - 1
            }
        };
        let allocation = &mut self.allocations[index];
        while allocation.buffers.len() <= target.buffer {
            allocation.buffers.push(Buffer {
                pool: self
                    .device
                    .create_present_pool(&adv_gpu::present::PoolDesc {
                        color: None,
                        width: target.size.0,
                        height: target.size.1,
                        format: adv_gpu::Format::Bgra8Unorm,
                        count: 1,
                        label: "display target",
                    })?,
                id: None,
                size: target.size,
            });
        }
        let buffer = &mut allocation.buffers[target.buffer];
        if buffer.size != target.size {
            buffer.pool = self
                .device
                .create_present_pool(&adv_gpu::present::PoolDesc {
                    color: None,
                    width: target.size.0,
                    height: target.size.1,
                    format: adv_gpu::Format::Bgra8Unorm,
                    count: 1,
                    label: "display target",
                })?;
            buffer.size = target.size;
        }
        let image = buffer.pool.acquire(queue)?.ok_or(adv_gpu::Error::Busy)?;
        buffer.id = Some(image.image().id());
        Ok(image)
    }
    pub fn present(&mut self, queue: &mut Queue, mut frame: Frame) -> Result<Option<Token>, Error> {
        self.last_read = Token(0);
        self.drain();
        if self.compositor.is_none() || self.settings.view.0 == 0 || self.settings.view.1 == 0 {
            return Ok(None);
        }
        if self.invalid {
            frame.info.damage = Damage::Whole;
        }
        let plan = plan(
            &frame.info,
            &self.settings,
            &self.caps,
            &self.cost,
            &self.pools,
        );
        let mut targets = vec![];
        for target in &plan.targets {
            targets.push((target.pool, self.acquire(queue, *target)?));
        }
        let mut encoder = queue.encoder("display frame");
        let mut passes = 0;
        for stage in &plan.stages {
            match stage {
                Stage::Ingest { into, tiles } => {
                    let target = &targets.iter().find(|(id, _)| id == into).unwrap().1;
                    ingest::encode(
                        &mut encoder,
                        &self.pipelines.ingest,
                        &mut self.ring,
                        &frame.base.software(),
                        target.image().view(),
                        tiles,
                    )?;
                    passes += u32::from(!tiles.rects.is_empty());
                }
                Stage::Output {
                    from,
                    into,
                    transform,
                    scale,
                    tiles,
                } => {
                    let target = &targets.iter().find(|(id, _)| id == into).unwrap().1;
                    let source = match from {
                        Source::Base => match &frame.base {
                            Base::Software { .. } => {
                                output::Source::Software(frame.base.software())
                            }
                            _ => output::Source::Image(frame.base.image().view()),
                        },
                        Source::Pool(id) => output::Source::Image(
                            targets
                                .iter()
                                .find(|(p, _)| p == id)
                                .unwrap()
                                .1
                                .image()
                                .view(),
                        ),
                        Source::Video(_) => unreachable!("output base source"),
                    };
                    let pipeline = if matches!(source, output::Source::Software(_)) {
                        &self.pipelines.software_output
                    } else {
                        &self.pipelines.output
                    };
                    output::encode(
                        &mut encoder,
                        source,
                        target.image().view(),
                        *transform,
                        *scale,
                        tiles,
                        (pipeline, &mut self.ring),
                    )?;
                    passes += u32::from(!tiles.rects.is_empty());
                }
                Stage::Video { into, tiles, .. } => {
                    let target = targets.iter().find(|(id, _)| id == into).unwrap().1.image();
                    let software = matches!(frame.base, Base::Software { .. });
                    if !software {
                        let size = target.desc().size;
                        output::encode(
                            &mut encoder,
                            output::Source::Image(frame.base.image().view()),
                            target.view(),
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
                            (&self.pipelines.output, &mut self.ring),
                        )?;
                        passes += u32::from(!tiles.rects.is_empty());
                    }
                    for (i, v) in frame.video.iter().enumerate() {
                        let planes = v.planes.as_ref().ok_or_else(|| {
                            adv_gpu::Error::Unsupported(
                                "composed converted video requires its decoder pool".into(),
                            )
                        })?;
                        if software && i == 0 {
                            video::encode_software(
                                &mut encoder,
                                target,
                                &frame.base.software(),
                                planes,
                                v.geometry,
                                tiles,
                                (
                                    &self.pipelines.software_video,
                                    &self.pipelines.sampler,
                                    &mut self.ring,
                                ),
                            )?;
                        } else {
                            video::encode(
                                &mut encoder,
                                target,
                                planes,
                                v.geometry,
                                tiles,
                                (
                                    &self.pipelines.video,
                                    &self.pipelines.sampler,
                                    &mut self.ring,
                                ),
                            )?;
                        }
                        passes += u32::from(!tiles.rects.is_empty());
                    }
                }
                Stage::Upscale { .. } | Stage::Refine { .. } => {
                    unreachable!("native upscaling belongs to U1")
                }
            }
        }
        let mut images = vec![];
        let mut intermediate = vec![];
        for (id, image) in targets {
            if plan.layers.base.source == Source::Pool(id) {
                images.push(AcquiredImage {
                    layer: LayerId::Base,
                    image,
                    acquire: Token(0),
                });
            } else {
                intermediate.push(image);
            }
        }
        let submitted = !plan.stages.is_empty();
        let token = if submitted {
            let mut writes: Vec<_> = images.iter_mut().map(|i| &mut i.image).collect();
            let token = queue.submit_present(encoder, &mut writes)?;
            for i in &mut images {
                i.acquire = token;
            }
            token
        } else {
            drop(encoder);
            Token(0)
        };
        self.last_read = token;
        if plan.layers.base.source == Source::Base {
            let Base::Presentable { image, ready } = frame.base else {
                unreachable!("direct base is presentable")
            };
            images.push(AcquiredImage {
                layer: LayerId::Base,
                image,
                acquire: ready,
            });
        }
        let mut layers = plan.layers.clone();
        let mut retired_video = vec![];
        let ids = frame.video.iter().map(|v| v.id).collect::<Vec<_>>();
        let removed = self
            .video_leases
            .extract_if(.., |(id, _)| !ids.contains(id));
        retired_video.extend(removed.map(|(_, image)| image));
        for v in frame.video {
            if let Some((_, layer)) = layers.video.iter_mut().find(|(id, _)| *id == v.id) {
                let size = v.image.desc().size;
                let uv = v.geometry.source_uv;
                layer.crop = Rect {
                    x: (uv[0] * size.width as f32).round() as u32,
                    y: (uv[1] * size.height as f32).round() as u32,
                    w: (uv[2] * size.width as f32).round() as u32,
                    h: (uv[3] * size.height as f32).round() as u32,
                };
                let retained = self
                    .video_leases
                    .iter()
                    .position(|(id, _)| *id == v.id)
                    .map(|i| self.video_leases.swap_remove(i).1);
                if let Some(image) = v.update.or(retained) {
                    images.push(AcquiredImage {
                        layer: LayerId::Video(v.id),
                        image,
                        acquire: Token(0),
                    });
                }
            } else if let Some(image) = v.update {
                if let Some(i) = self.video_leases.iter().position(|(id, _)| *id == v.id) {
                    retired_video.push(self.video_leases.swap_remove(i).1);
                }
                self.video_leases.push((v.id, image));
            }
        }
        if !retired_video.is_empty() {
            queue.on_complete(token, Box::new(move || drop(retired_video)));
        }
        let damage = Tiles {
            size: self.caps.tile,
            rects: match &plan.layers.base.damage {
                Damage::None => vec![],
                Damage::Whole => vec![plan.layers.base.crop],
                Damage::Rects(r) => r.to_vec(),
            },
        };
        self.compositor
            .as_mut()
            .unwrap()
            .present(&layers, &damage, images, frame.info.serial, frame.at)
            .map_err(|e| {
                for i in e.images {
                    i.image.released(None);
                }
                Error::Compositor(e.error)
            })?;
        self.recorder.record(recorder::Event::Present(Box::new(
            recorder::PresentRecord {
                frame: frame.info.clone(),
                settings: self.settings.clone(),
                caps: self.caps.clone(),
                cost: self.cost.clone(),
                pools: self.pools.clone(),
                plan: plan.clone(),
                stage_times: vec![],
            },
        )));
        self.pools.after_present(&plan);
        drop(intermediate);
        self.invalid = false;
        if self.statistics.len() == recorder::CAPACITY {
            self.statistics.pop_front();
        }
        self.statistics.push_back(Statistics {
            at: frame.at,
            serial: frame.info.serial,
            kind: if plan.stages.is_empty() {
                PlanKind::Direct
            } else {
                PlanKind::Merged
            },
            stages: plan
                .stages
                .iter()
                .map(|s| {
                    let (kind, tiles) = match s {
                        Stage::Ingest { tiles, .. } => (StageKind::Ingest, tiles),
                        Stage::Video { tiles, .. } => (StageKind::Video, tiles),
                        Stage::Output { tiles, .. } => (StageKind::Output, tiles),
                        _ => unreachable!(),
                    };
                    StageStats {
                        kind,
                        tiles: tiles.rects.len(),
                        pixels: tiles
                            .rects
                            .iter()
                            .map(|r| u64::from(r.w) * u64::from(r.h))
                            .sum(),
                    }
                })
                .collect(),
            passes,
            submissions: u32::from(submitted),
            latched: None,
            shown: None,
        });
        self.last = Some(Last {
            layers,
            damage,
            serial: frame.info.serial,
            targets: plan.targets,
        });
        self.drain();
        Ok(Some(token))
    }
    pub fn drain(&mut self) -> Vec<Presented> {
        let mut presented = vec![];
        let mut released = vec![];
        if let Some(c) = &mut self.compositor {
            c.drain(&mut presented, &mut released);
        }
        for release in released {
            for allocation in &self.allocations {
                if let Some(buffer) = allocation
                    .buffers
                    .iter()
                    .position(|b| b.id == Some(release.image.image().id()))
                {
                    self.pools.release(allocation.id, buffer);
                    self.recorder.record(recorder::Event::Released {
                        pool: allocation.id.0,
                        buffer,
                    });
                }
            }
            release.image.released(release.fence);
        }
        for p in &presented {
            if let Some(s) = self.statistics.iter_mut().find(|s| s.serial == p.serial) {
                s.latched = p.latched.or(s.latched);
                s.shown = p.shown.or(s.shown);
            }
        }
        presented
    }
}
