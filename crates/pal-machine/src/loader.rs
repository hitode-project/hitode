//! PE image loader.
//!
//! * Images load at their preferred base when it is free (TL2026's exe has no relocations).
//!   Otherwise they are rebased with their fixups; images without a relocation directory
//!   and without code (RESOURCE.dll / FX.dll share base 0x10000000) are moved as-is.
//! * Imports and delay imports are bound eagerly. A module the host implements (kernel32,
//!   or a registered codec/engine module) resolves to thunks; any other module is loaded from the game files as a
//!   guest DLL. Delay-load slots and the delay descriptor's HMODULE slot are prefilled, so
//!   the guest's `__delayLoadHelper2` never runs.
//! * Static TLS templates are recorded; each guest context gets its blocks at spawn.
//! * Initializers (TLS callbacks, DllMain) are returned as [`InitCall`]s, which the process
//!   start thunk runs as a chain of guest calls on the main context.

use pal_cpu::{Guest, MemFault, Prot};
use pal_pe::{ExportTarget, ImportName, Pe, PeError, file_flags, scn};
use thiserror::Error;

use crate::Sys;
use crate::layout::{GRANULE, HOST_MODULE_BASE, HOST_MODULE_CAP, PAGE};
use crate::module::{Module, ModuleId, ModuleKind, module_key};
use crate::vm::align_up;

/// System modules always implemented by the host, even if a file with that name exists
/// in the game directory. Engine modules are selected by the host registry instead.
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
];

const DLL_PROCESS_ATTACH: u32 = 1;

/// Guest-visible file access for the loader. `path` is exactly what the guest asked for
/// (an import descriptor name, possibly relative, or a LoadLibrary argument).
pub trait FileSource {
    fn read(&self, path: &str) -> Option<Vec<u8>>;
}

/// Resolve only module files. Ordinary guest file operations retain their exact
/// paths. Share this order with capability preflight so it inspects the same DLL
/// the loader will use: named file, bare name next to its importer, then dll/.
pub fn module_file(files: &dyn FileSource, path: &str, importer_dir: Option<&str>) -> Option<(String, Vec<u8>)> {
    if let Some(bytes) = files.read(path) {
        return Some((path.to_owned(), bytes));
    }
    if !path.contains(['\\', '/']) {
        if let Some(dir) = importer_dir.filter(|d| !d.is_empty()) {
            let near = format!("{dir}\\{path}");
            if let Some(bytes) = files.read(&near) {
                return Some((near, bytes));
            }
        }
        let bundled = format!("dll\\{path}");
        if let Some(bytes) = files.read(&bundled) {
            return Some((bundled, bytes));
        }
    }
    None
}

#[derive(Debug, Error)]
pub enum LoadError {
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

/// A guest function to call during process (or LoadLibrary) initialization.
#[derive(Clone, Debug)]
pub struct InitCall {
    pub target: u32,
    pub args: Vec<u32>,
    /// For logs: "tls callback of X" / "DllMain of X" / "entry of X".
    pub what: String,
}

/// Static TLS of one image (`__declspec(thread)` data).
#[derive(Clone, Debug)]
pub struct TlsImage {
    pub index: u32,
    pub template: Vec<u8>,
    pub zero_fill: u32,
}

pub fn is_host_module(sys: &Sys, key: &str) -> bool {
    HOST_MODULES.contains(&key) || key.starts_with("api-ms-win-") || sys.thunks.registry.has_module(key)
}

pub struct Loader<'a> {
    pub cpu: &'a mut dyn Guest,
    pub sys: &'a mut Sys,
    pub files: &'a dyn FileSource,
    /// Initializers of newly loaded images, dependencies first.
    pub inits: Vec<InitCall>,
    /// Guest directories of the images whose imports are being bound (innermost last).
    importers: Vec<String>,
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
    pub fn new(cpu: &'a mut dyn Guest, sys: &'a mut Sys, files: &'a dyn FileSource) -> Self {
        Self { cpu, sys, files, inits: Vec::new(), importers: Vec::new() }
    }

    /// Load `path` (or bump the load count if it is already loaded).
    pub fn load(&mut self, path: &str) -> Result<ModuleId, LoadError> {
        let key = module_key(path);
        if let Some(id) = self.sys.modules.by_name(&key) {
            self.sys.modules.get_mut(id).load_count += 1;
            return Ok(id);
        }
        if is_host_module(self.sys, &key) {
            return self.load_host(key, path);
        }
        if let Some((resolved, bytes)) = module_file(self.files, path, self.importers.last().map(String::as_str)) {
            if resolved != path {
                log::info!("{path}: found as {resolved}");
            }
            return self.load_image(key, &resolved, bytes);
        }
        // Anything the game doesn't ship is a system DLL: the host provides it, and calls
        // into unimplemented functions fail by name.
        log::info!("{path}: not in the game files, treating as a system module");
        self.load_host(key, path)
    }

