//! Loaded-module table: guest images (exe, guest DLLs, resource-only DLLs) and host modules
//! (kernel32, ...) with real, guest-readable synthetic PE images.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::{cell::RefCell, rc::Rc};

use adv_pe::{ExportTarget, ResourceDir};
mod datafile;
pub use datafile::DataFile;

pub type ModuleId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleKind {
    /// A PE image mapped into guest memory.
    Image,
    /// Implemented by the host; `base` addresses a synthetic PE image.
    Host,
    /// A raw readonly file view; no imports, TLS, initializer or LDR record.
    DataFile,
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
    pub data_file: Option<DataFile>,
    pub load_count: u32,
    /// Stable x86 LDR_DATA_TABLE_ENTRY in guest memory, zero until published.
    pub ldr_entry: u32,
    pub ldr_size: u32,
    /// Loader lifetime is separate from the numeric image base, which may be reused.
    pub lifetime: Lifetime,
}

#[derive(Debug)]
pub struct Lifetime {
    pub mapped: bool,
    pub pinned: bool,
    pub external_refs: u32,
    pub dependencies: BTreeSet<ModuleId>,
    pub callbacks: Vec<u32>,
    pub tls_index: Option<u32>,
    pub attach_started: bool,
    pub unloading: bool,
    pub thread_calls: bool,
}

impl Default for Lifetime {
    fn default() -> Self {
        Self {
            mapped: false,
            pinned: false,
            external_refs: 1,
            dependencies: BTreeSet::new(),
            callbacks: Vec::new(),
            tls_index: None,
            attach_started: false,
            unloading: false,
            thread_calls: true,
        }
    }
}

impl Module {
    pub fn handle(&self) -> u32 {
        self.base | u32::from(self.kind == ModuleKind::DataFile)
    }
    pub fn executable_visible(&self) -> bool {
        self.kind != ModuleKind::DataFile
    }
    pub fn contains(&self, addr: u32) -> bool {
        addr.wrapping_sub(self.base) < self.size
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
    list: BTreeMap<ModuleId, Module>,
    next_id: ModuleId,
    by_name: HashMap<String, ModuleId>,
    // Only name-table indexes; every lookup validates current guest PE bytes.
    // Module ids are never reused, and retiring an owner removes its hints.
    export_hints: HashMap<ModuleId, HashMap<String, u32>>,
    /// PAGE values belong to the live image owner, never to a reusable base.
    image_protections: BTreeMap<ModuleId, Vec<u32>>,
    ldr: u32,
    ldr_used: u32,
    ldr_free: Vec<(u32, u32)>,
    system_layout: Rc<RefCell<BTreeMap<String, (u32, u32)>>>,
}

impl Modules {
    /// A process family shares system-image addresses, never image bytes or
    /// module references. Install this before loading any child system image.
    pub fn inherit_system_image_layout(&mut self, source: &Self) -> Result<(), &'static str> {
        if self.host_count() != 0 {
            return Err("system image layout must be inherited before mapping host modules");
        }
        self.system_layout = source.system_layout.clone();
        Ok(())
    }

    pub(crate) fn system_image_base(&self, name: &str) -> u32 {
        let layout = self.system_layout.borrow();
        layout.get(name).map_or_else(
            || {
                layout
                    .values()
                    .map(|&(base, span)| base + span)
                    .max()
                    .unwrap_or(adv_machine::layout::HOST_MODULE_BASE)
            },
            |&(base, _)| base,
        )
    }

    pub(crate) fn reserve_system_image(&mut self, name: &str, base: u32, size: u32) -> bool {
        use adv_machine::layout::{GRANULE, HOST_MODULE_BASE, HOST_MODULE_CAP};
        let Some(span) = size
            .checked_add(GRANULE - 1)
            .map(|end| end & !(GRANULE - 1))
        else {
            return false;
        };
        let mut layout = self.system_layout.borrow_mut();
        if let Some(&(resident, capacity)) = layout.get(name) {
            return resident == base && span <= capacity;
        }
        if base
            .checked_add(span)
            .is_none_or(|end| end > HOST_MODULE_BASE + HOST_MODULE_CAP * GRANULE)
        {
            return false;
        }
        layout.insert(name.into(), (base, span));
        true
    }

