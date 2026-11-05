//! Render and compute pipelines, and the binding model.
//!
//! Pipelines are immutable and built on any thread; the compile pool builds
//! them in the background (GFX-CACHE-7) and [`PipelineCache`] persists driver
//! binaries across launches (GFX-CACHE-8, GFX-CACHE-10).
//!
//! Bindings are set per draw by slot ([`crate::encode::Bind`]); see there for
//! how each backend makes that free.
//!
//! A description states the complete fixed-function state. Each backend
//! creates driver objects only for the part its hardware cannot change
//! dynamically and shares them across descriptions that differ only in the
//! rest:
//!
//! - Vulkan, modern tier: cull, winding, topology class, depth and stencil
//!   state and depth bias are dynamic (extended dynamic state 1 and 2), and
//!   color write masks too where color write enable exists. Shaders, blend
//!   and formats key the `VkPipeline`. On older drivers that state joins the
//!   key instead.
//! - Metal: depth and stencil state are separate objects; cull, winding and
//!   depth bias are dynamic. Shaders, blend and formats key the pipeline
//!   state.
//!
//! No description carries a vertex layout: vertex fetch is shader code that
//! reads storage buffers (see the crate docs).
//!
//! [`crate::Device::pipeline_identity`] returns the key of the driver object a
//! description needs, so the executor's caches, prebuilds and disk cache count
//! real compilations instead of state combinations.

use crate::Format;
#[cfg(feature = "backend")]
use crate::Result;
#[cfg(feature = "backend")]
use crate::imp;
#[cfg(feature = "backend")]
use crate::shader::Entry;
#[cfg(feature = "backend")]
use arrayvec::ArrayVec;

pub const MAX_COLOR_TARGETS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BindingKind {
    UniformBuffer,
    SampledImage {
        dimension: crate::resource::Dimension,
        depth: bool,
        multisampled: bool,
    },
    Sampler {
        comparison: bool,
    },
    StorageImage {
        format: Format,
        writable: bool,
    },
    StorageBuffer {
        writable: bool,
    },
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Stages: u8 {
        const VERTEX = 1 << 0;
        const FRAGMENT = 1 << 1;
        const COMPUTE = 1 << 2;
    }
}

