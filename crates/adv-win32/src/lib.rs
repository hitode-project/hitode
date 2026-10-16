//! Win32 shim: the kernel32/user32/gdi32/winmm/... functions Game.exe and its CRT call,
//! implemented on top of the machine core.
//!
//! Grown on demand: every import is bound to a thunk, and an import without an
//! implementation fails by name when it is first called (`adv_machine::thunk`). For host
//! modules, core code-export existence follows a fixed Windows catalog; a missing
//! implementation fails explicitly when called. Other modules retain capability-based
//! dynamic lookup, and unknown optional names can still take their fallback paths.
//!
//! State lives in [`Win32`] inside the machine's `Ext`. Handlers get it with
//! `env.ext.get_mut::<Win32>()`, alongside `env.cpu` and `env.sys`.

mod advapi32;
pub mod codec_kernels;
pub mod gdi32;
mod guest_code;
pub(crate) mod heap_lock;
pub mod kernel32;
mod misc;
mod native;
mod native_heap;
mod ntdll;
pub mod personality;
pub mod printf;
mod query_leaves;
pub mod user32;
pub mod util;
mod vcomp;
mod version;
mod vfw;
pub mod winmm;
mod winsock;
mod xinput;

use std::collections::HashMap;
use std::path::PathBuf;

use adv_abi::Registry;
use adv_cpu::backend::{Backend, Guest, GuestExt, MemFault};
use adv_machine::layout::{PEB_ADDR, peb};
use adv_machine::{Conv, Handler, Machine, Sys};

use adv_os::{Codec, Heaps, OsConfig, Vfs};

/// The OS layer's configuration plus what only the Win32 personality uses.
#[derive(Clone, Debug)]
pub struct Win32Config {
    pub os: OsConfig,
    /// Minutes east of UTC for local time (JST = 540).
    pub utc_offset_min: i32,
    /// Desktop size in pixels.
    pub screen: (i32, i32),
    /// Headless runs: press a dialog's default button after this many (guest) ms. `None`
    /// leaves dialogs to the frontend.
    pub auto_dialog_ms: Option<u32>,
    /// The Windows identity every version fact is generated from.
    pub personality: personality::Profile,
}

impl Win32Config {
    pub fn new(game_dir: impl Into<PathBuf>, exe_name: &str, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            os: OsConfig::new(game_dir, exe_name, data_dir),
            utc_offset_min: 9 * 60,
            screen: (1920, 1080),
            auto_dialog_ms: None,
            personality: personality::WIN7_SP1,
        }
    }
}

pub use adv_os::vfs::FileObj;

/// One Toolhelp handle owner, with independently frozen process/thread/module/heap rows and
/// cursors. The existing public thread fields retain their meaning.
pub struct ThreadSnapshot {
    pub rows: Vec<(u32, u32)>,
    pub cursor: usize,
    pub(crate) modules: Vec<SnapshotModule>,
    pub(crate) module_cursor: usize,
    pub(crate) processes: Vec<SnapshotProcess>,
    pub(crate) process_cursor: usize,
    pub(crate) heaps: Vec<(u32, u32, u32)>,
    pub(crate) heap_cursor: usize,
}

#[derive(Clone)]
pub(crate) struct SnapshotProcess {
    pub pid: u32,
    pub parent: u32,
    pub threads: u32,
    pub name: String,
}

#[derive(Clone)]
pub(crate) struct SnapshotModule {
    pub base: u32,
    pub size: u32,
    pub pid: u32,
    pub name: String,
    pub path: String,
}

pub struct Win32 {
    pub cfg: Win32Config,
    pub codec: Codec,
    pub vfs: Vfs,
    pub tls_used: [bool; 64],
    pub unhandled_filter: u32,
    pub user: user32::User,
    pub gdi: gdi32::Gdi,
    /// Lazily created guest strings (command line, environment).
    pub statics: HashMap<&'static str, u32>,
    /// Common waitable mutex; guest initializer continuations retain ownership.
    pub(crate) loader_lock: Option<u32>,
    pub(crate) exiting_threads: std::collections::BTreeSet<adv_machine::CtxId>,
    pub(crate) process_exiting: bool,
}