    fn alloc_ldr(
        &mut self,
        cpu: &mut dyn adv_cpu::backend::Guest,
        size: u32,
    ) -> Result<u32, adv_cpu::backend::MemFault> {
        use adv_machine::layout::{LDR_BASE, LDR_SIZE};
        if let Some(index) = self.ldr_free.iter().position(|(_, free)| *free >= size) {
            let (address, free) = self.ldr_free[index];
            cpu.map(address, size, adv_cpu::backend::Prot::RW)?;
            self.ldr_free.swap_remove(index);
            if free > size {
                self.ldr_free.push((address + size, free - size));
            }
            return Ok(address);
        }
        let address = LDR_BASE + self.ldr_used;
        if size > LDR_SIZE.saturating_sub(self.ldr_used) {
            return Err(adv_cpu::backend::MemFault {
                addr: address,
                len: size,
            });
        }
        cpu.map(address, size, adv_cpu::backend::Prot::RW)?;
        self.ldr_used += size;
        Ok(address)
    }
    pub fn add(&mut self, m: Module) -> ModuleId {
        let id = self.next_id;
        self.next_id += 1;
        if m.executable_visible() {
            self.by_name.entry(m.name.clone()).or_insert(id);
        }
        self.list.insert(id, m);
        id
    }

    pub fn get(&self, id: ModuleId) -> &Module {
        &self.list[&id]
    }

    pub fn try_get(&self, id: ModuleId) -> Option<&Module> {
        self.list.get(&id)
    }

    pub(crate) fn export_search(&mut self, id: ModuleId) -> (&Module, &mut HashMap<String, u32>) {
        (&self.list[&id], self.export_hints.entry(id).or_default())
    }

    /// Dependency-first initialization order; callers reverse it for detach.
    pub fn initialization_order(&self) -> Vec<ModuleId> {
        let mut seen = BTreeSet::new();
        let mut result = Vec::new();
        for (id, _) in self.iter() {
            let mut pending = vec![(id, false)];
            while let Some((id, visited)) = pending.pop() {
                if visited {
                    result.push(id);
                } else if self.try_get(id).is_some() && seen.insert(id) {
                    pending.push((id, true));
                    pending.extend(
                        self.get(id)
                            .lifetime
                            .dependencies
                            .iter()
                            .rev()
                            .map(|&dependency| (dependency, false)),
                    );
                }
            }
        }
        result
    }

    pub fn get_mut(&mut self, id: ModuleId) -> &mut Module {
        self.list.get_mut(&id).expect("live module id")
    }

    /// Publish section PAGE values only after the complete image is mapped.
    pub fn set_image_protections(&mut self, id: ModuleId, protections: Vec<u32>) {
        let module = self.get(id);
        assert!(module.executable_visible());
        assert_eq!(protections.len(), module.size.div_ceil(4096) as usize);
        self.image_protections.insert(id, protections);
    }

    /// Capture loader-applied page permissions once; future guest PE-header
    /// writes do not replace the mapped image's allocation/section contract.
    pub fn capture_image_protections(&mut self, cpu: &dyn adv_cpu::backend::Guest, id: ModuleId) {
        use adv_cpu::backend::Prot;
        let module = self.get(id);
        let pages = (0..module.size.div_ceil(4096))
            .map(|index| {
                let page = module.base + index * 4096;
                match (
                    cpu.accessible(page, 4096, Prot::READ),
                    cpu.accessible(page, 4096, Prot::WRITE),
                    cpu.accessible(page, 4096, Prot::EXEC),
                ) {
                    (_, true, true) => 0x40,
                    (true, false, true) => 0x20,
                    (false, false, true) => 0x10,
                    (_, true, false) => 4,
                    (true, false, false) => 2,
                    _ => 1,
                }
            })
            .collect();
        self.set_image_protections(id, pages);
    }

