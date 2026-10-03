//! Loaded-module table: guest images (exe, guest DLLs, resource-only DLLs) and host modules
//! (kernel32, ...) with real, guest-readable synthetic PE images.

use std::collections::HashMap;

use adv_pe::{ExportTarget, ResourceDir};

pub type ModuleId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleKind {
    /// A PE image mapped into guest memory.
    Image,
    /// Implemented by the host; `base` addresses a synthetic PE image.
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
    /// Stable x86 LDR_DATA_TABLE_ENTRY in guest memory, zero until published.
    pub ldr_entry: u32,
}

impl Module {
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
    list: Vec<Module>,
    by_name: HashMap<String, ModuleId>,
    ldr: u32,
    ldr_used: u32,
}

impl Modules {
    fn alloc_ldr(
        &mut self,
        cpu: &mut dyn adv_cpu::backend::Guest,
        size: u32,
    ) -> Result<u32, adv_cpu::backend::MemFault> {
        use adv_machine::layout::{LDR_BASE, LDR_SIZE};
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
        let id = self.list.len();
        self.by_name.entry(m.name.clone()).or_insert(id);
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
        if crate::loader::is_system_path(path) {
            return self.system(path);
        }
        if path.contains(['\\', '/']) {
            let path = path.replace('/', "\\");
            return self
                .list
                .iter()
                .position(|m| m.path.eq_ignore_ascii_case(&path));
        }
        self.by_name.get(&module_key(path)).copied()
    }

    pub fn system(&self, name: &str) -> Option<ModuleId> {
        let key = module_key(name);
        self.list
            .iter()
            .position(|m| m.kind == ModuleKind::Host && m.name == key)
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
        if self.list[id].ldr_entry != 0 {
            return Ok(());
        }
        let m = &self.list[id];
        let full: Vec<u16> = full_path.encode_utf16().collect();
        let name: Vec<u16> = m.name.encode_utf16().collect();
        // UNICODE_STRING lengths are WORDs. Invalid overlong paths never enter
        // the loader; cap representable metadata without truncating its buffer.
        let full_bytes = full.len().min(32766) * 2;
        let name_bytes = name.len().min(32766) * 2;
        let size = adv_machine::vm::align_up((0x80 + full_bytes + name_bytes + 4) as u32, PAGE);
        let entry = self.alloc_ldr(cpu, size)?;
        let m = &self.list[id];
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
        self.list[id].ldr_entry = entry;
        Ok(())
    }

    pub(crate) fn complete_ldr(
        &self,
        cpu: &mut dyn adv_cpu::backend::Guest,
        id: ModuleId,
    ) -> Result<(), adv_cpu::backend::MemFault> {
        use adv_cpu::backend::GuestExt;
        let module = &self.list[id];
        if !module.is_dll {
            return Ok(());
        }
        let node = module.ldr_entry + 16;
        if cpu.read_u32(node)? != 0 {
            return Ok(());
        }
        let head = self.ldr + 0x1c;
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
