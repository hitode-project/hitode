//! Small real-ABI GDI and DirectDraw paths on a shared guest pixel store.
pub mod boot;
pub mod compat;
pub mod crt;
mod font;
mod graphics;
mod sound;
pub use adv_jit::CompileStrategy;
pub use graphics::{
    FrameView, Graphics, GraphicsStats, IID_DDRAW7, IID_SURFACE7, Presenter, Snapshot, api, install,
};
pub use sound::PcmDigest;