    /// Writable image pages retain COPY until the execution backend's first
    /// write. Setting COPY on an already private page never shares it again.
    pub fn image_protection(&self, cpu: &dyn adv_cpu::backend::Guest, address: u32) -> Option<u32> {
        let id = self.containing(address)?;
        let index = ((address - self.get(id).base) / 4096) as usize;
        let value = *self.image_protections.get(&id)?.get(index)?;
        Some(match (value, cpu.file_page_copy_on_write(address)) {
            (4 | 8, Some(true)) => 8,
            (0x40 | 0x80, Some(true)) => 0x80,
            (8, Some(false)) => 4,
            (0x80, Some(false)) => 0x40,
            _ => value,
        })
    }

    /// The caller applies CPU protection transactionally before publishing it.
    pub fn protect_image_pages(&mut self, address: u32, length: u32, protection: u32) {
        let Some(id) = self.containing(address) else {
            return;
        };
        let module = self.get(id);
        let first = ((address - module.base) / 4096) as usize;
        let count = (length / 4096) as usize;
        if let Some(pages) = self.image_protections.get_mut(&id)
            && let Some(changed) = pages.get_mut(first..first + count)
        {
            changed.fill(protection);
        }
    }

    pub fn by_name(&self, path: &str) -> Option<ModuleId> {
        if crate::loader::is_system_path(path) {
            return self.system(path);
        }
        if path.contains(['\\', '/']) {
            let path = path.replace('/', "\\");
            return self
                .iter()
                .find(|(_, m)| m.executable_visible() && m.path.eq_ignore_ascii_case(&path))
                .map(|(id, _)| id);
        }
        self.by_name.get(&module_key(path)).copied()
    }

    pub fn system(&self, name: &str) -> Option<ModuleId> {
        let key = module_key(name);
        self.iter()
            .find(|(_, m)| m.kind == ModuleKind::Host && m.name == key)
            .map(|(id, _)| id)
    }

    /// Module whose HMODULE is `handle`.
    pub fn by_handle(&self, handle: u32) -> Option<ModuleId> {
        self.iter()
            .find(|(_, m)| m.handle() == handle)
            .map(|(id, _)| id)
    }

    /// Image module containing guest address `addr`.
    pub fn containing(&self, addr: u32) -> Option<ModuleId> {
        self.iter()
            .find(|(_, m)| m.executable_visible() && m.contains(addr))
            .map(|(id, _)| id)
    }

    /// Raw allocation ownership for memory APIs, never executable lookup/LDR.
    pub fn data_file_containing(&self, addr: u32) -> Option<&Module> {
        self.iter()
            .map(|(_, m)| m)
            .find(|m| m.kind == ModuleKind::DataFile && m.contains(addr))
    }

    pub fn iter(&self) -> impl Iterator<Item = (ModuleId, &Module)> {
        self.list.iter().map(|(&id, module)| (id, module))
    }

    pub fn retain(
        &mut self,
        cpu: &mut dyn adv_cpu::backend::Guest,
        id: ModuleId,
    ) -> Result<(), adv_cpu::backend::MemFault> {
        self.get_mut(id).lifetime.external_refs =
            self.get(id).lifetime.external_refs.saturating_add(1);
        self.sync_counts(cpu)
    }

    /// Imports own one graph edge, irrespective of the number of exported symbols
    /// resolved through that dependency. Reachability also handles import cycles.
    pub(crate) fn dependency(
        &mut self,
        cpu: &mut dyn adv_cpu::backend::Guest,
        owner: ModuleId,
        dependency: ModuleId,
    ) -> Result<(), adv_cpu::backend::MemFault> {
        self.get_mut(owner).lifetime.dependencies.insert(dependency);
        self.sync_counts(cpu)
    }

