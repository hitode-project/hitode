//! Direct3D 11 device and context front.
mod abi;
pub mod caps;
pub mod desc;
mod exports;
mod glue;
pub mod object;
mod ops;
#[cfg(feature = "gpu")]
pub mod recording;
#[cfg(feature = "gpu")]
mod resource;
pub mod state;

pub use glue::{
    attach_pipeline, attach_resource, attach_view, configure_devices, install_children,
    register_children,
};

#[cfg(feature = "gpu")]
pub use glue::{configure_gpu, configure_surface_dc, configure_swap_chains};
