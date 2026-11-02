//! Planner contract. Inputs describe geometry, damage and capabilities, never
//! image content.

use crate::Rect;
use arrayvec::ArrayVec;
use smallvec::SmallVec;

/// Damage as a short list of rectangles. More than [`Damage::MAX`] rectangles
/// collapse into their union. `Whole` means unknown or everything.
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tiles {
    pub size: u32,
    pub rects: Vec<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseKind {
    /// A D3D9 or DirectDraw swap buffer.
    Gpu { presentable: bool },
    /// A GDI publication slot in GPU-visible memory.
    Software { indexed: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoGeometry {
    pub id: u64,
    pub source: (u32, u32),
    /// Destination in canvas coordinates.
    pub rect: Rect,
    /// True when the decoder wrote a new picture since the last present.
    pub changed: bool,
}

/// One published frame, as the planner sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameInfo {
    pub serial: u64,
    pub canvas: (u32, u32),
    pub base: BaseKind,
    /// Damage since the previous published frame.
    pub damage: Damage,
    pub video: SmallVec<[VideoGeometry; 2]>,
    pub cursor_regions: SmallVec<[Rect; 2]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Upscaler {
    #[default]
    Off,
    Anime4k,
    Cunny,
    Artcnn,
    K7,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rotation {
    None,
    Quarter,
    Half,
    ThreeQuarters,
}

/// Window and preference state. Changes invalidate pools (whole damage).
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The view in physical pixels, before rotation.
    pub view: (u32, u32),
    /// Panel orientation relative to the view.
    pub rotation: Rotation,
    pub upscaler: Upscaler,
    pub background: [f32; 3],
}

/// What the system compositor can do for us.
#[derive(Clone, Debug, PartialEq)]
pub struct CompositorCaps {
    /// Layers can scale with nearest or linear filtering.
    pub scales: bool,
    /// Layers can rotate without composition falling back to the GPU.
    pub rotates: bool,
    /// YUV layers are converted and scaled by the display controller.
    pub yuv_layers: bool,
    pub solid_color_layers: bool,
    pub damage_hints: bool,
}

/// Per-device cost model for upscaling tiers: GPU nanoseconds per output
/// megapixel, from timestamps, with the thermal state they were measured in.
#[derive(Clone, Debug, PartialEq)]
pub struct CostModel {
    pub ns_per_megapixel: SmallVec<[(Tier, f64); 4]>,
    pub thermal: Thermal,
    pub frame_budget_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Thermal {
    Nominal,
    Fair,
    Serious,
    Critical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tier {
    /// The selected model.
    Selected,
    /// A smaller network of the same family.
    Small,
    /// A spatial scaler.
    Spatial,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PoolId(pub u8);

/// Bookkeeping the planner reads: each pool's buffer ages and the tiers of
/// regions still awaiting refinement.
/// Age of each buffer in a pool; `None` when its content is undefined.
pub type BufferAges = SmallVec<[Option<u32>; 4]>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PoolState {
    pub pools: SmallVec<[(PoolId, BufferAges); 4]>,
    pub unrefined: SmallVec<[(Rect, Tier); 8]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub rotation: Rotation,
    /// Destination rectangle in the target, after rotation.
    pub dst: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    /// Exact integer scale with nearest sampling.
    Integer(u32),
    /// Integer nearest prescale per axis, then linear (sharp-bilinear).
    SharpBilinear { prescale: (u32, u32) },
    /// The upscaler's final layer resamples the virtual doubled image.
    Model,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Base,
    Pool(PoolId),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    /// Decode a software slot into a presentable buffer with a draw.
    Ingest { into: PoolId, tiles: Tiles },
    /// Compose video into the canvas (upscaler on).
    Video { into: PoolId, tiles: Tiles },
    /// Fused compute layers into private intermediates, then the final layer as
    /// a draw into a presentable buffer. Regions carry their tiers.
    Upscale {
        from: Source,
        into: PoolId,
        regions: Vec<(Tiles, Tier)>,
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

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub source: Source,
    pub crop: Rect,
    pub dst: Rect,
    pub rotation: Rotation,
    pub nearest: bool,
    pub damage: Damage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HostLayer {
    Hud,
    GestureHint,
    Cursor,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layers {
    pub base: Layer,
    pub video: SmallVec<[(u64, Layer); 2]>,
    pub host: SmallVec<[HostLayer; 2]>,
    pub background: [f32; 3],
}

/// What D3D9 swap buffers should be. The only edge from display back to the
/// executor; it changes with settings or rotation, never per frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapRole {
    /// The plan shows swap buffers by reference: render into presentable buffers.
    Presentable { width: u32, height: u32 },
    /// A pass reads swap buffers anyway: render into private, compressed images.
    Private,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub stages: ArrayVec<Stage, 4>,
    pub layers: Layers,
    pub swap_role: SwapRole,
}

/// Decide how to show `frame`. Pure and deterministic.
pub fn plan(
    _frame: &FrameInfo,
    _settings: &Settings,
    _caps: &CompositorCaps,
    _cost: &CostModel,
    _pools: &PoolState,
) -> Plan {
    todo!("D")
}