/// One binding slot. Slots are numbered per kind, as in D3D registers
/// (`b#`, `t#`, `s#`, `u#`). Shaders declare a slot as `@group(g)
/// @binding(slot)`, where the group names the kind:
///
/// | Group | Kind | [`crate::encode::Bind`] |
/// |---|---|---|
/// | 0 | uniform buffers | `Uniform` |
/// | 1 | sampled images | `Sampled` |
/// | 2 | samplers | `Sampler` |
/// | 3 | storage images and buffers, one slot space like `u#` | `StorageImage`, `StorageBuffer` |
///
/// The backend rewrites these declarations to its own binding model
/// (bindless heap indices, push descriptors, descriptor sets or Metal
/// argument indices) before lowering, so shaders never see it. Metal gives
/// uniform `b#` `buffer(#)`, storage buffer `u#` `buffer(29 - #)`, sampled
/// `t#` `texture(#)`, storage image `u#` `texture(95 - #)` and `s#`
/// `sampler(slot % 16)` for sampler slots `0..32`. A stage cannot use two
/// sampler slots with the same index modulo 16; pipeline creation rejects
/// such collisions. Buffers and textures whose indices meet in one stage's
/// table are also rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Binding {
    pub slot: u32,
    pub kind: BindingKind,
    pub stages: Stages,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Topology {
    Points,
    Lines,
    LineStrip,
    Triangles,
    TriangleStrip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cull {
    None,
    Front,
    Back,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Primitive {
    pub topology: Topology,
    pub cull: Cull,
    pub front_ccw: bool,
    pub wireframe: bool,
    /// Depth clipping off (D3D10+ `DepthClipEnable = false`); needs device support.
    pub unclipped_depth: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Compare {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StencilOp {
    Keep,
    Zero,
    Replace,
    IncrementClamp,
    DecrementClamp,
    Invert,
    IncrementWrap,
    DecrementWrap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StencilFace {
    pub compare: Compare,
    pub fail: StencilOp,
    pub depth_fail: StencilOp,
    pub pass: StencilOp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DepthStencil {
    pub format: Format,
    pub depth_compare: Compare,
    pub depth_write: bool,
    pub stencil: Option<(StencilFace, StencilFace)>,
    pub stencil_read_mask: u8,
    pub stencil_write_mask: u8,
    /// Depth bias is dynamic ([`crate::encode::RenderPass::set_depth_bias`]);
    /// this only enables it.
    pub depth_bias: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Factor {
    Zero,
    One,
    Src,
    OneMinusSrc,
    SrcAlpha,
    OneMinusSrcAlpha,
    Dst,
    OneMinusDst,
    DstAlpha,
    OneMinusDstAlpha,
    SrcAlphaSaturated,
    Constant,
    OneMinusConstant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BlendOp {
    Add,
    Subtract,
    ReverseSubtract,
    Min,
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlendComponent {
    pub src: Factor,
    pub dst: Factor,
    pub op: BlendOp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColorTarget {
    pub format: Format,
    pub blend: Option<(BlendComponent, BlendComponent)>,
    /// Bit 0 red … bit 3 alpha.
    pub write_mask: u8,
}

#[cfg(feature = "backend")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RenderPipelineDesc {
    pub vertex: Entry,
    pub fragment: Option<Entry>,
    pub bindings: Vec<Binding>,
    pub primitive: Primitive,
    pub colors: ArrayVec<ColorTarget, MAX_COLOR_TARGETS>,
    pub depth_stencil: Option<DepthStencil>,
    pub samples: u32,
    pub alpha_to_coverage: bool,
    pub label: &'static str,
}

#[cfg(feature = "backend")]
/// A render pipeline sampling a pool's converted RGB video. The fragment shader
/// declares a `texture_2d<f32>` at group 4, binding 0, and its sampler at binding 1.
/// These become one combined immutable sampler at binding 0 in a dedicated set.
/// Metal returns [`crate::Error::Unsupported`]; it samples NV12 plane views instead.
pub struct VideoPipelineDesc<'a> {
    pub pipeline: &'a RenderPipelineDesc,
    pub pool: &'a crate::PresentPool,
}

#[cfg(feature = "backend")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ComputePipelineDesc {
    pub entry: Entry,
    pub bindings: Vec<Binding>,
    pub label: &'static str,
}

#[cfg(feature = "backend")]
#[derive(Clone)]
pub struct RenderPipeline {
    #[allow(dead_code)] // read by the backends
    pub(crate) inner: crate::imp::RenderPipeline,
}

#[cfg(feature = "backend")]
#[derive(Clone)]
pub struct ComputePipeline {
    #[allow(dead_code)] // read by the backends
    pub(crate) inner: crate::imp::ComputePipeline,
}

/// Driver pipeline binaries keyed by our own pipeline identity. Vulkan uses a
/// `VkPipelineCache`, Metal a binary archive. Data from another driver version
/// is discarded on load, never trusted.
#[cfg(feature = "backend")]
pub struct PipelineCache {
    #[allow(dead_code)] // read by the backends
    pub(crate) inner: crate::imp::PipelineCache,
}

#[cfg(feature = "backend")]
impl PipelineCache {
    pub fn load(data: &[u8]) -> Self {
        imp::PipelineCache::load(data)
    }
    pub fn serialize(&self) -> Result<Vec<u8>> {
        self.inner.serialize()
    }
}

// Entry equality is by shader identity, entry index and stage.
#[cfg(feature = "backend")]
impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        (self.shader.identity, self.index, self.stage)
            == (other.shader.identity, other.index, other.stage)
    }
}
#[cfg(feature = "backend")]
impl Eq for Entry {}
#[cfg(feature = "backend")]
impl std::hash::Hash for Entry {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.shader.identity, self.index, self.stage).hash(state);
    }
}
#[cfg(feature = "backend")]
impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry").field("index", &self.index).finish()
    }
}
