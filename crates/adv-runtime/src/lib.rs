//! Small real-ABI GDI and DirectDraw paths on a shared guest pixel store.
pub use adv_device::text::Overrides as FontOverrides;
pub use adv_win32::avifil32 as avi;
pub use adv_win32::codec_kernels;
pub use adv_win32::msacm32 as acm;
pub mod boot;
pub mod codepage;
pub mod compat;
pub use adv_win32::msvcrt as crt;
mod family;
mod font;
pub use family::ProcessStatus;
mod graphics;
pub use adv_win32::winmm::mmio;
mod report;
pub use adv_abi::{ApiDiagnostics, DynamicLookupFailure, MessageBoxCall, RunMode};
pub use adv_cpu::{CompileStrategy, Engine, EngineOptions, HostFeatures};
pub use adv_dx::dsound::{AudioSourceStatus, PcmDigest};
pub use adv_win32::user32::dialog::{
    DialogControl, DialogEvent, DialogFont, DialogSet, DialogTarget, DialogWindow,
};
pub use adv_win32::user32::host_input::mouse_mapping;
pub use adv_win32::user32::menu::{
    MenuBar, MenuCommand, MenuEvent, MenuItem, MenuKey, PopupPresentation,
};
pub use adv_win32::vorbisfile as vorbis;
pub use graphics::{
    FrameConsumer, FrameProducer, FrameView, Graphics, GraphicsStats, IID_DDRAW7, IID_SURFACE7,
    PendingFrame, Presenter, Snapshot, VideoCursorRegion, api, frame_mailbox, install_graphics,
    register_graphics,
};
