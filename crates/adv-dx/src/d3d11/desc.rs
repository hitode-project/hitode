//! Pure validation and normalization of Direct3D 11 descriptions.
mod pipeline;
mod resource;
mod shader;
mod view;

pub use pipeline::PipelineState;
pub use resource::{Bind, Buffer, CpuAccess, Misc, Resource, Texture, TextureKind, Usage};
pub use shader::Shader;
pub use view::{Aspects, Subresources, View, ViewDesc, ViewKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidArgument,
    UnsupportedView,
}

#[cfg(test)]
mod tests;
