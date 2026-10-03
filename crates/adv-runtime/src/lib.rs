//! Small real-ABI GDI and DirectDraw paths on a shared guest pixel store.
pub mod acm;
pub mod avi;
pub mod boot;
pub mod compat;
pub mod crt;
mod font;
mod graphics;
pub mod mmio;
pub mod vorbis;
pub use adv_cpu::CompileStrategy;
pub use adv_dx::dsound::{AudioSourceStatus, PcmDigest};
pub use graphics::{
    FrameConsumer, FrameProducer, FrameView, Graphics, GraphicsStats, IID_DDRAW7, IID_SURFACE7,
    PendingFrame, Presenter, Snapshot, api, frame_mailbox, install,
};
