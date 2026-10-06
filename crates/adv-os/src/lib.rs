//! OS layer: what Windows does for a process before and around the guest's own code.
//!
//! * [`loader`] maps PE images, binds imports to guest images or host thunks and queues
//!   initializers; [`module`] is the module table (`HMODULE`s, exports, resources), kept
//!   in the machine's [`Ext`] so subsystems such as kernel32 can reach it.
//! * [`Process`] adds `load` and `start_process` to a [`Machine`].
//! * [`vfs`] maps guest paths onto the game directory and the data overlay, [`heap`] is
//!   the guest heap allocator, [`nls`] holds code pages and case mapping and
//!   [`registry`] is the private virtual registry; every personality layer shares them.
//!   The process's [`Heaps`] are their own `Ext` entry, and an open file is a
//!   [`vfs::FileObj`] in the machine's handle table, so any personality can reach both.
//! * [`gdi`] is the device-context table that GDI and DirectDraw share.

pub mod com;
pub mod gdi;
pub mod global;
pub mod clipboard;
pub mod heap;
pub mod loader;
pub mod module;
pub mod nls;
pub mod processor;
pub mod processes;
pub mod registry;
pub mod synthetic;
pub mod system_files;
pub mod vfs;

use std::path::PathBuf;

use adv_cpu::backend::{Backend, MemFault};
use adv_machine::{CtxId, Ext, Machine};

pub use heap::Heaps;
pub use loader::{FileSource, HOST_MODULES, HostDir, LoadError, Loader, module_file};
pub use module::{Module, ModuleId, ModuleKind, Modules, module_key};
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
    pub user_name: String,
}

impl OsConfig {
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

/// Read-only probes may observe an uninstalled OS without creating an owner.
pub fn try_modules(ext: &Ext) -> Option<&Modules> {
    ext.try_get()
}

/// The process-owned table, installed by OS loading or Win32 installation.
pub fn modules(ext: &Ext) -> &Modules {
    ext.get()
}

/// The module table of `ext`'s process, for loading and reference counts.
pub fn modules_mut(ext: &mut Ext) -> &mut Modules {
    ext.get_or_default()
}

/// Loading and starting a guest process on a [`Machine`].
pub trait Process {
    /// Load an image and its dependencies, queueing their initializers for
    /// [`Process::start_process`].
    fn load(&mut self, files: &dyn FileSource, path: &str) -> Result<ModuleId, LoadError>;

    /// Create the main context for `exe` (see [`Machine::start_main`]).
    fn start_process(&mut self, exe: ModuleId) -> Result<CtxId, MemFault>;
}

impl<B: Backend> Process for Machine<B> {
    fn load(&mut self, files: &dyn FileSource, path: &str) -> Result<ModuleId, LoadError> {
        let modules = self.ext.get_or_default::<Modules>();
        let mut l = Loader::new(&mut self.cpu, &mut self.sys, modules, files);
        let id = l.load(path)?;
        let inits = std::mem::take(&mut l.inits);
        self.sys.process_init.extend(inits);
        Ok(id)
    }

    fn start_process(&mut self, exe: ModuleId) -> Result<CtxId, MemFault> {
        let m = modules(&self.ext).get(exe);
        let (base, entry, stack, name) = (m.base, m.entry, m.stack_reserve, m.name.clone());
        self.start_main(base, entry, stack, &name)
    }
}
