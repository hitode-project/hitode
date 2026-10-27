//! Shared services for native device fronts: VFS, guest heaps, COM activation,
//! code pages and host key mapping. Wine owns image loading and Windows state.

pub mod com;
pub mod heap;
pub mod keyboard;
pub mod nls;
pub mod vfs;

use std::path::PathBuf;

pub use heap::Heaps;
pub use nls::Codec;
pub use vfs::Vfs;

/// What the OS layer needs to know about the game and the host.
#[derive(Clone, Debug)]
pub struct OsConfig {
    /// ANSI code page of the guest (932 for Japanese builds, 936 for GBK fan builds).
    pub codepage: u32,
    /// Host directory with the game files (writable only when write_game is true).
    pub game_dir: PathBuf,
    /// Write ordinary game files in place; explicit data overlays keep this false.
    pub write_game: bool,
    /// Optional read-only replacement files, with paths relative to the game root.
    /// Guest writes in data_dir take precedence over this layer.
    pub resource_dir: Option<PathBuf>,
    /// Guest path of the game directory.
    pub guest_root: String,
    /// Exe file name as it will appear in the guest (for the command line).
    pub exe_name: String,
    /// Host directory for everything the guest writes (overlay, user profile, registry).
    pub data_dir: PathBuf,
    /// Host directory behind guest C:, including per-user installer state.
    pub system_dir: PathBuf,
    pub user_name: String,
    /// Host folder behind guest `C:\Program Files`, written in place: installers
    /// put their files straight into it rather than into the data overlay.
    pub install_dir: Option<PathBuf>,
}

impl OsConfig {
    pub fn new(game_dir: impl Into<PathBuf>, exe_name: &str, data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        Self {
            codepage: 932,
            game_dir: game_dir.into(),
            write_game: false,
            resource_dir: None,
            guest_root: "C:\\Game".into(),
            exe_name: exe_name.into(),
            system_dir: data_dir.join("c"),
            data_dir,
            user_name: "Player".into(),
            install_dir: None,
        }
    }

    /// Guest path of the exe.
    pub fn exe_path(&self) -> String {
        if self.exe_name.as_bytes().get(1) == Some(&b':') || self.exe_name.starts_with("\\\\") {
            self.exe_name.clone()
        } else {
            format!("{}\\{}", self.guest_root, self.exe_name)
        }
    }
}
