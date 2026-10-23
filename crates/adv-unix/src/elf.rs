//! i386 ELF shared objects: the parts Wine's Unix libraries use.
//!
//! An object is parsed from its file into an image laid out at its virtual
//! addresses, with its dynamic symbols, the library each undefined symbol's
//! version names, its relocations and its initializers. Mapping and binding
//! are the loader's.

use std::collections::HashMap;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("not an i386 ELF shared object")]
    Format,
    #[error("truncated ELF field at {0:#x}")]
    Truncated(usize),
    #[error("unsupported relocation type {0}")]
    Relocation(u32),
}

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

const DT_NULL: u32 = 0;
const DT_NEEDED: u32 = 1;
const DT_PLTRELSZ: u32 = 2;
const DT_STRTAB: u32 = 5;
const DT_SYMTAB: u32 = 6;
const DT_INIT: u32 = 12;
const DT_REL: u32 = 17;
const DT_RELSZ: u32 = 18;
const DT_JMPREL: u32 = 23;
const DT_INIT_ARRAY: u32 = 25;
const DT_INIT_ARRAYSZ: u32 = 27;
const DT_GNU_HASH: u32 = 0x6fff_fef5;
const DT_VERSYM: u32 = 0x6fff_fff0;
const DT_VERNEED: u32 = 0x6fff_fffe;
const DT_VERNEEDNUM: u32 = 0x6fff_ffff;

pub const R_386_NONE: u32 = 0;
pub const R_386_32: u32 = 1;
pub const R_386_PC32: u32 = 2;
pub const R_386_GLOB_DAT: u32 = 6;
pub const R_386_JMP_SLOT: u32 = 7;
pub const R_386_RELATIVE: u32 = 8;

const STB_WEAK: u8 = 2;
const SHN_UNDEF: u16 = 0;

pub struct Symbol {
    pub name: String,
    /// Offset from the load base; 0 when undefined.
    pub value: u32,
    pub defined: bool,
    pub weak: bool,
    /// For an undefined symbol, the library its version names.
    pub library: Option<String>,
}

pub struct Relocation {
    pub offset: u32,
    pub kind: u32,
    pub symbol: u32,
}

pub struct Object {
    /// The loaded image from offset 0, zero-filled where segments leave gaps.
    pub image: Vec<u8>,
    /// Protection of each page: readable always, writable, executable.
    pub pages: Vec<(bool, bool)>,
    pub needed: Vec<String>,
    pub symbols: Vec<Symbol>,
    pub relocations: Vec<Relocation>,
    /// `DT_INIT`, then the `DT_INIT_ARRAY` entries, as offsets from the base
    /// (array entries are pointers the relocations fix up).
    pub init: Option<u32>,
    pub init_array: Option<(u32, u32)>,
}

impl Object {
    /// The defined dynamic symbols, by name.
    pub fn exports(&self) -> HashMap<&str, u32> {
        self.symbols
            .iter()
            .filter(|symbol| symbol.defined)
            .map(|symbol| (symbol.name.as_str(), symbol.value))
            .collect()
    }
}

struct File<'a>(&'a [u8]);

impl File<'_> {
    fn bytes(&self, at: usize, len: usize) -> Result<&[u8], Error> {
        self.0
            .get(at..at.checked_add(len).ok_or(Error::Truncated(at))?)
            .ok_or(Error::Truncated(at))
    }
    fn u8(&self, at: usize) -> Result<u8, Error> {
        Ok(self.bytes(at, 1)?[0])
    }
    fn u16(&self, at: usize) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.bytes(at, 2)?.try_into().unwrap()))
    }
    fn u32(&self, at: usize) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.bytes(at, 4)?.try_into().unwrap()))
    }
    fn str(&self, at: usize) -> Result<String, Error> {
        let rest = self.0.get(at..).ok_or(Error::Truncated(at))?;
        let end = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(Error::Truncated(at))?;
        Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
    }
}

