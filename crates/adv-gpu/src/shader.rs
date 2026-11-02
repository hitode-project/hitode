//! Shaders enter as naga IR. Backends lower IR to SPIR-V or MSL.
//!
//! - Guest shaders: the D3D9 translator emits WGSL today and keeps doing so;
//!   [`Source::Wgsl`] parses it. A D3D10/11 front end produces a
//!   [`naga::Module`] directly, or SPIR-V that naga's SPIR-V front end reads
//!   into one before it reaches this crate.
//! - Host shaders (fixed function, ingest, composition, upscalers) are WGSL,
//!   precompiled at build time into both outputs and loaded as
//!   [`Source::Precompiled`], so a launch never pays for their translation.

use crate::Result;

#[non_exhaustive]
pub enum Source<'a> {
    Wgsl(&'a str),
    Ir(Box<naga::Module>),
    /// Build-time output: SPIR-V words for Vulkan, MSL source or a Metal
    /// library for Metal, declaring the argument indices
    /// [`crate::pipeline::Binding`] lists. Only the current backend's half is
    /// read.
    Precompiled {
        spirv: &'a [u32],
        metal: &'a [u8],
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Stage {
    Vertex,
    Fragment,
    Compute,
}

pub struct ShaderDesc<'a> {
    pub source: Source<'a>,
    /// Content hash used by pipeline and disk caches (GFX-CACHE-8). The caller
    /// supplies it so equal sources hash once.
    pub identity: u64,
    pub label: &'static str,
}

/// A validated module, ready for pipelines on this device. Clone shares it.
#[derive(Clone)]
pub struct Shader {
    /// [`ShaderDesc::identity`].
    pub(crate) identity: u64,
    pub(crate) inner: crate::imp::Shader,
}

impl Shader {
    pub fn entry(&self, name: &str, stage: Stage) -> Result<Entry> {
        self.inner.entry(name, stage)
    }
}

#[derive(Clone)]
pub struct Entry {
    pub shader: Shader,
    pub index: u32,
    pub stage: Stage,
}