    fn load_host(&mut self, key: String, path: &str) -> Result<ModuleId, LoadError> {
        let n = self.sys.modules.host_count();
        if n >= HOST_MODULE_CAP {
            return Err(LoadError::TooManyHostModules);
        }
        Ok(self.sys.modules.add(Module {
            name: key,
            path: path.to_owned(),
            kind: ModuleKind::Host,
            base: HOST_MODULE_BASE + n * GRANULE,
            size: 0,
            is_dll: true,
            entry: 0,
            stack_reserve: 0,
            exports_by_name: Default::default(),
            exports_by_ordinal: Default::default(),
            resources: None,
            load_count: 1,
        }))
    }

    fn load_image(&mut self, key: String, path: &str, bytes: Vec<u8>) -> Result<ModuleId, LoadError> {
        let pe_err = |err| LoadError::Pe { path: path.to_owned(), err };
        let mut pe = Pe::parse(&bytes).map_err(pe_err)?;
        drop(bytes);

        let size = align_up(pe.size_of_image, PAGE);
        let relocs = pe.relocations().map_err(pe_err)?;
        let base = if self.sys.vm.reserve_at(pe.image_base, size) {
            pe.image_base
        } else {
            let has_code = pe.sections.iter().any(|s| s.is_executable());
            let stripped = pe.characteristics & file_flags::RELOCS_STRIPPED != 0;
            if relocs.is_empty() && (has_code || stripped) {
                return Err(LoadError::NoRelocs(path.to_owned(), pe.image_base));
            }
            self.sys.vm.reserve(size).ok_or_else(|| LoadError::NoSpace(path.to_owned(), size))?
        };
        if base != pe.image_base {
            let n = pe.relocate(base.wrapping_sub(pe.image_base)).map_err(pe_err)?;
            log::debug!("{key}: rebased {:#x} -> {base:#x} ({n} fixups)", pe.image_base);
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
        let id = self.sys.modules.add(Module {
            name: key.clone(),
            path: path.to_owned(),
            kind: ModuleKind::Image,
            base,
            size,
            is_dll: pe.is_dll(),
            entry: if pe.entry_rva != 0 { base + pe.entry_rva } else { 0 },
            stack_reserve: pe.stack_reserve,
            exports_by_name,
            exports_by_ordinal,
            resources: pe.resources().map_err(pe_err)?,
            load_count: 1,
        });

        let mut tls_callbacks = Vec::new();
        if let Some(tls) = pe.tls().map_err(pe_err)? {
            // The directory's VAs were fixed up with the rest of the image.
            let index = self.sys.tls_images.len() as u32;
            let len = tls.raw_end_va.saturating_sub(tls.raw_start_va);
            let template = pe.slice(tls.raw_start_va.wrapping_sub(base), len).map_err(pe_err)?.to_vec();
            if tls.index_va != 0 {
                pe.set_u32(tls.index_va.wrapping_sub(base), index).map_err(pe_err)?;
            }
            if !self.sys.contexts.is_empty() {
                log::warn!("{key}: static TLS in a DLL loaded after threads started; existing threads lack it");
            }
            self.sys.tls_images.push(TlsImage { index, template, zero_fill: tls.zero_fill });
            tls_callbacks = pe.tls_callbacks(&tls, base).map_err(pe_err)?;
        }

        // Detours' setdll prepends DLLs to the import table so Windows loads them first
        // (never part of the game as shipped) and keeps the original table in its header. Bind the original: the game runs as it shipped, and the added DLLs never load.
        if let Some(orig) = pe.detours_original_imports().map_err(pe_err)? {
            let now: Vec<String> = pe.imports().map_err(pe_err)?.into_iter().map(|m| m.dll).collect();
            pe.dirs[pal_pe::dir::IMPORT] = orig;
            let kept: Vec<String> = pe.imports().map_err(pe_err)?.into_iter().map(|m| m.dll).collect();
            let added: Vec<&String> = now.iter().filter(|d| !kept.contains(d)).collect();
            log::info!("{key}: rewritten by Detours setdll; binding the original imports, not loading {added:?}");
        }

        let dir = path.rfind(['\\', '/']).map_or(String::new(), |i| path[..i].to_owned());
        self.importers.push(dir);
        let bound = self.bind(&mut pe, path, base);
        self.importers.pop();
        bound?;

        // Map, copy, protect.
        self.cpu.map(base, size, Prot::RW)?;
        self.cpu.write(base, &pe.image)?;
        if pe.section_alignment >= PAGE {
            self.cpu.protect(base, align_up(pe.size_of_headers.max(1), PAGE), Prot::READ)?;
            for s in &pe.sections {
                let len = align_up(s.mem_size(), PAGE);
                if len != 0 {
                    self.cpu.protect(base + s.virtual_address, len, prot_of(s.characteristics))?;
                }
            }
        }
        log::info!("loaded {key} at {base:#010x} (+{size:#x})");

        for cb in tls_callbacks {
            self.inits.push(InitCall {
                target: cb,
                args: vec![base, DLL_PROCESS_ATTACH, 0],
                what: format!("TLS callback of {key}"),
            });
        }
        let m = self.sys.modules.get(id);
        if m.is_dll && m.entry != 0 {
            self.inits.push(InitCall {
                target: m.entry,
                args: vec![base, DLL_PROCESS_ATTACH, 0],
                what: format!("DllMain of {key}"),
            });
        }
        Ok(id)
    }

    /// Fill the IAT and delay-load IAT (and the delay descriptor's HMODULE slot).
    fn bind(&mut self, pe: &mut Pe, path: &str, base: u32) -> Result<(), LoadError> {
        let pe_err = |err| LoadError::Pe { path: path.to_owned(), err };
        for m in pe.imports().map_err(pe_err)? {
            let dep = self.load(&m.dll)?;
            for imp in &m.imports {
                let addr = self.resolve(dep, &imp.name)?;
                pe.set_u32(imp.iat_rva, addr).map_err(pe_err)?;
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
            let dep = self.load(&m.dll)?;
            if m.module_handle_rva != 0 {
                pe.set_u32(m.module_handle_rva, self.sys.modules.get(dep).base).map_err(pe_err)?;
            }
            for imp in &m.imports {
                let addr = self.resolve(dep, &imp.name)?;
                pe.set_u32(imp.iat_rva, addr).map_err(pe_err)?;
            }
        }
        Ok(())
    }

    /// Address of `sym` in module `m` (a thunk for host modules). Follows forwarders.
    pub fn resolve(&mut self, m: ModuleId, sym: &ImportName) -> Result<u32, LoadError> {
        let module = self.sys.modules.get(m);
        let name = sym.to_string();
        if module.kind == ModuleKind::Host {
            let key = module.name.clone();
            return Ok(self.sys.thunks.resolve(self.cpu, &key, &name)?);
        }
        let target = match sym {
            ImportName::Name { name, .. } => module.exports_by_name.get(name),
            ImportName::Ordinal(o) => module.exports_by_ordinal.get(o),
        };
        match target.cloned() {
            Some(ExportTarget::Rva(rva)) => Ok(module.base + rva),
            Some(ExportTarget::Forwarder(f)) => {
                let (dll, func) = f.split_once('.').unwrap_or((&f, ""));
                let dep = self.load(dll)?;
                let sym = match func.strip_prefix('#').and_then(|o| o.parse().ok()) {
                    Some(o) => ImportName::Ordinal(o),
                    None => ImportName::Name { hint: 0, name: func.to_owned() },
                };
                self.resolve(dep, &sym)
            }
            None => Err(LoadError::MissingExport { module: module.name.clone(), name }),
        }
    }
}

/// A host directory seen by the guest as `guest_root` (e.g. `C:\Game`). Guest paths are
/// matched case-insensitively, `\` and `/` alike; relative paths are relative to the root.
pub struct HostDir {
    pub root: std::path::PathBuf,
    pub guest_root: String,
}

impl HostDir {
    pub fn new(root: impl Into<std::path::PathBuf>, guest_root: &str) -> Self {
        Self { root: root.into(), guest_root: guest_root.trim_end_matches(['\\', '/']).to_owned() }
    }

    /// Host path for a guest path, if it is under the root and exists.
    pub fn host_path(&self, guest: &str) -> Option<std::path::PathBuf> {
        let g = guest.replace('/', "\\");
        let prefix = format!("{}\\", self.guest_root.replace('/', "\\"));
        let rel = if g.get(..prefix.len()).is_some_and(|p| p.eq_ignore_ascii_case(&prefix)) {
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
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let p = self.host_path(path)?;
        p.is_file().then(|| std::fs::read(p).ok()).flatten()
    }
}

#[cfg(test)]
mod host_dir_tests {
    use super::HostDir;
    #[test]
    fn multibyte_relative_names_do_not_slice_inside_utf8() {
        let root = std::env::temp_dir().join(format!("pal-host-dir-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let name = "時計のゲーム.exe";
        std::fs::write(root.join(name), b"test").unwrap();
        let files = HostDir::new(&root, "C:\\Game");
        assert_eq!(files.host_path(name), Some(root.join(name)));
        assert_eq!(files.host_path(&format!("C:\\Game\\{name}")), Some(root.join(name)));
        assert_eq!(files.host_path("C:\\時計\\other.exe"), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