    pub fn release(
        &mut self,
        cpu: &mut dyn adv_cpu::backend::Guest,
        id: ModuleId,
    ) -> Result<Vec<ModuleId>, adv_cpu::backend::MemFault> {
        let life = &mut self.get_mut(id).lifetime;
        life.external_refs = life.external_refs.saturating_sub(1);
        self.sync_counts(cpu)?;
        Ok(self.unreachable())
    }

    pub fn unreachable(&self) -> Vec<ModuleId> {
        let mut reachable = BTreeSet::new();
        let mut pending: Vec<_> = self
            .iter()
            .filter(|(_, m)| m.lifetime.pinned || m.lifetime.external_refs > 0)
            .map(|(id, _)| id)
            .collect();
        while let Some(id) = pending.pop() {
            if reachable.insert(id) {
                pending.extend(self.get(id).lifetime.dependencies.iter().copied());
            }
        }
        // A dependent is initialized after its dependencies. Postorder reversed
        // releases it first, including modules which were already loaded earlier.
        let dead: BTreeSet<_> = self
            .iter()
            .filter(|(id, m)| !m.lifetime.unloading && !reachable.contains(id))
            .map(|(id, _)| id)
            .collect();
        let mut seen = BTreeSet::new();
        let mut result = Vec::new();
        for &id in &dead {
            let mut pending = vec![(id, false)];
            while let Some((id, visited)) = pending.pop() {
                if visited {
                    result.push(id);
                } else if dead.contains(&id) && seen.insert(id) {
                    pending.push((id, true));
                    pending.extend(
                        self.get(id)
                            .lifetime
                            .dependencies
                            .iter()
                            .rev()
                            .map(|&dependency| (dependency, false)),
                    );
                }
            }
        }
        result.reverse();
        result
    }

    pub(crate) fn sync_counts(
        &mut self,
        cpu: &mut dyn adv_cpu::backend::Guest,
    ) -> Result<(), adv_cpu::backend::MemFault> {
        let mut counts: BTreeMap<_, _> = self
            .iter()
            .map(|(id, m)| (id, m.lifetime.external_refs))
            .collect();
        for (_, module) in self.iter() {
            for &id in &module.lifetime.dependencies {
                if let Some(count) = counts.get_mut(&id) {
                    *count = count.saturating_add(1);
                }
            }
        }
        for (&id, module) in &mut self.list {
            let count = counts[&id];
            module.load_count = count;
            if module.ldr_entry != 0 {
                let count = count.min(u16::MAX as u32) as u16;
                cpu.write(module.ldr_entry + 0x38, &count.to_le_bytes())?;
            }
        }
        Ok(())
    }

    /// Retire loader links before unmapping the image. Host ids are not reused;
    /// storage is released and guest LDR addresses may be reused after unlinking.
    pub fn retire(
        &mut self,
        cpu: &mut dyn adv_cpu::backend::Guest,
        id: ModuleId,
    ) -> Result<(), adv_cpu::backend::MemFault> {
        use adv_cpu::backend::GuestExt;
        let (entry, size) = (self.get(id).ldr_entry, self.get(id).ldr_size);
        if entry != 0 {
            for offset in [0, 8, 16] {
                let node = entry + offset;
                let next = cpu.read_u32(node)?;
                let previous = cpu.read_u32(node + 4)?;
                if next != 0 && previous != 0 {
                    cpu.write_u32(previous, next)?;
                    cpu.write_u32(next + 4, previous)?;
                    cpu.write_u32(node, 0)?;
                    cpu.write_u32(node + 4, 0)?;
                }
            }
        }
        if entry != 0 {
            cpu.unmap(entry, size)?;
            self.ldr_free.push((entry, size));
        }
        let module = self.list.remove(&id).expect("live module id");
        self.export_hints.remove(&id);
        self.image_protections.remove(&id);
        if module.executable_visible() {
            let name = module.name;
            self.by_name.remove(&name);
            let replacement = self
                .iter()
                .find(|(_, m)| m.executable_visible() && m.name == name)
                .map(|(id, _)| id);
            if let Some(replacement) = replacement {
                self.by_name.insert(name, replacement);
            }
        }
        self.sync_counts(cpu)
    }

