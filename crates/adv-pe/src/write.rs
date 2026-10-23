//! Generic i386 PE32 DLL writing. Addresses supplied by callers are RVAs;
//! only HIGHLOW relocation targets contain preferred-base virtual addresses.

use std::collections::BTreeMap;

use crate::{
    DataDir, Export, ExportTarget, Import, ImportModule, ImportName, PeError, Result, dir, scn,
};

const PAGE: u32 = 0x1000;
const ALIGN: u32 = 0x200;
const NT: usize = 0x80;
const OPT: usize = NT + 24;
const SECTIONS: usize = OPT + 224;

struct Section {
    name: String,
    rva: u32,
    size: u32,
    flags: u32,
    bytes: Vec<u8>,
}

/// A DLL with page-aligned sections and 512-byte file alignment.
/// Directories are added after the sections whose RVAs they reference.
pub struct DllWriter {
    name: String,
    base: u32,
    entry: u32,
    next: u32,
    sections: Vec<Section>,
    dirs: [DataDir; 16],
}

fn size(value: usize) -> Result<u32> {
    u32::try_from(value).map_err(|_| PeError::Malformed("DLL exceeds PE32 size".into()))
}

fn aligned(value: u32, alignment: u32) -> Result<u32> {
    value
        .checked_next_multiple_of(alignment)
        .ok_or_else(|| PeError::Malformed("DLL alignment overflows".into()))
}

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn string(bytes: &mut Vec<u8>, base: u32, text: &str) -> Result<u32> {
    let rva = crate::add(base, size(bytes.len())?)?;
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(0);
    Ok(rva)
}

impl DllWriter {
    pub fn new(name: &str, image_base: u32) -> Self {
        Self {
            name: name.into(),
            base: image_base,
            entry: 0,
            next: PAGE,
            sections: Vec::new(),
            dirs: [DataDir::default(); 16],
        }
    }

    /// RVA the next section will receive.
    pub fn next_rva(&self) -> u32 {
        self.next
    }

    /// Zero means no DllMain, as for a DLL containing only exports.
    pub fn set_entry(&mut self, rva: u32) {
        self.entry = rva;
    }

    /// Append a section; `virtual_size` includes any zero-filled tail.
    pub fn section(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        virtual_size: u32,
        flags: u32,
    ) -> Result<u32> {
        if name.len() > 8 || virtual_size < size(bytes.len())? {
            return Err(PeError::Malformed(
                "invalid DLL section name or size".into(),
            ));
        }
        let rva = self.next;
        self.next = crate::add(rva, aligned(virtual_size.max(1), PAGE)?)?;
        self.sections.push(Section {
            name: name.into(),
            rva,
            size: virtual_size,
            flags,
            bytes,
        });
        Ok(rva)
    }

    /// Write a sorted name table and a possibly sparse ordinal table.
    pub fn exports(&mut self, exports: &[Export]) -> Result<()> {
        if exports.is_empty() {
            return Ok(());
        }
        let first = exports.iter().map(|e| e.ordinal).min().unwrap();
        let last = exports.iter().map(|e| e.ordinal).max().unwrap();
        let count = usize::from(last - first) + 1;
        let mut named: Vec<_> = exports.iter().filter(|e| e.name.is_some()).collect();
        named.sort_by(|a, b| a.name.cmp(&b.name));
        let base = self.next;
        let funcs = 40;
        let names = funcs + count * 4;
        let ords = names + named.len() * 4;
        let mut bytes = vec![0; ords + named.len() * 2];
        let dll = string(&mut bytes, base, &self.name)?;
        put32(&mut bytes, 12, dll);
        put32(&mut bytes, 16, first.into());
        put32(&mut bytes, 20, size(count)?);
        put32(&mut bytes, 24, size(named.len())?);
        put32(&mut bytes, 28, base + size(funcs)?);
        put32(&mut bytes, 32, base + size(names)?);
        put32(&mut bytes, 36, base + size(ords)?);
        for export in exports {
            let at = funcs + usize::from(export.ordinal - first) * 4;
            if bytes[at..at + 4] != [0; 4] {
                return Err(PeError::Malformed("duplicate DLL export ordinal".into()));
            }
            let target = match &export.target {
                ExportTarget::Rva(rva) => *rva,
                ExportTarget::Forwarder(name) => string(&mut bytes, base, name)?,
            };
            put32(&mut bytes, at, target);
        }
        for (i, export) in named.iter().enumerate() {
            let rva = string(&mut bytes, base, export.name.as_ref().unwrap())?;
            put32(&mut bytes, names + i * 4, rva);
            put16(&mut bytes, ords + i * 2, export.ordinal - first);
        }
        self.directory(dir::EXPORT, ".edata", bytes, scn::MEM_READ)?;
        Ok(())
    }

