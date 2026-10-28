//! Neutral GPU drawing and ownership of submitted resources. This module has
//! no guest addresses, Windows handles, COM layouts or D3D state identifiers.
//! Apple device requests, native draws, copies and compilation drain their
//! autoreleased temporaries on the calling thread.
use crate::surface::Rect;
mod clear;
mod constant_snapshot;
pub use constant_snapshot::ConstantSnapshot;
pub mod compile;
mod depth;
mod index_bytes;
mod line;
pub use index_bytes::IndexBytes;
mod programmable;
mod renderer;
mod submission;
mod vertex_bytes;
use depth::DeferredClear;
pub use depth::{Depth, DepthFormat, Stencil, StencilFace, StencilOp};
pub use programmable::{
    Attribute, AttributeFormat, ConstantBlock, Program, ProgramTexture, ProgrammableDraw,
    VertexStream,
};
#[cfg(target_vendor = "apple")]
pub use renderer::DisplayCompletion;
pub use renderer::Submission;
pub use renderer::validate_program;
pub use renderer::{Gpu, Image, NativeContext, NativeFrame, Readback, Staged, Target};
pub use submission::SubmissionPause;
pub use vertex_bytes::VertexBytes;
#[derive(Clone, Copy, Debug)]
pub enum GpuBackend {
    Automatic,
    Vulkan,
    Gl,
    Direct3D12,
    Metal,
}

/// Whether sampled alpha is stored or implicitly one (RGBX color images).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlphaMode {
    #[default]
    Stored,
    Opaque,
}

/// Clip-space position, RGBA diffuse color and normalized texture coordinates.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 4],
    pub color: [f32; 4],
    pub uv: [f32; 2],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Topology {
    Points,
    Lines,
    LineStrip,
    Triangles,
    TriangleStrip,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Cull {
    None,
    Clockwise,
    CounterClockwise,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Factor {
    Zero,
    One,
    SourceColor,
    OneMinusSourceColor,
    SourceAlpha,
    OneMinusSourceAlpha,
    DestinationColor,
    OneMinusDestinationColor,
    DestinationAlpha,
    OneMinusDestinationAlpha,
    SourceAlphaSaturated,
    Constant,
    OneMinusConstant,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum BlendOp {
    Add,
    Subtract,
    ReverseSubtract,
    Minimum,
    Maximum,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Blend {
    pub source: Factor,
    pub destination: Factor,
    pub operation: BlendOp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Filter {
    Nearest,
    Linear,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Address {
    Clamp,
    Repeat,
    Mirror,
    Border,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Sampler {
    pub min: Filter,
    pub mag: Filter,
    /// None clamps sampling to the most detailed permitted level.
    pub mip: Option<Filter>,
    pub first_level: u32,
    pub u: Address,
    pub v: Address,
    /// Border color, packed RGBA with red in the least significant byte.
    pub border_color: u32,
}
/// Shared portable border sampling source for composed shader modules.
pub const BORDER_WGSL: &str = include_str!("gpu/border.wgsl");
pub fn sampler_parameters(sampler: Sampler) -> [u32; 4] {
    let mode = |a| match a {
        Address::Repeat => 0,
        Address::Mirror => 1,
        Address::Clamp => 2,
        Address::Border => 3,
    };
    [
        u32::from(sampler.u == Address::Border)
            | (u32::from(sampler.v == Address::Border) << 1)
            | (mode(sampler.u) << 4)
            | (mode(sampler.v) << 6),
        sampler.border_color,
        sampler.first_level,
        u32::from(sampler.min == Filter::Linear)
            | (u32::from(sampler.mag == Filter::Linear) << 1)
            | (u32::from(sampler.mip == Some(Filter::Linear)) << 2)
            | (u32::from(sampler.mip.is_some()) << 3),
    ]
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TextureOp {
    Color,
    Texture,
    Multiply,
    Add,
    Subtract,
}
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub minimum_depth: f32,
    pub maximum_depth: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct DrawState {
    pub topology: Topology,
    /// Include the terminal pixel of aliased line segments. Other topologies ignore it.
    pub last_pixel: bool,
    pub cull: Cull,
    pub blend: Option<Blend>,
    /// None uses the color blend component for alpha too.
    pub alpha_blend: Option<Blend>,
    pub blend_constant: Option<u32>,
    pub depth: Option<Compare>,
    pub depth_write: bool,
    pub stencil: Option<Stencil>,
    pub stencil_reference: u32,
    pub viewport: Viewport,
    pub scissor: Option<Rect>,
    pub sampler: Sampler,
    pub texture_op: TextureOp,
    pub alpha_texture_op: TextureOp,
    /// RGBA write mask in bits 0..3.
    pub color_write: u8,
    pub alpha_test: Compare,
    pub alpha_reference: f32,
}
impl DrawState {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            topology: Topology::Triangles,
            last_pixel: true,
            cull: Cull::None,
            blend: None,
            alpha_blend: None,
            blend_constant: None,
            depth: None,
            depth_write: false,
            stencil: None,
            stencil_reference: 0,
            viewport: Viewport {
                x: 0.0,
                y: 0.0,
                width: width as f32,
                height: height as f32,
                minimum_depth: 0.0,
                maximum_depth: 1.0,
            },
            scissor: None,
            sampler: Sampler {
                mip: None,
                first_level: 0,
                min: Filter::Nearest,
                mag: Filter::Nearest,
                u: Address::Clamp,
                v: Address::Clamp,
                border_color: 0,
            },
            texture_op: TextureOp::Color,
            alpha_texture_op: TextureOp::Color,
            color_write: 15,
            alpha_test: Compare::Always,
            alpha_reference: 0.0,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub passes: u64,
    pub color_loads: u64,
    pub depth_loads: u64,
    pub merged_clears: u64,
    pub submissions: u64,
    pub draws: u64,
    pub vertices: u64,
    pub readbacks: u64,
    pub image_copy_bytes: u64,
    pub native_frames: u64,
    pub native_frame_allocations: u64,
    pub vertex_buffer_allocations: u64,
    pub metal_draws: u64,
    pub backend_switches: u64,
    /// Partial clear, copy, upload, fixed draw, readback, other (including entry to Metal).
    pub backend_switches_by_reason: [u64; 6],
    pub ring_bytes: u64,
    pub ring_busy: u64,
    pub buffer_writes: u64,
    pub last_flush_buffer_writes: u64,
    pub bind_group_creations: u64,
    pub pipeline_cache_hits: u64,
    pub compile_waits: u64,
    pub compile_wait_ns: u64,
    pub prebuild_hits: u64,
    pub prebuild_misses: u64,
    pub persisted_pipelines: u64,
    pub discarded_pipelines: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Unavailable(String),
    Invalid(String),
    Device(String),
    Busy,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(s) | Self::Invalid(s) | Self::Device(s) => f.write_str(s),
            Self::Busy => f.write_str("GPU command resource budget exhausted"),
        }
    }
}
impl std::error::Error for Error {}