    pub fn host_count(&self) -> u32 {
        self.iter()
            .filter(|(_, m)| m.kind == ModuleKind::Host)
            .count() as u32
    }

    /// Publish stable x86 loader records. Updating a list never rebuilds existing
    /// entries, so guest traversal/hook code can retain their addresses.
    pub(crate) fn publish_ldr(
        &mut self,
        cpu: &mut dyn adv_cpu::backend::Guest,
        id: ModuleId,
        full_path: &str,
    ) -> Result<(), adv_cpu::backend::MemFault> {
        use adv_cpu::backend::GuestExt;
        use adv_machine::layout::{PAGE, PEB_ADDR};
        if self.ldr == 0 {
            let address = self.alloc_ldr(cpu, PAGE)?;
            cpu.write_u32(address, 0x30)?;
            cpu.write_u32(address + 4, 1)?;
            for offset in [0x0c, 0x14, 0x1c] {
                cpu.write_u32(address + offset, address + offset)?;
                cpu.write_u32(address + offset + 4, address + offset)?;
            }
            cpu.write_u32(PEB_ADDR + 0x0c, address)?;
            self.ldr = address;
        }
        if self.get(id).ldr_entry != 0 {
            return Ok(());
        }
        let m = self.get(id);
        let full: Vec<u16> = full_path.encode_utf16().collect();
        let name: Vec<u16> = m.name.encode_utf16().collect();
        // UNICODE_STRING lengths are WORDs. Invalid overlong paths never enter
        // the loader; cap representable metadata without truncating its buffer.
        let full_bytes = full.len().min(32766) * 2;
        let name_bytes = name.len().min(32766) * 2;
        let size = adv_machine::vm::align_up((0x80 + full_bytes + name_bytes + 4) as u32, PAGE);
        let entry = self.alloc_ldr(cpu, size)?;
        self.get_mut(id).ldr_entry = entry;
        self.get_mut(id).ldr_size = size;
        let m = self.get(id);
        cpu.write_u32(entry + 0x18, m.base)?;
        cpu.write_u32(entry + 0x1c, m.entry)?;
        cpu.write_u32(entry + 0x20, m.size)?;
        cpu.write_u32(entry + 0x34, if m.is_dll { 0x0008_0004 } else { 0 })?;
        cpu.write(
            entry + 0x38,
            &(m.load_count.min(u16::MAX as u32) as u16).to_le_bytes(),
        )?;
        let mut cursor = entry + 0x80;
        for (offset, text, length) in [(0x24, &full, full_bytes), (0x2c, &name, name_bytes)] {
            cpu.write(entry + offset, &(length as u16).to_le_bytes())?;
            cpu.write(entry + offset + 2, &((length + 2) as u16).to_le_bytes())?;
            cpu.write_u32(entry + offset + 4, cursor)?;
            let mut encoded = Vec::with_capacity(length + 2);
            for unit in text.iter().take(length / 2) {
                encoded.extend_from_slice(&unit.to_le_bytes());
            }
            encoded.extend_from_slice(&[0, 0]);
            cpu.write(cursor, &encoded)?;
            cursor += length as u32 + 2;
        }
        for (offset, head_offset) in [(0, 0x0c), (8, 0x14)] {
            let node = entry + offset;
            let head = self.ldr + head_offset;
            let tail = cpu.read_u32(head + 4)?;
            cpu.write_u32(node, head)?;
            cpu.write_u32(node + 4, tail)?;
            cpu.write_u32(tail, node)?;
            cpu.write_u32(head + 4, node)?;
        }
        Ok(())
    }