    /// Write descriptors, name thunks and writable IATs. Input IAT RVAs are
    /// ignored; the returned modules contain the slots assigned by this writer.
    pub fn imports(&mut self, modules: &[ImportModule]) -> Result<Vec<ImportModule>> {
        if modules.is_empty() {
            return Ok(Vec::new());
        }
        let base = self.next;
        let mut bytes = vec![0; (modules.len() + 1) * 20];
        let iat_start = bytes.len();
        for module in modules {
            bytes.resize(bytes.len() + (module.imports.len() + 1) * 4, 0);
        }
        let iat_size = bytes.len() - iat_start;
        let mut iat = iat_start;
        let mut result = Vec::new();
        for (i, module) in modules.iter().enumerate() {
            bytes.resize(bytes.len().next_multiple_of(4), 0);
            let int = bytes.len();
            bytes.resize(int + (module.imports.len() + 1) * 4, 0);
            let dll = string(&mut bytes, base, &module.dll)?;
            put32(&mut bytes, i * 20, base + size(int)?);
            put32(&mut bytes, i * 20 + 12, dll);
            put32(&mut bytes, i * 20 + 16, base + size(iat)?);
            let mut imports = Vec::new();
            for (j, import) in module.imports.iter().enumerate() {
                let value = match &import.name {
                    ImportName::Ordinal(ordinal) => 0x8000_0000 | u32::from(*ordinal),
                    ImportName::Name { hint, name } => {
                        if !bytes.len().is_multiple_of(2) {
                            bytes.push(0);
                        }
                        let rva = base + size(bytes.len())?;
                        bytes.extend_from_slice(&hint.to_le_bytes());
                        string(&mut bytes, base, name)?;
                        rva
                    }
                };
                put32(&mut bytes, int + j * 4, value);
                put32(&mut bytes, iat + j * 4, value);
                imports.push(Import {
                    name: import.name.clone(),
                    iat_rva: base + size(iat + j * 4)?,
                });
            }
            iat += (module.imports.len() + 1) * 4;
            result.push(ImportModule {
                dll: module.dll.clone(),
                imports,
            });
        }
        self.dirs[dir::IAT] = DataDir {
            rva: base + size(iat_start)?,
            size: size(iat_size)?,
        };
        self.directory(dir::IMPORT, ".idata", bytes, scn::MEM_READ | scn::MEM_WRITE)?;
        Ok(result)
    }

    /// Write HIGHLOW records for preferred-base 32-bit VAs in the image.
    pub fn relocations(&mut self, rvas: &[u32]) -> Result<()> {
        if rvas.is_empty() {
            return Ok(());
        }
        let mut pages = BTreeMap::<u32, Vec<u16>>::new();
        for &rva in rvas {
            pages
                .entry(rva & !0xfff)
                .or_default()
                .push(0x3000 | (rva & 0xfff) as u16);
        }
        let mut bytes = Vec::new();
        for (page, mut entries) in pages {
            entries.sort_unstable();
            entries.dedup();
            if !entries.len().is_multiple_of(2) {
                entries.push(0);
            }
            bytes.extend_from_slice(&page.to_le_bytes());
            bytes.extend_from_slice(&(8 + size(entries.len())? * 2).to_le_bytes());
            for entry in entries {
                bytes.extend_from_slice(&entry.to_le_bytes());
            }
        }
        self.directory(dir::BASERELOC, ".reloc", bytes, scn::MEM_READ | 0x0200_0000)?;
        Ok(())
    }

    fn directory(&mut self, index: usize, name: &str, bytes: Vec<u8>, flags: u32) -> Result<()> {
        let len = size(bytes.len())?;
        self.dirs[index] = DataDir {
            rva: self.section(name, bytes, len, flags | 0x40)?,
            size: len,
        };
        Ok(())
    }

    pub fn finish(self) -> Result<Vec<u8>> {
        let count = u16::try_from(self.sections.len())
            .map_err(|_| PeError::Malformed("too many DLL sections".into()))?;
        let headers = aligned(size(SECTIONS + self.sections.len() * 40)?, ALIGN)?;
        if headers > PAGE {
            return Err(PeError::Malformed(
                "DLL headers overlap first section".into(),
            ));
        }
        let mut bytes = vec![0; headers as usize];
        bytes[..2].copy_from_slice(b"MZ");
        put32(&mut bytes, 0x3c, NT as u32);
        bytes[NT..NT + 4].copy_from_slice(b"PE\0\0");
        put16(&mut bytes, NT + 4, 0x14c);
        put16(&mut bytes, NT + 6, count);
        put16(&mut bytes, NT + 20, 224);
        put16(&mut bytes, NT + 22, 0x2102);
        put16(&mut bytes, OPT, 0x10b);
        for (at, value) in [
            (16, self.entry),
            (28, self.base),
            (32, PAGE),
            (36, ALIGN),
            (56, self.next),
            (60, headers),
            (72, 0x10_0000),
            (76, PAGE),
            (80, 0x10_0000),
            (84, PAGE),
            (92, 16),
        ] {
            put32(&mut bytes, OPT + at, value);
        }
        for (at, value) in [(40, 6), (48, 6), (68, 3), (70, 0x140)] {
            put16(&mut bytes, OPT + at, value);
        }
        for (i, dir) in self.dirs.iter().enumerate() {
            put32(&mut bytes, OPT + 96 + i * 8, dir.rva);
            put32(&mut bytes, OPT + 100 + i * 8, dir.size);
        }
        let mut code_size = 0;
        let mut data_size = 0;
        for (i, section) in self.sections.iter().enumerate() {
            let raw = aligned(size(section.bytes.len())?, ALIGN)?;
            let offset = size(bytes.len())?;
            let at = SECTIONS + i * 40;
            bytes[at..at + section.name.len()].copy_from_slice(section.name.as_bytes());
            for (field, value) in [
                (8, section.size),
                (12, section.rva),
                (16, raw),
                (20, offset),
                (36, section.flags),
            ] {
                put32(&mut bytes, at + field, value);
            }
            if section.flags & scn::CNT_CODE != 0 {
                code_size += raw;
                if code_size == raw {
                    put32(&mut bytes, OPT + 20, section.rva);
                }
            } else {
                data_size += raw;
                if data_size == raw {
                    put32(&mut bytes, OPT + 24, section.rva);
                }
            }
            bytes.extend_from_slice(&section.bytes);
            bytes.resize(offset as usize + raw as usize, 0);
        }
        put32(&mut bytes, OPT + 4, code_size);
        put32(&mut bytes, OPT + 8, data_size);
        Ok(bytes)
    }
}
