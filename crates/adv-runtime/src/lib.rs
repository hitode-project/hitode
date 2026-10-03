//! Small real-ABI GDI and DirectDraw paths on a shared guest pixel store.
pub mod acm;
pub use adv_win32::avifil32 as avi;
pub mod boot;
pub mod compat;
pub use adv_win32::msvcrt as crt;
mod font;
mod graphics;
pub use adv_win32::winmm::mmio;
mod report;
pub mod vorbis;
pub use adv_abi::RunMode;
pub use adv_cpu::CompileStrategy;
pub use adv_dx::dsound::{AudioSourceStatus, PcmDigest};
pub use graphics::{
    FrameConsumer, FrameProducer, FrameView, Graphics, GraphicsStats, IID_DDRAW7, IID_SURFACE7,
    PendingFrame, Presenter, Snapshot, api, frame_mailbox, install_graphics, register_graphics,
};
