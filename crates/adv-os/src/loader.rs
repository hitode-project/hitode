//! PE image loader.
//!
//! * Images load at their preferred base when it is free (TL2026's exe has no relocations).
//!   Otherwise they are rebased with their fixups; images without a relocation directory
//!   and without code (RESOURCE.dll / FX.dll share base 0x10000000) are moved as-is.
//! * Imports and delay imports are bound eagerly. Shipped DLLs win over synthetic
//!   system images; an explicit whole-module provider requires a capability check.
//!   Delay-load slots and the delay descriptor's HMODULE slot are prefilled, so
//!   the guest's `__delayLoadHelper2` never runs.
//! * Static TLS templates are recorded; each guest context gets its blocks at spawn.
//! * Initializers (TLS callbacks, DllMain) are returned as [`InitCall`]s, which the process
//!   start thunk runs as a chain of guest calls on the main context.

mod aot;

use adv_cpu::backend::{Guest, GuestExt, MemFault, Prot};
use adv_pe::{ExportTarget, ImportName, Pe, PeError, file_flags, scn};
use thiserror::Error;

use adv_machine::layout::{HOST_MODULE_CAP, PAGE};
use adv_machine::vm::align_up;
use adv_machine::{Image, InitCall, Sys, TlsImage};

use crate::module::{Lifetime, Module, ModuleId, ModuleKind, Modules, module_key};

/// System modules available from the synthetic System32 view. A game-side proxy
/// takes precedence for a bare module name; an absolute System32 path bypasses it.
pub const HOST_MODULES: &[&str] = &[
    "kernel32.dll",
    "user32.dll",
    "gdi32.dll",
    "advapi32.dll",
    "shell32.dll",
    "shlwapi.dll",
    "ole32.dll",
    "oleaut32.dll",
    "comdlg32.dll",
    "comctl32.dll",
    "riched32.dll",
    "winmm.dll",
    "imm32.dll",
    "version.dll",
    "dsound.dll",
    "ddraw.dll",
    "d3d9.dll",
    "dinput8.dll",
    "ntdll.dll",
    "msvcrt.dll",
    "sensapi.dll",
    "ws2_32.dll",
    "wininet.dll",
    "uxtheme.dll",
    "dwmapi.dll",
    "mpr.dll",
    "gdiplus.dll",
];

const DLL_PROCESS_ATTACH: u32 = 1;

fn begin_initialization(env: &mut adv_machine::Env<'_>, id: usize) {
    crate::modules_mut(env.ext)
        .get_mut(id)
        .lifetime
        .attach_started = true;
}

/// Guest-visible file access for the loader. `path` is exactly what the guest asked for
/// (an import descriptor name, possibly relative, or a LoadLibrary argument).
pub trait FileSource {
    fn read(&self, path: &str) -> Option<Vec<u8>>;
    /// A loader-only read may defer to work outside the synchronous load stack.
    /// The caller supplies that result through this source before retrying the
    /// same transaction. Ordinary file probes keep using `read`.
    fn read_for_loader(&self, path: &str) -> Result<Option<Vec<u8>>, LoadError> {
        Ok(self.read(path))
    }
    fn absolute_path(&self, path: &str) -> String {
        if path.contains(':') {
            path.replace('/', "\\")
        } else {
            format!("C:\\Game\\{path}")
        }
    }
}

/// Resolve only module files. Ordinary guest file operations retain their exact
/// paths. Share this order with capability preflight so it inspects the same DLL
/// the loader will use: named file, bare name next to its importer, then dll/.
pub fn module_file(
    files: &dyn FileSource,
    path: &str,
    importer_dir: Option<&str>,
) -> Option<(String, Vec<u8>)> {
    module_file_with(path, importer_dir, |candidate| Ok(files.read(candidate)))
        .ok()
        .flatten()
}

fn module_file_with(
    path: &str,
    importer_dir: Option<&str>,
    mut read: impl FnMut(&str) -> Result<Option<Vec<u8>>, LoadError>,
) -> Result<Option<(String, Vec<u8>)>, LoadError> {
    if let Some(bytes) = read(path)? {
        return Ok(Some((path.to_owned(), bytes)));
    }
    if !path.contains(['\\', '/']) {
        if let Some(dir) = importer_dir.filter(|d| !d.is_empty()) {
            let near = format!("{dir}\\{path}");
            if let Some(bytes) = read(&near)? {
                return Ok(Some((near, bytes)));
            }
        }
        let bundled = format!("dll\\{path}");
        if let Some(bytes) = read(&bundled)? {
            return Ok(Some((bundled, bytes)));
        }
    }
    Ok(None)
}

#[derive(Debug, Error)]
pub enum LoadError {
    /// No initializer runs before this request unwinds the current transaction.
    #[error("{0}: loader file read deferred")]
    DeferredRead(String),
    #[error("{0}: not found")]
    NotFound(String),
    #[error("{path}: {err}")]
    Pe { path: String, err: PeError },
    #[error("{0}: preferred base {1:#x} is taken and the image has no relocations")]
    NoRelocs(String, u32),
    #[error("{0}: no free address space for {1:#x} bytes")]
    NoSpace(String, u32),
    #[error("{module}: export {name} not found")]
    MissingExport { module: String, name: String },
    #[error("too many host modules")]
    TooManyHostModules,
    #[error("guest memory: {0}")]
    Mem(#[from] MemFault),
}

pub fn is_host_module(sys: &Sys, key: &str) -> bool {
    HOST_MODULES.contains(&key)
        || key.starts_with("api-ms-win-")
        || sys.thunks.resolver().provides(key)
}

pub fn is_system_path(path: &str) -> bool {
    let path = path.replace('/', "\\");
    ["C:\\Windows\\system32\\", "C:\\Windows\\syswow64\\"]
        .iter()
        .any(|prefix| {
            path.get(..prefix.len())
                .is_some_and(|part| part.eq_ignore_ascii_case(prefix))
                && !path[prefix.len()..].contains('\\')
        })
}

/// A mapped executable whose guest imports and initializers have not run.
/// Kept by the process supervisor until the primary thread first becomes ready.
pub struct PreparedProcess {
    pub module: ModuleId,
    initializers: Vec<InitCall>,
    delay: Vec<adv_pe::DelayImportModule>,
    sections: Vec<(u32, u32)>,
}

pub struct Loader<'a> {
    pub cpu: &'a mut dyn Guest,
    pub sys: &'a mut Sys,
    pub modules: &'a mut Modules,
    pub files: &'a dyn FileSource,
    /// Initializers of newly loaded images, dependencies first.
    pub inits: Vec<InitCall>,
    /// Guest directories of the images whose imports are being bound (innermost last).
    importers: Vec<String>,
    /// Per-load dependency search root for LOAD_WITH_ALTERED_SEARCH_PATH.
    altered_directory: Option<String>,
    changes: ResolutionChanges,
    defer_executable: bool,
    pending_delay: Vec<adv_pe::DelayImportModule>,
    pending_sections: Vec<(u32, u32)>,
}

