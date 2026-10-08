//! Raw readonly PE views. Modules owns the shared section; guest pages are
//! aliases, not a byte snapshot, and no executable loader action occurs here.
use std::rc::Rc;

use adv_cpu::backend::{Guest, Prot};
use adv_machine::{Sys, layout::PAGE};
use adv_pe::{Pe, PeError, ResId, ResourceDir};

use super::{Lifetime, Module, ModuleId, ModuleKind, Modules, module_key};
use crate::{LoadError, vfs::FileSection};

pub struct DataFile {
    pub section: Rc<FileSection>,
    pub file_len: u32,
    pub minimum_len: u64,
    pub protections: Vec<u32>,
}

impl std::fmt::Debug for DataFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataFile")
            .field("file_len", &self.file_len)
            .finish_non_exhaustive()
    }
}

fn read(
    cpu: &dyn Guest,
    base: u32,
    file_len: u32,
    offset: u32,
    bytes: &mut [u8],
) -> Result<(), PeError> {
    let length = bytes.len() as u32;
    let address = offset
        .checked_add(length)
        .filter(|&end| end <= file_len)
        .and_then(|_| base.checked_add(offset))
        .ok_or(PeError::OutOfImage {
            rva: offset,
            len: length,
        })?;
    cpu.read(address, bytes).map_err(|_| PeError::OutOfImage {
        rva: offset,
        len: length,
    })
}

impl DataFile {
    pub fn protection(&self, cpu: &dyn Guest, base: u32, page: u32) -> u32 {
        let value = self.protections[((page - base) / PAGE) as usize];
        if value == 8 && cpu.file_page_copy_on_write(page) == Some(false) {
            4
        } else {
            value
        }
    }
    /// Compact only the DOS/NT/section headers for the common PE decoder. An
    /// enormous e_lfanew gap never allocates an enormous temporary buffer.
    fn headers(&self, cpu: &dyn Guest, base: u32) -> Result<Pe, PeError> {
        let mut dos = [0; 64];
        read(cpu, base, self.file_len, 0, &mut dos)?;
        if &dos[..2] != b"MZ" {
            return Err(PeError::NotPe);
        }
        let nt = u32::from_le_bytes(dos[60..64].try_into().unwrap());
        let mut prefix = [0; 24];
        read(cpu, base, self.file_len, nt, &mut prefix)?;
        if &prefix[..4] != b"PE\0\0" {
            return Err(PeError::NotPe);
        }
        let count = u32::from(u16::from_le_bytes(prefix[6..8].try_into().unwrap()));
        let optional = u32::from(u16::from_le_bytes(prefix[20..22].try_into().unwrap()));
        if optional < 96 {
            return Err(PeError::Malformed("short optional header".into()));
        }
        let tail = optional + count * 40; // Both inputs are WORDs, bounded by 2.6 MiB.
        let start = nt.checked_add(24).ok_or(PeError::NotPe)?;
        if start
            .checked_add(tail)
            .is_none_or(|end| end > self.file_len)
        {
            return Err(PeError::OutOfImage {
                rva: start,
                len: tail,
            });
        }
        let mut headers = vec![0; 64 + 24 + tail as usize];
        dos[60..64].copy_from_slice(&64u32.to_le_bytes());
        headers[..64].copy_from_slice(&dos);
        headers[64..88].copy_from_slice(&prefix);
        read(cpu, base, self.file_len, start, &mut headers[88..])?;
        let directories = u32::from_le_bytes(headers[180..184].try_into().unwrap()).min(16);
        if 96 + directories * 8 > optional {
            return Err(PeError::Malformed(
                "data directories exceed optional header".into(),
            ));
        }
        Pe::parse_headers(&headers)
    }

