//! Neutral GPU drawing and ownership of submitted resources. This module has
//! no guest addresses, Windows handles, COM layouts or D3D state identifiers.
use crate::surface::Rect;
mod renderer;
mod submission;
pub use renderer::Submission;
pub use renderer::{Gpu, Image, NativeContext, NativeFrame, Readback, Target};
pub use submission::SubmissionPause;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    Clockwise,
    CounterClockwise,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendOp {
    Add,
    Subtract,
    ReverseSubtract,
    Minimum,
    Maximum,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Blend {
    pub source: Factor,
    pub destination: Factor,
    pub operation: BlendOp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
pub enum Filter {
    Nearest,
    Linear,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Address {
    Clamp,
    Repeat,
    Mirror,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sampler {
    pub min: Filter,
    pub mag: Filter,
    pub u: Address,
    pub v: Address,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    pub cull: Cull,
    pub blend: Option<Blend>,
    /// None uses the color blend component for alpha too.
    pub alpha_blend: Option<Blend>,
    pub depth: Option<Compare>,
    pub depth_write: bool,
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
            cull: Cull::None,
            blend: None,
            alpha_blend: None,
            depth: None,
            depth_write: false,
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
                min: Filter::Nearest,
                mag: Filter::Nearest,
                u: Address::Clamp,
                v: Address::Clamp,
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
    pub submissions: u64,
    pub draws: u64,
    pub vertices: u64,
    pub readbacks: u64,
    pub native_frames: u64,
    pub native_frame_allocations: u64,
    pub vertex_buffer_allocations: u64,
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
