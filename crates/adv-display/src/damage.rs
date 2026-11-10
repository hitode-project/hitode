//! GPU-only damage histories in an independent transparent compositor layer.
#[cfg(target_os = "macos")]
mod hud;
#[cfg(target_os = "macos")]
pub use hud::DamageHud;

use crate::{
    Rect,
    compositor::{AcquiredImage, LayerId},
    plan::{HostLayer, Layers, Region, Rotation},
    recorder::PresentRecord,
    stages::{bindings, upscale},
};
use adv_gpu::{
    Buffer, ComputePipeline, Device, Image, PresentImage, PresentPool, Queue, RenderPipeline, Ring,
    encode::{Bind, ColorAttachment, ComputePassDesc, Load, RenderPassDesc, Store},
    pipeline::{
        ColorTarget, ComputePipelineDesc, Cull, Primitive, RenderPipelineDesc, Stages, Topology,
    },
    resource::{BufferDesc, BufferUsage, Memory},
    shader::{ShaderDesc, Source as ShaderSource, Stage},
};

pub(crate) struct Damage {
    canvas: (u32, u32),
    view: (u32, u32),
    unit: u32,
    output_unit: u32,
    history: Buffer,
    #[cfg(target_os = "macos")]
    clock: std::time::Instant,
    age: ComputePipeline,
    mark: ComputePipeline,
    tint: RenderPipeline,
    ring: Ring,
    pool: Option<PresentPool>,
}
impl Damage {
    fn new(
        device: &Device,
        canvas: (u32, u32),
        view: (u32, u32),
        output_unit: u32,
    ) -> adv_gpu::Result<Self> {
        let mut unit = 8;
        for model in upscale::models::REFERENCE
            .iter()
            .chain(&upscale::models::FUSED)
            .chain(&upscale::models::MATRIX)
        {
            let last = model.passes.last().unwrap();
            while !(last.block / last.scale).is_multiple_of(unit) {
                unit /= 2;
            }
        }
        let shader = device.create_shader(ShaderDesc {
            source: ShaderSource::Precompiled {
                spirv: &include!(concat!(env!("OUT_DIR"), "/damage.spv.rs")),
                metal: include_bytes!(concat!(env!("OUT_DIR"), "/damage.msl")),
                entries: &include!(concat!(env!("OUT_DIR"), "/damage.entries.rs")),
            },
            identity: include!(concat!(env!("OUT_DIR"), "/damage.identity.rs")),
            label: "damage overlay",
        })?;
        let compute = |entry| {
            device.create_compute_pipeline(
                &ComputePipelineDesc {
                    entry: shader.entry(entry, Stage::Compute)?,
                    bindings: bindings::DAMAGE
                        .into_iter()
                        .map(|mut b| {
                            b.stages = Stages::COMPUTE;
                            b
                        })
                        .collect(),
                    label: "damage history",
                },
                None,
            )
        };
        let tint = device.create_render_pipeline(
            &RenderPipelineDesc {
                vertex: shader.entry("tint_vs", Stage::Vertex)?,
                fragment: Some(shader.entry(
                    if cfg!(target_os = "macos") {
                        "contour_fs"
                    } else {
                        "tint_fs"
                    },
                    Stage::Fragment,
                )?),
                bindings: bindings::DAMAGE[..2]
                    .iter()
                    .copied()
                    .map(|mut b| {
                        b.stages = Stages::FRAGMENT;
                        b
                    })
                    .collect(),
                primitive: Primitive {
                    topology: Topology::Triangles,
                    cull: Cull::None,
                    front_ccw: true,
                    wireframe: false,
                    unclipped_depth: false,
                },
                colors: [ColorTarget {
                    format: adv_gpu::present::COLOR_FORMAT,
                    blend: None,
                    write_mask: 15,
                }]
                .into_iter()
                .collect(),
                depth_stencil: None,
                samples: 1,
                alpha_to_coverage: false,
                label: "damage tints",
            },
            None,
        )?;
        Ok(Self {
            canvas,
            view,
            unit,
            output_unit,
            #[cfg(target_os = "macos")]
            clock: std::time::Instant::now(),
            history: device.create_buffer(&BufferDesc {
                size: 16
                    + u64::from(canvas.0.div_ceil(unit)) * u64::from(canvas.1.div_ceil(unit)) * 16,
                usage: BufferUsage::STORAGE | BufferUsage::COPY_DST | BufferUsage::COPY_SRC,
                memory: Memory::Device,
                label: "damage history",
            })?,
            age: compute("age")?,
            mark: compute("output_cells")?,
            tint,
            ring: device.ring(4 << 20)?,
            pool: None,
        })
    }
    fn encode(
        &mut self,
        queue: &mut Queue,
        target: &Image,
        record: &PresentRecord,
        upscaling: Option<&upscale::Upscaling>,
        initialize: bool,
        #[cfg(target_os = "macos")] sample: Option<&mut hud::Sample>,
    ) -> adv_gpu::Result<adv_gpu::Encoder> {
        let has_upscale = upscaling.is_some();
        let upscaling = upscaling.filter(|_| record.plan.stages.iter().any(|s| matches!(s, crate::plan::Stage::Upscale { tiles, .. } if !tiles.rects.is_empty())));
        let mut encoder = queue.encoder("damage overlay");
        #[cfg(target_os = "macos")]
        let readback = sample.as_ref().and_then(|sample| sample.output.as_ref());
        #[cfg(not(target_os = "macos"))]
        let count_output = false;
        #[cfg(target_os = "macos")]
        let count_output = readback.is_some();
        if initialize {
            encoder.fill_buffer(self.history.slice(0..self.history.desc().size), 0);
        } else if count_output {
            encoder.fill_buffer(self.history.slice(0..4), 0);
        }
        let mut producer = record.frame.damage.clone();
        if producer.size() != self.canvas {
            producer = Region::whole(self.canvas);
        }
        for v in record.frame.video.iter().filter(|v| v.changed) {
            producer.add(v.rect.into());
        }
        let stride = self.canvas.0.div_ceil(8).div_ceil(32);
        let mut bits = vec![0u32; (stride * self.canvas.1.div_ceil(8)) as usize];
        let rects = producer.rects();
        for rect in &rects {
            for y in rect.y / 8..(rect.y + rect.h).div_ceil(8) {
                for x in rect.x / 8..(rect.x + rect.w).div_ceil(8) {
                    bits[(y * stride + x / 32) as usize] |= 1 << (x % 32);
                }
            }
        }
        let mut uploaded = self.ring.reserve(bits.len() as u64 * 4, 256)?;
        for (out, value) in uploaded.bytes().as_chunks_mut::<4>().0.iter_mut().zip(bits) {
            out.copy_from_slice(&value.to_ne_bytes());
        }
        let mut constants = self.ring.reserve(64, 256)?;
        let dst = record.plan.layers.base.dst;
        let rotation = match record.settings.rotation {
            Rotation::None => 0,
            Rotation::Quarter => 1,
            Rotation::Half => 2,
            Rotation::ThreeQuarters => 3,
        };
        let values = [
            self.canvas.0,
            self.canvas.1,
            self.canvas.0.div_ceil(self.unit),
            self.canvas.1.div_ceil(self.unit),
            self.unit,
            self.output_unit,
            u32::from(upscaling.is_some())
                | (u32::from(has_upscale) << 1)
                | (u32::from(count_output) << 2),
            rotation,
        ];
        for (out, value) in constants.bytes()[..32]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(values)
        {
            out.copy_from_slice(&value.to_ne_bytes());
        }
        #[cfg(target_os = "macos")]
        let mark_time = (self.clock.elapsed().as_millis() as u32).saturating_add(1);
        #[cfg(not(target_os = "macos"))]
        let mark_time = 0u32;
        constants.bytes()[32..36].copy_from_slice(&mark_time.to_ne_bytes());
        constants.bytes()[36..40].copy_from_slice(&(record.settings.scale as f32).to_ne_bytes());
        for (out, value) in constants.bytes()[48..]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip([dst.x as f32, dst.y as f32, dst.w as f32, dst.h as f32])
        {
            out.copy_from_slice(&value.to_ne_bytes());
        }
        let uniform = constants
            .buffer()
            .slice(constants.offset()..constants.offset() + constants.len());
        {
            let mut pass = encoder.compute_pass(&ComputePassDesc {
                timestamps: None,
                label: "damage history",
            });
            pass.set_pipeline(&self.age);
            pass.bind(1, Bind::Uniform(uniform));
            pass.bind(
                4,
                Bind::StorageBuffer(self.history.slice(0..self.history.desc().size)),
            );
            let source = uploaded
                .buffer()
                .slice(uploaded.offset()..uploaded.offset() + uploaded.len());
            pass.bind(5, Bind::StorageBuffer(source));
            pass.bind(
                6,
                Bind::StorageBuffer(upscaling.map_or(source, |u| u.damage_delta())),
            );
            pass.bind(7, Bind::StorageBuffer(source));
            pass.dispatch(
                (self.canvas.0.div_ceil(self.unit) * self.canvas.1.div_ceil(self.unit))
                    .div_ceil(64),
                1,
                1,
            );
            if let Some(u) = upscaling {
                pass.set_pipeline(&self.mark);
                for (cells, arguments) in u.damage_cells() {
                    pass.bind(7, Bind::StorageBuffer(cells));
                    pass.dispatch_indirect(arguments);
                }
            }
        }
        let whole = Rect {
            x: 0,
            y: 0,
            w: self.view.0,
            h: self.view.1,
        };
        {
            let mut pass = encoder.render_pass(&RenderPassDesc {
                colors: [ColorAttachment {
                    view: target.view(),
                    load: Load::Clear([0.; 4]),
                    store: Store::Store,
                    resolve: None,
                }]
                .into_iter()
                .collect(),
                depth_stencil: None,
                area: whole.into(),
                timestamps: None,
                label: "damage overlay",
            });
            pass.set_pipeline(&self.tint);
            pass.bind(1, Bind::Uniform(uniform));
            pass.bind(
                4,
                Bind::StorageBuffer(self.history.slice(0..self.history.desc().size)),
            );
            pass.set_viewport(0., 0., self.view.0 as f32, self.view.1 as f32, 0., 1.);
            pass.draw(0..3, 0..1);
        }
        #[cfg(target_os = "macos")]
        if let Some(readback) = readback {
            encoder.copy_buffer(self.history.slice(0..4), readback, 0);
        }
        #[cfg(target_os = "macos")]
        if let Some(sample) = sample {
            sample.producer = rects.iter().map(|r| u64::from(r.w) * u64::from(r.h)).sum();
        }
        encoder.retain(constants);
        encoder.retain(uploaded);
        Ok(encoder)
    }
}

