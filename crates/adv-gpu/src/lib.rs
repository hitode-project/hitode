//! The GPU layer. It is the only crate that calls Vulkan or Metal.
//!
//! **Skeleton (issue #103, wave 0).** The public types here state the
//! contract and delegate every call to the target's backend module,
//! `vulkan` or `metal`, which owns all FFI. Both start as stubs whose
//! `Device::open` reports [`Error::Unavailable`]; each backend package
//! replaces its own module and leaves this layer alone. The API freezes once
//! both backends pass `tests/conformance.rs`.
//!
//! # Users
//!
//! - `adv-device::gpu`: the D3D9 and DirectDraw executor (recording, damage,
//!   clipped execution, pipeline keys). It keeps its D3D9-shaped facade and
//!   runs it over this crate.
//! - `adv-display`: ingest, upscaling, output passes and presentable pools.
//! - Later front ends such as D3D10/11 sit beside the D3D9 executor, directly on
//!   this crate. Nothing here is specific to a guest API: shapes are general,
//!   enums are `#[non_exhaustive]`, and optional hardware is reported by
//!   [`Caps`] rather than assumed.
//!
//! # Backends
//!
//! Chosen at compile time: Metal on Apple targets, Vulkan elsewhere. The public
//! API has no dynamic dispatch and no backend types. Platform handles appear
//! only where the platform requires them: presentable-buffer export
//! ([`present::Export`]) and compositor fences ([`queue::Fence`], Android).
//!
//! # Threads
//!
//! - [`Device`] is `Clone + Send + Sync`. Any thread may create resources and
//!   pipelines: the execution thread creates a guest texture synchronously,
//!   the compile pool builds pipelines.
//! - [`Queue`] is `Send` but not `Sync`. It moves into the GPU owner thread,
//!   which is the only thread that encodes or submits. Encoding order is
//!   submission order is execution order; this is what lets the Vulkan backend
//!   track image layouts without a global tracker.
//! - Resource handles are reference counted and `Send + Sync`. Dropping the last
//!   handle defers destruction until every submission that used it completes.
//! - Producers write upload data once, into [`Ring`] spans or mapped buffers,
//!   from their own thread. The owner thread only references those bytes.
//!
//! # Platform baselines
//!
//! Each backend uses its hardware's best mechanism; the API states intent
//! (lifetime, change frequency), not a common denominator.
//!
//! The device floor follows the CPU: A12-class cores (2019 onwards). GPU
//! drivers on such devices are often older than their hardware, so the
//! backends are designed around modern features and carry a small
//! compatibility tier for older drivers instead of raising the floor.
//!
//! - Vulkan requires 1.1 with Android hardware buffers, external fence and
//!   semaphore fds and YCbCr sampling, which every Adreno 6xx and Mali G76 or
//!   later reports. The design center is the modern set; each member has a
//!   mechanical stand-in confined to the backend:
//!
//!   | Modern feature | Stand-in on older drivers |
//!   |---|---|
//!   | dynamic rendering | `VkRenderPass` and framebuffer cached by attachments |
//!   | synchronization2 | ordinary pipeline barriers |
//!   | timeline semaphores | a pool of fences behind [`Token`] |
//!   | descriptor indexing (bindless) | push descriptors (Adreno 6xx), cached descriptor sets (Mali G76) |
//!   | extended dynamic state 1/2, color write enable | the state joins the pipeline key |
//!   | load/store op none | load and store |
//!
//!   Optional fast paths: cooperative matrices, int8 dot products, tile-local
//!   reads (`dynamic_rendering_local_read`, `shader_tile_image`), descriptor
//!   buffers, host image copy, pipeline binaries, EDS3, Qualcomm tile
//!   properties and weighted sampling.
//!
//!   Detection is per feature at [`Device::open`], not one switch between
//!   tiers: a device may render dynamically and still bind through push
//!   descriptors. A quirk table keyed by vendor, driver version and feature
//!   overrides drivers that report a feature they break; it starts empty and
//!   grows only from real driver bugs. [`Caps::modern`] records the choices,
//!   and [`queue::DeviceOptions::disable`] forces stand-ins, which is how CI
//!   covers both tiers on lavapipe.
//! - Metal requires Apple5 (A12): memoryless and losslessly compressed
//!   targets, tile shaders and raster order groups. SIMD-group matrices from
//!   Apple7.
//!
//! Vertex fetch is shader code on both backends: front ends read vertex
//! streams from storage buffers (the D3D9 translator emits the fetch), so no
//! pipeline carries a vertex layout and no driver needs dynamic vertex input.
//!
//! Measured per-pass fixed cost (2026-11-02): about 3.5 µs on Adreno 750
//! against 40 µs on M4 and 55–60 µs on A12, so the planner merges passes on
//! Apple and need not on Adreno.
//!
//! # Damage
//!
//! Every pass takes a render area ([`encode::RenderPassDesc::area`]). Vulkan
//! passes it as `renderArea`; Metal clamps each scissor to it. Measured on A12
//! and M4 (2026-11-02): a Metal pass scissored to 64×64 costs the same at
//! 640×480 and 3840×2160, so tiles outside the scissor are neither loaded nor
//! stored, and no scratch targets are needed. Adreno 750 behaves the same
//! with either `renderArea` or the scissor.

