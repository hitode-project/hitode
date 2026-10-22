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
mod debug_views;
mod report;
pub mod survey;
pub use adv_abi::{ApiDiagnostics, DynamicLookupFailure, MessageBoxCall, RunMode};
/// The in-process JIT address map, for the debugger's sampler.
#[cfg(target_arch = "aarch64")]
pub use adv_cpu::jit_map::{
    Location as JitLocation, Part as JitPart, code_of as jit_map_code_of,
    enable_memory as jit_map_enable, locate as jit_map_locate, lookup as jit_map_lookup,
    stub as jit_map_stub,
};
pub use adv_cpu::{CompileStrategy, Engine, EngineOptions, HostFeatures};
#[cfg(target_os = "ios")]
pub use adv_cpu::{code_pool_state, prepare_code_pool_after_attach, prepare_code_pool_if_debugged};

/// Reserves the guest window for the life of the app (IOS-MEM-3); call it
/// at launch before any webview loads.
#[cfg(target_os = "ios")]
pub fn reserve_guest_window() -> Result<(), String> {
    adv_cpu::virtual_memory::GuestWindow::preserve()
}
pub use adv_dx::dsound::{AudioSourceStatus, PcmDigest};
pub use adv_win32::user32::dialog::{
    DialogControl, DialogEvent, DialogFont, DialogSet, DialogTarget, DialogWindow,
};
pub use adv_win32::user32::host_input::{InputView, WindowView, mouse_mapping};
pub use adv_win32::user32::menu::{
    MenuBar, MenuCommand, MenuEvent, MenuItem, MenuKey, PopupPresentation,
};
pub use adv_win32::vorbisfile as vorbis;
pub use graphics::{
    FrameConsumer, FrameProducer, FrameView, Graphics, GraphicsStats, IID_DDRAW7, IID_SURFACE7,
    PendingFrame, Presenter, Snapshot, VideoCursorRegion, api, frame_mailbox, install_graphics,
    register_graphics,
};