/// Prepare only the host image and host-layer entry at presentation hand-off.
pub(crate) fn prepare(
    state: &mut Option<Damage>,
    context: (&Device, &mut Queue),
    frame: (&PresentRecord, bool, Option<&upscale::Upscaling>),
    layers: &mut Layers,
    images: &mut Vec<AcquiredImage>,
    screen: &[(u64, AcquiredImage)],
    #[cfg(target_os = "macos")] hud: Option<&DamageHud>,
) -> adv_gpu::Result<()> {
    let (device, queue) = context;
    let (record, enabled, upscaling) = frame;
    if !enabled {
        *state = None;
        layers.host.retain(|h| *h != HostLayer::Damage);
        for acquired in images.extract_if(.., |a| a.layer == LayerId::Host(HostLayer::Damage)) {
            acquired.image.released(None);
        }
        return Ok(());
    }
    if !layers.host.contains(&HostLayer::Damage) {
        layers.host.push(HostLayer::Damage);
    }
    // Rebuilding reattaches the existing image just as it reattaches the picture.
    if images
        .iter()
        .any(|a| a.layer == LayerId::Host(HostLayer::Damage))
    {
        return Ok(());
    }
    let canvas = record.frame.canvas;
    let view = record.settings.view;
    let output_unit = upscaling.map_or(8, |u| u.damage_unit());
    let initialize = state.as_ref().is_none_or(|s| s.canvas != canvas);
    if state
        .as_ref()
        .is_none_or(|s| (s.canvas, s.view) != (canvas, view))
    {
        let mut next = Damage::new(device, canvas, view, output_unit)?;
        if let Some(previous) = state.take().filter(|s| s.canvas == canvas) {
            next.history = previous.history;
            #[cfg(target_os = "macos")]
            {
                next.clock = previous.clock;
            }
        }
        *state = Some(next);
    }
    let gpu = state.as_mut().unwrap();
    gpu.output_unit = output_unit;
    if gpu.pool.is_none() {
        gpu.pool = Some(device.create_present_pool(&adv_gpu::present::PoolDesc {
            color: None,
            width: view.0,
            height: view.1,
            format: adv_gpu::present::COLOR_FORMAT,
            count: 3,
            storage: false,
            label: "damage overlay",
        })?);
    }
    let pool = gpu.pool.as_mut().unwrap();
    let index = (0..)
        .find(|index| {
            !screen
                .iter()
                .any(|(_, a)| a.image.pool().id() == pool.id() && a.image.index() == *index)
        })
        .unwrap();
    let mut image = pool.acquire_index(queue, index, true)?.unwrap();
    #[cfg(target_os = "macos")]
    let mut sample = hud
        .map(|hud| hud.sample(device, upscaling.is_some()))
        .transpose()?
        .flatten();
    let encoder = gpu.encode(
        queue,
        image.image(),
        record,
        upscaling,
        initialize,
        #[cfg(target_os = "macos")]
        sample.as_mut(),
    )?;
    let token = queue.submit_present(encoder, &mut [&mut image])?;
    #[cfg(target_os = "macos")]
    if let Some(sample) = sample {
        sample.finish(queue, token, canvas);
    }
    images.push(AcquiredImage {
        layer: LayerId::Host(HostLayer::Damage),
        image,
        acquire: token,
    });
    Ok(())
}

