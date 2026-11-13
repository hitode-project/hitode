//! Planner contract. Inputs describe geometry, damage and capabilities, never
//! image content.

use crate::Rect;
pub use adv_gpu::Region;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::collections::VecDeque;

/// Tiles the GPU stages run on. Tile size is per stage (the upscaler's
/// workgroup footprint, the output pass's alignment); the list is the damage
/// dilated and rounded to that size, as disjoint rectangles.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tiles {
    pub size: (u32, u32),
    pub rects: Vec<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BaseKind {
    /// A D3D9 or DirectDraw swap buffer. A private allocation needs a copy
    /// until the executor adopts the requested `SwapRole`.
    Gpu { presentable: bool },
    /// A GDI publication slot in GPU-visible memory.
    Software { indexed: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoGeometry {
    pub id: u64,
    pub source: (u32, u32),
    /// Destination in canvas coordinates.
    pub rect: Rect,
    /// True when the decoder wrote a new picture since the last present.
    pub changed: bool,
    pub source_uv: Option<[f32; 4]>,
    pub background: Option<(Rect, [u8; 4])>,
}

/// A guest window over the frame, shown at the host's scale like a native
/// window (WINE-GFX-10).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub id: u32,
    /// Top-left corner in canvas coordinates, possibly outside the canvas.
    pub at: (i32, i32),
    pub size: (u32, u32),
    /// True when its pixels changed since the shown image.
    pub changed: bool,
}

/// One published frame, as the planner sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameInfo {
    pub serial: u64,
    pub canvas: (u32, u32),
    pub base: BaseKind,
    /// Damage since the previous published frame. A region of another size
    /// than the canvas means everything changed.
    pub damage: Region,
    pub video: SmallVec<[VideoGeometry; 2]>,
    pub cursor_regions: SmallVec<[Rect; 2]>,
    /// Bottom first.
    #[serde(default)]
    pub windows: SmallVec<[WindowGeometry; 2]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Upscaler {
    #[default]
    Off,
    Anime4k,
    Cunny,
    Artcnn,
    K7,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Rotation {
    None,
    Quarter,
    Half,
    ThreeQuarters,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Placement {
    #[default]
    Fit,
    Integer,
    Stretch,
}
impl Placement {
    pub fn rectangle(self, client: (f64, f64), canvas: (u32, u32)) -> [f64; 4] {
        let (cw, ch) = client;
        if self == Self::Stretch {
            return [0., 0., cw, ch];
        }
        let scale = (cw / f64::from(canvas.0)).min(ch / f64::from(canvas.1));
        let scale = if self == Self::Integer && scale >= 1. {
            scale.floor()
        } else {
            scale
        };
        let w = (f64::from(canvas.0) * scale).floor();
        let h = (f64::from(canvas.1) * scale).floor();
        [((cw - w) / 2.).floor(), ((ch - h) / 2.).floor(), w, h]
    }
}

/// Window and preference state. Changes invalidate pools (whole damage).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub placement: Placement,
    pub scale: f64,
    /// The view in physical pixels, before rotation.
    pub view: (u32, u32),
    /// Panel orientation relative to the view.
    pub rotation: Rotation,
    pub upscaler: Upscaler,
    pub background: [f32; 3],
    /// Live debugger damage visualization.
    #[serde(default)]
    pub damage_overlay: bool,
}

/// What the system compositor can do for us.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompositorCaps {
    pub fractional_crops: bool,
    /// Layers can scale with linear filtering.
    pub scales: bool,
    /// Layers can also magnify with nearest filtering. Without it (Android's
    /// SurfaceFlinger filters linearly) the output pass does integer scales.
    pub nearest: bool,
    /// Layers can rotate without composition falling back to the GPU.
    pub rotates: bool,
    /// YUV layers are converted and scaled by the display controller.
    /// Core Animation and Android SurfaceControl both support these.
    pub yuv_layers: bool,
    pub solid_color_layers: bool,
    pub damage_hints: bool,
    /// Filled from `adv_gpu::FormatCaps::PRESENTABLE_COMPRESSED`.
    pub presentable_compressed: bool,
    /// `adv_gpu::Limits::tile`, or `(0, 0)` when unknown.
    pub tile: (u32, u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PoolId(pub u8);

/// Age is measured in presents, including presents that did not use this pool.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolBuffer {
    pub age: Option<u32>,
    /// Read by a compositor or by the frame being planned; set by `occupy`.
    pub in_use: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pool {
    pub id: PoolId,
    pub size: (u32, u32),
    pub buffers: Vec<PoolBuffer>,
}

/// The executor keeps this ledger alongside its actual allocations. Invalidate
/// it when geometry, scaling, the upscaler or the device changes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PoolState {
    pub pools: Vec<Pool>,
    /// Canvas damage, oldest first; bounded by `HISTORY`.
    pub history: VecDeque<Region>,
}

/// A stage's chosen allocation. All stages for one pool write this buffer;
/// buffers still owned by a compositor are never selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub pool: PoolId,
    pub buffer: usize,
    pub size: (u32, u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub rotation: Rotation,
    /// Destination rectangle in the target, after rotation.
    pub dst: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scale {
    /// Exact integer scale with nearest sampling.
    Integer { x: u32, y: u32 },
    /// Integer nearest prescale per axis, then linear (sharp-bilinear).
    SharpBilinear { prescale: (u32, u32) },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Base,
    Pool(PoolId),
    Video(u64),
    Window(u32),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Stage {
    /// Decode a software slot into a presentable buffer with a draw.
    Ingest { into: PoolId, tiles: Tiles },
    /// Compose video and the base into the canvas, decoding software inline.
    Video {
        from: Source,
        into: PoolId,
        tiles: Tiles,
    },
    /// Compose the base, and the video when `video`, into the model's private
    /// input, then recompute the doubled image the damage reaches. `into`
    /// ledgers the persistent input and output; tiles are canvas pixels.
    Upscale {
        video: bool,
        into: PoolId,
        tiles: Tiles,
    },
    /// The single output pass: scale, rotate, cover damage only.
    Output {
        from: Source,
        into: PoolId,
        transform: Transform,
        scale: Scale,
        tiles: Tiles,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub source_uv: Option<[f32; 4]>,
    pub source: Source,
    pub crop: Rect,
    pub dst: Rect,
    pub rotation: Rotation,
    pub nearest: bool,
    /// In source-image pixels.
    pub damage: Region,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HostLayer {
    Hud,
    GestureHint,
    Cursor,
    Damage,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layers {
    pub solids: SmallVec<[(u64, Rect, [f32; 4]); 2]>,
    pub base: Layer,
    pub video: SmallVec<[(u64, Layer); 2]>,
    /// Guest windows over the video, bottom first.
    #[serde(default)]
    pub windows: SmallVec<[(u32, Layer); 2]>,
    pub host: SmallVec<[HostLayer; 2]>,
    pub background: [f32; 3],
}

/// What D3D9 swap buffers should be. The only edge from display back to the
/// executor; it changes with settings or rotation, never per frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwapRole {
    /// The plan shows swap buffers by reference: render into presentable buffers.
    Presentable { canvas: (u32, u32) },
    /// A pass reads swap buffers anyway: render into private, compressed images.
    Private,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub stages: Vec<Stage>,
    pub targets: Vec<Target>,
    /// Canvas damage including video changes.
    pub damage: Region,
    pub layers: Layers,
    pub swap_role: SwapRole,
}

/// Number of past presents retained for buffer-age reconstruction.
pub const HISTORY: usize = 32;
const CANVAS: PoolId = PoolId(0);
const VIDEO: PoolId = PoolId(1);
const OUTPUT: PoolId = PoolId(2);
/// The upscaler's private input and doubled output: one persistent buffer.
pub const UPSCALE: PoolId = PoolId(3);

impl PoolState {
    /// Commit all writes once, after the atomic present succeeds. Intermediate
    /// targets age too.
    pub fn after_present(&mut self, plan: &Plan) {
        for pool in &mut self.pools {
            for buffer in &mut pool.buffers {
                buffer.age = buffer.age.map(|age| age.saturating_add(1));
            }
        }
        for target in &plan.targets {
            self.buffer(target).age = Some(1);
        }
        self.history.push_back(plan.damage.clone());
        if self.history.len() > HISTORY {
            self.history.pop_front();
        }
    }

    /// Mark exactly the `leased` buffers in use: those a compositor may read
    /// or the next frame samples, presented or not.
    pub fn occupy(&mut self, leased: impl IntoIterator<Item = Target>) {
        for pool in &mut self.pools {
            for buffer in &mut pool.buffers {
                buffer.in_use = false;
            }
        }
        for target in leased {
            self.buffer(&target).in_use = true;
        }
    }
    fn buffer(&mut self, target: &Target) -> &mut PoolBuffer {
        let pool = if let Some(index) = self.pools.iter().position(|p| p.id == target.pool) {
            &mut self.pools[index]
        } else {
            self.pools.push(Pool {
                id: target.pool,
                size: target.size,
                buffers: vec![],
            });
            self.pools.last_mut().unwrap()
        };
        if pool.size != target.size {
            pool.size = target.size;
            pool.buffers.clear();
        }
        pool.buffers.resize_with(
            pool.buffers.len().max(target.buffer + 1),
            PoolBuffer::default,
        );
        &mut pool.buffers[target.buffer]
    }

    /// Size, rotation, scaling, upscaler changes and device loss discard content
    /// validity. Outstanding compositor ownership survives invalidation.
    pub fn invalidate(&mut self) {
        for pool in &mut self.pools {
            for buffer in &mut pool.buffers {
                buffer.age = None;
            }
        }
        self.history.clear();
    }

    /// The executor trims its free allocations to one. Drop their ledger too;
    /// outstanding images keep their indices until released.
    pub fn trim_to_one(&mut self) {
        self.invalidate();
        for pool in &mut self.pools {
            while pool.buffers.len() > 1 && !pool.buffers.last().unwrap().in_use {
                pool.buffers.pop();
            }
        }
    }

    pub(crate) fn target_age(&self, id: PoolId, size: (u32, u32)) -> Option<u32> {
        self.target(id, size).1
    }

    fn target(&self, id: PoolId, size: (u32, u32)) -> (Target, Option<u32>) {
        let pool = self.pools.iter().find(|p| p.id == id && p.size == size);
        let buffer = pool
            .and_then(|p| {
                p.buffers
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| !b.in_use)
                    .min_by_key(|(_, b)| b.age.unwrap_or(u32::MAX))
                    .map(|(i, _)| i)
            })
            .unwrap_or_else(|| pool.map_or(0, |p| p.buffers.len()));
        let age = pool
            .filter(|p| p.size == size)
            .and_then(|p| p.buffers.get(buffer))
            .and_then(|b| b.age);
        (
            Target {
                pool: id,
                buffer,
                size,
            },
            age,
        )
    }

    fn damage(&self, damage: &Region, age: Option<u32>) -> Region {
        let count = age.filter(|&age| age > 0).map(|age| (age - 1) as usize);
        let Some(count) = count.filter(|&count| count <= self.history.len()) else {
            return Region::whole(damage.size());
        };
        let mut out = damage.clone();
        for old in self.history.iter().rev().take(count) {
            out.union(old);
        }
        out
    }
}

fn whole((w, h): (u32, u32)) -> Rect {
    Rect { x: 0, y: 0, w, h }
}
fn end_x(r: Rect) -> u32 {
    r.x + r.w
}
fn end_y(r: Rect) -> u32 {
    r.y + r.h
}
fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = end_x(a).min(end_x(b));
    let bottom = end_y(a).min(end_y(b));
    (right > x && bottom > y).then(|| Rect {
        x,
        y,
        w: right - x,
        h: bottom - y,
    })
}
fn swapped(size: (u32, u32), rotation: Rotation) -> (u32, u32) {
    if matches!(rotation, Rotation::Quarter | Rotation::ThreeQuarters) {
        (size.1, size.0)
    } else {
        size
    }
}
fn rotate(r: Rect, size: (u32, u32), rotation: Rotation) -> Rect {
    match rotation {
        Rotation::None => r,
        Rotation::Quarter => Rect {
            x: size.1 - end_y(r),
            y: r.x,
            w: r.h,
            h: r.w,
        },
        Rotation::Half => Rect {
            x: size.0 - end_x(r),
            y: size.1 - end_y(r),
            ..r
        },
        Rotation::ThreeQuarters => Rect {
            x: r.y,
            y: size.0 - end_x(r),
            w: r.h,
            h: r.w,
        },
    }
}
fn viewport(canvas: (u32, u32), view: (u32, u32), placement: Placement) -> Rect {
    let [x, y, w, h] = placement.rectangle((view.0 as f64, view.1 as f64), canvas);
    Rect {
        x: x as u32,
        y: y as u32,
        w: w as u32,
        h: h as u32,
    }
}
fn map(r: Rect, from: (u32, u32), to: (u32, u32)) -> Rect {
    let floor = |x, a, b| (u64::from(x) * u64::from(b) / u64::from(a)) as u32;
    let ceil = |x, a, b| (u64::from(x) * u64::from(b)).div_ceil(u64::from(a)) as u32;
    let x = floor(r.x, from.0, to.0);
    let y = floor(r.y, from.1, to.1);
    Rect {
        x,
        y,
        w: ceil(end_x(r), from.0, to.0) - x,
        h: ceil(end_y(r), from.1, to.1) - y,
    }
}
fn tile_size(caps: &CompositorCaps) -> (u32, u32) {
    fn aligned(n: u32) -> u32 {
        if n == 0 {
            return 16;
        }
        let (mut a, mut b) = (16, n);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        16 / a * n
    }
    (aligned(caps.tile.0), aligned(caps.tile.1))
}
/// The region covering exactly `rects`.
fn covering(size: (u32, u32), rects: &[Rect]) -> Region {
    let mut region = Region::empty(size);
    for &r in rects {
        region.add(r.into());
    }
    region
}
fn tiles(
    damage: &Region,
    canvas: (u32, u32),
    output: (u32, u32),
    rotation: Rotation,
    reach: u32,
    size: (u32, u32),
) -> Tiles {
    let target = swapped(output, rotation);
    // Dilated tiles overlap; the region keeps their union exact.
    let mut cover = Region::empty(target);
    for r in damage.rects() {
        let r = Rect::from(r);
        cover.add(
            {
                let x = r.x.saturating_sub(reach);
                let y = r.y.saturating_sub(reach);
                let r = Rect {
                    x,
                    y,
                    w: end_x(r).saturating_add(reach).min(canvas.0) - x,
                    h: end_y(r).saturating_add(reach).min(canvas.1) - y,
                };
                let r = rotate(map(r, canvas, output), output, rotation);
                let x = r.x / size.0 * size.0;
                let y = r.y / size.1 * size.1;
                Rect {
                    x,
                    y,
                    w: end_x(r)
                        .div_ceil(size.0)
                        .saturating_mul(size.0)
                        .min(target.0)
                        - x,
                    h: end_y(r)
                        .div_ceil(size.1)
                        .saturating_mul(size.1)
                        .min(target.1)
                        - y,
                }
            }
            .into(),
        );
    }
    Tiles {
        size,
        rects: cover.rects().into_iter().map(Into::into).collect(),
    }
}
/// Window layers: each at one integer scale near the host's, the largest
/// the view holds, so its pixels stay whole and crisp; its center over the
/// point of the game it covers; inside the view.
fn windows(
    frame: &FrameInfo,
    settings: &Settings,
    panel: (u32, u32),
    viewport: Rect,
) -> SmallVec<[(u32, Layer); 2]> {
    let fits = frame
        .windows
        .iter()
        .map(|w| (panel.0 / w.size.0.max(1)).min(panel.1 / w.size.1.max(1)))
        .min()
        .unwrap_or(1);
    let scale = (settings.scale.round() as u32).min(fits).max(1);
    let center = |at: i32, size: u32, canvas: u32, origin: u32, length: u32| {
        f64::from(origin)
            + (f64::from(at) + f64::from(size) / 2.) * f64::from(length) / f64::from(canvas)
    };
    let place = |center: f64, size: u32, panel: u32| {
        ((center - f64::from(size) / 2.).round().max(0.) as u32).min(panel.saturating_sub(size))
    };
    frame
        .windows
        .iter()
        .map(|w| {
            let (width, height) = (w.size.0 * scale, w.size.1 * scale);
            let x = center(w.at.0, w.size.0, frame.canvas.0, viewport.x, viewport.w);
            let y = center(w.at.1, w.size.1, frame.canvas.1, viewport.y, viewport.h);
            let dst = Rect {
                x: place(x, width, panel.0),
                y: place(y, height, panel.1),
                w: width,
                h: height,
            };
            let layer = Layer {
                source_uv: None,
                source: Source::Window(w.id),
                crop: whole(w.size),
                dst: rotate(dst, panel, settings.rotation),
                rotation: settings.rotation,
                nearest: true,
                damage: if w.changed {
                    Region::whole(w.size)
                } else {
                    Region::empty(w.size)
                },
            };
            (w.id, layer)
        })
        .collect()
}

/// Decide how to show a frame. `canvas` dimensions must be nonzero. Destinations
/// are in panel pixels; crops and layer damage are in source-image coordinates.
/// The caller acquires the named targets before executing any stage.
pub fn plan(
    frame: &FrameInfo,
    settings: &Settings,
    caps: &CompositorCaps,
    pools: &PoolState,
) -> Plan {
    let canvas = frame.canvas;
    let panel = swapped(settings.view, settings.rotation);
    let viewport = viewport(canvas, panel, settings.placement);
    let viewport_size = (viewport.w, viewport.h);
    let dst = rotate(viewport, panel, settings.rotation);
    let integer = viewport.w >= canvas.0
        && viewport.h >= canvas.1
        && viewport.w.is_multiple_of(canvas.0)
        && viewport.h.is_multiple_of(canvas.1);
    let prescale = (
        (viewport.w / canvas.0).max(1),
        (viewport.h / canvas.1).max(1),
    );
    // Native-size, downscaled and 720p+ video frames bypass super-resolution (HOST-VIEW-7).
    let canvas_upscale =
        settings.upscaler != Upscaler::Off && viewport.w > canvas.0 && viewport.h > canvas.1;
    let hd_video = frame
        .video
        .iter()
        .any(|v| u64::from(v.source.0) * u64::from(v.source.1) >= 1280 * 720);
    let upscale = canvas_upscale && !hd_video;
    let needs_output = |upscaled: bool| {
        !upscaled
            && ((!integer && prescale != (1, 1))
                || (!caps.rotates && settings.rotation != Rotation::None)
                || ((!caps.scales || (integer && !caps.nearest)) && viewport_size != canvas))
    };
    let output_needed = needs_output(upscale);
    // This decision deliberately excludes frame damage, videos and pool state.
    let swap_role = if caps.presentable_compressed || (!canvas_upscale && !needs_output(false)) {
        SwapRole::Presentable { canvas }
    } else {
        SwapRole::Private
    };
    let separate_video = !upscale
        && (caps.rotates || settings.rotation == Rotation::None)
        && caps.yuv_layers
        && (caps.fractional_crops
            || frame.video.iter().all(|v| {
                v.source_uv.is_none_or(|uv| {
                    uv.iter().enumerate().all(|(i, n)| {
                        let value = *n * if i % 2 == 0 { v.source.0 } else { v.source.1 } as f32;
                        value == value.round()
                    })
                })
            }))
        && (caps.solid_color_layers || frame.video.iter().all(|v| v.background.is_none()))
        && frame.video.iter().all(|v| {
            frame
                .cursor_regions
                .iter()
                .all(|&c| intersection(c, v.rect).is_none())
        });
    let compose_video = !frame.video.is_empty() && !separate_video;
    let mut damage = if frame.damage.size() == canvas {
        frame.damage.clone()
    } else {
        Region::whole(canvas)
    };
    if compose_video {
        for video in frame.video.iter().filter(|v| v.changed) {
            damage.add(video.rect.into());
        }
    }
    let size = tile_size(caps);
    let mut result = Plan {
        stages: vec![],
        targets: vec![],
        damage: damage.clone(),
        swap_role,
        layers: Layers {
            solids: SmallVec::new(),
            base: Layer {
                source_uv: None,
                source: Source::Base,
                crop: whole(canvas),
                dst,
                rotation: settings.rotation,
                nearest: integer,
                damage: damage.clone(),
            },
            video: SmallVec::new(),
            windows: windows(frame, settings, panel, viewport),
            host: SmallVec::new(),
            background: settings.background,
        },
    };
    if separate_video {
        for video in &frame.video {
            if let Some((r, c)) = video.background {
                let mut r = map(r, canvas, viewport_size);
                r.x += viewport.x;
                r.y += viewport.y;
                result.layers.solids.push((
                    video.id,
                    rotate(r, panel, settings.rotation),
                    [
                        c[2] as f32 / 255.,
                        c[1] as f32 / 255.,
                        c[0] as f32 / 255.,
                        1.,
                    ],
                ));
            }
            let mut video_dst = map(video.rect, canvas, viewport_size);
            video_dst.x += viewport.x;
            video_dst.y += viewport.y;
            result.layers.video.push((
                video.id,
                Layer {
                    source_uv: video.source_uv,
                    source: Source::Video(video.id),
                    crop: whole(video.source),
                    dst: rotate(video_dst, panel, settings.rotation),
                    rotation: settings.rotation,
                    nearest: false,
                    damage: if video.changed {
                        Region::whole(video.source)
                    } else {
                        Region::empty(video.source)
                    },
                },
            ));
        }
    }
    let mut source = Source::Base;
    if compose_video && !upscale {
        let (target, age) = pools.target(VIDEO, canvas);
        let work = pools.damage(&damage, age);
        result.stages.push(Stage::Video {
            from: source,
            into: VIDEO,
            tiles: tiles(&work, canvas, canvas, Rotation::None, 0, size),
        });
        result.targets.push(target);
        source = Source::Pool(VIDEO);
    }
    if upscale {
        let (target, age) = pools.target(UPSCALE, canvas);
        let work = pools.damage(&damage, age);
        result.stages.push(Stage::Upscale {
            video: compose_video,
            into: UPSCALE,
            tiles: tiles(&work, canvas, canvas, Rotation::None, 0, size),
        });
        result.targets.push(target);
        source = Source::Pool(UPSCALE);
    }
    if upscale
        || output_needed
        || (source == Source::Base && matches!(frame.base, BaseKind::Gpu { presentable: false }))
    {
        let rotation = if caps.rotates {
            Rotation::None
        } else {
            settings.rotation
        };
        // The layer magnifies the upscaler's doubled image when the viewport
        // holds it; otherwise this pass resamples it to the viewport.
        let doubled = (canvas.0 * 2, canvas.1 * 2);
        let output = if upscale {
            if caps.scales && viewport.w >= doubled.0 && viewport.h >= doubled.1 {
                doubled
            } else {
                viewport_size
            }
        } else if !caps.scales || (integer && !caps.nearest) {
            viewport_size
        } else if integer {
            canvas
        } else {
            (canvas.0 * prescale.0, canvas.1 * prescale.1)
        };
        let (target, age) = pools.target(OUTPUT, swapped(output, rotation));
        let work = pools.damage(&damage, age);
        // What this pass does. With compositor scaling it only prescales by
        // the integer part (1 when it exists for rotation alone) and the layer
        // does the rest; without it the pass covers the whole viewport.
        let scale = if upscale {
            if output == doubled {
                Scale::Integer { x: 1, y: 1 }
            } else {
                Scale::SharpBilinear { prescale: (1, 1) }
            }
        } else if caps.scales {
            Scale::Integer {
                x: output.0 / canvas.0,
                y: output.1 / canvas.1,
            }
        } else if integer {
            Scale::Integer {
                x: prescale.0,
                y: prescale.1,
            }
        } else {
            Scale::SharpBilinear { prescale }
        };
        let reach = if upscale {
            crate::stages::upscale::reach(settings.upscaler) + u32::from(output != doubled)
        } else {
            u32::from(!caps.scales && !integer)
        };
        let tile = tiles(&work, canvas, output, rotation, reach, size);
        result.layers.base.damage = covering(target.size, &tile.rects);
        result.stages.push(Stage::Output {
            from: source,
            into: OUTPUT,
            transform: Transform {
                rotation,
                dst: whole(target.size),
            },
            scale,
            tiles: tile,
        });
        result.layers.base.source = Source::Pool(OUTPUT);
        result.layers.base.crop = whole(target.size);
        result.layers.base.rotation = if caps.rotates {
            settings.rotation
        } else {
            Rotation::None
        };
        result.layers.base.nearest = integer && !upscale;
        result.targets.push(target);
    } else if source != Source::Base {
        result.layers.base.source = source;
        if let Stage::Video { tiles, .. } = &result.stages[0] {
            result.layers.base.damage = covering(canvas, &tiles.rects);
        }
    } else if matches!(frame.base, BaseKind::Software { .. }) {
        let (target, age) = pools.target(CANVAS, canvas);
        let work = pools.damage(&damage, age);
        let tile = tiles(&work, canvas, canvas, Rotation::None, 0, size);
        result.layers.base.source = Source::Pool(CANVAS);
        result.layers.base.damage = covering(canvas, &tile.rects);
        result.stages.push(Stage::Ingest {
            into: CANVAS,
            tiles: tile,
        });
        result.targets.push(target);
    }
    if settings.damage_overlay {
        result.layers.host.push(HostLayer::Damage);
    }
    result
}
