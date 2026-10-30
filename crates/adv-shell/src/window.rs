pub use adv_sync::WakeEvent;
#[cfg(target_os = "macos")]
pub(crate) use macos::native_viewport;
use std::sync::Arc;
use std::time::Duration;

pub enum Event {
    Menu(adv_runtime::MenuEvent),
    Dialog(adv_runtime::DialogEvent),
    Input {
        message: u32,
        wparam: u32,
        lparam: u32,
    },
    #[cfg(host_view)]
    Text(String, std::sync::mpsc::SyncSender<usize>),
    Close,
    #[cfg(host_view)]
    ForceClose,
    FocusLost,
    #[cfg(host_view)]
    AudioSuspended(bool),
    /// The launcher paused the game: no guest execution, frozen guest time
    /// and suspended audio until it resumes.
    #[cfg(host_view)]
    Paused(bool),
    #[cfg(host_view)]
    Volume(u8),
    Repaint,
}
pub type MenuSink = Box<dyn FnMut(adv_runtime::MenuEvent) -> Result<(), String>>;
pub type DialogSink = Box<dyn FnMut(adv_runtime::DialogEvent) -> Result<(), String>>;

#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::Projection;
#[cfg(target_os = "macos")]
pub use macos::{GameContainer, GameView, Window, initialize_dpi};

#[cfg(mobile)]
pub(crate) mod mobile;
/// The guest dialog and menu data a mobile host's overlay draws and answers.
#[cfg(mobile)]
pub use adv_runtime::{
    DialogControl, DialogEvent, DialogFont, DialogSet, DialogTarget, DialogWindow, MenuBar,
    MenuCommand, MenuEvent, MenuItem,
};
#[cfg(target_os = "ios")]
pub use mobile::GameContainer;
#[cfg(mobile)]
pub(crate) use mobile::native_viewport;
#[cfg(mobile)]
pub use mobile::{GameView, TouchAction, ViewEvent};
#[cfg(any(mobile, test))]
pub(crate) mod android_epoch;
#[cfg(all(test, not(target_os = "android")))]
#[path = "window/android/input.rs"]
mod android_input_tests;

#[cfg(host_view)]
/// The guest is told the display runs at 60 Hz, and both `Present` and
/// software frames are published at guest frame boundaries, so presenting
/// faster only repeats conversion and composition. Faster displays keep 10%
/// slack so a 60 Hz guest whose frames arrive early is still shown at once.
pub(crate) fn refresh_interval(display_fps: isize) -> Duration {
    let fps = display_fps.clamp(24, 240) as f64;
    if fps > 60.0 {
        Duration::from_secs_f64(0.9 / 60.0)
    } else {
        Duration::from_secs_f64(1.0 / fps)
    }
}
