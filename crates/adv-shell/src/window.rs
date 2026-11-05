pub use adv_sync::WakeEvent;
#[cfg(target_os = "macos")]
use std::sync::Arc;

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
pub use mobile::{GameView, TouchAction, ViewEvent};
#[cfg(any(mobile, test))]
pub(crate) mod android_epoch;
#[cfg(all(test, not(target_os = "android")))]
#[path = "window/android/input.rs"]
mod android_input_tests;