/// Mutations made while following dynamic export forwarders. The frontend can
/// undo only these edges if a newly loaded DLL rejects process initialization.
/// Existing module references and mutations from reentrant guest calls survive.
#[derive(Default)]
pub struct ResolutionChanges {
    created: Vec<ModuleId>,
    dependencies: Vec<(ModuleId, ModuleId)>,
}

fn prot_of(characteristics: u32) -> Prot {
    let mut p = Prot::default();
    if characteristics & scn::MEM_READ != 0 {
        p = p | Prot::READ;
    }
    if characteristics & scn::MEM_WRITE != 0 {
        p = p | Prot::WRITE;
    }
    if characteristics & scn::MEM_EXECUTE != 0 {
        p = p | Prot::EXEC;
    }
    if p == Prot::default() { Prot::READ } else { p }
}

impl<'a> Loader<'a> {
    pub fn new(
        cpu: &'a mut dyn Guest,
        sys: &'a mut Sys,
        modules: &'a mut Modules,
        files: &'a dyn FileSource,
    ) -> Self {
        Self {
            cpu,
            sys,
            modules,
            files,
            inits: Vec::new(),
            importers: Vec::new(),
            altered_directory: None,
            changes: ResolutionChanges::default(),
            defer_executable: false,
            pending_delay: Vec::new(),
            pending_sections: Vec::new(),
        }
    }

    /// Load `path` (or bump the load count if it is already loaded).
    pub fn load(&mut self, path: &str) -> Result<ModuleId, LoadError> {
        let initializers = self.inits.len();
        let result = self.load_inner(path, true);
        let changes = std::mem::take(&mut self.changes);
        if result.is_err() {
            self.inits.truncate(initializers);
            self.rollback_storage(changes)?;
        }
        result
    }

    pub fn prepare_process(&mut self, path: &str) -> Result<PreparedProcess, LoadError> {
        self.defer_executable = true;
        let loaded = self.load(path);
        self.defer_executable = false;
        let module = loaded?;
        if self.modules.get(module).is_dll {
            return Err(LoadError::Pe {
                path: path.into(),
                err: PeError::Malformed("process image is a DLL".into()),
            });
        }
        Ok(PreparedProcess {
            module,
            initializers: std::mem::take(&mut self.inits),
            delay: std::mem::take(&mut self.pending_delay),
            sections: std::mem::take(&mut self.pending_sections),
        })
    }

    /// Bind the live guest import directory, including allocations beyond the
    /// image's SizeOfImage. This is normal PE loading after CREATE_SUSPENDED,
    /// with no knowledge of a particular injector, DLL name or payload format.
    pub fn finish_process(&mut self, prepared: PreparedProcess) -> Result<(), LoadError> {
        let module = self.modules.get(prepared.module);
        let (base, path) = (module.base, module.path.clone());
        let invalid = || LoadError::Pe {
            path: path.clone(),
            err: PeError::Malformed("invalid mapped process import directory".into()),
        };
        let add = |base: u32, offset: u32| base.checked_add(offset).ok_or_else(invalid);
        let read = |cpu: &dyn Guest, address| -> Result<u32, LoadError> {
            if !cpu.accessible(address, 4, Prot::READ) {
                return Err(invalid());
            }
            Ok(cpu.read_u32(address)?)
        };
        if !self.cpu.accessible(base, 64, Prot::READ) || self.cpu.read_u16(base)? != 0x5a4d {
            return Err(invalid());
        }
        let nt_offset = read(self.cpu, add(base, 0x3c)?)?;
        if nt_offset > 0x100000 {
            return Err(invalid());
        }
        let nt = add(base, nt_offset)?;
        if !self.cpu.accessible(nt, 136, Prot::READ)
            || read(self.cpu, nt)? != 0x4550
            || self.cpu.read_u16(add(nt, 20)?)? < 112
            || self.cpu.read_u16(add(nt, 24)?)? != 0x10b
            || read(self.cpu, add(nt, 116)?)? < 2
        {
            return Err(invalid());
        }
        let import = read(self.cpu, add(nt, 128)?)?;
        let directory = path
            .rfind(['\\', '/'])
            .map_or(String::new(), |at| path[..at].to_owned());
        self.importers.push(directory);
        let result = (|| {
            let mut writes = Vec::new();
            if import != 0 {
                let table = add(base, import)?;
                let mut ended = false;
                for index in 0..512u32 {
                    let descriptor = add(table, index * 20)?;
                    let original = read(self.cpu, descriptor)?;
                    let name = read(self.cpu, add(descriptor, 12)?)?;
                    let first = read(self.cpu, add(descriptor, 16)?)?;
                    if original == 0 && name == 0 && first == 0 {
                        ended = true;
                        break;
                    }
                    if name == 0 || first == 0 {
                        return Err(invalid());
                    }
                    let dll = mapped_string(self.cpu, add(base, name)?, 32768)?;
                    let dependency = self.load_inner(&dll, false)?;
                    self.dependency(prepared.module, dependency)?;
                    let lookup = add(base, if original != 0 { original } else { first })?;
                    let target = add(base, first)?;
                    let mut terminated = false;
                    for n in 0..65536u32 {
                        let word = read(self.cpu, add(lookup, n * 4)?)?;
                        if word == 0 {
                            terminated = true;
                            break;
                        }
                        if writes.len() >= 65536 {
                            return Err(invalid());
                        }
                        let symbol = if word & 0x80000000 != 0 {
                            ImportName::Ordinal(word as u16)
                        } else {
                            let hint = add(base, word)?;
                            if !self.cpu.accessible(hint, 2, Prot::READ) {
                                return Err(invalid());
                            }
                            ImportName::Name {
                                hint: self.cpu.read_u16(hint)?,
                                name: mapped_string(self.cpu, add(hint, 2)?, 4096)?,
                            }
                        };
                        let address = self.resolve_inner(dependency, &symbol, 0, false)?;
                        let slot = add(target, n * 4)?;
                        if !self.cpu.accessible(slot, 4, Prot::default()) {
                            return Err(invalid());
                        }
                        writes.push((slot, address));
                    }
                    if !terminated {
                        return Err(invalid());
                    }
                }
                if !ended {
                    return Err(invalid());
                }
            }
            // Preserve the existing eager delay-load shortcut without rewriting
            // any other bytes that the parent patched in the mapped image.
            for delayed in prepared.delay {
                let dependency = self.load_inner(&delayed.dll, false)?;
                self.dependency(prepared.module, dependency)?;
                if delayed.module_handle_rva != 0 {
                    writes.push((
                        add(base, delayed.module_handle_rva)?,
                        self.modules.get(dependency).base,
                    ));
                }
                for import in delayed.imports {
                    let address = self.resolve_inner(dependency, &import.name, 0, false)?;
                    writes.push((add(base, import.iat_rva)?, address));
                }
            }
            if writes
                .iter()
                .any(|(slot, _)| !self.cpu.accessible(*slot, 4, Prot::default()))
            {
                return Err(invalid());
            }
            let bound_slots: Vec<_> = writes.iter().map(|&(slot, _)| slot - base).collect();
            for (slot, address) in writes {
                self.cpu.write_u32(slot, address)?;
            }
            self.privatize_import_sections(base, &prepared.sections, &bound_slots)?;
            Ok(())
        })();
        self.importers.pop();
        result?;
        self.inits.extend(prepared.initializers);
        Ok(())
    }