    fn offset(&self, pe: &Pe, rva: u32, length: u32) -> Option<u32> {
        let offset = if rva < pe.size_of_headers {
            (rva.checked_add(length)? <= pe.size_of_headers).then_some(rva)?
        } else {
            pe.sections.iter().find_map(|section| {
                let delta = rva.checked_sub(section.virtual_address)?;
                (delta < section.raw_size && delta.checked_add(length)? <= section.raw_size)
                    .then(|| section.raw_offset.checked_add(delta))
                    .flatten()
            })?
        };
        (offset.checked_add(length)? <= self.file_len).then_some(offset)
    }

    pub fn find(
        &self,
        cpu: &dyn Guest,
        base: u32,
        ty: &ResId,
        name: &ResId,
        lang: Option<u16>,
    ) -> Option<u32> {
        let pe = self.headers(cpu, base).ok()?;
        let directory = pe.dir(adv_pe::dir::RESOURCE);
        if !directory.is_present() {
            return None;
        }
        let resources = ResourceDir::read(directory.rva, |rva, bytes| {
            let length = bytes.len() as u32;
            let offset = self
                .offset(&pe, rva, length)
                .ok_or(PeError::OutOfImage { rva, len: length })?;
            read(cpu, base, self.file_len, offset, bytes)
        })
        .ok()?;
        let (_, data) = resources.find(ty, name, lang)?;
        base.checked_add(self.offset(&pe, data.entry_rva, 16)?)
    }

    pub fn data(&self, cpu: &dyn Guest, base: u32, entry: u32) -> Option<(u32, u32)> {
        let offset = entry.checked_sub(base)?;
        let mut bytes = [0; 16];
        read(cpu, base, self.file_len, offset, &mut bytes).ok()?;
        let rva = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let pe = self.headers(cpu, base).ok()?;
        Some((base.checked_add(self.offset(&pe, rva, size)?)?, size))
    }
}