#[cfg(any(target_vendor = "apple", target_os = "android"))]
pub(crate) fn layer(size: (u32, u32)) -> crate::plan::Layer {
    let whole = Rect {
        x: 0,
        y: 0,
        w: size.0,
        h: size.1,
    };
    crate::plan::Layer {
        source_uv: None,
        source: crate::plan::Source::Base,
        crop: whole,
        dst: whole,
        rotation: Rotation::None,
        nearest: true,
        damage: Region::whole(size),
    }
}

pub(crate) fn released(image: PresentImage, fence: Option<adv_gpu::queue::Fence>) {
    image.released(fence);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{plan::*, recorder::Statistics};
    use adv_gpu::{
        encode::{BufferCopy, ImageCopy},
        resource::*,
        *,
    };
    use std::time::Duration;

    fn open() -> (Device, Queue) {
        Device::open(&queue::DeviceOptions {
            software: !cfg!(any(target_vendor = "apple", target_os = "android")),
            validation: !cfg!(target_os = "android"),
            ..Default::default()
        })
        .unwrap()
    }
    fn image(device: &Device, size: (u32, u32)) -> Image {
        device
            .create_image(&ImageDesc {
                dimension: Dimension::D2,
                size: Extent {
                    width: size.0,
                    height: size.1,
                    depth_or_layers: 1,
                },
                format: adv_gpu::present::COLOR_FORMAT,
                mips: 1,
                samples: 1,
                usage: Usage::COLOR | Usage::SAMPLED | Usage::COPY_SRC,
                view_formats: &[],
                label: "damage fixture",
            })
            .unwrap()
    }
    fn fill(queue: &mut Queue, image: &Image, color: [f64; 4]) {
        let mut encoder = queue.encoder("damage fixture");
        let size = image.desc().size;
        {
            let _pass = encoder.render_pass(&RenderPassDesc {
                colors: [ColorAttachment {
                    view: image.view(),
                    load: Load::Clear(color),
                    store: Store::Store,
                    resolve: None,
                }]
                .into_iter()
                .collect(),
                depth_stencil: None,
                area: adv_gpu::Rect {
                    x: 0,
                    y: 0,
                    w: size.width,
                    h: size.height,
                },
                timestamps: None,
                label: "damage fixture",
            });
        }
        queue.submit(encoder).unwrap();
    }
    fn read(device: &Device, queue: &mut Queue, image: &Image) -> Vec<u8> {
        let size = image.desc().size;
        let buffer = device
            .create_buffer(&BufferDesc {
                size: u64::from(size.width * size.height) * 4,
                usage: BufferUsage::COPY_DST,
                memory: Memory::Readback,
                label: "damage readback",
            })
            .unwrap();
        let mut encoder = queue.encoder("damage readback");
        encoder.copy_image_to_buffer(
            ImageCopy {
                image,
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            BufferCopy {
                buffer: &buffer,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            size,
        );
        let token = queue.submit(encoder).unwrap();
        assert!(queue.wait(token, Duration::from_secs(30)).unwrap());
        // SAFETY: the bounded wait completed the copy.
        unsafe { buffer.mapped().unwrap().as_ref() }.to_vec()
    }
    fn record(
        canvas: (u32, u32),
        view: (u32, u32),
        rotation: Rotation,
        placement: Placement,
    ) -> PresentRecord {
        let settings = Settings {
            placement,
            scale: 1.,
            view,
            rotation,
            upscaler: Upscaler::Off,
            background: [0.; 3],
            damage_overlay: true,
        };
        let frame = FrameInfo {
            serial: 1,
            canvas,
            base: BaseKind::Gpu { presentable: true },
            damage: Region::empty(canvas),
            video: Default::default(),
            cursor_regions: Default::default(),
            windows: Default::default(),
        };
        let caps = CompositorCaps {
            fractional_crops: true,
            scales: true,
            nearest: true,
            rotates: true,
            yuv_layers: true,
            solid_color_layers: true,
            damage_hints: true,
            presentable_compressed: true,
            tile: (16, 16),
        };
        let pools = PoolState::default();
        let plan = plan(&frame, &settings, &caps, &pools);
        PresentRecord {
            statistics: Statistics::default(),
            token: 0,
            published_at: Duration::ZERO,
            at: Duration::ZERO,
            cpu_time: Duration::ZERO,
            frame,
            settings,
            caps,
            pools,
            plan,
            stage_times: vec![],
        }
    }
    fn draw(
        gpu: &mut Damage,
        device: &Device,
        queue: &mut Queue,
        record: &PresentRecord,
        upscale: Option<&upscale::Upscaling>,
        initialize: bool,
    ) -> Vec<u8> {
        let target = image(device, gpu.view);
        let encoder = gpu
            .encode(
                queue,
                &target,
                record,
                upscale,
                initialize,
                #[cfg(target_os = "macos")]
                None,
            )
            .unwrap();
        queue.submit(encoder).unwrap();
        read(device, queue, &target)
    }

    #[test]
    // spec: HOST-VIEW-12
    fn producer_tint_maps_to_the_view_and_fades_to_clean_pixels() {
        let (device, mut queue) = open();
        let canvas = (32, 16);
        let view = (81, 63);
        for color in [[0.02, 0.03, 0.04, 1.], [0.95, 0.96, 0.97, 1.]] {
            for rotation in [
                Rotation::None,
                Rotation::Quarter,
                Rotation::Half,
                Rotation::ThreeQuarters,
            ] {
                for placement in [Placement::Fit, Placement::Integer, Placement::Stretch] {
                    let mut record = record(canvas, view, rotation, placement);
                    let mut gpu = Damage::new(&device, canvas, view, 8).unwrap();
                    let clean = draw(&mut gpu, &device, &mut queue, &record, None, true);
                    record.frame.damage.add(adv_gpu::Rect {
                        x: 8,
                        y: 0,
                        w: 8,
                        h: 8,
                    });
                    let tinted = draw(&mut gpu, &device, &mut queue, &record, None, false);
                    let dst = record.plan.layers.base.dst;
                    let mut inside_count = 0;
                    for y in 0..view.1 {
                        for x in 0..view.0 {
                            let uv = (
                                (x as f32 + 0.5 - dst.x as f32) / dst.w as f32,
                                (y as f32 + 0.5 - dst.y as f32) / dst.h as f32,
                            );
                            let (u, v) = match rotation {
                                Rotation::None => uv,
                                Rotation::Quarter => (uv.1, 1. - uv.0),
                                Rotation::Half => (1. - uv.0, 1. - uv.1),
                                Rotation::ThreeQuarters => (1. - uv.1, uv.0),
                            };
                            // A pixel center exactly on a cell edge may rasterize to
                            // either side.
                            let edge = [u - 0.25, u - 0.5, v - 0.5].iter().any(|d| d.abs() < 1e-4);
                            if edge {
                                continue;
                            }
                            let inside = (0.25..0.5).contains(&u) && (0.0..0.5).contains(&v);
                            let at = ((y * view.0 + x) * 4) as usize;
                            let marked = tinted[at..at + 4] != clean[at..at + 4];
                            if cfg!(target_os = "macos") {
                                assert!(!marked || inside, "{rotation:?} {placement:?} at {x},{y}");
                            } else {
                                assert_eq!(marked, inside, "{rotation:?} {placement:?} at {x},{y}");
                            }
                            if marked {
                                let alpha = f64::from(tinted[at + 3]) / 255.;
                                assert!(alpha > 0.);
                                assert!((0..3).any(|channel| {
                                    (f64::from(tinted[at + channel]) / 255.
                                        + color[channel] * (1. - alpha)
                                        - color[channel])
                                        .abs()
                                        > 0.02
                                }));
                            }
                            inside_count += usize::from(inside);
                        }
                    }
                    assert!(inside_count > 0);
                    record.frame.damage = Region::empty(canvas);
                    #[cfg(target_os = "macos")]
                    {
                        gpu.clock -= Duration::from_millis(330);
                        assert_eq!(
                            draw(&mut gpu, &device, &mut queue, &record, None, false),
                            clean
                        );
                    }
                    #[cfg(not(target_os = "macos"))]
                    for age in 1..=8 {
                        let faded = draw(&mut gpu, &device, &mut queue, &record, None, false);
                        if age == 8 {
                            assert_eq!(faded, clean);
                        } else {
                            assert_ne!(faded, clean);
                        }
                    }
                }
            }
        }
    }

    #[test]
    // spec: HOST-VIEW-12
    fn upscale_delta_and_compacted_output_are_marked_without_s_only_cells() {
        let (device, mut queue) = open();
        let canvas = (96, 32);
        let view = (192, 64);
        let mut record = record(canvas, view, Rotation::None, Placement::Fit);
        record.settings.upscaler = Upscaler::Anime4k;
        record.frame.damage = Region::whole(canvas);
        record.plan = plan(&record.frame, &record.settings, &record.caps, &record.pools);
        let u =
            upscale::Upscaling::with_precision(&device, upscale::models::FUSED[0], canvas, false)
                .unwrap();
        let mut ring = device.ring(4 << 20).unwrap();
        let mut run = |queue: &mut Queue, color: [f64; 4]| {
            fill(queue, u.input(), color);
            let mut encoder = queue.encoder("damage upscale fixture");
            u.encode(
                (&mut encoder, &mut ring),
                None,
                upscale::Input::Composed,
                None,
            )
            .unwrap();
            queue.submit(encoder).unwrap();
        };
        run(&mut queue, [0.1, 0.2, 0.3, 1.]);
        // The other input image holds nothing yet: compose the same frame.
        fill(&mut queue, u.input(), [0.1, 0.2, 0.3, 1.]);
        // Change one input cell while S conservatively covers the whole canvas.
        let upload = device
            .create_buffer(&BufferDesc {
                size: 8 * 8 * 8,
                usage: BufferUsage::COPY_SRC,
                memory: Memory::Upload,
                label: "damage changed cell",
            })
            .unwrap();
        // SAFETY: this upload is new and not submitted yet.
        unsafe { upload.mapped().unwrap().as_mut() }
            .copy_from_slice(&[0x00, 0x3c, 0x00, 0x3c, 0x00, 0x3c, 0x00, 0x3c].repeat(64));
        let mut encoder = queue.encoder("damage changed cell");
        encoder.copy_buffer_to_image(
            BufferCopy {
                buffer: &upload,
                offset: 0,
                row_pitch: 0,
                rows: 0,
            },
            ImageCopy {
                image: u.input(),
                mip: 0,
                layer: 0,
                origin: (0, 0, 0),
            },
            Extent {
                width: 8,
                height: 8,
                depth_or_layers: 1,
            },
        );
        u.encode(
            (&mut encoder, &mut ring),
            None,
            upscale::Input::Composed,
            None,
        )
        .unwrap();
        queue.submit(encoder).unwrap();
        let mut gpu = Damage::new(&device, canvas, view, u.damage_unit()).unwrap();
        let pixels = draw(&mut gpu, &device, &mut queue, &record, Some(&u), true);
        // Compare against S alone: the distant unchanged cell must be identical.
        let mut s_only = Damage::new(&device, canvas, view, u.damage_unit()).unwrap();
        let producer = draw(&mut s_only, &device, &mut queue, &record, None, true);
        for y in 0..16 {
            for x in 160..176 {
                let at = ((y * view.0 + x) * 4) as usize;
                assert_eq!(pixels[at..at + 4], producer[at..at + 4]);
            }
        }
        let mut delta_tint = false;
        let mut output_tint = false;
        for y in 0..16 {
            for x in 0..40 {
                let at = ((y * view.0 + x) * 4) as usize;
                let stripe = (x + y) % 6 / 2;
                if !cfg!(target_os = "macos") && x < 16 && stripe == 1 {
                    delta_tint |= pixels[at + 1] > producer[at + 1];
                }
                if !cfg!(target_os = "macos") && x >= 16 && stripe == 1 {
                    assert_eq!(
                        pixels[at..at + 4],
                        producer[at..at + 4],
                        "delta leaked into an unchanged cell"
                    );
                }
                if x >= 16 && (cfg!(target_os = "macos") || stripe == 2) {
                    output_tint |= pixels[at..at + 4] != producer[at..at + 4];
                }
            }
        }
        if !cfg!(target_os = "macos") {
            assert!(delta_tint, "changed input cell has no green delta tint");
        }
        assert!(
            output_tint,
            "recomputed output outside delta has no blue tint"
        );
        // The empty-S plan must age histories without reusing stale delta/lists.
        record.frame.damage = Region::empty(canvas);
        record.plan = plan(
            &record.frame,
            &record.settings,
            &record.caps,
            &PoolState {
                pools: vec![Pool {
                    id: UPSCALE,
                    size: canvas,
                    buffers: vec![PoolBuffer {
                        age: Some(1),
                        in_use: false,
                    }],
                }],
                history: Default::default(),
            },
        );
        #[cfg(target_os = "macos")]
        {
            gpu.clock -= Duration::from_millis(350);
        }
        #[cfg(not(target_os = "macos"))]
        for _ in 0..8 {
            draw(&mut gpu, &device, &mut queue, &record, Some(&u), false);
        }
        let faded = draw(&mut gpu, &device, &mut queue, &record, Some(&u), false);
        let mut clean = Damage::new(&device, canvas, view, u.damage_unit()).unwrap();
        assert_eq!(
            faded,
            draw(&mut clean, &device, &mut queue, &record, None, true)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    // spec: HOST-VIEW-12
    fn hud_counts_clipped_output_cells_and_clears_the_count_without_work() {
        let (device, mut queue) = open();
        let canvas = (97, 33);
        let view = (194, 66);
        let mut record = record(canvas, view, Rotation::None, Placement::Fit);
        record.settings.upscaler = Upscaler::Anime4k;
        record.frame.damage = Region::whole(canvas);
        record.plan = plan(&record.frame, &record.settings, &record.caps, &record.pools);
        let u =
            upscale::Upscaling::with_precision(&device, upscale::models::FUSED[0], canvas, false)
                .unwrap();
        fill(&mut queue, u.input(), [0.1, 0.2, 0.3, 1.]);
        let mut ring = device.ring(4 << 20).unwrap();
        let mut encoder = queue.encoder("damage HUD fixture");
        u.encode(
            (&mut encoder, &mut ring),
            None,
            upscale::Input::Composed,
            None,
        )
        .unwrap();
        queue.submit(encoder).unwrap();
        let target = image(&device, view);
        let mut gpu = Damage::new(&device, canvas, view, u.damage_unit()).unwrap();
        for whole in [true, false] {
            if !whole {
                record.frame.damage = Region::empty(canvas);
                for stage in &mut record.plan.stages {
                    if let crate::plan::Stage::Upscale { tiles, .. } = stage {
                        tiles.rects.clear();
                    }
                }
            }
            let hud = DamageHud::default();
            hud.observe(|_| {});
            hud.set_enabled(true);
            let mut sample = hud.sample(&device, true).unwrap().unwrap();
            let encoder = gpu
                .encode(
                    &mut queue,
                    &target,
                    &record,
                    Some(&u),
                    whole,
                    Some(&mut sample),
                )
                .unwrap();
            let token = queue.submit(encoder).unwrap();
            assert!(queue.wait(token, Duration::from_secs(5)).unwrap());
            let buffer = sample.output.as_ref().unwrap();
            // SAFETY: The submission that copied the area count has completed.
            let bytes = unsafe { buffer.mapped().unwrap().as_ref() };
            let expected = if whole { canvas.0 * canvas.1 } else { 0 };
            assert_eq!(sample.producer, u64::from(expected));
            assert_eq!(u32::from_ne_bytes(bytes[..4].try_into().unwrap()), expected);
            if whole {
                let pixels = read(&device, &mut queue, &target);
                let at = |x, y| ((y * view.0 + x) * 4 + 3) as usize;
                assert_ne!(pixels[at(0, 0)], 0, "the full-frame contour is missing");
                assert_eq!(
                    pixels[at(view.0 / 2, view.1 / 2)],
                    0,
                    "the frame interior is tinted"
                );
            }
        }
    }
}