    /// Load with the requested absolute DLL's directory ahead of the normal
    /// dependency search. The policy lasts only for this load and its graph;
    /// later LoadLibrary calls from DllMain use their own ordinary policy.
    pub fn load_altered(&mut self, path: &str) -> Result<ModuleId, LoadError> {
        let normalized = path.replace('/', "\\");
        let absolute = normalized.starts_with("\\\\")
            || (normalized.as_bytes().get(1) == Some(&b':')
                && normalized.as_bytes().get(2) == Some(&b'\\'));
        let directory = absolute
            .then(|| {
                normalized
                    .rfind('\\')
                    .map(|end| normalized[..end].to_owned())
            })
            .flatten();
        let previous = std::mem::replace(&mut self.altered_directory, directory);
        let result = self.load(path);
        self.altered_directory = previous;
        result
    }

    fn dependency(&mut self, owner: ModuleId, dependency: ModuleId) -> Result<(), LoadError> {
        if !self
            .modules
            .get(owner)
            .lifetime
            .dependencies
            .contains(&dependency)
        {
            self.changes.dependencies.push((owner, dependency));
            self.modules.dependency(self.cpu, owner, dependency)?;
        }
        Ok(())
    }

    fn remove_edges(&mut self, changes: &ResolutionChanges) -> Result<(), LoadError> {
        for &(owner, dependency) in changes.dependencies.iter().rev() {
            if self.modules.try_get(owner).is_some() {
                self.modules
                    .get_mut(owner)
                    .lifetime
                    .dependencies
                    .remove(&dependency);
            }
        }
        self.modules.sync_counts(self.cpu)?;
        Ok(())
    }

    fn rollback_storage(&mut self, changes: ResolutionChanges) -> Result<(), LoadError> {
        // No guest initializer ran yet. First detach graph edges (including
        // edges added to pre-existing forwarder modules), then retire new images.
        self.remove_edges(&changes)?;
        for id in changes.created.into_iter().rev() {
            if self.modules.get(id).kind == ModuleKind::Image {
                self.retire(id)?;
            }
        }
        Ok(())
    }

    /// Return the detach plan after a dynamic forwarder's initializer fails.
    /// New explicit references acquired by guest callbacks remain graph roots.
    pub fn abort_resolution(
        &mut self,
        mut changes: ResolutionChanges,
    ) -> Result<Vec<ModuleId>, LoadError> {
        // Keep dependencies owned by new images until their detach callbacks
        // finish. They determine reverse-initialization order, and a reentrant
        // LoadLibrary may have retained one of those images independently.
        let created: std::collections::BTreeSet<_> = changes.created.iter().copied().collect();
        changes
            .dependencies
            .retain(|(owner, _)| !created.contains(owner));
        self.remove_edges(&changes)?;
        Ok(self.modules.unreachable())
    }

    fn load_inner(&mut self, path: &str, retain: bool) -> Result<ModuleId, LoadError> {
        // Both LoadLibrary and PE forwarders accept a module name without the
        // extension. Normalize the file lookup as well as the module-table key.
        let extended;
        let path = if !path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(path)
            .contains('.')
        {
            extended = format!("{path}.dll");
            extended.as_str()
        } else {
            path
        };
        let key = module_key(path);
        let system = is_system_path(path);
        let loaded = if system {
            self.modules.system(&key)
        } else {
            self.modules.by_name(path).or_else(|| self.same_file(path))
        };
        if let Some(id) = loaded {
            if retain {
                self.modules.retain(self.cpu, id)?;
            }
            return Ok(id);
        }
        if system {
            return if is_host_module(self.sys, &key) {
                self.load_host(key, path, retain)
            } else {
                Err(LoadError::NotFound(path.to_owned()))
            };
        }
        if self.sys.thunks.resolver().replaces_module(&key) {
            return self.load_host(key, path, retain);
        }
        let altered = match (
            !self.importers.is_empty() && !path.contains(['\\', '/']),
            self.altered_directory.as_ref(),
        ) {
            (true, Some(directory)) => {
                let near = format!("{directory}\\{path}");
                self.files
                    .read_for_loader(&near)?
                    .map(|bytes| (near, bytes))
            }
            _ => None,
        };
        let resolved = match altered {
            Some(found) => Some(found),
            None => module_file_with(
                path,
                self.importers.last().map(String::as_str),
                |candidate| self.files.read_for_loader(candidate),
            )?,
        };
        if let Some((resolved, bytes)) = resolved {
            if resolved != path {
                log::info!("{path}: found as {resolved}");
            }
            return self.load_image(key, &resolved, bytes, retain);
        }
        // An extension alone does not establish a system provider. Optional
        // absent DLLs must fail rather than publish a fabricated module handle.
        if is_host_module(self.sys, &key) {
            return self.load_host(key, path, retain);
        }
        // Windows refuses to start an image whose static import is missing, so
        // an absent import of a natively runnable image is a system DLL (glu32,
        // wtsapi32, a VC++ redistributable). Its unimplemented exports bind to
        // diagnosed stubs; dynamic probes above still fail.
        if !self.importers.is_empty() {
            log::info!(
                "{path}: imported but absent from the game files, binding as a system module"
            );
            return self.load_host(key, path, retain);
        }
        Err(LoadError::NotFound(path.to_owned()))
    }

    /// A path naming an image already loaded through another spelling, such as
    /// an absolute path to a DLL first found relative to the game directory.
    fn same_file(&self, path: &str) -> Option<ModuleId> {
        if !path.contains(['\\', '/']) {
            return None;
        }
        let wanted = self.files.absolute_path(path);
        self.modules
            .iter()
            .find(|(_, m)| {
                m.kind == ModuleKind::Image
                    && m.executable_visible()
                    && self
                        .files
                        .absolute_path(&m.path)
                        .eq_ignore_ascii_case(&wanted)
            })
            .map(|(id, _)| id)
    }