/// Opens a device for a backend's internal tests, or returns from the test
/// where the target has none, unless `ADV_GPU_REQUIRE_BACKEND` is set (the
/// conformance suite's rule).
#[cfg(test)]
macro_rules! test_device {
    ($options:expr) => {
        match crate::Device::open(&$options) {
            Ok(opened) => opened,
            Err(crate::Error::Unavailable(reason))
                if std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_none() =>
            {
                eprintln!("skipped: {reason}");
                return;
            }
            Err(error) => panic!("open device: {error}"),
        }
    };
}

#[cfg(target_vendor = "apple")]
mod metal;
#[cfg(target_vendor = "apple")]
use metal as imp;
#[cfg(not(target_vendor = "apple"))]
mod vulkan;
#[cfg(not(target_vendor = "apple"))]
use vulkan as imp;

pub mod caps;
pub mod encode;
pub mod format;
pub mod pipeline;
pub mod present;
pub mod queue;
pub mod resource;
pub mod ring;
pub mod shader;

pub use caps::{Caps, Features, FormatCaps, Limits, Modern};
pub use encode::{ComputePass, Encoder, RenderPass, RenderPassDesc};
pub use format::Format;
pub use pipeline::{ComputePipeline, RenderPipeline};
pub use present::{PresentImage, PresentPool};
pub use queue::{Device, Queue, Token};
pub use resource::{Buffer, Image, ImageView, Sampler};
pub use ring::{Ring, Span};
pub use shader::Shader;

/// A rectangle in texels. Origin top left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Extent {
    pub width: u32,
    pub height: u32,
    /// Depth for 3D images, layer count for arrays and cubes (6 per cube), else 1.
    pub depth_or_layers: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// No usable adapter, or a required baseline feature is missing.
    Unavailable(String),
    /// The device was lost. Submit, wait and poll fail with this until the
    /// owner recreates the device; other calls do not check, and handles from
    /// the lost device stay inert.
    DeviceLost(String),
    /// Out of device or host memory for this request. Nothing was allocated.
    OutOfMemory,
    /// A bounded resource (ring, pool, frames in flight) is full now and will
    /// free when submissions complete. Nothing was written or encoded.
    Busy,
    /// A creation request exceeds the device's limits or carries malformed
    /// input. Contract violations on encode paths (binds, draws, ring
    /// reservations) are caller bugs: debug assertions, never checked in
    /// release builds.
    Invalid(String),
    /// The requested operation is not supported by this backend.
    Unsupported(String),
    /// A shader or pipeline failed to compile; the message names the stage.
    Compile(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(m) => write!(f, "GPU unavailable: {m}"),
            Self::DeviceLost(m) => write!(f, "GPU device lost: {m}"),
            Self::OutOfMemory => f.write_str("GPU out of memory"),
            Self::Busy => f.write_str("GPU resource busy"),
            Self::Invalid(m) => write!(f, "invalid GPU request: {m}"),
            Self::Unsupported(m) => write!(f, "unsupported GPU request: {m}"),
            Self::Compile(m) => write!(f, "GPU compile failed: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
