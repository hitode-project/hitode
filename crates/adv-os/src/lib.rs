//! OS layer: what Windows does for a process before and around the guest's own code.
//!
//! * [`loader`] maps PE images, binds imports to guest images or host thunks and queues
//!   initializers; [`module`] is the module table (`HMODULE`s, exports, resources), kept
//!   in the machine's [`Ext`] so subsystems such as kernel32 can reach it.
//! * [`Process`] adds `load` and `start_process` to a [`Machine`].

pub mod loader;
pub mod module;

use std::sync::OnceLock;

use adv_cpu::backend::{Backend, MemFault};
use adv_machine::{CtxId, Ext, Machine};

pub use loader::{FileSource, HOST_MODULES, HostDir, LoadError, Loader, module_file};
pub use module::{Module, ModuleId, ModuleKind, Modules, module_key};

/// The module table of `ext`'s process (empty before anything is loaded).
pub fn modules(ext: &Ext) -> &Modules {
    static EMPTY: OnceLock<Modules> = OnceLock::new();
    ext.try_get()
        .unwrap_or_else(|| EMPTY.get_or_init(Modules::default))
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