    fn load_host(&mut self, key: String, _path: &str, retain: bool) -> Result<ModuleId, LoadError> {
        let n = self.modules.host_count();
        if n >= HOST_MODULE_CAP {
            return Err(LoadError::TooManyHostModules);
        }
        let base = self.modules.system_image_base(&key);
        let image = system_image(self.cpu, self.sys, &key, base)?;
        let size = image.bytes.len() as u32;
        if !self.modules.reserve_system_image(&key, base, size) {
            return Err(LoadError::NoSpace(key, size));
        }
        let pe = Pe::parse(&image.bytes).map_err(|err| LoadError::Pe {
            path: key.clone(),
            err,
        })?;
        let mut exports_by_name = std::collections::HashMap::new();
        let mut exports_by_ordinal = std::collections::HashMap::new();
        for export in pe.exports().map_err(|err| LoadError::Pe {
            path: key.clone(),
            err,
        })? {
            if let Some(name) = export.name {
                exports_by_name.insert(name, export.target.clone());
            }
            exports_by_ordinal.insert(export.ordinal, export.target);
        }
        self.cpu
            .map_private_copy_pages(base, &image.bytes, Prot::RW)?;
        self.cpu.protect(base, size, Prot::READ)?;
        self.cpu
            .protect(base + image.text_rva, image.text_size, Prot::RX)?;
        self.sys
            .thunks
            .register_thunk_pages(base + image.text_rva, image.text_size);
        if image.data_size != 0 {
            self.cpu
                .protect(base + image.data_rva, image.data_size, Prot::RW)?;
        }
        self.sys.images.add(Image {
            name: key.clone(),
            base,
            size,
        });
        let path = format!("C:\\Windows\\system32\\{key}");
        let id = self.modules.add(Module {
            path: path.clone(),
            name: key,
            kind: ModuleKind::Host,
            base,
            size,
            is_dll: true,
            entry: 0,
            stack_reserve: 0,
            exports_by_name,
            exports_by_ordinal,
            resources: None,
            data_file: None,
            load_count: 1,
            ldr_entry: 0,
            ldr_size: 0,
            lifetime: Lifetime {
                mapped: true,
                pinned: true,
                external_refs: u32::from(retain),
                ..Lifetime::default()
            },
        });
        self.changes.created.push(id);
        self.modules.publish_ldr(self.cpu, id, &path)?;
        self.modules.complete_ldr(self.cpu, id)?;
        self.modules.capture_image_protections(self.cpu, id);
        if self.modules.get(id).name == "ntdll.dll" {
            if let Some(&rva) = image.private_entries.get("return_pad") {
                self.sys.set_guest_return_entry(base + rva);
            }
            if let Some(&rva) = image.private_entries.get("thread_exception_handler") {
                self.sys.set_thread_exception_entry(base + rva);
            }
        }
        Ok(id)
    }

