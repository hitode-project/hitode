//! Planner contract. Inputs describe geometry, damage and capabilities, never
//! image content.

use crate::Rect;
use arrayvec::ArrayVec;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::collections::VecDeque;

/// Damage as a short list of rectangles. More than [`Damage::MAX`] rectangles
/// collapse into their union. `Whole` means unknown or everything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Damage {
    None,
    Rects(ArrayVec<Rect, 4>),
    Whole,
}

impl Damage {
    pub const MAX: usize = 4;
}

/// Tiles the GPU stages run on. Tile size is per stage (the upscaler's
/// workgroup footprint, the output pass's alignment); the list is the damage
/// dilated and rounded to that size.
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

/// One published frame, as the planner sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameInfo {
    pub serial: u64,
    pub canvas: (u32, u32),
    pub base: BaseKind,
    /// Damage since the previous published frame.
    pub damage: Damage,
    pub video: SmallVec<[VideoGeometry; 2]>,
    pub cursor_regions: SmallVec<[Rect; 2]>,
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
    pub rate: u32,
    /// The view in physical pixels, before rotation.
    pub view: (u32, u32),
    /// Panel orientation relative to the view.
    pub rotation: Rotation,
    pub upscaler: Upscaler,
    pub background: [f32; 3],
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

/// Per-device cost model for upscaling tiers: GPU nanoseconds per output
/// megapixel, from timestamps, with the thermal state they were measured in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CostModel {
    pub ns_per_megapixel: SmallVec<[(Tier, f64); 4]>,
    pub thermal: Thermal,
    pub frame_budget_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Thermal {
    Nominal,
    Fair,
    Serious,
    Critical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tier {
    /// The selected model.
    Selected,
    /// A smaller network of the same family.
    Small,
    /// A spatial scaler.
    Spatial,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unrefined {
    /// Canvas coordinates, ordered by the present when last damaged.
    pub rect: Rect,
    pub tier: Tier,
    pub since: u64,
}

/// The executor keeps this ledger alongside its actual allocations. Invalidate
/// it when geometry, scaling, the upscaler or the device changes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PoolState {
    pub pools: Vec<Pool>,
    pub unrefined: Vec<Unrefined>,
    /// Canvas damage, oldest first; bounded by `HISTORY`.
    pub history: VecDeque<Damage>,
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
    /// The upscaler's final layer resamples the virtual doubled image.
    Model,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Base,
    Pool(PoolId),
    Video(u64),
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
    /// Fused compute layers into private intermediates, then the final layer as
    /// a draw into a presentable buffer. Regions carry their tiers.
    Upscale {
        from: Source,
        into: PoolId,
        regions: Vec<(Tiles, Tier)>,
        scale: Scale,
        transform: Transform,
    },
    /// Background refinement of quiet regions with the selected model.
    Refine {
        from: Source,
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
    pub damage: Damage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HostLayer {
    Hud,
    GestureHint,
    Cursor,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layers {
    pub solids: SmallVec<[(u64, Rect, [f32; 4]); 2]>,
    pub base: Layer,
    pub video: SmallVec<[(u64, Layer); 2]>,
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
    /// Canvas damage including video changes and completed refinements.
    pub damage: Damage,
    /// Committed to the ledger only after the frame is presented.
    pub unrefined: Vec<Unrefined>,
    pub layers: Layers,
    pub swap_role: SwapRole,
}

/// Number of past presents retained for buffer-age reconstruction.
pub const HISTORY: usize = 32;
const CANVAS: PoolId = PoolId(0);
const VIDEO: PoolId = PoolId(1);
const OUTPUT: PoolId = PoolId(2);

/// Placeholder input-pixel receptive field radii. Package F replaces these
/// with the kernel generator's exact radii for the shipped models.
pub const MODEL_RADII: [(Upscaler, u32); 4] = [
    (Upscaler::Anime4k, 8),
    (Upscaler::Cunny, 14),
    (Upscaler::Artcnn, 10),
    (Upscaler::K7, 8),
];

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
        self.unrefined.clone_from(&plan.unrefined);
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
        self.unrefined.clear();
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

    fn damage(&self, damage: &Damage, age: Option<u32>) -> Damage {
        let Some(age) = age.filter(|&age| age > 0) else {
            return Damage::Whole;
        };
        let count = (age - 1) as usize;
        if count > self.history.len() {
            return Damage::Whole;
        }
        self.history
            .iter()
            .rev()
            .take(count)
            .fold(damage.clone(), |d, old| d.union(old))
    }
}

impl Damage {
    pub fn rects(rects: impl IntoIterator<Item = Rect>) -> Self {
        let rects = merge(rects.into_iter().collect());
        if rects.is_empty() {
            Self::None
        } else if rects.len() <= Self::MAX {
            Self::Rects(rects.into_iter().collect())
        } else {
            Self::Rects(std::iter::once(rects.into_iter().reduce(union).unwrap()).collect())
        }
    }

    fn list(&self, size: (u32, u32)) -> Vec<Rect> {
        match self {
            Self::None => vec![],
            Self::Whole => vec![whole(size)],
            Self::Rects(rects) => rects
                .iter()
                .filter_map(|&r| intersection(r, whole(size)))
                .collect(),
        }
    }

    pub(crate) fn union(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::Whole, _) | (_, Self::Whole) => Self::Whole,
            (Self::None, d) | (d, Self::None) => d.clone(),
            (Self::Rects(a), Self::Rects(b)) => Self::rects(a.iter().chain(b).copied()),
        }
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
fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect {
        x,
        y,
        w: end_x(a).max(end_x(b)) - x,
        h: end_y(a).max(end_y(b)) - y,
    }
}
fn merge(rects: Vec<Rect>) -> Vec<Rect> {
    let mut out: Vec<Rect> = vec![];
    for mut rect in rects.into_iter().filter(|r| r.w > 0 && r.h > 0) {
        let mut i = 0;
        while i < out.len() {
            if intersection(rect, out[i]).is_some() {
                rect = union(rect, out.remove(i));
                i = 0;
            } else {
                i += 1;
            }
        }
        out.push(rect);
    }
    out.sort_by_key(|r| (r.y, r.x, r.h, r.w));
    out
}

/// Remove a changed/refined rectangle, retaining untouched pieces and their age.
fn subtract(regions: &mut Vec<Unrefined>, cut: Rect) {
    *regions = regions
        .drain(..)
        .flat_map(|region| {
            let r = region.rect;
            let Some(i) = intersection(r, cut) else {
                return vec![region];
            };
            [
                Rect {
                    x: r.x,
                    y: r.y,
                    w: r.w,
                    h: i.y - r.y,
                },
                Rect {
                    x: r.x,
                    y: end_y(i),
                    w: r.w,
                    h: end_y(r) - end_y(i),
                },
                Rect {
                    x: r.x,
                    y: i.y,
                    w: i.x - r.x,
                    h: i.h,
                },
                Rect {
                    x: end_x(i),
                    y: i.y,
                    w: end_x(r) - end_x(i),
                    h: i.h,
                },
            ]
            .into_iter()
            .filter(|r| r.w > 0 && r.h > 0)
            .map(|rect| Unrefined { rect, ..region })
            .collect()
        })
        .collect();
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
fn tiles(
    damage: &Damage,
    canvas: (u32, u32),
    output: (u32, u32),
    rotation: Rotation,
    reach: u32,
    size: (u32, u32),
) -> Tiles {
    let target = swapped(output, rotation);
    let rects = damage
        .list(canvas)
        .into_iter()
        .map(|r| {
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
        })
        .collect();
    Tiles {
        size,
        rects: merge(rects),
    }
}
fn pixels(tiles: &Tiles) -> f64 {
    tiles
        .rects
        .iter()
        .map(|r| f64::from(r.w) * f64::from(r.h))
        .sum()
}
fn predicted(cost: &CostModel, tier: Tier, tiles: &Tiles) -> f64 {
    cost.ns_per_megapixel
        .iter()
        .find(|(t, _)| *t == tier)
        .map_or(
            if tier == Tier::Spatial {
                0.0
            } else {
                f64::INFINITY
            },
            |(_, ns)| ns * pixels(tiles) / 1_000_000.0,
        )
}
fn allowed(tier: Tier, thermal: Thermal) -> bool {
    match thermal {
        Thermal::Critical => tier == Tier::Spatial,
        Thermal::Serious => tier != Tier::Selected,
        Thermal::Nominal | Thermal::Fair => true,
    }
}
/// Decide how to show a frame. `canvas` dimensions must be nonzero. Destinations
/// are in panel pixels; crops and layer damage are in source-image coordinates.
/// The caller acquires the named targets before executing any stage.
pub fn plan(
    frame: &FrameInfo,
    settings: &Settings,
    caps: &CompositorCaps,
    cost: &CostModel,
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
    // Native-size and downscaled views bypass super-resolution (HOST-VIEW-7).
    let upscale =
        settings.upscaler != Upscaler::Off && viewport.w > canvas.0 && viewport.h > canvas.1;
    let output_needed = !upscale
        && ((!integer && prescale != (1, 1))
            || (!caps.rotates && settings.rotation != Rotation::None)
            || ((!caps.scales || (integer && !caps.nearest)) && viewport_size != canvas));
    // This decision deliberately excludes frame damage, videos and pool state.
    let swap_role = if caps.presentable_compressed || (!upscale && !output_needed) {
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
    let mut damage = frame.damage.clone();
    if compose_video {
        damage = damage.union(&Damage::rects(
            frame.video.iter().filter(|v| v.changed).map(|v| v.rect),
        ));
    }
    let size = tile_size(caps);
    let mut result = Plan {
        stages: vec![],
        targets: vec![],
        damage: damage.clone(),
        unrefined: vec![],
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
                        Damage::Whole
                    } else {
                        Damage::None
                    },
                },
            ));
        }
    }
    let mut source = Source::Base;
    if compose_video {
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
        let output = if caps.scales && viewport.w >= canvas.0 * 2 && viewport.h >= canvas.1 * 2 {
            (canvas.0 * 2, canvas.1 * 2)
        } else {
            viewport_size
        };
        let (target, age) = pools.target(OUTPUT, swapped(output, settings.rotation));
        let work = pools.damage(&damage, age);
        let reach = MODEL_RADII
            .iter()
            .find(|(m, _)| *m == settings.upscaler)
            .unwrap()
            .1;
        let mut budget = cost.frame_budget_ns as f64;
        let mut regions = vec![];
        // Merge after dilation/alignment as well, so no target pixel gets two
        // conflicting tier decisions. Back-map these regions to the canvas.
        let affected = tiles(&work, canvas, output, settings.rotation, reach, size);
        let mut unrefined = pools.unrefined.clone();
        for output_rect in affected.rects {
            let tile = Tiles {
                size,
                rects: vec![output_rect],
            };
            let inverse = match settings.rotation {
                Rotation::Quarter => Rotation::ThreeQuarters,
                Rotation::ThreeQuarters => Rotation::Quarter,
                r => r,
            };
            let rect = map(rotate(output_rect, target.size, inverse), output, canvas);
            let tier = [Tier::Selected, Tier::Small, Tier::Spatial]
                .into_iter()
                .find(|&tier| allowed(tier, cost.thermal) && predicted(cost, tier, &tile) <= budget)
                // Spatial is the real-time floor, even when no tier fits.
                .unwrap_or(Tier::Spatial);
            budget = (budget - predicted(cost, tier, &tile)).max(0.0);
            let since = if damage
                .list(canvas)
                .iter()
                .any(|&d| intersection(d, rect).is_some())
            {
                frame.serial
            } else {
                pools
                    .unrefined
                    .iter()
                    .filter(|r| intersection(r.rect, rect).is_some())
                    .map(|r| r.since)
                    .min()
                    .unwrap_or(frame.serial)
            };
            subtract(&mut unrefined, rect);
            if tier != Tier::Selected {
                unrefined.push(Unrefined { rect, tier, since });
            }
            regions.push((tile, tier));
        }
        result.stages.push(Stage::Upscale {
            from: source,
            into: OUTPUT,
            regions,
            transform: Transform {
                rotation: settings.rotation,
                dst: whole(target.size),
            },
            scale: Scale::Model,
        });
        // Only previously quiet work is eligible. Split it into bounded chunks,
        // so a large region can converge over several small frame budgets.
        let mut quiet = unrefined.clone();
        for rect in damage.list(canvas) {
            subtract(&mut quiet, rect);
        }
        quiet.sort_by_key(|r| r.since);
        let mut refined = vec![];
        if allowed(Tier::Selected, cost.thermal) {
            for region in quiet {
                for y in (region.rect.y..end_y(region.rect)).step_by(size.1 as usize) {
                    for x in (region.rect.x..end_x(region.rect)).step_by(size.0 as usize) {
                        let rect = Rect {
                            x,
                            y,
                            w: size.0.min(end_x(region.rect) - x),
                            h: size.1.min(end_y(region.rect) - y),
                        };
                        let tile = tiles(
                            &Damage::rects([rect]),
                            canvas,
                            output,
                            settings.rotation,
                            reach,
                            size,
                        );
                        let ns = predicted(cost, Tier::Selected, &tile);
                        if ns <= budget {
                            budget -= ns;
                            refined.extend(tile.rects);
                            subtract(&mut unrefined, rect);
                            result.damage = result.damage.union(&Damage::rects([rect]));
                        }
                    }
                }
            }
        }
        if !refined.is_empty() {
            result.stages.push(Stage::Refine {
                from: source,
                into: OUTPUT,
                tiles: Tiles {
                    size,
                    rects: merge(refined),
                },
            });
        }
        unrefined.sort_by_key(|r| (r.since, r.rect.y, r.rect.x));
        result.unrefined = unrefined;
        let written = result
            .stages
            .iter()
            .filter_map(|stage| match stage {
                Stage::Upscale { regions, .. } => Some(
                    regions
                        .iter()
                        .flat_map(|(t, _)| t.rects.clone())
                        .collect::<Vec<_>>(),
                ),
                Stage::Refine { tiles, .. } => Some(tiles.rects.clone()),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        result.layers.base = Layer {
            source_uv: None,
            source: Source::Pool(OUTPUT),
            crop: whole(target.size),
            dst,
            rotation: Rotation::None,
            nearest: false,
            damage: Damage::rects(written),
        };
        result.targets.push(target);
    } else if output_needed
        || (source == Source::Base && matches!(frame.base, BaseKind::Gpu { presentable: false }))
    {
        let rotation = if caps.rotates {
            Rotation::None
        } else {
            settings.rotation
        };
        let output = if !caps.scales || (integer && !caps.nearest) {
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
        let scale = if caps.scales {
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
        let reach = u32::from(!caps.scales && !integer);
        let tile = tiles(&work, canvas, output, rotation, reach, size);
        result.layers.base.damage = Damage::rects(tile.rects.clone());
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
        result.layers.base.nearest = integer;
        result.targets.push(target);
    } else if source != Source::Base {
        result.layers.base.source = source;
        if let Stage::Video { tiles, .. } = &result.stages[0] {
            result.layers.base.damage = Damage::rects(tiles.rects.clone());
        }
    } else if matches!(frame.base, BaseKind::Software { .. }) {
        let (target, age) = pools.target(CANVAS, canvas);
        let work = pools.damage(&damage, age);
        let tile = tiles(&work, canvas, canvas, Rotation::None, 0, size);
        result.layers.base.source = Source::Pool(CANVAS);
        result.layers.base.damage = Damage::rects(tile.rects.clone());
        result.stages.push(Stage::Ingest {
            into: CANVAS,
            tiles: tile,
        });
        result.targets.push(target);
    }
    result
}
