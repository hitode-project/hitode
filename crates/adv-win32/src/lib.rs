//! Win32 shim: the kernel32/user32/gdi32/winmm/... functions Game.exe and its CRT call,
//! implemented on top of the machine core.
//!
//! Grown on demand: every import is bound to a thunk, and an import without an
//! implementation fails by name when it is first called (`adv_machine::thunk`). For host
//! modules, `GetProcAddress` only returns implemented functions, so feature probes (the CRT
//! looking for `FlsAlloc`, say) take their fallback paths.
//!
//! State lives in [`Win32`] inside the machine's `Ext`. Handlers get it with
//! `env.ext.get_mut::<Win32>()`, alongside `env.cpu` and `env.sys`.

mod advapi32;
pub mod gdi32;
pub mod heap;
pub mod kernel32;
mod misc;
pub mod nls;
pub mod printf;
pub mod user32;
pub mod util;
mod version;
pub mod vfs;
mod winmm;
mod winsock;

use std::collections::HashMap;
use std::path::PathBuf;

use adv_machine::layout::{PEB_ADDR, peb};
use adv_machine::{Conv, Handle, Handler, Machine, Registry, Sys};
use pal_cpu::{Backend, Guest, GuestExt, MemFault};

pub use heap::Heaps;
pub use nls::Codec;
pub use vfs::Vfs;

#[derive(Clone, Debug)]
pub struct Win32Config {
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
    /// Host directory for everything the guest writes (overlay, user profile).
    pub data_dir: PathBuf,
    pub user_name: String,
    /// Minutes east of UTC for local time (JST = 540).
    pub utc_offset_min: i32,
    /// Desktop size in pixels.
    pub screen: (i32, i32),
    /// Headless runs: press a dialog's default button after this many (guest) ms. `None`
    /// leaves dialogs to the frontend.
    pub auto_dialog_ms: Option<u32>,
}

impl Win32Config {
    pub fn new(game_dir: impl Into<PathBuf>, exe_name: &str, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            codepage: 932,
            game_dir: game_dir.into(),
            write_game: false,
            resource_dir: None,
            guest_root: "C:\\Game".into(),
            exe_name: exe_name.into(),
            data_dir: data_dir.into(),
            user_name: "Player".into(),
            utc_offset_min: 9 * 60,
            screen: (1920, 1080),
            auto_dialog_ms: None,
        }
    }
}

/// Open file.
pub struct FileObj {
    pub file: std::fs::File,
    pub guest_path: String,
    pub access: u32,
}

/// FindFirstFile enumeration.
pub struct FindObj {
    pub entries: Vec<vfs::DirEntry>,
    pub next: usize,
}

pub struct Win32 {
    pub cfg: Win32Config,
    pub codec: Codec,
    pub vfs: Vfs,
    pub heaps: Heaps,
    pub files: HashMap<Handle, FileObj>,
    pub finds: HashMap<Handle, FindObj>,
    /// Toolhelp thread snapshots: immutable (thread id, process id) rows and cursor.
    pub thread_snapshots: HashMap<Handle, (Vec<(u32, u32)>, usize)>,
    pub tls_used: [bool; 64],
    /// HRSRC -> (data address, size).
    pub resources: HashMap<u32, (u32, u32)>,
    pub next_hrsrc: u32,
    pub unhandled_filter: u32,
    pub user: user32::User,
    pub gdi: gdi32::Gdi,
    /// Lazily created guest strings (command line, environment).
    pub statics: HashMap<&'static str, u32>,
}

/// Standard handles (the values old Windows consoles used).
pub const STD_INPUT: u32 = 3;
pub const STD_OUTPUT: u32 = 7;
pub const STD_ERROR: u32 = 11;

impl Win32 {
    /// Create the Win32 state (process heap included) and install it into the machine.
    pub fn install<B: Backend>(m: &mut Machine<B>, cfg: Win32Config) -> Result<(), MemFault> {
        let mut heaps = Heaps::default();
        let process_heap = heaps.create(&mut m.cpu, &mut m.sys.vm)?;
        heaps.process_heap = process_heap;
        m.cpu
            .write_u32(PEB_ADDR + peb::PROCESS_HEAP, process_heap)?;
        let vfs = Vfs::new(&cfg);
        let w = Win32 {
            codec: Codec::new(cfg.codepage),
            vfs,
            heaps,
            files: HashMap::new(),
            finds: HashMap::new(),
            thread_snapshots: HashMap::new(),
            tls_used: [false; 64],
            resources: HashMap::new(),
            next_hrsrc: 0x7e80_0000,
            unhandled_filter: 0,
            statics: HashMap::new(),
            user: user32::User::new(cfg.screen, cfg.auto_dialog_ms),
            gdi: gdi32::Gdi::default(),
            cfg,
        };
        m.ext.insert(w);
        user32::install(m)?;
        Ok(())
    }

    /// Guest path of the exe.
    pub fn exe_path(&self) -> String {
        format!("{}\\{}", self.cfg.guest_root, self.cfg.exe_name)
    }

    /// A guest copy of `bytes` from the process heap, created once per `key`.
    pub fn static_bytes(
        &mut self,
        key: &'static str,
        cpu: &mut dyn Guest,
        sys: &mut Sys,
        bytes: &[u8],
    ) -> u32 {
        if let Some(&p) = self.statics.get(key) {
            return p;
        }
        let h = self.heaps.process_heap;
        let p = self
            .heaps
            .alloc(cpu, &mut sys.vm, h, bytes.len() as u32, false)
            .unwrap_or(0);
        if p != 0 {
            let _ = cpu.write(p, bytes);
        }
        self.statics.insert(key, p);
        p
    }
}

/// Register every implemented Win32 function.
pub fn register(reg: &mut Registry) {
    kernel32::register(reg);
    user32::register(reg);
    gdi32::register(reg);
    advapi32::register(reg);
    misc::register(reg);
    version::register(reg);
    winsock::register(reg);
    winmm::register(reg);
}

/// `reg.add` for stdcall functions.
pub(crate) fn stdcall(reg: &mut Registry, module: &str, name: &str, argc: u8, h: Handler) {
    reg.add(module, name, Conv::Stdcall { argc }, h);
}

pub(crate) fn cdecl(reg: &mut Registry, module: &str, name: &str, argc: u8, h: Handler) {
    reg.add(module, name, Conv::Cdecl { argc }, h);
}