    fn load_image(
        &mut self,
        key: String,
        path: &str,
        bytes: Vec<u8>,
        retain: bool,
    ) -> Result<ModuleId, LoadError> {
        let pe_err = |err| LoadError::Pe {
            path: path.to_owned(),
            err,
        };
        let pe = Pe::parse(&bytes).map_err(pe_err)?;
        let identity = self
            .cpu
            .wants_code_images()
            .then(|| adv_cpu::AotImage::identity(&bytes));
        drop(bytes);

        let size = align_up(pe.size_of_image, PAGE);
        pe.relocations().map_err(pe_err)?;
        let base = if self.sys.vm.reserve_at(pe.image_base, size) {
            pe.image_base
        } else {
            let stripped = pe.characteristics & file_flags::RELOCS_STRIPPED != 0;
            // A valid image can be position-independent and need zero fixups.
            // Native loading rejects relocation by the explicit stripped flag,
            // not by the number of entries or the presence of executable code.
            if stripped {
                return Err(LoadError::NoRelocs(path.to_owned(), pe.image_base));
            }
            // The image view spans SizeOfImage; the granule tail stays free.
            self.sys
                .vm
                .reserve_exact(size)
                .ok_or_else(|| LoadError::NoSpace(path.to_owned(), size))?
        };
        let result = self.populate_image(key, path, pe, base, size, retain, identity);
        if result.is_err() && self.modules.by_handle(base).is_none() {
            self.sys.vm.free(self.cpu, base)?;
        }
        result
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "image placement and identity are passed together to the loader"
    )]
    fn populate_image(
        &mut self,
        key: String,
        path: &str,
        mut pe: Pe,
        base: u32,
        size: u32,
        retain: bool,
        identity: Option<[u8; 32]>,
    ) -> Result<ModuleId, LoadError> {
        let pe_err = |err| LoadError::Pe {
            path: path.to_owned(),
            err,
        };
        if base != pe.image_base {
            let n = pe.rebase(base).map_err(pe_err)?;
            log::debug!(
                "{key}: rebased {:#x} -> {base:#x} ({n} fixups)",
                pe.image_base
            );
        }

        let mut exports_by_name = std::collections::HashMap::new();
        let mut exports_by_ordinal = std::collections::HashMap::new();
        for e in pe.exports().map_err(pe_err)? {
            if let Some(n) = &e.name {
                exports_by_name.insert(n.clone(), e.target.clone());
            }
            exports_by_ordinal.insert(e.ordinal, e.target);
        }
        // Registered before binding so import cycles terminate. TLS is registered before
        // binding too, so the exe gets TLS index 0 as on Windows.
        let id = self.modules.add(Module {
            name: key.clone(),
            path: path.to_owned(),
            kind: ModuleKind::Image,
            base,
            size,
            is_dll: pe.is_dll(),
            entry: if pe.entry_rva != 0 {
                base + pe.entry_rva
            } else {
                0
            },
            stack_reserve: pe.stack_reserve,
            exports_by_name,
            exports_by_ordinal,
            resources: pe.resources().map_err(pe_err)?,
            data_file: None,
            load_count: 1,
            ldr_entry: 0,
            ldr_size: 0,
            lifetime: Lifetime {
                pinned: !pe.is_dll(),
                external_refs: u32::from(retain),
                ..Lifetime::default()
            },
        });
        self.changes.created.push(id);
        self.modules
            .publish_ldr(self.cpu, id, &self.files.absolute_path(path))?;
        self.sys.images.add(Image {
            name: key.clone(),
            base,
            size,
        });

        let mut tls_callbacks = Vec::new();
        if let Some(tls) = pe.tls().map_err(pe_err)? {
            // The directory's VAs were fixed up with the rest of the image.
            let len = tls.raw_end_va.saturating_sub(tls.raw_start_va);
            let template = pe
                .slice(tls.raw_start_va.wrapping_sub(base), len)
                .map_err(pe_err)?
                .to_vec();
            let index = self.sys.add_static_tls(
                self.cpu,
                TlsImage {
                    index: 0,
                    template,
                    zero_fill: tls.zero_fill,
                    active: true,
                },
            )?;
            self.modules.get_mut(id).lifetime.tls_index = Some(index);
            if tls.index_va != 0 {
                pe.set_u32(tls.index_va.wrapping_sub(base), index)
                    .map_err(pe_err)?;
            }
            tls_callbacks = pe.tls_callbacks(&tls, base).map_err(pe_err)?;
        }

        // Bind the current import directory, including Detours-added DLLs.
        // Historical imports in .detour are rollback metadata, not loader policy.

        let dir = path
            .rfind(['\\', '/'])
            .map_or(String::new(), |i| path[..i].to_owned());
        self.importers.push(dir);
        let sections: Vec<_> = pe
            .sections
            .iter()
            .filter(|section| section.mem_size() != 0)
            .map(|section| {
                let start = section.virtual_address & !(PAGE - 1);
                let length = align_up(section.mem_size() + section.virtual_address - start, PAGE);
                (start, length)
            })
            .collect();
        if !pe.is_dll() {
            // IMAGE_DLLCHARACTERISTICS_NX_COMPAT opts a 32-bit process into DEP.
            self.modules.decide_dep(pe.dll_characteristics & 0x100 != 0);
        }
        let bound = if self.defer_executable && !pe.is_dll() {
            self.pending_delay = pe.delay_imports_at(base).unwrap_or_default();
            self.pending_sections = sections.clone();
            Ok(Vec::new())
        } else {
            self.bind(&mut pe, path, base, id)
        };
        self.importers.pop();
        let bound_slots = bound?;

        // Seed private-copy pages without consuming their first guest write.
        pe.image.resize(size as usize, 0);
        self.cpu.map_private_copy_pages(base, &pe.image, Prot::RW)?;
        self.privatize_import_sections(base, &sections, &bound_slots)?;
        if pe.section_alignment >= PAGE {
            self.cpu
                .protect(base, align_up(pe.size_of_headers.max(1), PAGE), Prot::READ)?;
            for s in &pe.sections {
                let len = align_up(s.mem_size(), PAGE);
                if len != 0 {
                    self.cpu
                        .protect(base + s.virtual_address, len, prot_of(s.characteristics))?;
                }
            }
        }
        log::info!("loaded {key} at {base:#010x} (+{size:#x})");
        self.modules.get_mut(id).lifetime.mapped = true;
        self.modules.capture_image_protections(self.cpu, id);
        if self.modules.dep_disabled() && pe.section_alignment >= PAGE {
            // Captured protections keep the section flags the guest queries.
            self.cpu.protect(
                base,
                align_up(pe.size_of_headers.max(1), PAGE),
                Prot::READ | Prot::EXEC,
            )?;
            for s in &pe.sections {
                let len = align_up(s.mem_size(), PAGE);
                if len != 0 {
                    let prot = prot_of(s.characteristics) | Prot::EXEC;
                    self.cpu.protect(base + s.virtual_address, len, prot)?;
                }
            }
        }

        self.modules.get_mut(id).lifetime.callbacks = tls_callbacks.clone();
        for cb in tls_callbacks {
            self.inits.push(InitCall {
                target: cb,
                args: vec![base, DLL_PROCESS_ATTACH, 0],
                what: format!("TLS callback of {key}"),
                failure_exit: None,
                begin: Some((begin_initialization, id)),
            });
        }
        let m = self.modules.get(id);
        if m.is_dll && m.entry != 0 {
            self.inits.push(InitCall {
                target: m.entry,
                args: vec![base, DLL_PROCESS_ATTACH, 0],
                what: format!("DllMain of {key}"),
                failure_exit: Some(0xc0000142),
                begin: Some((begin_initialization, id)),
            });
        }
        self.modules.complete_ldr(self.cpu, id)?;
        if let Some(identity) = identity {
            aot::register(self.cpu, &mut pe, base, path, identity);
        }
        Ok(id)
    }

    /// Fill the IAT and delay-load IAT (and the delay descriptor's HMODULE slot).
    fn bind(
        &mut self,
        pe: &mut Pe,
        path: &str,
        base: u32,
        owner: ModuleId,
    ) -> Result<Vec<u32>, LoadError> {
        let pe_err = |err| LoadError::Pe {
            path: path.to_owned(),
            err,
        };
        let mut bound_slots = Vec::new();
        for m in pe.imports().map_err(pe_err)? {
            let dep = self.load_inner(&m.dll, false)?;
            self.dependency(owner, dep)?;
            for imp in &m.imports {
                let addr = self.resolve_inner(dep, &imp.name, 0, false)?;
                pe.set_u32(imp.iat_rva, addr).map_err(pe_err)?;
                bound_slots.push(imp.iat_rva);
            }
        }
        // The Windows loader never reads the delay-import directory; the guest's
        // __delayLoadHelper2 does, at first call. Prefilling is only a shortcut, so skip it
        // when the directory isn't valid at load time (packed images unpack it later).
        let delay = pe.delay_imports_at(base).unwrap_or_else(|e| {
            log::warn!("{path}: delay imports left to the guest's helper ({e})");
            Vec::new()
        });
        for m in delay {
            let dep = self.load_inner(&m.dll, false)?;
            self.dependency(owner, dep)?;
            if m.module_handle_rva != 0 {
                pe.set_u32(m.module_handle_rva, self.modules.get(dep).base)
                    .map_err(pe_err)?;
                bound_slots.push(m.module_handle_rva);
            }
            for imp in &m.imports {
                let addr = self.resolve_inner(dep, &imp.name, 0, false)?;
                pe.set_u32(imp.iat_rva, addr).map_err(pe_err)?;
                bound_slots.push(imp.iat_rva);
            }
        }
        Ok(bound_slots)
    }

    /// Import binding makes its containing section private, including untouched
    /// pages. Keep the original mapped sections when process binding is deferred.
    fn privatize_import_sections(
        &mut self,
        base: u32,
        sections: &[(u32, u32)],
        slots: &[u32],
    ) -> Result<(), LoadError> {
        for &(start, length) in sections {
            if slots.iter().any(|&slot| slot.wrapping_sub(start) < length) {
                self.cpu.privatize_copy_pages(base + start, length)?;
            }
        }
        Ok(())
    }

    /// Address of `sym` in module `m` (a thunk for host modules). Follows forwarders.
    pub fn resolve(&mut self, m: ModuleId, sym: &ImportName) -> Result<u32, LoadError> {
        self.resolve_transaction(m, sym, false)
            .map(|(address, _)| address)
    }

    /// Dynamic probes never fabricate diagnostic imports for missing exports,
    /// including when the missing function is reached through a forwarder.
    pub fn resolve_dynamic(
        &mut self,
        m: ModuleId,
        sym: &ImportName,
    ) -> Result<(u32, ResolutionChanges), LoadError> {
        self.resolve_transaction(m, sym, true)
    }

    /// Inspect a mapped synthetic code export without calling its pristine
    /// implementation. A live EAT replacement or changed entry belongs to the
    /// guest; untouched entries return None regardless of host availability.
    /// Inspection never loads dependencies or queues initializers. Changed
    /// forwarders require a separate loader transaction and are rejected here.
    pub fn guest_override(
        &mut self,
        module: ModuleId,
        name: &str,
    ) -> Result<Option<u32>, LoadError> {
        let (image, hints) = self.modules.export_search(module);
        if image.kind != ModuleKind::Host || !image.lifetime.mapped {
            return Ok(None);
        }
        let Some(original) = image.exports_by_name.get(name) else {
            return Ok(None);
        };
        let published = self.sys.thunks.published_index(&image.name, name);
        if published.is_none()
            && self
                .sys
                .thunks
                .resolver()
                .data_size(&image.name, name)
                .is_some()
        {
            return Ok(None);
        }
        let hint = hints.get(name).copied();
        let live = match mapped_export_inner(
            self.cpu,
            image.base,
            image.size,
            MappedName::Name { name, hint },
        ) {
            Some(resolved) => {
                if let Some(found) = resolved.hint
                    && hint != Some(found)
                {
                    hints.insert(name.to_owned(), found);
                }
                resolved.target
            }
            None => {
                hints.remove(name);
                return Err(LoadError::MissingExport {
                    module: image.name.clone(),
                    name: name.to_owned(),
                });
            }
        };
        let rva = match live {
            ExportTarget::Rva(rva) => rva,
            ExportTarget::Forwarder(_) if &live == original => return Ok(None),
            ExportTarget::Forwarder(_) => {
                return Err(LoadError::Pe {
                    path: image.path.clone(),
                    err: PeError::Malformed(
                        "guest forwarder override inspection is unavailable".into(),
                    ),
                });
            }
        };
        let address = image.base + rva;
        if original != &ExportTarget::Rva(rva) {
            return Ok(Some(address));
        }
        let index = match published {
            Some(index) => index,
            None => {
                let thunk = self.sys.thunks.publish(self.cpu, &image.name, name)?;
                (thunk - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE
            }
        };
        let size = self.sys.thunks.export_entry_bytes(index).len() as u32;
        adv_abi::guest::check_range(self.cpu, address, size, Prot::READ)?;
        Ok((!self
            .sys
            .thunks
            .export_entry_matches(self.cpu, index, address)?)
        .then_some(address))
    }

    fn resolve_transaction(
        &mut self,
        m: ModuleId,
        sym: &ImportName,
        dynamic: bool,
    ) -> Result<(u32, ResolutionChanges), LoadError> {
        let initializers = self.inits.len();
        let result = self.resolve_inner(m, sym, 0, dynamic);
        let changes = std::mem::take(&mut self.changes);
        match result {
            Ok(address) => Ok((address, changes)),
            Err(error) => {
                self.inits.truncate(initializers);
                self.rollback_storage(changes)?;
                Err(error)
            }
        }
    }

    fn resolve_inner(
        &mut self,
        m: ModuleId,
        sym: &ImportName,
        depth: u8,
        dynamic: bool,
    ) -> Result<u32, LoadError> {
        let module = self.modules.get(m);
        let name = sym.to_string();
        if depth >= 32 {
            return Err(LoadError::Pe {
                path: module.path.clone(),
                err: PeError::Malformed("export forwarder cycle or excessive depth".into()),
            });
        }
        if module.kind == ModuleKind::Host {
            let key = module.name.clone();
            let published = match sym {
                ImportName::Name { name, .. } => module.exports_by_name.contains_key(name),
                ImportName::Ordinal(ordinal) => {
                    let resolver = self.sys.thunks.resolver();
                    let known = resolver.host_fn(&key, &name).is_some()
                        || resolver.data_size(&key, &name).is_some();
                    known && module.exports_by_ordinal.contains_key(ordinal)
                }
            };
            if !published
                && adv_abi::system_exports::lookup(&key, &name).is_some_and(|export| {
                    export.kind == adv_abi::system_exports::Kind::DataUnsupported
                })
                && mapped_export(self.cpu, module.base, module.size, sym).is_none()
            {
                return Err(LoadError::MissingExport { module: key, name });
            }
            if !dynamic && !published {
                // Unknown static imports retain fail-before-return diagnostics;
                // they do not fabricate a PE export or a calling convention.
                return Ok(self.sys.thunks.resolve(self.cpu, &key, &name)?);
            }
        }
        // GetProcAddress observes the live guest EAT, including patch edits.
        let target = if module.lifetime.mapped {
            mapped_export(self.cpu, module.base, module.size, sym)
        } else {
            // Circular imports can resolve exports before the importing image's
            // IAT is bound. Once mapped, the live EAT always takes precedence.
            match sym {
                ImportName::Name { name, .. } => module.exports_by_name.get(name),
                ImportName::Ordinal(ordinal) => module.exports_by_ordinal.get(ordinal),
            }
            .cloned()
        };
        match target {
            Some(ExportTarget::Rva(rva)) => {
                let address = module.base + rva;
                let original = match sym {
                    ImportName::Name { name, .. } => module.exports_by_name.get(name),
                    ImportName::Ordinal(ordinal) => module.exports_by_ordinal.get(ordinal),
                };
                let binding = match sym {
                    ImportName::Name { name, .. } => Some(name.clone()),
                    ImportName::Ordinal(_) => module
                        .exports_by_name
                        .iter()
                        .find(|(_, target)| Some(*target) == original)
                        .map(|(name, _)| name.clone()),
                }
                .unwrap_or_else(|| name.clone());
                let data_size = self.sys.thunks.resolver().data_size(&module.name, &binding);
                if let Some(data_size) = data_size
                    && module.kind == ModuleKind::Host
                    && original == Some(&ExportTarget::Rva(rva))
                    && !self
                        .sys
                        .thunks
                        .resolver()
                        .implemented(&module.name, &binding)
                {
                    // Undeclared behavior is a named static/dynamic failure,
                    // never an executable stub masquerading as a pointer cell.
                    let mut live = vec![0; data_size as usize];
                    if self.cpu.read(address, &mut live).is_err()
                        || live.iter().all(|&byte| byte == 0)
                    {
                        return Err(LoadError::MissingExport {
                            module: module.name.clone(),
                            name,
                        });
                    }
                }
                if dynamic && module.kind == ModuleKind::Host {
                    // A changed live EAT is guest-owned behavior. Capability
                    // filtering applies only to the original diagnostic entry.
                    if original == Some(&ExportTarget::Rva(rva)) {
                        let key = module.name.clone();
                        let available = (self.sys.thunks.resolver() as &dyn std::any::Any)
                            .downcast_ref::<adv_abi::Registry>()
                            .map_or_else(
                                || self.sys.thunks.resolver().implemented(&key, &binding),
                                |registry| registry.dynamic_available(&key, &binding),
                            );
                        if !available && data_size.is_none() {
                            let thunk = self.sys.thunks.publish(self.cpu, &key, &binding)?;
                            let index = (thunk - adv_machine::layout::THUNK_BASE)
                                / adv_machine::layout::THUNK_STRIDE;
                            if self
                                .sys
                                .thunks
                                .export_entry_matches(self.cpu, index, address)
                                .unwrap_or(true)
                            {
                                return Err(LoadError::MissingExport { module: key, name });
                            }
                            // Inline hooks, including a changed original thunk,
                            // win over host metadata's capability declaration.
                        }
                    }
                }
                if module.kind == ModuleKind::Host && data_size.is_none() {
                    self.sys.thunks.resolve(self.cpu, &module.name, &name)?;
                }
                Ok(address)
            }
            Some(ExportTarget::Forwarder(f)) => {
                let (dll, func) = f.rsplit_once('.').unwrap_or((&f, ""));
                let dep = self.load_inner(dll, false)?;
                self.dependency(m, dep)?;
                let sym = match func.strip_prefix('#').and_then(|o| o.parse().ok()) {
                    Some(o) => ImportName::Ordinal(o),
                    None => ImportName::Name {
                        hint: 0,
                        name: func.to_owned(),
                    },
                };
                self.resolve_inner(dep, &sym, depth + 1, dynamic)
            }
            None => Err(LoadError::MissingExport {
                module: module.name.clone(),
                name,
            }),
        }
    }

    /// Frontends run guest detach callbacks first, then retire process storage.
    pub fn retire(&mut self, id: ModuleId) -> Result<(), LoadError> {
        let module = self.modules.get(id);
        let (base, tls) = (module.base, module.lifetime.tls_index);
        if module.kind == ModuleKind::DataFile {
            // Raw guest aliases retire before the last shared content owner.
            self.sys.vm.free(self.cpu, base)?;
            self.modules.retire(self.cpu, id)?;
            return Ok(());
        }
        if let Some(index) = tls {
            self.sys.remove_static_tls(self.cpu, index)?;
        }
        self.modules.retire(self.cpu, id)?;
        self.sys.images.remove(base);
        self.sys.vm.free(self.cpu, base)?;
        Ok(())
    }
}