impl Modules {
    /// A cold file-resize query against this existing owner, not a lease table.
    /// The raw mapping covers [0, file_len), excluding rounded tail padding.
    pub fn minimum_data_file_length(&self, section: &Rc<FileSection>) -> Option<u64> {
        self.iter()
            .filter_map(|(_, module)| module.data_file.as_ref())
            .filter(|raw| Rc::ptr_eq(&raw.section, section))
            .map(|raw| raw.minimum_len)
            .max()
    }
    /// Publish only after all raw pages and the bounded headers are validated.
    /// A failed backend mapping/header read frees the complete reservation.
    pub fn map_data_file(
        &mut self,
        cpu: &mut dyn Guest,
        sys: &mut Sys,
        path: &str,
        section: Rc<FileSection>,
        file_len: u32,
    ) -> Result<ModuleId, LoadError> {
        if file_len < 64 {
            return Err(LoadError::Pe {
                path: path.into(),
                err: PeError::NotPe,
            });
        }
        let size = file_len
            .checked_add(PAGE - 1)
            .map(|n| n & !(PAGE - 1))
            .filter(|&n| n != 0)
            .ok_or_else(|| LoadError::NoSpace(path.into(), file_len))?;
        let base = sys
            .vm
            .reserve_exact(size)
            .ok_or_else(|| LoadError::NoSpace(path.into(), size))?;
        let raw = DataFile {
            section,
            file_len,
            minimum_len: u64::from(file_len),
            protections: vec![2; (size / PAGE) as usize],
        };
        let result = (|| {
            let pages: Vec<_> = (0..size / PAGE)
                .map(|index| raw.section.page(u64::from(index)))
                .collect();
            // Readonly pages keep sharing until an explicit WRITECOPY
            // protection permits a first write. The existing backend owns
            // transactional per-page detachment; no remapping is necessary.
            cpu.map_file_pages(base, &pages, Prot::READ, true)?;
            raw.headers(cpu, base).map_err(|err| LoadError::Pe {
                path: path.into(),
                err,
            })?;
            Ok::<_, LoadError>(())
        })();
        if let Err(error) = result {
            sys.vm.free(cpu, base)?;
            return Err(error);
        }
        let mut raw = raw;
        raw.minimum_len = raw
            .minimum_len
            .max(self.minimum_data_file_length(&raw.section).unwrap_or(0));
        for module in self.list.values_mut() {
            if let Some(existing) = &mut module.data_file
                && Rc::ptr_eq(&existing.section, &raw.section)
            {
                existing.minimum_len = raw.minimum_len;
            }
        }
        Ok(self.add(Module {
            name: module_key(path),
            path: path.into(),
            kind: ModuleKind::DataFile,
            base,
            size,
            is_dll: false,
            entry: 0,
            stack_reserve: 0,
            exports_by_name: Default::default(),
            exports_by_ordinal: Default::default(),
            resources: None,
            data_file: Some(raw),
            load_count: 1,
            ldr_entry: 0,
            ldr_size: 0,
            lifetime: Lifetime {
                mapped: true,
                thread_calls: false,
                ..Lifetime::default()
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::SharedFiles;
    use adv_cpu::backend::{GuestExt, MemFault, Reg};
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};

    struct PartialMap<'a>(&'a mut dyn Guest);
    impl Guest for PartialMap<'_> {
        fn reg(&self, reg: Reg) -> u32 {
            self.0.reg(reg)
        }
        fn set_reg(&mut self, reg: Reg, value: u32) {
            self.0.set_reg(reg, value);
        }
        fn read(&self, addr: u32, bytes: &mut [u8]) -> Result<(), MemFault> {
            self.0.read(addr, bytes)
        }
        fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), MemFault> {
            self.0.write(addr, bytes)
        }
        fn map(&mut self, addr: u32, len: u32, prot: Prot) -> Result<(), MemFault> {
            self.0.map(addr, len, prot)
        }
        fn protect(&mut self, addr: u32, len: u32, prot: Prot) -> Result<(), MemFault> {
            self.0.protect(addr, len, prot)
        }
        fn unmap(&mut self, addr: u32, len: u32) -> Result<(), MemFault> {
            self.0.unmap(addr, len)
        }
        fn map_file_pages(
            &mut self,
            addr: u32,
            pages: &[Rc<adv_cpu::backend::FilePage>],
            prot: Prot,
            copy: bool,
        ) -> Result<(), MemFault> {
            self.0.map_file_pages(addr, &pages[..1], prot, copy)?;
            Err(MemFault {
                addr: addr + PAGE,
                len: PAGE,
            })
        }
    }

    fn own_pe() -> Vec<u8> {
        let mut bytes = vec![0; 8192];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&128u32.to_le_bytes());
        bytes[128..132].copy_from_slice(b"PE\0\0");
        bytes[132..134].copy_from_slice(&0x14cu16.to_le_bytes());
        bytes[134..136].copy_from_slice(&1u16.to_le_bytes());
        bytes[148..150].copy_from_slice(&224u16.to_le_bytes());
        bytes[152..154].copy_from_slice(&0x10bu16.to_le_bytes());
        bytes[208..212].copy_from_slice(&0x20000u32.to_le_bytes());
        bytes[212..216].copy_from_slice(&512u32.to_le_bytes());
        bytes[388..392].copy_from_slice(&0x1000u32.to_le_bytes());
        bytes[392..396].copy_from_slice(&4096u32.to_le_bytes());
        bytes[396..400].copy_from_slice(&512u32.to_le_bytes());
        bytes
    }