/// Standard handles (the values old Windows consoles used).
pub const STD_INPUT: u32 = 3;
pub const STD_OUTPUT: u32 = 7;
pub const STD_ERROR: u32 = 11;

impl Win32 {
    /// System infrastructure is readable before a suspended child runs its
    /// loader gate or a caller redirects its initial thread context.
    pub fn prepare_process_infrastructure<B: Backend>(m: &mut Machine<B>) -> Result<(), String> {
        kernel32::prepare_system_modules(m)
    }

    /// Create the Win32 state and the process heap and install them into the machine. The
    /// heaps are their own `Ext` entry: other personalities allocate from them too.
    pub fn install<B: Backend>(m: &mut Machine<B>, cfg: Win32Config) -> Result<(), MemFault> {
        m.ext.get_or_default::<adv_os::Modules>();
        let mut heaps = Heaps::default();
        let process_heap = heaps.create(&mut m.cpu, &mut m.sys.vm)?;
        heaps.process_heap = process_heap;
        heaps.enable_small(&mut m.cpu)?;
        m.cpu
            .write_u32(PEB_ADDR + peb::PROCESS_HEAP, process_heap)?;
        ntdll::install_peb_lock(m)?;
        personality::install_peb(&mut m.cpu, &cfg.personality)?;
        let vfs = Vfs::new(&cfg.os);
        let w = Win32 {
            codec: Codec::new(cfg.os.codepage),
            vfs,
            tls_used: [false; 64],
            unhandled_filter: 0,
            statics: HashMap::new(),
            loader_lock: None,
            exiting_threads: Default::default(),
            process_exiting: false,
            user: user32::User::new(&mut m.sys.objects, cfg.screen, cfg.auto_dialog_ms)
                .ok_or(MemFault { addr: 0, len: 0 })?,
            gdi: gdi32::Gdi::default(),
            cfg,
        };
        m.ext.insert(w);
        kernel32::install_native_services(m)?;
        kernel32::install_exceptions(m);
        gdi32::raster::install(m);
        kernel32::install_startup(m)?;
        m.ext.insert(heaps);
        ole32::install(m);
        gdi32::font::install(m);
        user32::install(m)?;
        msvcrt::initialize_data(m)?;
        kernel32::install_parameters(m)?;
        Ok(())
    }

    /// Guest path of the exe.
    pub fn exe_path(&self) -> String {
        self.cfg.os.exe_path()
    }

    /// A guest copy of `bytes` from the process heap, created once per `key`.
    pub fn static_bytes(
        &mut self,
        key: &'static str,
        cpu: &mut dyn Guest,
        sys: &mut Sys,
        heaps: &mut Heaps,
        bytes: &[u8],
    ) -> u32 {
        if let Some(&p) = self.statics.get(key) {
            return p;
        }
        let h = heaps.process_heap;
        let p = heaps
            .alloc(cpu, &mut sys.vm, h, bytes.len() as u32, false)
            .unwrap_or(0);
        if p != 0 {
            let _ = cpu.write(p, bytes);
        }
        self.statics.insert(key, p);
        p
    }
}

/// Change the personality's ANSI code page and refresh its existing process string.
pub fn set_codepage(env: &mut adv_machine::Env<'_>, codepage: u32) -> Result<(), MemFault> {
    kernel32::configure_codepage(env, codepage)
}

/// Register every implemented Win32 function.
pub fn register(reg: &mut Registry) {
    kernel32::register(reg);
    user32::register(reg);
    gdi32::register(reg);
    advapi32::register(reg);
    misc::register(reg);
    imm32::register(reg);
    version::register(reg);
    vfw::register(reg);
    winsock::register(reg);
    winmm::register(reg);
    xinput::register(reg);
    vcomp::register(reg);
    ntdll::register(reg);
    guest_code::install(reg);
    query_leaves::install(reg);
}

/// `reg.add` for stdcall functions.
pub(crate) fn stdcall(reg: &mut Registry, module: &str, name: &str, argc: u8, h: Handler) {
    reg.add(module, name, Conv::Stdcall { argc }, h);
}

pub mod avifil32;
pub mod compat;
pub mod imm32;
pub mod msacm32;
pub mod msvcrt;
pub mod ole32;
pub mod vorbisfile;