    pub(crate) fn complete_ldr(
        &self,
        cpu: &mut dyn adv_cpu::backend::Guest,
        id: ModuleId,
    ) -> Result<(), adv_cpu::backend::MemFault> {
        use adv_cpu::backend::GuestExt;
        let module = self.get(id);
        if !module.is_dll {
            return Ok(());
        }
        let node = module.ldr_entry + 16;
        if cpu.read_u32(node)? != 0 {
            return Ok(());
        }
        let head = self.ldr + 0x1c;
        if module.kind == ModuleKind::Host && module.name == "ntdll.dll" {
            // Native process infrastructure initializes before ordinary DLLs,
            // even when its synthetic image is published after static imports.
            let first = cpu.read_u32(head)?;
            cpu.write_u32(node, first)?;
            cpu.write_u32(node + 4, head)?;
            cpu.write_u32(first + 4, node)?;
            cpu.write_u32(head, node)?;
            return Ok(());
        }
        let tail = cpu.read_u32(head + 4)?;
        cpu.write_u32(node, head)?;
        cpu.write_u32(node + 4, tail)?;
        cpu.write_u32(tail, node)?;
        cpu.write_u32(head + 4, node)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(name: &str, base: u32) -> Module {
        Module {
            name: name.into(),
            path: name.into(),
            kind: ModuleKind::Image,
            base,
            size: 4096,
            is_dll: true,
            entry: base,
            stack_reserve: 0,
            exports_by_name: HashMap::new(),
            exports_by_ordinal: HashMap::new(),
            resources: None,
            data_file: None,
            load_count: 1,
            ldr_entry: 0,
            ldr_size: 0,
            lifetime: Lifetime::default(),
        }
    }

    #[test]
    fn dependency_cycles_release_as_a_group_and_explicit_references_keep_them_alive() {
        let mut cpu = adv_cpu::backend::jit::JitCpu::new(0x1000, false).unwrap();
        let mut modules = Modules::default();
        let a = modules.add(image("a.dll", 0x10000));
        let b = modules.add(image("b.dll", 0x20000));
        let c = modules.add(image("c.dll", 0x30000));
        modules.export_search(a).1.insert("target".into(), 7);
        modules.dependency(&mut cpu, a, b).unwrap();
        modules.dependency(&mut cpu, b, a).unwrap();
        modules.dependency(&mut cpu, a, c).unwrap();
        modules.dependency(&mut cpu, a, c).unwrap();
        assert_eq!(
            modules.get(c).load_count,
            2,
            "repeated export resolution must not retain twice"
        );
        assert!(modules.release(&mut cpu, a).unwrap().is_empty());
        assert_eq!(modules.release(&mut cpu, b).unwrap(), vec![a, b]);
        modules.retire(&mut cpu, a).unwrap();
        assert!(!modules.export_hints.contains_key(&a));
        modules.retire(&mut cpu, b).unwrap();
        assert_eq!(modules.get(c).load_count, 1);
        assert_eq!(modules.release(&mut cpu, c).unwrap(), vec![c]);
        modules.retire(&mut cpu, c).unwrap();
        assert_eq!(modules.iter().count(), 0);
        let later = modules.add(image("a.dll", 0x10000));
        assert!(later > c, "host identities must not alias retired modules");
        assert_eq!(modules.by_name("A.DLL"), Some(later));
        assert!(modules.export_search(later).1.is_empty());
    }

    #[test]
    fn keys() {
        assert_eq!(module_key("KERNEL32.dll"), "kernel32.dll");
        assert_eq!(module_key("C:\\Game\\dll\\RESOURCE.dll"), "resource.dll");
        assert_eq!(module_key("Game/Plugin/Hook.dll"), "hook.dll");
        assert_eq!(module_key("user32"), "user32.dll");
        assert_eq!(module_key("foo."), "foo");
    }
}