    #[test]
    fn partial_mapping_and_bad_headers_roll_back_pages_vm_and_section_owner() {
        let mut machine = Machine::new(
            adv_cpu::JitCpu::new(PARK_ADDR, false).unwrap(),
            adv_abi::Registry::default(),
            Clock::virtual_time(),
        )
        .unwrap();
        let mut modules = Modules::default();
        let files = SharedFiles::default();
        let expected = machine.sys.vm.reserve(PAGE).unwrap();
        machine.sys.vm.free(&mut machine.cpu, expected).unwrap();
        for partial in [true, false] {
            let bytes: std::sync::Arc<[u8]> = if partial {
                own_pe().into()
            } else {
                vec![0; 8192].into()
            };
            let section = files.memory(bytes);
            let weak = Rc::downgrade(&section);
            let result = if partial {
                modules.map_data_file(
                    &mut PartialMap(&mut machine.cpu),
                    &mut machine.sys,
                    "own.dll",
                    section,
                    8192,
                )
            } else {
                modules.map_data_file(&mut machine.cpu, &mut machine.sys, "own.dll", section, 8192)
            };
            assert!(result.is_err());
            assert_eq!(modules.iter().count(), 0);
            assert_eq!(machine.sys.vm.region(expected), None);
            assert!(machine.cpu.read_u32(expected).is_err());
            assert!(machine.cpu.read_u32(expected + PAGE).is_err());
            assert!(weak.upgrade().is_none());
        }
        let section = files.memory(own_pe().into());
        let weak = Rc::downgrade(&section);
        let id = modules
            .map_data_file(&mut machine.cpu, &mut machine.sys, "own.dll", section, 8192)
            .unwrap();
        assert_eq!(modules.get(id).base, expected);
        assert_eq!(modules.by_name("own.dll"), None);
        assert_eq!(modules.by_handle(expected), None);
        assert_eq!(modules.by_handle(expected | 1), Some(id));
        crate::Loader::new(
            &mut machine.cpu,
            &mut machine.sys,
            &mut modules,
            &crate::HostDir::new(".", "C:\\Game"),
        )
        .retire(id)
        .unwrap();
        assert!(machine.cpu.read_u32(expected).is_err());
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn raw_rva_ranges_exclude_virtual_tails_and_cross_section_spans() {
        let bytes = own_pe();
        let pe = Pe::parse_headers(&bytes).unwrap();
        let raw = DataFile {
            section: FileSection::anonymous(),
            file_len: bytes.len() as u32,
            minimum_len: bytes.len() as u64,
            protections: Vec::new(),
        };
        assert_eq!(raw.offset(&pe, 0x1000, 4), Some(512));
        assert_eq!(raw.offset(&pe, 0x1ffe, 4), None);
        assert_eq!(raw.offset(&pe, 0x2000, 1), None);
        assert_eq!(raw.offset(&pe, 510, 4), None);
        assert_eq!(raw.offset(&pe, u32::MAX, 16), None);
    }

    #[test]
    fn failed_map_does_not_raise_shared_floor_and_closing_larger_view_does_not_lower_it() {
        let mut machine = Machine::new(
            adv_cpu::JitCpu::new(PARK_ADDR, false).unwrap(),
            adv_abi::Registry::default(),
            Clock::virtual_time(),
        )
        .unwrap();
        let mut modules = Modules::default();
        let section = SharedFiles::default().memory(own_pe().into());
        let first = modules
            .map_data_file(
                &mut machine.cpu,
                &mut machine.sys,
                "own.dll",
                section.clone(),
                4097,
            )
            .unwrap();
        assert!(
            modules
                .map_data_file(
                    &mut PartialMap(&mut machine.cpu),
                    &mut machine.sys,
                    "own.dll",
                    section.clone(),
                    4098
                )
                .is_err()
        );
        assert_eq!(modules.minimum_data_file_length(&section), Some(4097));
        assert_eq!(modules.iter().count(), 1);
        let second = modules
            .map_data_file(
                &mut machine.cpu,
                &mut machine.sys,
                "own.dll",
                section.clone(),
                4098,
            )
            .unwrap();
        assert_eq!(modules.minimum_data_file_length(&section), Some(4098));
        crate::Loader::new(
            &mut machine.cpu,
            &mut machine.sys,
            &mut modules,
            &crate::HostDir::new(".", "C:\\Game"),
        )
        .retire(second)
        .unwrap();
        assert_eq!(modules.minimum_data_file_length(&section), Some(4098));
        crate::Loader::new(
            &mut machine.cpu,
            &mut machine.sys,
            &mut modules,
            &crate::HostDir::new(".", "C:\\Game"),
        )
        .retire(first)
        .unwrap();
        assert_eq!(modules.minimum_data_file_length(&section), None);
    }
}