/// Bounded reads from a loaded PE, without copying/reparsing an entire image on
/// every symbol lookup. Forwarder strings are bounded by the export directory.
fn mapped_export(
    cpu: &dyn Guest,
    base: u32,
    size: u32,
    symbol: &ImportName,
) -> Option<ExportTarget> {
    let symbol = match symbol {
        ImportName::Name { name, hint } => MappedName::Name {
            name,
            hint: Some(u32::from(*hint)),
        },
        ImportName::Ordinal(ordinal) => MappedName::Ordinal(*ordinal),
    };
    mapped_export_inner(cpu, base, size, symbol).map(|resolved| resolved.target)
}

enum MappedName<'a> {
    Name { name: &'a str, hint: Option<u32> },
    Ordinal(u16),
}

struct MappedExport {
    target: ExportTarget,
    hint: Option<u32>,
}

// Preserve the bounded live-name search, including changed name pointers and
// malformed unterminated strings. Ordinary ASCII PE names need no allocation.
fn mapped_name_order(
    cpu: &dyn Guest,
    base: u32,
    size: u32,
    rva: u32,
    name: &str,
) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    let limit = size.checked_sub(rva)?.min(65536) as usize;
    let address = base.checked_add(rva)?;
    let mut used = 0;
    let mut order = Ordering::Equal;
    let mut chunk = [0; 64];
    while used < limit {
        let at = address.checked_add(used as u32)?;
        let count = chunk
            .len()
            .min(limit - used)
            .min(4096 - (at as usize & 4095));
        cpu.read(at, &mut chunk[..count]).ok()?;
        let end = chunk[..count].iter().position(|&byte| byte == 0);
        let bytes = &chunk[..end.unwrap_or(count)];
        if !bytes.is_ascii() {
            // Retain the existing UTF-8 parser policy for rare non-ASCII names.
            let bytes = cpu.read_cstr(address, limit).ok()?;
            if bytes.len() == limit {
                return None;
            }
            return Some(name.cmp(std::str::from_utf8(&bytes).ok()?));
        }
        if order == Ordering::Equal {
            let remaining = &name.as_bytes()[used.min(name.len())..];
            let shared = remaining.len().min(bytes.len());
            order = remaining[..shared].cmp(&bytes[..shared]);
            if order == Ordering::Equal && remaining.len() < bytes.len() {
                order = Ordering::Less;
            }
        }
        if let Some(end) = end {
            return Some(if order == Ordering::Equal {
                name.len().cmp(&(used + end))
            } else {
                order
            });
        }
        used += count;
    }
    None
}

