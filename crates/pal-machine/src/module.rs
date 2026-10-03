//! Loaded-module table: guest images (exe, guest DLLs, resource-only DLLs) and host modules
//! (kernel32, pal.dll, ...) that exist only as a fake HMODULE plus thunks.

use std::collections::HashMap;

use pal_pe::{ExportTarget, ResourceDir};

pub type ModuleId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleKind {
    /// A PE image mapped into guest memory.
    Image,
    /// Implemented by the host; `base` is a fake, unmapped HMODULE.
    Host,
}

#[derive(Debug)]
pub struct Module {
    /// Lowercase file name, the key for lookups ("kernel32.dll", "leyline.exe").
    pub name: String,
    /// The path it was requested by (import descriptor or LoadLibrary argument).
    pub path: String,
    pub kind: ModuleKind,
    pub base: u32,
    pub size: u32,
    pub is_dll: bool,
    /// Entry point VA, 0 if none.
    pub entry: u32,
    pub stack_reserve: u32,
    pub exports_by_name: HashMap<String, ExportTarget>,
    pub exports_by_ordinal: HashMap<u16, ExportTarget>,
    pub resources: Option<ResourceDir>,
    pub load_count: u32,
}

impl Module {
    pub fn contains(&self, addr: u32) -> bool {
        self.kind == ModuleKind::Image && addr.wrapping_sub(self.base) < self.size
    }
}

/// Normalized module key: file name only, lowercase, `.dll` appended when there is no
/// extension (LoadLibrary semantics).
pub fn module_key(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path).trim();
    let mut k = file.to_ascii_lowercase();
    if !k.contains('.') {
        k.push_str(".dll");
    } else if k.ends_with('.') {
        k.pop();
    }
    k
}

#[derive(Default)]
pub struct Modules {
    list: Vec<Module>,
    by_name: HashMap<String, ModuleId>,
}

impl Modules {
    pub fn add(&mut self, m: Module) -> ModuleId {
        let id = self.list.len();
        self.by_name.insert(m.name.clone(), id);
        self.list.push(m);
        id
    }

    pub fn get(&self, id: ModuleId) -> &Module {
        &self.list[id]
    }

    pub fn get_mut(&mut self, id: ModuleId) -> &mut Module {
        &mut self.list[id]
    }

    pub fn by_name(&self, path: &str) -> Option<ModuleId> {
        self.by_name.get(&module_key(path)).copied()
    }

    /// Module whose HMODULE is `handle`.
    pub fn by_handle(&self, handle: u32) -> Option<ModuleId> {
        self.list.iter().position(|m| m.base == handle)
    }

    /// Image module containing guest address `addr`.
    pub fn containing(&self, addr: u32) -> Option<ModuleId> {
        self.list.iter().position(|m| m.contains(addr))
    }

    pub fn iter(&self) -> impl Iterator<Item = (ModuleId, &Module)> {
        self.list.iter().enumerate()
    }

    pub fn host_count(&self) -> u32 {
        self.list
            .iter()
            .filter(|m| m.kind == ModuleKind::Host)
            .count() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::module_key;

    #[test]
    fn keys() {
        assert_eq!(module_key("KERNEL32.dll"), "kernel32.dll");
        assert_eq!(module_key("C:\\Game\\dll\\RESOURCE.dll"), "resource.dll");
        assert_eq!(module_key("Game/Plugin/Hook.dll"), "hook.dll");
        assert_eq!(module_key("user32"), "user32.dll");
        assert_eq!(module_key("foo."), "foo");
    }
}
