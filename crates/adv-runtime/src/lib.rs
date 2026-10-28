//! Assembly of the Wine userland with the host fronts, devices and execution.
pub use adv_device::text::Overrides as FontOverrides;
pub use adv_device::text::system_font_families;
pub mod codepage;
mod debug_views;
mod font;
mod graphics;
/// The Wine userland: sessions, the host fronts and their drivers.
#[cfg(unix)]
pub mod wine;
/// The in-process JIT address map, for the debugger's sampler.
#[cfg(target_arch = "aarch64")]
pub use adv_cpu::jit_map::{
    Location as JitLocation, Part as JitPart, code_of as jit_map_code_of,
    enable_memory as jit_map_enable, locate as jit_map_locate, lookup as jit_map_lookup,
    stub as jit_map_stub,
};
pub use adv_cpu::{CompileStrategy, Cpu, Engine, EngineOptions, HostFeatures};
#[cfg(target_os = "ios")]
pub use adv_cpu::{code_pool_state, prepare_code_pool_after_attach, prepare_code_pool_if_debugged};
pub use debug_views::DebugTarget;

/// Reserves the guest window for the life of the app (IOS-MEM-3); call it
/// at launch before any webview loads.
#[cfg(target_os = "ios")]
pub fn reserve_guest_window() -> Result<(), String> {
    adv_cpu::virtual_memory::GuestWindow::preserve()
}
pub use adv_device::ui::{
    DialogControl, DialogEvent, DialogFont, DialogSet, DialogTarget, DialogWindow, InputView,
    MenuBar, MenuCommand, MenuEvent, MenuItem, MenuKey, PopupPresentation, WindowView,
};
pub use adv_dx::dsound::{AudioSourceStatus, PcmDigest};
pub use graphics::{
    FrameConsumer, FrameProducer, FrameView, GraphicsStats, PendingFrame, Presenter, Snapshot,
    VideoCursorRegion, frame_mailbox,
};