fn mapped_export_inner(
    cpu: &dyn Guest,
    base: u32,
    size: u32,
    symbol: MappedName<'_>,
) -> Option<MappedExport> {
    let range =
        |rva: u32, len: u32| (rva.checked_add(len)? <= size).then_some(base.checked_add(rva)?);
    let word = |rva| cpu.read_u32(range(rva, 4)?).ok();
    let short = |rva| cpu.read_u16(range(rva, 2)?).ok();
    let string = |rva: u32, limit: u32| {
        let len = size.checked_sub(rva)?.min(limit).min(65536);
        let bytes = cpu.read_cstr(range(rva, 1)?, len as usize).ok()?;
        if bytes.len() == len as usize {
            return None;
        }
        String::from_utf8(bytes).ok()
    };
    if short(0)? != 0x5a4d {
        return None;
    }
    let pe = word(0x3c)?;
    if word(pe)? != 0x4550 || short(pe.checked_add(24)?)? != 0x10b {
        return None;
    }
    let dir = word(pe.checked_add(120)?)?;
    let dir_size = word(pe.checked_add(124)?)?;
    if dir == 0 || dir_size < 40 {
        return None;
    }
    range(dir, dir_size)?;
    let ordinal_base = word(dir.checked_add(16)?)?;
    let count = word(dir.checked_add(20)?)?;
    let named = word(dir.checked_add(24)?)?;
    let functions = word(dir.checked_add(28)?)?;
    let names = word(dir.checked_add(32)?)?;
    let ordinals = word(dir.checked_add(36)?)?;
    range(functions, count.checked_mul(4)?)?;
    range(names, named.checked_mul(4)?)?;
    range(ordinals, named.checked_mul(2)?)?;
    let (index, hint) = match symbol {
        MappedName::Ordinal(ordinal) => (u32::from(ordinal).checked_sub(ordinal_base)?, None),
        MappedName::Name { name, hint } => {
            let (mut low, mut high) = (0, named);
            // Windows import hints locate a name, never a function RVA. All
            // current headers and table ranges have already been validated.
            let mut found = hint.filter(|&hint| {
                hint < named
                    && word(names + hint * 4).is_some_and(|rva| {
                        mapped_name_order(cpu, base, size, rva, name)
                            == Some(std::cmp::Ordering::Equal)
                    })
            });
            while found.is_none() && low < high {
                let middle = low + (high - low) / 2;
                match mapped_name_order(cpu, base, size, word(names + middle * 4)?, name)? {
                    std::cmp::Ordering::Less => high = middle,
                    std::cmp::Ordering::Greater => low = middle + 1,
                    std::cmp::Ordering::Equal => {
                        found = Some(middle);
                        break;
                    }
                }
            }
            let found = found?;
            (u32::from(short(ordinals + found * 2)?), Some(found))
        }
    };
    if index >= count {
        return None;
    }
    let rva = word(functions + index * 4)?;
    if rva == 0 {
        return None;
    }
    range(rva, 1)?;
    let target = if rva >= dir && rva < dir + dir_size {
        ExportTarget::Forwarder(string(rva, dir + dir_size - rva)?)
    } else {
        ExportTarget::Rva(rva)
    };
    Some(MappedExport { target, hint })
}

