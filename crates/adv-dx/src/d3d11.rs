//! Direct3D 11 device and context front.
mod abi;
pub mod caps;
pub mod desc;
mod glue;
pub mod object;
mod ops;
#[cfg(feature = "gpu")]
pub mod recording;
pub mod state;

pub use glue::{
    attach_pipeline, attach_resource, attach_view, install_children, register_children,
};