/// Parses an i386 ELF shared object.
pub fn parse(data: &[u8]) -> Result<Object, Error> {
    let file = File(data);
    // ELFCLASS32, ELFDATA2LSB, ET_DYN, EM_386.
    if file.bytes(0, 6)? != b"\x7fELF\x01\x01" || file.u16(16)? != 3 || file.u16(18)? != 3 {
        return Err(Error::Format);
    }
    let (phoff, phentsize, phnum) = (
        file.u32(28)? as usize,
        file.u16(42)? as usize,
        file.u16(44)? as usize,
    );

    let mut segments = Vec::new();
    let mut dynamic = None;
    for index in 0..phnum {
        let ph = phoff + index * phentsize;
        let (kind, offset, vaddr, filesz, memsz, flags) = (
            file.u32(ph)?,
            file.u32(ph + 4)?,
            file.u32(ph + 8)?,
            file.u32(ph + 16)?,
            file.u32(ph + 20)?,
            file.u32(ph + 24)?,
        );
        match kind {
            PT_LOAD => segments.push((offset, vaddr, filesz, memsz, flags)),
            PT_DYNAMIC => dynamic = Some(vaddr),
            _ => {}
        }
    }
    let end = segments
        .iter()
        .map(|&(_, vaddr, _, memsz, _)| vaddr as usize + memsz as usize)
        .max()
        .ok_or(Error::Format)?;
    let pages_len = end.div_ceil(PAGE);
    let mut image = vec![0u8; pages_len * PAGE];
    let mut pages = vec![(false, false); pages_len];
    for &(offset, vaddr, filesz, memsz, flags) in &segments {
        let bytes = file.bytes(offset as usize, filesz as usize)?;
        image[vaddr as usize..vaddr as usize + bytes.len()].copy_from_slice(bytes);
        let first = vaddr as usize / PAGE;
        let last = (vaddr as usize + memsz as usize).div_ceil(PAGE);
        for page in &mut pages[first..last] {
            page.0 |= flags & PF_W != 0;
            page.1 |= flags & PF_X != 0;
        }
    }

    // The dynamic section, read from the image where it is loaded.
    let image_file = File(&image);
    let mut tags: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Some(dynamic) = dynamic {
        let mut at = dynamic as usize;
        loop {
            let (tag, value) = (image_file.u32(at)?, image_file.u32(at + 4)?);
            if tag == DT_NULL {
                break;
            }
            tags.entry(tag).or_default().push(value);
            at += 8;
        }
    }
    let tag = |tag| tags.get(&tag).and_then(|values| values.first().copied());
    let strtab = tag(DT_STRTAB).ok_or(Error::Format)? as usize;
    let symtab = tag(DT_SYMTAB).ok_or(Error::Format)? as usize;
    let string = |offset: u32| image_file.str(strtab + offset as usize);

    let needed = tags
        .get(&DT_NEEDED)
        .into_iter()
        .flatten()
        .map(|&offset| string(offset))
        .collect::<Result<_, _>>()?;

    // Version index to library, from the version needs.
    let mut versions = HashMap::new();
    if let (Some(verneed), Some(count)) = (tag(DT_VERNEED), tag(DT_VERNEEDNUM)) {
        let mut need = verneed as usize;
        for _ in 0..count {
            let library = string(image_file.u32(need + 4)?)?;
            let (auxes, mut aux) = (
                image_file.u16(need + 2)?,
                need + image_file.u32(need + 8)? as usize,
            );
            for _ in 0..auxes {
                versions.insert(image_file.u16(aux + 6)?, library.clone());
                aux += image_file.u32(aux + 12)? as usize;
            }
            need += image_file.u32(need + 12)? as usize;
        }
    }
    let versym = tag(DT_VERSYM);

    let count = match tag(DT_GNU_HASH) {
        Some(hash) => gnu_hash_count(&image_file, hash as usize)?,
        None => return Err(Error::Format),
    };
    let mut symbols = Vec::with_capacity(count);
    for index in 0..count {
        let sym = symtab + index * 16;
        let (name, value, info, shndx) = (
            image_file.u32(sym)?,
            image_file.u32(sym + 4)?,
            image_file.u8(sym + 12)?,
            image_file.u16(sym + 14)?,
        );
        let defined = shndx != SHN_UNDEF;
        let library = match versym {
            Some(versym) if !defined => {
                let version = image_file.u16(versym as usize + index * 2)? & 0x7fff;
                versions.get(&version).cloned()
            }
            _ => None,
        };
        symbols.push(Symbol {
            name: string(name)?,
            value: if defined { value } else { 0 },
            defined,
            weak: info >> 4 == STB_WEAK,
            library,
        });
    }

    let mut relocations = Vec::new();
    for (start, size) in [
        (tag(DT_REL), tag(DT_RELSZ)),
        (tag(DT_JMPREL), tag(DT_PLTRELSZ)),
    ] {
        let (Some(start), Some(size)) = (start, size) else {
            continue;
        };
        for at in (start as usize..(start + size) as usize).step_by(8) {
            let info = image_file.u32(at + 4)?;
            let kind = info & 0xff;
            if !matches!(
                kind,
                R_386_NONE
                    | R_386_32
                    | R_386_PC32
                    | R_386_GLOB_DAT
                    | R_386_JMP_SLOT
                    | R_386_RELATIVE
            ) {
                return Err(Error::Relocation(kind));
            }
            relocations.push(Relocation {
                offset: image_file.u32(at)?,
                kind,
                symbol: info >> 8,
            });
        }
    }

    Ok(Object {
        image,
        pages,
        needed,
        symbols,
        relocations,
        init: tag(DT_INIT),
        init_array: tag(DT_INIT_ARRAY).zip(tag(DT_INIT_ARRAYSZ)),
    })
}

pub const PAGE: usize = 0x1000;

/// The number of dynamic symbols: one past the last a GNU hash chain ends at.
fn gnu_hash_count(file: &File<'_>, hash: usize) -> Result<usize, Error> {
    let (buckets, offset, blooms) = (
        file.u32(hash)? as usize,
        file.u32(hash + 4)? as usize,
        file.u32(hash + 8)? as usize,
    );
    let bucket_at = hash + 16 + blooms * 4;
    let chain_at = bucket_at + buckets * 4;
    let mut last = 0;
    for bucket in 0..buckets {
        last = last.max(file.u32(bucket_at + bucket * 4)? as usize);
    }
    if last < offset {
        return Ok(offset);
    }
    while file.u32(chain_at + (last - offset) * 4)? & 1 == 0 {
        last += 1;
    }
    Ok(last + 1)
}