/// A host directory seen by the guest as `guest_root` (e.g. `C:\Game`). Guest paths are
/// matched case-insensitively, `\` and `/` alike; relative paths are relative to the root.
pub struct HostDir {
    pub root: std::path::PathBuf,
    pub guest_root: String,
}

impl HostDir {
    pub fn new(root: impl Into<std::path::PathBuf>, guest_root: &str) -> Self {
        Self {
            root: root.into(),
            guest_root: guest_root.trim_end_matches(['\\', '/']).to_owned(),
        }
    }

    /// Host path for a guest path, if it is under the root and exists.
    pub fn host_path(&self, guest: &str) -> Option<std::path::PathBuf> {
        let g = guest.replace('/', "\\");
        let prefix = format!("{}\\", self.guest_root.replace('/', "\\"));
        let rel = if g
            .get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(&prefix))
        {
            &g[prefix.len()..]
        } else if g.contains(':') || g.starts_with('\\') {
            return None;
        } else {
            g.trim_start_matches(".\\")
        };
        let mut cur = self.root.clone();
        for comp in rel.split('\\').filter(|c| !c.is_empty() && *c != ".") {
            let exact = cur.join(comp);
            cur = if exact.exists() {
                exact
            } else {
                std::fs::read_dir(&cur)
                    .ok()?
                    .flatten()
                    .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(comp))?
                    .path()
            };
        }
        Some(cur)
    }
}

impl FileSource for HostDir {
    fn absolute_path(&self, path: &str) -> String {
        if path.contains(':') {
            path.replace('/', "\\")
        } else {
            format!("{}\\{path}", self.guest_root)
        }
    }
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let p = self.host_path(path)?;
        p.is_file().then(|| std::fs::read(p).ok()).flatten()
    }
}

/// Build the immutable system file and mapped module from the same catalog.
pub(crate) fn system_image(
    cpu: &mut dyn Guest,
    sys: &mut Sys,
    key: &str,
    base: u32,
) -> Result<crate::synthetic::Image, LoadError> {
    let registry =
        (sys.thunks.resolver() as &dyn std::any::Any).downcast_ref::<adv_abi::Registry>();
    let names = registry.map(|r| r.exports(key)).unwrap_or_default();
    let mut known_ordinals: std::collections::BTreeMap<String, u16> =
        if adv_abi::system_exports::has_module(key) {
            adv_abi::system_exports::exports(key)
                .filter_map(|export| {
                    export
                        .ordinal
                        .map(|ordinal| (export.name.to_owned(), ordinal))
                })
                .collect()
        } else {
            adv_abi::ordinals::exports(key)
                .map(|(ordinal, name)| (name.to_owned(), ordinal))
                .collect()
        };
    let explicit_ordinals: std::collections::BTreeSet<u16> = names
        .iter()
        .filter_map(|name| name.strip_prefix('#')?.parse().ok())
        .collect();
    known_ordinals.retain(|_, ordinal| !explicit_ordinals.contains(ordinal));
    let mut exports = Vec::with_capacity(names.len());
    for name in names {
        let catalog = adv_abi::system_exports::lookup(key, &name);
        let target = if let Some(adv_abi::system_exports::Export {
            kind: adv_abi::system_exports::Kind::Forwarder(target),
            ..
        }) = catalog
        {
            crate::synthetic::Target::Forwarder((*target).to_owned())
        } else if let Some(size) = sys.thunks.resolver().data_size(key, &name) {
            let bytes = sys
                .thunks
                .resolver()
                .host_data(key, &name)
                .map(<[u8]>::to_vec)
                .unwrap_or_else(|| vec![0; size as usize]);
            if bytes.len() != size as usize {
                return Err(LoadError::Pe {
                    path: key.to_owned(),
                    err: PeError::Malformed("data descriptor size mismatch".into()),
                });
            }
            crate::synthetic::Target::Data {
                binding: sys
                    .thunks
                    .resolver()
                    .canonical_name(key, &name)
                    .unwrap_or_else(|| name.clone()),
                bytes,
            }
        } else if catalog
            .is_some_and(|export| export.kind == adv_abi::system_exports::Kind::DataUnsupported)
        {
            crate::synthetic::Target::Absent
        } else {
            let thunk = sys.thunks.publish(cpu, key, &name)?;
            let index =
                (thunk - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
            let bytes = sys.thunks.export_entry_bytes(index).to_vec();
            crate::synthetic::Target::Code(bytes)
        };
        let ordinal = name.strip_prefix('#').and_then(|n| n.parse().ok());
        exports.push(crate::synthetic::Export {
            ordinal: ordinal.or_else(|| known_ordinals.get(&name).copied()),
            name: ordinal.is_none().then_some(name),
            target,
        });
    }
    let mut private = Vec::new();
    if key == "ntdll.dll" {
        private.push((
            "return_pad".to_owned(),
            sys.thunks.pristine_entry(adv_machine::thunk::RETURN_PAD),
        ));
        if sys
            .thunks
            .resolver()
            .host_fn("<win32>", "thread_exception_handler")
            .is_some()
        {
            let entry = sys
                .thunks
                .publish(cpu, "<win32>", "thread_exception_handler")?;
            let index =
                (entry - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
            private.push((
                "thread_exception_handler".to_owned(),
                sys.thunks.pristine_entry(index),
            ));
        }
    }
    crate::synthetic::build_with_private_entries(base, key, &exports, &private).map_err(|err| {
        LoadError::Pe {
            path: key.to_owned(),
            err,
        }
    })
}

fn mapped_string(cpu: &dyn Guest, at: u32, limit: u32) -> Result<String, LoadError> {
    let mut bytes = Vec::new();
    for offset in 0..limit {
        let address = at.checked_add(offset).ok_or(MemFault {
            addr: at,
            len: limit,
        })?;
        if !cpu.accessible(address, 1, Prot::READ) {
            return Err(MemFault {
                addr: address,
                len: 1,
            }
            .into());
        }
        let byte = cpu.read_u8(address)?;
        if byte == 0 {
            return String::from_utf8(bytes).map_err(|_| LoadError::Pe {
                path: "mapped import".into(),
                err: PeError::Malformed("non-UTF8 import name".into()),
            });
        }
        bytes.push(byte);
    }
    Err(LoadError::Pe {
        path: "mapped import".into(),
        err: PeError::Malformed("unterminated import name".into()),
    })
}

#[cfg(test)]
mod host_dir_tests {
    use super::HostDir;
    #[test]
    fn multibyte_relative_names_do_not_slice_inside_utf8() {
        let root = std::env::temp_dir().join(format!("adv-sync-dir-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let name = "時計のゲーム.exe";
        std::fs::write(root.join(name), b"test").unwrap();
        let files = HostDir::new(&root, "C:\\Game");
        assert_eq!(files.host_path(name), Some(root.join(name)));
        assert_eq!(
            files.host_path(&format!("C:\\Game\\{name}")),
            Some(root.join(name))
        );
        assert_eq!(files.host_path("C:\\時計\\other.exe"), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
